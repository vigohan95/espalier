//! Measures burnt gas on the real runtime against tree depth and subtree size.
use crate::env::*;
use crate::fresh_env;
use anyhow::Result;
use near_workspaces::types::Gas;
use near_workspaces::Account;
use serde_json::json;

fn tgas(r: &near_workspaces::result::ExecutionFinalResult) -> f64 {
    r.total_gas_burnt.as_gas() as f64 / 1e12
}

pub async fn run(root: &Account, report: Option<&str>) -> Result<bool> {
    let mut by_depth = vec![];
    let e = fresh_env(root, 200, 60).await?;
    let (agents, _) = crate::scenarios::chain(&e, 8).await?;
    for (depth, a) in agents.iter().enumerate() {
        let spend = e.spend_tx(a, &e.r(0), MILLI, None).await?;
        let denied = e.spend_tx(a, &e.r(0), 101 * MILLI, None).await?;
        let settle_tgas = spend.receipt_outcomes().iter().last().map(|o| o.gas_burnt.as_gas() as f64 / 1e12);
        let max_receipt = spend.receipt_outcomes().iter().map(|o| o.gas_burnt.as_gas()).max().unwrap_or(0) as f64 / 1e12;
        println!("depth {depth}: spend {:.2} Tgas total, largest receipt {max_receipt:.2}, denied {:.2}", tgas(&spend), tgas(&denied));
        by_depth.push(json!({
            "depth": depth,
            "spend_total_tgas": tgas(&spend),
            "spend_largest_receipt_tgas": max_receipt,
            "last_receipt_tgas": settle_tgas,
            "denied_total_tgas": tgas(&denied),
        }));
    }

    let mut by_size = vec![];
    for width in [1usize, 4, 12, 30] {
        let e = fresh_env(root, 200, 60).await?;
        let t = Tm::root(e.now_ms().await?);
        let top = e.root(&t).await?;
        let mut pt = t.child();
        pt.gas_budget = 7_000 * MILLI;
        pt.node_budget = 35;
        let parent = e.child(&top, &pt).await?;
        let mut leaf = pt.child();
        leaf.key_allowance = 50 * MILLI;
        leaf.gas_budget = 0;
        leaf.node_budget = 0;
        leaf.max_depth = 0;
        for _ in 0..width {
            e.child(&parent, &leaf).await?;
        }
        let keys = e.key_count().await?;
        let (d, r) = e.revoke_by_owner(parent.grant).await?;
        let backlog = e.info().await?["sweep_backlog"].as_u64().unwrap();
        let mut sweeps = vec![];
        while e.info().await?["sweep_backlog"].as_u64() != Some(0) {
            let s = e
                .owner
                .call(e.vault_id(), "sweep")
                .args_json(json!({"limit": 64}))
                .gas(Gas::from_tgas(300))
                .transact()
                .await?;
            sweeps.push(tgas(&s));
        }
        let removed = keys - e.key_count().await?;
        println!("subtree {}: revoke {:.2} Tgas, {}, sweeps {sweeps:?}, keys removed {removed}", width + 1, tgas(&r), d.detail);
        by_size.push(json!({
            "subtree_grants": width + 1,
            "revoke_total_tgas": tgas(&r),
            "revoke_detail": d.detail,
            "backlog_after_revoke": backlog,
            "sweep_tgas": sweeps,
            "keys_removed": removed,
        }));
    }
    let doc = json!({
        "network": "sandbox",
        "wasm_sha256": crate::report::wasm_sha256(),
        "git_commit": crate::report::git_commit(),
        "spend_by_depth": by_depth,
        "revoke_by_subtree_size": by_size,
    });
    if let Some(p) = report {
        std::fs::write(format!("{p}.json"), serde_json::to_string_pretty(&doc)?)?;
    }
    Ok(true)
}
