//! Espalier: an on-chain delegation tree for agent spending.
//!
//! The vault account holds the funds and carries one function-call key per grant. Agents sign with
//! those keys, so inside the contract `predecessor == signer == vault` and the key itself is the
//! identity. See docs/DESIGN.md for the invariants and the gas analysis.

use near_sdk::json_types::U128;
use near_sdk::serde_json::json;
use near_sdk::store::{LookupMap, LookupSet};
use near_sdk::{
    env, near, require, AccountId, Allowance, BorshStorageKey, Gas, NearToken, PanicOnDefault,
    Promise, PromiseError, PromiseOrValue, PublicKey,
};
use std::num::NonZeroU128;

mod events;
pub mod types;
pub mod window;

#[cfg(test)]
mod property;

use types::*;
use window::Window;

const GAS_SETTLE: Gas = Gas::from_tgas(20);
const GAS_FT_TRANSFER: Gas = Gas::from_tgas(15);
const GAS_KEY_CALLBACK: Gas = Gas::from_tgas(10);
/// A spend needs its own execution, the FT transfer and the callback. Below this the call would
/// die on the static gas attachment without ever reaching the denial counter.
const GAS_MIN_SPEND: Gas = Gas::from_tgas(50);
const GAS_MIN_DELEGATE: Gas = Gas::from_tgas(20);
/// Headroom for what this call itself writes, on top of the storage already in use.
const STORAGE_MARGIN_BYTES: u64 = 4096;

#[derive(BorshStorageKey)]
#[near]
enum Key {
    Grants,
    Allowlists,
    Children,
    Keys,
    Pending,
    Approvals,
    Tokens,
    Blocked,
    Sweep,
}

#[near(contract_state)]
#[derive(PanicOnDefault)]
pub struct Vault {
    owner: AccountId,
    paused: bool,
    gas_floor: u128,
    approval_ttl_ms: u64,
    next_grant: GrantId,
    next_nonce: u64,
    in_flight: u64,
    grants: LookupMap<GrantId, Grant>,
    allowlists: LookupMap<GrantId, Vec<AccountId>>,
    children: LookupMap<GrantId, Vec<GrantId>>,
    keys: LookupMap<PublicKey, GrantId>,
    pending: LookupMap<u64, Pending>,
    approvals: LookupMap<u64, Approval>,
    /// Accepted tokens and the vault's unreserved balance of each.
    tokens: LookupMap<AccountId, u128>,
    blocked: LookupSet<AccountId>,
    sweep_queue: LookupMap<u64, GrantId>,
    sweep_head: u64,
    sweep_tail: u64,
}

/// Why something was refused. `counted` is false for refusals that are not the agent's doing.
#[derive(Debug)]
struct Denial {
    code: &'static str,
    detail: String,
    blocking: Option<GrantId>,
    limit: Option<u128>,
    used: Option<u128>,
    attempted: Option<u128>,
    counted: bool,
}

impl Denial {
    fn new(code: &'static str, detail: impl Into<String>) -> Self {
        Denial {
            code,
            detail: detail.into(),
            blocking: None,
            limit: None,
            used: None,
            attempted: None,
            counted: true,
        }
    }
    fn at(mut self, grant: GrantId) -> Self {
        self.blocking = Some(grant);
        self
    }
    fn numbers(mut self, limit: u128, used: u128, attempted: u128) -> Self {
        self.limit = Some(limit);
        self.used = Some(used);
        self.attempted = Some(attempted);
        self
    }
    fn uncounted(mut self) -> Self {
        self.counted = false;
        self
    }
}

enum Caller {
    Owner,
    Grant(GrantId),
}

impl Caller {
    fn grant(&self) -> Option<GrantId> {
        match self {
            Caller::Owner => None,
            Caller::Grant(g) => Some(*g),
        }
    }
}

type Refusal = (Option<GrantId>, Denial);

fn now_ms() -> u64 {
    env::block_timestamp_ms()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// A result too long to read still means the call succeeded, so only an explicit failure counts.
fn promise_succeeded() -> bool {
    !matches!(env::promise_result_checked(0, 64), Err(PromiseError::Failed))
}

fn allowance(amount: u128) -> Allowance {
    Allowance::Limited(NonZeroU128::new(amount).expect("allowance checked non-zero"))
}

#[near]
impl Vault {
    #[init]
    pub fn new(owner: AccountId, gas_floor: U128, approval_ttl_secs: u32) -> Self {
        // An owner equal to the vault would turn every agent key into the owner, because agent
        // calls arrive with predecessor == vault.
        require!(owner != env::current_account_id(), "owner must not be the vault account");
        Self {
            owner,
            paused: false,
            gas_floor: gas_floor.0,
            approval_ttl_ms: approval_ttl_secs as u64 * 1000,
            next_grant: 1,
            next_nonce: 1,
            in_flight: 0,
            grants: LookupMap::new(Key::Grants),
            allowlists: LookupMap::new(Key::Allowlists),
            children: LookupMap::new(Key::Children),
            keys: LookupMap::new(Key::Keys),
            pending: LookupMap::new(Key::Pending),
            approvals: LookupMap::new(Key::Approvals),
            tokens: LookupMap::new(Key::Tokens),
            blocked: LookupSet::new(Key::Blocked),
            sweep_queue: LookupMap::new(Key::Sweep),
            sweep_head: 0,
            sweep_tail: 0,
        }
    }

    // ---- Owner ------------------------------------------------------------------------------

    pub fn create_root(&mut self, pk: PublicKey, terms: Terms) -> PromiseOrValue<Decision> {
        self.assert_owner();
        let now = now_ms();
        let checked = self
            .check_terms(&terms, now)
            .and_then(|_| self.check_asset_accepted(&terms.asset))
            .and_then(|_| self.check_key_free(&pk))
            .and_then(|_| self.check_storage_room());
        if let Err(d) = checked {
            return PromiseOrValue::Value(self.deny(None, d));
        }
        PromiseOrValue::Promise(self.create_grant(None, pk, terms))
    }

    pub fn pause(&mut self) {
        self.assert_owner();
        self.paused = true;
        events::emit("paused", json!({"by": self.owner}));
    }

    pub fn unpause(&mut self) {
        self.assert_owner();
        self.paused = false;
        events::emit("unpaused", json!({"by": self.owner}));
    }

    pub fn add_token(&mut self, token: AccountId) {
        self.assert_owner();
        if !self.tokens.contains_key(&token) {
            self.tokens.insert(token.clone(), 0);
        }
        events::emit("token_added", json!({"token": token}));
    }

    pub fn block_recipient(&mut self, account: AccountId) {
        self.assert_owner();
        self.blocked.insert(account.clone());
        events::emit("recipient_blocked", json!({"account": account}));
    }

    pub fn unblock_recipient(&mut self, account: AccountId) {
        self.assert_owner();
        self.blocked.remove(&account);
        events::emit("recipient_unblocked", json!({"account": account}));
    }

    /// Takes out funds that no grant has in flight. Same reserve and rollback path as a spend.
    pub fn withdraw(&mut self, asset: Asset, amount: U128, to: AccountId) -> PromiseOrValue<Decision> {
        self.assert_owner();
        require!(amount.0 > 0, "amount must be positive");
        if let Err(d) = self.check_vault_funds(&asset, amount.0) {
            return PromiseOrValue::Value(self.deny(None, d));
        }
        let spend_id = self.take_nonce();
        if let Asset::Ft(token) = &asset {
            let free = self.tokens.get_mut(token).expect("funds checked");
            *free -= amount.0;
        }
        self.pending.insert(
            spend_id,
            Pending {
                grant: None,
                asset: asset.clone(),
                amount: amount.0,
                recipient: to.clone(),
                at_ms: now_ms(),
            },
        );
        self.in_flight += 1;
        events::emit(
            "withdrawal_reserved",
            json!({"spend_id": spend_id, "asset": asset, "amount": amount, "to": to}),
        );
        PromiseOrValue::Promise(Self::dispatch(asset, to, amount.0, spend_id))
    }

    // ---- Agent: spending --------------------------------------------------------------------

    pub fn request_spend(
        &mut self,
        recipient: AccountId,
        amount: U128,
        memo: Option<String>,
    ) -> PromiseOrValue<Decision> {
        let gid = match self.agent("request_spend") {
            Ok(g) => g,
            Err((g, d)) => return PromiseOrValue::Value(self.deny(g, d)),
        };
        let now = now_ms();
        let checked = Self::check_gas(GAS_MIN_SPEND)
            .and_then(|_| Self::check_memo(&memo))
            .and_then(|_| self.evaluate_spend(gid, &recipient, amount.0, now));
        if let Err(d) = checked {
            return PromiseOrValue::Value(self.deny(Some(gid), d));
        }
        if amount.0 > self.grant(gid).threshold {
            return PromiseOrValue::Value(self.enqueue(gid, recipient, amount.0, memo, now));
        }
        PromiseOrValue::Promise(self.commit_spend(gid, recipient, amount.0, memo, now))
    }

    /// Runs an approved spend. The arguments are hashed again, so the only spend this can execute
    /// is the exact one the approver saw, and every limit is evaluated again at this moment.
    pub fn execute_approved(
        &mut self,
        approval_id: u64,
        recipient: AccountId,
        amount: U128,
        memo: Option<String>,
    ) -> PromiseOrValue<Decision> {
        let caller = match self.caller("execute_approved") {
            Ok(c) => c,
            Err((g, d)) => return PromiseOrValue::Value(self.deny(g, d)),
        };
        let now = now_ms();
        let approval = match self.check_execute(&caller, approval_id, &recipient, amount.0, &memo, now) {
            Ok(a) => a,
            Err(d) => return PromiseOrValue::Value(self.deny(caller.grant(), d)),
        };
        self.approvals.remove(&approval_id);
        self.grants.get_mut(&approval.grant).expect("grant").queued.retain(|id| *id != approval_id);
        events::emit(
            "approval_executed",
            json!({"approval_id": approval_id, "grant": approval.grant, "spend_hash": hex(&approval.hash)}),
        );
        PromiseOrValue::Promise(self.commit_spend(approval.grant, recipient, amount.0, memo, now))
    }

    /// Owner approves anything. An ancestor approves what it could have spent without approval
    /// itself, so approval authority climbs the tree instead of always landing on the human.
    pub fn approve(&mut self, approval_id: u64, spend_hash: String) -> Decision {
        let caller = match self.caller("approve") {
            Ok(c) => c,
            Err((g, d)) => return self.deny(g, d),
        };
        let now = now_ms();
        if let Err(d) = self.check_approve(&caller, approval_id, &spend_hash, now) {
            return self.deny(caller.grant(), d);
        }
        let approver = match caller {
            Caller::Owner => Approver::Owner,
            Caller::Grant(g) => Approver::Grant(g),
        };
        let approval = self.approvals.get_mut(&approval_id).expect("checked");
        approval.approved_by = Some(approver.clone());
        let grant = approval.grant;
        events::emit(
            "approval_granted",
            json!({"approval_id": approval_id, "grant": grant, "by": approver, "spend_hash": spend_hash}),
        );
        Decision {
            outcome: "allowed".into(),
            code: "APPROVED".into(),
            grant: Some(grant),
            approval_id: Some(approval_id),
            spend_hash: Some(spend_hash),
            ..Default::default()
        }
    }

    // ---- Agent: the tree --------------------------------------------------------------------

    /// Creates a child under the signing key's grant. `child_pk` comes from the child: the parent
    /// never holds the child's secret key. Terms must be narrower in every dimension.
    pub fn delegate(&mut self, child_pk: PublicKey, terms: Terms) -> PromiseOrValue<Decision> {
        let gid = match self.agent("delegate") {
            Ok(g) => g,
            Err((g, d)) => return PromiseOrValue::Value(self.deny(g, d)),
        };
        let now = now_ms();
        let checked = Self::check_gas(GAS_MIN_DELEGATE)
            .and_then(|_| self.check_not_paused())
            .and_then(|_| self.check_live(gid, now))
            .and_then(|_| self.check_terms(&terms, now))
            .and_then(|_| self.check_attenuation(gid, &terms, now))
            .and_then(|_| self.check_key_free(&child_pk))
            .and_then(|_| self.check_storage_room());
        if let Err(d) = checked {
            return PromiseOrValue::Value(self.deny(Some(gid), d));
        }
        PromiseOrValue::Promise(self.create_grant(Some(gid), child_pk, terms))
    }

    /// Revokes a grant and with it every descendant, in one write. Descendants fail their next
    /// ancestry walk, so the gas of this call does not depend on the size of the subtree. Up to
    /// EAGER_SWEEP keys are deleted here as well; `sweep` or a dead key's own attempt reaps the rest.
    pub fn revoke(&mut self, grant_id: GrantId) -> Decision {
        let caller = match self.caller("revoke") {
            Ok(c) => c,
            Err((g, d)) => return self.deny(g, d),
        };
        if let Err(d) = self.check_revoke(&caller, grant_id) {
            return self.deny(caller.grant(), d);
        }
        self.grants.get_mut(&grant_id).expect("checked").revoked = true;
        let by = match caller {
            Caller::Owner => json!("owner"),
            Caller::Grant(g) => json!(g),
        };
        events::emit("grant_revoked", json!({"grant": grant_id, "by": by, "cascade": true}));
        self.enqueue_sweep(grant_id);
        let visited = self.run_sweep(EAGER_SWEEP);
        Decision {
            outcome: "allowed".into(),
            code: "REVOKED".into(),
            detail: format!("visited {visited}, backlog {}", self.sweep_tail - self.sweep_head),
            grant: Some(grant_id),
            ..Default::default()
        }
    }

    /// Continues key cleanup after a revoke. Anyone may call it: it only touches keys of grants
    /// that are already revoked, and each call does bounded work.
    pub fn sweep(&mut self, limit: u32) -> Decision {
        let visited = self.run_sweep(limit.min(MAX_SWEEP));
        Decision {
            outcome: "allowed".into(),
            code: "SWEPT".into(),
            detail: format!("visited {visited}, backlog {}", self.sweep_tail - self.sweep_head),
            ..Default::default()
        }
    }

    /// Gives a key a fresh allowance. The protocol cannot raise an allowance in place, so this
    /// deletes and re-adds the key in one receipt. This is the way out of the lockout that a key
    /// with an exhausted allowance otherwise ends in. Only an ancestor or the owner can do it.
    pub fn refill_key(&mut self, grant_id: GrantId, allowance_yocto: U128) -> PromiseOrValue<Decision> {
        let caller = match self.caller("refill_key") {
            Ok(c) => c,
            Err((g, d)) => return PromiseOrValue::Value(self.deny(g, d)),
        };
        let checked = self.check_key_op(&caller, grant_id, false, allowance_yocto.0);
        if let Err(d) = checked {
            return PromiseOrValue::Value(self.deny(caller.grant(), d));
        }
        if let Caller::Grant(c) = caller {
            self.grants.get_mut(&c).expect("caller").gas_left -= allowance_yocto.0;
        }
        let g = self.grant(grant_id).clone();
        events::emit(
            "key_refilled",
            json!({"grant": grant_id, "pk": g.pk, "allowance": allowance_yocto}),
        );
        let me = env::current_account_id();
        PromiseOrValue::Promise(
            Promise::new(me.clone())
                .delete_key(g.pk.clone())
                .add_access_key_allowance(
                    g.pk,
                    allowance(allowance_yocto.0),
                    me.clone(),
                    method_names(g.methods).join(","),
                )
                .then(
                    Self::ext(me)
                        .with_static_gas(GAS_KEY_CALLBACK)
                        .on_key_changed(grant_id, None, caller.grant(), allowance_yocto),
                ),
        )
    }

    /// Replaces a key: DeleteKey and AddKey in one receipt, so there is never a gap with two live
    /// keys. The grant itself, an ancestor or the owner can do it.
    pub fn rotate_key(&mut self, grant_id: GrantId, new_pk: PublicKey) -> PromiseOrValue<Decision> {
        let caller = match self.caller("rotate_key") {
            Ok(c) => c,
            Err((g, d)) => return PromiseOrValue::Value(self.deny(g, d)),
        };
        let cost = self.grants.get(&grant_id).map_or(0, |g| g.key_allowance);
        let checked = self
            .check_key_op(&caller, grant_id, true, cost)
            .and_then(|_| self.check_key_free(&new_pk));
        if let Err(d) = checked {
            return PromiseOrValue::Value(self.deny(caller.grant(), d));
        }
        if let Caller::Grant(c) = caller {
            self.grants.get_mut(&c).expect("caller").gas_left -= cost;
        }
        let g = self.grants.get_mut(&grant_id).expect("checked");
        let old_pk = std::mem::replace(&mut g.pk, new_pk.clone());
        let methods = g.methods;
        self.keys.remove(&old_pk);
        self.keys.insert(new_pk.clone(), grant_id);
        events::emit("key_rotated", json!({"grant": grant_id, "old_pk": old_pk, "new_pk": new_pk}));
        let me = env::current_account_id();
        PromiseOrValue::Promise(
            Promise::new(me.clone())
                .delete_key(old_pk.clone())
                .add_access_key_allowance(new_pk, allowance(cost), me.clone(), method_names(methods).join(","))
                .then(
                    Self::ext(me)
                        .with_static_gas(GAS_KEY_CALLBACK)
                        .on_key_changed(grant_id, Some(old_pk), caller.grant(), U128(cost)),
                ),
        )
    }

    // ---- Callbacks --------------------------------------------------------------------------
    // #[private] alone is not enough here: an agent key on this account also arrives with
    // predecessor == current_account. What keeps agents out is that no FC key the vault issues
    // ever lists these names, and `method_names` can only produce names from METHODS.

    #[private]
    pub fn on_spend_settled(&mut self, spend_id: u64) -> Decision {
        let ok = promise_succeeded();
        let p = self.pending.remove(&spend_id).expect("pending spend");
        self.in_flight -= 1;
        if let Some(gid) = p.grant {
            let leaf = self.grants.get_mut(&gid).expect("grant");
            leaf.direct_reserved -= p.amount;
            if ok {
                leaf.direct_spent += p.amount;
            }
            let mut cur = Some(gid);
            while let Some(id) = cur {
                let g = self.grants.get_mut(&id).expect("ancestor");
                g.reserved -= p.amount;
                if ok {
                    g.spent += p.amount;
                } else {
                    g.window.sub(p.at_ms, g.window_ms, p.amount);
                }
                cur = g.parent;
            }
        }
        if !ok {
            if let Asset::Ft(token) = &p.asset {
                *self.tokens.get_mut(token).expect("token") += p.amount;
            }
        }
        let (event, outcome, code) = if ok {
            ("spend_settled", "settled", "SETTLED")
        } else {
            ("spend_rolled_back", "rolled_back", "TRANSFER_FAILED")
        };
        events::emit(
            event,
            json!({"spend_id": spend_id, "grant": p.grant, "asset": p.asset, "amount": U128(p.amount), "recipient": p.recipient}),
        );
        Decision {
            outcome: outcome.into(),
            code: code.into(),
            grant: p.grant,
            attempted: Some(U128(p.amount)),
            spend_id: Some(spend_id),
            ..Default::default()
        }
    }

    #[private]
    pub fn on_key_added(&mut self, grant_id: GrantId) -> Decision {
        let g = self.grant(grant_id).clone();
        if promise_succeeded() {
            events::emit("key_added", json!({"grant": grant_id, "pk": g.pk}));
            return Decision {
                outcome: "allowed".into(),
                code: "GRANTED".into(),
                grant: g.parent,
                new_grant: Some(grant_id),
                ..Default::default()
            };
        }
        // AddKey failed, most likely because the key already exists on this account outside the
        // vault's map. The grant never had a working key: bury it and return what it was charged.
        if self.keys.get(&g.pk) == Some(&grant_id) {
            self.keys.remove(&g.pk);
        }
        let stored = self.grants.get_mut(&grant_id).expect("grant");
        stored.key_live = false;
        stored.revoked = true;
        let refund_gas = stored.gas_left + stored.key_allowance;
        let refund_nodes = stored.nodes_left + 1;
        if let Some(parent) = g.parent {
            let p = self.grants.get_mut(&parent).expect("parent");
            p.gas_left += refund_gas;
            p.nodes_left += refund_nodes;
        }
        events::emit("key_add_failed", json!({"grant": grant_id, "pk": g.pk}));
        Decision {
            outcome: "denied".into(),
            code: "KEY_ADD_FAILED".into(),
            grant: g.parent,
            blocking_grant: Some(grant_id),
            ..Default::default()
        }
    }

    /// Settles a refill or rotation. On failure nothing changed on chain, because the DeleteKey and
    /// AddKey shared one receipt, so the vault's records and the charge go back as well.
    #[private]
    pub fn on_key_changed(
        &mut self,
        grant_id: GrantId,
        old_pk: Option<PublicKey>,
        charged: Option<GrantId>,
        cost: U128,
    ) -> Decision {
        if promise_succeeded() {
            return Decision {
                outcome: "allowed".into(),
                code: "KEY_CHANGED".into(),
                grant: Some(grant_id),
                ..Default::default()
            };
        }
        if let Some(old) = old_pk {
            let g = self.grants.get_mut(&grant_id).expect("grant");
            let new = std::mem::replace(&mut g.pk, old.clone());
            self.keys.remove(&new);
            self.keys.insert(old, grant_id);
        }
        if let Some(c) = charged {
            self.grants.get_mut(&c).expect("charged").gas_left += cost.0;
        }
        events::emit("key_change_failed", json!({"grant": grant_id}));
        Decision {
            outcome: "denied".into(),
            code: "KEY_CHANGE_FAILED".into(),
            grant: Some(grant_id),
            ..Default::default()
        }
    }

    /// NEP-141 receiver. Tokens the owner did not accept are returned in full.
    pub fn ft_on_transfer(
        &mut self,
        sender_id: AccountId,
        amount: U128,
        msg: String,
    ) -> PromiseOrValue<U128> {
        let _ = msg;
        let token = env::predecessor_account_id();
        match self.tokens.get_mut(&token) {
            Some(free) => {
                *free += amount.0;
                events::emit(
                    "ft_deposit",
                    json!({"token": token, "from": sender_id, "amount": amount}),
                );
                PromiseOrValue::Value(U128(0))
            }
            None => PromiseOrValue::Value(amount),
        }
    }

    // ---- Views ------------------------------------------------------------------------------

    pub fn get_grant(&self, grant_id: GrantId) -> Option<GrantView> {
        let g = self.grants.get(&grant_id)?;
        let now = now_ms();
        Some(GrantView {
            id: g.id,
            parent: g.parent,
            depth: g.depth,
            asset: g.asset.clone(),
            cap: U128(g.cap),
            per_tx: U128(g.per_tx),
            window_cap: U128(g.window_cap),
            window_secs: (g.window_ms / 1000) as u32,
            window_used: U128(g.window.used(now, g.window_ms)),
            approval_threshold: U128(g.threshold),
            recipients: self.allowlists.get(&grant_id).cloned(),
            methods: method_names(g.methods),
            expires_at_ms: g.expires_at_ms,
            max_depth: g.max_depth,
            max_denials: g.max_denials,
            key_allowance: U128(g.key_allowance),
            pk: g.pk.clone(),
            key_live: g.key_live,
            spent: U128(g.spent),
            reserved: U128(g.reserved),
            direct_spent: U128(g.direct_spent),
            direct_reserved: U128(g.direct_reserved),
            gas_left: U128(g.gas_left),
            nodes_left: g.nodes_left,
            denied: g.denied,
            revoked: g.revoked,
            live: self.check_live(grant_id, now).is_ok(),
            spendable: U128(self.spendable(grant_id, now)),
            children: self.children.get(&grant_id).cloned().unwrap_or_default(),
        })
    }

    /// The grant followed by each ancestor up to the root.
    pub fn get_lineage(&self, grant_id: GrantId) -> Vec<GrantId> {
        let mut out = vec![];
        let mut cur = self.grants.get(&grant_id).map(|g| g.id);
        while let Some(id) = cur {
            out.push(id);
            cur = self.grant(id).parent;
        }
        out
    }

    pub fn grant_of_key(&self, pk: PublicKey) -> Option<GrantId> {
        self.keys.get(&pk).copied()
    }

    pub fn get_approval(&self, approval_id: u64) -> Option<ApprovalView> {
        self.approvals.get(&approval_id).map(|a| ApprovalView {
            id: approval_id,
            grant: a.grant,
            recipient: a.recipient.clone(),
            amount: U128(a.amount),
            memo: a.memo.clone(),
            spend_hash: hex(&a.hash),
            expires_at_ms: a.expires_at_ms,
            approved_by: a.approved_by.clone(),
        })
    }

    pub fn is_pending(&self, spend_id: u64) -> bool {
        self.pending.contains_key(&spend_id)
    }

    pub fn ft_balance(&self, token: AccountId) -> Option<U128> {
        self.tokens.get(&token).map(|b| U128(*b))
    }

    pub fn vault_info(&self) -> VaultInfo {
        VaultInfo {
            owner: self.owner.clone(),
            paused: self.paused,
            gas_floor: U128(self.gas_floor),
            approval_ttl_ms: self.approval_ttl_ms,
            grants: self.next_grant - 1,
            in_flight: self.in_flight,
            sweep_backlog: self.sweep_tail - self.sweep_head,
            available_native: U128(self.available_native()),
            now_ms: now_ms(),
        }
    }

    /// What `request_spend` would decide right now, without touching state. A wrapper can call
    /// this for free before spending any of its key's allowance.
    pub fn preview_spend(&self, grant_id: GrantId, recipient: AccountId, amount: U128) -> Decision {
        let now = now_ms();
        if !self.grants.contains_key(&grant_id) {
            return Decision { outcome: "denied".into(), code: "UNKNOWN_GRANT".into(), ..Default::default() };
        }
        match self.evaluate_spend(grant_id, &recipient, amount.0, now) {
            Err(d) => Self::denied_decision(Some(grant_id), &d),
            Ok(()) if amount.0 > self.grant(grant_id).threshold => Decision {
                outcome: "queued".into(),
                code: "NEEDS_APPROVAL".into(),
                grant: Some(grant_id),
                ..Default::default()
            },
            Ok(()) => Decision {
                outcome: "allowed".into(),
                code: "WITHIN_POLICY".into(),
                grant: Some(grant_id),
                ..Default::default()
            },
        }
    }
}

// ---- Internals ------------------------------------------------------------------------------

impl Vault {
    fn assert_owner(&self) {
        require!(env::predecessor_account_id() == self.owner, "owner only");
    }

    fn grant(&self, id: GrantId) -> &Grant {
        self.grants.get(&id).expect("unknown grant")
    }

    fn take_nonce(&mut self) -> u64 {
        let n = self.next_nonce;
        self.next_nonce += 1;
        n
    }

    /// Resolves the signing key to a grant. Requires both signer and predecessor to be the vault:
    /// the signer alone would accept a contract calling back in the middle of a chain the agent
    /// started, and the public key alone would accept the same key registered on another account.
    fn agent(&self, method: &str) -> Result<GrantId, Refusal> {
        let me = env::current_account_id();
        if env::predecessor_account_id() != me || env::signer_account_id() != me {
            return Err((None, Denial::new("NOT_DIRECT_KEY", "caller is not a key on the vault account")));
        }
        let Some(gid) = self.keys.get(&env::signer_account_pk()).copied() else {
            return Err((None, Denial::new("UNKNOWN_KEY", "signing key maps to no grant")));
        };
        let bit = method_bit(method).expect("method is in METHODS");
        if self.grant(gid).methods & bit == 0 {
            return Err((Some(gid), Denial::new("METHOD_NOT_GRANTED", format!("{method} is not in this grant"))));
        }
        Ok(gid)
    }

    fn caller(&self, method: &str) -> Result<Caller, Refusal> {
        if env::predecessor_account_id() == self.owner {
            return Ok(Caller::Owner);
        }
        self.agent(method).map(Caller::Grant)
    }

    fn is_strict_ancestor(&self, ancestor: GrantId, of: GrantId) -> bool {
        let mut cur = self.grants.get(&of).and_then(|g| g.parent);
        while let Some(id) = cur {
            if id == ancestor {
                return true;
            }
            cur = self.grant(id).parent;
        }
        false
    }

    fn denied_decision(grant: Option<GrantId>, d: &Denial) -> Decision {
        Decision {
            outcome: "denied".into(),
            code: d.code.into(),
            detail: d.detail.clone(),
            grant,
            blocking_grant: d.blocking,
            limit: d.limit.map(U128),
            used: d.used.map(U128),
            attempted: d.attempted.map(U128),
            ..Default::default()
        }
    }

    /// Records a refusal. This returns normally so that the counter below survives. Nothing in
    /// here touches a spending counter.
    fn deny(&mut self, grant: Option<GrantId>, d: Denial) -> Decision {
        let decision = Self::denied_decision(grant, &d);
        events::emit("denied", json!(decision));
        if let (Some(gid), true) = (grant, d.counted) {
            self.count_denial(gid, d.code);
        }
        decision
    }

    fn count_denial(&mut self, gid: GrantId, code: &str) {
        let g = self.grants.get_mut(&gid).expect("grant");
        g.denied = g.denied.saturating_add(1);
        let (denied, max, already) = (g.denied, g.max_denials, g.revoked);
        if matches!(code, "REVOKED" | "ANCESTOR_REVOKED" | "EXPIRED" | "ANCESTOR_EXPIRED") {
            // This key can never spend again: reap it on the spot.
            self.delete_key_of(gid);
        } else if denied >= max && !already {
            self.grants.get_mut(&gid).expect("grant").revoked = true;
            events::emit("auto_revoked", json!({"grant": gid, "denied": denied, "max_denials": max}));
            self.enqueue_sweep(gid);
            self.run_sweep(EAGER_SWEEP);
        }
    }

    fn delete_key_of(&mut self, gid: GrantId) {
        let g = self.grants.get_mut(&gid).expect("grant");
        if !g.key_live {
            return;
        }
        g.key_live = false;
        let pk = g.pk.clone();
        self.keys.remove(&pk);
        // One receipt per key: actions in a receipt are atomic, so one bad key in a shared batch
        // would keep all the others alive.
        Promise::new(env::current_account_id()).delete_key(pk.clone()).detach();
        events::emit("key_deleted", json!({"grant": gid, "pk": pk}));
    }

    fn enqueue_sweep(&mut self, gid: GrantId) {
        self.sweep_queue.insert(self.sweep_tail, gid);
        self.sweep_tail += 1;
    }

    /// Breadth-first over revoked subtrees with a persistent queue. Each visit and each child pushed
    /// costs one unit, so a call stops at `limit` units plus at most MAX_CHILDREN.
    fn run_sweep(&mut self, limit: u32) -> u32 {
        let mut work = 0;
        while work < limit && self.sweep_head < self.sweep_tail {
            let gid = self.sweep_queue.remove(&self.sweep_head).expect("queued grant");
            self.sweep_head += 1;
            work += 1;
            self.grants.get_mut(&gid).expect("grant").revoked = true;
            self.delete_key_of(gid);
            for child in self.children.get(&gid).cloned().unwrap_or_default() {
                if !self.grant(child).revoked || self.grant(child).key_live {
                    self.enqueue_sweep(child);
                    work += 1;
                }
            }
        }
        work
    }

    fn create_grant(&mut self, parent: Option<GrantId>, pk: PublicKey, t: Terms) -> Promise {
        let id = self.next_grant;
        self.next_grant += 1;
        let depth = parent.map_or(0, |p| self.grant(p).depth + 1);
        let methods = t.methods.iter().fold(0u8, |acc, m| acc | method_bit(m).expect("checked"));
        if let Some(p) = parent {
            let parent_grant = self.grants.get_mut(&p).expect("parent");
            parent_grant.gas_left -= t.key_allowance.0 + t.gas_budget.0;
            parent_grant.nodes_left -= t.node_budget + 1;
            let mut siblings = self.children.get(&p).cloned().unwrap_or_default();
            siblings.push(id);
            self.children.insert(p, siblings);
        }
        if let Some(list) = &t.recipients {
            self.allowlists.insert(id, list.clone());
        }
        self.keys.insert(pk.clone(), id);
        self.grants.insert(
            id,
            Grant {
                id,
                parent,
                depth,
                asset: t.asset.clone(),
                cap: t.cap.0,
                per_tx: t.per_tx.0,
                window_cap: t.window_cap.0,
                window_ms: t.window_secs as u64 * 1000,
                threshold: t.approval_threshold.0,
                methods,
                expires_at_ms: t.expires_at_ms,
                max_depth: t.max_depth,
                max_denials: t.max_denials,
                key_allowance: t.key_allowance.0,
                pk: pk.clone(),
                key_live: true,
                spent: 0,
                reserved: 0,
                direct_spent: 0,
                direct_reserved: 0,
                window: Window::default(),
                gas_left: t.gas_budget.0,
                nodes_left: t.node_budget,
                denied: 0,
                revoked: false,
                queued: vec![],
            },
        );
        events::emit(
            "grant_created",
            json!({
                "grant": id, "parent": parent, "depth": depth, "pk": pk, "terms": t,
            }),
        );
        let me = env::current_account_id();
        Promise::new(me.clone())
            .add_access_key_allowance(pk, allowance(t.key_allowance.0), me.clone(), method_names(methods).join(","))
            .then(Self::ext(me).with_static_gas(GAS_KEY_CALLBACK).on_key_added(id))
    }

    fn commit_spend(
        &mut self,
        gid: GrantId,
        recipient: AccountId,
        amount: u128,
        memo: Option<String>,
        now: u64,
    ) -> Promise {
        let spend_id = self.take_nonce();
        let mut headroom = vec![];
        let mut cur = Some(gid);
        while let Some(id) = cur {
            let g = self.grants.get_mut(&id).expect("lineage");
            headroom.push(json!({
                "grant": id,
                "cap_left": U128(g.cap - g.spent - g.reserved),
                "window_left": U128(g.window_cap.saturating_sub(g.window.used(now, g.window_ms))),
            }));
            g.reserved += amount;
            g.window.add(now, g.window_ms, amount);
            cur = g.parent;
        }
        let leaf = self.grants.get_mut(&gid).expect("leaf");
        leaf.direct_reserved += amount;
        let asset = leaf.asset.clone();
        if let Asset::Ft(token) = &asset {
            *self.tokens.get_mut(token).expect("funds checked") -= amount;
        }
        self.pending.insert(
            spend_id,
            Pending {
                grant: Some(gid),
                asset: asset.clone(),
                amount,
                recipient: recipient.clone(),
                at_ms: now,
            },
        );
        self.in_flight += 1;
        events::emit(
            "spend_reserved",
            json!({
                "spend_id": spend_id, "grant": gid, "asset": asset, "amount": U128(amount),
                "recipient": recipient, "memo": memo, "headroom_before": headroom,
            }),
        );
        Self::dispatch(asset, recipient, amount, spend_id)
    }

    fn dispatch(asset: Asset, recipient: AccountId, amount: u128, spend_id: u64) -> Promise {
        let transfer = match asset {
            Asset::Native => Promise::new(recipient).transfer(NearToken::from_yoctonear(amount)),
            Asset::Ft(token) => Promise::new(token).function_call(
                "ft_transfer".to_string(),
                json!({"receiver_id": recipient, "amount": U128(amount)}).to_string().into_bytes(),
                NearToken::from_yoctonear(1),
                GAS_FT_TRANSFER,
            ),
        };
        transfer.then(
            Self::ext(env::current_account_id())
                .with_static_gas(GAS_SETTLE)
                .on_spend_settled(spend_id),
        )
    }

    fn enqueue(
        &mut self,
        gid: GrantId,
        recipient: AccountId,
        amount: u128,
        memo: Option<String>,
        now: u64,
    ) -> Decision {
        // Expired entries free their slot here, so a full queue cannot block a grant forever. A
        // refused request leaves them in place: a denial changes nothing but the denial counter.
        let (mut live, stale): (Vec<u64>, Vec<u64>) = self
            .grant(gid)
            .queued
            .iter()
            .partition(|id| self.approvals.get(id).is_some_and(|a| a.expires_at_ms > now));
        if live.len() >= MAX_QUEUED {
            return self.deny(Some(gid), Denial::new("QUEUE_FULL", format!("{MAX_QUEUED} approvals already waiting")));
        }
        for id in stale {
            self.approvals.remove(&id);
        }
        let approval_id = self.take_nonce();
        let asset = self.grant(gid).asset.clone();
        let hash = self.spend_hash(gid, &asset, &recipient, amount, &memo, approval_id);
        live.push(approval_id);
        self.grants.get_mut(&gid).expect("grant").queued = live;
        self.approvals.insert(
            approval_id,
            Approval {
                grant: gid,
                recipient: recipient.clone(),
                amount,
                memo,
                hash,
                expires_at_ms: now + self.approval_ttl_ms,
                approved_by: None,
            },
        );
        let threshold = self.grant(gid).threshold;
        events::emit(
            "approval_queued",
            json!({
                "approval_id": approval_id, "grant": gid, "recipient": recipient, "amount": U128(amount),
                "threshold": U128(threshold), "spend_hash": hex(&hash),
            }),
        );
        Decision {
            outcome: "queued".into(),
            code: "NEEDS_APPROVAL".into(),
            detail: format!("{amount} > {threshold} approval threshold"),
            grant: Some(gid),
            limit: Some(U128(threshold)),
            attempted: Some(U128(amount)),
            approval_id: Some(approval_id),
            spend_hash: Some(hex(&hash)),
            ..Default::default()
        }
    }

    /// Commits to the vault, grant, asset, recipient, amount, memo and a nonce that is never
    /// reused, so an approval cannot move to another spend, another grant or a later repeat.
    fn spend_hash(
        &self,
        grant: GrantId,
        asset: &Asset,
        recipient: &AccountId,
        amount: u128,
        memo: &Option<String>,
        nonce: u64,
    ) -> [u8; 32] {
        let bytes = near_sdk::borsh::to_vec(&(
            env::current_account_id(),
            grant,
            asset,
            recipient,
            amount,
            memo,
            nonce,
        ))
        .expect("borsh");
        env::sha256_array(&bytes)
    }

    fn available_native(&self) -> u128 {
        let storage = (env::storage_usage() + STORAGE_MARGIN_BYTES) as u128
            * env::storage_byte_cost().as_yoctonear();
        env::account_balance()
            .as_yoctonear()
            .saturating_sub(storage)
            .saturating_sub(self.gas_floor)
    }

    fn spendable(&self, gid: GrantId, now: u64) -> u128 {
        if self.check_live(gid, now).is_err() {
            return 0;
        }
        let mut best = u128::MAX;
        let mut cur = Some(gid);
        while let Some(id) = cur {
            let g = self.grant(id);
            let cap_left = g.cap - g.spent - g.reserved;
            let window_left = g.window_cap.saturating_sub(g.window.used(now, g.window_ms));
            best = best.min(cap_left).min(window_left);
            cur = g.parent;
        }
        best
    }

    // ---- Checks. All read-only; state changes only after every check has passed. --------------

    fn check_gas(min: Gas) -> Result<(), Denial> {
        if env::prepaid_gas() < min {
            return Err(Denial::new("GAS_TOO_LOW", format!("{} < {} prepaid gas", env::prepaid_gas(), min)));
        }
        Ok(())
    }

    fn check_memo(memo: &Option<String>) -> Result<(), Denial> {
        match memo {
            Some(m) if m.len() > MAX_MEMO_BYTES => {
                Err(Denial::new("BAD_MEMO", format!("{} > {MAX_MEMO_BYTES} bytes", m.len())))
            }
            _ => Ok(()),
        }
    }

    fn check_not_paused(&self) -> Result<(), Denial> {
        if self.paused {
            return Err(Denial::new("PAUSED", "vault is paused").uncounted());
        }
        Ok(())
    }

    fn check_asset_accepted(&self, asset: &Asset) -> Result<(), Denial> {
        match asset {
            Asset::Ft(token) if !self.tokens.contains_key(token) => {
                Err(Denial::new("TOKEN_NOT_ACCEPTED", format!("{token} was not added by the owner")))
            }
            _ => Ok(()),
        }
    }

    fn check_key_free(&self, pk: &PublicKey) -> Result<(), Denial> {
        if self.keys.contains_key(pk) {
            return Err(Denial::new("KEY_IN_USE", "this public key already belongs to a grant"));
        }
        Ok(())
    }

    fn check_storage_room(&self) -> Result<(), Denial> {
        if self.available_native() == 0 {
            return Err(Denial::new("VAULT_BALANCE", "no free balance left to pay for storage"));
        }
        Ok(())
    }

    /// Walks the lineage. This loop is the whole of cascading revocation: every path that can move
    /// funds or extend the tree runs it, and it visits at most MAX_TREE_DEPTH + 1 grants.
    fn check_live(&self, gid: GrantId, now: u64) -> Result<(), Denial> {
        let mut cur = Some(gid);
        while let Some(id) = cur {
            let g = self.grant(id);
            if g.revoked {
                let code = if id == gid { "REVOKED" } else { "ANCESTOR_REVOKED" };
                return Err(Denial::new(code, format!("grant {id} is revoked")).at(id));
            }
            if now >= g.expires_at_ms {
                let code = if id == gid { "EXPIRED" } else { "ANCESTOR_EXPIRED" };
                return Err(Denial::new(code, format!("grant {id} expired at {}", g.expires_at_ms)).at(id));
            }
            cur = g.parent;
        }
        Ok(())
    }

    fn check_headroom(&self, gid: GrantId, amount: u128, now: u64) -> Result<(), Denial> {
        let mut cur = Some(gid);
        while let Some(id) = cur {
            let g = self.grant(id);
            let used = g.spent + g.reserved;
            if amount > g.cap - used {
                let code = if id == gid { "CAP" } else { "ANCESTOR_CAP" };
                return Err(Denial::new(code, format!("{used} + {amount} > {} cap on grant {id}", g.cap))
                    .at(id)
                    .numbers(g.cap, used, amount));
            }
            let in_window = g.window.used(now, g.window_ms);
            if amount > g.window_cap.saturating_sub(in_window) {
                let code = if id == gid { "WINDOW" } else { "ANCESTOR_WINDOW" };
                return Err(Denial::new(
                    code,
                    format!("{in_window} + {amount} > {} in rolling window on grant {id}", g.window_cap),
                )
                .at(id)
                .numbers(g.window_cap, in_window, amount));
            }
            cur = g.parent;
        }
        Ok(())
    }

    fn check_vault_funds(&self, asset: &Asset, amount: u128) -> Result<(), Denial> {
        let free = match asset {
            Asset::Native => self.available_native(),
            Asset::Ft(token) => self.tokens.get(token).copied().unwrap_or(0),
        };
        if amount > free {
            return Err(Denial::new("VAULT_BALANCE", format!("{amount} > {free} free in vault"))
                .numbers(free, 0, amount));
        }
        Ok(())
    }

    /// The single spending policy. Used by request_spend, execute_approved and preview_spend.
    fn evaluate_spend(&self, gid: GrantId, recipient: &AccountId, amount: u128, now: u64) -> Result<(), Denial> {
        self.check_live(gid, now)?;
        self.check_not_paused()?;
        if amount == 0 {
            return Err(Denial::new("BAD_AMOUNT", "amount must be positive"));
        }
        if *recipient == env::current_account_id() {
            return Err(Denial::new("RECIPIENT_SELF", "the vault cannot pay itself"));
        }
        if self.blocked.contains(recipient) {
            return Err(Denial::new("RECIPIENT_BLOCKED", format!("{recipient} is blocked by the owner")));
        }
        // The leaf's list is enough: delegation proved it a subset of every ancestor's list, and
        // no method can change a list afterwards.
        if let Some(list) = self.allowlists.get(&gid) {
            if !list.contains(recipient) {
                return Err(Denial::new("RECIPIENT_NOT_ALLOWED", format!("{recipient} is not in grant {gid}")).at(gid));
            }
        }
        let g = self.grant(gid);
        if amount > g.per_tx {
            return Err(Denial::new("PER_TX", format!("{amount} > {} per-tx", g.per_tx))
                .at(gid)
                .numbers(g.per_tx, 0, amount));
        }
        self.check_headroom(gid, amount, now)?;
        self.check_vault_funds(&g.asset, amount)
    }

    fn check_terms(&self, t: &Terms, now: u64) -> Result<(), Denial> {
        let bad = |msg: &str| Err(Denial::new("BAD_TERMS", msg));
        if t.cap.0 == 0 || t.per_tx.0 == 0 || t.window_cap.0 == 0 {
            return bad("cap, per_tx and window_cap must be positive");
        }
        if t.window_secs == 0 {
            return bad("window_secs must be positive");
        }
        // An FC key with an empty method list may call every method, so empty must never pass.
        if t.methods.is_empty() {
            return bad("methods must not be empty: an empty list means all methods at protocol level");
        }
        if t.methods.iter().any(|m| method_bit(m).is_none()) {
            return bad("unknown method");
        }
        if t.key_allowance.0 == 0 {
            return bad("key_allowance must be positive");
        }
        if t.max_denials == 0 {
            return bad("max_denials must be positive");
        }
        if t.max_depth > MAX_TREE_DEPTH {
            return bad("max_depth over the tree limit");
        }
        if t.expires_at_ms <= now {
            return bad("expiry is in the past");
        }
        if let Some(list) = &t.recipients {
            if list.is_empty() || list.len() > MAX_RECIPIENTS {
                return bad("recipient list must hold 1 to 16 accounts");
            }
        }
        Ok(())
    }

    /// A child must be narrower than its parent in every dimension. Ordered like Terms.
    fn check_attenuation(&self, parent: GrantId, t: &Terms, now: u64) -> Result<(), Denial> {
        let p = self.grant(parent);
        let wide = |code: &'static str, limit: u128, asked: u128, what: &str| {
            Err(Denial::new(code, format!("child {what} {asked} > parent {limit}"))
                .at(parent)
                .numbers(limit, 0, asked))
        };
        if t.asset != p.asset {
            return Err(Denial::new("ATT_ASSET", "child asset differs from parent").at(parent));
        }
        // Remaining, not nominal: the smallest headroom anywhere on the lineage.
        let mut remaining = u128::MAX;
        let mut cur = Some(parent);
        while let Some(id) = cur {
            let a = self.grant(id);
            remaining = remaining.min(a.cap - a.spent - a.reserved);
            cur = a.parent;
        }
        if t.cap.0 > remaining {
            return wide("ATT_CAP", remaining, t.cap.0, "cap");
        }
        if t.per_tx.0 > p.per_tx {
            return wide("ATT_PER_TX", p.per_tx, t.per_tx.0, "per_tx");
        }
        if t.window_cap.0 > p.window_cap {
            return wide("ATT_WINDOW_CAP", p.window_cap, t.window_cap.0, "window_cap");
        }
        // The same amount over a shorter window is a higher rate, so the window may only grow.
        let window_ms = t.window_secs as u64 * 1000;
        if window_ms < p.window_ms {
            return Err(Denial::new(
                "ATT_WINDOW_LEN",
                format!("child window {window_ms} ms < parent {} ms", p.window_ms),
            )
            .at(parent)
            .numbers(p.window_ms as u128, 0, window_ms as u128));
        }
        if t.approval_threshold.0 > p.threshold {
            return wide("ATT_THRESHOLD", p.threshold, t.approval_threshold.0, "approval_threshold");
        }
        match (self.allowlists.get(&parent), &t.recipients) {
            (None, _) => {}
            (Some(_), None) => {
                return Err(Denial::new("ATT_RECIPIENTS", "child asked for any recipient under a list").at(parent));
            }
            (Some(list), Some(asked)) => {
                if let Some(extra) = asked.iter().find(|a| !list.contains(a)) {
                    return Err(Denial::new("ATT_RECIPIENTS", format!("{extra} is not in the parent list")).at(parent));
                }
            }
        }
        let bits = t.methods.iter().fold(0u8, |acc, m| acc | method_bit(m).expect("checked"));
        if bits & !p.methods != 0 {
            return Err(Denial::new(
                "ATT_METHODS",
                format!("{:?} are not in the parent", method_names(bits & !p.methods)),
            )
            .at(parent));
        }
        if t.expires_at_ms > p.expires_at_ms {
            return wide("ATT_EXPIRY", p.expires_at_ms as u128, t.expires_at_ms as u128, "expiry");
        }
        if p.max_depth == 0 || t.max_depth > p.max_depth - 1 {
            return Err(Denial::new(
                "ATT_DEPTH",
                format!("child max_depth {} with parent max_depth {}", t.max_depth, p.max_depth),
            )
            .at(parent)
            .numbers(p.max_depth as u128, 0, t.max_depth as u128 + 1));
        }
        if t.max_denials > p.max_denials {
            return wide("ATT_DENIALS", p.max_denials as u128, t.max_denials as u128, "max_denials");
        }
        let gas_asked = t.key_allowance.0.saturating_add(t.gas_budget.0);
        if gas_asked > p.gas_left {
            return wide("ATT_GAS", p.gas_left, gas_asked, "key_allowance + gas_budget");
        }
        let nodes_asked = t.node_budget.saturating_add(1);
        if nodes_asked > p.nodes_left {
            return wide("ATT_NODES", p.nodes_left as u128, nodes_asked as u128, "node_budget + 1");
        }
        if self.children.get(&parent).map_or(0, Vec::len) >= MAX_CHILDREN {
            return Err(Denial::new("ATT_NODES", format!("parent already has {MAX_CHILDREN} children")).at(parent));
        }
        let _ = now;
        Ok(())
    }

    fn check_revoke(&self, caller: &Caller, target: GrantId) -> Result<(), Denial> {
        let Some(t) = self.grants.get(&target) else {
            return Err(Denial::new("UNKNOWN_GRANT", format!("grant {target} does not exist")));
        };
        if let Caller::Grant(c) = caller {
            self.check_live(*c, now_ms())?;
            if !self.is_strict_ancestor(*c, target) {
                return Err(Denial::new("NOT_ANCESTOR", format!("grant {c} is not above grant {target}")).at(target));
            }
        }
        if t.revoked {
            return Err(Denial::new("ALREADY_REVOKED", format!("grant {target} is already revoked")).uncounted());
        }
        Ok(())
    }

    fn check_key_op(&self, caller: &Caller, target: GrantId, allow_self: bool, cost: u128) -> Result<(), Denial> {
        let now = now_ms();
        let Some(t) = self.grants.get(&target) else {
            return Err(Denial::new("UNKNOWN_GRANT", format!("grant {target} does not exist")));
        };
        if cost == 0 {
            return Err(Denial::new("BAD_TERMS", "allowance must be positive"));
        }
        self.check_live(target, now)?;
        if !t.key_live {
            return Err(Denial::new("KEY_GONE", format!("grant {target} has no live key")).at(target));
        }
        if let Caller::Grant(c) = caller {
            self.check_live(*c, now)?;
            let related = (allow_self && *c == target) || self.is_strict_ancestor(*c, target);
            if !related {
                return Err(Denial::new("NOT_ANCESTOR", format!("grant {c} is not above grant {target}")).at(target));
            }
            let left = self.grant(*c).gas_left;
            if cost > left {
                return Err(Denial::new("GAS_BUDGET", format!("{cost} > {left} gas budget left"))
                    .at(*c)
                    .numbers(left, 0, cost));
            }
        }
        Ok(())
    }

    fn check_approve(&self, caller: &Caller, id: u64, spend_hash: &str, now: u64) -> Result<(), Denial> {
        let Some(a) = self.approvals.get(&id) else {
            return Err(Denial::new("APPROVAL_NOT_FOUND", format!("no approval {id}")));
        };
        if now >= a.expires_at_ms {
            return Err(Denial::new("APPROVAL_EXPIRED", format!("approval {id} expired")));
        }
        if hex(&a.hash) != spend_hash {
            return Err(Denial::new("APPROVAL_HASH_MISMATCH", "hash is not the hash of this spend"));
        }
        if let Caller::Grant(c) = caller {
            self.check_live(*c, now)?;
            if !self.is_strict_ancestor(*c, a.grant) {
                return Err(Denial::new("NOT_ANCESTOR", format!("grant {c} is not above grant {}", a.grant)).at(a.grant));
            }
            let threshold = self.grant(*c).threshold;
            if a.amount > threshold {
                return Err(Denial::new("APPROVER_LIMIT", format!("{} > {threshold} approver threshold", a.amount))
                    .at(*c)
                    .numbers(threshold, 0, a.amount));
            }
        }
        Ok(())
    }

    fn check_execute(
        &self,
        caller: &Caller,
        id: u64,
        recipient: &AccountId,
        amount: u128,
        memo: &Option<String>,
        now: u64,
    ) -> Result<Approval, Denial> {
        Self::check_gas(GAS_MIN_SPEND)?;
        let Some(a) = self.approvals.get(&id) else {
            return Err(Denial::new("APPROVAL_NOT_FOUND", format!("no approval {id}")));
        };
        if let Caller::Grant(c) = caller {
            if *c != a.grant {
                return Err(Denial::new("APPROVAL_WRONG_GRANT", format!("approval {id} belongs to grant {}", a.grant)));
            }
        }
        let Some(approver) = &a.approved_by else {
            return Err(Denial::new("APPROVAL_NOT_APPROVED", format!("approval {id} is still waiting")));
        };
        if now >= a.expires_at_ms {
            return Err(Denial::new("APPROVAL_EXPIRED", format!("approval {id} expired")));
        }
        let asset = &self.grant(a.grant).asset;
        if self.spend_hash(a.grant, asset, recipient, amount, memo, id) != a.hash {
            return Err(Denial::new("APPROVAL_HASH_MISMATCH", "arguments are not the approved spend"));
        }
        if let Approver::Grant(x) = approver {
            if let Err(d) = self.check_live(*x, now) {
                return Err(Denial::new("APPROVER_REVOKED", d.detail).at(*x));
            }
        }
        self.evaluate_spend(a.grant, recipient, amount, now)?;
        Ok(a.clone())
    }
}
