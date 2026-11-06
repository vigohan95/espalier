//! Market scenarios: sealed-bid subcontracting where the prize is a grant in the vault. Every one
//! runs real signed transactions against the compiled wasm of both contracts on a NEAR node.
#![allow(unused_must_use)]
use crate::env::*;
use crate::scenarios::Scenario;
use anyhow::{anyhow, Result};
use ed25519_dalek::{Signer, SigningKey};
use futures::future::join_all;
use near_workspaces::result::ExecutionFinalResult;
use near_workspaces::types::{Gas, KeyType, NearToken, SecretKey};
use near_workspaces::{Account, AccountId};
use serde_json::{json, Value};

macro_rules! sc {
    ($id:literal, $title:literal, $f:ident) => {
        Scenario { id: $id, group: "market", title: $title, testnet: false, run: |e| Box::pin($f(e)) }
    };
}

pub fn all() -> Vec<Scenario> {
    vec![
        sc!("M01", "one task end to end: sealed bids, second price, a grant as the prize, pay on acceptance", m01),
        sc!("M02", "three levels of subcontracting: every draw is charged to every level above", m02),
        sc!("M03", "a lone bidder is paid the maximum budget, and too few bids award nothing", m03),
        sc!("M04", "equal lowest bids: the earlier commitment wins", m04),
        sc!("M05", "a post needs the poster's grant key: wrong key, tampered post and replay are refused", m05),
        sc!("M06", "a revoked or unknown grant, or an unlisted vault, cannot post", m06),
        sc!("M07", "a grant that can pay anyone else, or needs clearance tickets, cannot post", m07),
        sc!("M08", "a poster cannot promise more than its grant can still spend", m08),
        sc!("M09", "a poster without the depth, nodes or lifetime for the award cannot post", m09),
        sc!("M10", "only the vault's owner names the workers, and only they can bid", m10),
        sc!("M11", "windows are enforced: late commit, early reveal, late reveal, early close", m11),
        sc!("M12", "a reveal must match its commitment, and a copied commitment cannot be opened", m12),
        sc!("M13", "a bid that is never opened loses its bond to the vault, and the others get theirs back", m13),
        sc!("M14", "wrong bond, second bid, price over the budget and a key already in use are refused", m14),
        sc!("M15", "bidding lower does not raise the winner's pay, and a cartel is capped by the budget", m15),
        sc!("M16", "a grant with the wrong cap, key, parent, limits or lifetime is not the award", m16),
        sc!("M17", "a poster that never awards loses its bond to the winner, and is banned after three", m17),
        sc!("M18", "a squatted key voids the task with nobody punished; a wrong-terms grant is a default", m18),
        sc!("M19", "only the winner delivers, and a missed delivery sends its bond to the vault", m19),
        sc!("M20", "a rejected winner is not paid: what it draws is held and goes back to the vault", m20),
        sc!("M21", "acceptance needs the poster's key: the winner cannot accept its own work", m21),
        sc!("M22", "silence until review_end is acceptance", m22),
        sc!("M23", "a draw before acceptance is held, never paid early, and never above the price", m23),
        sc!("M24", "a hijacked contractor cannot pay an outside account, directly or through the market", m24),
        sc!("M25", "poster revoked during bidding: the task is void and every bond goes back", m25),
        sc!("M26", "winner revoked during the work: void, bond returned, nothing paid", m26),
        sc!("M27", "one revoke of a contractor ends every task below it, in the vault and in the market", m27),
        sc!("M28", "revoked after acceptance and before the draw: nothing more can be drawn", m28),
        sc!("M29", "paid before the revoke stays paid, and the tree's counters match the money", m29),
        sc!("M30", "a task cannot be voided while its grants are live", m30),
        sc!("M31", "a root that pays the market by mistake gets the money back to the vault", m31),
        sc!("M32", "a contractor that draws early starves its own subcontractor, and the tree still holds", m32),
        sc!("M33", "sixteen bidders: commit, reveal and close stay within gas, and the books balance", m33),
        sc!("M34", "the market never owes more than it holds, at every step of a task", m34),
        sc!("M35", "two tasks from one poster: winners draw side by side and stop at the poster's cap", m35),
    ]
}

pub const BID_BOND: u128 = 10 * MILLI;
pub const POSTER_BOND: u128 = 20 * MILLI;
const DRAW_GRACE_MS: u64 = 1_000;

/// Length of the commit window, measured from before the post is sent. Slower machines can raise it.
fn commit_ms() -> u64 {
    std::env::var("MARKET_COMMIT_MS").ok().and_then(|v| v.parse().ok()).unwrap_or(15_000)
}

fn reveal_ms() -> u64 {
    std::env::var("MARKET_REVEAL_MS").ok().and_then(|v| v.parse().ok()).unwrap_or(13_000)
}

fn hexs(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
}

/// Signs with the key a grant is bound to, the way an agent would off chain.
pub fn sign(sk: &SecretKey, message: &[u8]) -> String {
    let text = sk.to_string();
    let raw = bs58::decode(text.split(':').nth(1).unwrap()).into_vec().unwrap();
    let key = SigningKey::from_bytes(raw[..32].try_into().unwrap());
    hexs(&key.sign(message).to_bytes())
}

/// The reason code of a call: from the returned value, or from the panic text of a failed one.
pub fn code_of(r: &ExecutionFinalResult) -> String {
    if r.is_success() {
        return r.clone().json::<Value>().ok().and_then(|v| v["code"].as_str().map(str::to_string)).unwrap_or_else(|| "ok".into());
    }
    let text = format!("{:?}", r.failures());
    match text.find("panicked: ") {
        Some(i) => text[i + 10..].split(|c: char| !(c.is_ascii_uppercase() || c == '_')).next().unwrap_or("").to_string(),
        None => "FAILED".to_string(),
    }
}

fn tgas(r: &ExecutionFinalResult) -> f64 {
    r.total_gas_burnt.as_gas() as f64 / 1e12
}

/// One sealed bid: who, what price, which key the grant should carry, and the salt.
#[derive(Clone)]
pub struct Bidv {
    pub who: Account,
    pub price: u128,
    pub sk: SecretKey,
    pub salt: [u8; 32],
}

impl Bidv {
    pub fn new(who: &Account, mnear: u128) -> Self {
        Bidv { who: who.clone(), price: mnear * MILLI, sk: SecretKey::from_random(KeyType::ED25519), salt: rand::random() }
    }

    pub fn pk(&self) -> String {
        self.sk.public_key().to_string()
    }
}

#[derive(Clone)]
pub struct Tk {
    pub id: u64,
    pub spec: Value,
    pub poster: Agent,
    pub ptm: Tm,
}

pub struct Mk {
    pub e: Env,
    pub market: Account,
    pub gc: Agent,
    pub gc_tm: Tm,
    pub w: Vec<Account>,
    /// The general contractor's own account. It carries its signed messages and holds no authority.
    pub relay: Account,
}

impl Mk {
    pub async fn new(e: &Env, workers: usize, root_mnear: u128) -> Result<Mk> {
        let market = e.base.create_subaccount("market").initial_balance(NearToken::from_near(6)).transact().await?.into_result()?;
        market.deploy(&std::fs::read(crate::wasm_path("espalier_market"))?).await?.into_result()?;
        market
            .call(market.id(), "new")
            .args_json(json!({"vaults": [e.vault_id()], "poster_bond": POSTER_BOND.to_string(), "min_window_ms": 1000, "draw_grace_ms": DRAW_GRACE_MS}))
            .transact()
            .await?
            .into_result()?;
        let mut w = vec![];
        for i in 0..workers {
            w.push(e.base.create_subaccount(&format!("w{i}")).initial_balance(NearToken::from_millinear(400)).transact().await?.into_result()?);
        }
        let mut t = Tm::root(e.now_ms().await?);
        t.cap = root_mnear * MILLI;
        t.per_tx = t.cap;
        t.window_cap = t.cap;
        t.threshold = t.cap;
        t.recipients = Some(vec![market.id().to_string()]);
        t.methods = vec!["request_spend".into(), "delegate".into(), "revoke".into()];
        t.max_depth = 4;
        t.node_budget = 60;
        t.gas_budget = 60 * 400 * MILLI;
        let gc = e.root(&t).await?;
        let mk = Mk { e: e.clone(), market, gc, gc_tm: t, w, relay: e.recipients[0].clone() };
        let ids: Vec<&AccountId> = mk.w.iter().map(|a| a.id()).collect();
        let r = mk.call(&e.owner, "set_roster", json!({"vault": e.vault_id(), "workers": ids}), 0, 60).await?;
        if code_of(&r) != "ROSTER_SET" {
            return Err(anyhow!("roster: {}", code_of(&r)));
        }
        Ok(mk)
    }

    pub fn id(&self) -> &AccountId {
        self.market.id()
    }

    pub async fn view(&self, method: &str, args: Value) -> Result<Value> {
        Ok(self.market.view(self.market.id(), method).args_json(args).await?.json()?)
    }

    pub async fn task(&self, id: u64) -> Result<Value> {
        self.view("get_task", json!({"task_id": id})).await
    }

    pub async fn status(&self, id: u64) -> Result<String> {
        Ok(self.task(id).await?["status"].as_str().unwrap_or("").to_string())
    }

    pub async fn owed(&self, who: &AccountId) -> Result<u128> {
        Ok(self.view("balance_of", json!({"account_id": who})).await?.as_str().unwrap().parse()?)
    }

    pub async fn stat(&self, grant: u64) -> Result<Value> {
        self.view("get_poster", json!({"vault": self.e.vault_id(), "grant_id": grant})).await
    }

    /// A call on the market, recorded with its reason code and gas.
    pub async fn call(&self, who: &Account, method: &str, args: Value, deposit: u128, gas: u64) -> Result<ExecutionFinalResult> {
        let r = who
            .call(self.market.id(), method)
            .args_json(args)
            .deposit(NearToken::from_yoctonear(deposit))
            .gas(Gas::from_tgas(gas))
            .transact()
            .await?;
        let code = code_of(&r);
        self.e.trace.txs.lock().unwrap().push(TxRecord {
            label: method.to_string(),
            hash: r.outcome().transaction_hash.to_string(),
            signer: who.id().to_string(),
            method: method.to_string(),
            tgas_burnt: tgas(&r),
            outcome: if r.is_success() && r.clone().json::<Value>().map(|v| v["ok"] != json!(false)).unwrap_or(true) { "ok".into() } else { "refused".into() },
            code,
            detail: r.clone().json::<Value>().ok().and_then(|v| v["detail"].as_str().map(str::to_string)).unwrap_or_default(),
        });
        Ok(r)
    }

    /// Expects a call to end with `code`, whether it returned it or panicked with it.
    pub async fn expect(&self, claim: &str, who: &Account, method: &str, args: Value, deposit: u128, code: &str) -> Result<ExecutionFinalResult> {
        let r = self.call(who, method, args, deposit, 120).await?;
        let got = code_of(&r);
        self.e.check(claim, got == code, code, &got);
        Ok(r)
    }

    /// A spec with the default windows. Scenarios change single fields.
    pub async fn spec(&self, poster: &Agent, max_mnear: u128) -> Result<Value> {
        let now = self.e.now_ms().await?;
        let commit = now + commit_ms();
        let reveal = commit + reveal_ms();
        Ok(json!({
            "vault": self.e.vault_id(),
            "grant_id": poster.grant,
            "spec_hash": hexs(&rand::random::<[u8; 32]>()),
            "max_budget": (max_mnear * MILLI).to_string(),
            "bid_bond": BID_BOND.to_string(),
            "min_bids": 1,
            "commit_end_ms": commit,
            "reveal_end_ms": reveal,
            "award_end_ms": reveal + 60_000,
            "deliver_end_ms": reveal + 120_000,
            "review_end_ms": reveal + 180_000,
            "sub_depth": 0,
            "sub_nodes": 0,
            "sub_gas": "0",
            "key_allowance": (200 * MILLI).to_string(),
            "nonce": rand::random::<u32>(),
        }))
    }

    /// A spec whose winner may subcontract `depth` levels further.
    pub async fn spec_sub(&self, poster: &Agent, max_mnear: u128, depth: u8) -> Result<Value> {
        let mut s = self.spec(poster, max_mnear).await?;
        s["sub_depth"] = json!(depth);
        s["sub_nodes"] = json!(6u32 * depth as u32);
        s["sub_gas"] = json!((6 * depth as u128 * 400 * MILLI).to_string());
        Ok(s)
    }

    pub async fn sign_post(&self, sk: &SecretKey, spec: &Value) -> Result<String> {
        let payload = self.view("post_payload", json!({"spec": spec})).await?;
        Ok(sign(sk, &unhex(payload.as_str().unwrap())))
    }

    pub async fn post_raw(&self, relay: &Account, spec: &Value, signature: &str, bond: u128) -> Result<ExecutionFinalResult> {
        self.call(relay, "post_task", json!({"spec": spec, "signature": signature}), bond, 120).await
    }

    /// Signs and sends a post that is expected to be refused, and returns the reason code.
    pub async fn try_post(&self, poster: &Agent, spec: Value) -> Result<String> {
        let sig = self.sign_post(&poster.sk, &spec).await?;
        Ok(code_of(&self.post_raw(&self.relay, &spec, &sig, POSTER_BOND).await?))
    }

    pub async fn post(&self, poster: &Agent, ptm: &Tm, relay: &Account, spec: Value) -> Result<Tk> {
        let sig = self.sign_post(&poster.sk, &spec).await?;
        let r = self.post_raw(relay, &spec, &sig, POSTER_BOND).await?;
        let v: Value = r.clone().json().map_err(|_| anyhow!("post failed: {}", code_of(&r)))?;
        let id = v["task"].as_u64().ok_or(anyhow!("post refused: {} {}", v["code"], v["detail"]))?;
        Ok(Tk { id, spec, poster: poster.clone(), ptm: ptm.clone() })
    }

    pub async fn commitment(&self, task: u64, b: &Bidv) -> Result<String> {
        let v = self
            .view("commitment_of", json!({"task_id": task, "worker": b.who.id(), "price": b.price.to_string(), "pk": b.pk(), "salt": hexs(&b.salt)}))
            .await?;
        Ok(v.as_str().unwrap().to_string())
    }

    pub async fn commit(&self, task: u64, b: &Bidv) -> Result<ExecutionFinalResult> {
        let c = self.commitment(task, b).await?;
        self.call(&b.who, "commit_bid", json!({"task_id": task, "commitment": c}), BID_BOND, 30).await
    }

    pub async fn reveal(&self, task: u64, b: &Bidv) -> Result<ExecutionFinalResult> {
        self.call(&b.who, "reveal_bid", json!({"task_id": task, "price": b.price.to_string(), "pk": b.pk(), "salt": hexs(&b.salt)}), 0, 80).await
    }

    pub async fn at(&self, t: &Tk, field: &str) -> Result<()> {
        self.e.wait_chain(t.spec[field].as_u64().unwrap() + 1).await
    }

    /// Commits every bid, waits, reveals every bid, waits, and closes. Returns the close result.
    pub async fn auction(&self, t: &Tk, bids: &[Bidv]) -> Result<Value> {
        for r in join_all(bids.iter().map(|b| self.commit(t.id, b))).await {
            let r = r?;
            if !r.is_success() {
                return Err(anyhow!("commit: {}", code_of(&r)));
            }
        }
        self.at(t, "commit_end_ms").await?;
        for r in join_all(bids.iter().map(|b| self.reveal(t.id, b))).await {
            let r = r?;
            if code_of(&r) != "REVEALED" {
                return Err(anyhow!("reveal: {}", code_of(&r)));
            }
        }
        self.close(t).await
    }

    pub async fn close(&self, t: &Tk) -> Result<Value> {
        self.at(t, "reveal_end_ms").await?;
        let r = self.call(&self.relay, "close_bidding", json!({"task_id": t.id}), 0, 60).await?;
        Ok(r.json()?)
    }

    /// The terms of the grant a task promises its winner, at a given price.
    pub fn award_tm(&self, t: &Tk, price: u128) -> Tm {
        let mut c = t.ptm.clone();
        c.cap = price;
        c.per_tx = price;
        c.window_cap = price;
        c.threshold = price;
        let depth = t.spec["sub_depth"].as_u64().unwrap() as u8;
        c.max_depth = depth;
        c.node_budget = t.spec["sub_nodes"].as_u64().unwrap() as u32;
        c.gas_budget = t.spec["sub_gas"].as_str().unwrap().parse().unwrap();
        c.key_allowance = t.spec["key_allowance"].as_str().unwrap().parse().unwrap();
        c.methods = if depth > 0 { vec!["request_spend".into(), "delegate".into(), "revoke".into()] } else { vec!["request_spend".into()] };
        c
    }

    /// The poster creates a grant in the vault for a key. Returns the grant as an agent.
    pub async fn grant_for(&self, poster: &Agent, tm: &Tm, sk: &SecretKey) -> Result<Agent> {
        let d = self.e.delegate_raw(poster, tm, &sk.public_key()).await?;
        let g = d.new_grant.ok_or(anyhow!("delegate refused: {} {}", d.code, d.detail))?;
        Ok(Agent { acct: self.e.vault.clone_with_key(sk.clone()), pk: sk.public_key(), sk: sk.clone(), grant: g })
    }

    pub async fn confirm(&self, t: &Tk, grant: u64) -> Result<ExecutionFinalResult> {
        self.call(&self.relay, "confirm_award", json!({"task_id": t.id, "grant_id": grant}), 0, 80).await
    }

    /// Delegate the right grant and confirm it.
    pub async fn award(&self, t: &Tk, b: &Bidv, price: u128) -> Result<(Agent, Tm)> {
        let tm = self.award_tm(t, price);
        let a = self.grant_for(&t.poster, &tm, &b.sk).await?;
        let r = self.confirm(t, a.grant).await?;
        if code_of(&r) != "AWARDED" {
            return Err(anyhow!("confirm_award: {}", code_of(&r)));
        }
        Ok((a, tm))
    }

    pub async fn deliver(&self, t: &Tk, who: &Account) -> Result<ExecutionFinalResult> {
        self.call(who, "deliver", json!({"task_id": t.id, "result_hash": hexs(&rand::random::<[u8; 32]>())}), 0, 30).await
    }

    pub async fn accept_as(&self, t: &Tk, sk: &SecretKey) -> Result<ExecutionFinalResult> {
        let payload = self.view("accept_payload", json!({"task_id": t.id})).await?;
        let sig = sign(sk, &unhex(payload.as_str().unwrap()));
        self.call(&self.relay, "accept", json!({"task_id": t.id, "signature": sig}), 0, 80).await
    }

    pub async fn reject_as(&self, t: &Tk, sk: &SecretKey) -> Result<ExecutionFinalResult> {
        let reason = hexs(&rand::random::<[u8; 32]>());
        let payload = self.view("reject_payload", json!({"task_id": t.id, "reason_hash": reason})).await?;
        let sig = sign(sk, &unhex(payload.as_str().unwrap()));
        self.call(&self.relay, "reject", json!({"task_id": t.id, "reason_hash": reason, "signature": sig}), 0, 80).await
    }

    /// The winner draws from its own grant. The vault decides.
    pub async fn draw(&self, a: &Agent, amount: u128) -> Result<Decision> {
        self.e.spend(a, self.market.id(), amount).await
    }

    pub async fn claim(&self, t: &Tk) -> Result<Value> {
        let r = self.call(&self.relay, "claim", json!({"task_id": t.id}), 0, 80).await?;
        Ok(r.json()?)
    }

    pub async fn payout(&self, who: &AccountId) -> Result<ExecutionFinalResult> {
        self.call(&self.relay, "payout", json!({"account_id": who}), 0, 30).await
    }

    /// Deliver, accept, draw the whole price, claim. Leaves the money owed to the winner.
    pub async fn finish(&self, t: &Tk, a: &Agent, who: &Account, price: u128) -> Result<()> {
        self.deliver(t, who).await?.into_result()?;
        let r = self.accept_as(t, &t.poster.sk).await?;
        if code_of(&r) != "ACCEPTED" {
            return Err(anyhow!("accept: {}", code_of(&r)));
        }
        let d = self.draw(a, price).await?;
        if d.outcome != "settled" {
            return Err(anyhow!("draw: {} {}", d.code, d.detail));
        }
        self.claim(t).await?;
        Ok(())
    }

    /// Post, run the auction and award the winner, for scenarios that are about what comes after.
    pub async fn hired(&self, poster: &Agent, ptm: &Tm, relay: &Account, spec: Value, bids: &[Bidv]) -> Result<(Tk, Agent, Tm, u128)> {
        let t = self.post(poster, ptm, relay, spec).await?;
        let res = self.auction(&t, bids).await?;
        let price: u128 = res["amount"].as_str().ok_or(anyhow!("no winner: {res}"))?.parse()?;
        let view = self.task(t.id).await?;
        let winner = view["winner"].as_str().unwrap().to_string();
        let b = bids.iter().find(|b| b.who.id().as_str() == winner).unwrap();
        let (a, tm) = self.award(&t, b, price).await?;
        Ok((t, a, tm, price))
    }

    pub async fn solvent(&self, claim: &str) -> Result<()> {
        let s = self.view("solvency", json!({})).await?;
        let owed: u128 = s["owed"].as_str().unwrap().parse()?;
        let balance: u128 = s["balance"].as_str().unwrap().parse()?;
        let storage = self.market.view_account().await?.storage_usage as u128 * 10u128.pow(19);
        self.e.check(claim, balance >= owed + storage, format!("balance >= owed {owed} + storage {storage}"), balance);
        Ok(())
    }
}

const DEADLINES: [&str; 5] = ["commit_end_ms", "reveal_end_ms", "award_end_ms", "deliver_end_ms", "review_end_ms"];

/// Pushes one deadline and every later one out by `by` ms.
fn shift(mut s: Value, from: &str, by: u64) -> Value {
    let start = DEADLINES.iter().position(|d| *d == from).unwrap();
    for d in &DEADLINES[start..] {
        s[*d] = json!(s[*d].as_u64().unwrap() + by);
    }
    s
}

/// Sets the windows after the reveal, in ms after reveal_end.
fn after_reveal(mut s: Value, award: u64, deliver: u64, review: u64) -> Value {
    let r = s["reveal_end_ms"].as_u64().unwrap();
    s["award_end_ms"] = json!(r + award);
    s["deliver_end_ms"] = json!(r + deliver);
    s["review_end_ms"] = json!(r + review);
    s
}

/// The same deadlines under a new brief and nonce, so several tasks can share one timetable.
fn twin(mut s: Value) -> Value {
    s["spec_hash"] = json!(hexs(&rand::random::<[u8; 32]>()));
    s["nonce"] = json!(rand::random::<u32>());
    s
}

fn mn(v: &Value) -> u128 {
    v.as_str().unwrap_or("0").parse::<u128>().unwrap_or(0) / MILLI
}

async fn m01(e: Env) -> Result<()> {
    let m = Mk::new(&e, 3, 300).await?;
    let t = m.post(&m.gc, &m.gc_tm, &m.relay, { let mut s = m.spec(&m.gc, 100).await?; s["min_bids"] = json!(2); s }).await?;
    let bids = [Bidv::new(&m.w[0], 70), Bidv::new(&m.w[1], 40), Bidv::new(&m.w[2], 55)];
    for b in &bids {
        m.commit(t.id, b).await?.into_result()?;
    }
    let sealed = m.task(t.id).await?;
    e.check("while sealed, the chain shows commitments and no price", sealed["bids"].as_array().unwrap().iter().all(|b| b["price"].is_null() && b["pk"].is_null()), "no price, no key", sealed["phase"].to_string());
    m.at(&t, "commit_end_ms").await?;
    for b in &bids {
        let r = m.reveal(t.id, b).await?;
        e.eq("reveal accepted", code_of(&r).as_str(), "REVEALED");
    }
    let res = m.close(&t).await?;
    e.eq("the lowest bid wins", m.task(t.id).await?["winner"].as_str().unwrap(), m.w[1].id().as_str());
    e.eq("at the second-lowest price", mn(&res["amount"]), 55);
    e.eq("losers get their bonds back", (m.owed(m.w[0].id()).await?, m.owed(m.w[2].id()).await?), (BID_BOND, BID_BOND));
    let price = 55 * MILLI;
    let (a, _) = m.award(&t, &bids[1], price).await?;
    let g = e.grant(a.grant).await?;
    e.eq("the prize is a grant under the poster", g["parent"].as_u64(), Some(m.gc.grant));
    e.eq("with the price as its cap", mn(&g["cap"]), 55);
    e.eq("that can pay only the market", g["recipients"].clone(), json!([m.id()]));
    e.eq("the poster's bond is returned at the award", m.owed(m.relay.id()).await?, POSTER_BOND);
    e.eq("nothing has left the vault yet", mn(&e.grant(m.gc.grant).await?["spent"]), 0);
    m.deliver(&t, &m.w[1]).await?.into_result()?;
    let r = m.accept_as(&t, &m.gc.sk).await?;
    e.eq("accepted by the poster's key", code_of(&r).as_str(), "ACCEPTED");
    let d = m.draw(&a, price).await?;
    e.eq("the winner draws its price from its own grant", d.outcome.as_str(), "settled");
    let c = m.claim(&t).await?;
    e.eq("the market forwards exactly the price", mn(&c["amount"]), 55);
    e.eq("owed to the winner: price plus its bond", m.owed(m.w[1].id()).await?, price + BID_BOND);
    let before = e.balance(&m.w[1]).await?;
    m.payout(m.w[1].id()).await?.into_result()?;
    e.eq("the winner's account receives it", e.balance(&m.w[1]).await? - before, price + BID_BOND);
    e.eq("the task is paid", m.status(t.id).await?.as_str(), "Paid");
    e.eq("the tree was charged exactly the price, at the grant", mn(&e.grant(a.grant).await?["spent"]), 55);
    e.eq("and at the root", mn(&e.grant(m.gc.grant).await?["spent"]), 55);
    let s = m.stat(m.gc.grant).await?;
    e.eq("the poster has nothing left promised", (s["open"].as_u64(), mn(&s["committed"])), (Some(0), 0));
    m.solvent("market holds at least what it owes").await
}

async fn m02(e: Env) -> Result<()> {
    let m = Mk::new(&e, 3, 300).await?;
    // Level 1: the general contractor hires w0, who may subcontract two levels down.
    let (t1, a1, tm1, p1) = m.hired(&m.gc, &m.gc_tm, &m.relay, m.spec_sub(&m.gc, 120, 2).await?, &[Bidv::new(&m.w[0], 100)]).await?;
    e.eq("level 1 price: a lone bid is paid the budget", p1 / MILLI, 120);
    // Level 2: w0 posts from its own grant, and w1 wins.
    let (t2, a2, tm2, p2) = m.hired(&a1, &tm1, &m.w[0], m.spec_sub(&a1, 50, 1).await?, &[Bidv::new(&m.w[1], 45)]).await?;
    // Level 3: w1 posts from its grant, and w2 wins.
    let (t3, a3, _, p3) = m.hired(&a2, &tm2, &m.w[1], m.spec(&a2, 20).await?, &[Bidv::new(&m.w[2], 15)]).await?;
    let lineage: Vec<u64> = serde_json::from_value(e.vault.view(e.vault_id(), "get_lineage").args_json(json!({"grant_id": a3.grant})).await?.json()?)?;
    e.eq("the tree grew one level per auction", lineage, vec![a3.grant, a2.grant, a1.grant, m.gc.grant]);
    e.eq("each cap is its auction's price", (mn(&e.grant(a1.grant).await?["cap"]), mn(&e.grant(a2.grant).await?["cap"]), mn(&e.grant(a3.grant).await?["cap"])), (120, 50, 20));
    // Work is delivered and accepted from the bottom up.
    m.finish(&t3, &a3, &m.w[2], p3).await?;
    let spent = |g: Value| mn(&g["spent"]);
    e.eq(
        "the level-3 draw is charged to all four levels",
        (spent(e.grant(a3.grant).await?), spent(e.grant(a2.grant).await?), spent(e.grant(a1.grant).await?), spent(e.grant(m.gc.grant).await?)),
        (20, 20, 20, 20),
    );
    // Level 2 keeps what its subcontractor did not draw: 50 - 20.
    m.deliver(&t2, &m.w[1]).await?.into_result()?;
    m.accept_as(&t2, &a1.sk).await?;
    let d = m.draw(&a2, p2).await?;
    e.code("level 2 cannot draw its whole price: 20 of it is already spent below", &d, "CAP");
    let d = m.draw(&a2, 30 * MILLI).await?;
    e.eq("level 2 draws its margin, 30", d.outcome.as_str(), "settled");
    m.claim(&t2).await?;
    m.deliver(&t1, &m.w[0]).await?.into_result()?;
    m.accept_as(&t1, &m.gc.sk).await?;
    let d = m.draw(&a1, 70 * MILLI).await?;
    e.eq("level 1 draws its margin, 120 - 50", d.outcome.as_str(), "settled");
    m.claim(&t1).await?;
    e.eq(
        "owed to each worker, less the bonds they get back",
        (
            (m.owed(m.w[0].id()).await? - BID_BOND - POSTER_BOND) / MILLI,
            (m.owed(m.w[1].id()).await? - BID_BOND - POSTER_BOND) / MILLI,
            (m.owed(m.w[2].id()).await? - BID_BOND) / MILLI,
        ),
        (70, 30, 20),
    );
    e.eq("the root was charged exactly the level-1 price", spent(e.grant(m.gc.grant).await?), 120);
    e.eq("level 2's task is paid in part and still open for its remainder", m.status(t2.id).await?.as_str(), "Accepted");
    m.solvent("market holds at least what it owes").await
}

async fn m03(e: Env) -> Result<()> {
    let m = Mk::new(&e, 2, 300).await?;
    let t = m.post(&m.gc, &m.gc_tm, &m.relay, m.spec(&m.gc, 80).await?).await?;
    let res = m.auction(&t, &[Bidv::new(&m.w[0], 30)]).await?;
    e.eq("a lone bid of 30 is paid the budget of 80", mn(&res["amount"]), 80);
    let mut s = m.spec(&m.gc, 80).await?;
    s["min_bids"] = json!(2);
    let t2 = m.post(&m.gc, &m.gc_tm, &m.relay, s).await?;
    let res = m.auction(&t2, &[Bidv::new(&m.w[1], 30)]).await?;
    e.eq("one bid where two are needed: no award", res["code"].as_str().unwrap(), "NO_AWARD");
    e.eq("the task is over", m.status(t2.id).await?.as_str(), "NoAward");
    e.eq("the bidder's bond is returned", m.owed(m.w[1].id()).await?, BID_BOND);
    e.eq("the poster's bond is returned", m.owed(m.relay.id()).await?, POSTER_BOND);
    let st = m.stat(m.gc.grant).await?;
    e.eq("only the first task's price is still promised", mn(&st["committed"]), 80);
    Ok(())
}

async fn m04(e: Env) -> Result<()> {
    let m = Mk::new(&e, 3, 300).await?;
    let t = m.post(&m.gc, &m.gc_tm, &m.relay, m.spec(&m.gc, 100).await?).await?;
    let bids = [Bidv::new(&m.w[2], 40), Bidv::new(&m.w[0], 40), Bidv::new(&m.w[1], 90)];
    // Committed one after another, so the order on chain is known.
    for b in &bids {
        m.commit(t.id, b).await?.into_result()?;
    }
    m.at(&t, "commit_end_ms").await?;
    // Revealed in the opposite order: the order of reveals must not matter.
    for b in bids.iter().rev() {
        m.reveal(t.id, b).await?;
    }
    let res = m.close(&t).await?;
    e.eq("the earlier of two equal bids wins", m.task(t.id).await?["winner"].as_str().unwrap(), m.w[2].id().as_str());
    e.eq("and is paid the tied price", mn(&res["amount"]), 40);
    Ok(())
}

async fn m05(e: Env) -> Result<()> {
    let m = Mk::new(&e, 1, 300).await?;
    let spec = m.spec(&m.gc, 50).await?;
    let stranger = SecretKey::from_random(KeyType::ED25519);
    let before = e.balance(&m.relay).await?;
    let sig = m.sign_post(&stranger, &spec).await?;
    let r = m.post_raw(&m.relay, &spec, &sig, POSTER_BOND).await?;
    e.eq("signed by a key that is not the grant's", code_of(&r).as_str(), "BAD_SIGNATURE");
    let good = m.sign_post(&m.gc.sk, &spec).await?;
    let mut bigger = spec.clone();
    bigger["max_budget"] = json!((250 * MILLI).to_string());
    let r = m.post_raw(&m.relay, &bigger, &good, POSTER_BOND).await?;
    e.eq("the carrier raises the budget after signing", code_of(&r).as_str(), "BAD_SIGNATURE");
    let lost = before - e.balance(&m.relay).await?;
    e.check("refused posts return the bond", lost < 5 * MILLI, "under 5 mNEAR of gas", lost);
    let r = m.post_raw(&m.relay, &spec, &good, POSTER_BOND).await?;
    e.eq("the signed post itself goes through", code_of(&r).as_str(), "POSTED");
    let r = m.post_raw(&m.w[0], &spec, &good, POSTER_BOND).await?;
    e.eq("the same signed post sent again", code_of(&r).as_str(), "DUPLICATE_TASK");
    let r = m.post_raw(&m.relay, &m.spec(&m.gc, 50).await?, &good, POSTER_BOND - 1).await?;
    e.eq("a post without the full bond", code_of(&r).as_str(), "WRONG_BOND");
    e.eq("one task exists", m.view("config", json!({})).await?["tasks"].as_u64(), Some(1));
    Ok(())
}

async fn m06(e: Env) -> Result<()> {
    let m = Mk::new(&e, 1, 300).await?;
    let mut tm = m.gc_tm.clone();
    tm.cap = 50 * MILLI;
    tm.per_tx = tm.cap;
    tm.window_cap = tm.cap;
    tm.threshold = tm.cap;
    tm.max_depth = 2;
    tm.node_budget = 6;
    tm.gas_budget = 6 * 400 * MILLI;
    let sub = e.child(&m.gc, &tm).await?;
    let spec = m.spec(&sub, 20).await?;
    let sig = m.sign_post(&sub.sk, &spec).await?;
    e.revoke_by(&m.gc, sub.grant).await?;
    let r = m.post_raw(&m.relay, &spec, &sig, POSTER_BOND).await?;
    e.eq("a revoked grant cannot post, even with a valid signature", code_of(&r).as_str(), "GRANT_NOT_LIVE");
    let mut ghost = m.spec(&m.gc, 20).await?;
    ghost["grant_id"] = json!(9999);
    let sig = m.sign_post(&m.gc.sk, &ghost).await?;
    let r = m.post_raw(&m.relay, &ghost, &sig, POSTER_BOND).await?;
    e.eq("a grant that does not exist", code_of(&r).as_str(), "UNKNOWN_GRANT");
    let mut elsewhere = m.spec(&m.gc, 20).await?;
    elsewhere["vault"] = json!(e.r(1));
    let sig = m.sign_post(&m.gc.sk, &elsewhere).await?;
    let r = m.post_raw(&m.relay, &elsewhere, &sig, POSTER_BOND).await?;
    e.eq("a vault the market was not created with", code_of(&r).as_str(), "VAULT_NOT_LISTED");
    e.eq("no task was created", m.view("config", json!({})).await?["tasks"].as_u64(), Some(0));
    Ok(())
}

async fn m07(e: Env) -> Result<()> {
    let m = Mk::new(&e, 1, 300).await?;
    let now = e.now_ms().await?;
    let mut open = m.gc_tm.clone();
    open.expires_at_ms = now + 86_000_000;
    open.recipients = Some(vec![m.id().to_string(), e.r(1).to_string()]);
    let two = e.root(&open).await?;
    let spec = m.spec(&two, 20).await?;
    let r = m.post_raw(&m.relay, &spec, &m.sign_post(&two.sk, &spec).await?, POSTER_BOND).await?;
    e.eq("a grant with a second recipient", code_of(&r).as_str(), "RECIPIENTS_NOT_MARKET_ONLY");
    open.recipients = None;
    let any = e.root(&open).await?;
    let spec = m.spec(&any, 20).await?;
    let r = m.post_raw(&m.relay, &spec, &m.sign_post(&any.sk, &spec).await?, POSTER_BOND).await?;
    e.eq("a grant that can pay anyone", code_of(&r).as_str(), "RECIPIENTS_NOT_MARKET_ONLY");
    let mut bound = m.gc_tm.clone();
    bound.expires_at_ms = now + 86_000_000;
    bound.clearance = true;
    let cl = e.root(&bound).await?;
    let spec = m.spec(&cl, 20).await?;
    let r = m.post_raw(&m.relay, &spec, &m.sign_post(&cl.sk, &spec).await?, POSTER_BOND).await?;
    e.eq("a grant bound to clearance tickets", code_of(&r).as_str(), "CLEARANCE_BOUND");
    let mut nodel = m.gc_tm.clone();
    nodel.expires_at_ms = now + 86_000_000;
    nodel.methods = vec!["request_spend".into()];
    let nd = e.root(&nodel).await?;
    let spec = m.spec(&nd, 20).await?;
    let r = m.post_raw(&m.relay, &spec, &m.sign_post(&nd.sk, &spec).await?, POSTER_BOND).await?;
    e.eq("a grant whose key cannot delegate", code_of(&r).as_str(), "CANNOT_DELEGATE");
    Ok(())
}

async fn m08(e: Env) -> Result<()> {
    let m = Mk::new(&e, 1, 100).await?;
    let spec = m.spec(&m.gc, 101).await?;
    let r = m.post_raw(&m.relay, &spec, &m.sign_post(&m.gc.sk, &spec).await?, POSTER_BOND).await?;
    e.eq("a budget above everything the grant has", code_of(&r).as_str(), "LIMITS_SHORT");
    let t1 = m.post(&m.gc, &m.gc_tm, &m.relay, { let mut s = m.spec(&m.gc, 60).await?; s["min_bids"] = json!(2); s }).await?;
    let spec = m.spec(&m.gc, 41).await?;
    let r = m.post_raw(&m.relay, &spec, &m.sign_post(&m.gc.sk, &spec).await?, POSTER_BOND).await?;
    e.eq("60 promised, 41 more asked, 100 spendable", code_of(&r).as_str(), "OVERCOMMITTED");
    let t2 = m.post(&m.gc, &m.gc_tm, &m.relay, m.spec(&m.gc, 40).await?).await?;
    e.eq("60 and 40 together fit", mn(&m.stat(m.gc.grant).await?["committed"]), 100);
    // The first task ends with no award, and its promise is released.
    let res = m.close(&t1).await?;
    e.eq("first task: no bids, no award", res["code"].as_str().unwrap(), "NO_AWARD");
    e.eq("only the second task's budget is still promised", mn(&m.stat(m.gc.grant).await?["committed"]), 40);
    let t3 = m.post(&m.gc, &m.gc_tm, &m.relay, m.spec(&m.gc, 60).await?).await;
    e.check("after the release, 60 fits again", t3.is_ok(), "posted", t3.is_ok());
    let _ = t2;
    Ok(())
}

async fn m09(e: Env) -> Result<()> {
    let m = Mk::new(&e, 1, 300).await?;
    let mut s = m.spec(&m.gc, 20).await?;
    s["sub_depth"] = json!(4);
    e.eq("the poster has 4 levels left and the task wants 4 below the winner", m.try_post(&m.gc, s).await?.as_str(), "DEPTH_SHORT");
    let mut s = m.spec(&m.gc, 20).await?;
    s["sub_nodes"] = json!(60);
    e.eq("more nodes than the poster can create", m.try_post(&m.gc, s).await?.as_str(), "NODES_SHORT");
    let mut s = m.spec(&m.gc, 20).await?;
    s["sub_gas"] = json!((60 * 400 * MILLI).to_string());
    e.eq("more gas budget than the poster has", m.try_post(&m.gc, s).await?.as_str(), "GAS_SHORT");
    let mut s = m.spec(&m.gc, 20).await?;
    s["review_end_ms"] = json!(m.gc_tm.expires_at_ms);
    e.eq("the poster's grant would expire before the winner could draw", m.try_post(&m.gc, s).await?.as_str(), "EXPIRES_TOO_SOON");
    let mut s = m.spec(&m.gc, 20).await?;
    s["reveal_end_ms"] = json!(s["commit_end_ms"].as_u64().unwrap() + 10);
    e.eq("a 10 ms reveal window", m.try_post(&m.gc, s).await?.as_str(), "BAD_WINDOWS");
    let mut s = m.spec(&m.gc, 20).await?;
    s["spec_hash"] = json!("zz");
    e.eq("a brief hash that is not a hash", m.try_post(&m.gc, s).await?.as_str(), "BAD_HASH");
    Ok(())
}

async fn m10(e: Env) -> Result<()> {
    let m = Mk::new(&e, 2, 300).await?;
    let outsider = e.recipients[2].clone();
    m.expect("a stranger sets the roster", &outsider, "set_roster", json!({"vault": e.vault_id(), "workers": [outsider.id()]}), 0, "NOT_OWNER").await?;
    m.expect("a worker sets the roster", &m.w[0], "set_roster", json!({"vault": e.vault_id(), "workers": [outsider.id()]}), 0, "NOT_OWNER").await?;
    let roster: Vec<String> = serde_json::from_value(m.view("get_roster", json!({"vault": e.vault_id()})).await?)?;
    e.eq("the roster is still the owner's", roster.len(), 2);
    let t = m.post(&m.gc, &m.gc_tm, &m.relay, m.spec(&m.gc, 50).await?).await?;
    let b = Bidv::new(&outsider, 10);
    let r = m.commit(t.id, &b).await?;
    e.eq("an account off the roster bids", code_of(&r).as_str(), "NOT_ON_ROSTER");
    m.expect("the owner drops a worker", &e.owner, "set_roster", json!({"vault": e.vault_id(), "workers": [m.w[0].id()]}), 0, "ROSTER_SET").await?;
    let r = m.commit(t.id, &Bidv::new(&m.w[1], 10)).await?;
    e.eq("the dropped worker can no longer bid", code_of(&r).as_str(), "NOT_ON_ROSTER");
    let r = m.commit(t.id, &Bidv::new(&m.w[0], 10)).await?;
    e.check("the remaining worker can", r.is_success(), "ok", code_of(&r));
    Ok(())
}

async fn m11(e: Env) -> Result<()> {
    let m = Mk::new(&e, 2, 300).await?;
    let t = m.post(&m.gc, &m.gc_tm, &m.relay, m.spec(&m.gc, 50).await?).await?;
    let a = Bidv::new(&m.w[0], 30);
    let late = Bidv::new(&m.w[1], 20);
    m.commit(t.id, &a).await?.into_result()?;
    let r = m.reveal(t.id, &a).await?;
    e.eq("reveal while bids are still being committed", code_of(&r).as_str(), "WRONG_PHASE");
    m.expect("close while bids are still being committed", &m.relay, "close_bidding", json!({"task_id": t.id}), 0, "WRONG_PHASE").await?;
    m.at(&t, "commit_end_ms").await?;
    let r = m.commit(t.id, &late).await?;
    e.eq("commit after commit_end", code_of(&r).as_str(), "WRONG_PHASE");
    m.expect("close during the reveal window", &m.relay, "close_bidding", json!({"task_id": t.id}), 0, "WRONG_PHASE").await?;
    m.at(&t, "reveal_end_ms").await?;
    let r = m.reveal(t.id, &a).await?;
    e.eq("reveal after reveal_end", code_of(&r).as_str(), "WRONG_PHASE");
    m.expect("close after reveal_end", &m.relay, "close_bidding", json!({"task_id": t.id}), 0, "NO_AWARD").await?;
    m.expect("close a second time", &m.w[0], "close_bidding", json!({"task_id": t.id}), 0, "WRONG_PHASE").await?;
    e.eq("the bid that missed its reveal lost its bond to the vault", m.owed(e.vault_id()).await?, BID_BOND);
    Ok(())
}

async fn m12(e: Env) -> Result<()> {
    let m = Mk::new(&e, 2, 300).await?;
    let t = m.post(&m.gc, &m.gc_tm, &m.relay, m.spec(&m.gc, 50).await?).await?;
    let a = Bidv::new(&m.w[0], 30);
    m.commit(t.id, &a).await?.into_result()?;
    // The second worker copies the first one's commitment from the chain.
    let copied = m.task(t.id).await?["bids"][0]["commitment"].as_str().unwrap().to_string();
    m.call(&m.w[1], "commit_bid", json!({"task_id": t.id, "commitment": copied}), BID_BOND, 30).await?.into_result()?;
    m.at(&t, "commit_end_ms").await?;
    let mut wrong = a.clone();
    wrong.price = 29 * MILLI;
    e.eq("reveal with another price", code_of(&m.reveal(t.id, &wrong).await?).as_str(), "BAD_REVEAL");
    let mut wrong = a.clone();
    wrong.salt = [7; 32];
    e.eq("reveal with another salt", code_of(&m.reveal(t.id, &wrong).await?).as_str(), "BAD_REVEAL");
    let mut wrong = a.clone();
    wrong.sk = SecretKey::from_random(KeyType::ED25519);
    e.eq("reveal with another key", code_of(&m.reveal(t.id, &wrong).await?).as_str(), "BAD_REVEAL");
    e.eq("the true reveal still works", code_of(&m.reveal(t.id, &a).await?).as_str(), "REVEALED");
    e.eq("revealing twice", code_of(&m.reveal(t.id, &a).await?).as_str(), "ALREADY_REVEALED");
    // The copier now knows price, key and salt, and still cannot open the copy: the hash names the worker.
    let mut copy = a.clone();
    copy.who = m.w[1].clone();
    e.eq("the copier opens the copy with the real numbers", code_of(&m.reveal(t.id, &copy).await?).as_str(), "BAD_REVEAL");
    let res = m.close(&t).await?;
    e.eq("the honest bid wins alone", mn(&res["amount"]), 50);
    e.eq("the copier's bond went to the vault", m.owed(e.vault_id()).await?, BID_BOND);
    Ok(())
}

async fn m13(e: Env) -> Result<()> {
    let m = Mk::new(&e, 3, 300).await?;
    let t = m.post(&m.gc, &m.gc_tm, &m.relay, m.spec(&m.gc, 50).await?).await?;
    let bids = [Bidv::new(&m.w[0], 30), Bidv::new(&m.w[1], 35), Bidv::new(&m.w[2], 10)];
    for b in &bids {
        m.commit(t.id, b).await?.into_result()?;
    }
    m.at(&t, "commit_end_ms").await?;
    m.reveal(t.id, &bids[0]).await?;
    m.reveal(t.id, &bids[1]).await?;
    let res = m.close(&t).await?;
    e.eq("the silent low bid does not win", m.task(t.id).await?["winner"].as_str().unwrap(), m.w[0].id().as_str());
    e.eq("price is the second revealed bid", mn(&res["amount"]), 35);
    e.eq("the revealed loser has its bond back", m.owed(m.w[1].id()).await?, BID_BOND);
    e.eq("the silent bidder has nothing", m.owed(m.w[2].id()).await?, 0);
    e.eq("its bond is owed to the vault", m.owed(e.vault_id()).await?, BID_BOND);
    let before = e.balance(&e.vault).await?;
    m.payout(e.vault_id()).await?.into_result()?;
    e.eq("anyone can send it on, and it lands in the vault account", e.balance(&e.vault).await? - before, BID_BOND);
    m.expect("a second payout finds nothing", &m.w[2], "payout", json!({"account_id": e.vault_id()}), 0, "NOTHING_OWED").await?;
    Ok(())
}

async fn m14(e: Env) -> Result<()> {
    let m = Mk::new(&e, 3, 300).await?;
    let base = shift(shift(m.spec(&m.gc, 50).await?, "commit_end_ms", 8_000), "reveal_end_ms", 12_000);
    let t = m.post(&m.gc, &m.gc_tm, &m.relay, base.clone()).await?;
    let t2 = m.post(&m.gc, &m.gc_tm, &m.relay, twin(base)).await?;
    let a = Bidv::new(&m.w[0], 30);
    let c = m.commitment(t.id, &a).await?;
    m.expect("a commit with half the bond", &m.w[0], "commit_bid", json!({"task_id": t.id, "commitment": c}), BID_BOND / 2, "WRONG_BOND").await?;
    m.commit(t.id, &a).await?.into_result()?;
    e.eq("a second bid from the same worker", code_of(&m.commit(t.id, &Bidv::new(&m.w[0], 20)).await?).as_str(), "ALREADY_BID");
    let over = Bidv::new(&m.w[1], 51);
    m.commit(t.id, &over).await?.into_result()?;
    // A bid that names the general contractor's own key: it already belongs to a grant.
    let mut squat = Bidv::new(&m.w[2], 20);
    squat.sk = m.gc.sk.clone();
    m.commit(t.id, &squat).await?.into_result()?;
    // On the second task a worker reuses the key it reveals in the first.
    let mut again = Bidv::new(&m.w[0], 25);
    again.sk = a.sk.clone();
    m.commit(t2.id, &again).await?.into_result()?;
    m.at(&t2, "commit_end_ms").await?;
    e.eq("a price above the budget", code_of(&m.reveal(t.id, &over).await?).as_str(), "PRICE_OUT_OF_RANGE");
    e.eq("a key that already belongs to a grant in the vault", code_of(&m.reveal(t.id, &squat).await?).as_str(), "KEY_TAKEN");
    e.eq("a fresh key", code_of(&m.reveal(t.id, &a).await?).as_str(), "REVEALED");
    e.eq("the same key in a second bid", code_of(&m.reveal(t2.id, &again).await?).as_str(), "KEY_TAKEN");
    let _ = c;
    Ok(())
}

async fn m15(e: Env) -> Result<()> {
    let m = Mk::new(&e, 3, 300).await?;
    // The winner bids almost nothing. Its pay is still set by the next bid.
    let t = m.post(&m.gc, &m.gc_tm, &m.relay, m.spec(&m.gc, 100).await?).await?;
    let res = m.auction(&t, &[Bidv::new(&m.w[0], 1), Bidv::new(&m.w[1], 60), Bidv::new(&m.w[2], 80)]).await?;
    e.eq("a bid of 1 is paid 60, the next bid", mn(&res["amount"]), 60);
    // All three agree to bid the maximum.
    let t2 = m.post(&m.gc, &m.gc_tm, &m.relay, m.spec(&m.gc, 100).await?).await?;
    let res = m.auction(&t2, &[Bidv::new(&m.w[0], 100), Bidv::new(&m.w[1], 100), Bidv::new(&m.w[2], 100)]).await?;
    e.eq("a cartel gets the budget and not more", mn(&res["amount"]), 100);
    let r = m.view("price_of", json!({"bids": [["1", 0], ["60", 1], ["80", 2]], "max_budget": "100", "min_bids": 1})).await?;
    e.eq("the rule as a view gives the same answer", r, json!([0, "60"]));
    Ok(())
}

async fn m16(e: Env) -> Result<()> {
    let m = Mk::new(&e, 2, 300).await?;
    let t = m.post(&m.gc, &m.gc_tm, &m.relay, m.spec(&m.gc, 100).await?).await?;
    let bids = [Bidv::new(&m.w[0], 40), Bidv::new(&m.w[1], 60)];
    let res = m.auction(&t, &bids).await?;
    let price: u128 = res["amount"].as_str().unwrap().parse()?;
    e.eq("price", price / MILLI, 60);
    let right = m.award_tm(&t, price);
    let fresh = || SecretKey::from_random(KeyType::ED25519);
    // Each wrong grant is created under the right key where the vault allows it, checked, then revoked,
    // so the key is free again for the next attempt.
    let mut low = right.clone();
    low.cap = 50 * MILLI;
    low.per_tx = low.cap;
    low.window_cap = low.cap;
    low.threshold = low.cap;
    let g = m.grant_for(&m.gc, &low, &bids[0].sk).await?;
    e.eq("a grant with a cap under the price", code_of(&m.confirm(&t, g.grant).await?).as_str(), "WRONG_CAP");
    e.revoke_by(&m.gc, g.grant).await?;
    let mut tight = right.clone();
    tight.per_tx = MILLI;
    let g = m.grant_for(&m.gc, &tight, &bids[0].sk).await?;
    e.eq("the right cap with a 1 mNEAR per-transaction limit", code_of(&m.confirm(&t, g.grant).await?).as_str(), "WRONG_LIMITS");
    e.revoke_by(&m.gc, g.grant).await?;
    let mut short = right.clone();
    short.expires_at_ms = t.spec["review_end_ms"].as_u64().unwrap();
    let g = m.grant_for(&m.gc, &short, &bids[0].sk).await?;
    e.eq("a grant that expires before the winner could draw", code_of(&m.confirm(&t, g.grant).await?).as_str(), "EXPIRES_TOO_SOON");
    e.revoke_by(&m.gc, g.grant).await?;
    let g = m.grant_for(&m.gc, &right, &fresh()).await?;
    e.eq("the right terms for somebody else's key", code_of(&m.confirm(&t, g.grant).await?).as_str(), "WRONG_KEY");
    let loser = m.grant_for(&m.gc, &right, &bids[1].sk).await?;
    e.eq("the right terms for the losing bidder", code_of(&m.confirm(&t, loser.grant).await?).as_str(), "WRONG_KEY");
    // A grant with the winner's key, created under another parent.
    let mut side_tm = m.gc_tm.clone();
    side_tm.cap = 100 * MILLI;
    side_tm.per_tx = side_tm.cap;
    side_tm.window_cap = side_tm.cap;
    side_tm.threshold = side_tm.cap;
    side_tm.max_depth = 2;
    side_tm.node_budget = 6;
    side_tm.gas_budget = 6 * 400 * MILLI;
    let side = e.child(&m.gc, &side_tm).await?;
    let mut under_side = right.clone();
    under_side.max_depth = 0;
    let g = m.grant_for(&side, &under_side, &bids[0].sk).await?;
    e.eq("the right terms under the wrong parent", code_of(&m.confirm(&t, g.grant).await?).as_str(), "WRONG_PARENT");
    e.revoke_by(&side, g.grant).await?;
    e.eq("the poster grant itself", code_of(&m.confirm(&t, m.gc.grant).await?).as_str(), "WRONG_PARENT");
    e.eq("a grant that does not exist", code_of(&m.confirm(&t, 9999).await?).as_str(), "UNKNOWN_GRANT");
    e.eq("still waiting for the award", m.status(t.id).await?.as_str(), "Awarding");
    let g = m.grant_for(&m.gc, &right, &bids[0].sk).await?;
    e.eq("the right grant", code_of(&m.confirm(&t, g.grant).await?).as_str(), "AWARDED");
    e.eq("confirming again", code_of(&m.confirm(&t, g.grant).await?).as_str(), "WRONG_PHASE");
    Ok(())
}

async fn m17(e: Env) -> Result<()> {
    let m = Mk::new(&e, 3, 300).await?;
    // Three tasks at once, each with one bidder, and the poster awards none of them.
    let base = after_reveal(shift(m.spec(&m.gc, 30).await?, "commit_end_ms", 10_000), 4_000, 60_000, 90_000);
    let mut tasks = vec![];
    for i in 0..3 {
        let t = m.post(&m.gc, &m.gc_tm, &m.relay, twin(base.clone())).await?;
        tasks.push((t, Bidv::new(&m.w[i], 20)));
    }
    for r in join_all(tasks.iter().map(|(t, b)| m.commit(t.id, b))).await {
        r?.into_result()?;
    }
    m.at(&tasks[2].0, "commit_end_ms").await?;
    for r in join_all(tasks.iter().map(|(t, b)| m.reveal(t.id, b))).await {
        e.eq("reveal accepted", code_of(&r?).as_str(), "REVEALED");
    }
    for (t, _) in &tasks {
        m.close(t).await?;
    }
    m.expect("calling the default before award_end", &m.w[0], "default_award", json!({"task_id": tasks[2].0.id}), 0, "WRONG_PHASE").await?;
    m.at(&tasks[2].0, "award_end_ms").await?;
    let before = m.owed(m.relay.id()).await?;
    for (i, (t, _)) in tasks.iter().enumerate() {
        let r = m.call(&m.w[i], "default_award", json!({"task_id": t.id}), 0, 150).await?;
        e.eq("the poster was live, the key was free, and no grant was made", code_of(&r).as_str(), "DEFAULTED");
        e.eq("the winner gets its bond and the poster's", m.owed(m.w[i].id()).await?, BID_BOND + POSTER_BOND);
    }
    e.eq("the poster's carrier got nothing back", m.owed(m.relay.id()).await?, before);
    let st = m.stat(m.gc.grant).await?;
    e.eq("three defaults on record, nothing left promised", (st["defaults"].as_u64(), st["open"].as_u64(), mn(&st["committed"])), (Some(3), Some(0), 0));
    let spec = m.spec(&m.gc, 30).await?;
    let r = m.post_raw(&m.relay, &spec, &m.sign_post(&m.gc.sk, &spec).await?, POSTER_BOND).await?;
    e.eq("after three, the grant cannot post again", code_of(&r).as_str(), "POSTER_BANNED");
    Ok(())
}

async fn m18(e: Env) -> Result<()> {
    let m = Mk::new(&e, 2, 300).await?;
    // A second grant holder in the same vault, who can create grants of its own.
    let mut rival_tm = m.gc_tm.clone();
    rival_tm.cap = 50 * MILLI;
    rival_tm.per_tx = rival_tm.cap;
    rival_tm.window_cap = rival_tm.cap;
    rival_tm.threshold = rival_tm.cap;
    rival_tm.max_depth = 2;
    rival_tm.node_budget = 6;
    rival_tm.gas_budget = 6 * 400 * MILLI;
    let rival = e.child(&m.gc, &rival_tm).await?;
    let base = after_reveal(shift(m.spec(&m.gc, 40).await?, "commit_end_ms", 8_000), 14_000, 60_000, 90_000);
    let t = m.post(&m.gc, &m.gc_tm, &m.relay, base.clone()).await?;
    let t2 = m.post(&m.gc, &m.gc_tm, &m.relay, twin(base)).await?;
    let b = Bidv::new(&m.w[0], 20);
    let b2 = Bidv::new(&m.w[1], 20);
    m.commit(t.id, &b).await?.into_result()?;
    m.commit(t2.id, &b2).await?.into_result()?;
    m.at(&t2, "commit_end_ms").await?;
    m.reveal(t.id, &b).await?;
    m.reveal(t2.id, &b2).await?;
    m.close(&t).await?;
    m.close(&t2).await?;
    // Task 1: the rival binds the winner's key to a grant of its own before the poster can award.
    let mut junk = m.award_tm(&t, MILLI);
    junk.max_depth = 0;
    m.grant_for(&rival, &junk, &b.sk).await?;
    let d = e.delegate_raw(&m.gc, &m.award_tm(&t, 40 * MILLI), &b.sk.public_key()).await?;
    e.code("the poster can no longer create the award: the key is taken", &d, "KEY_IN_USE");
    // Task 2: the poster creates a grant for the winner with a cap under the price.
    let mut cheat = m.award_tm(&t2, 40 * MILLI);
    cheat.cap = 10 * MILLI;
    cheat.per_tx = cheat.cap;
    cheat.window_cap = cheat.cap;
    cheat.threshold = cheat.cap;
    let g = m.grant_for(&m.gc, &cheat, &b2.sk).await?;
    e.eq("the cheap grant is not the award", code_of(&m.confirm(&t2, g.grant).await?).as_str(), "WRONG_CAP");
    m.at(&t2, "award_end_ms").await?;
    let r = m.call(&m.w[0], "default_award", json!({"task_id": t.id}), 0, 200).await?;
    e.eq("squatted key: the task is void", code_of(&r).as_str(), "VOID");
    e.eq("with the reason on record", m.task(t.id).await?["note"].as_str().unwrap(), "KEY_TAKEN_BY_ANOTHER_GRANT");
    e.eq("the winner has only its own bond back", m.owed(m.w[0].id()).await?, BID_BOND);
    let r = m.call(&m.w[1], "default_award", json!({"task_id": t2.id}), 0, 200).await?;
    e.eq("wrong-terms grant: the poster defaulted", code_of(&r).as_str(), "DEFAULTED");
    e.eq("the cheated winner gets its bond and the poster's", m.owed(m.w[1].id()).await?, BID_BOND + POSTER_BOND);
    e.eq("one default on record, not two", m.stat(m.gc.grant).await?["defaults"].as_u64(), Some(1));
    e.eq("the poster's carrier got one bond back, from the void task", m.owed(m.relay.id()).await?, POSTER_BOND);
    Ok(())
}

async fn m19(e: Env) -> Result<()> {
    let m = Mk::new(&e, 2, 300).await?;
    let s = after_reveal(m.spec(&m.gc, 40).await?, 14_000, 28_000, 70_000);
    let (t, a, _, _) = m.hired(&m.gc, &m.gc_tm, &m.relay, s, &[Bidv::new(&m.w[0], 20), Bidv::new(&m.w[1], 30)]).await?;
    e.eq("somebody else delivers", code_of(&m.deliver(&t, &m.w[1]).await?).as_str(), "NOT_WINNER");
    m.expect("calling a missed delivery before deliver_end", &m.w[1], "fail_delivery", json!({"task_id": t.id}), 0, "WRONG_PHASE").await?;
    m.at(&t, "deliver_end_ms").await?;
    e.eq("the winner delivers late", code_of(&m.deliver(&t, &m.w[0]).await?).as_str(), "WRONG_PHASE");
    let owed = m.owed(e.vault_id()).await?;
    m.expect("anyone calls the missed delivery", &m.w[1], "fail_delivery", json!({"task_id": t.id}), 0, "FAILED").await?;
    e.eq("the winner's bond is owed to the vault", m.owed(e.vault_id()).await? - owed, BID_BOND);
    e.eq("the winner has nothing", m.owed(m.w[0].id()).await?, 0);
    e.eq("nothing is promised any more", mn(&m.stat(m.gc.grant).await?["committed"]), 0);
    // The grant is still there until the poster revokes it. Whatever it draws can only go back.
    let d = m.draw(&a, 30 * MILLI).await?;
    e.eq("the failed winner can still draw into the market", d.outcome.as_str(), "settled");
    let c = m.claim(&t).await?;
    e.eq("and is paid none of it", mn(&c["amount"]), 0);
    m.expect("the draw goes back to the vault", &m.w[1], "refund", json!({"task_id": t.id}), 0, "REFUNDED").await?;
    e.eq("owed to the vault: the bond and the draw", m.owed(e.vault_id()).await? - owed, BID_BOND + 30 * MILLI);
    Ok(())
}

async fn m20(e: Env) -> Result<()> {
    let m = Mk::new(&e, 2, 300).await?;
    let (t, a, _, price) = m.hired(&m.gc, &m.gc_tm, &m.relay, m.spec(&m.gc, 40).await?, &[Bidv::new(&m.w[0], 20), Bidv::new(&m.w[1], 30)]).await?;
    m.deliver(&t, &m.w[0]).await?.into_result()?;
    let r = m.reject_as(&t, &a.sk).await?;
    e.eq("the winner signs a rejection of its own task", code_of(&r).as_str(), "BAD_SIGNATURE");
    let r = m.reject_as(&t, &m.gc.sk).await?;
    e.eq("the poster rejects", code_of(&r).as_str(), "REJECTED");
    e.eq("the bond is returned: rejection is the poster's word, not a missed deadline", m.owed(m.w[0].id()).await?, BID_BOND);
    let d = m.draw(&a, price).await?;
    e.eq("the rejected winner draws its whole price anyway", d.outcome.as_str(), "settled");
    let c = m.claim(&t).await?;
    e.eq("the market pays none of it", mn(&c["amount"]), 0);
    e.eq("the winner is owed only its bond", m.owed(m.w[0].id()).await?, BID_BOND);
    e.eq("the draw is held", mn(&m.task(t.id).await?["held"]), 30);
    let before = e.balance(&e.vault).await?;
    m.expect("anyone sends it back", &m.w[1], "refund", json!({"task_id": t.id}), 0, "REFUNDED").await?;
    m.payout(e.vault_id()).await?.into_result()?;
    e.eq("and it lands in the vault account", e.balance(&e.vault).await? - before, price);
    e.eq("the tree keeps the charge: returned money does not restore budget", mn(&e.grant(m.gc.grant).await?["spent"]), 30);
    e.eq("the rejection is on the poster's record", m.stat(m.gc.grant).await?["rejected"].as_u64(), Some(1));
    Ok(())
}

async fn m21(e: Env) -> Result<()> {
    let m = Mk::new(&e, 2, 300).await?;
    let (t, a, _, _) = m.hired(&m.gc, &m.gc_tm, &m.relay, m.spec(&m.gc, 40).await?, &[Bidv::new(&m.w[0], 20)]).await?;
    let (t2, _, _, _) = m.hired(&m.gc, &m.gc_tm, &m.relay, m.spec(&m.gc, 40).await?, &[Bidv::new(&m.w[1], 20)]).await?;
    let r = m.accept_as(&t, &m.gc.sk).await?;
    e.eq("acceptance before anything was delivered", code_of(&r).as_str(), "WRONG_PHASE");
    m.deliver(&t, &m.w[0]).await?.into_result()?;
    m.deliver(&t2, &m.w[1]).await?.into_result()?;
    let r = m.accept_as(&t, &a.sk).await?;
    e.eq("the winner accepts its own work", code_of(&r).as_str(), "BAD_SIGNATURE");
    // The poster's real acceptance of task 2, replayed on task 1.
    let payload = m.view("accept_payload", json!({"task_id": t2.id})).await?;
    let sig2 = sign(&m.gc.sk, &unhex(payload.as_str().unwrap()));
    let r = m.call(&m.w[0], "accept", json!({"task_id": t.id, "signature": sig2}), 0, 80).await?;
    e.eq("an acceptance of another task", code_of(&r).as_str(), "BAD_SIGNATURE");
    let r = m.call(&m.w[0], "accept", json!({"task_id": t2.id, "signature": sig2}), 0, 80).await?;
    e.eq("the same signature on its own task, carried by anyone", code_of(&r).as_str(), "ACCEPTED");
    e.eq("task 1 is still waiting", m.status(t.id).await?.as_str(), "Delivered");
    Ok(())
}

async fn m22(e: Env) -> Result<()> {
    let m = Mk::new(&e, 1, 300).await?;
    let s = after_reveal(m.spec(&m.gc, 40).await?, 12_000, 22_000, 34_000);
    let (t, a, _, price) = m.hired(&m.gc, &m.gc_tm, &m.relay, s, &[Bidv::new(&m.w[0], 20)]).await?;
    m.deliver(&t, &m.w[0]).await?.into_result()?;
    let d = m.draw(&a, price).await?;
    e.eq("the winner draws while the poster says nothing", d.outcome.as_str(), "settled");
    let c = m.claim(&t).await?;
    e.eq("before review_end nothing is paid", mn(&c["amount"]), 0);
    m.at(&t, "review_end_ms").await?;
    let r = m.reject_as(&t, &m.gc.sk).await?;
    e.eq("a rejection after review_end", code_of(&r).as_str(), "WRONG_PHASE");
    let c = m.claim(&t).await?;
    e.eq("after review_end the price is paid", mn(&c["amount"]), 40);
    e.eq("accepted by silence", m.task(t.id).await?["note"].as_str().unwrap(), "accepted by silence");
    e.eq("the task is paid", m.status(t.id).await?.as_str(), "Paid");
    Ok(())
}

async fn m23(e: Env) -> Result<()> {
    let m = Mk::new(&e, 2, 300).await?;
    let (t, a, _, price) = m.hired(&m.gc, &m.gc_tm, &m.relay, m.spec(&m.gc, 40).await?, &[Bidv::new(&m.w[0], 20), Bidv::new(&m.w[1], 30)]).await?;
    let d = m.draw(&a, 10 * MILLI).await?;
    e.eq("the winner draws before delivering", d.outcome.as_str(), "settled");
    let c = m.claim(&t).await?;
    e.eq("nothing is paid", mn(&c["amount"]), 0);
    e.eq("it is held", mn(&m.task(t.id).await?["held"]), 10);
    m.expect("a refund while the task is in progress", &m.w[1], "refund", json!({"task_id": t.id}), 0, "WRONG_PHASE").await?;
    let d = m.draw(&a, price).await?;
    e.code("a draw that would take the grant past the price", &d, "CAP");
    m.deliver(&t, &m.w[0]).await?.into_result()?;
    m.accept_as(&t, &m.gc.sk).await?;
    let c = m.claim(&t).await?;
    e.eq("after acceptance the held draw is paid", mn(&c["amount"]), 10);
    let d = m.draw(&a, 20 * MILLI).await?;
    e.eq("the rest of the price", d.outcome.as_str(), "settled");
    let c = m.claim(&t).await?;
    e.eq("is paid on the next claim", mn(&c["amount"]), 20);
    let c = m.claim(&t).await?;
    e.eq("a claim with nothing new pays nothing", mn(&c["amount"]), 0);
    e.eq("paid in total: the price", mn(&m.task(t.id).await?["paid"]), 30);
    e.eq("owed to the winner: price and bond, once", m.owed(m.w[0].id()).await?, price + BID_BOND);
    Ok(())
}

async fn m24(e: Env) -> Result<()> {
    let m = Mk::new(&e, 2, 300).await?;
    let attacker = e.recipients[2].clone();
    let (t, a, tm, _) = m.hired(&m.gc, &m.gc_tm, &m.relay, m.spec_sub(&m.gc, 80, 1).await?, &[Bidv::new(&m.w[0], 50)]).await?;
    let before = e.balance(&attacker).await?;
    let d = e.spend(&a, attacker.id(), 10 * MILLI).await?;
    e.code("pay the attacker from its grant", &d, "RECIPIENT_NOT_ALLOWED");
    let mut wide = m.award_tm(&Tk { id: 0, spec: m.spec(&a, 10).await?, poster: a.clone(), ptm: tm.clone() }, 10 * MILLI);
    wide.recipients = Some(vec![attacker.id().to_string()]);
    let d = e.try_delegate(&a, &wide).await?;
    e.code("create a sub-grant that can pay the attacker", &d, "ATT_RECIPIENTS");
    // Hire the attacker through the market: it is not on the owner's roster.
    let t2 = m.post(&a, &tm, &m.w[0], m.spec(&a, 30).await?).await?;
    let r = m.commit(t2.id, &Bidv::new(&attacker, 30)).await?;
    e.eq("the attacker bids on the hijacked contractor's task", code_of(&r).as_str(), "NOT_ON_ROSTER");
    m.expect("the market pays out only what it owes, to the account itself", &m.w[0], "payout", json!({"account_id": attacker.id()}), 0, "NOTHING_OWED").await?;
    e.eq("the attacker received nothing", e.balance(&attacker).await?.saturating_sub(before), 0);
    // The general contractor sees the refusals and cuts the contractor off.
    e.revoke_by(&m.gc, a.grant).await?;
    m.expect("the contractor's own task is void", &m.w[1], "void_task", json!({"task_id": t.id}), 0, "VOID").await?;
    m.expect("and so is the task it opened", &m.w[1], "void_task", json!({"task_id": t2.id}), 0, "VOID").await?;
    Ok(())
}

async fn m25(e: Env) -> Result<()> {
    let m = Mk::new(&e, 2, 300).await?;
    let (_t1, a, tm, _) = m.hired(&m.gc, &m.gc_tm, &m.relay, m.spec_sub(&m.gc, 80, 1).await?, &[Bidv::new(&m.w[0], 50)]).await?;
    let t2 = m.post(&a, &tm, &m.w[0], m.spec(&a, 30).await?).await?;
    let b = Bidv::new(&m.w[1], 20);
    m.commit(t2.id, &b).await?.into_result()?;
    m.expect("voiding while the poster is live", &m.w[1], "void_task", json!({"task_id": t2.id}), 0, "STILL_LIVE").await?;
    e.revoke_by(&m.gc, a.grant).await?;
    m.expect("after the poster is revoked, anyone voids its task", &m.w[1], "void_task", json!({"task_id": t2.id}), 0, "VOID").await?;
    e.eq("the bidder's bond is back", m.owed(m.w[1].id()).await?, BID_BOND);
    e.eq("the poster's bond is back with whoever carried the post", m.owed(m.w[0].id()).await?, POSTER_BOND);
    e.eq("the reason is on record", m.task(t2.id).await?["note"].as_str().unwrap(), "POSTER_NOT_LIVE");
    let r = m.commit(t2.id, &Bidv::new(&m.w[0], 10)).await?;
    e.eq("a void task takes no more bids", code_of(&r).as_str(), "WRONG_PHASE");
    Ok(())
}

async fn m26(e: Env) -> Result<()> {
    let m = Mk::new(&e, 2, 300).await?;
    let s = after_reveal(m.spec(&m.gc, 40).await?, 14_000, 26_000, 70_000);
    let (t, a, _, _) = m.hired(&m.gc, &m.gc_tm, &m.relay, s, &[Bidv::new(&m.w[0], 20), Bidv::new(&m.w[1], 30)]).await?;
    let d = m.draw(&a, 5 * MILLI).await?;
    e.eq("the winner draws a little before it is revoked", d.outcome.as_str(), "settled");
    e.revoke_by(&m.gc, a.grant).await?;
    e.eq("the vault deleted the winner's key", e.has_key(&a.pk).await?, false);
    m.at(&t, "deliver_end_ms").await?;
    // After the deadline both calls are open. For a revoked winner both end the same way.
    m.expect("a missed delivery by a revoked winner is void, not a failure", &m.w[1], "fail_delivery", json!({"task_id": t.id}), 0, "VOID").await?;
    e.eq("its bond is returned: being revoked is not a missed deadline", m.owed(m.w[0].id()).await?, BID_BOND);
    e.eq("the vault is owed no bond", m.owed(e.vault_id()).await?, 0);
    let c = m.claim(&t).await?;
    e.eq("what it drew is counted and not paid", (mn(&c["amount"]), mn(&m.task(t.id).await?["held"])), (0, 5));
    m.expect("and goes back to the vault", &m.w[1], "refund", json!({"task_id": t.id}), 0, "REFUNDED").await?;
    e.eq("owed to the vault: the draw", m.owed(e.vault_id()).await?, 5 * MILLI);
    Ok(())
}

async fn m27(e: Env) -> Result<()> {
    let m = Mk::new(&e, 4, 300).await?;
    // The contractor w0 wins a task and opens three of its own: one awarded, one delivered, one in bidding.
    let (t1, a1, tm1, _) = m.hired(&m.gc, &m.gc_tm, &m.relay, m.spec_sub(&m.gc, 150, 1).await?, &[Bidv::new(&m.w[0], 100)]).await?;
    let (t2, a2, _, _) = m.hired(&a1, &tm1, &m.w[0], m.spec(&a1, 40).await?, &[Bidv::new(&m.w[1], 30)]).await?;
    let (t3, a3, _, _) = m.hired(&a1, &tm1, &m.w[0], m.spec(&a1, 40).await?, &[Bidv::new(&m.w[2], 30)]).await?;
    m.deliver(&t3, &m.w[2]).await?.into_result()?;
    let t4 = m.post(&a1, &tm1, &m.w[0], m.spec(&a1, 30).await?).await?;
    m.commit(t4.id, &Bidv::new(&m.w[3], 20)).await?.into_result()?;
    let keys = e.key_count().await?;
    let d = e.revoke_by(&m.gc, a1.grant).await?;
    e.code("one revoke by the general contractor", &d, "REVOKED");
    e.eq("removes the contractor's key and both subcontractors' keys", keys - e.key_count().await?, 3);
    let live = (e.grant(a1.grant).await?["live"].clone(), e.grant(a2.grant).await?["live"].clone(), e.grant(a3.grant).await?["live"].clone());
    e.eq("nothing below it is live", live, (json!(false), json!(false), json!(false)));
    for (name, t) in [("the contractor's own task", &t1), ("its awarded subtask", &t2), ("its delivered subtask", &t3), ("its subtask still in bidding", &t4)] {
        m.expect(name, &m.w[3], "void_task", json!({"task_id": t.id}), 0, "VOID").await?;
    }
    e.eq("every bond is back with its worker", (m.owed(m.w[1].id()).await?, m.owed(m.w[2].id()).await?, m.owed(m.w[3].id()).await?), (BID_BOND, BID_BOND, BID_BOND));
    e.eq("the contractor has its own bond and the three bonds it posted for its subtasks", m.owed(m.w[0].id()).await?, BID_BOND + 3 * POSTER_BOND);
    let r = m.accept_as(&t3, &a1.sk).await?;
    e.eq("the revoked contractor cannot accept the delivered subtask", code_of(&r).as_str(), "WRONG_PHASE");
    e.eq("nothing was ever charged to the tree", mn(&e.grant(m.gc.grant).await?["spent"]), 0);
    let st = m.stat(m.gc.grant).await?;
    e.eq("the general contractor's promise is released", (st["open"].as_u64(), mn(&st["committed"])), (Some(0), 0));
    let spec = m.spec(&a1, 10).await?;
    let r = m.post_raw(&m.w[0], &spec, &m.sign_post(&a1.sk, &spec).await?, POSTER_BOND).await?;
    e.eq("the revoked contractor cannot post again", code_of(&r).as_str(), "GRANT_NOT_LIVE");
    m.solvent("market holds at least what it owes").await
}

async fn m28(e: Env) -> Result<()> {
    let m = Mk::new(&e, 2, 300).await?;
    let (t1, a1, tm1, _) = m.hired(&m.gc, &m.gc_tm, &m.relay, m.spec_sub(&m.gc, 100, 1).await?, &[Bidv::new(&m.w[0], 80)]).await?;
    let (t2, a2, _, _) = m.hired(&a1, &tm1, &m.w[0], m.spec(&a1, 40).await?, &[Bidv::new(&m.w[1], 30)]).await?;
    m.deliver(&t2, &m.w[1]).await?.into_result()?;
    m.accept_as(&t2, &a1.sk).await?;
    e.eq("the subcontractor's work is accepted", m.status(t2.id).await?.as_str(), "Accepted");
    // The general contractor revokes the contractor before the subcontractor has drawn.
    e.revoke_by(&m.gc, a1.grant).await?;
    e.eq("the accepted subcontractor's key is gone with its contractor", e.has_key(&a2.pk).await?, false);
    let r = e.spend_tx(&a2, m.id(), 40 * MILLI, None).await;
    e.check("so it cannot draw", r.is_err(), "the protocol refuses a deleted key", r.is_ok());
    let c = m.claim(&t2).await?;
    e.eq("the market pays nothing: nothing arrived", mn(&c["amount"]), 0);
    e.eq("the accepted task is closed with nothing paid", (m.status(t2.id).await?, mn(&m.task(t2.id).await?["paid"])), ("Paid".to_string(), 0));
    e.eq("the subcontractor keeps its bond", m.owed(m.w[1].id()).await?, BID_BOND);
    m.expect("an accepted task cannot be voided", &m.w[1], "void_task", json!({"task_id": t2.id}), 0, "WRONG_PHASE").await?;
    m.expect("the contractor's task is void", &m.w[1], "void_task", json!({"task_id": t1.id}), 0, "VOID").await?;
    e.eq("the tree was charged nothing", mn(&e.grant(m.gc.grant).await?["spent"]), 0);
    Ok(())
}

async fn m29(e: Env) -> Result<()> {
    let m = Mk::new(&e, 2, 300).await?;
    let (t1, a1, tm1, _) = m.hired(&m.gc, &m.gc_tm, &m.relay, m.spec_sub(&m.gc, 100, 1).await?, &[Bidv::new(&m.w[0], 80)]).await?;
    let (t2, a2, _, p2) = m.hired(&a1, &tm1, &m.w[0], m.spec(&a1, 40).await?, &[Bidv::new(&m.w[1], 30)]).await?;
    m.finish(&t2, &a2, &m.w[1], p2).await?;
    e.eq("the subcontractor is paid", m.owed(m.w[1].id()).await?, p2 + BID_BOND);
    e.revoke_by(&m.gc, a1.grant).await?;
    m.expect("the contractor's task is void", &m.w[0], "void_task", json!({"task_id": t1.id}), 0, "VOID").await?;
    e.eq("the paid subtask stays paid", (m.status(t2.id).await?, mn(&m.task(t2.id).await?["paid"])), ("Paid".to_string(), 40));
    let before = e.balance(&m.w[1]).await?;
    m.payout(m.w[1].id()).await?.into_result()?;
    e.eq("and the subcontractor still collects", e.balance(&m.w[1]).await? - before, p2 + BID_BOND);
    e.eq(
        "the counters at every level equal what left the vault",
        (mn(&e.grant(a2.grant).await?["spent"]), mn(&e.grant(a1.grant).await?["spent"]), mn(&e.grant(m.gc.grant).await?["spent"])),
        (40, 40, 40),
    );
    let c: Value = e.vault.view(e.vault_id(), "contact").args_json(json!({"grant_id": a2.grant, "recipient": m.id()})).await?.json()?;
    e.eq("the vault's own record of what the grant paid the market", mn(&c["total"]), 40);
    Ok(())
}

async fn m30(e: Env) -> Result<()> {
    let m = Mk::new(&e, 2, 300).await?;
    let t = m.post(&m.gc, &m.gc_tm, &m.relay, m.spec(&m.gc, 40).await?).await?;
    let bids = [Bidv::new(&m.w[0], 20), Bidv::new(&m.w[1], 30)];
    for b in &bids {
        m.commit(t.id, b).await?.into_result()?;
    }
    m.expect("in bidding", &m.w[1], "void_task", json!({"task_id": t.id}), 0, "STILL_LIVE").await?;
    m.at(&t, "commit_end_ms").await?;
    for b in &bids {
        m.reveal(t.id, b).await?;
    }
    let res = m.close(&t).await?;
    let price: u128 = res["amount"].as_str().unwrap().parse()?;
    m.expect("waiting for the award", &m.w[1], "void_task", json!({"task_id": t.id}), 0, "STILL_LIVE").await?;
    let (a, _) = m.award(&t, &bids[0], price).await?;
    m.expect("awarded", &m.w[1], "void_task", json!({"task_id": t.id}), 0, "STILL_LIVE").await?;
    m.deliver(&t, &m.w[0]).await?.into_result()?;
    m.expect("delivered", &m.w[1], "void_task", json!({"task_id": t.id}), 0, "STILL_LIVE").await?;
    e.eq("the task is untouched", m.status(t.id).await?.as_str(), "Delivered");
    e.eq("and the winner's bond is still locked", mn(&m.task(t.id).await?["bids"][0]["bond"]), 10);
    let _ = a;
    Ok(())
}

async fn m31(e: Env) -> Result<()> {
    let m = Mk::new(&e, 1, 300).await?;
    let d = m.draw(&m.gc, 25 * MILLI).await?;
    e.eq("the root pays the market, outside any task", d.outcome.as_str(), "settled");
    let (t, a, _, _) = m.hired(&m.gc, &m.gc_tm, &m.relay, m.spec(&m.gc, 40).await?, &[Bidv::new(&m.w[0], 20)]).await?;
    m.expect("a grant that belongs to a task uses claim and refund", &m.w[0], "return_unbound", json!({"vault": e.vault_id(), "grant_id": a.grant}), 0, "GRANT_BOUND").await?;
    m.expect("anyone asks for the root's stray payment back", &m.w[0], "return_unbound", json!({"vault": e.vault_id(), "grant_id": m.gc.grant}), 0, "RETURNED").await?;
    e.eq("it is owed to the vault", m.owed(e.vault_id()).await?, 25 * MILLI);
    m.expect("a second time finds nothing new", &m.w[0], "return_unbound", json!({"vault": e.vault_id(), "grant_id": m.gc.grant}), 0, "NOTHING_HELD").await?;
    let before = e.balance(&e.vault).await?;
    m.payout(e.vault_id()).await?.into_result()?;
    e.eq("and lands in the vault account", e.balance(&e.vault).await? - before, 25 * MILLI);
    // A grant that can also pay someone else: its direct spending cannot be told apart, so it is refused.
    let mut two = m.gc_tm.clone();
    two.expires_at_ms -= 1000;
    two.recipients = Some(vec![m.id().to_string(), e.r(1).to_string()]);
    let other = e.root(&two).await?;
    e.spend(&other, &e.r(1), 5 * MILLI).await?;
    m.expect("a grant with a second recipient", &m.w[0], "return_unbound", json!({"vault": e.vault_id(), "grant_id": other.grant}), 0, "RECIPIENTS_NOT_MARKET_ONLY").await?;
    let _ = t;
    m.solvent("market holds at least what it owes").await
}

async fn m32(e: Env) -> Result<()> {
    let m = Mk::new(&e, 2, 300).await?;
    let (t1, a1, tm1, _) = m.hired(&m.gc, &m.gc_tm, &m.relay, m.spec_sub(&m.gc, 60, 1).await?, &[Bidv::new(&m.w[0], 50)]).await?;
    let (t2, a2, _, p2) = m.hired(&a1, &tm1, &m.w[0], m.spec(&a1, 40).await?, &[Bidv::new(&m.w[1], 30)]).await?;
    e.eq("the contractor has 60 and promised 40 below", (mn(&e.grant(a1.grant).await?["cap"]), p2 / MILLI), (60, 40));
    // The contractor is not accepted yet, and draws 35 of its 60 anyway.
    let d = m.draw(&a1, 35 * MILLI).await?;
    e.eq("the vault lets it: 35 is inside its cap", d.outcome.as_str(), "settled");
    m.deliver(&t2, &m.w[1]).await?.into_result()?;
    m.accept_as(&t2, &a1.sk).await?;
    let d = m.draw(&a2, p2).await?;
    e.code("the accepted subcontractor's draw of 40 hits the contractor's cap", &d, "ANCESTOR_CAP");
    e.eq("and names the contractor's grant", d.blocking_grant, Some(a1.grant));
    let d = m.draw(&a2, 25 * MILLI).await?;
    e.eq("it can draw only what is left, 25", d.outcome.as_str(), "settled");
    let c = m.claim(&t2).await?;
    e.eq("and is paid 25 of 40", mn(&c["amount"]), 25);
    e.eq("the tree never went past the contractor's price", mn(&e.grant(a1.grant).await?["spent"]), 60);
    // The contractor's early draw sits in the market and pays nobody. Its poster sees it and rejects.
    let c = m.claim(&t1).await?;
    e.eq("the contractor's own draw is held, not paid", (mn(&c["amount"]), mn(&m.task(t1.id).await?["held"])), (0, 35));
    m.deliver(&t1, &m.w[0]).await?.into_result()?;
    m.reject_as(&t1, &m.gc.sk).await?;
    m.expect("after the rejection it goes back to the vault", &m.w[1], "refund", json!({"task_id": t1.id}), 0, "REFUNDED").await?;
    e.eq("the vault gets the 35 back", m.owed(e.vault_id()).await?, 35 * MILLI);
    e.eq("the contractor is owed only bonds: its own, and the one it posted for its subtask", m.owed(m.w[0].id()).await?, BID_BOND + POSTER_BOND);
    m.solvent("market holds at least what it owes").await
}

async fn m33(e: Env) -> Result<()> {
    let m = Mk::new(&e, 16, 300).await?;
    let t = m.post(&m.gc, &m.gc_tm, &m.relay, m.spec(&m.gc, 100).await?).await?;
    let bids: Vec<Bidv> = (0..16).map(|i| Bidv::new(&m.w[i], 40 + i as u128)).collect();
    let commits = join_all(bids.iter().map(|b| m.commit(t.id, b))).await;
    e.eq("sixteen commits land", commits.iter().filter(|r| r.as_ref().is_ok_and(|r| r.is_success())).count(), 16);
    let extra = e.recipients[1].clone();
    let r = m.commit(t.id, &Bidv::new(&extra, 30)).await?;
    e.eq("a seventeenth account is not on the roster", code_of(&r).as_str(), "NOT_ON_ROSTER");
    m.at(&t, "commit_end_ms").await?;
    // Fifteen reveal. One stays silent.
    let reveals = join_all(bids.iter().skip(1).map(|b| m.reveal(t.id, b))).await;
    e.eq("fifteen reveals land", reveals.iter().filter(|r| r.as_ref().is_ok_and(|r| code_of(r) == "REVEALED")).count(), 15);
    m.at(&t, "reveal_end_ms").await?;
    let r = m.call(&m.relay, "close_bidding", json!({"task_id": t.id}), 0, 100).await?;
    let res: Value = r.clone().json()?;
    e.eq("the lowest revealed bid wins at the next price", (m.task(t.id).await?["winner"].as_str().unwrap().to_string(), mn(&res["amount"])), (m.w[1].id().to_string(), 42));
    e.check("closing sixteen bids fits in one small receipt", tgas(&r) < 30.0, "under 30 Tgas", format!("{:.2} Tgas", tgas(&r)));
    let mut back = 0;
    for w in m.w.iter().skip(2) {
        back += m.owed(w.id()).await?;
    }
    e.eq("fourteen losing bonds are returned", back, 14 * BID_BOND);
    e.eq("the silent bond is owed to the vault", m.owed(e.vault_id()).await?, BID_BOND);
    let s = m.view("solvency", json!({})).await?;
    e.eq("bonds still locked: the winner's and the poster's", s["bonds_held"].as_str().unwrap().parse::<u128>()?, BID_BOND + POSTER_BOND);
    m.solvent("market holds at least what it owes").await
}

async fn m34(e: Env) -> Result<()> {
    let m = Mk::new(&e, 2, 300).await?;
    let books = |s: Value| -> (u128, u128, u128) { (mn(&s["bonds_held"]), mn(&s["balances"]), mn(&s["held"])) };
    let t = m.post(&m.gc, &m.gc_tm, &m.relay, m.spec(&m.gc, 60).await?).await?;
    e.eq("posted: the poster bond is locked", books(m.view("solvency", json!({})).await?), (20, 0, 0));
    let bids = [Bidv::new(&m.w[0], 20), Bidv::new(&m.w[1], 30)];
    for b in &bids {
        m.commit(t.id, b).await?.into_result()?;
    }
    e.eq("two bids: two bonds more", books(m.view("solvency", json!({})).await?), (40, 0, 0));
    m.at(&t, "commit_end_ms").await?;
    for b in &bids {
        m.reveal(t.id, b).await?;
    }
    let res = m.close(&t).await?;
    let price: u128 = res["amount"].as_str().unwrap().parse()?;
    e.eq("closed: the loser's bond becomes a balance", books(m.view("solvency", json!({})).await?), (30, 10, 0));
    let (a, _) = m.award(&t, &bids[0], price).await?;
    e.eq("awarded: the poster bond becomes a balance", books(m.view("solvency", json!({})).await?), (10, 30, 0));
    m.draw(&a, 10 * MILLI).await?;
    m.claim(&t).await?;
    e.eq("an early draw is held", books(m.view("solvency", json!({})).await?), (10, 30, 10));
    m.solvent("solvent with money held").await?;
    m.deliver(&t, &m.w[0]).await?.into_result()?;
    m.accept_as(&t, &m.gc.sk).await?;
    m.draw(&a, 20 * MILLI).await?;
    m.claim(&t).await?;
    e.eq("accepted and claimed: bond, held draw and new draw are all balances", books(m.view("solvency", json!({})).await?), (0, 70, 0));
    for who in [m.w[0].id(), m.w[1].id(), m.relay.id()] {
        m.payout(who).await?.into_result()?;
    }
    e.eq("after the payouts the market owes nothing", books(m.view("solvency", json!({})).await?), (0, 0, 0));
    m.solvent("solvent at the end").await
}

async fn m35(e: Env) -> Result<()> {
    let m = Mk::new(&e, 2, 100).await?;
    // Two tasks from a root of 100, posted while both fit. The first clears at its full budget of 60.
    let (t1, a1, _, p1) = m.hired(&m.gc, &m.gc_tm, &m.relay, m.spec(&m.gc, 60).await?, &[Bidv::new(&m.w[0], 50)]).await?;
    let (t2, a2, _, p2) = m.hired(&m.gc, &m.gc_tm, &m.relay, m.spec(&m.gc, 40).await?, &[Bidv::new(&m.w[1], 30)]).await?;
    e.eq("prices", (p1 / MILLI, p2 / MILLI), (60, 40));
    let spec = m.spec(&m.gc, 1).await?;
    let r = m.post_raw(&m.relay, &spec, &m.sign_post(&m.gc.sk, &spec).await?, POSTER_BOND).await?;
    e.eq("with 100 promised, even 1 more is refused", code_of(&r).as_str(), "OVERCOMMITTED");
    m.finish(&t1, &a1, &m.w[0], p1).await?;
    m.finish(&t2, &a2, &m.w[1], p2).await?;
    e.eq("both winners are paid in full", ((m.owed(m.w[0].id()).await? - BID_BOND) / MILLI, (m.owed(m.w[1].id()).await? - BID_BOND) / MILLI), (60, 40));
    let root = e.grant(m.gc.grant).await?;
    e.eq("the root is spent to its cap and no further", (mn(&root["spent"]), mn(&root["spendable"])), (100, 0));
    let st = m.stat(m.gc.grant).await?;
    e.eq("nothing is promised and nothing is open", (st["open"].as_u64(), mn(&st["committed"])), (Some(0), 0));
    Ok(())
}
