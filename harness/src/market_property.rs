//! Random operations against the vault and the market together, on the real runtime. Posts, bids,
//! reveals, awards with right and wrong terms, deliveries, reviews, draws, revokes and every
//! permissionless call are thrown at both contracts in waves. After each wave the invariants are
//! checked from views alone: nothing here trusts what a transaction returned.
#![allow(unused_must_use)]
use crate::env::*;
use crate::market::*;
use anyhow::Result;
use futures::future::join_all;
use near_workspaces::types::{KeyType, SecretKey};
use near_workspaces::{Account, AccountId};
use rand::rngs::SmallRng;
use rand::seq::SliceRandom;
use rand::{Rng, SeedableRng};
use serde_json::{json, Value};
use std::collections::BTreeMap;

struct St {
    t: Tk,
    bids: Vec<Bidv>,
    agent: Option<Agent>,
    tm: Option<Tm>,
    worker: Option<Account>,
}

#[derive(Default)]
struct Tally {
    sent: u64,
    by_code: BTreeMap<String, u64>,
    bonds_in: u128,
    paid_out: u128,
    checks: u64,
    broken: Vec<String>,
}

impl Tally {
    fn note(&mut self, method: &str, code: &str) {
        self.sent += 1;
        *self.by_code.entry(format!("{method}:{code}")).or_default() += 1;
    }

    fn hold(&mut self, name: &str, ok: bool, detail: impl ToString) {
        self.checks += 1;
        if !ok {
            self.broken.push(format!("{name}: {}", detail.to_string()));
        }
    }
}

fn u(v: &Value) -> u128 {
    v.as_str().unwrap_or("0").parse().unwrap_or(0)
}

fn is_final(status: &str) -> bool {
    matches!(status, "Paid" | "Rejected" | "Failed" | "NoAward" | "Defaulted" | "Void")
}

/// The pricing rule again, written independently of the contract.
fn model_pick(bids: &[(u128, usize)], max: u128, min_bids: u64) -> Option<(usize, u128)> {
    if bids.is_empty() || (bids.len() as u64) < min_bids.max(1) {
        return None;
    }
    let mut sorted = bids.to_vec();
    sorted.sort();
    let price = if sorted.len() > 1 { sorted[1].0.min(max) } else { max };
    Some((sorted[0].1, price))
}

async fn call(m: &Mk, tally: &mut Tally, who: &Account, method: &str, args: Value, deposit: u128, gas: u64) -> Result<String> {
    let r = m.call(who, method, args, deposit, gas).await?;
    let code = code_of(&r);
    tally.note(method, &code);
    Ok(code)
}

/// A draw from a grant. The protocol itself refuses a deleted key or an allowance that is
/// still waiting for its last refund, and that is a result like any other.
async fn draw(m: &Mk, tally: &mut Tally, a: &Agent, amount: u128) {
    match m.draw(a, amount).await {
        Ok(d) => tally.note("vault.request_spend", &d.code),
        Err(_) => tally.note("vault.request_spend", "REFUSED_BY_PROTOCOL"),
    }
}

async fn check(m: &Mk, e: &Env, states: &[St], finals: &mut BTreeMap<u64, String>, tally: &mut Tally) -> Result<()> {
    // The vault's tree.
    let grants = e.info().await?["grants"].as_u64().unwrap();
    let mut by_id = BTreeMap::new();
    for id in 1..=grants {
        by_id.insert(id, e.grant(id).await?);
    }
    let mut direct_total = 0u128;
    for (id, g) in &by_id {
        tally.hold("N1 spent + reserved <= cap", u(&g["spent"]) + u(&g["reserved"]) <= u(&g["cap"]), format!("grant {id}"));
        let below: u128 = g["children"].as_array().unwrap().iter().map(|c| u(&by_id[&c.as_u64().unwrap()]["spent"])).sum();
        tally.hold("N1 a parent's spent covers its children's", u(&g["spent"]) >= below, format!("grant {id}: {} < {below}", u(&g["spent"])));
        if g["recipients"] == json!([m.id()]) {
            direct_total += u(&g["direct_spent"]);
        }
    }
    // The market's tasks.
    let count = m.view("config", json!({})).await?["tasks"].as_u64().unwrap();
    let mut promised: BTreeMap<u64, (u128, u64)> = BTreeMap::new();
    let mut held_sum = 0u128;
    for id in 1..=count {
        let t = m.task(id).await?;
        let status = t["status"].as_str().unwrap().to_string();
        let (price, paid, held, refunded) = (u(&t["price"]), u(&t["paid"]), u(&t["held"]), u(&t["refunded"]));
        held_sum += held;
        tally.hold("N2 paid <= price", paid <= price, format!("task {id}: {paid} > {price}"));
        if let Some(g) = t["grant"].as_u64() {
            let direct = u(&by_id[&g]["direct_spent"]);
            tally.hold("N2 paid + held + refunded == what the grant drew", paid + held + refunded == direct, format!("task {id}: {paid} + {held} + {refunded} != {direct}"));
            tally.hold("N2 a grant's cap is its task's price", u(&by_id[&g]["cap"]) == price, format!("task {id}"));
            let spent = u(&by_id[&g]["spent"]).min(price);
            tally.hold("N6 what a task counts as drawn is what the vault says its grant and hires spent", u(&t["drawn"]) == spent, format!("task {id}: {} != {spent}", t["drawn"]));
            if !matches!(status.as_str(), "Accepted" | "Paid") {
                tally.hold("N4 nothing is paid before acceptance", paid == 0, format!("task {id} is {status} with {paid} paid"));
            }
        } else {
            tally.hold("N2 a task with no grant holds and pays nothing", paid + held + refunded == 0, format!("task {id}"));
        }
        if let Some(was) = finals.get(&id) {
            tally.hold("N5 a final task never changes status", *was == status, format!("task {id}: {was} -> {status}"));
        } else if is_final(&status) {
            finals.insert(id, status.clone());
        }
        let poster = t["spec"]["grant_id"].as_u64().unwrap();
        let entry = promised.entry(poster).or_default();
        if !is_final(&status) {
            entry.0 += if status == "Open" { u(&t["spec"]["max_budget"]) } else { price - u(&t["drawn"]) };
            entry.1 += 1;
        }
        // The contract's winner and price against the model, from the revealed bids alone.
        if status != "Open" && status != "Void" {
            let bids: Vec<(u128, usize)> = t["bids"].as_array().unwrap().iter().enumerate().filter(|(_, b)| !b["price"].is_null()).map(|(i, b)| (u(&b["price"]), i)).collect();
            let want = model_pick(&bids, u(&t["spec"]["max_budget"]), t["spec"]["min_bids"].as_u64().unwrap());
            let got = t["winner"].as_str().map(|w| (t["bids"].as_array().unwrap().iter().position(|b| b["worker"] == w).unwrap(), price));
            tally.hold("N8 winner and price equal the model's", want == got, format!("task {id}: contract {got:?}, model {want:?}"));
        }
    }
    for (poster, (budget, open)) in &promised {
        let s = m.stat(*poster).await?;
        tally.hold("N6 promised budget is the sum over open tasks", u(&s["committed"]) == *budget && s["open"].as_u64() == Some(*open), format!("grant {poster}: {} / {} against {budget} / {open}", s["committed"], s["open"]));
    }
    let s = m.view("solvency", json!({})).await?;
    let (bonds, balances, held) = (u(&s["bonds_held"]), u(&s["balances"]), u(&s["held"]));
    tally.hold("N3 held draws add up", held == held_sum, format!("{held} != {held_sum}"));
    let storage = m.market.view_account().await?.storage_usage as u128 * 10u128.pow(19);
    tally.hold("N3 balance covers everything owed plus storage", u(&s["balance"]) >= bonds + balances + held + storage, format!("{} < {}", s["balance"], bonds + balances + held + storage));
    // Every yocto that came in is locked, owed, held, or was paid out. Bonds in plus draws in.
    tally.hold(
        "N7 money in == bonds locked + owed + held + paid out",
        tally.bonds_in + direct_total == bonds + balances + held + tally.paid_out,
        format!("{} + {direct_total} != {bonds} + {balances} + {held} + {}", tally.bonds_in, tally.paid_out),
    );
    let _ = states;
    Ok(())
}

pub async fn run(root: &Account, waves: usize, seed: u64, report: Option<&str>) -> Result<bool> {
    let started = std::time::Instant::now();
    let e = crate::fresh_env(root, 60, 60).await?;
    let mut m = Mk::new(&e, 8, 3000).await?;
    // A root with room for many grants, and accounts with room for many bonds in flight.
    let mut big = m.gc_tm.clone();
    big.node_budget = 3000;
    big.gas_budget = 3000 * 400 * MILLI;
    big.expires_at_ms -= 1;
    m.gc = e.root(&big).await?;
    m.gc_tm = big;
    for who in m.w.iter().chain([&m.relay]) {
        root.transfer_near(who.id(), near_workspaces::types::NearToken::from_near(6)).await?.into_result()?;
    }
    let m = &m;
    let mut rng = SmallRng::seed_from_u64(seed);
    let mut tally = Tally::default();
    let mut finals = BTreeMap::new();
    let mut states: Vec<St> = vec![];
    let mut last_review = 0u64;
    let mut root_strayed = false;

    for wave in 0..waves {
        // Posters: the root, and every winner that may still subcontract.
        let mut posters: Vec<(Agent, Tm, Account)> = vec![(m.gc.clone(), m.gc_tm.clone(), m.relay.clone())];
        for s in &states {
            if let (Some(a), Some(tm), Some(w)) = (&s.agent, &s.tm, &s.worker) {
                if tm.max_depth > 0 && e.grant(a.grant).await?["live"] == json!(true) {
                    posters.push((a.clone(), tm.clone(), w.clone()));
                }
            }
        }
        posters[1..].shuffle(&mut rng);
        let base = after_reveal(shift(m.spec(&m.gc, 10).await?, "commit_end_ms", 32_000), 70_000, 130_000, 190_000);
        last_review = last_review.max(base["review_end_ms"].as_u64().unwrap());
        let first_new = states.len();
        for (agent, tm, carrier) in posters.iter().take(4) {
            let posts = if agent.grant == m.gc.grant { rng.gen_range(4..=6) } else { rng.gen_range(1..=2) };
            for _ in 0..posts {
                let mut spec = twin(base.clone());
                spec["grant_id"] = json!(agent.grant);
                // Sometimes far more than the poster can have left.
                let max = if rng.gen_bool(0.15) { rng.gen_range(2000..4000u128) } else { rng.gen_range(20..160u128) };
                spec["max_budget"] = json!((max * MILLI).to_string());
                spec["min_bids"] = json!(if rng.gen_bool(0.3) { 2 } else { 1 });
                let depth = if tm.max_depth > 1 { rng.gen_range(0..tm.max_depth.min(3)) } else { 0 };
                spec["sub_depth"] = json!(depth);
                spec["sub_nodes"] = json!(4u32 * depth as u32);
                spec["sub_gas"] = json!((4 * depth as u128 * 400 * MILLI).to_string());
                // Sometimes signed by a key that is not the poster's.
                let signer = if rng.gen_bool(0.07) { SecretKey::from_random(KeyType::ED25519) } else { agent.sk.clone() };
                let sig = m.sign_post(&signer, &spec).await?;
                let r = m.post_raw(carrier, &spec, &sig, POSTER_BOND).await?;
                let code = code_of(&r);
                tally.note("post_task", &code);
                if code == "POSTED" {
                    tally.bonds_in += POSTER_BOND;
                    let id = r.json::<Value>()?["task"].as_u64().unwrap();
                    states.push(St { t: Tk { id, spec, poster: agent.clone(), ptm: tm.clone() }, bids: vec![], agent: None, tm: None, worker: None });
                }
            }
        }
        // Bids: a few workers per task, some with a price over the budget, some with a wrong bond.
        for s in states[first_new..].iter_mut() {
            let mut ws: Vec<&Account> = m.w.iter().collect();
            ws.shuffle(&mut rng);
            let max = u(&s.t.spec["max_budget"]) / MILLI;
            let bids: Vec<(Bidv, u128)> = ws
                .into_iter()
                .take(rng.gen_range(1..=5))
                .map(|w| {
                    let price = if rng.gen_bool(0.05) { max + 1 } else { rng.gen_range(1..=max) };
                    (Bidv::new(w, price), if rng.gen_bool(0.05) { BID_BOND / 2 } else { BID_BOND })
                })
                .collect();
            let sent = join_all(bids.iter().map(|(b, bond)| async {
                let c = m.commitment(s.t.id, b).await?;
                m.call(&b.who, "commit_bid", json!({"task_id": s.t.id, "commitment": c}), *bond, 30).await
            }))
            .await;
            for ((b, _), r) in bids.into_iter().zip(sent) {
                let r = r?;
                tally.note("commit_bid", &code_of(&r));
                if r.is_success() {
                    tally.bonds_in += BID_BOND;
                    s.bids.push(b);
                }
            }
        }
        if let Some(s) = states.get(first_new) {
            m.at(&s.t, "commit_end_ms").await?;
        }
        // Reveals: most bids open, a few stay silent, a few try a wrong salt first.
        for s in states[first_new..].iter() {
            let plan: Vec<(&Bidv, bool, bool)> = s.bids.iter().map(|b| (b, rng.gen_bool(0.9), rng.gen_bool(0.08))).collect();
            let sent = join_all(plan.iter().filter(|(_, open, _)| *open).map(|(b, _, wrong)| async move {
                let mut codes = vec![];
                if *wrong {
                    let mut bad = (*b).clone();
                    bad.salt = [9; 32];
                    codes.push(code_of(&m.reveal(s.t.id, &bad).await?));
                }
                codes.push(code_of(&m.reveal(s.t.id, b).await?));
                Ok::<_, anyhow::Error>(codes)
            }))
            .await;
            for codes in sent {
                for c in codes? {
                    tally.note("reveal_bid", &c);
                }
            }
        }
        if let Some(s) = states.get(first_new) {
            m.at(&s.t, "reveal_end_ms").await?;
        }
        for s in states[first_new..].iter() {
            call(m, &mut tally, &m.relay, "close_bidding", json!({"task_id": s.t.id}), 0, 100).await?;
        }
        // Awards: right, wrong, missing, or the poster is revoked first.
        for i in first_new..states.len() {
            let view = m.task(states[i].t.id).await?;
            if view["status"] != "Awarding" {
                continue;
            }
            let price = u(&view["price"]);
            let winner = view["winner"].as_str().unwrap().to_string();
            let b = states[i].bids.iter().find(|b| b.who.id().as_str() == winner).unwrap().clone();
            let roll: f64 = rng.gen();
            let right = m.award_tm(&states[i].t, price);
            if roll < 0.08 {
                continue;
            }
            if roll < 0.14 && states[i].t.poster.grant != m.gc.grant {
                let (d, _) = e.revoke_by_owner(states[i].t.poster.grant).await?;
                tally.note("vault.revoke", &d.code);
                continue;
            }
            if roll < 0.26 {
                let mut wrong = right.clone();
                wrong.cap = price - 1;
                wrong.per_tx = wrong.cap;
                wrong.window_cap = wrong.cap;
                wrong.threshold = wrong.cap;
                let d = e.delegate_raw(&states[i].t.poster, &wrong, &b.sk.public_key()).await?;
                tally.note("vault.delegate", &d.code);
                if let Some(g) = d.new_grant {
                    let r = m.confirm(&states[i].t, g).await?;
                    tally.note("confirm_award", &code_of(&r));
                    if rng.gen_bool(0.5) {
                        continue;
                    }
                    let d = e.revoke_by(&states[i].t.poster, g).await?;
                    tally.note("vault.revoke", &d.code);
                }
            }
            let d = e.delegate_raw(&states[i].t.poster, &right, &b.sk.public_key()).await?;
            tally.note("vault.delegate", &d.code);
            if let Some(g) = d.new_grant {
                let r = m.confirm(&states[i].t, g).await?;
                let code = code_of(&r);
                tally.note("confirm_award", &code);
                if code == "AWARDED" {
                    states[i].agent = Some(Agent { acct: e.vault.clone_with_key(b.sk.clone()), pk: b.sk.public_key(), sk: b.sk.clone(), grant: g });
                    states[i].tm = Some(right);
                    states[i].worker = Some(b.who.clone());
                }
            }
        }
        // Work, review and draws, over every task that has a grant, old and new.
        for i in 0..states.len() {
            let (Some(a), Some(w)) = (states[i].agent.clone(), states[i].worker.clone()) else { continue };
            let t = states[i].t.clone();
            let view = m.task(t.id).await?;
            let status = view["status"].as_str().unwrap().to_string();
            let price = u(&view["price"]);
            let live = e.grant(a.grant).await?["live"] == json!(true);
            if live && rng.gen_bool(0.35) {
                // A draw at any time, sometimes for more than the grant has.
                let amount = if rng.gen_bool(0.2) { price + MILLI } else { rng.gen_range(1..=price / MILLI) * MILLI };
                draw(m, &mut tally, &a, amount).await;
            }
            if status == "Awarded" && rng.gen_bool(if states[i].tm.as_ref().unwrap().max_depth > 0 { 0.5 } else { 0.85 }) {
                let r = m.deliver(&t, &w).await?;
                tally.note("deliver", &code_of(&r));
            }
            if m.status(t.id).await? == "Delivered" {
                let roll: f64 = rng.gen();
                if roll < 0.65 {
                    let signer = if rng.gen_bool(0.1) { a.sk.clone() } else { t.poster.sk.clone() };
                    tally.note("accept", &code_of(&m.accept_as(&t, &signer).await?));
                } else if roll < 0.8 {
                    tally.note("reject", &code_of(&m.reject_as(&t, &t.poster.sk).await?));
                }
            }
            if m.status(t.id).await? == "Accepted" && live {
                let left = u(&e.grant(a.grant).await?["spendable"]);
                if left > 0 {
                    draw(m, &mut tally, &a, left.min(price)).await;
                }
            }
        }
        // A revoke somewhere in the tree, by the owner.
        if rng.gen_bool(0.5) {
            let live: Vec<u64> = states.iter().filter_map(|s| s.agent.as_ref().map(|a| a.grant)).collect();
            if let Some(g) = live.choose(&mut rng) {
                let (d, _) = e.revoke_by_owner(*g).await?;
                tally.note("vault.revoke", &d.code);
            }
        }
        if !root_strayed && wave == 1 {
            draw(m, &mut tally, &m.gc, 7 * MILLI).await;
            root_strayed = true;
        }
        sweep(m, &e, &states, &mut tally, &mut rng).await?;
        check(m, &e, &states, &mut finals, &mut tally).await?;
        println!("wave {wave}: {} tasks, {} transactions, {} checks, {} broken", states.len(), tally.sent, tally.checks, tally.broken.len());
        for b in &tally.broken {
            println!("    BROKEN {b}");
        }
    }
    // Let every deadline pass, then sweep until nothing moves.
    while e.now_ms().await? < last_review + 2_000 {
        e.sleep_ms(1_000).await;
    }
    for _ in 0..2 {
        sweep(m, &e, &states, &mut tally, &mut rng).await?;
        check(m, &e, &states, &mut finals, &mut tally).await?;
    }
    let mut statuses: BTreeMap<String, u64> = BTreeMap::new();
    let mut open_with_live_grant = 0;
    for s in &states {
        let status = m.status(s.t.id).await?;
        if !is_final(&status) {
            let live = match &s.agent {
                Some(a) => e.grant(a.grant).await?["live"] == json!(true),
                None => false,
            };
            tally.hold("N5 after every deadline, only accepted tasks with a live grant are still open", status == "Accepted" && live, format!("task {} is {status}", s.t.id));
            open_with_live_grant += 1;
        }
        *statuses.entry(status).or_default() += 1;
    }
    let doc = json!({
        "what": "random operations against vault and market on a real local node",
        "seed": seed,
        "waves": waves,
        "tasks": states.len(),
        "transactions": tally.sent,
        "invariant_checks": tally.checks,
        "broken": tally.broken,
        "final_statuses": statuses,
        "accepted_and_still_drawable": open_with_live_grant,
        "bonds_in_yocto": tally.bonds_in.to_string(),
        "paid_out_yocto": tally.paid_out.to_string(),
        "calls_by_result": tally.by_code,
        "vault_wasm_sha256": crate::report::wasm_sha256(),
        "market_wasm_sha256": crate::report::sha256_of("espalier_market"),
        "git_commit": crate::report::git_commit(),
        "seconds": started.elapsed().as_secs(),
    });
    println!("{}", serde_json::to_string_pretty(&doc)?);
    if let Some(p) = report {
        std::fs::write(format!("{p}.json"), serde_json::to_string_pretty(&doc)?)?;
    }
    Ok(tally.broken.is_empty())
}

/// Every call anyone may make, on every task, plus payouts. Most are refused, which is the point.
async fn sweep(m: &Mk, e: &Env, states: &[St], tally: &mut Tally, rng: &mut SmallRng) -> Result<()> {
    for s in states {
        let id = s.t.id;
        let view = m.task(id).await?;
        let status = view["status"].as_str().unwrap();
        if is_final(status) && view["grant"].is_null() {
            continue;
        }
        let caller = m.w.choose(rng).unwrap();
        match status {
            "Awarding" => {
                call(m, tally, caller, "default_award", json!({"task_id": id}), 0, 200).await?;
            }
            "Awarded" => {
                call(m, tally, caller, "fail_delivery", json!({"task_id": id}), 0, 100).await?;
            }
            _ => {}
        }
        if matches!(m.status(id).await?.as_str(), "Open" | "Awarding" | "Awarded" | "Delivered") {
            call(m, tally, caller, "void_task", json!({"task_id": id}), 0, 100).await?;
        }
        if view["grant"].is_u64() {
            call(m, tally, caller, "claim", json!({"task_id": id}), 0, 100).await?;
        }
        let view = m.task(id).await?;
        if is_final(view["status"].as_str().unwrap()) && u(&view["held"]) > 0 {
            call(m, tally, caller, "refund", json!({"task_id": id}), 0, 60).await?;
        }
    }
    if u(&e.grant(m.gc.grant).await?["direct_spent"]) > 0 {
        call(m, tally, &m.relay, "return_unbound", json!({"vault": e.vault_id(), "grant_id": m.gc.grant}), 0, 100).await?;
    }
    let mut accounts: Vec<AccountId> = m.w.iter().map(|w| w.id().clone()).collect();
    accounts.push(m.relay.id().clone());
    accounts.push(e.vault_id().clone());
    for who in accounts {
        let owed = m.owed(&who).await?;
        if owed > 0 && rng.gen_bool(0.6) {
            let r = m.payout(&who).await?;
            tally.note("payout", &code_of(&r));
            if r.is_success() {
                tally.paid_out += owed;
            }
        }
    }
    Ok(())
}
