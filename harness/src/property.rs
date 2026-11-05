//! The property test on a real node: random operations as signed transactions, submitted in
//! parallel batches from many keys so that receipts really interleave. The tree runs out of budget
//! after a few hundred operations, so the run goes in rounds, each with a fresh tree on one vault.
//!
//! After every batch the oracle reads every grant of the round from the chain and checks:
//!   R1 spent + reserved <= cap on every grant
//!   R2 each grant's spent equals its direct spent plus its children's spent
//!   R3 each grant's spent equals the sum of settled decisions in that subtree, seen by the callers
//!   R4 no spend settles from a key whose revoke had finished before the batch started
//!   R5 once a batch is done, nothing is reserved anywhere
//!   R6 a denied or rolled-back decision adds nothing to any counter (R3 would catch it)
//! And once at the end:
//!   R7 the recipients' balances grew by exactly the settled total
//! With --screened every spend also goes through a real registry contract that lists one recipient:
//!   R8 the listed recipient's balance never grows, and no native funds stay held after a batch
use crate::env::*;
use crate::fresh_env;
use anyhow::{anyhow, Result};
use futures::future::join_all;
use near_workspaces::{Account, AccountId};
use rand::rngs::SmallRng;
use rand::{Rng, SeedableRng};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::time::Instant;

const ROUND_OPS: u64 = 600;

struct Node {
    agent: Agent,
    parent: Option<usize>,
    terms: Tm,
    settled: u128,
    dead: bool,
}

fn lineage(nodes: &[Node], mut i: usize) -> Vec<usize> {
    let mut out = vec![i];
    while let Some(p) = nodes[i].parent {
        out.push(p);
        i = p;
    }
    out
}

/// A tree of `keys` grants over up to 4 levels, with random and tight limits so ancestors refuse.
async fn build_tree(e: &Env, rng: &mut SmallRng, keys: usize, nobody: &AccountId) -> Result<Vec<Node>> {
    let mut t = Tm::root(e.now_ms().await?);
    t.cap = 3_000 * MILLI;
    t.per_tx = 40 * MILLI;
    t.window_cap = 2_000 * MILLI;
    t.threshold = 40 * MILLI;
    t.max_denials = 2_000_000;
    t.key_allowance = 8_000 * MILLI;
    t.max_depth = 3;
    let root = e.root(&t).await?;
    let mut nodes = vec![Node { agent: root, parent: None, terms: t, settled: 0, dead: false }];
    while nodes.len() < keys {
        let idx = if nodes.len() < 3 { nodes.len() - 1 } else { rng.gen_range(0..nodes.len()) };
        if nodes[idx].terms.max_depth == 0 {
            continue;
        }
        let p = nodes[idx].terms.clone();
        let mut c = p.child();
        c.key_allowance = 3_000 * MILLI;
        c.gas_budget = p.gas_budget / 20;
        c.node_budget = p.node_budget / 20;
        c.cap = rng.gen_range(p.cap / 3..=p.cap);
        c.per_tx = rng.gen_range(5 * MILLI..=p.per_tx);
        c.window_cap = rng.gen_range(p.window_cap / 2..=p.window_cap);
        if c.recipients.is_none() && rng.gen_bool(0.4) {
            c.recipients = Some(vec![e.r(0).to_string(), e.r(1).to_string(), nobody.to_string()]);
        }
        let agent = e.child(&nodes[idx].agent, &c).await?;
        nodes.push(Node { agent, parent: Some(idx), terms: c, settled: 0, dead: false });
    }
    Ok(nodes)
}

fn check_views(nodes: &[Node], views: &[Value]) -> Result<()> {
    let num = |v: &Value, f: &str| -> u128 { v[f].as_str().unwrap().parse().unwrap() };
    for (i, v) in views.iter().enumerate() {
        if num(v, "spent") + num(v, "reserved") > num(v, "cap") {
            return Err(anyhow!("R1 broken on node {i}"));
        }
        let kids: u128 = (0..nodes.len()).filter(|c| nodes[*c].parent == Some(i)).map(|c| num(&views[c], "spent")).sum();
        if num(v, "spent") != num(v, "direct_spent") + kids {
            return Err(anyhow!("R2 broken on node {i}"));
        }
        if num(v, "spent") != nodes[i].settled {
            return Err(anyhow!("R3 broken on node {i}: chain {} ledger {}", num(v, "spent"), nodes[i].settled));
        }
        if num(v, "reserved") != 0 {
            return Err(anyhow!("R5 reservation left after the batch on node {i}"));
        }
    }
    Ok(())
}

/// Puts a real registry in front of the vault and lists the last recipient through two reporters.
async fn screen(e: &Env) -> Result<()> {
    let reg = e.use_registry().await?;
    let listed = e.recipients.last().unwrap().id().clone();
    let reporters = [e.base.clone(), e.owner.clone()];
    for (i, who) in reporters.iter().enumerate() {
        who.call(&reg, "report")
            .args_json(json!({
                "subject": listed, "category": "property-test", "evidence_sha256": format!("{:064x}", i + 1),
                "evidence_uri": "evidence://property", "ttl_secs": 15_000_000,
            }))
            .deposit(near_workspaces::types::NearToken::from_millinear(10))
            .gas(near_workspaces::types::Gas::from_tgas(30))
            .transact()
            .await?
            .into_result()?;
    }
    e.owner_call("set_screen", json!({"screen": {
        "registry": reg,
        "reporters": reporters.iter().map(|a| a.id().to_string()).collect::<Vec<_>>(),
        "quorum": 2,
    }}))
    .await?
    .into_result()?;
    Ok(())
}

pub async fn run(root: &Account, ops: u64, seed: u64, keys: usize, screened: bool, report: Option<&str>) -> Result<bool> {
    let started = Instant::now();
    let mut rng = SmallRng::seed_from_u64(seed);
    let e = fresh_env(root, 1_500, 60).await?;
    if screened {
        screen(&e).await?;
    }
    let nobody: AccountId = format!("nobody-here.{}", e.owner.id()).parse()?;
    let mut start_balances = vec![];
    for r in &e.recipients {
        start_balances.push(e.balance(r).await?);
    }
    let mut done = 0u64;
    let mut outcomes: BTreeMap<String, u64> = BTreeMap::new();
    let mut native_out = 0u128;
    let mut batches = 0u64;
    let mut rounds = 0u64;
    let mut revokes = 0u64;
    let mut raced = 0u64;

    while done < ops {
        rounds += 1;
        let mut nodes = build_tree(&e, &mut rng, keys, &nobody).await?;
        let round_end = (done + ROUND_OPS).min(ops);
        while done < round_end {
            batches += 1;
            // One tx per key, all in flight together. Dead keys keep trying: R4 needs attempts to judge.
            let mut calls = vec![];
            let mut plan = vec![];
            for (i, n) in nodes.iter().enumerate() {
                let amount = match rng.gen_range(0..10) {
                    0 => n.terms.per_tx + 1,
                    _ => rng.gen_range(1..=n.terms.per_tx),
                };
                let to = if rng.gen_bool(0.15) {
                    nobody.clone()
                } else {
                    e.r(rng.gen_range(0..e.recipients.len()))
                };
                plan.push((i, amount));
                let e2 = e.clone();
                let agent = n.agent.clone();
                calls.push(async move { e2.spend_tx(&agent, &to, amount, None).await });
            }
            // Now and then revoke one grant in the same breath as the spends.
            let revoke_target = if batches % 25 == 0 {
                let live: Vec<usize> = (1..nodes.len()).filter(|i| !nodes[*i].dead).collect();
                if live.len() > 2 { Some(live[rng.gen_range(0..live.len())]) } else { None }
            } else {
                None
            };
            let revoke_fut = async {
                match revoke_target {
                    Some(i) => e.revoke_by_owner(nodes[i].agent.grant).await.map(|_| ()),
                    None => Ok(()),
                }
            };
            let (results, rv) = tokio::join!(join_all(calls), revoke_fut);
            rv?;
            for ((i, amount), r) in plan.into_iter().zip(results) {
                done += 1;
                let in_revoked_subtree = lineage(&nodes, i).iter().any(|x| revoke_target == Some(*x));
                let d: Decision = match r {
                    Ok(r) => r.json()?,
                    Err(err) => {
                        // A key the sweep has deleted is refused by the protocol.
                        *outcomes.entry("protocol_refused".into()).or_default() += 1;
                        if !nodes[i].dead && !in_revoked_subtree {
                            return Err(anyhow!("live key refused by protocol: {err:?}"));
                        }
                        continue;
                    }
                };
                let label = if d.outcome == "denied" { format!("denied:{}", d.code) } else { d.outcome.clone() };
                *outcomes.entry(label).or_default() += 1;
                if d.outcome == "settled" {
                    native_out += amount;
                    if nodes[i].dead {
                        return Err(anyhow!("R4 spend settled after the revoke finished on node {i}"));
                    }
                    if in_revoked_subtree {
                        raced += 1;
                    }
                    for a in lineage(&nodes, i) {
                        nodes[a].settled += amount;
                    }
                }
            }
            if let Some(target) = revoke_target {
                revokes += 1;
                for i in 0..nodes.len() {
                    if lineage(&nodes, i).contains(&target) {
                        nodes[i].dead = true;
                    }
                }
            }
            let mut views = vec![];
            for n in &nodes {
                views.push(e.grant(n.agent.grant).await?);
            }
            check_views(&nodes, &views)?;
            if screened && e.info().await?["native_held"].as_str() != Some("0") {
                return Err(anyhow!("R8 funds still held after the batch"));
            }
            if batches % 20 == 0 {
                println!("{done}/{ops} ops, round {rounds}, {:.0}s, {outcomes:?}", started.elapsed().as_secs_f64());
            }
            if nodes.iter().filter(|n| !n.dead).count() < 2 {
                break;
            }
        }
    }
    e.sleep_ms(2000).await;
    let mut got = 0u128;
    for (r, s) in e.recipients.iter().zip(&start_balances) {
        got += e.balance(r).await? - s;
    }
    if got != native_out {
        return Err(anyhow!("R7 recipients got {got}, settled {native_out}"));
    }
    if screened && e.balance(e.recipients.last().unwrap()).await? != *start_balances.last().unwrap() {
        return Err(anyhow!("R8 the listed recipient was paid"));
    }
    let doc = json!({
        "harness": "real NEAR runtime: near-workspaces sandbox (neard 2.14.0), compiled wasm, signed transactions, parallel batches",
        "screened": screened,
        "seed": seed,
        "keys_per_tree": keys,
        "operations": done,
        "rounds": rounds,
        "batches": batches,
        "seconds": started.elapsed().as_secs_f64(),
        "outcomes": outcomes,
        "revokes_racing_a_batch": revokes,
        "spends_that_settled_in_the_same_batch_as_their_revoke": raced,
        "native_paid_yocto": native_out.to_string(),
        "invariants": if screened { json!(["R1","R2","R3","R4","R5","R6","R7","R8"]) } else { json!(["R1","R2","R3","R4","R5","R6","R7"]) },
        "wasm_sha256": crate::report::wasm_sha256(),
        "git_commit": crate::report::git_commit(),
    });
    println!("{}", serde_json::to_string_pretty(&doc)?);
    if let Some(p) = report {
        std::fs::write(format!("{p}.json"), serde_json::to_string_pretty(&doc)?)?;
    }
    Ok(true)
}
