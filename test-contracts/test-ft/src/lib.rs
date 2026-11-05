//! Real NEP-141 token (near-contract-standards) with two attacker switches for the scenarios:
//! `fail_transfers` makes ft_transfer panic, and `reenter` makes it call back into the vault in the
//! middle of a spend, which is what a malicious accepted token would do.
use near_contract_standards::fungible_token::core::FungibleTokenCore;
use near_contract_standards::fungible_token::metadata::{
    FungibleTokenMetadata, FungibleTokenMetadataProvider, FT_METADATA_SPEC,
};
use near_contract_standards::fungible_token::resolver::FungibleTokenResolver;
use near_contract_standards::fungible_token::FungibleToken;
use near_contract_standards::storage_management::{
    StorageBalance, StorageBalanceBounds, StorageManagement,
};
use near_sdk::json_types::U128;
use near_sdk::{
    env, near, AccountId, BorshStorageKey, Gas, NearToken, PanicOnDefault, Promise, PromiseOrValue,
};

#[derive(BorshStorageKey)]
#[near]
enum Key {
    Token,
}

#[near(contract_state)]
#[derive(PanicOnDefault)]
pub struct TestFt {
    token: FungibleToken,
    fail_transfers: bool,
    reenter: Option<(AccountId, String, String)>,
}

#[near]
impl TestFt {
    #[init]
    pub fn new(owner: AccountId, supply: U128) -> Self {
        let mut this = Self {
            token: FungibleToken::new(Key::Token),
            fail_transfers: false,
            reenter: None,
        };
        this.token.internal_register_account(&owner);
        this.token.internal_deposit(&owner, supply.0);
        this
    }

    pub fn set_fail_transfers(&mut self, fail: bool) {
        self.fail_transfers = fail;
    }

    /// On the next ft_transfer, calls `target.method(args)` before returning.
    pub fn set_reenter(&mut self, target: AccountId, method: String, args: String) {
        self.reenter = Some((target, method, args));
    }
}

#[near]
impl FungibleTokenCore for TestFt {
    #[payable]
    fn ft_transfer(&mut self, receiver_id: AccountId, amount: U128, memo: Option<String>) {
        if self.fail_transfers {
            env::panic_str("test-ft: transfer failure switched on");
        }
        self.token.ft_transfer(receiver_id, amount, memo);
        if let Some((target, method, args)) = self.reenter.take() {
            Promise::new(target)
                .function_call(method, args.into_bytes(), NearToken::from_yoctonear(0), Gas::from_tgas(8))
                .detach();
        }
    }

    #[payable]
    fn ft_transfer_call(
        &mut self,
        receiver_id: AccountId,
        amount: U128,
        memo: Option<String>,
        msg: String,
    ) -> PromiseOrValue<U128> {
        self.token.ft_transfer_call(receiver_id, amount, memo, msg)
    }

    fn ft_total_supply(&self) -> U128 {
        self.token.ft_total_supply()
    }

    fn ft_balance_of(&self, account_id: AccountId) -> U128 {
        self.token.ft_balance_of(account_id)
    }
}

#[near]
impl FungibleTokenResolver for TestFt {
    #[private]
    fn ft_resolve_transfer(&mut self, sender_id: AccountId, receiver_id: AccountId, amount: U128) -> U128 {
        self.token.ft_resolve_transfer(sender_id, receiver_id, amount)
    }
}

#[near]
impl StorageManagement for TestFt {
    #[payable]
    fn storage_deposit(&mut self, account_id: Option<AccountId>, registration_only: Option<bool>) -> StorageBalance {
        self.token.storage_deposit(account_id, registration_only)
    }

    #[payable]
    fn storage_withdraw(&mut self, amount: Option<NearToken>) -> StorageBalance {
        self.token.storage_withdraw(amount)
    }

    #[payable]
    fn storage_unregister(&mut self, force: Option<bool>) -> bool {
        self.token.internal_storage_unregister(force).is_some()
    }

    fn storage_balance_bounds(&self) -> StorageBalanceBounds {
        self.token.storage_balance_bounds()
    }

    fn storage_balance_of(&self, account_id: AccountId) -> Option<StorageBalance> {
        self.token.storage_balance_of(account_id)
    }
}

#[near]
impl FungibleTokenMetadataProvider for TestFt {
    fn ft_metadata(&self) -> FungibleTokenMetadata {
        FungibleTokenMetadata {
            spec: FT_METADATA_SPEC.to_string(),
            name: "Espalier Test Token".to_string(),
            symbol: "EST".to_string(),
            icon: None,
            reference: None,
            reference_hash: None,
            decimals: 6,
        }
    }
}
