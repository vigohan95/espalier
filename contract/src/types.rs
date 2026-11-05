use near_sdk::json_types::U128;
use near_sdk::{near, AccountId, PublicKey};

use crate::window::Window;

pub type GrantId = u64;

/// Hard ceiling on tree height, so every ancestry walk touches at most MAX_TREE_DEPTH + 1 grants.
pub const MAX_TREE_DEPTH: u8 = 8;
pub const MAX_RECIPIENTS: usize = 16;
pub const MAX_CHILDREN: usize = 32;
pub const MAX_QUEUED: usize = 8;
pub const MAX_MEMO_BYTES: usize = 128;
/// Grants visited by the eager key sweep inside one revoke. Everything past this is reaped by
/// `sweep` or by the key's own next attempt. The revocation itself is already complete.
///
/// The property test runs with 1, so that revoked grants keep live keys for a while and their
/// attempts reach the lineage walk instead of being stopped by a key that is already gone.
pub const EAGER_SWEEP: u32 = if cfg!(test) { 1 } else { 32 };
pub const AUTO_REVOKE_SWEEP: u32 = 8;
pub const MAX_SWEEP: u32 = 64;
/// A clearance ticket must expire within this much of the moment it is used.
pub const MAX_CLEARANCE_TTL_MS: u64 = 3_600_000;
pub const MAX_REPORTERS: usize = 16;
/// One reporter alone must never be able to block a recipient, so this is the floor for any vault.
pub const MIN_QUORUM: u8 = 2;

/// Vault methods a grant can hold. The FC key the vault issues lists exactly these names, so the
/// protocol enforces this dimension before the contract runs.
pub const METHODS: [&str; 8] = [
    "request_spend",
    "execute_approved",
    "delegate",
    "revoke",
    "approve",
    "refill_key",
    "rotate_key",
    "sweep",
];

pub fn method_bit(name: &str) -> Option<u8> {
    METHODS.iter().position(|m| *m == name).map(|i| 1u8 << i)
}

pub fn method_names(bits: u8) -> Vec<String> {
    METHODS
        .iter()
        .enumerate()
        .filter(|(i, _)| bits & (1u8 << i) != 0)
        .map(|(_, m)| m.to_string())
        .collect()
}

#[near(serializers = [borsh, json])]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Asset {
    Native,
    Ft(AccountId),
}

/// What a delegator offers. Every field is one attenuation dimension.
#[near(serializers = [json])]
#[derive(Clone, Debug)]
pub struct Terms {
    pub asset: Asset,
    /// Lifetime budget of this grant and everything below it.
    pub cap: U128,
    pub per_tx: U128,
    pub window_cap: U128,
    pub window_secs: u32,
    /// Amounts above this go to the approval queue. Zero means every spend needs approval.
    pub approval_threshold: U128,
    /// None means any recipient. Only a root can hold None.
    pub recipients: Option<Vec<AccountId>>,
    pub methods: Vec<String>,
    pub expires_at_ms: u64,
    /// Levels of delegation still allowed below this grant.
    pub max_depth: u8,
    /// Denied attempts before the vault revokes this grant and deletes its key.
    pub max_denials: u32,
    /// Protocol gas allowance on the FC key issued for this grant, in yoctoNEAR.
    pub key_allowance: U128,
    /// Allowance this grant can hand to keys below it or refill with.
    pub gas_budget: U128,
    /// Grants this grant can create below itself, across all levels.
    pub node_budget: u32,
    /// Every spend needs a fresh ticket signed by the vault's attestor, or a human approval.
    pub clearance_required: bool,
}

/// The shared denylist this vault consults on every spend, and whose reports it counts.
#[near(serializers = [borsh, json])]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Screen {
    pub registry: AccountId,
    pub reporters: Vec<AccountId>,
    pub quorum: u8,
}

/// Signed off chain after the recipient check says allow. The grant and the recipient are not
/// fields: they come from the call itself and go into the signed message, so a ticket for anything
/// else fails the signature.
#[near(serializers = [json])]
#[derive(Clone, Debug)]
pub struct Clearance {
    pub max_amount: U128,
    pub expires_at_ms: u64,
    /// Hex sha256 of the recipient's code as the attestor saw it. All zero for a plain account.
    pub code_hash: String,
    /// Hex ed25519 signature.
    pub signature: String,
}

/// Payments that settled between a grant and a recipient. Written only by settlement.
#[near(serializers = [borsh, json])]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Contact {
    pub count: u32,
    pub total: U128,
    pub first_ms: u64,
    pub last_ms: u64,
}

/// What the vault reads from the registry. Anything that does not parse counts as no answer.
#[near(serializers = [json])]
#[derive(Clone, Debug)]
pub struct ScreenAnswer {
    pub listed: bool,
}

#[near(serializers = [borsh])]
#[derive(Clone, Debug)]
pub struct Grant {
    // Fixed at creation. No method writes these, which is what makes the subset checks at
    // delegation time hold for the life of the tree.
    pub id: GrantId,
    pub parent: Option<GrantId>,
    pub depth: u8,
    pub asset: Asset,
    pub cap: u128,
    pub per_tx: u128,
    pub window_cap: u128,
    pub window_ms: u64,
    pub threshold: u128,
    pub methods: u8,
    pub expires_at_ms: u64,
    pub max_depth: u8,
    pub max_denials: u32,
    pub key_allowance: u128,
    pub clearance: bool,

    // Live state.
    pub pk: PublicKey,
    pub key_live: bool,
    /// Settled by this grant and everything below it.
    pub spent: u128,
    /// In flight for this grant and everything below it.
    pub reserved: u128,
    pub direct_spent: u128,
    pub direct_reserved: u128,
    pub window: Window,
    pub gas_left: u128,
    pub nodes_left: u32,
    pub denied: u32,
    pub revoked: bool,
    pub queued: Vec<u64>,
}

#[near(serializers = [borsh])]
#[derive(Clone, Debug)]
pub struct Pending {
    /// None for an owner withdrawal, which belongs to no grant.
    pub grant: Option<GrantId>,
    pub asset: Asset,
    pub amount: u128,
    pub recipient: AccountId,
    pub at_ms: u64,
}

#[near(serializers = [borsh, json])]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Approver {
    Owner,
    Grant(GrantId),
}

#[near(serializers = [borsh])]
#[derive(Clone, Debug)]
pub struct Approval {
    pub grant: GrantId,
    pub recipient: AccountId,
    pub amount: u128,
    pub memo: Option<String>,
    pub hash: [u8; 32],
    pub expires_at_ms: u64,
    pub approved_by: Option<Approver>,
    /// Queued for want of a ticket, so only someone not bound by clearance can approve it.
    pub no_clearance: bool,
}

/// Returned by every state-changing agent method. A denial is a normal return, never a panic: a
/// panic would roll back the denial counter that stops a compromised key burning gas.
#[near(serializers = [json])]
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Decision {
    /// "allowed", "queued", "denied", "settled" or "rolled_back".
    pub outcome: String,
    pub code: String,
    pub detail: String,
    pub grant: Option<GrantId>,
    pub blocking_grant: Option<GrantId>,
    pub limit: Option<U128>,
    pub used: Option<U128>,
    pub attempted: Option<U128>,
    pub spend_id: Option<u64>,
    pub approval_id: Option<u64>,
    pub spend_hash: Option<String>,
    pub new_grant: Option<GrantId>,
}

#[near(serializers = [json])]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GrantView {
    pub id: GrantId,
    pub parent: Option<GrantId>,
    pub depth: u8,
    pub asset: Asset,
    pub cap: U128,
    pub per_tx: U128,
    pub window_cap: U128,
    pub window_secs: u32,
    pub window_used: U128,
    pub approval_threshold: U128,
    pub recipients: Option<Vec<AccountId>>,
    pub methods: Vec<String>,
    pub expires_at_ms: u64,
    pub max_depth: u8,
    pub max_denials: u32,
    pub key_allowance: U128,
    pub clearance_required: bool,
    pub pk: PublicKey,
    pub key_live: bool,
    pub spent: U128,
    pub reserved: U128,
    pub direct_spent: U128,
    pub direct_reserved: U128,
    pub gas_left: U128,
    pub nodes_left: u32,
    pub denied: u32,
    pub revoked: bool,
    /// False when this grant or any ancestor is revoked or expired.
    pub live: bool,
    /// Most this grant can spend now: the smallest cap and window headroom along its lineage.
    pub spendable: U128,
    pub children: Vec<GrantId>,
}

#[near(serializers = [json])]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApprovalView {
    pub id: u64,
    pub grant: GrantId,
    pub recipient: AccountId,
    pub amount: U128,
    pub memo: Option<String>,
    pub spend_hash: String,
    pub expires_at_ms: u64,
    pub approved_by: Option<Approver>,
    pub no_clearance: bool,
}

#[near(serializers = [json])]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VaultInfo {
    pub owner: AccountId,
    pub paused: bool,
    pub gas_floor: U128,
    pub approval_ttl_ms: u64,
    pub grants: u64,
    pub in_flight: u64,
    pub sweep_backlog: u64,
    pub available_native: U128,
    /// Native funds promised to spends that are waiting for the registry's answer.
    pub native_held: U128,
    pub screen: Option<Screen>,
    pub attestor: Option<PublicKey>,
    pub now_ms: u64,
}
