//! Attack and behaviour scenarios. Every one runs real signed transactions against the compiled
//! wasm on a NEAR node. `testnet: true` scenarios use no sandbox-only tool and also run on testnet.
use crate::env::*;
use anyhow::{anyhow, Result};
use futures::future::join_all;
use near_workspaces::types::{Gas, KeyType, NearToken, SecretKey};
use near_workspaces::{Account, AccountId, AccessKey};
use serde_json::{json, Value};
use std::future::Future;
use std::pin::Pin;

type Run = fn(Env) -> Pin<Box<dyn Future<Output = Result<()>> + Send>>;

#[derive(Clone)]
pub struct Scenario {
    pub id: &'static str,
    pub group: &'static str,
    pub title: &'static str,
    pub testnet: bool,
    pub run: Run,
}

macro_rules! sc {
    ($id:literal, $group:literal, $title:literal, $testnet:literal, $f:ident) => {
        Scenario { id: $id, group: $group, title: $title, testnet: $testnet, run: |e| Box::pin($f(e)) }
    };
}

pub fn all() -> Vec<Scenario> {
    vec![
        sc!("A01", "attenuation", "child cap above parent's remaining is refused", true, a01),
        sc!("A02", "attenuation", "child per-tx limit above parent is refused", false, a02),
        sc!("A03", "attenuation", "child window cap above parent is refused", false, a03),
        sc!("A04", "attenuation", "child with a shorter window is refused", false, a04),
        sc!("A05", "attenuation", "child approval threshold above parent is refused", false, a05),
        sc!("A06", "attenuation", "child recipient outside parent's list is refused", true, a06),
        sc!("A07", "attenuation", "child asking for any recipient under a list is refused", false, a07),
        sc!("A08", "attenuation", "child method outside parent's set is refused", false, a08),
        sc!("A09", "attenuation", "child expiry after parent is refused", false, a09),
        sc!("A10", "attenuation", "delegation past the depth limit is refused", false, a10),
        sc!("A11", "attenuation", "child denial limit above parent is refused", false, a11),
        sc!("A12", "attenuation", "child gas allowance beyond parent's budget is refused", false, a12),
        sc!("A13", "attenuation", "child node budget beyond parent's budget is refused", false, a13),
        sc!("A14", "attenuation", "child in another asset is refused", false, a14),
        sc!("A15", "attenuation", "empty method list is refused: it would mean every method", false, a15),
        sc!("A16", "attenuation", "cap check uses what is left, not the nominal cap", false, a16),
        sc!("A17", "attenuation", "oversubscribed siblings stop exactly at the parent's cap", true, a17),
        sc!("A18", "attenuation", "a mid-tree ancestor limit binds a leaf with room of its own", false, a18),
        sc!("B01", "impersonation", "child cannot revoke its parent", true, b01),
        sc!("B02", "impersonation", "child cannot approve its own queued spend", false, b02),
        sc!("B03", "impersonation", "sibling cannot revoke sibling", false, b03),
        sc!("B04", "impersonation", "contract relaying a call is not an agent", false, b04),
        sc!("B05", "impersonation", "same key on another account is not the agent", false, b05),
        sc!("B06", "impersonation", "malicious token re-entering mid-spend gets nothing", false, b06),
        sc!("B07", "impersonation", "agent key cannot call settlement callbacks", true, b07),
        sc!("B08", "impersonation", "agent key cannot transfer, add keys, attach deposit or leave the vault", true, b08),
        sc!("B09", "impersonation", "method subset is enforced by the protocol before the contract runs", false, b09),
        sc!("B10", "impersonation", "stranger cannot use owner methods", false, b10),
        sc!("B11", "impersonation", "vault refuses to initialise with itself as owner", false, b11),
        sc!("C01", "revocation", "one transaction revokes a depth-6 chain and deletes its keys", true, c01),
        sc!("C02", "revocation", "a deleted key cannot sign anything", true, c02),
        sc!("C03", "revocation", "a subtree too wide for one sweep is dead at once, keys reaped after", false, c03),
        sc!("C04", "revocation", "ancestor key revokes a subtree and siblings keep working", false, c04),
        sc!("C05", "revocation", "approved spend is dead once its grant is revoked", false, c05),
        sc!("C06", "revocation", "spend racing a revoke settles or is refused, never half", false, c06),
        sc!("C07", "revocation", "revoked parent cannot delegate", false, c07),
        sc!("C08", "revocation", "expiry stops a grant and reaps its key", false, c08),
        sc!("D01", "approval", "above threshold queues; funds move only after approval", true, d01),
        sc!("D02", "approval", "approval cannot be replayed", true, d02),
        sc!("D03", "approval", "changed recipient at execution fails the hash", false, d03),
        sc!("D04", "approval", "approving with the hash of another spend fails", false, d04),
        sc!("D05", "approval", "recipient blocked after approval: refused at execution", false, d05),
        sc!("D06", "approval", "budget consumed after approval: refused at execution", false, d06),
        sc!("D07", "approval", "approval climbs the tree, bounded by the approver's own threshold", false, d07),
        sc!("D08", "approval", "expired approval is refused", false, d08),
        sc!("D09", "approval", "approval dies with its approver", false, d09),
        sc!("D10", "approval", "queue is bounded per grant", false, d10),
        sc!("E01", "spend", "native spend moves the exact amount and settles every level", true, e01),
        sc!("E02", "spend", "NEP-141 spend settles through a real token contract", false, e02),
        sc!("E03", "spend", "failed native transfer rolls back all levels and the window", true, e03),
        sc!("E04", "spend", "failed ft_transfer rolls back and restores the token ledger", false, e04),
        sc!("E05", "spend", "balance check keeps storage funded", false, e05),
        sc!("E06", "spend", "12 concurrent spends from 12 keys stop exactly at the shared cap", false, e06),
        sc!("E07", "spend", "interleaved success and failure callbacks leave exact counters", false, e07),
        sc!("E08", "spend", "rolling window has no reset to straddle", false, e08),
        sc!("E09", "spend", "too little gas is a counted refusal, not a stuck reservation", false, e09),
        sc!("E10", "spend", "tokens the owner did not accept are returned", false, e10),
        sc!("E11", "spend", "splitting a payment does not get past the window", false, e11),
        sc!("E12", "spend", "owner withdrawal respects the same balance floor", false, e12),
        sc!("F01", "keys", "N denied attempts revoke the grant and delete its key", true, f01),
        sc!("F02", "keys", "protocol allowance is a hard stop on gas drain", false, f02),
        sc!("F03", "keys", "parent refills a locked-out child, paid from its own budget", false, f03),
        sc!("F04", "keys", "malformed args die before the counter and only the allowance bounds them", false, f04),
        sc!("F05", "keys", "rotation swaps the key and keeps the grant's history", false, f05),
        sc!("F06", "keys", "AddKey failure buries the grant and refunds the parent", false, f06),
        sc!("F07", "keys", "issued key has exactly the granted methods, receiver and allowance", true, f07),
        sc!("F08", "keys", "a denial is a successful transaction, so its counter persists", false, f08),
        sc!("F09", "keys", "a key already in use cannot be issued twice", false, f09),
        sc!("G01", "events", "denials are NEP-297 events with reason code and numbers", true, g01),
        sc!("P01", "controls", "pause stops spending and delegation but not revocation", false, p01),
        sc!("P02", "controls", "blocklist overrides an allowlist", false, p02),
    ]
    .into_iter()
    .chain(crate::recipient::all())
    .collect()
}

pub fn events(logs: &[&str], name: &str) -> Vec<Value> {
    logs.iter()
        .filter_map(|l| l.strip_prefix("EVENT_JSON:"))
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter(|v| v["event"] == name)
        .collect()
}

fn err_text<T, E: std::fmt::Debug>(r: std::result::Result<T, E>) -> Option<String> {
    r.err().map(|e| format!("{e:?}"))
}

async fn root_with_list(e: &Env) -> Result<(Agent, Tm)> {
    let mut t = Tm::root(e.now_ms().await?);
    t.recipients = Some(vec![e.r(0).to_string(), e.r(1).to_string()]);
    Ok((e.root(&t).await?, t))
}

/// Shared shape of the A scenarios: widen one dimension, expect its code, expect nothing changed.
async fn refused_dimension(e: &Env, code: &str, widen: impl FnOnce(&mut Tm)) -> Result<()> {
    let (root, t) = root_with_list(e).await?;
    let keys = e.key_count().await?;
    let before = e.grant(root.grant).await?;
    let mut c = t.child();
    widen(&mut c);
    let d = e.try_delegate(&root, &c).await?;
    e.eq("outcome", d.outcome.as_str(), "denied");
    e.code("reason code", &d, code);
    e.eq("no key was added", e.key_count().await?, keys);
    let after = e.grant(root.grant).await?;
    e.eq("parent gas budget unchanged", &after["gas_left"], &before["gas_left"]);
    e.eq("parent node budget unchanged", &after["nodes_left"], &before["nodes_left"]);
    e.eq("denial counted on the parent", after["denied"].as_u64(), Some(1));
    Ok(())
}

async fn a01(e: Env) -> Result<()> {
    refused_dimension(&e, "ATT_CAP", |c| c.cap += MILLI).await
}
async fn a02(e: Env) -> Result<()> {
    refused_dimension(&e, "ATT_PER_TX", |c| c.per_tx += 1).await
}
async fn a03(e: Env) -> Result<()> {
    refused_dimension(&e, "ATT_WINDOW_CAP", |c| c.window_cap += 1).await
}
async fn a04(e: Env) -> Result<()> {
    refused_dimension(&e, "ATT_WINDOW_LEN", |c| c.window_secs -= 1).await
}
async fn a05(e: Env) -> Result<()> {
    refused_dimension(&e, "ATT_THRESHOLD", |c| c.threshold += 1).await
}
async fn a06(e: Env) -> Result<()> {
    let extra = e.r(2).to_string();
    refused_dimension(&e, "ATT_RECIPIENTS", |c| c.recipients.as_mut().unwrap().push(extra)).await
}
async fn a07(e: Env) -> Result<()> {
    refused_dimension(&e, "ATT_RECIPIENTS", |c| c.recipients = None).await
}
async fn a08(e: Env) -> Result<()> {
    let t = Tm::root(e.now_ms().await?).methods(&["request_spend", "delegate"]);
    let root = e.root(&t).await?;
    let c = t.child().methods(&["request_spend", "revoke"]);
    let d = e.try_delegate(&root, &c).await?;
    e.code("reason code", &d, "ATT_METHODS");
    Ok(())
}
async fn a09(e: Env) -> Result<()> {
    refused_dimension(&e, "ATT_EXPIRY", |c| c.expires_at_ms += 1).await
}
async fn a10(e: Env) -> Result<()> {
    let mut t = Tm::root(e.now_ms().await?);
    t.max_depth = 2;
    let root = e.root(&t).await?;
    let d = e.try_delegate(&root, &{ let mut c = t.child(); c.max_depth = 2; c }).await?;
    e.code("child cannot keep the parent's depth", &d, "ATT_DEPTH");
    let c1t = t.child();
    let c1 = e.child(&root, &c1t).await?;
    let c2t = c1t.child();
    let c2 = e.child(&c1, &c2t).await?;
    e.eq("last level has depth 0 left", e.grant(c2.grant).await?["max_depth"].as_u64(), Some(0));
    let d = e.try_delegate(&c2, &c2t.child()).await?;
    e.code("depth 0 cannot delegate", &d, "ATT_DEPTH");
    Ok(())
}
async fn a11(e: Env) -> Result<()> {
    refused_dimension(&e, "ATT_DENIALS", |c| c.max_denials += 1).await
}
async fn a12(e: Env) -> Result<()> {
    refused_dimension(&e, "ATT_GAS", |c| c.gas_budget *= 100).await
}
async fn a13(e: Env) -> Result<()> {
    refused_dimension(&e, "ATT_NODES", |c| c.node_budget *= 100).await
}
async fn a14(e: Env) -> Result<()> {
    let token = e.vault_id().to_string();
    let t = Tm::root(e.now_ms().await?);
    let root = e.root(&t).await?;
    let mut c = t.child();
    c.asset = json!({"Ft": token});
    let d = e.try_delegate(&root, &c).await?;
    e.code("reason code", &d, "ATT_ASSET");
    Ok(())
}
async fn a15(e: Env) -> Result<()> {
    let t = Tm::root(e.now_ms().await?);
    let root = e.root(&t).await?;
    let d = e.try_delegate(&root, &t.child().methods(&[])).await?;
    e.code("empty method list refused", &d, "BAD_TERMS");
    Ok(())
}
async fn a16(e: Env) -> Result<()> {
    let t = Tm::root(e.now_ms().await?);
    let root = e.root(&t).await?;
    e.spend(&root, &e.r(0), 100 * MILLI).await?;
    let mut c = t.child();
    c.cap = 900 * MILLI + 1;
    let d = e.try_delegate(&root, &c).await?;
    e.code("nominal cap would pass, remaining does not", &d, "ATT_CAP");
    e.eq("limit reported is the remainder", d.limit, Some((900 * MILLI).to_string()));
    c.cap = 900 * MILLI;
    let d = e.try_delegate(&root, &c).await?;
    e.code("exactly the remainder is allowed", &d, "GRANTED");
    Ok(())
}
async fn a17(e: Env) -> Result<()> {
    let mut t = Tm::root(e.now_ms().await?);
    t.cap = 30 * MILLI;
    t.per_tx = 10 * MILLI;
    let root = e.root(&t).await?;
    let a = e.child(&root, &t.child()).await?;
    let b = e.child(&root, &t.child()).await?;
    e.eq("children together are promised twice the parent", e.field_u128(a.grant, "cap").await? + e.field_u128(b.grant, "cap").await?, 60 * MILLI);
    for agent in [&a, &b, &a] {
        let d = e.spend(agent, &e.r(0), 10 * MILLI).await?;
        e.eq("spend inside the shared cap settles", d.outcome.as_str(), "settled");
    }
    let d = e.spend(&b, &e.r(0), 10 * MILLI).await?;
    e.code("next spend hits the parent", &d, "ANCESTOR_CAP");
    e.eq("blocking grant is the root", d.blocking_grant, Some(root.grant));
    e.eq("root total equals its cap", e.field_u128(root.grant, "spent").await?, 30 * MILLI);
    Ok(())
}
async fn a18(e: Env) -> Result<()> {
    let t = Tm::root(e.now_ms().await?);
    let root = e.root(&t).await?;
    let mut mid_t = t.child();
    mid_t.cap = 150 * MILLI;
    let mid = e.child(&root, &mid_t).await?;
    let leaf_t = mid_t.child();
    let leaf = e.child(&mid, &leaf_t).await?;
    let sibling = e.child(&mid, &leaf_t).await?;
    e.spend(&sibling, &e.r(0), 100 * MILLI).await?;
    let d = e.spend(&leaf, &e.r(0), 100 * MILLI).await?;
    e.code("mid-tree limit binds", &d, "ANCESTOR_CAP");
    e.eq("blocking grant is the middle one", d.blocking_grant, Some(mid.grant));
    e.eq("leaf has only spent nothing", e.field_u128(leaf.grant, "spent").await?, 0);
    let view = e.grant(leaf.grant).await?;
    e.eq("view reports the lineage minimum", view["spendable"].as_str(), Some((50 * MILLI).to_string().as_str()));
    Ok(())
}

async fn b01(e: Env) -> Result<()> {
    let t = Tm::root(e.now_ms().await?);
    let root = e.root(&t).await?;
    let child = e.child(&root, &t.child()).await?;
    let d = e.revoke_by(&child, root.grant).await?;
    e.code("reason code", &d, "NOT_ANCESTOR");
    e.eq("parent still live", e.grant(root.grant).await?["live"].as_bool(), Some(true));
    Ok(())
}
async fn b02(e: Env) -> Result<()> {
    let mut t = Tm::root(e.now_ms().await?);
    t.threshold = 10 * MILLI;
    let root = e.root(&t).await?;
    let mut ct = t.child();
    ct.threshold = 5 * MILLI;
    let child = e.child(&root, &ct).await?;
    let q = e.spend(&child, &e.r(0), 20 * MILLI).await?;
    e.eq("queued", q.outcome.as_str(), "queued");
    let d = e.approve_as(&child.acct, "child", q.approval_id.unwrap(), q.spend_hash.as_ref().unwrap()).await?;
    e.code("self-approval refused", &d, "NOT_ANCESTOR");
    Ok(())
}
async fn b03(e: Env) -> Result<()> {
    let t = Tm::root(e.now_ms().await?);
    let root = e.root(&t).await?;
    let a = e.child(&root, &t.child()).await?;
    let b = e.child(&root, &t.child()).await?;
    let d = e.revoke_by(&a, b.grant).await?;
    e.code("reason code", &d, "NOT_ANCESTOR");
    Ok(())
}
async fn b04(e: Env) -> Result<()> {
    let t = Tm::root(e.now_ms().await?);
    let root = e.root(&t).await?;
    let relay = e.relay().await?;
    let attacker = e.new_account("atk", 5).await?;
    let r = attacker
        .call(relay.id(), "forward")
        .args_json(json!({
            "target": e.vault_id(),
            "method": "request_spend",
            "args": json!({"recipient": attacker.id(), "amount": (10 * MILLI).to_string()}).to_string(),
        }))
        .gas(Gas::from_tgas(200))
        .transact()
        .await?;
    let d: Decision = r.json()?;
    e.code("reason code", &d, "NOT_DIRECT_KEY");
    e.eq("nothing spent", e.field_u128(root.grant, "spent").await?, 0);
    Ok(())
}
async fn b05(e: Env) -> Result<()> {
    let t = Tm::root(e.now_ms().await?);
    let root = e.root(&t).await?;
    let relay = e.relay().await?;
    let attacker = e.new_account("atk", 5).await?;
    // The attacker holds the agent's secret key and registers it on their own account, so the
    // signer public key the vault sees is a real grant key, on the wrong account.
    let r = attacker
        .batch(attacker.id())
        .add_key(root.pk.clone(), AccessKey::full_access())
        .transact()
        .await?;
    e.eq("key registered on the attacker account", r.is_success(), true);
    let mimic = attacker.clone_with_key(root.sk.clone());
    let r = mimic
        .call(relay.id(), "forward")
        .args_json(json!({
            "target": e.vault_id(),
            "method": "request_spend",
            "args": json!({"recipient": attacker.id(), "amount": (10 * MILLI).to_string()}).to_string(),
        }))
        .gas(Gas::from_tgas(200))
        .transact()
        .await?;
    let d: Decision = r.json()?;
    e.code("reason code", &d, "NOT_DIRECT_KEY");
    e.eq("nothing spent", e.field_u128(root.grant, "spent").await?, 0);
    Ok(())
}
async fn b06(e: Env) -> Result<()> {
    let ft = e.funded_token(1_000).await?;
    let mut t = Tm::root(e.now_ms().await?);
    t.asset = json!({"Ft": ft.id()});
    t.cap = 500;
    t.per_tx = 100;
    t.window_cap = 500;
    t.threshold = 100;
    let root = e.root(&t).await?;
    e.register_on_token(&ft, &e.recipients[0]).await?;
    // The evil token calls back into the vault in the middle of ft_transfer. In that receipt the
    // signer is the vault and the signer key is the agent's real key. Only predecessor differs.
    let replay = json!({"recipient": e.r(0), "amount": "100"}).to_string();
    let r = ft
        .call(ft.id(), "set_reenter")
        .args_json(json!({"target": e.vault_id(), "method": "request_spend", "args": replay}))
        .transact()
        .await?;
    e.eq("switch set", r.is_success(), true);
    let r = e.spend_tx(&root, &e.r(0), 100, None).await?;
    let denied = events(&r.logs(), "denied");
    e.eq("re-entrant call was refused", denied.first().map(|d| d["data"][0]["code"].clone()), Some(json!("NOT_DIRECT_KEY")));
    e.eq("exactly one spend settled", e.field_u128(root.grant, "spent").await?, 100);
    e.eq("recipient got one payment", e.token_balance(&ft, &e.r(0)).await?, 100);
    Ok(())
}
async fn b07(e: Env) -> Result<()> {
    let t = Tm::root(e.now_ms().await?);
    let root = e.root(&t).await?;
    let r = root
        .acct
        .call(e.vault_id(), "on_spend_settled")
        .args_json(json!({"spend_id": 1}))
        .gas(Gas::from_tgas(50))
        .transact()
        .await;
    let text = err_text(r).unwrap_or_default();
    e.check("protocol rejects the callback name for this key", text.contains("MethodNameMismatch"), "MethodNameMismatch", &text);
    Ok(())
}
async fn b08(e: Env) -> Result<()> {
    let t = Tm::root(e.now_ms().await?);
    let root = e.root(&t).await?;
    let before = e.balance(&e.recipients[0]).await?;

    let text = err_text(root.acct.batch(&e.r(0)).transfer(NearToken::from_yoctonear(1)).transact().await).unwrap_or_default();
    e.check("plain transfer refused", !text.is_empty(), "protocol error", &text);

    let other = SecretKey::from_random(KeyType::ED25519).public_key();
    let text = err_text(root.acct.batch(e.vault_id()).add_key(other, AccessKey::full_access()).transact().await).unwrap_or_default();
    e.check("adding a key refused", !text.is_empty(), "protocol error", &text);

    let text = err_text(
        root.acct
            .call(e.vault_id(), "request_spend")
            .args_json(json!({"recipient": e.r(0), "amount": "1"}))
            .deposit(NearToken::from_yoctonear(1))
            .gas(Gas::from_tgas(100))
            .transact()
            .await,
    )
    .unwrap_or_default();
    e.check("attached deposit refused", text.contains("DepositWithFunctionCall"), "DepositWithFunctionCall", &text);

    let text = err_text(root.acct.call(&e.r(0), "anything").gas(Gas::from_tgas(50)).transact().await).unwrap_or_default();
    e.check("other receiver refused", text.contains("ReceiverMismatch"), "ReceiverMismatch", &text);

    e.eq("recipient balance unchanged", e.balance(&e.recipients[0]).await?, before);
    Ok(())
}
async fn b09(e: Env) -> Result<()> {
    let t = Tm::root(e.now_ms().await?);
    let root = e.root(&t).await?;
    let ct = t.child().methods(&["request_spend"]);
    let child = e.child(&root, &ct).await?;
    let text = err_text(
        child
            .acct
            .call(e.vault_id(), "delegate")
            .args_json(json!({"child_pk": child.pk, "terms": ct.json()}))
            .gas(Gas::from_tgas(100))
            .transact()
            .await,
    )
    .unwrap_or_default();
    e.check("protocol stops it", text.contains("MethodNameMismatch"), "MethodNameMismatch", &text);
    e.eq("contract never ran: counter still zero", e.grant(child.grant).await?["denied"].as_u64(), Some(0));
    Ok(())
}
async fn b10(e: Env) -> Result<()> {
    let stranger = &e.recipients[0];
    let r = stranger.call(e.vault_id(), "pause").gas(Gas::from_tgas(30)).transact().await?;
    e.check("pause by stranger fails", r.is_failure(), "failure", format!("{:?}", r.failures().len()));
    e.eq("vault not paused", e.info().await?["paused"].as_bool(), Some(false));
    Ok(())
}
async fn b11(e: Env) -> Result<()> {
    let fresh = e.new_account("selfowned", 8).await?;
    let r = fresh.deploy(&e.wasm().await?).await?;
    let c = r.into_result()?;
    let r = c
        .call("new")
        .args_json(json!({"owner": fresh.id(), "gas_floor": "0", "approval_ttl_secs": 60}))
        .transact()
        .await?;
    e.check("init refused", r.is_failure(), "failure", format!("{:?}", r.is_failure()));
    Ok(())
}

pub async fn chain(e: &Env, levels: usize) -> Result<(Vec<Agent>, Vec<Tm>)> {
    let mut t = Tm::root(e.now_ms().await?);
    t.max_depth = levels.max(6) as u8;
    let mut agents = vec![e.root(&t).await?];
    let mut terms = vec![t];
    for i in 0..levels {
        let c = terms[i].child();
        agents.push(e.child(&agents[i], &c).await?);
        terms.push(c);
    }
    Ok((agents, terms))
}

async fn c01(e: Env) -> Result<()> {
    let (a, _) = chain(&e, 6).await?;
    e.eq("tree has 7 levels", a.len(), 7);
    e.eq("deepest grant reports depth 6", e.grant(a[6].grant).await?["depth"].as_u64(), Some(6));
    let d = e.spend(&a[6], &e.r(0), 10 * MILLI).await?;
    e.eq("deepest grant can spend before revoke", d.outcome.as_str(), "settled");
    let (d, r) = e.revoke_by_owner(a[1].grant).await?;
    e.eq("one transaction", d.code.as_str(), "REVOKED");
    e.check("tx succeeded", r.is_success(), true, r.is_success());
    for agent in &a[1..] {
        e.eq("every level is dead", e.grant(agent.grant).await?["live"].as_bool(), Some(false));
        e.eq("every level's key is gone from the account", e.has_key(&agent.pk).await?, false);
        e.eq("spendable is zero", e.grant(agent.grant).await?["spendable"].as_str(), Some("0"));
    }
    e.eq("root untouched", e.grant(a[0].grant).await?["live"].as_bool(), Some(true));
    let d = e.spend(&a[0], &e.r(0), 10 * MILLI).await?;
    e.eq("root still spends", d.outcome.as_str(), "settled");
    Ok(())
}
async fn c02(e: Env) -> Result<()> {
    let (a, _) = chain(&e, 2).await?;
    let _ = e.revoke_by_owner(a[1].grant).await?;
    let text = err_text(e.spend_tx(&a[2], &e.r(0), MILLI, None).await).unwrap_or_default();
    e.check("deleted key is rejected by the protocol", !text.is_empty(), "protocol error", &text);
    e.eq("nothing spent", e.field_u128(a[2].grant, "spent").await?, 0);
    Ok(())
}
async fn c03(e: Env) -> Result<()> {
    let t = Tm::root(e.now_ms().await?);
    let root = e.root(&t).await?;
    let mut pt = t.child();
    pt.gas_budget = 6_000 * MILLI;
    pt.node_budget = 32;
    let parent = e.child(&root, &pt).await?;
    let mut leaf_t = pt.child();
    leaf_t.key_allowance = 150 * MILLI;
    leaf_t.gas_budget = 0;
    leaf_t.node_budget = 0;
    leaf_t.max_depth = 0;
    let mut kids = vec![];
    for _ in 0..30 {
        kids.push(e.child(&parent, &leaf_t).await?);
    }
    let keys_with_tree = e.key_count().await?;
    let (d, r) = e.revoke_by_owner(parent.grant).await?;
    let tgas = r.total_gas_burnt.as_gas() as f64 / 1e12;
    e.check("revoke stays far below the 300 Tgas limit", tgas < 100.0, "< 100 Tgas", format!("{tgas:.2} Tgas, {}", d.detail));
    let backlog = e.info().await?["sweep_backlog"].as_u64().unwrap();
    e.check("the sweep left work for later", backlog > 0, "> 0", backlog);
    let mut probed_delegate = false;
    for k in &kids {
        if e.has_key(&k.pk).await? {
            // A key the sweep has not reached yet: the revocation binds it anyway.
            if !probed_delegate {
                let d = e.try_delegate(k, &leaf_t).await?;
                e.code("unswept key cannot delegate", &d, "ANCESTOR_REVOKED");
                probed_delegate = true;
                e.sleep_ms(1500).await;
                e.eq("its own attempt reaped the key", e.has_key(&k.pk).await?, false);
                continue;
            }
            let d = e.spend(k, &e.r(0), MILLI).await?;
            e.code("unswept key cannot spend", &d, "ANCESTOR_REVOKED");
            e.sleep_ms(1500).await;
            e.eq("its own attempt reaped the key", e.has_key(&k.pk).await?, false);
            break;
        }
    }
    let sweeper = e.recipients[1].clone();
    for _ in 0..4 {
        let r = sweeper.call(e.vault_id(), "sweep").args_json(json!({"limit": 64})).gas(Gas::from_tgas(200)).transact().await?;
        e.check("anyone may sweep", r.is_success(), true, r.is_success());
        if e.info().await?["sweep_backlog"].as_u64() == Some(0) {
            break;
        }
    }
    e.eq("backlog cleared", e.info().await?["sweep_backlog"].as_u64(), Some(0));
    e.eq("31 keys gone", keys_with_tree - e.key_count().await?, 31);
    Ok(())
}
async fn c04(e: Env) -> Result<()> {
    let (a, terms) = chain(&e, 3).await?;
    let sibling = e.child(&a[1], &terms[1].child()).await?;
    let d = e.revoke_by(&a[1], a[2].grant).await?;
    e.code("mid-tree key revokes its child", &d, "REVOKED");
    e.eq("target dead", e.grant(a[2].grant).await?["live"].as_bool(), Some(false));
    e.eq("target's child dead", e.grant(a[3].grant).await?["live"].as_bool(), Some(false));
    let d = e.spend(&sibling, &e.r(0), MILLI).await?;
    e.eq("sibling keeps working", d.outcome.as_str(), "settled");
    Ok(())
}
async fn c05(e: Env) -> Result<()> {
    let mut t = Tm::root(e.now_ms().await?);
    t.threshold = 10 * MILLI;
    let root = e.root(&t).await?;
    let child = e.child(&root, &t.child()).await?;
    let q = e.spend(&child, &e.r(0), 50 * MILLI).await?;
    let id = q.approval_id.unwrap();
    e.approve_owner(id, q.spend_hash.as_ref().unwrap()).await?;
    let _ = e.revoke_by_owner(child.grant).await?;
    let r = e
        .owner_call("execute_approved", json!({"approval_id": id, "recipient": e.r(0), "amount": (50 * MILLI).to_string(), "memo": null}))
        .await?;
    let d: Decision = r.json()?;
    e.code("execution re-checks the lineage", &d, "REVOKED");
    e.eq("no funds moved", e.field_u128(child.grant, "spent").await?, 0);
    Ok(())
}
async fn c06(e: Env) -> Result<()> {
    let (a, _) = chain(&e, 2).await?;
    // One serial spend first, so the client's nonce cache is warm and the three below get distinct
    // nonces. Without that, identical concurrent transactions collapse into one on the node.
    e.spend(&a[2], &e.r(0), 5 * MILLI).await?;
    let before = e.balance(&e.recipients[0]).await?;
    let to = e.r(0);
    let (s1, s2, s3, rv) = tokio::join!(
        e.spend_tx(&a[2], &to, 5 * MILLI, Some("one")),
        e.spend_tx(&a[2], &to, 5 * MILLI, Some("two")),
        e.spend_tx(&a[2], &to, 5 * MILLI, Some("three")),
        e.revoke_by_owner(a[1].grant)
    );
    rv?;
    let settled = [s1, s2, s3]
        .into_iter()
        .filter_map(|r| r.ok())
        .filter_map(|r| r.json::<Decision>().ok())
        .filter(|d| d.outcome == "settled")
        .count() as u128;
    e.sleep_ms(1500).await;
    let got = e.balance(&e.recipients[0]).await? - before;
    e.eq("recipient received exactly what settled", got, settled * 5 * MILLI);
    e.eq("counters match the money", e.field_u128(a[2].grant, "spent").await?, (settled + 1) * 5 * MILLI);
    e.eq("nothing left reserved", e.field_u128(a[0].grant, "reserved").await?, 0);
    Ok(())
}
async fn c07(e: Env) -> Result<()> {
    let (a, terms) = chain(&e, 3).await?;
    let (d, _) = e.revoke_by_owner(a[1].grant).await?;
    e.code("revoked", &d, "REVOKED");
    let text = err_text(
        a[3].acct
            .call(e.vault_id(), "delegate")
            .args_json(json!({"child_pk": a[0].pk, "terms": terms[3].child().json()}))
            .gas(Gas::from_tgas(100))
            .transact()
            .await,
    )
    .unwrap_or_default();
    e.check("delegation by a reaped key is impossible", !text.is_empty(), "protocol error", &text);
    Ok(())
}
async fn c08(e: Env) -> Result<()> {
    let t = Tm::root(e.now_ms().await?);
    let root = e.root(&t).await?;
    let mut ct = t.child();
    ct.expires_at_ms = e.now_ms().await? + 4_000;
    let child = e.child(&root, &ct).await?;
    e.wait_chain(ct.expires_at_ms + 200).await?;
    let d = e.spend(&child, &e.r(0), MILLI).await?;
    e.code("expired", &d, "EXPIRED");
    e.sleep_ms(1500).await;
    e.eq("key reaped by the attempt", e.has_key(&child.pk).await?, false);
    Ok(())
}

async fn queued_setup(e: &Env, amount: u128) -> Result<(Agent, Agent, Decision, Tm)> {
    let mut t = Tm::root(e.now_ms().await?);
    t.threshold = 10 * MILLI;
    let root = e.root(&t).await?;
    let ct = t.child();
    let child = e.child(&root, &ct).await?;
    let q = e.spend(&child, &e.r(0), amount).await?;
    Ok((root, child, q, ct))
}

async fn d01(e: Env) -> Result<()> {
    let before = e.balance(&e.recipients[0]).await?;
    let (_, child, q, _) = queued_setup(&e, 50 * MILLI).await?;
    e.eq("request is queued", q.outcome.as_str(), "queued");
    e.eq("nothing moved yet", e.balance(&e.recipients[0]).await?, before);
    e.eq("nothing reserved yet", e.field_u128(child.grant, "reserved").await?, 0);
    let id = q.approval_id.unwrap();
    let hash = q.spend_hash.unwrap();
    let early = e.execute(&child, id, &e.r(0), 50 * MILLI).await?;
    e.code("cannot execute before approval", &early, "APPROVAL_NOT_APPROVED");
    let d = e.approve_owner(id, &hash).await?;
    e.code("owner approves the hash", &d, "APPROVED");
    let d = e.execute(&child, id, &e.r(0), 50 * MILLI).await?;
    e.eq("execution settles", d.outcome.as_str(), "settled");
    e.eq("recipient paid", e.balance(&e.recipients[0]).await? - before, 50 * MILLI);
    Ok(())
}
async fn d02(e: Env) -> Result<()> {
    let (_, child, q, _) = queued_setup(&e, 50 * MILLI).await?;
    let id = q.approval_id.unwrap();
    e.approve_owner(id, q.spend_hash.as_ref().unwrap()).await?;
    let d = e.execute(&child, id, &e.r(0), 50 * MILLI).await?;
    e.eq("first execution settles", d.outcome.as_str(), "settled");
    let d = e.execute(&child, id, &e.r(0), 50 * MILLI).await?;
    e.code("replay refused", &d, "APPROVAL_NOT_FOUND");
    e.eq("spent once", e.field_u128(child.grant, "spent").await?, 50 * MILLI);
    Ok(())
}
async fn d03(e: Env) -> Result<()> {
    let (_, child, q, _) = queued_setup(&e, 50 * MILLI).await?;
    let id = q.approval_id.unwrap();
    e.approve_owner(id, q.spend_hash.as_ref().unwrap()).await?;
    let d = e.execute(&child, id, &e.r(1), 50 * MILLI).await?;
    e.code("other recipient", &d, "APPROVAL_HASH_MISMATCH");
    let d = e.execute(&child, id, &e.r(0), 51 * MILLI).await?;
    e.code("other amount", &d, "APPROVAL_HASH_MISMATCH");
    Ok(())
}
async fn d04(e: Env) -> Result<()> {
    let (_, child, q, _) = queued_setup(&e, 50 * MILLI).await?;
    let q2 = e.spend(&child, &e.r(1), 60 * MILLI).await?;
    let d = e.approve_owner(q.approval_id.unwrap(), q2.spend_hash.as_ref().unwrap()).await?;
    e.code("hash of a different spend", &d, "APPROVAL_HASH_MISMATCH");
    let view: Value = e.vault.view(e.vault_id(), "get_approval").args_json(json!({"approval_id": q.approval_id})).await?.json()?;
    e.eq("still unapproved", view["approved_by"].clone(), Value::Null);
    Ok(())
}
async fn d05(e: Env) -> Result<()> {
    let (_, child, q, _) = queued_setup(&e, 50 * MILLI).await?;
    let id = q.approval_id.unwrap();
    e.approve_owner(id, q.spend_hash.as_ref().unwrap()).await?;
    e.owner_call("block_recipient", json!({"account": e.r(0)})).await?.into_result()?;
    let d = e.execute(&child, id, &e.r(0), 50 * MILLI).await?;
    e.code("re-checked at execution", &d, "RECIPIENT_BLOCKED");
    Ok(())
}
async fn d06(e: Env) -> Result<()> {
    let mut t = Tm::root(e.now_ms().await?);
    t.threshold = 10 * MILLI;
    t.cap = 60 * MILLI;
    let root = e.root(&t).await?;
    let child = e.child(&root, &t.child()).await?;
    let q = e.spend(&child, &e.r(0), 50 * MILLI).await?;
    let id = q.approval_id.unwrap();
    e.approve_owner(id, q.spend_hash.as_ref().unwrap()).await?;
    for _ in 0..2 {
        e.spend(&root, &e.r(1), 10 * MILLI).await?;
    }
    let d = e.execute(&child, id, &e.r(0), 50 * MILLI).await?;
    e.code("budget re-checked at execution", &d, "ANCESTOR_CAP");
    Ok(())
}
async fn d07(e: Env) -> Result<()> {
    let mut t = Tm::root(e.now_ms().await?);
    t.threshold = 40 * MILLI;
    let root = e.root(&t).await?;
    let mut mid_t = t.child();
    mid_t.threshold = 20 * MILLI;
    let mid = e.child(&root, &mid_t).await?;
    let mut leaf_t = mid_t.child();
    leaf_t.threshold = 5 * MILLI;
    let leaf = e.child(&mid, &leaf_t).await?;

    let q = e.spend(&leaf, &e.r(0), 15 * MILLI).await?;
    let d = e.approve_as(&mid.acct, "mid", q.approval_id.unwrap(), q.spend_hash.as_ref().unwrap()).await?;
    e.code("parent approves within its own threshold", &d, "APPROVED");
    let d = e.execute(&leaf, q.approval_id.unwrap(), &e.r(0), 15 * MILLI).await?;
    e.eq("settles without any human", d.outcome.as_str(), "settled");

    let q = e.spend(&leaf, &e.r(0), 30 * MILLI).await?;
    let id = q.approval_id.unwrap();
    let hash = q.spend_hash.unwrap();
    let d = e.approve_as(&mid.acct, "mid", id, &hash).await?;
    e.code("above the parent's threshold", &d, "APPROVER_LIMIT");
    let d = e.approve_as(&root.acct, "root", id, &hash).await?;
    e.code("the next level up can", &d, "APPROVED");

    let q = e.spend(&leaf, &e.r(0), 50 * MILLI).await?;
    let id = q.approval_id.unwrap();
    let hash = q.spend_hash.unwrap();
    let d = e.approve_as(&root.acct, "root", id, &hash).await?;
    e.code("above every agent", &d, "APPROVER_LIMIT");
    let d = e.approve_owner(id, &hash).await?;
    e.code("only the owner can", &d, "APPROVED");
    Ok(())
}
async fn d08(e: Env) -> Result<()> {
    let (_, child, q, _) = queued_setup(&e, 50 * MILLI).await?;
    let id = q.approval_id.unwrap();
    e.approve_owner(id, q.spend_hash.as_ref().unwrap()).await?;
    let view: Value = e.vault.view(e.vault_id(), "get_approval").args_json(json!({"approval_id": id})).await?.json()?;
    e.wait_chain(view["expires_at_ms"].as_u64().unwrap() + 100).await?;
    let d = e.execute(&child, id, &e.r(0), 50 * MILLI).await?;
    e.code("approval expired", &d, "APPROVAL_EXPIRED");
    Ok(())
}
async fn d09(e: Env) -> Result<()> {
    let mut t = Tm::root(e.now_ms().await?);
    t.threshold = 40 * MILLI;
    let root = e.root(&t).await?;
    let mut mid_t = t.child();
    mid_t.threshold = 20 * MILLI;
    let mid = e.child(&root, &mid_t).await?;
    let mut leaf_t = mid_t.child();
    leaf_t.threshold = 5 * MILLI;
    let leaf = e.child(&mid, &leaf_t).await?;
    let q = e.spend(&leaf, &e.r(0), 15 * MILLI).await?;
    let id = q.approval_id.unwrap();
    e.approve_as(&mid.acct, "mid", id, q.spend_hash.as_ref().unwrap()).await?;
    let _ = e.revoke_by_owner(mid.grant).await?;
    let r = e
        .owner_call("execute_approved", json!({"approval_id": id, "recipient": e.r(0), "amount": (15 * MILLI).to_string(), "memo": null}))
        .await?;
    let d: Decision = r.json()?;
    e.code("approval died with the approver", &d, "APPROVER_REVOKED");
    Ok(())
}
async fn d10(e: Env) -> Result<()> {
    let (_, child, q, _) = queued_setup(&e, 50 * MILLI).await?;
    e.eq("first queued", q.outcome.as_str(), "queued");
    for _ in 0..7 {
        let d = e.spend(&child, &e.r(0), 50 * MILLI).await?;
        e.eq("queued", d.outcome.as_str(), "queued");
    }
    let d = e.spend(&child, &e.r(0), 50 * MILLI).await?;
    e.code("ninth refused", &d, "QUEUE_FULL");
    Ok(())
}

async fn e01(e: Env) -> Result<()> {
    let (a, _) = chain(&e, 3).await?;
    let before = e.balance(&e.recipients[0]).await?;
    let r = e.spend_tx(&a[3], &e.r(0), 7 * MILLI, Some("task-42")).await?;
    let logs = r.logs();
    let d: Decision = r.clone().json()?;
    e.eq("settled", d.outcome.as_str(), "settled");
    e.eq("recipient delta is exact", e.balance(&e.recipients[0]).await? - before, 7 * MILLI);
    for agent in &a {
        e.eq("every level counts it", e.field_u128(agent.grant, "spent").await?, 7 * MILLI);
        e.eq("nothing stays reserved", e.field_u128(agent.grant, "reserved").await?, 0);
    }
    let reserved = events(&logs, "spend_reserved");
    e.eq("reservation lists headroom at all 4 levels", reserved[0]["data"][0]["headroom_before"].as_array().map(Vec::len), Some(4));
    e.eq("memo is in the event", reserved[0]["data"][0]["memo"].as_str(), Some("task-42"));
    e.eq("settlement event emitted", events(&logs, "spend_settled").len(), 1);
    Ok(())
}
async fn e02(e: Env) -> Result<()> {
    let ft = e.funded_token(1_000).await?;
    let mut t = Tm::root(e.now_ms().await?);
    t.asset = json!({"Ft": ft.id()});
    t.cap = 500;
    t.per_tx = 100;
    t.window_cap = 500;
    t.threshold = 100;
    let root = e.root(&t).await?;
    let child = e.child(&root, &t.child()).await?;
    e.register_on_token(&ft, &e.recipients[0]).await?;
    let d = e.spend(&child, &e.r(0), 100).await?;
    e.eq("settled", d.outcome.as_str(), "settled");
    e.eq("recipient holds the tokens", e.token_balance(&ft, &e.r(0)).await?, 100);
    e.eq("vault ledger", e.vault_token_ledger(&ft).await?, 900);
    e.eq("vault real token balance", e.token_balance(&ft, e.vault_id()).await?, 900);
    Ok(())
}
async fn e03(e: Env) -> Result<()> {
    let (a, _) = chain(&e, 2).await?;
    let nobody: AccountId = format!("nobody-here.{}", e.owner.id()).parse()?;
    let before = e.balance(&e.vault).await?;
    let d = e.spend(&a[2], &nobody, 40 * MILLI).await?;
    e.eq("rolled back", d.outcome.as_str(), "rolled_back");
    e.code("reason", &d, "TRANSFER_FAILED");
    for agent in &a {
        e.eq("spent stays zero", e.field_u128(agent.grant, "spent").await?, 0);
        e.eq("reserved released", e.field_u128(agent.grant, "reserved").await?, 0);
        e.eq("window released", e.field_u128(agent.grant, "window_used").await?, 0);
    }
    e.sleep_ms(1500).await;
    let after = e.balance(&e.vault).await?;
    let lost = before.saturating_sub(after);
    e.check("refund came back: vault lost only gas", lost < 40 * MILLI, "< 40 milliNEAR", lost);
    let d = e.spend(&a[2], &e.r(0), 40 * MILLI).await?;
    e.eq("the same budget is usable again", d.outcome.as_str(), "settled");
    Ok(())
}
async fn e04(e: Env) -> Result<()> {
    let ft = e.funded_token(1_000).await?;
    let mut t = Tm::root(e.now_ms().await?);
    t.asset = json!({"Ft": ft.id()});
    t.cap = 500;
    t.per_tx = 100;
    t.window_cap = 500;
    t.threshold = 100;
    let root = e.root(&t).await?;
    e.register_on_token(&ft, &e.recipients[0]).await?;
    ft.call(ft.id(), "set_fail_transfers").args_json(json!({"fail": true})).transact().await?.into_result()?;
    let d = e.spend(&root, &e.r(0), 100).await?;
    e.eq("rolled back", d.outcome.as_str(), "rolled_back");
    e.eq("ledger restored", e.vault_token_ledger(&ft).await?, 1_000);
    e.eq("spent zero", e.field_u128(root.grant, "spent").await?, 0);
    e.eq("window released", e.field_u128(root.grant, "window_used").await?, 0);
    ft.call(ft.id(), "set_fail_transfers").args_json(json!({"fail": false})).transact().await?.into_result()?;
    let d = e.spend(&root, &e.r(0), 100).await?;
    e.eq("works again", d.outcome.as_str(), "settled");
    Ok(())
}
async fn e05(e: Env) -> Result<()> {
    let mut t = Tm::root(e.now_ms().await?);
    t.cap = 100_000 * MILLI;
    t.per_tx = 100_000 * MILLI;
    t.window_cap = 100_000 * MILLI;
    t.threshold = 100_000 * MILLI;
    let root = e.root(&t).await?;
    let available: u128 = e.info().await?["available_native"].as_str().unwrap().parse()?;
    let d = e.spend(&root, &e.r(0), available + 500 * MILLI).await?;
    e.code("more than free balance is refused", &d, "VAULT_BALANCE");
    e.eq("nothing reserved", e.field_u128(root.grant, "reserved").await?, 0);
    let available: u128 = e.info().await?["available_native"].as_str().unwrap().parse()?;
    let d = e.spend(&root, &e.r(0), available - 200 * MILLI).await?;
    e.eq("all but a sliver settles", d.outcome.as_str(), "settled");
    let info = e.info().await?;
    let left = e.balance(&e.vault).await?;
    let storage = e.vault.view_account().await?.storage_usage as u128 * 10_000_000_000_000_000_000;
    e.check("balance still covers storage", left > storage, format!("> {storage}"), left);
    let r = e.spend_tx(&root, &e.r(0), 1_000 * MILLI, None).await?;
    e.check("contract still answers", r.is_success(), true, format!("{}", info["available_native"]));
    Ok(())
}
async fn e06(e: Env) -> Result<()> {
    let mut t = Tm::root(e.now_ms().await?);
    t.cap = 50 * MILLI;
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
    let before = e.balance(&e.recipients[0]).await?;
    let to = e.r(0);
    let mut calls = vec![];
    for k in &kids {
        calls.push(e.spend_tx(k, &to, 10 * MILLI, None));
    }
    let results = join_all(calls).await;
    let mut settled = 0u128;
    let mut refused = 0u128;
    for r in results {
        let d: Decision = r?.json()?;
        match d.outcome.as_str() {
            "settled" => settled += 1,
            "denied" => {
                e.code("the only refusal is the shared cap", &d, "ANCESTOR_CAP");
                refused += 1;
            }
            other => return Err(anyhow!("unexpected {other}")),
        }
    }
    e.eq("exactly 5 of 12 got through", settled, 5);
    e.eq("the other 7 were refused", refused, 7);
    e.sleep_ms(1500).await;
    e.eq("root spent equals its cap", e.field_u128(root.grant, "spent").await?, 50 * MILLI);
    e.eq("recipient got exactly the cap", e.balance(&e.recipients[0]).await? - before, 50 * MILLI);
    Ok(())
}
async fn e07(e: Env) -> Result<()> {
    let t = Tm::root(e.now_ms().await?);
    let root = e.root(&t).await?;
    let a = e.child(&root, &t.child()).await?;
    let b = e.child(&root, &t.child()).await?;
    let nobody: AccountId = format!("nobody-here.{}", e.owner.id()).parse()?;
    let (r0, r1) = (e.r(0), e.r(1));
    let mut calls = vec![];
    for i in 0..10u128 {
        let (agent, to) = match i % 4 {
            0 => (&a, &r0),
            1 => (&b, &nobody),
            2 => (&a, &nobody),
            _ => (&b, &r1),
        };
        calls.push(e.spend_tx(agent, to, (i + 1) * MILLI, None));
    }
    let mut expect_ok = 0u128;
    for (i, r) in join_all(calls).await.into_iter().enumerate() {
        let d: Decision = r?.json()?;
        let good = matches!(i % 4, 0 | 3);
        e.eq("each spend ends the way its recipient dictates", d.outcome.as_str(), if good { "settled" } else { "rolled_back" });
        if good {
            expect_ok += (i as u128 + 1) * MILLI;
        }
    }
    e.eq("root spent is the sum of successes", e.field_u128(root.grant, "spent").await?, expect_ok);
    e.eq("root reserved is zero", e.field_u128(root.grant, "reserved").await?, 0);
    e.eq("window holds successes only", e.field_u128(root.grant, "window_used").await?, expect_ok);
    let sum = e.field_u128(a.grant, "spent").await? + e.field_u128(b.grant, "spent").await?;
    e.eq("children add up to the parent", sum, expect_ok);
    Ok(())
}
async fn e08(e: Env) -> Result<()> {
    let mut t = Tm::root(e.now_ms().await?);
    t.window_secs = 8;
    t.window_cap = 100 * MILLI;
    let root = e.root(&t).await?;
    let t0 = e.now_ms().await?;
    let d = e.spend(&root, &e.r(0), 60 * MILLI).await?;
    e.eq("first spend", d.outcome.as_str(), "settled");
    e.wait_chain(t0 + 3_000).await?;
    let d = e.spend(&root, &e.r(0), 40 * MILLI).await?;
    e.eq("second spend fills the window", d.outcome.as_str(), "settled");
    let d = e.spend(&root, &e.r(0), 30 * MILLI).await?;
    e.code("window is full", &d, "WINDOW");
    e.wait_chain(t0 + 9_500).await?;
    let d = e.spend(&root, &e.r(0), 50 * MILLI).await?;
    e.eq("only the first spend has aged out", d.outcome.as_str(), "settled");
    let d = e.spend(&root, &e.r(0), 20 * MILLI).await?;
    e.code("the second still counts: a fixed reset would have allowed this", &d, "WINDOW");
    Ok(())
}
async fn e09(e: Env) -> Result<()> {
    let t = Tm::root(e.now_ms().await?);
    let root = e.root(&t).await?;
    let r = root
        .acct
        .call(e.vault_id(), "request_spend")
        .args_json(json!({"recipient": e.r(0), "amount": MILLI.to_string()}))
        .gas(Gas::from_tgas(30))
        .transact()
        .await?;
    let d: Decision = r.json()?;
    e.code("reason code", &d, "GAS_TOO_LOW");
    e.eq("counted", e.grant(root.grant).await?["denied"].as_u64(), Some(1));
    e.eq("nothing reserved", e.field_u128(root.grant, "reserved").await?, 0);
    Ok(())
}
async fn e10(e: Env) -> Result<()> {
    let ft = e.token_unaccepted(1_000).await?;
    e.eq("sender keeps the tokens", e.token_balance(&ft, ft.id()).await?, 1_000);
    e.eq("vault holds none", e.token_balance(&ft, e.vault_id()).await?, 0);
    let ledger: Option<String> = e.vault.view(e.vault_id(), "ft_balance").args_json(json!({"token": ft.id()})).await?.json()?;
    e.eq("no ledger entry", ledger, None);
    Ok(())
}
async fn e11(e: Env) -> Result<()> {
    let mut t = Tm::root(e.now_ms().await?);
    t.per_tx = 75 * MILLI;
    t.window_cap = 150 * MILLI;
    let root = e.root(&t).await?;
    let d = e.spend(&root, &e.r(0), 80 * MILLI).await?;
    e.code("one large payment hits per-tx", &d, "PER_TX");
    for _ in 0..2 {
        let d = e.spend(&root, &e.r(0), 75 * MILLI).await?;
        e.eq("split part", d.outcome.as_str(), "settled");
    }
    let d = e.spend(&root, &e.r(0), 75 * MILLI).await?;
    e.code("the third part hits the window", &d, "WINDOW");
    Ok(())
}
async fn e12(e: Env) -> Result<()> {
    let r = e
        .owner_call("withdraw", json!({"asset": "Native", "amount": (500_000 * MILLI).to_string(), "to": e.owner.id()}))
        .await?;
    let d: Decision = r.json()?;
    e.code("over the free balance", &d, "VAULT_BALANCE");
    let before = e.balance(&e.recipients[0]).await?;
    let r = e
        .owner_call("withdraw", json!({"asset": "Native", "amount": (100 * MILLI).to_string(), "to": e.r(0)}))
        .await?;
    let d: Decision = r.json()?;
    e.eq("settled", d.outcome.as_str(), "settled");
    e.eq("paid", e.balance(&e.recipients[0]).await? - before, 100 * MILLI);
    Ok(())
}

async fn f01(e: Env) -> Result<()> {
    let t = Tm::root(e.now_ms().await?);
    let root = e.root(&t).await?;
    let mut ct = t.child();
    ct.max_denials = 3;
    ct.recipients = Some(vec![e.r(0).to_string()]);
    let child = e.child(&root, &ct).await?;
    let grand = e.child(&child, &ct.child()).await?;
    for i in 1..=3u64 {
        let d = e.spend(&child, &e.r(1), MILLI).await?;
        e.code("injected payment outside the list", &d, "RECIPIENT_NOT_ALLOWED");
        if i < 3 {
            e.eq("counter", e.grant(child.grant).await?["denied"].as_u64(), Some(i));
        }
    }
    e.sleep_ms(1500).await;
    let g = e.grant(child.grant).await?;
    e.eq("grant revoked by the vault itself", g["revoked"].as_bool(), Some(true));
    e.eq("key gone from the account", e.has_key(&child.pk).await?, false);
    e.eq("its subtree is dead too", e.grant(grand.grant).await?["live"].as_bool(), Some(false));
    e.eq("subtree key gone as well", e.has_key(&grand.pk).await?, false);
    let text = err_text(e.spend_tx(&child, &e.r(0), MILLI, None).await).unwrap_or_default();
    e.check("further attempts cannot even be signed", !text.is_empty(), "protocol error", &text);
    Ok(())
}
async fn f02(e: Env) -> Result<()> {
    let mut t = Tm::root(e.now_ms().await?);
    t.max_denials = 1_000;
    let root = e.root(&t).await?;
    let mut ct = t.child();
    // The sandbox prices gas at 1e9 yocto, so a 60 Tgas call must find 60 milliNEAR of allowance.
    ct.key_allowance = 75 * MILLI;
    let child = e.child(&root, &ct).await?;
    let before = e.balance(&e.vault).await?;
    let mut attempts = 0u32;
    let mut burnt = 0f64;
    let mut error = String::new();
    for i in 0..120 {
        match e.spend_with_gas(&child, &e.r(0), 1_000 * MILLI, Some(&format!("drain {i}")), 60).await {
            Ok(r) => {
                attempts += 1;
                burnt += r.total_gas_burnt.as_gas() as f64 / 1e12;
            }
            Err(err) => {
                error = format!("{err:?}");
                break;
            }
        }
    }
    e.check("the attack really ran", attempts >= 3, ">= 3 attempts reached the contract", attempts);
    e.eq("each attempt was counted", e.grant(child.grant).await?["denied"].as_u64(), Some(attempts as u64));
    e.check("protocol stopped the key", error.contains("NotEnoughAllowance"), "NotEnoughAllowance", format!("after {attempts} attempts: {error}"));
    e.sleep_ms(2000).await;
    let after = e.balance(&e.vault).await?;
    let lost = before as i128 - after as i128;
    e.check("vault lost no more than the allowance", lost <= 75 * MILLI as i128, "<= 75 milliNEAR", format!("balance change {lost} yocto over {attempts} attempts, {burnt:.1} Tgas burnt"));
    Ok(())
}
async fn f03(e: Env) -> Result<()> {
    let mut t = Tm::root(e.now_ms().await?);
    t.max_denials = 1_000;
    let root = e.root(&t).await?;
    let mut ct = t.child();
    ct.key_allowance = 70 * MILLI;
    let child = e.child(&root, &ct).await?;
    let mut locked = false;
    let mut attempts = 0;
    for i in 0..120 {
        if e.spend_with_gas(&child, &e.r(0), 1_000 * MILLI, Some(&format!("drain {i}")), 60).await.is_err() {
            locked = true;
            break;
        }
        attempts += 1;
    }
    e.check("the key was really used up", attempts >= 1, ">= 1 attempt", attempts);
    e.eq("child is locked out", locked, true);
    let gas_before = e.field_u128(root.grant, "gas_left").await?;
    let r = root
        .acct
        .call(e.vault_id(), "refill_key")
        .args_json(json!({"grant_id": child.grant, "allowance_yocto": (300 * MILLI).to_string()}))
        .gas(Gas::from_tgas(60))
        .transact()
        .await?;
    let d: Decision = r.json()?;
    e.code("refilled", &d, "KEY_CHANGED");
    e.eq("paid from the parent's budget", gas_before - e.field_u128(root.grant, "gas_left").await?, 300 * MILLI);
    e.sleep_ms(1500).await;
    let d = e.spend(&child, &e.r(0), MILLI).await?;
    e.eq("same key works again", d.outcome.as_str(), "settled");
    let r = child
        .acct
        .call(e.vault_id(), "refill_key")
        .args_json(json!({"grant_id": child.grant, "allowance_yocto": MILLI.to_string()}))
        .gas(Gas::from_tgas(60))
        .transact()
        .await?;
    let d: Decision = r.json()?;
    e.code("a grant cannot refill itself", &d, "NOT_ANCESTOR");
    Ok(())
}
async fn f04(e: Env) -> Result<()> {
    let t = Tm::root(e.now_ms().await?);
    let root = e.root(&t).await?;
    let r = root
        .acct
        .call(e.vault_id(), "request_spend")
        .args_json(json!({"nonsense": true}))
        .gas(Gas::from_tgas(60))
        .transact()
        .await?;
    e.check("bad JSON fails inside the entry point", r.is_failure(), true, r.is_failure());
    e.eq("known limit: counter did not move", e.grant(root.grant).await?["denied"].as_u64(), Some(0));
    Ok(())
}
async fn f05(e: Env) -> Result<()> {
    let t = Tm::root(e.now_ms().await?);
    let root = e.root(&t).await?;
    let child = e.child(&root, &t.child()).await?;
    e.spend(&child, &e.r(0), 3 * MILLI).await?;
    let sk = SecretKey::from_random(KeyType::ED25519);
    let r = child
        .acct
        .call(e.vault_id(), "rotate_key")
        .args_json(json!({"grant_id": child.grant, "new_pk": sk.public_key()}))
        .gas(Gas::from_tgas(60))
        .transact()
        .await?;
    let d: Decision = r.json()?;
    e.code("rotated", &d, "KEY_CHANGED");
    e.eq("old key gone", e.has_key(&child.pk).await?, false);
    e.eq("new key on the account", e.has_key(&sk.public_key()).await?, true);
    let fresh = Agent { acct: e.vault.clone_with_key(sk.clone()), pk: sk.public_key(), sk, grant: child.grant };
    let d = e.spend(&fresh, &e.r(0), 4 * MILLI).await?;
    e.eq("new key spends under the same grant", d.outcome.as_str(), "settled");
    e.eq("history carried over", e.field_u128(child.grant, "spent").await?, 7 * MILLI);
    Ok(())
}
async fn f06(e: Env) -> Result<()> {
    let t = Tm::root(e.now_ms().await?);
    let root = e.root(&t).await?;
    let gas = e.field_u128(root.grant, "gas_left").await?;
    let nodes = e.grant(root.grant).await?["nodes_left"].as_u64();
    // The vault's own full-access key: the contract's map has never seen it, AddKey must fail.
    let vault_pk = e.vault.secret_key().public_key();
    let d = e.delegate_raw(&root, &t.child(), &vault_pk).await?;
    e.code("reason code", &d, "KEY_ADD_FAILED");
    let buried = d.blocking_grant.ok_or(anyhow!("no grant id"))?;
    e.eq("grant is dead", e.grant(buried).await?["revoked"].as_bool(), Some(true));
    e.eq("gas budget refunded", e.field_u128(root.grant, "gas_left").await?, gas);
    e.eq("node budget refunded", e.grant(root.grant).await?["nodes_left"].as_u64(), nodes);
    let mapped: Option<u64> = e.vault.view(e.vault_id(), "grant_of_key").args_json(json!({"pk": vault_pk})).await?.json()?;
    e.eq("no grant owns the vault's own key", mapped, None);
    Ok(())
}
async fn f07(e: Env) -> Result<()> {
    let t = Tm::root(e.now_ms().await?);
    let root = e.root(&t).await?;
    let ct = t.child().methods(&["request_spend", "execute_approved"]);
    let child = e.child(&root, &ct).await?;
    let key = e.vault.view_access_key(&child.pk).await?;
    let text = format!("{:?}", key.permission);
    e.check("exact method list", text.contains("request_spend") && text.contains("execute_approved") && !text.contains("delegate"), "request_spend, execute_approved only", &text);
    e.check("receiver is the vault", text.contains(e.vault_id().as_str()), e.vault_id(), &text);
    e.check("allowance is the granted amount", text.contains(&ct.key_allowance.to_string()), ct.key_allowance, &text);
    Ok(())
}
async fn f08(e: Env) -> Result<()> {
    let t = Tm::root(e.now_ms().await?);
    let root = e.root(&t).await?;
    let r = e.spend_tx(&root, &e.r(0), 101 * MILLI, None).await?;
    e.check("transaction status is success", r.is_success() && r.receipt_failures().is_empty(), true, r.is_success());
    let d: Decision = r.json()?;
    e.code("yet the spend was refused", &d, "PER_TX");
    e.eq("and the counter persisted", e.grant(root.grant).await?["denied"].as_u64(), Some(1));
    Ok(())
}
async fn f09(e: Env) -> Result<()> {
    let t = Tm::root(e.now_ms().await?);
    let root = e.root(&t).await?;
    let child = e.child(&root, &t.child()).await?;
    let d = e.delegate_raw(&root, &t.child(), &child.pk).await?;
    e.code("reason code", &d, "KEY_IN_USE");
    Ok(())
}
async fn g01(e: Env) -> Result<()> {
    let mut t = Tm::root(e.now_ms().await?);
    t.per_tx = 25 * MILLI;
    let root = e.root(&t).await?;
    let r = e.spend_tx(&root, &e.r(0), 26 * MILLI, None).await?;
    let logs = r.logs();
    let ev = events(&logs, "denied");
    e.eq("one denied event", ev.len(), 1);
    let v = &ev[0];
    e.eq("standard", v["standard"].as_str(), Some("espalier"));
    e.eq("version", v["version"].as_str(), Some("1.0.0"));
    let data = &v["data"][0];
    e.eq("code", data["code"].as_str(), Some("PER_TX"));
    e.eq("limit", data["limit"].as_str(), Some((25 * MILLI).to_string().as_str()));
    e.eq("attempted", data["attempted"].as_str(), Some((26 * MILLI).to_string().as_str()));
    let detail = data["detail"].as_str().unwrap_or_default();
    e.check("detail states the comparison", detail.ends_with("per-tx") && detail.contains(" > "), "<attempted> > <limit> per-tx", detail);
    Ok(())
}
async fn p01(e: Env) -> Result<()> {
    let (a, terms) = chain(&e, 2).await?;
    e.owner_call("pause", json!({})).await?.into_result()?;
    let d = e.spend(&a[2], &e.r(0), MILLI).await?;
    e.code("spend stopped", &d, "PAUSED");
    let d = e.try_delegate(&a[1], &terms[1].child()).await?;
    e.code("delegation stopped", &d, "PAUSED");
    e.eq("a pause is not the agent's fault: not counted", e.grant(a[2].grant).await?["denied"].as_u64(), Some(0));
    let (d, _) = e.revoke_by_owner(a[2].grant).await?;
    e.code("revoke still works while paused", &d, "REVOKED");
    e.owner_call("unpause", json!({})).await?.into_result()?;
    let d = e.spend(&a[1], &e.r(0), MILLI).await?;
    e.eq("back to normal", d.outcome.as_str(), "settled");
    Ok(())
}
async fn p02(e: Env) -> Result<()> {
    let (root, _) = root_with_list(&e).await?;
    e.owner_call("block_recipient", json!({"account": e.r(0)})).await?.into_result()?;
    let d = e.spend(&root, &e.r(0), MILLI).await?;
    e.code("blocked although allowlisted", &d, "RECIPIENT_BLOCKED");
    Ok(())
}

impl Env {
    pub async fn new_account(&self, prefix: &str, near_amount: u128) -> Result<Account> {
        let name = format!("{prefix}{}", rand::random::<u32>());
        let a = self
            .base
            .create_subaccount(&name)
            .initial_balance(near(near_amount))
            .transact()
            .await?
            .into_result()?;
        Ok(a)
    }

    pub async fn wasm(&self) -> Result<Vec<u8>> {
        Ok(std::fs::read(crate::wasm_path("espalier_vault"))?)
    }

    /// The shared registry if there is one, otherwise a fresh one for this scenario.
    pub async fn use_registry(&self) -> Result<AccountId> {
        if let Some(r) = &self.registry {
            return Ok(r.clone());
        }
        let a = self.new_account("registry", 5).await?;
        let c = a.deploy(&std::fs::read(crate::wasm_path("espalier_registry"))?).await?.into_result()?;
        c.call("new").transact().await?.into_result()?;
        Ok(a.id().clone())
    }

    pub async fn relay(&self) -> Result<Account> {
        let a = self.new_account("relay", 5).await?;
        a.deploy(&std::fs::read(crate::wasm_path("relay"))?).await?.into_result()?;
        Ok(a)
    }

    async fn deploy_token(&self, supply: u128) -> Result<Account> {
        let a = self.new_account("token", 8).await?;
        let c = a.deploy(&std::fs::read(crate::wasm_path("test_ft"))?).await?.into_result()?;
        c.call("new").args_json(json!({"owner": a.id(), "supply": supply.to_string()})).transact().await?.into_result()?;
        Ok(a)
    }

    pub async fn register_on_token(&self, ft: &Account, who: &Account) -> Result<()> {
        who.call(ft.id(), "storage_deposit")
            .args_json(json!({"account_id": who.id()}))
            .deposit(NearToken::from_millinear(10))
            .gas(Gas::from_tgas(30))
            .transact()
            .await?
            .into_result()?;
        Ok(())
    }

    async fn pay_vault(&self, ft: &Account, amount: u128) -> Result<()> {
        ft.call(ft.id(), "storage_deposit")
            .args_json(json!({"account_id": self.vault_id()}))
            .deposit(NearToken::from_millinear(10))
            .gas(Gas::from_tgas(30))
            .transact()
            .await?
            .into_result()?;
        ft.call(ft.id(), "ft_transfer_call")
            .args_json(json!({"receiver_id": self.vault_id(), "amount": amount.to_string(), "msg": ""}))
            .deposit(NearToken::from_yoctonear(1))
            .gas(Gas::from_tgas(100))
            .transact()
            .await?
            .into_result()?;
        Ok(())
    }

    /// Token the owner accepted, with `amount` deposited into the vault through ft_transfer_call.
    pub async fn funded_token(&self, amount: u128) -> Result<Account> {
        let ft = self.deploy_token(amount).await?;
        self.owner_call("add_token", json!({"token": ft.id()})).await?.into_result()?;
        self.pay_vault(&ft, amount).await?;
        Ok(ft)
    }

    pub async fn token_unaccepted(&self, amount: u128) -> Result<Account> {
        let ft = self.deploy_token(amount).await?;
        self.pay_vault(&ft, amount).await?;
        Ok(ft)
    }

    pub async fn token_balance(&self, ft: &Account, who: &AccountId) -> Result<u128> {
        let s: String = ft.view(ft.id(), "ft_balance_of").args_json(json!({"account_id": who})).await?.json()?;
        Ok(s.parse()?)
    }

    pub async fn vault_token_ledger(&self, ft: &Account) -> Result<u128> {
        let s: Option<String> = self.vault.view(self.vault_id(), "ft_balance").args_json(json!({"token": ft.id()})).await?.json()?;
        Ok(s.ok_or(anyhow!("no ledger"))?.parse()?)
    }
}
