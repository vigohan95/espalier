//! Runs the scenarios and the real-runtime property test, and writes the evidence reports.
//!   harness sandbox [--only A01,B02] [--group market] [--report docs/evidence/sandbox-report]
//!   harness sandbox-property --ops 2000 --seed 7 [--report docs/evidence/sandbox-property]
//!   harness market-property --waves 6 --seed 7 [--report docs/evidence/market-property]
//!   harness gas [--report docs/evidence/gas]
//!   harness testnet --account NAME --key-file PATH [--report docs/evidence/testnet-run]
mod env;
mod gas;
mod market;
mod market_property;
mod property;
mod recipient;
mod report;
mod scenarios;
mod testnet;

use anyhow::{anyhow, Result};
use env::*;
use futures::stream::{self, StreamExt};
use near_workspaces::network::Sandbox;
use near_workspaces::types::NearToken;
use near_workspaces::{Account, Worker};
use report::*;
use scenarios::Scenario;
use serde_json::json;
use std::time::{Duration, Instant};

pub fn wasm_path(name: &str) -> String {
    let dir = std::env::var("ESPALIER_WASM_DIR").unwrap_or_else(|_| "res".to_string());
    format!("{dir}/{name}.wasm")
}

fn arg(name: &str) -> Option<String> {
    let args: Vec<String> = std::env::args().collect();
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1).cloned())
}

/// A fresh funded namespace with its own owner, vault and recipients.
pub async fn fresh_env(root: &Account, vault_near: u128, ttl_secs: u32) -> Result<Env> {
    let tag = rand::random::<u32>();
    let base = root
        .create_subaccount(&format!("b{tag}"))
        .initial_balance(NearToken::from_near(vault_near + 30))
        .transact()
        .await?
        .into_result()?;
    let owner = base
        .create_subaccount("owner")
        .initial_balance(NearToken::from_near(10))
        .transact()
        .await?
        .into_result()?;
    let vault = base
        .create_subaccount("vault")
        .initial_balance(NearToken::from_near(vault_near))
        .transact()
        .await?
        .into_result()?;
    let contract = vault.deploy(&std::fs::read(wasm_path("espalier_vault"))?).await?.into_result()?;
    contract
        .call("new")
        .args_json(json!({"owner": owner.id(), "gas_floor": "0", "approval_ttl_secs": ttl_secs}))
        .transact()
        .await?
        .into_result()?;
    let mut recipients = vec![];
    for i in 0..3 {
        recipients.push(
            base.create_subaccount(&format!("r{i}"))
                .initial_balance(NearToken::from_near(1))
                .transact()
                .await?
                .into_result()?,
        );
    }
    Ok(Env { vault, owner, base, recipients, trace: Trace::default(), registry: None })
}

async fn run_one(root: Account, s: Scenario) -> ScenarioResult {
    let started = Instant::now();
    let setup = fresh_env(&root, 30, 20).await;
    let (status, error, trace) = match setup {
        Err(e) => ("error".to_string(), format!("setup: {e:#}"), Trace::default()),
        Ok(e) => {
            let trace = e.trace.clone();
            match tokio::time::timeout(Duration::from_secs(420), (s.run)(e)).await {
                Err(_) => ("error".to_string(), "timed out".to_string(), trace),
                Ok(Err(err)) => ("error".to_string(), format!("{err:#}"), trace),
                Ok(Ok(())) => {
                    let failed = trace.facts.lock().unwrap().iter().any(|f| !f.pass);
                    (if failed { "fail" } else { "pass" }.to_string(), String::new(), trace)
                }
            }
        }
    };
    let result = ScenarioResult::new(&s, status, error, started.elapsed().as_secs_f64(), &trace);
    println!("{} {:<5} {} ({:.1}s, {} facts){}", result.id, result.status, result.title, result.seconds, result.facts.len(),
        if result.error.is_empty() { String::new() } else { format!(" :: {}", result.error) });
    for f in result.facts.iter().filter(|f| !f.pass) {
        println!("    FAIL {}: expected {} observed {}", f.claim, f.expected, f.observed);
    }
    result
}

async fn sandbox_worker() -> Result<Worker<Sandbox>> {
    Ok(near_workspaces::sandbox().await?)
}

async fn cmd_sandbox() -> Result<bool> {
    let worker = sandbox_worker().await?;
    let root = worker.root_account()?;
    let only: Option<Vec<String>> = arg("--only").map(|s| s.split(',').map(str::to_string).collect());
    let group = arg("--group");
    let list: Vec<Scenario> = scenarios::all()
        .into_iter()
        .filter(|s| only.as_ref().is_none_or(|o| o.iter().any(|x| x == s.id)))
        .filter(|s| group.as_ref().is_none_or(|g| g == s.group))
        .collect();
    let started = Instant::now();
    let concurrency: usize = arg("--jobs").and_then(|j| j.parse().ok()).unwrap_or(4);
    let mut results: Vec<ScenarioResult> = stream::iter(list)
        .map(|s| run_one(root.clone(), s))
        .buffer_unordered(concurrency)
        .collect()
        .await;
    results.sort_by(|a, b| a.id.cmp(&b.id));
    let version = format!("{:?}", worker.status().await?.version);
    let path = arg("--report");
    let ok = write_report(path.as_deref(), "sandbox", &version, started.elapsed().as_secs_f64(), &results)?;
    Ok(ok)
}

#[tokio::main]
async fn main() -> Result<()> {
    let cmd = std::env::args().nth(1).ok_or(anyhow!("command?"))?;
    let ok = match cmd.as_str() {
        "sandbox" => cmd_sandbox().await?,
        "sandbox-property" => {
            let worker = sandbox_worker().await?;
            let ops = arg("--ops").and_then(|v| v.parse().ok()).unwrap_or(500);
            let seed = arg("--seed").and_then(|v| v.parse().ok()).unwrap_or(1);
            let keys = arg("--keys").and_then(|v| v.parse().ok()).unwrap_or(8);
            let screened = std::env::args().any(|a| a == "--screened");
            property::run(&worker.root_account()?, ops, seed, keys, screened, arg("--report").as_deref()).await?
        }
        "market-property" => {
            let worker = sandbox_worker().await?;
            let waves = arg("--waves").and_then(|v| v.parse().ok()).unwrap_or(4);
            let seed = arg("--seed").and_then(|v| v.parse().ok()).unwrap_or(1);
            market_property::run(&worker.root_account()?, waves, seed, arg("--report").as_deref()).await?
        }
        "gas" => {
            let worker = sandbox_worker().await?;
            gas::run(&worker.root_account()?, arg("--report").as_deref()).await?
        }
        "testnet" => {
            let account = arg("--account").ok_or(anyhow!("--account"))?;
            let key_file = arg("--key-file").ok_or(anyhow!("--key-file"))?;
            testnet::run(&account, &key_file, arg("--report").as_deref()).await?
        }
        other => return Err(anyhow!("unknown command {other}")),
    };
    if !ok {
        std::process::exit(1);
    }
    Ok(())
}
