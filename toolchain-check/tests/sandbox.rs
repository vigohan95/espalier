//! Proves: wasm deploys on a real sandbox node, state call works, contract can AddKey on itself,
//! the issued key can sign, method restriction is enforced by the protocol, DeleteKey works.
use near_workspaces::types::{KeyType, NearToken, SecretKey};
use serde_json::json;

#[tokio::test]
async fn hello_and_key_lifecycle() -> anyhow::Result<()> {
    let wasm = std::fs::read(std::env::var("HELLO_WASM")?)?;
    let worker = near_workspaces::sandbox().await?;
    let contract = worker.dev_deploy(&wasm).await?;

    let r = contract.call("set").args_json(json!({"greeting":"hi"})).transact().await?;
    assert!(r.is_success(), "{r:#?}");
    let got: String = contract.view("get").await?.json()?;
    assert_eq!(got, "hi");

    let sk = SecretKey::from_random(KeyType::ED25519);
    let pk = sk.public_key();
    let r = contract
        .call("issue_key")
        .args_json(json!({"pk": pk, "allowance": NearToken::from_millinear(250)}))
        .max_gas()
        .transact()
        .await?;
    assert!(r.is_success(), "AddKey on self failed: {r:#?}");
    println!("issue_key gas burnt {}", r.total_gas_burnt);

    let keys = contract.view_access_keys().await?;
    println!("keys after AddKey: {}", keys.len());
    assert_eq!(keys.len(), 2);

    let agent = near_workspaces::Account::from_secret_key(contract.id().clone(), sk, &worker);
    let r = agent
        .call(contract.id(), "set")
        .args_json(json!({"greeting":"from-agent"}))
        .transact()
        .await?;
    assert!(r.is_success(), "fc key call failed: {r:#?}");

    let denied = agent.call(contract.id(), "drop_key").args_json(json!({"pk": pk})).transact().await;
    println!("wrong-method result: {:?}", denied.as_ref().map(|x| x.is_success()).map_err(|e| e.to_string()));
    assert!(denied.is_err() || denied.unwrap().is_failure());

    let r = contract.call("drop_key").args_json(json!({"pk": pk})).max_gas().transact().await?;
    assert!(r.is_success(), "DeleteKey failed: {r:#?}");
    assert_eq!(contract.view_access_keys().await?.len(), 1);
    Ok(())
}
