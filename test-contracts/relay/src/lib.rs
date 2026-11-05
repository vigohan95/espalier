//! Attacker tool for the scenarios: forwards any call, so the vault sees a contract as predecessor.
use near_sdk::{near, AccountId, Gas, NearToken, Promise};

#[near(contract_state)]
#[derive(Default)]
pub struct Relay {}

#[near]
impl Relay {
    pub fn forward(&mut self, target: AccountId, method: String, args: String) -> Promise {
        Promise::new(target).function_call(
            method,
            args.into_bytes(),
            NearToken::from_yoctonear(0),
            Gas::from_tgas(120),
        )
    }

    /// Stands in for a broken registry: an answer with the right method name and the wrong shape.
    pub fn check(&self) -> String {
        "definitely not listed".to_string()
    }
}
