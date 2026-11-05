//! NEAR testnet run. Testnet only: the RPC URL is fixed here and the account must end in .testnet.
//! Key files stay under ~/.near-credentials, outside the repository.
//!   deploy: creates vault, owner and recipient subaccounts, deploys and initialises the contract
//!   run:    runs the testnet-tagged scenarios against that one vault, each under its own root grant
//!   lock:   deletes the vault's full-access key, after which the code can never change
use crate::env::*;
use crate::report::*;
use crate::scenarios::{self, Scenario};
use anyhow::{anyhow, Result};
use near_workspaces::network::Testnet;
use near_workspaces::types::{Gas, NearToken};
use near_workspaces::{Account, AccountId, Worker};
use serde_json::json;
use std::path::PathBuf;
use std::time::Instant;

const RPC: &str = "https://test.rpc.fastnear.com";
const WNEAR: &str = "wrap.testnet";

fn cred_dir() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap()).join(".near-credentials/testnet")
}

/// Reads near-cli and near-workspaces key files alike. The two differ in their field names.
fn load(path: &std::path::Path, id: &str, worker: &Worker<Testnet>) -> Result<Account> {
    let file: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(path)?)?;
    let secret = file["private_key"].as_str().or(file["secret_key"].as_str()).ok_or(anyhow!("no key in {path:?}"))?;
    Ok(Account::from_secret_key(id.parse()?, secret.parse()?, worker))
}

fn stored(id: &str, worker: &Worker<Testnet>) -> Result<Account> {
    load(&cred_dir().join(format!("{id}.json")), id, worker)
}

/// The first vault, `vault.<base>`, ran an earlier contract with a different state layout and stays
/// as it is. Each layout change gets a new account, because deployed state cannot be reinterpreted.
const VAULT: &str = "vault2";

fn names(base: &str) -> (String, String, Vec<String>) {
    (format!("{VAULT}.{base}"), format!("owner.{base}"), (0..3).map(|i| format!("r{i}.{base}")).collect())
}

async fn deploy(worker: &Worker<Testnet>, base: &Account) -> Result<()> {
    let wasm = std::fs::read(crate::wasm_path("espalier_vault"))?;
    let registry_wasm = std::fs::read(crate::wasm_path("espalier_registry"))?;
    let mut txs = vec![];
    let dir = cred_dir();
    let (_, owner_id, _) = names(base.id().as_str());
    let owner = stored(&owner_id, worker)?;
    let storage_near = wasm.len() as u128 / 100 + 200;
    let vault = base
        .create_subaccount(VAULT)
        .initial_balance(NearToken::from_millinear(storage_near as u128 + 1_000))
        .transact()
        .await?
        .into_result()?;
    vault.store_credentials(&dir).await?;
    let registry = base
        .create_subaccount("registry")
        .initial_balance(NearToken::from_millinear(registry_wasm.len() as u128 / 100 + 150))
        .transact()
        .await?
        .into_result()?;
    registry.store_credentials(&dir).await?;
    let d = registry.deploy(&registry_wasm).await?;
    txs.push(json!({"label": "deploy registry", "hash": d.details.outcome().transaction_hash.to_string()}));
    let c = d.into_result()?;
    let r = c.call("new").gas(Gas::from_tgas(30)).transact().await?;
    txs.push(json!({"label": "init registry", "hash": r.outcome().transaction_hash.to_string()}));
    r.into_result()?;
    let d = vault.deploy(&wasm).await?;
    txs.push(json!({"label": "deploy", "hash": d.details.outcome().transaction_hash.to_string()}));
    let c = d.into_result()?;
    let r = c
        .call("new")
        .args_json(json!({"owner": owner.id(), "gas_floor": "0", "approval_ttl_secs": 60}))
        .gas(Gas::from_tgas(30))
        .transact()
        .await?;
    txs.push(json!({"label": "init", "hash": r.outcome().transaction_hash.to_string()}));
    r.into_result()?;
    let doc = json!({
        "vault": vault.id(),
        "owner": owner.id(),
        "registry": registry.id(),
        "wasm_sha256": wasm_sha256(),
        "registry_wasm_sha256": sha256_of("espalier_registry"),
        "first_block": worker.view_block().await?.height(),
        "txs": txs,
    });
    println!("{}", serde_json::to_string_pretty(&doc)?);
    std::fs::write("docs/evidence/testnet-deploy.json", serde_json::to_string_pretty(&doc)?)?;
    let _ = worker;
    Ok(())
}

async fn wnear_scenario(e: Env) -> Result<()> {
    let token: AccountId = WNEAR.parse()?;
    let keep = |r: &near_workspaces::result::ExecutionFinalResult, label: &str, signer: &str| {
        e.trace.txs.lock().unwrap().push(record_from(label, signer, label, r));
    };
    let reg = |who: AccountId| {
        let owner = e.owner.clone();
        let token = token.clone();
        async move {
            owner
                .call(&token, "storage_deposit")
                .args_json(json!({"account_id": who}))
                .deposit(NearToken::from_millinear(2))
                .gas(Gas::from_tgas(30))
                .transact()
                .await
        }
    };
    let r = reg(e.owner.id().clone()).await?;
    keep(&r, "storage_deposit", "owner");
    let r = reg(e.vault_id().clone()).await?;
    keep(&r, "storage_deposit", "owner");
    let r = reg(e.r(0)).await?;
    keep(&r, "storage_deposit", "owner");
    let r = e.owner.call(&token, "near_deposit").deposit(NearToken::from_millinear(30)).gas(Gas::from_tgas(30)).transact().await?;
    keep(&r, "near_deposit", "owner");
    e.owner_call("add_token", json!({"token": token})).await?.into_result()?;
    let before: Option<String> = e.vault.view(e.vault_id(), "ft_balance").args_json(json!({"token": token})).await?.json()?;
    let before: u128 = before.unwrap_or_default().parse().unwrap_or(0);
    let r = e
        .owner
        .call(&token, "ft_transfer_call")
        .args_json(json!({"receiver_id": e.vault_id(), "amount": (20 * MILLI).to_string(), "msg": ""}))
        .deposit(NearToken::from_yoctonear(1))
        .gas(Gas::from_tgas(100))
        .transact()
        .await?;
    keep(&r, "ft_transfer_call", "owner");
    let ledger: Option<String> = e.vault.view(e.vault_id(), "ft_balance").args_json(json!({"token": token})).await?.json()?;
    e.eq("vault ledger credited through ft_on_transfer", ledger, Some((before + 20 * MILLI).to_string()));

    let mut t = Tm::root(e.now_ms().await?);
    t.asset = json!({"Ft": WNEAR});
    t.cap = 15 * MILLI;
    t.per_tx = 5 * MILLI;
    t.window_cap = 15 * MILLI;
    t.threshold = 5 * MILLI;
    let root = e.root(&t).await?;
    let child = e.child(&root, &t.child()).await?;
    let got: String = e.owner.view(&token, "ft_balance_of").args_json(json!({"account_id": e.r(0)})).await?.json()?;
    let got: u128 = got.parse()?;
    let d = e.spend(&child, &e.r(0), 5 * MILLI).await?;
    e.eq("child paid in wNEAR", d.outcome.as_str(), "settled");
    let now: String = e.owner.view(&token, "ft_balance_of").args_json(json!({"account_id": e.r(0)})).await?.json()?;
    e.eq("recipient's wNEAR grew by the exact amount", now.parse::<u128>()? - got, 5 * MILLI);
    // r1 never registered on wrap.testnet, so the real token panics and the vault must roll back.
    let d = e.spend(&child, &e.r(1), 5 * MILLI).await?;
    e.eq("unregistered recipient rolls back", d.outcome.as_str(), "rolled_back");
    e.eq("spent still one payment", e.field_u128(root.grant, "spent").await?, 5 * MILLI);
    e.eq("window released", e.field_u128(root.grant, "window_used").await?, 5 * MILLI);
    Ok(())
}

pub async fn run(account: &str, key_file: &str, report: Option<&str>) -> Result<bool> {
    if !account.ends_with(".testnet") {
        return Err(anyhow!("refusing to run on anything but a .testnet account"));
    }
    let worker = near_workspaces::testnet().rpc_addr(RPC).await?;
    let base = load(std::path::Path::new(key_file), account, &worker)?;
    let phase = std::env::args()
        .skip_while(|a| a != "--phase")
        .nth(1)
        .unwrap_or_else(|| "run".to_string());
    let (vault_id, owner_id, recipient_ids) = names(account);
    match phase.as_str() {
        "deploy" => {
            deploy(&worker, &base).await?;
            return Ok(true);
        }
        "redeploy" => {
            // Same account, same state layout: only the code changes. Needs the full-access key,
            // so this works until the lock phase has run.
            let vault = stored(&vault_id, &worker)?;
            let d = vault.deploy(&std::fs::read(crate::wasm_path("espalier_vault"))?).await?;
            let hash = d.details.outcome().transaction_hash.to_string();
            d.into_result()?;
            let path = "docs/evidence/testnet-deploy.json";
            let mut doc: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(path)?)?;
            doc["txs"].as_array_mut().unwrap().push(json!({"label": "redeploy", "hash": hash}));
            doc["wasm_sha256"] = json!(wasm_sha256());
            std::fs::write(path, serde_json::to_string_pretty(&doc)?)?;
            println!("{doc}");
            return Ok(true);
        }
        "lock" => {
            let vault = stored(&vault_id, &worker)?;
            let pk = vault.secret_key().public_key();
            let r = vault.batch(vault.id()).delete_key(pk).transact().await?;
            println!("locked, tx {}", r.outcome().transaction_hash);
            let keys = vault.view_access_keys().await?;
            let full = keys.iter().filter(|k| format!("{:?}", k.access_key.permission).contains("FullAccess")).count();
            let doc = json!({
                "lock_tx": r.outcome().transaction_hash.to_string(),
                "keys_on_vault": keys.len(),
                "full_access_keys": full,
            });
            println!("{doc}");
            std::fs::write("docs/evidence/testnet-lock.json", serde_json::to_string_pretty(&doc)?)?;
            return Ok(full == 0);
        }
        _ => {}
    }
    let env = Env {
        vault: stored(&vault_id, &worker)?,
        owner: stored(&owner_id, &worker)?,
        base,
        recipients: recipient_ids.iter().map(|r| stored(r, &worker)).collect::<Result<_>>()?,
        trace: Trace::default(),
        registry: Some(format!("registry.{account}").parse()?),
    };
    let only: Option<Vec<String>> = std::env::args()
        .skip_while(|a| a != "--only")
        .nth(1)
        .map(|s| s.split(',').map(str::to_string).collect());
    let mut list: Vec<Scenario> = scenarios::all().into_iter().filter(|s| s.testnet).collect();
    list.push(Scenario {
        id: "T01",
        group: "spend",
        title: "NEP-141 spend and rollback on the real wrap.testnet contract",
        testnet: true,
        run: |e| Box::pin(wnear_scenario(e)),
    });
    list.retain(|s| only.as_ref().is_none_or(|o| o.iter().any(|x| x == s.id)));
    let started = Instant::now();
    let mut results = vec![];
    for s in list {
        let mut e = env.clone();
        e.trace = Trace::default();
        let trace = e.trace.clone();
        let t0 = Instant::now();
        let out = (s.run)(e).await;
        let failed = trace.facts.lock().unwrap().iter().any(|f| !f.pass);
        let (status, error) = match out {
            Err(err) => ("error".to_string(), format!("{err:#}")),
            Ok(()) if failed => ("fail".to_string(), String::new()),
            Ok(()) => ("pass".to_string(), String::new()),
        };
        let r = ScenarioResult::new(&s, status, error, t0.elapsed().as_secs_f64(), &trace);
        println!("{} {:<5} {} ({:.1}s, {} facts, {} txs) {}", r.id, r.status, r.title, r.seconds, r.facts.len(), r.txs.len(), r.error);
        for f in r.facts.iter().filter(|f| !f.pass) {
            println!("    FAIL {}: expected {} observed {}", f.claim, f.expected, f.observed);
        }
        results.push(r);
    }
    let version = format!("{:?}", worker.status().await?.version);
    let ok = write_report(report, "testnet", &version, started.elapsed().as_secs_f64(), &results)?;
    if let Some(p) = report {
        let mut md = format!(
            "# Testnet transactions\n\nVault: [`{vault_id}`](https://testnet.nearblocks.io/address/{vault_id}). Generated by `harness testnet`. Check them without keys: `python3 scripts/verify-testnet.py`.\n\n"
        );
        for r in &results {
            md.push_str(&format!("## {} {} ({})\n\n| Step | Signer | Result | Tgas | Transaction |\n|---|---|---|---|---|\n", r.id, r.title, r.status));
            for t in &r.txs {
                let result = if t.code.is_empty() { t.outcome.clone() } else { format!("{} {}", t.outcome, t.code) };
                md.push_str(&format!(
                    "| {} | {} | {} | {:.2} | [{}](https://testnet.nearblocks.io/txns/{}) |\n",
                    t.label,
                    t.signer,
                    result,
                    t.tgas_burnt,
                    &t.hash[..10],
                    t.hash
                ));
            }
            md.push('\n');
        }
        std::fs::write(format!("{p}-txs.md"), md)?;
    }
    Ok(ok)
}
