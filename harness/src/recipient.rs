//! Recipient scenarios: the shared denylist and clearance tickets, on the real runtime.
use crate::env::*;
use crate::scenarios::{chain, events, Scenario};
use anyhow::{anyhow, Result};
use ed25519_dalek::{Signer, SigningKey};
use futures::future::join_all;
use near_workspaces::result::ExecutionFinalResult;
use near_workspaces::types::{Gas, NearToken};
use near_workspaces::{Account, AccountId};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

macro_rules! sc {
    ($id:literal, $title:literal, $testnet:literal, $f:ident) => {
        Scenario { id: $id, group: "recipient", title: $title, testnet: $testnet, run: |e| Box::pin($f(e)) }
    };
}

pub fn all() -> Vec<Scenario> {
    vec![
        sc!("H01", "recipient listed by the reporter quorum is refused on chain, nothing moves", true, h01),
        sc!("H02", "poisoning: one trusted reporter alone cannot block anyone", true, h02),
        sc!("H03", "poisoning: eight fresh accounts outside the trusted set change nothing", false, h03),
        sc!("H04", "poisoning: copied evidence does not count twice and nobody reports twice", false, h04),
        sc!("H05", "a vault cannot be set to a quorum of 1 or to repeated reporters", false, h05),
        sc!("H06", "a report needs evidence and a deposit, and nobody can report themselves", false, h06),
        sc!("H07", "retraction and expiry both end a listing, and the deposit goes back", false, h07),
        sc!("H08", "the subject's response is public and does not unlist it", false, h08),
        sc!("H09", "registry with no code: the spend is refused and every counter comes back", true, h09),
        sc!("H10", "registry answering nonsense: the spend is refused, nothing leaks", false, h10),
        sc!("H11", "12 concurrent screened spends, 3 of them listed, stay inside the shared cap", false, h11),
        sc!("H12", "spends waiting for the registry cannot promise the same NEAR twice", false, h12),
        sc!("H13", "a revoke racing a screened spend: it settles or it is refused, never half", false, h13),
        sc!("H14", "approved spend to a recipient listed after approval dies at execution", false, h14),
        sc!("H15", "no ticket: the spend queues for a human and only then moves", true, h15),
        sc!("H16", "valid ticket pays at once and the contact is recorded at leaf and root", true, h16),
        sc!("H17", "forged ticket is refused and counted", true, h17),
        sc!("H18", "a ticket for another recipient or another grant fails the signature", false, h18),
        sc!("H19", "expired, over-amount and too-long tickets are refused", false, h19),
        sc!("H20", "a child cannot drop the clearance requirement", false, h20),
        sc!("H21", "a clearance-bound parent cannot approve an unticketed spend; the owner can", false, h21),
        sc!("H22", "replacing the attestor kills every ticket it signed", false, h22),
        sc!("H23", "repeated forged tickets revoke the key", false, h23),
        sc!("H24", "refused and failed payments leave no contact, so history cannot be faked with them", false, h24),
        sc!("H25", "screened NEP-141 spend: listed rolls the ledger back, clean settles", false, h25),
        sc!("H26", "too little gas for a screened spend is refused before any reservation", false, h26),
        sc!("H27", "refusal at depth 8 fits in the callback's fixed gas", false, h27),
    ]
}

fn sha_hex(s: &str) -> String {
    Sha256::digest(s.as_bytes()).iter().map(|b| format!("{b:02x}")).collect()
}

fn hexs(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn str_borsh(out: &mut Vec<u8>, s: &str) {
    out.extend((s.len() as u32).to_le_bytes());
    out.extend(s.as_bytes());
}

/// Written out by hand on purpose: this is what the Python attestor has to reproduce byte for byte.
fn message(vault: &str, grant: u64, recipient: &str, max_amount: u128, expires: u64, code: &[u8; 32]) -> Vec<u8> {
    let mut out = b"espalier-clearance-v1".to_vec();
    str_borsh(&mut out, vault);
    out.extend(grant.to_le_bytes());
    str_borsh(&mut out, recipient);
    out.extend(max_amount.to_le_bytes());
    out.extend(expires.to_le_bytes());
    out.extend(code);
    out
}

pub fn attestor_key(seed: u8) -> SigningKey {
    SigningKey::from_bytes(&[seed; 32])
}

pub fn attestor_pk(key: &SigningKey) -> String {
    format!("ed25519:{}", bs58::encode(key.verifying_key().to_bytes()).into_string())
}

async fn set_attestor(e: &Env, key: &SigningKey) -> Result<()> {
    e.owner_call("set_attestor", json!({"attestor": attestor_pk(key)})).await?.into_result()?;
    Ok(())
}

fn ticket(e: &Env, key: &SigningKey, grant: u64, to: &AccountId, max: u128, expires: u64) -> Value {
    let code = [0u8; 32];
    let msg = message(e.vault_id().as_str(), grant, to.as_str(), max, expires, &code);
    json!({
        "max_amount": max.to_string(),
        "expires_at_ms": expires,
        "code_hash": hexs(&code),
        "signature": hexs(&key.sign(&msg).to_bytes()),
    })
}

async fn spend_with(e: &Env, a: &Agent, to: &AccountId, amount: u128, clearance: Value) -> Result<Decision> {
    let r = a
        .acct
        .call(e.vault_id(), "request_spend")
        .args_json(json!({"recipient": to, "amount": amount.to_string(), "memo": null, "clearance": clearance}))
        .gas(Gas::from_tgas(120))
        .transact()
        .await?;
    e.keep("spend", &format!("grant {}", a.grant), "request_spend", &r);
    Ok(r.json()?)
}

async fn report_raw(e: &Env, who: &Account, reg: &AccountId, subject: &AccountId, evidence: &str, ttl: u32) -> Result<ExecutionFinalResult> {
    let r = who
        .call(reg, "report")
        .args_json(json!({
            "subject": subject, "category": "impersonation", "evidence_sha256": sha_hex(evidence),
            "evidence_uri": format!("evidence://{evidence}"), "ttl_secs": ttl,
        }))
        .deposit(NearToken::from_millinear(10))
        .gas(Gas::from_tgas(30))
        .transact()
        .await?;
    e.keep("report", who.id().as_str(), "report", &r);
    Ok(r)
}

async fn report(e: &Env, who: &Account, reg: &AccountId, subject: &AccountId, evidence: &str) -> Result<()> {
    report_raw(e, who, reg, subject, evidence, 3600).await?.into_result()?;
    Ok(())
}

async fn retract(e: &Env, who: &Account, reg: &AccountId, subject: &AccountId) -> Result<ExecutionFinalResult> {
    let r = who.call(reg, "retract").args_json(json!({"subject": subject})).gas(Gas::from_tgas(30)).transact().await?;
    e.keep("retract", who.id().as_str(), "retract", &r);
    Ok(r)
}

async fn check(e: &Env, reg: &AccountId, subject: &AccountId, quorum: u8) -> Result<Value> {
    Ok(e.vault
        .view(reg, "check")
        .args_json(json!({"subject": subject, "reporters": reporter_ids(e), "quorum": quorum}))
        .await?
        .json()?)
}

fn reporters(e: &Env) -> Vec<Account> {
    vec![e.base.clone(), e.owner.clone(), e.recipients[1].clone()]
}

fn reporter_ids(e: &Env) -> Vec<String> {
    reporters(e).iter().map(|a| a.id().to_string()).collect()
}

async fn clean_reports(e: &Env, reg: &AccountId, subject: &AccountId) -> Result<()> {
    for who in reporters(e) {
        let r: Option<Value> = e
            .vault
            .view(reg, "get_report")
            .args_json(json!({"subject": subject, "reporter": who.id()}))
            .await?
            .json()?;
        if r.is_some() {
            retract(e, &who, reg, subject).await?.into_result()?;
        }
    }
    Ok(())
}

/// Turns the registry on for this vault, and leaves no stale reports on the subject when the same
/// registry outlives one run, as it does on testnet.
async fn screen_on(e: &Env, subject: &AccountId) -> Result<AccountId> {
    let reg = e.use_registry().await?;
    clean_reports(e, &reg, subject).await?;
    screen_at(e, &reg).await?;
    Ok(reg)
}

async fn screen_at(e: &Env, reg: &AccountId) -> Result<()> {
    e.owner_call("set_screen", json!({"screen": {"registry": reg, "reporters": reporter_ids(e), "quorum": 2}}))
        .await?
        .into_result()?;
    Ok(())
}

/// A shared vault must not stay screened for the scenarios that come after.
async fn screen_off(e: &Env) -> Result<()> {
    e.owner_call("set_screen", json!({"screen": null})).await?.into_result()?;
    Ok(())
}

async fn held(e: &Env) -> Result<String> {
    Ok(e.info().await?["native_held"].as_str().unwrap_or("?").to_string())
}

async fn expect_clean(e: &Env, agents: &[Agent]) -> Result<()> {
    for a in agents {
        e.eq("reserved released on every level", e.field_u128(a.grant, "reserved").await?, 0);
        e.eq("window released on every level", e.field_u128(a.grant, "window_used").await?, 0);
        e.eq("nothing counted as spent", e.field_u128(a.grant, "spent").await?, 0);
    }
    e.eq("no native funds held", held(e).await?, "0".to_string());
    e.eq("nothing in flight", e.info().await?["in_flight"].as_u64(), Some(0));
    Ok(())
}

async fn h01(e: Env) -> Result<()> {
    let bad = e.r(2);
    let reg = screen_on(&e, &bad).await?;
    let (a, _) = chain(&e, 2).await?;
    report(&e, &e.base, &reg, &bad, "h01-reporter-a").await?;
    report(&e, &e.owner, &reg, &bad, "h01-reporter-b").await?;
    e.eq("registry says listed", check(&e, &reg, &bad, 2).await?["listed"].as_bool(), Some(true));
    let before = e.balance(&e.recipients[2]).await?;
    let r = e.spend_tx(&a[2], &bad, 5 * MILLI, None).await?;
    let logs = r.logs();
    let d: Decision = r.clone().json()?;
    e.code("refused by the contract", &d, "RECIPIENT_LISTED");
    e.eq("the refusal is a NEP-297 event", events(&logs, "denied").len(), 1);
    e.eq("reservation was released in the open", events(&logs, "spend_released").len(), 1);
    e.eq("recipient received nothing", e.balance(&e.recipients[2]).await? - before, 0);
    expect_clean(&e, &a).await?;
    e.eq("denial counted on the leaf", e.grant(a[2].grant).await?["denied"].as_u64(), Some(1));
    let ok = e.spend(&a[2], &e.r(0), 5 * MILLI).await?;
    e.eq("a clean recipient still gets paid", ok.outcome.as_str(), "settled");
    clean_reports(&e, &reg, &bad).await?;
    screen_off(&e).await
}

async fn h02(e: Env) -> Result<()> {
    let target = e.r(0);
    let reg = screen_on(&e, &target).await?;
    let (a, _) = chain(&e, 1).await?;
    report(&e, &e.base, &reg, &target, "h02-malicious").await?;
    let c = check(&e, &reg, &target, 2).await?;
    e.eq("one report is not a listing", c["listed"].as_bool(), Some(false));
    e.eq("it is visible, for a wrapper to warn on", c["trusted_reports"].as_u64(), Some(1));
    let asked_one = check(&e, &reg, &target, 1).await?;
    e.eq("the registry ignores a quorum of 1", asked_one["listed"].as_bool(), Some(false));
    let d = e.spend(&a[1], &target, 5 * MILLI).await?;
    e.eq("the poisoned recipient still gets paid", d.outcome.as_str(), "settled");
    clean_reports(&e, &reg, &target).await?;
    screen_off(&e).await
}

async fn h03(e: Env) -> Result<()> {
    let target = e.r(0);
    let reg = screen_on(&e, &target).await?;
    let (a, _) = chain(&e, 1).await?;
    for i in 0..8 {
        let sybil = e.new_account(&format!("sybil{i}"), 1).await?;
        report(&e, &sybil, &reg, &target, &format!("h03-sybil-{i}")).await?;
    }
    let c = check(&e, &reg, &target, 2).await?;
    e.eq("all eight were filed", c["all_reports"].as_u64(), Some(8));
    e.eq("none of them is trusted", c["trusted_reports"].as_u64(), Some(0));
    e.eq("not listed", c["listed"].as_bool(), Some(false));
    let d = e.spend(&a[1], &target, 5 * MILLI).await?;
    e.eq("payment goes through", d.outcome.as_str(), "settled");
    Ok(())
}

async fn h04(e: Env) -> Result<()> {
    let target = e.r(0);
    let reg = screen_on(&e, &target).await?;
    report(&e, &e.base, &reg, &target, "h04-same").await?;
    let again = report_raw(&e, &e.base, &reg, &target, "h04-other", 3600).await?;
    e.eq("the same reporter cannot file twice", again.is_failure(), true);
    report(&e, &e.owner, &reg, &target, "h04-same").await?;
    let c = check(&e, &reg, &target, 2).await?;
    e.eq("two trusted reports", c["trusted_reports"].as_u64(), Some(2));
    e.eq("one piece of evidence", c["distinct_evidence"].as_u64(), Some(1));
    e.eq("not listed", c["listed"].as_bool(), Some(false));
    report(&e, &e.recipients[1], &reg, &target, "h04-independent").await?;
    e.eq("independent evidence reaches the quorum", check(&e, &reg, &target, 2).await?["listed"].as_bool(), Some(true));
    Ok(())
}

async fn h05(e: Env) -> Result<()> {
    let reg = e.use_registry().await?;
    let try_set = |reporters: Vec<String>, quorum: u8| {
        let e = e.clone();
        let reg = reg.clone();
        async move {
            e.owner_call("set_screen", json!({"screen": {"registry": reg, "reporters": reporters, "quorum": quorum}})).await
        }
    };
    let ids = reporter_ids(&e);
    e.eq("quorum 1 refused", try_set(ids.clone(), 1).await?.is_failure(), true);
    e.eq("quorum above the reporter count refused", try_set(ids[..1].to_vec(), 2).await?.is_failure(), true);
    e.eq("one account named twice refused", try_set(vec![ids[0].clone(), ids[0].clone()], 2).await?.is_failure(), true);
    e.eq("a valid setting works", try_set(ids, 2).await?.is_success(), true);
    e.eq("the screen is visible in vault_info", e.info().await?["screen"]["quorum"].as_u64(), Some(2));
    let stranger = e
        .recipients[0]
        .call(e.vault_id(), "set_screen")
        .args_json(json!({"screen": null}))
        .gas(Gas::from_tgas(30))
        .transact()
        .await?;
    e.eq("a stranger cannot switch it off", stranger.is_failure(), true);
    Ok(())
}

async fn h06(e: Env) -> Result<()> {
    let reg = e.use_registry().await?;
    let call = |who: Account, args: Value, deposit: NearToken| {
        let reg = reg.clone();
        async move {
            who.call(&reg, "report").args_json(args).deposit(deposit).gas(Gas::from_tgas(30)).transact().await
        }
    };
    let good = |subject: &AccountId| {
        json!({"subject": subject, "category": "scam", "evidence_sha256": sha_hex("x"), "evidence_uri": "evidence://x", "ttl_secs": 60})
    };
    let mut no_hash = good(&e.r(0));
    no_hash["evidence_sha256"] = json!("0".repeat(64));
    let mut bad_hash = good(&e.r(0));
    bad_hash["evidence_sha256"] = json!("not a hash");
    let mut no_uri = good(&e.r(0));
    no_uri["evidence_uri"] = json!("");
    let mut long_ttl = good(&e.r(0));
    long_ttl["ttl_secs"] = json!(200 * 86_400);
    let deposit = NearToken::from_millinear(10);
    e.eq("all-zero evidence refused", call(e.base.clone(), no_hash, deposit).await?.is_failure(), true);
    e.eq("malformed evidence refused", call(e.base.clone(), bad_hash, deposit).await?.is_failure(), true);
    e.eq("empty evidence link refused", call(e.base.clone(), no_uri, deposit).await?.is_failure(), true);
    e.eq("a report cannot last past 180 days", call(e.base.clone(), long_ttl, deposit).await?.is_failure(), true);
    e.eq("no deposit refused", call(e.base.clone(), good(&e.r(0)), NearToken::from_millinear(1)).await?.is_failure(), true);
    e.eq("reporting yourself refused", call(e.recipients[0].clone(), good(&e.r(0)), deposit).await?.is_failure(), true);
    e.eq("a well-formed report passes", call(e.base.clone(), good(&e.r(0)), deposit).await?.is_success(), true);
    Ok(())
}

async fn h07(e: Env) -> Result<()> {
    let bad = e.r(2);
    let reg = screen_on(&e, &bad).await?;
    report(&e, &e.base, &reg, &bad, "h07-a").await?;
    report(&e, &e.owner, &reg, &bad, "h07-b").await?;
    e.eq("listed", check(&e, &reg, &bad, 2).await?["listed"].as_bool(), Some(true));
    let before = e.balance(&e.base).await?;
    retract(&e, &e.base, &reg, &bad).await?.into_result()?;
    e.check("deposit came back", e.balance(&e.base).await? > before, "more than before", "balance grew");
    e.eq("retraction ends the listing", check(&e, &reg, &bad, 2).await?["listed"].as_bool(), Some(false));
    let stats: Value = e.vault.view(&reg, "reporter_stats").args_json(json!({"reporter": e.base.id()})).await?.json()?;
    e.eq("retraction shows on the reporter's record", stats["retracted"].as_u64(), Some(1));
    report_raw(&e, &e.recipients[1], &reg, &bad, "h07-c", 2).await?.into_result()?;
    e.eq("listed again", check(&e, &reg, &bad, 2).await?["listed"].as_bool(), Some(true));
    let early = e.base.call(&reg, "prune").args_json(json!({"subject": bad, "reporter": e.owner.id()})).gas(Gas::from_tgas(30)).transact().await?;
    e.eq("a live report cannot be pruned", early.is_failure(), true);
    e.wait_chain(e.now_ms().await? + 2_500).await?;
    e.eq("the short report expired, so the listing ended", check(&e, &reg, &bad, 2).await?["listed"].as_bool(), Some(false));
    let pruned = e.base.call(&reg, "prune").args_json(json!({"subject": bad, "reporter": e.recipients[1].id()})).gas(Gas::from_tgas(30)).transact().await?;
    e.eq("anyone can clear an expired report", pruned.is_success(), true);
    Ok(())
}

async fn h08(e: Env) -> Result<()> {
    let bad = e.r(2);
    let reg = screen_on(&e, &bad).await?;
    report(&e, &e.base, &reg, &bad, "h08-a").await?;
    report(&e, &e.owner, &reg, &bad, "h08-b").await?;
    let r = e.recipients[2]
        .call(&reg, "respond")
        .args_json(json!({"evidence_sha256": sha_hex("my side"), "evidence_uri": "evidence://my-side"}))
        .deposit(NearToken::from_millinear(10))
        .gas(Gas::from_tgas(30))
        .transact()
        .await?;
    e.eq("response filed", r.is_success(), true);
    let c = check(&e, &reg, &bad, 2).await?;
    e.eq("it shows", c["responded"].as_bool(), Some(true));
    e.eq("it does not unlist", c["listed"].as_bool(), Some(true));
    let (a, _) = chain(&e, 1).await?;
    let d = e.spend(&a[1], &bad, 5 * MILLI).await?;
    e.code("still refused", &d, "RECIPIENT_LISTED");
    Ok(())
}

async fn h09(e: Env) -> Result<()> {
    let (a, _) = chain(&e, 2).await?;
    // A plain account has no `check` method, which is what a deleted registry looks like.
    e.owner_call(
        "set_screen",
        json!({"screen": {"registry": e.r(1), "reporters": reporter_ids(&e), "quorum": 2}}),
    )
    .await?
    .into_result()?;
    let before = e.balance(&e.recipients[0]).await?;
    let d = e.spend(&a[2], &e.r(0), 5 * MILLI).await?;
    e.code("failed closed", &d, "REGISTRY_UNAVAILABLE");
    e.eq("recipient received nothing", e.balance(&e.recipients[0]).await? - before, 0);
    expect_clean(&e, &a).await?;
    e.eq("not the agent's fault, so not counted", e.grant(a[2].grant).await?["denied"].as_u64(), Some(0));
    screen_off(&e).await
}

async fn h10(e: Env) -> Result<()> {
    let (a, _) = chain(&e, 1).await?;
    let fake = e.relay().await?;
    e.owner_call("set_screen", json!({"screen": {"registry": fake.id(), "reporters": reporter_ids(&e), "quorum": 2}}))
        .await?
        .into_result()?;
    let d = e.spend(&a[1], &e.r(0), 5 * MILLI).await?;
    e.code("a string where a verdict belongs fails closed", &d, "REGISTRY_UNAVAILABLE");
    expect_clean(&e, &a).await
}

async fn h11(e: Env) -> Result<()> {
    let bad = e.r(2);
    let reg = screen_on(&e, &bad).await?;
    report(&e, &e.base, &reg, &bad, "h11-a").await?;
    report(&e, &e.owner, &reg, &bad, "h11-b").await?;
    let mut t = Tm::root(e.now_ms().await?);
    t.cap = 100 * MILLI;
    t.per_tx = 10 * MILLI;
    let root = e.root(&t).await?;
    let mut ct = t.child();
    ct.gas_budget = 0;
    ct.node_budget = 0;
    ct.max_depth = 0;
    let mut kids = vec![];
    for _ in 0..12 {
        kids.push(e.child(&root, &ct).await?);
    }
    let listed_before = e.balance(&e.recipients[2]).await?;
    let dests: Vec<AccountId> = (0..12).map(|i| if i % 4 == 3 { bad.clone() } else { e.r(0) }).collect();
    let calls = kids.iter().zip(&dests).map(|(k, to)| e.spend(k, to, 10 * MILLI));
    let results: Vec<Result<Decision>> = join_all(calls).await;
    let mut settled = 0;
    let mut listed = 0;
    let mut refused = 0;
    for r in results {
        match r?.code.as_str() {
            "SETTLED" => settled += 1,
            "RECIPIENT_LISTED" => listed += 1,
            _ => refused += 1,
        }
    }
    e.check("no more than the cap settled", settled <= 10, "at most 10 of 10 milliNEAR", settled);
    e.check("the registry refused the listed ones", listed >= 1, "at least 1 of 3 (only 2 can hit the cap first)", listed);
    e.eq("the listed recipient got nothing", e.balance(&e.recipients[2]).await? - listed_before, 0);
    e.eq("spent on the root equals what settled", e.field_u128(root.grant, "spent").await?, settled as u128 * 10 * MILLI);
    e.eq("nothing stays reserved", e.field_u128(root.grant, "reserved").await?, 0);
    e.eq("no native funds held", held(&e).await?, "0".to_string());
    e.check("every spend was accounted for", settled + listed + refused == 12, "12", settled + listed + refused);
    Ok(())
}

async fn h12(e: Env) -> Result<()> {
    let reg = screen_on(&e, &e.r(0)).await?;
    let (a, _) = chain(&e, 1).await?;
    let mut sibling_t = Tm::root(e.now_ms().await?);
    sibling_t.per_tx = 900_000 * MILLI;
    sibling_t.cap = 900_000 * MILLI;
    sibling_t.window_cap = 900_000 * MILLI;
    sibling_t.threshold = 900_000 * MILLI;
    let big1 = e.root(&sibling_t).await?;
    let big2 = e.root(&sibling_t).await?;
    let _ = (a, reg);
    // Take the vault down to a known free balance, so that two payments fit one at a time only.
    let free: u128 = e.info().await?["available_native"].as_str().ok_or(anyhow!("free"))?.parse()?;
    let keep = 3000 * MILLI;
    e.owner_call("withdraw", json!({"asset": "Native", "amount": (free - keep).to_string(), "to": e.owner.id()}))
        .await?
        .into_result()?;
    let free: u128 = e.info().await?["available_native"].as_str().ok_or(anyhow!("free"))?.parse()?;
    let each = free * 2 / 3;
    let before = e.balance(&e.recipients[0]).await?;
    let to = e.r(0);
    let (x, y) = tokio::join!(e.spend(&big1, &to, each), e.spend(&big2, &to, each));
    let (x, y) = (x?, y?);
    let settled = [&x, &y].iter().filter(|d| d.outcome == "settled").count();
    e.eq("only one payment fits the free balance", settled, 1);
    let loser = if x.outcome == "settled" { &y } else { &x };
    e.code("the other sees the held funds", loser, "VAULT_BALANCE");
    e.eq("the recipient got one payment", e.balance(&e.recipients[0]).await? - before, each);
    e.eq("no native funds held", held(&e).await?, "0".to_string());
    Ok(())
}

async fn h13(e: Env) -> Result<()> {
    screen_on(&e, &e.r(0)).await?;
    let (a, _) = chain(&e, 2).await?;
    let before = e.balance(&e.recipients[0]).await?;
    let to = e.r(0);
    let spends = (0..4).map(|_| e.spend(&a[2], &to, 5 * MILLI));
    let (results, revoke) = tokio::join!(join_all(spends), e.revoke_by_owner(a[1].grant));
    revoke?;
    let mut settled = 0u128;
    for r in results.into_iter().flatten() {
        if r.outcome == "settled" {
            settled += 1;
        }
    }
    e.sleep_ms(1500).await;
    e.eq("money moved equals what settled", e.balance(&e.recipients[0]).await? - before, settled * 5 * MILLI);
    e.eq("spent on the root equals what settled", e.field_u128(a[0].grant, "spent").await?, settled * 5 * MILLI);
    e.eq("reserved returned to zero", e.field_u128(a[0].grant, "reserved").await?, 0);
    e.eq("no native funds held", held(&e).await?, "0".to_string());
    Ok(())
}

async fn h14(e: Env) -> Result<()> {
    let bad = e.r(2);
    let reg = screen_on(&e, &bad).await?;
    let mut t = Tm::root(e.now_ms().await?);
    t.threshold = 10 * MILLI;
    let root = e.root(&t).await?;
    let a = vec![root.clone(), e.child(&root, &t.child()).await?];
    let amount = 20 * MILLI;
    let d = e.spend(&a[1], &bad, amount).await?;
    if d.outcome != "queued" {
        return Err(anyhow!("expected a queued spend, got {d:?}"));
    }
    let id = d.approval_id.ok_or(anyhow!("id"))?;
    e.approve_owner(id, d.spend_hash.as_deref().unwrap()).await?;
    report(&e, &e.base, &reg, &bad, "h14-a").await?;
    report(&e, &e.owner, &reg, &bad, "h14-b").await?;
    let before = e.balance(&e.recipients[2]).await?;
    let d = e.execute(&a[1], id, &bad, amount).await?;
    e.code("the approval does not outrank the listing", &d, "RECIPIENT_LISTED");
    e.eq("recipient received nothing", e.balance(&e.recipients[2]).await? - before, 0);
    expect_clean(&e, &a).await
}

async fn bound_chain(e: &Env, depth: usize) -> Result<Vec<Agent>> {
    let mut t = Tm::root(e.now_ms().await?);
    t.clearance = true;
    let mut out = vec![e.root(&t).await?];
    for _ in 0..depth {
        t = t.child();
        let c = e.child(out.last().unwrap(), &t).await?;
        out.push(c);
    }
    Ok(out)
}

async fn h15(e: Env) -> Result<()> {
    let k = attestor_key(21);
    set_attestor(&e, &k).await?;
    let a = bound_chain(&e, 1).await?;
    let before = e.balance(&e.recipients[0]).await?;
    let d = e.spend(&a[1], &e.r(0), 5 * MILLI).await?;
    e.eq("queued", d.outcome.as_str(), "queued");
    e.code("for want of a ticket", &d, "NEEDS_CLEARANCE");
    e.eq("no funds moved", e.balance(&e.recipients[0]).await? - before, 0);
    let id = d.approval_id.ok_or(anyhow!("id"))?;
    let v: Value = e.vault.view(e.vault_id(), "get_approval").args_json(json!({"approval_id": id})).await?.json()?;
    e.eq("the queue entry says why", v["no_clearance"].as_bool(), Some(true));
    let denied = e.execute(&a[1], id, &e.r(0), 5 * MILLI).await?;
    e.code("the agent cannot execute it alone", &denied, "APPROVAL_NOT_APPROVED");
    e.approve_owner(id, d.spend_hash.as_deref().unwrap()).await?;
    let d = e.execute(&a[1], id, &e.r(0), 5 * MILLI).await?;
    e.eq("after the human it settles", d.outcome.as_str(), "settled");
    e.eq("exact amount moved", e.balance(&e.recipients[0]).await? - before, 5 * MILLI);
    Ok(())
}

async fn h16(e: Env) -> Result<()> {
    let k = attestor_key(22);
    set_attestor(&e, &k).await?;
    let a = bound_chain(&e, 1).await?;
    let now = e.now_ms().await?;
    let to = e.r(0);
    let before = e.balance(&e.recipients[0]).await?;
    let d = spend_with(&e, &a[1], &to, 5 * MILLI, ticket(&e, &k, a[1].grant, &to, 7 * MILLI, now + 120_000)).await?;
    e.eq("settled with no human", d.outcome.as_str(), "settled");
    e.eq("exact amount moved", e.balance(&e.recipients[0]).await? - before, 5 * MILLI);
    for g in [a[1].grant, a[0].grant] {
        let c: Option<Value> = e.vault.view(e.vault_id(), "contact").args_json(json!({"grant_id": g, "recipient": to})).await?.json()?;
        let c = c.ok_or(anyhow!("no contact on grant {g}"))?;
        e.eq("contact count", c["count"].as_u64(), Some(1));
        e.eq("contact total", c["total"].as_str(), Some((5 * MILLI).to_string().as_str()));
    }
    Ok(())
}

async fn h17(e: Env) -> Result<()> {
    let k = attestor_key(23);
    set_attestor(&e, &k).await?;
    let a = bound_chain(&e, 1).await?;
    let now = e.now_ms().await?;
    let to = e.r(0);
    let before = e.balance(&e.recipients[0]).await?;
    let forged = ticket(&e, &attestor_key(99), a[1].grant, &to, 7 * MILLI, now + 120_000);
    let d = spend_with(&e, &a[1], &to, 5 * MILLI, forged).await?;
    e.code("signature from the wrong key", &d, "CLEARANCE_BAD_SIG");
    e.eq("no funds moved", e.balance(&e.recipients[0]).await? - before, 0);
    e.eq("denial counted", e.grant(a[1].grant).await?["denied"].as_u64(), Some(1));
    let mut garbled = ticket(&e, &k, a[1].grant, &to, 7 * MILLI, now + 120_000);
    garbled["signature"] = json!("zz");
    let d = spend_with(&e, &a[1], &to, 5 * MILLI, garbled).await?;
    e.code("unreadable signature", &d, "CLEARANCE_BAD_SIG");
    let mut raised = ticket(&e, &k, a[1].grant, &to, 7 * MILLI, now + 120_000);
    raised["max_amount"] = json!((70 * MILLI).to_string());
    let d = spend_with(&e, &a[1], &to, 50 * MILLI, raised).await?;
    e.code("a raised limit breaks the signature", &d, "CLEARANCE_BAD_SIG");
    Ok(())
}

async fn h18(e: Env) -> Result<()> {
    let k = attestor_key(24);
    set_attestor(&e, &k).await?;
    let a = bound_chain(&e, 1).await?;
    let now = e.now_ms().await?;
    let good = e.r(0);
    let other = e.r(1);
    let t = ticket(&e, &k, a[1].grant, &good, 7 * MILLI, now + 120_000);
    let d = spend_with(&e, &a[1], &other, 5 * MILLI, t.clone()).await?;
    e.code("ticket for another recipient", &d, "CLEARANCE_BAD_SIG");
    let d = spend_with(&e, &a[0], &good, 5 * MILLI, t).await?;
    e.code("ticket for another grant", &d, "CLEARANCE_BAD_SIG");
    let mut other_vault = ticket(&e, &k, a[1].grant, &good, 7 * MILLI, now + 120_000);
    let msg = message("another-vault.near", a[1].grant, good.as_str(), 7 * MILLI, now + 120_000, &[0u8; 32]);
    other_vault["signature"] = json!(hexs(&k.sign(&msg).to_bytes()));
    let d = spend_with(&e, &a[1], &good, 5 * MILLI, other_vault).await?;
    e.code("ticket for another vault", &d, "CLEARANCE_BAD_SIG");
    Ok(())
}

async fn h19(e: Env) -> Result<()> {
    let k = attestor_key(25);
    set_attestor(&e, &k).await?;
    let a = bound_chain(&e, 1).await?;
    let now = e.now_ms().await?;
    let to = e.r(0);
    let d = spend_with(&e, &a[1], &to, 5 * MILLI, ticket(&e, &k, a[1].grant, &to, 7 * MILLI, now.saturating_sub(1))).await?;
    e.code("expired", &d, "CLEARANCE_EXPIRED");
    let d = spend_with(&e, &a[1], &to, 8 * MILLI, ticket(&e, &k, a[1].grant, &to, 7 * MILLI, now + 120_000)).await?;
    e.code("more than the ticket allows", &d, "CLEARANCE_AMOUNT");
    let d = spend_with(&e, &a[1], &to, 5 * MILLI, ticket(&e, &k, a[1].grant, &to, 7 * MILLI, now + 7_200_000)).await?;
    e.code("a ticket cannot run for hours", &d, "CLEARANCE_TOO_LONG");
    Ok(())
}

async fn h20(e: Env) -> Result<()> {
    let mut t = Tm::root(e.now_ms().await?);
    t.clearance = true;
    let root = e.root(&t).await?;
    let mut c = t.child();
    c.clearance = false;
    let d = e.try_delegate(&root, &c).await?;
    e.code("child without the requirement refused", &d, "ATT_CLEARANCE");
    let mut open = Tm::root(e.now_ms().await?);
    open.clearance = false;
    let root = e.root(&open).await?;
    let mut c = open.child();
    c.clearance = true;
    let child = e.child(&root, &c).await?;
    e.eq("a child can add the requirement", e.grant(child.grant).await?["clearance_required"].as_bool(), Some(true));
    Ok(())
}

async fn h21(e: Env) -> Result<()> {
    let k = attestor_key(26);
    set_attestor(&e, &k).await?;
    let a = bound_chain(&e, 1).await?;
    let d = e.spend(&a[1], &e.r(0), 5 * MILLI).await?;
    let hash = d.spend_hash.clone().unwrap();
    let id = d.approval_id.unwrap();
    let denied = e.approve_as(&a[0].acct, "parent", id, &hash).await?;
    e.code("a bound parent cannot wave it through", &denied, "APPROVER_NEEDS_CLEARANCE");
    let ok = e.approve_owner(id, &hash).await?;
    e.eq("the owner can", ok.outcome.as_str(), "allowed");
    // A parent that is not bound by clearance could have paid this itself, so it may approve.
    let mut open = Tm::root(e.now_ms().await?);
    open.clearance = false;
    let root = e.root(&open).await?;
    let mut c = open.child();
    c.clearance = true;
    let child = e.child(&root, &c).await?;
    let d = e.spend(&child, &e.r(0), 5 * MILLI).await?;
    let ok = e.approve_as(&root.acct, "unbound parent", d.approval_id.unwrap(), &d.spend_hash.unwrap()).await?;
    e.eq("an unbound parent can", ok.outcome.as_str(), "allowed");
    Ok(())
}

async fn h22(e: Env) -> Result<()> {
    let old = attestor_key(27);
    set_attestor(&e, &old).await?;
    let a = bound_chain(&e, 0).await?;
    let now = e.now_ms().await?;
    let to = e.r(0);
    let t = ticket(&e, &old, a[0].grant, &to, 7 * MILLI, now + 120_000);
    set_attestor(&e, &attestor_key(28)).await?;
    let d = spend_with(&e, &a[0], &to, 5 * MILLI, t.clone()).await?;
    e.code("the old ticket is dead", &d, "CLEARANCE_BAD_SIG");
    e.owner_call("set_attestor", json!({"attestor": null})).await?.into_result()?;
    let d = spend_with(&e, &a[0], &to, 5 * MILLI, t).await?;
    e.code("no attestor, no ticket", &d, "NO_ATTESTOR");
    Ok(())
}

async fn h23(e: Env) -> Result<()> {
    let k = attestor_key(29);
    set_attestor(&e, &k).await?;
    let mut t = Tm::root(e.now_ms().await?);
    t.clearance = true;
    t.max_denials = 3;
    let root = e.root(&t).await?;
    let now = e.now_ms().await?;
    let to = e.r(0);
    for _ in 0..3 {
        let forged = ticket(&e, &attestor_key(98), root.grant, &to, 7 * MILLI, now + 120_000);
        spend_with(&e, &root, &to, 5 * MILLI, forged).await?;
    }
    let g = e.grant(root.grant).await?;
    e.eq("revoked by its own denials", g["revoked"].as_bool(), Some(true));
    e.eq("the key is gone", e.has_key(&root.pk).await?, false);
    Ok(())
}

async fn h24(e: Env) -> Result<()> {
    let bad = e.r(2);
    let reg = screen_on(&e, &bad).await?;
    report(&e, &e.base, &reg, &bad, "h24-a").await?;
    report(&e, &e.owner, &reg, &bad, "h24-b").await?;
    let (a, _) = chain(&e, 1).await?;
    let nobody: AccountId = format!("nobody-here.{}", e.owner.id()).parse()?;
    e.spend(&a[1], &bad, 5 * MILLI).await?;
    let d = e.spend(&a[1], &nobody, 5 * MILLI).await?;
    e.eq("the transfer failed", d.outcome.as_str(), "rolled_back");
    e.spend(&a[1], &e.r(0), 500 * MILLI).await?;
    for to in [&bad, &nobody, &e.r(0)] {
        let c: Option<Value> = e.vault.view(e.vault_id(), "contact").args_json(json!({"grant_id": a[1].grant, "recipient": to})).await?.json()?;
        e.eq("no contact recorded", c, None);
    }
    Ok(())
}

async fn h25(e: Env) -> Result<()> {
    let ft = e.funded_token(1_000).await?;
    let bad = e.r(2);
    let reg = screen_on(&e, &bad).await?;
    report(&e, &e.base, &reg, &bad, "h25-a").await?;
    report(&e, &e.owner, &reg, &bad, "h25-b").await?;
    let mut t = Tm::root(e.now_ms().await?);
    t.asset = json!({"Ft": ft.id()});
    t.cap = 500;
    t.per_tx = 100;
    t.window_cap = 500;
    t.threshold = 100;
    let root = e.root(&t).await?;
    e.register_on_token(&ft, &e.recipients[0]).await?;
    e.register_on_token(&ft, &e.recipients[2]).await?;
    let d = e.spend(&root, &bad, 60).await?;
    e.code("listed", &d, "RECIPIENT_LISTED");
    e.eq("token ledger restored", e.vault_token_ledger(&ft).await?, 1_000);
    e.eq("recipient got no tokens", e.token_balance(&ft, &bad).await?, 0);
    let d = e.spend(&root, &e.r(0), 60).await?;
    e.eq("clean recipient settles", d.outcome.as_str(), "settled");
    e.eq("ledger debited", e.vault_token_ledger(&ft).await?, 940);
    e.eq("tokens arrived", e.token_balance(&ft, &e.r(0)).await?, 60);
    Ok(())
}

async fn h26(e: Env) -> Result<()> {
    screen_on(&e, &e.r(0)).await?;
    let (a, _) = chain(&e, 1).await?;
    let r = e.spend_with_gas(&a[1], &e.r(0), 5 * MILLI, None, 60).await?;
    let d: Decision = r.json()?;
    e.code("refused before reserving", &d, "GAS_TOO_LOW");
    expect_clean(&e, &a).await
}

async fn h27(e: Env) -> Result<()> {
    let bad = e.r(2);
    let reg = screen_on(&e, &bad).await?;
    report(&e, &e.base, &reg, &bad, "h27-a").await?;
    report(&e, &e.owner, &reg, &bad, "h27-b").await?;
    let (a, _) = chain(&e, 6).await?;
    let d = e.spend(&a[6], &bad, 5 * MILLI).await?;
    e.code("refused at the bottom of the tree", &d, "RECIPIENT_LISTED");
    expect_clean(&e, &a).await
}
