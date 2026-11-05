//! Thin client for the vault as an outside caller sees it: JSON in, JSON out, real signed transactions.
use anyhow::{anyhow, Result};
use near_workspaces::result::ExecutionFinalResult;
use near_workspaces::types::{Gas, KeyType, NearToken, PublicKey, SecretKey};
use near_workspaces::{Account, AccountId};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};

pub const MILLI: u128 = 1_000_000_000_000_000_000_000;

pub const ALL_METHODS: [&str; 8] = [
    "request_spend",
    "execute_approved",
    "delegate",
    "revoke",
    "approve",
    "refill_key",
    "rotate_key",
    "sweep",
];

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct Decision {
    pub outcome: String,
    pub code: String,
    pub detail: String,
    pub grant: Option<u64>,
    pub blocking_grant: Option<u64>,
    pub limit: Option<String>,
    pub used: Option<String>,
    pub attempted: Option<String>,
    pub spend_id: Option<u64>,
    pub approval_id: Option<u64>,
    pub spend_hash: Option<String>,
    pub new_grant: Option<u64>,
}

#[derive(Clone, Debug, Serialize)]
pub struct TxRecord {
    pub label: String,
    pub hash: String,
    pub signer: String,
    pub method: String,
    pub tgas_burnt: f64,
    pub outcome: String,
    pub code: String,
    pub detail: String,
}

/// One assertion, with what was expected and what the chain returned.
#[derive(Clone, Debug, Serialize)]
pub struct Fact {
    pub claim: String,
    pub expected: String,
    pub observed: String,
    pub pass: bool,
}

#[derive(Clone, Default)]
pub struct Trace {
    pub facts: Arc<Mutex<Vec<Fact>>>,
    pub txs: Arc<Mutex<Vec<TxRecord>>>,
}

#[derive(Clone)]
pub struct Agent {
    pub acct: Account,
    pub pk: PublicKey,
    pub sk: SecretKey,
    pub grant: u64,
}

/// Terms as sent to the contract. Defaults are a generous root; scenarios change one thing.
/// The gas and node budgets are ceilings on what can be issued, not funds, so a large root costs
/// nothing and leaves room for an 8-level chain with three siblings at each level.
#[derive(Clone, Debug)]
pub struct Tm {
    pub asset: Value,
    pub cap: u128,
    pub per_tx: u128,
    pub window_cap: u128,
    pub window_secs: u32,
    pub threshold: u128,
    pub recipients: Option<Vec<String>>,
    pub methods: Vec<String>,
    pub expires_at_ms: u64,
    pub max_depth: u8,
    pub max_denials: u32,
    pub key_allowance: u128,
    pub gas_budget: u128,
    pub node_budget: u32,
}

impl Tm {
    pub fn root(now_ms: u64) -> Self {
        Tm {
            asset: json!("Native"),
            cap: 1000 * MILLI,
            per_tx: 100 * MILLI,
            window_cap: 600 * MILLI,
            window_secs: 3600,
            threshold: 100 * MILLI,
            recipients: None,
            methods: ALL_METHODS.iter().map(|m| m.to_string()).collect(),
            expires_at_ms: now_ms + 86_400_000,
            max_depth: 6,
            max_denials: 25,
            key_allowance: 200 * MILLI,
            gas_budget: 8_000_000 * MILLI,
            node_budget: 100_000,
        }
    }

    /// Valid child of `self`: same limits where the rules allow equality, a share of the budgets.
    pub fn child(&self) -> Self {
        let mut c = self.clone();
        c.max_depth = self.max_depth.saturating_sub(1);
        c.key_allowance = 200 * MILLI;
        c.gas_budget = self.gas_budget / 4;
        c.node_budget = self.node_budget / 4;
        c
    }

    pub fn methods(mut self, m: &[&str]) -> Self {
        self.methods = m.iter().map(|x| x.to_string()).collect();
        self
    }

    pub fn json(&self) -> Value {
        json!({
            "asset": self.asset,
            "cap": self.cap.to_string(),
            "per_tx": self.per_tx.to_string(),
            "window_cap": self.window_cap.to_string(),
            "window_secs": self.window_secs,
            "approval_threshold": self.threshold.to_string(),
            "recipients": self.recipients,
            "methods": self.methods,
            "expires_at_ms": self.expires_at_ms,
            "max_depth": self.max_depth,
            "max_denials": self.max_denials,
            "key_allowance": self.key_allowance.to_string(),
            "gas_budget": self.gas_budget.to_string(),
            "node_budget": self.node_budget,
        })
    }
}

#[derive(Clone)]
pub struct Env {
    pub vault: Account,
    pub owner: Account,
    pub base: Account,
    pub recipients: Vec<Account>,
    pub trace: Trace,
}

pub fn record_from(label: &str, signer: &str, method: &str, r: &ExecutionFinalResult) -> TxRecord {
    let d: Option<Decision> = r.clone().json().ok();
    TxRecord {
        label: label.to_string(),
        hash: r.outcome().transaction_hash.to_string(),
        signer: signer.to_string(),
        method: method.to_string(),
        tgas_burnt: r.total_gas_burnt.as_gas() as f64 / 1e12,
        outcome: d.as_ref().map(|d| d.outcome.clone()).unwrap_or_else(|| {
            if r.is_success() { "ok".into() } else { "failed".into() }
        }),
        code: d.as_ref().map(|d| d.code.clone()).unwrap_or_default(),
        detail: d.map(|d| d.detail).unwrap_or_default(),
    }
}

impl Env {
    pub fn vault_id(&self) -> &AccountId {
        self.vault.id()
    }

    pub fn r(&self, i: usize) -> AccountId {
        self.recipients[i].id().clone()
    }

    pub fn check(&self, claim: &str, pass: bool, expected: impl ToString, observed: impl ToString) {
        self.trace.facts.lock().unwrap().push(Fact {
            claim: claim.to_string(),
            expected: expected.to_string(),
            observed: observed.to_string(),
            pass,
        });
    }

    pub fn eq<A: PartialEq<B> + std::fmt::Debug, B: std::fmt::Debug>(&self, claim: &str, observed: A, expected: B) {
        let pass = observed == expected;
        self.check(claim, pass, format!("{expected:?}"), format!("{observed:?}"));
    }

    pub fn code(&self, claim: &str, d: &Decision, code: &str) {
        let pass = d.code == code;
        let observed = if d.detail.is_empty() { d.code.clone() } else { format!("{}: {}", d.code, d.detail) };
        self.check(claim, pass, code, observed);
    }

    fn keep(&self, label: &str, signer: &str, method: &str, r: &ExecutionFinalResult) {
        self.trace.txs.lock().unwrap().push(record_from(label, signer, method, r));
    }

    pub async fn info(&self) -> Result<Value> {
        Ok(self.vault.view(self.vault.id(), "vault_info").await?.json()?)
    }

    pub async fn now_ms(&self) -> Result<u64> {
        Ok(self.info().await?["now_ms"].as_u64().ok_or(anyhow!("now_ms"))?)
    }

    pub async fn grant(&self, id: u64) -> Result<Value> {
        Ok(self
            .vault
            .view(self.vault.id(), "get_grant")
            .args_json(json!({"grant_id": id}))
            .await?
            .json()?)
    }

    pub async fn field_u128(&self, id: u64, field: &str) -> Result<u128> {
        let g = self.grant(id).await?;
        Ok(g[field].as_str().ok_or(anyhow!("no field {field}"))?.parse()?)
    }

    pub async fn balance(&self, acct: &Account) -> Result<u128> {
        Ok(acct.view_account().await?.balance.as_yoctonear())
    }

    pub async fn key_count(&self) -> Result<usize> {
        Ok(self.vault.view_access_keys().await?.len())
    }

    pub async fn has_key(&self, pk: &PublicKey) -> Result<bool> {
        Ok(self.vault.view_access_key(pk).await.is_ok())
    }

    fn new_agent(&self, grant: u64, sk: SecretKey) -> Agent {
        let pk = sk.public_key();
        let acct = self.vault.clone_with_key(sk.clone());
        Agent { acct, pk, sk, grant }
    }

    pub async fn root(&self, t: &Tm) -> Result<Agent> {
        let sk = SecretKey::from_random(KeyType::ED25519);
        let r = self
            .owner
            .call(self.vault.id(), "create_root")
            .args_json(json!({"pk": sk.public_key(), "terms": t.json()}))
            .gas(Gas::from_tgas(100))
            .transact()
            .await?;
        self.keep("create_root", self.owner.id().as_str(), "create_root", &r);
        let d: Decision = r.json()?;
        let g = d.new_grant.ok_or(anyhow!("root refused: {d:?}"))?;
        Ok(self.new_agent(g, sk))
    }

    /// Raw delegation attempt: the Decision comes back whichever way it went.
    pub async fn delegate_raw(&self, parent: &Agent, t: &Tm, pk: &PublicKey) -> Result<Decision> {
        let r = parent
            .acct
            .call(self.vault.id(), "delegate")
            .args_json(json!({"child_pk": pk, "terms": t.json()}))
            .gas(Gas::from_tgas(100))
            .transact()
            .await?;
        self.keep("delegate", &format!("grant {}", parent.grant), "delegate", &r);
        Ok(r.json()?)
    }

    pub async fn child(&self, parent: &Agent, t: &Tm) -> Result<Agent> {
        let sk = SecretKey::from_random(KeyType::ED25519);
        let d = self.delegate_raw(parent, t, &sk.public_key()).await?;
        let g = d.new_grant.ok_or(anyhow!("delegate refused: {d:?}"))?;
        Ok(self.new_agent(g, sk))
    }

    pub async fn try_delegate(&self, parent: &Agent, t: &Tm) -> Result<Decision> {
        let sk = SecretKey::from_random(KeyType::ED25519);
        self.delegate_raw(parent, t, &sk.public_key()).await
    }

    pub async fn spend_tx(&self, a: &Agent, to: &AccountId, amount: u128, memo: Option<&str>) -> Result<ExecutionFinalResult> {
        self.spend_with_gas(a, to, amount, memo, 100).await
    }

    pub async fn spend_with_gas(&self, a: &Agent, to: &AccountId, amount: u128, memo: Option<&str>, tgas: u64) -> Result<ExecutionFinalResult> {
        Ok(a.acct
            .call(self.vault.id(), "request_spend")
            .args_json(json!({"recipient": to, "amount": amount.to_string(), "memo": memo}))
            .gas(Gas::from_tgas(tgas))
            .transact()
            .await?)
    }

    pub async fn spend(&self, a: &Agent, to: &AccountId, amount: u128) -> Result<Decision> {
        let r = self.spend_tx(a, to, amount, None).await?;
        self.keep("spend", &format!("grant {}", a.grant), "request_spend", &r);
        Ok(r.json()?)
    }

    pub async fn revoke_by_owner(&self, id: u64) -> Result<(Decision, ExecutionFinalResult)> {
        let r = self
            .owner
            .call(self.vault.id(), "revoke")
            .args_json(json!({"grant_id": id}))
            .gas(Gas::from_tgas(300))
            .transact()
            .await?;
        self.keep("revoke", self.owner.id().as_str(), "revoke", &r);
        Ok((r.clone().json()?, r))
    }

    pub async fn revoke_by(&self, a: &Agent, id: u64) -> Result<Decision> {
        let r = a
            .acct
            .call(self.vault.id(), "revoke")
            .args_json(json!({"grant_id": id}))
            .gas(Gas::from_tgas(100))
            .transact()
            .await?;
        self.keep("revoke", &format!("grant {}", a.grant), "revoke", &r);
        Ok(r.json()?)
    }

    pub async fn approve_as(&self, who: &Account, label: &str, id: u64, hash: &str) -> Result<Decision> {
        let r = who
            .call(self.vault.id(), "approve")
            .args_json(json!({"approval_id": id, "spend_hash": hash}))
            .gas(Gas::from_tgas(50))
            .transact()
            .await?;
        self.keep("approve", label, "approve", &r);
        Ok(r.json()?)
    }

    pub async fn approve_owner(&self, id: u64, hash: &str) -> Result<Decision> {
        self.approve_as(&self.owner.clone(), self.owner.id().as_str(), id, hash).await
    }

    pub async fn execute(&self, a: &Agent, id: u64, to: &AccountId, amount: u128) -> Result<Decision> {
        let r = a
            .acct
            .call(self.vault.id(), "execute_approved")
            .args_json(json!({"approval_id": id, "recipient": to, "amount": amount.to_string(), "memo": null}))
            .gas(Gas::from_tgas(100))
            .transact()
            .await?;
        self.keep("execute_approved", &format!("grant {}", a.grant), "execute_approved", &r);
        Ok(r.json()?)
    }

    pub async fn owner_call(&self, method: &str, args: Value) -> Result<ExecutionFinalResult> {
        let r = self
            .owner
            .call(self.vault.id(), method)
            .args_json(args)
            .gas(Gas::from_tgas(100))
            .transact()
            .await?;
        self.keep(method, self.owner.id().as_str(), method, &r);
        Ok(r)
    }

    pub async fn sleep_ms(&self, ms: u64) {
        tokio::time::sleep(std::time::Duration::from_millis(ms)).await;
    }

    /// Waits until chain time passes `target_ms`.
    pub async fn wait_chain(&self, target_ms: u64) -> Result<()> {
        for _ in 0..600 {
            if self.now_ms().await? >= target_ms {
                return Ok(());
            }
            self.sleep_ms(200).await;
        }
        Err(anyhow!("chain time never reached {target_ms}"))
    }
}

pub trait WithKey {
    fn clone_with_key(&self, sk: SecretKey) -> Account;
}

impl WithKey for Account {
    fn clone_with_key(&self, sk: SecretKey) -> Account {
        let mut a = self.clone();
        a.set_secret_key(sk);
        a
    }
}

pub fn near(n: u128) -> NearToken {
    NearToken::from_near(n)
}
