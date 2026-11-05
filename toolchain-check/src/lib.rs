//! Toolchain proof only: one state write, one AddKey on the contract's own account.
use near_sdk::{env, near, Allowance, Promise, PublicKey};

#[near(contract_state)]
#[derive(Default)]
pub struct Hello {
    greeting: String,
}

#[near]
impl Hello {
    pub fn set(&mut self, greeting: String) {
        self.greeting = greeting;
    }

    pub fn get(&self) -> String {
        self.greeting.clone()
    }

    /// The design depends on this: can a contract add a function-call key to itself?
    pub fn issue_key(&mut self, pk: PublicKey, allowance: near_sdk::NearToken) -> Promise {
        Promise::new(env::current_account_id()).add_access_key_allowance(
            pk,
            Allowance::limited(allowance).expect("non-zero"),
            env::current_account_id(),
            "set".to_string(),
        )
    }

    pub fn drop_key(&mut self, pk: PublicKey) -> Promise {
        Promise::new(env::current_account_id()).delete_key(pk)
    }

    pub fn who(&self) -> (String, String, String) {
        (
            env::signer_account_id().to_string(),
            env::predecessor_account_id().to_string(),
            String::from(&env::signer_account_pk()),
        )
    }
}
