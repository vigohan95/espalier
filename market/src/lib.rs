//! Espalier market: a sealed-bid subcontracting market where the prize is a grant.
//!
//! A grant holder in an Espalier vault posts a task. Workers bid sealed. The lowest bid wins at the
//! second-lowest price. The poster then creates a child grant for the winner with the vault's own
//! `delegate`, and this contract reads the vault to confirm it is the right one: right parent, right
//! key, cap equal to the price. The winner can post tasks of its own from that grant, and so on down.
//!
//! No money sits here except bonds. A winner is paid by drawing from its own grant, which can pay
//! only this contract, and this contract forwards the draw once the poster has accepted the work.
//! The vault is never changed and never called for anything but its views. See docs/SWARM-RULES.md.

use near_sdk::json_types::U128;
use near_sdk::serde::Deserialize;
use near_sdk::serde_json::{self, json, Value};
use near_sdk::store::{LookupMap, LookupSet};
use near_sdk::{
    env, near, require, AccountId, BorshStorageKey, CurveType, Gas, NearToken, PanicOnDefault, Promise,
    PromiseOrValue, PublicKey,
};

pub mod pricing;

const GAS_VIEW: Gas = Gas::from_tgas(12);
const GAS_CALLBACK: Gas = Gas::from_tgas(30);
const GAS_PAYOUT_CALLBACK: Gas = Gas::from_tgas(6);

pub const MAX_BIDS: usize = 16;
pub const MAX_OPEN_TASKS: u32 = 8;
pub const MAX_ROSTER: usize = 64;
pub const MAX_VAULTS: usize = 8;
/// A poster that fails to create the winner's grant this many times cannot post again.
pub const MAX_DEFAULTS: u32 = 3;
/// A winner's grant must survive at least this many refused calls, or one mistake would kill it.
pub const MIN_DENIALS: u32 = 3;
const MAX_VIEW_BYTES: u64 = 16_384;

const TAG_POST: &[u8] = b"espalier-market-post-v1";
const TAG_ACCEPT: &[u8] = b"espalier-market-accept-v1";
const TAG_REJECT: &[u8] = b"espalier-market-reject-v1";
const TAG_BID: &[u8] = b"espalier-market-bid-v1";

pub const STANDARD: &str = "espalier-market";

#[derive(BorshStorageKey)]
#[near]
enum Key {
    Tasks,
    Posted,
    Rosters,
    UsedKeys,
    GrantTask,
    Posters,
    Seen,
    Balances,
}

/// What a poster signs with its grant key. Every field is part of the signed message.
#[near(serializers = [borsh, json])]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Spec {
    pub vault: AccountId,
    /// The poster's grant. The winner's grant will be its child.
    pub grant_id: u64,
    /// Hex sha256 of the brief. The brief itself stays off chain.
    pub spec_hash: String,
    /// The most the poster will pay. Also what a lone bidder is paid.
    pub max_budget: U128,
    pub bid_bond: U128,
    pub min_bids: u8,
    pub commit_end_ms: u64,
    pub reveal_end_ms: u64,
    pub award_end_ms: u64,
    pub deliver_end_ms: u64,
    pub review_end_ms: u64,
    /// Levels of subcontracting the winner may open below itself.
    pub sub_depth: u8,
    /// Node budget the winner's grant must carry, for its own subcontractors.
    pub sub_nodes: u32,
    /// Gas budget the winner's grant must carry, for its subcontractors' keys.
    pub sub_gas: U128,
    /// Least allowance on the winner's own key.
    pub key_allowance: U128,
    pub nonce: u64,
}

#[near(serializers = [borsh, json])]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    /// Bids are being committed, then revealed.
    Open,
    /// A winner is chosen. Waiting for the poster to create its grant in the vault.
    Awarding,
    /// The grant is confirmed. Work is in progress.
    Awarded,
    Delivered,
    /// The winner may draw its price.
    Accepted,
    /// Accepted and finished: paid in full, or the grant is gone.
    Paid,
    Rejected,
    /// The winner did not deliver in time.
    Failed,
    /// Too few valid bids.
    NoAward,
    /// The poster did not create the winner's grant in time.
    Defaulted,
    /// The poster or the winner lost its authority in the vault.
    Void,
}

impl Status {
    fn is_final(self) -> bool {
        matches!(self, Status::Paid | Status::Rejected | Status::Failed | Status::NoAward | Status::Defaulted | Status::Void)
    }
}

#[near(serializers = [borsh])]
#[derive(Clone, Debug)]
pub struct Bid {
    pub worker: AccountId,
    pub commitment: [u8; 32],
    /// Bond still held for this bid. Zero once it was returned or forfeited.
    pub bond: u128,
    pub price: Option<u128>,
    pub pk: Option<String>,
}

#[near(serializers = [borsh])]
#[derive(Clone, Debug)]
pub struct Task {
    pub id: u64,
    pub spec: Spec,
    pub status: Status,
    pub note: String,
    pub posted_ms: u64,
    pub bond_payer: AccountId,
    /// Poster bond still held.
    pub poster_bond: u128,
    pub bids: Vec<Bid>,
    pub winner: Option<u32>,
    pub price: u128,
    pub grant: Option<u64>,
    pub result_hash: Option<String>,
    pub reject_hash: Option<String>,
    /// Spent by the winner's grant and everything below it, as last read from the vault. This is
    /// how much of the price has already been charged to the poster's branch of the tree.
    pub drawn: u128,
    /// Forwarded to the winner so far.
    pub paid: u128,
    /// Drawn by the winner's grant and not payable: before acceptance, or beyond the price.
    pub held: u128,
    /// Sent back to the vault so far.
    pub refunded: u128,
}

#[near(serializers = [borsh, json])]
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PosterStat {
    pub posted: u32,
    /// Tasks not yet in a final state.
    pub open: u32,
    /// Budget promised to open tasks and not yet drawn.
    pub committed: U128,
    pub defaults: u32,
    pub rejected: u32,
    pub accepted: u32,
}

/// The fields of the vault's GrantView this contract reads. Other fields are ignored.
#[derive(Deserialize, Clone, Debug)]
#[serde(crate = "near_sdk::serde")]
pub struct GrantLite {
    pub parent: Option<u64>,
    pub asset: Value,
    pub cap: U128,
    pub per_tx: U128,
    pub window_cap: U128,
    pub approval_threshold: U128,
    pub recipients: Option<Vec<AccountId>>,
    pub methods: Vec<String>,
    pub expires_at_ms: u64,
    pub max_depth: u8,
    pub max_denials: u32,
    pub key_allowance: U128,
    pub clearance_required: bool,
    pub pk: String,
    pub key_live: bool,
    pub spent: U128,
    pub direct_spent: U128,
    pub gas_left: U128,
    pub nodes_left: u32,
    pub live: bool,
    pub spendable: U128,
}

/// Returned by every call that asks the vault first. A refusal is a normal return with a code.
#[near(serializers = [json])]
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Res {
    pub ok: bool,
    pub code: String,
    pub detail: String,
    pub task: Option<u64>,
    pub amount: Option<U128>,
}

#[near(serializers = [json])]
#[derive(Clone, Debug)]
pub struct BidView {
    pub worker: AccountId,
    pub commitment: String,
    pub bond: U128,
    pub revealed: bool,
    pub price: Option<U128>,
    pub pk: Option<String>,
}

#[near(serializers = [json])]
#[derive(Clone, Debug)]
pub struct TaskView {
    pub id: u64,
    pub spec: Spec,
    pub status: Status,
    /// "commit", "reveal", "close" while the status is Open, else the status in lower case.
    pub phase: String,
    pub note: String,
    pub posted_ms: u64,
    pub bond_payer: AccountId,
    pub poster_bond: U128,
    pub bids: Vec<BidView>,
    pub winner: Option<AccountId>,
    pub winner_pk: Option<String>,
    pub price: U128,
    pub grant: Option<u64>,
    pub result_hash: Option<String>,
    pub reject_hash: Option<String>,
    /// Spent by the winner's grant and its own hires, as last read from the vault.
    pub drawn: U128,
    /// Of that, forwarded to the winner.
    pub paid: U128,
    pub held: U128,
    pub refunded: U128,
}

#[near(serializers = [json])]
#[derive(Clone, Debug)]
pub struct Config {
    pub vaults: Vec<AccountId>,
    pub poster_bond: U128,
    pub min_window_ms: u64,
    pub draw_grace_ms: u64,
    pub tasks: u64,
    pub now_ms: u64,
}

#[near(serializers = [json])]
#[derive(Clone, Debug)]
pub struct Solvency {
    pub bonds_held: U128,
    pub balances: U128,
    pub held: U128,
    pub owed: U128,
    pub balance: U128,
}

#[near(contract_state)]
#[derive(PanicOnDefault)]
pub struct Market {
    vaults: Vec<AccountId>,
    poster_bond: u128,
    min_window_ms: u64,
    draw_grace_ms: u64,
    next_task: u64,
    tasks: LookupMap<u64, Task>,
    /// sha256 of every signed post already used, so a post cannot be replayed.
    posted: LookupSet<[u8; 32]>,
    /// Vault -> the worker accounts its owner lets bid for its money.
    rosters: LookupMap<AccountId, Vec<AccountId>>,
    /// Grant keys already bound to a revealed bid.
    used_keys: LookupSet<String>,
    /// Awarded grant -> its task.
    grant_task: LookupMap<(AccountId, u64), u64>,
    posters: LookupMap<(AccountId, u64), PosterStat>,
    /// How much of a grant's `direct_spent` this contract has already counted.
    seen: LookupMap<(AccountId, u64), u128>,
    balances: LookupMap<AccountId, u128>,
    bonds_held: u128,
    balances_total: u128,
    held_total: u128,
}

fn now_ms() -> u64 {
    env::block_timestamp_ms()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(s: &str) -> Option<Vec<u8>> {
    if s.len() % 2 != 0 {
        return None;
    }
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok()).collect()
}

fn fail(code: &str, detail: impl AsRef<str>) -> ! {
    env::panic_str(&format!("{code}: {}", detail.as_ref()))
}

fn emit(event: &str, data: Value) {
    let line = json!({"standard": STANDARD, "version": "1.0.0", "event": event, "data": [data]});
    env::log_str(&format!("EVENT_JSON:{line}"));
}

fn ok(code: &str, task: Option<u64>) -> Res {
    Res { ok: true, code: code.into(), task, ..Default::default() }
}

fn no(code: &str, detail: impl Into<String>, task: Option<u64>) -> Res {
    let res = Res { ok: false, code: code.into(), detail: detail.into(), task, amount: None };
    emit("refused", json!(res));
    res
}

fn tagged<T: near_sdk::borsh::BorshSerialize>(tag: &[u8], body: &T) -> Vec<u8> {
    let mut out = tag.to_vec();
    out.extend(near_sdk::borsh::to_vec(body).expect("borsh"));
    out
}

/// The bytes a poster signs to post a task.
pub fn post_message(market: &AccountId, spec: &Spec) -> Vec<u8> {
    tagged(TAG_POST, &(market, spec))
}

/// The bytes a poster signs to accept a delivery.
pub fn accept_message(market: &AccountId, task_id: u64) -> Vec<u8> {
    tagged(TAG_ACCEPT, &(market, task_id))
}

/// The bytes a poster signs to reject a delivery.
pub fn reject_message(market: &AccountId, task_id: u64, reason_hash: &str) -> Vec<u8> {
    tagged(TAG_REJECT, &(market, task_id, reason_hash))
}

/// What a worker commits to. The hash covers the market, the task and the worker, so it cannot be
/// copied by another worker or moved to another task.
pub fn bid_commitment(market: &AccountId, task_id: u64, worker: &AccountId, price: u128, pk: &str, salt: &[u8; 32]) -> [u8; 32] {
    env::sha256_array(&tagged(TAG_BID, &(market, task_id, worker, price, pk, salt)))
}

fn verify(pk: &str, message: &[u8], signature_hex: &str) -> bool {
    let Ok(pk) = pk.parse::<PublicKey>() else { return false };
    if pk.curve_type() != CurveType::ED25519 {
        return false;
    }
    let Some(sig) = unhex(signature_hex).and_then(|b| <[u8; 64]>::try_from(b).ok()) else { return false };
    let key: [u8; 32] = pk.as_bytes()[1..].try_into().expect("ed25519 key");
    env::ed25519_verify(&sig, message, &key)
}

/// The vault's answer to `get_grant`. Err means the vault could not be read at all.
fn read_grant(index: u64) -> Result<Option<GrantLite>, ()> {
    let bytes = env::promise_result_checked(index, MAX_VIEW_BYTES as usize).map_err(|_| ())?;
    serde_json::from_slice::<Option<GrantLite>>(&bytes).map_err(|_| ())
}

fn view(vault: &AccountId, method: &str, args: Value) -> Promise {
    Promise::new(vault.clone()).function_call(method.to_string(), args.to_string().into_bytes(), NearToken::from_yoctonear(0), GAS_VIEW)
}

fn get_grant(vault: &AccountId, grant_id: u64) -> Promise {
    view(vault, "get_grant", json!({"grant_id": grant_id}))
}

#[near]
impl Market {
    #[init]
    pub fn new(vaults: Vec<AccountId>, poster_bond: U128, min_window_ms: u64, draw_grace_ms: u64) -> Self {
        require!(!vaults.is_empty() && vaults.len() <= MAX_VAULTS, "1 to 8 vaults");
        require!(!vaults.contains(&env::current_account_id()), "the market cannot be its own vault");
        require!(poster_bond.0 > 0, "poster bond must be positive");
        Self {
            vaults,
            poster_bond: poster_bond.0,
            min_window_ms,
            draw_grace_ms,
            next_task: 1,
            tasks: LookupMap::new(Key::Tasks),
            posted: LookupSet::new(Key::Posted),
            rosters: LookupMap::new(Key::Rosters),
            used_keys: LookupSet::new(Key::UsedKeys),
            grant_task: LookupMap::new(Key::GrantTask),
            posters: LookupMap::new(Key::Posters),
            seen: LookupMap::new(Key::Seen),
            balances: LookupMap::new(Key::Balances),
            bonds_held: 0,
            balances_total: 0,
            held_total: 0,
        }
    }

    // ---- The vault's owner names who may be hired with its money -------------------------------

    pub fn set_roster(&mut self, vault: AccountId, workers: Vec<AccountId>) -> Promise {
        if !self.vaults.contains(&vault) {
            fail("VAULT_NOT_LISTED", vault);
        }
        if workers.len() > MAX_ROSTER {
            fail("ROSTER_TOO_LONG", format!("{} > {MAX_ROSTER}", workers.len()));
        }
        view(&vault, "vault_info", json!({})).then(
            Self::ext(env::current_account_id())
                .with_static_gas(GAS_CALLBACK)
                .on_roster(env::predecessor_account_id(), vault, workers),
        )
    }

    #[private]
    pub fn on_roster(&mut self, caller: AccountId, vault: AccountId, workers: Vec<AccountId>) -> Res {
        let owner = env::promise_result_checked(0, 4096)
            .ok()
            .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
            .and_then(|v| v["owner"].as_str().map(str::to_string));
        match owner {
            None => no("VAULT_UNAVAILABLE", "no usable answer from the vault", None),
            Some(o) if o != caller.as_str() => no("NOT_OWNER", format!("{caller} does not own {vault}"), None),
            Some(_) => {
                emit("roster_set", json!({"vault": vault, "workers": workers}));
                self.rosters.insert(vault, workers);
                ok("ROSTER_SET", None)
            }
        }
    }

    // ---- Posting -------------------------------------------------------------------------------

    /// Posts a task. `signature` is hex ed25519 by the poster's grant key over `post_message`.
    /// Anyone may carry it here. The carrier attaches the poster bond and gets it back.
    #[payable]
    pub fn post_task(&mut self, spec: Spec, signature: String) -> Promise {
        let bond = env::attached_deposit().as_yoctonear();
        if bond != self.poster_bond {
            fail("WRONG_BOND", format!("{bond} attached, the poster bond is {}", self.poster_bond));
        }
        self.check_spec(&spec);
        get_grant(&spec.vault, spec.grant_id).then(
            Self::ext(env::current_account_id())
                .with_static_gas(GAS_CALLBACK)
                .on_post(spec, signature, env::predecessor_account_id(), U128(bond)),
        )
    }

    #[private]
    pub fn on_post(&mut self, spec: Spec, signature: String, payer: AccountId, bond: U128) -> Res {
        let message = post_message(&env::current_account_id(), &spec);
        let digest = env::sha256_array(&message);
        if let Err((code, detail)) = self.check_post(&spec, &message, &digest, &signature) {
            Promise::new(payer).transfer(NearToken::from_yoctonear(bond.0)).detach();
            return no(code, detail, None);
        }
        self.posted.insert(digest);
        let id = self.next_task;
        self.next_task += 1;
        let key = (spec.vault.clone(), spec.grant_id);
        let mut stat = self.posters.get(&key).cloned().unwrap_or_default();
        stat.posted += 1;
        stat.open += 1;
        stat.committed = U128(stat.committed.0 + spec.max_budget.0);
        self.posters.insert(key, stat);
        self.bonds_held += bond.0;
        emit("task_posted", json!({"task": id, "spec": spec, "bond_payer": payer}));
        self.tasks.insert(
            id,
            Task {
                id,
                spec,
                status: Status::Open,
                note: String::new(),
                posted_ms: now_ms(),
                bond_payer: payer,
                poster_bond: bond.0,
                bids: vec![],
                winner: None,
                price: 0,
                grant: None,
                result_hash: None,
                reject_hash: None,
                drawn: 0,
                paid: 0,
                held: 0,
                refunded: 0,
            },
        );
        ok("POSTED", Some(id))
    }

    // ---- Bidding -------------------------------------------------------------------------------

    /// A worker commits to a sealed bid and attaches the task's bid bond.
    #[payable]
    pub fn commit_bid(&mut self, task_id: u64, commitment: String) {
        let worker = env::predecessor_account_id();
        let mut task = self.task(task_id);
        if task.status != Status::Open || now_ms() >= task.spec.commit_end_ms {
            fail("WRONG_PHASE", "bids are committed before commit_end");
        }
        let bond = env::attached_deposit().as_yoctonear();
        if bond != task.spec.bid_bond.0 {
            fail("WRONG_BOND", format!("{bond} attached, the bid bond is {}", task.spec.bid_bond.0));
        }
        let roster = self.rosters.get(&task.spec.vault).cloned().unwrap_or_default();
        if !roster.contains(&worker) {
            fail("NOT_ON_ROSTER", format!("{worker} is not on the roster of {}", task.spec.vault));
        }
        if task.bids.iter().any(|b| b.worker == worker) {
            fail("ALREADY_BID", "one bid per worker per task");
        }
        if task.bids.len() >= MAX_BIDS {
            fail("TASK_FULL", format!("{MAX_BIDS} bids already"));
        }
        let Some(commitment) = unhex(&commitment).and_then(|b| <[u8; 32]>::try_from(b).ok()) else {
            fail("BAD_COMMITMENT", "64 hex characters expected");
        };
        self.bonds_held += bond;
        emit("bid_committed", json!({"task": task_id, "worker": worker, "commitment": hex(&commitment)}));
        task.bids.push(Bid { worker, commitment, bond, price: None, pk: None });
        self.tasks.insert(task_id, task);
    }

    /// Opens a bid. `pk` is the key the worker wants on its grant if it wins. The vault is asked
    /// whether that key is free, because a key that already belongs to a grant could never be awarded.
    pub fn reveal_bid(&mut self, task_id: u64, price: U128, pk: String, salt: String) -> Promise {
        let worker = env::predecessor_account_id();
        let task = self.task(task_id);
        let now = now_ms();
        if task.status != Status::Open || now < task.spec.commit_end_ms || now >= task.spec.reveal_end_ms {
            fail("WRONG_PHASE", "bids are revealed between commit_end and reveal_end");
        }
        let Some(bid) = task.bids.iter().find(|b| b.worker == worker) else {
            fail("NO_BID", format!("{worker} did not commit a bid"));
        };
        if bid.price.is_some() {
            fail("ALREADY_REVEALED", "this bid is already open");
        }
        let Some(salt) = unhex(&salt).and_then(|b| <[u8; 32]>::try_from(b).ok()) else {
            fail("BAD_REVEAL", "salt must be 64 hex characters");
        };
        if bid_commitment(&env::current_account_id(), task_id, &worker, price.0, &pk, &salt) != bid.commitment {
            fail("BAD_REVEAL", "price, key and salt do not match the commitment");
        }
        if price.0 == 0 || price.0 > task.spec.max_budget.0 {
            fail("PRICE_OUT_OF_RANGE", format!("{} is not between 1 and {}", price.0, task.spec.max_budget.0));
        }
        match pk.parse::<PublicKey>() {
            Ok(k) if k.curve_type() == CurveType::ED25519 => {}
            _ => fail("BAD_KEY", "an ed25519 public key is expected"),
        }
        if self.used_keys.contains(&pk) {
            fail("KEY_TAKEN", "this key is already bound to another bid");
        }
        view(&task.spec.vault, "grant_of_key", json!({"pk": pk})).then(
            Self::ext(env::current_account_id()).with_static_gas(GAS_CALLBACK).on_reveal(task_id, worker, price, pk),
        )
    }

    #[private]
    pub fn on_reveal(&mut self, task_id: u64, worker: AccountId, price: U128, pk: String) -> Res {
        let taken = match env::promise_result_checked(0, 256).ok().and_then(|b| serde_json::from_slice::<Option<u64>>(&b).ok()) {
            None => return no("VAULT_UNAVAILABLE", "no usable answer from the vault", Some(task_id)),
            Some(g) => g,
        };
        if let Some(g) = taken {
            return no("KEY_TAKEN", format!("this key already belongs to grant {g}"), Some(task_id));
        }
        let mut task = self.task(task_id);
        if task.status != Status::Open {
            return no("WRONG_PHASE", "bidding was closed before this reveal landed", Some(task_id));
        }
        if self.used_keys.contains(&pk) {
            return no("KEY_TAKEN", "this key is already bound to another bid", Some(task_id));
        }
        let bid = task.bids.iter_mut().find(|b| b.worker == worker).expect("bid checked");
        if bid.price.is_some() {
            return no("ALREADY_REVEALED", "this bid is already open", Some(task_id));
        }
        bid.price = Some(price.0);
        bid.pk = Some(pk.clone());
        self.used_keys.insert(pk.clone());
        emit("bid_revealed", json!({"task": task_id, "worker": worker, "price": price, "pk": pk}));
        self.tasks.insert(task_id, task);
        ok("REVEALED", Some(task_id))
    }

    /// Picks the winner once the reveal window is over. Anyone may call it. It runs once.
    pub fn close_bidding(&mut self, task_id: u64) -> Res {
        let mut task = self.task(task_id);
        if task.status != Status::Open || now_ms() < task.spec.reveal_end_ms {
            fail("WRONG_PHASE", "bidding closes after reveal_end, once");
        }
        let before = Self::commitment(&task);
        // A bid that was never opened loses its bond to the vault whose task it held up.
        let mut forfeited = 0;
        for b in task.bids.iter_mut().filter(|b| b.price.is_none()) {
            forfeited += std::mem::take(&mut b.bond);
        }
        if forfeited > 0 {
            self.bonds_held -= forfeited;
            self.credit(&task.spec.vault.clone(), forfeited);
        }
        let valid: Vec<(usize, u128)> = task.bids.iter().enumerate().filter_map(|(i, b)| b.price.map(|p| (i, p))).collect();
        let picked = pricing::pick(
            &valid.iter().map(|(i, p)| (*p, *i as u32)).collect::<Vec<_>>(),
            task.spec.max_budget.0,
            task.spec.min_bids,
        );
        let res = match picked {
            None => {
                for i in valid.iter().map(|(i, _)| *i) {
                    let bond = std::mem::take(&mut task.bids[i].bond);
                    self.bonds_held -= bond;
                    self.credit(&task.bids[i].worker.clone(), bond);
                }
                self.release_poster_bond(&mut task);
                task.status = Status::NoAward;
                task.note = format!("{} valid bids, {} needed", valid.len(), task.spec.min_bids.max(1));
                ok("NO_AWARD", Some(task_id))
            }
            Some((w, price)) => {
                let winner = valid[w].0;
                for i in valid.iter().map(|(i, _)| *i).filter(|i| *i != winner) {
                    let bond = std::mem::take(&mut task.bids[i].bond);
                    self.bonds_held -= bond;
                    self.credit(&task.bids[i].worker.clone(), bond);
                }
                task.winner = Some(winner as u32);
                task.price = price;
                task.status = Status::Awarding;
                Res { amount: Some(U128(price)), ..ok("WINNER", Some(task_id)) }
            }
        };
        emit(
            "bidding_closed",
            json!({
                "task": task_id, "status": task.status, "valid_bids": valid.len(), "forfeited": U128(forfeited),
                "winner": task.winner.map(|w| task.bids[w as usize].worker.clone()), "price": U128(task.price),
            }),
        );
        self.settle_stat(&task, before);
        self.tasks.insert(task_id, task);
        res
    }

    // ---- Award ---------------------------------------------------------------------------------

    /// Binds the grant the poster created in the vault to the task. Anyone may call it: the vault
    /// is read, and the grant either is the award or is not.
    pub fn confirm_award(&mut self, task_id: u64, grant_id: u64) -> Promise {
        let task = self.task(task_id);
        if task.status != Status::Awarding {
            fail("WRONG_PHASE", "there is no winner waiting for a grant");
        }
        get_grant(&task.spec.vault, grant_id)
            .then(Self::ext(env::current_account_id()).with_static_gas(GAS_CALLBACK).on_award(task_id, grant_id))
    }

    #[private]
    pub fn on_award(&mut self, task_id: u64, grant_id: u64) -> Res {
        let mut task = self.task(task_id);
        if task.status != Status::Awarding {
            return no("WRONG_PHASE", "there is no winner waiting for a grant", Some(task_id));
        }
        let g = match read_grant(0) {
            Err(()) => return no("VAULT_UNAVAILABLE", "no usable answer from the vault", Some(task_id)),
            Ok(None) => return no("UNKNOWN_GRANT", format!("grant {grant_id} does not exist"), Some(task_id)),
            Ok(Some(g)) => g,
        };
        if let Err((code, detail)) = self.check_award(&task, grant_id, &g) {
            return no(code, detail, Some(task_id));
        }
        let before = Self::commitment(&task);
        let key = (task.spec.vault.clone(), grant_id);
        self.grant_task.insert(key.clone(), task_id);
        // Anything this grant paid before it was bound is not part of the task.
        self.seen.insert(key, g.direct_spent.0);
        task.grant = Some(grant_id);
        task.drawn = g.spent.0.min(task.price);
        task.status = Status::Awarded;
        self.release_poster_bond(&mut task);
        emit(
            "award_confirmed",
            json!({"task": task_id, "grant": grant_id, "parent": task.spec.grant_id, "price": U128(task.price), "worker": task.bids[task.winner.unwrap() as usize].worker}),
        );
        self.settle_stat(&task, before);
        self.tasks.insert(task_id, task);
        ok("AWARDED", Some(task_id))
    }

    /// After `award_end` with no grant bound: finds out why, and settles the bonds.
    pub fn default_award(&mut self, task_id: u64) -> Promise {
        let task = self.task(task_id);
        if task.status != Status::Awarding || now_ms() < task.spec.award_end_ms {
            fail("WRONG_PHASE", "a missing award can be called after award_end");
        }
        let pk = task.bids[task.winner.unwrap() as usize].pk.clone().unwrap();
        get_grant(&task.spec.vault, task.spec.grant_id)
            .and(view(&task.spec.vault, "grant_of_key", json!({"pk": pk})))
            .then(Self::ext(env::current_account_id()).with_static_gas(Gas::from_tgas(60)).on_default(task_id))
    }

    #[private]
    pub fn on_default(&mut self, task_id: u64) -> PromiseOrValue<Res> {
        let mut task = self.task(task_id);
        if task.status != Status::Awarding {
            return PromiseOrValue::Value(no("WRONG_PHASE", "the award was settled meanwhile", Some(task_id)));
        }
        let poster = match read_grant(0) {
            Err(()) => return PromiseOrValue::Value(no("VAULT_UNAVAILABLE", "no usable answer from the vault", Some(task_id))),
            Ok(p) => p,
        };
        let holder = match env::promise_result_checked(1, 256).ok().and_then(|b| serde_json::from_slice::<Option<u64>>(&b).ok()) {
            None => return PromiseOrValue::Value(no("VAULT_UNAVAILABLE", "no usable answer from the vault", Some(task_id))),
            Some(h) => h,
        };
        if !poster.is_some_and(|p| p.live) {
            // The poster lost its authority, so it could not have created the grant. Nobody is at fault.
            return PromiseOrValue::Value(self.void(&mut task, "POSTER_NOT_LIVE"));
        }
        match holder {
            // The winner's key is free and the poster is live: the poster simply did not award.
            None => PromiseOrValue::Value(self.poster_defaulted(&mut task, "no grant was created for the winner")),
            // The key belongs to some grant. Whose fault depends on who created it.
            Some(g) => PromiseOrValue::Promise(
                get_grant(&task.spec.vault, g)
                    .then(Self::ext(env::current_account_id()).with_static_gas(GAS_CALLBACK).on_default_key(task_id, g)),
            ),
        }
    }

    #[private]
    pub fn on_default_key(&mut self, task_id: u64, grant_id: u64) -> Res {
        let mut task = self.task(task_id);
        if task.status != Status::Awarding {
            return no("WRONG_PHASE", "the award was settled meanwhile", Some(task_id));
        }
        match read_grant(0) {
            Err(()) | Ok(None) => no("VAULT_UNAVAILABLE", "no usable answer from the vault", Some(task_id)),
            // The poster created a grant for the winner's key, and it never passed `confirm_award`.
            Ok(Some(g)) if g.parent == Some(task.spec.grant_id) => {
                self.poster_defaulted(&mut task, &format!("grant {grant_id} was created with terms that are not the award"))
            }
            // Someone else bound the winner's key first. Neither the poster nor the winner could act.
            Ok(Some(_)) => self.void(&mut task, "KEY_TAKEN_BY_ANOTHER_GRANT"),
        }
    }

    // ---- Delivery and review -------------------------------------------------------------------

    pub fn deliver(&mut self, task_id: u64, result_hash: String) {
        let mut task = self.task(task_id);
        if task.status != Status::Awarded || now_ms() >= task.spec.deliver_end_ms {
            fail("WRONG_PHASE", "work is delivered after the award and before deliver_end");
        }
        if env::predecessor_account_id() != task.bids[task.winner.unwrap() as usize].worker {
            fail("NOT_WINNER", "only the winner delivers");
        }
        if result_hash.len() != 64 || unhex(&result_hash).is_none() {
            fail("BAD_HASH", "64 hex characters expected");
        }
        emit("delivered", json!({"task": task_id, "result_hash": result_hash}));
        task.result_hash = Some(result_hash);
        task.status = Status::Delivered;
        self.tasks.insert(task_id, task);
    }

    /// The poster accepts the delivery. `signature` is by the poster's grant key over `accept_message`.
    pub fn accept(&mut self, task_id: u64, signature: String) -> Promise {
        let task = self.task(task_id);
        if task.status != Status::Delivered {
            fail("WRONG_PHASE", "only a delivered task can be accepted");
        }
        get_grant(&task.spec.vault, task.spec.grant_id)
            .then(Self::ext(env::current_account_id()).with_static_gas(GAS_CALLBACK).on_review(task_id, true, String::new(), signature))
    }

    /// The poster rejects the delivery, before `review_end`. Silence after that counts as acceptance.
    pub fn reject(&mut self, task_id: u64, reason_hash: String, signature: String) -> Promise {
        let task = self.task(task_id);
        if task.status != Status::Delivered || now_ms() >= task.spec.review_end_ms {
            fail("WRONG_PHASE", "a delivery is rejected before review_end");
        }
        if reason_hash.len() != 64 || unhex(&reason_hash).is_none() {
            fail("BAD_HASH", "64 hex characters expected");
        }
        get_grant(&task.spec.vault, task.spec.grant_id)
            .then(Self::ext(env::current_account_id()).with_static_gas(GAS_CALLBACK).on_review(task_id, false, reason_hash, signature))
    }

    #[private]
    pub fn on_review(&mut self, task_id: u64, accepted: bool, reason_hash: String, signature: String) -> Res {
        let mut task = self.task(task_id);
        if task.status != Status::Delivered {
            return no("WRONG_PHASE", "the task is no longer waiting for review", Some(task_id));
        }
        let poster = match read_grant(0) {
            Err(()) => return no("VAULT_UNAVAILABLE", "no usable answer from the vault", Some(task_id)),
            Ok(None) => return no("UNKNOWN_GRANT", "the poster's grant does not exist", Some(task_id)),
            Ok(Some(p)) => p,
        };
        if !poster.live {
            return no("GRANT_NOT_LIVE", "the poster's grant is revoked or expired", Some(task_id));
        }
        let me = env::current_account_id();
        let message = if accepted { accept_message(&me, task_id) } else { reject_message(&me, task_id, &reason_hash) };
        if !verify(&poster.pk, &message, &signature) {
            return no("BAD_SIGNATURE", "not signed by the poster's grant key", Some(task_id));
        }
        if !accepted && now_ms() >= task.spec.review_end_ms {
            return no("WRONG_PHASE", "review_end passed before this rejection landed", Some(task_id));
        }
        let before = Self::commitment(&task);
        let key = (task.spec.vault.clone(), task.spec.grant_id);
        let mut stat = self.posters.get(&key).cloned().unwrap_or_default();
        self.release_winner_bond(&mut task, None);
        let res = if accepted {
            task.status = Status::Accepted;
            stat.accepted += 1;
            emit("accepted", json!({"task": task_id}));
            ok("ACCEPTED", Some(task_id))
        } else {
            task.status = Status::Rejected;
            task.note = "rejected by the poster".into();
            task.reject_hash = Some(reason_hash.clone());
            stat.rejected += 1;
            emit("rejected", json!({"task": task_id, "reason_hash": reason_hash}));
            ok("REJECTED", Some(task_id))
        };
        self.posters.insert(key, stat);
        self.settle_stat(&task, before);
        self.tasks.insert(task_id, task);
        res
    }

    /// After `deliver_end` with nothing delivered. If the winner still holds its grant, it had the
    /// means and did not deliver: its bond goes to the vault. If the grant is gone, the task is void.
    pub fn fail_delivery(&mut self, task_id: u64) -> Promise {
        let task = self.task(task_id);
        if task.status != Status::Awarded || now_ms() < task.spec.deliver_end_ms {
            fail("WRONG_PHASE", "a missed delivery can be called after deliver_end");
        }
        get_grant(&task.spec.vault, task.grant.unwrap())
            .then(Self::ext(env::current_account_id()).with_static_gas(GAS_CALLBACK).on_fail(task_id))
    }

    #[private]
    pub fn on_fail(&mut self, task_id: u64) -> Res {
        let mut task = self.task(task_id);
        if task.status != Status::Awarded {
            return no("WRONG_PHASE", "the task moved on meanwhile", Some(task_id));
        }
        let live = match read_grant(0) {
            Err(()) => return no("VAULT_UNAVAILABLE", "no usable answer from the vault", Some(task_id)),
            Ok(g) => g.is_some_and(|g| g.live),
        };
        if !live {
            return self.void(&mut task, "WINNER_NOT_LIVE");
        }
        let before = Self::commitment(&task);
        let vault = task.spec.vault.clone();
        self.release_winner_bond(&mut task, Some(&vault));
        task.status = Status::Failed;
        task.note = "nothing was delivered by deliver_end".into();
        emit("delivery_failed", json!({"task": task_id, "bond_to": vault}));
        self.settle_stat(&task, before);
        self.tasks.insert(task_id, task);
        ok("FAILED", Some(task_id))
    }

    // ---- Revocation ----------------------------------------------------------------------------

    /// Ends a task whose poster or winner lost its authority in the vault. Anyone may call it. The
    /// vault is asked, so a task with live grants cannot be voided. Bonds go back: being revoked
    /// is not a missed deadline.
    pub fn void_task(&mut self, task_id: u64) -> Promise {
        let task = self.task(task_id);
        if !matches!(task.status, Status::Open | Status::Awarding | Status::Awarded | Status::Delivered) {
            fail("WRONG_PHASE", "only a task in progress can be voided");
        }
        // A winner's grant is below the poster's, so it is not live if the poster's is not.
        let target = task.grant.unwrap_or(task.spec.grant_id);
        get_grant(&task.spec.vault, target)
            .then(Self::ext(env::current_account_id()).with_static_gas(GAS_CALLBACK).on_void(task_id, target))
    }

    #[private]
    pub fn on_void(&mut self, task_id: u64, target: u64) -> Res {
        let mut task = self.task(task_id);
        if !matches!(task.status, Status::Open | Status::Awarding | Status::Awarded | Status::Delivered)
            || task.grant.unwrap_or(task.spec.grant_id) != target
        {
            return no("WRONG_PHASE", "the task moved on meanwhile", Some(task_id));
        }
        match read_grant(0) {
            Err(()) => no("VAULT_UNAVAILABLE", "no usable answer from the vault", Some(task_id)),
            Ok(Some(g)) if g.live => no("STILL_LIVE", format!("grant {target} is live"), Some(task_id)),
            Ok(_) => {
                let reason = if task.grant.is_some() { "WINNER_NOT_LIVE" } else { "POSTER_NOT_LIVE" };
                self.void(&mut task, reason)
            }
        }
    }

    // ---- Money ---------------------------------------------------------------------------------

    /// Counts what the winner's grant has drawn into this contract and, once the work is accepted,
    /// forwards it to the winner, up to the price. Anyone may call it.
    pub fn claim(&mut self, task_id: u64) -> Promise {
        let task = self.task(task_id);
        let Some(grant) = task.grant else { fail("NO_GRANT", "this task has no awarded grant") };
        get_grant(&task.spec.vault, grant)
            .then(Self::ext(env::current_account_id()).with_static_gas(GAS_CALLBACK).on_claim(task_id))
    }

    #[private]
    pub fn on_claim(&mut self, task_id: u64) -> Res {
        let mut task = self.task(task_id);
        let grant = task.grant.expect("checked");
        let g = match read_grant(0) {
            Err(()) | Ok(None) => return no("VAULT_UNAVAILABLE", "no usable answer from the vault", Some(task_id)),
            Ok(Some(g)) => g,
        };
        let before = Self::commitment(&task);
        // What the winner and its own hires have spent is charged to the poster already, so it is
        // no longer a promise.
        task.drawn = g.spent.0.min(task.price);
        let key = (task.spec.vault.clone(), grant);
        let seen = self.seen.get(&key).copied().unwrap_or(0);
        let fresh = g.direct_spent.0.saturating_sub(seen);
        if fresh > 0 {
            self.seen.insert(key, g.direct_spent.0);
            task.held += fresh;
            self.held_total += fresh;
        }
        // Silence until review_end is acceptance.
        if task.status == Status::Delivered && now_ms() >= task.spec.review_end_ms {
            self.release_winner_bond(&mut task, None);
            task.status = Status::Accepted;
            task.note = "accepted by silence".into();
            let pkey = (task.spec.vault.clone(), task.spec.grant_id);
            let mut stat = self.posters.get(&pkey).cloned().unwrap_or_default();
            stat.accepted += 1;
            self.posters.insert(pkey, stat);
            emit("accepted", json!({"task": task_id, "by": "silence"}));
        }
        let mut pay = 0;
        if matches!(task.status, Status::Accepted | Status::Paid) {
            pay = task.held.min(task.price - task.paid);
            if pay > 0 {
                task.held -= pay;
                self.held_total -= pay;
                task.paid += pay;
                let worker = task.bids[task.winner.unwrap() as usize].worker.clone();
                self.credit(&worker, pay);
            }
            // Finished when nothing more can be drawn: the grant is used up, by the winner or by
            // its hires, or it is gone.
            if task.status == Status::Accepted && (task.drawn == task.price || !g.live) {
                task.status = Status::Paid;
            }
        }
        emit(
            "claim",
            json!({"task": task_id, "grant": grant, "counted": U128(fresh), "paid_now": U128(pay), "paid": U128(task.paid), "held": U128(task.held), "drawn": U128(task.drawn), "status": task.status}),
        );
        self.settle_stat(&task, before);
        self.tasks.insert(task_id, task);
        Res { amount: Some(U128(pay)), ..ok("CLAIMED", Some(task_id)) }
    }

    /// Sends what a task holds and can never pay out back to the vault it came from. Anyone may
    /// call it once the task is finished. It never goes anywhere but the vault account.
    pub fn refund(&mut self, task_id: u64) -> Res {
        let mut task = self.task(task_id);
        if !task.status.is_final() {
            fail("WRONG_PHASE", "a task in progress may still pay out what it holds");
        }
        let amount = std::mem::take(&mut task.held);
        if amount == 0 {
            fail("NOTHING_HELD", "this task holds nothing");
        }
        self.held_total -= amount;
        task.refunded += amount;
        let vault = task.spec.vault.clone();
        self.credit(&vault, amount);
        emit("refund", json!({"task": task_id, "vault": vault, "amount": U128(amount)}));
        self.tasks.insert(task_id, task);
        Res { amount: Some(U128(amount)), ..ok("REFUNDED", Some(task_id)) }
    }

    /// Sends back what a grant paid this contract outside any task, for example a root that
    /// deposited by mistake. Only for grants whose one recipient is this contract, because only
    /// then is `direct_spent` exactly what arrived here.
    pub fn return_unbound(&mut self, vault: AccountId, grant_id: u64) -> Promise {
        if !self.vaults.contains(&vault) {
            fail("VAULT_NOT_LISTED", vault);
        }
        if self.grant_task.contains_key(&(vault.clone(), grant_id)) {
            fail("GRANT_BOUND", "this grant belongs to a task: use claim and refund");
        }
        get_grant(&vault, grant_id)
            .then(Self::ext(env::current_account_id()).with_static_gas(GAS_CALLBACK).on_return_unbound(vault, grant_id))
    }

    #[private]
    pub fn on_return_unbound(&mut self, vault: AccountId, grant_id: u64) -> Res {
        let g = match read_grant(0) {
            Err(()) | Ok(None) => return no("UNKNOWN_GRANT", "the vault has no such grant", None),
            Ok(Some(g)) => g,
        };
        let key = (vault.clone(), grant_id);
        if self.grant_task.contains_key(&key) {
            return no("GRANT_BOUND", "this grant was bound to a task meanwhile", None);
        }
        if g.recipients.as_deref() != Some(&[env::current_account_id()][..]) {
            return no("RECIPIENTS_NOT_MARKET_ONLY", "this grant can pay others, so its payments here cannot be told apart", None);
        }
        let seen = self.seen.get(&key).copied().unwrap_or(0);
        let amount = g.direct_spent.0.saturating_sub(seen);
        if amount == 0 {
            return no("NOTHING_HELD", "nothing new arrived from this grant", None);
        }
        self.seen.insert(key, g.direct_spent.0);
        self.credit(&vault, amount);
        emit("returned_unbound", json!({"vault": vault, "grant": grant_id, "amount": U128(amount)}));
        Res { amount: Some(U128(amount)), ..ok("RETURNED", None) }
    }

    /// Pays an account what this contract owes it. Anyone may call it: the money goes to the
    /// account itself. This is how a vault receives forfeited bonds and refunds.
    pub fn payout(&mut self, account_id: AccountId) -> Promise {
        let amount = self.balances.get(&account_id).copied().unwrap_or(0);
        if amount == 0 {
            fail("NOTHING_OWED", account_id);
        }
        self.balances.remove(&account_id);
        self.balances_total -= amount;
        emit("payout", json!({"account": account_id, "amount": U128(amount)}));
        Promise::new(account_id.clone())
            .transfer(NearToken::from_yoctonear(amount))
            .then(Self::ext(env::current_account_id()).with_static_gas(GAS_PAYOUT_CALLBACK).on_payout(account_id, U128(amount)))
    }

    #[private]
    pub fn on_payout(&mut self, account_id: AccountId, amount: U128) -> bool {
        if matches!(env::promise_result_checked(0, 16), Err(near_sdk::PromiseError::Failed)) {
            self.credit(&account_id, amount.0);
            emit("payout_failed", json!({"account": account_id, "amount": amount}));
            return false;
        }
        true
    }

    // ---- Views ---------------------------------------------------------------------------------

    pub fn get_task(&self, task_id: u64) -> Option<TaskView> {
        self.tasks.get(&task_id).map(|t| self.task_view(t))
    }

    pub fn get_tasks(&self, from: u64, limit: u64) -> Vec<TaskView> {
        (from.max(1)..self.next_task).take(limit.min(50) as usize).filter_map(|id| self.get_task(id)).collect()
    }

    pub fn task_of_grant(&self, vault: AccountId, grant_id: u64) -> Option<u64> {
        self.grant_task.get(&(vault, grant_id)).copied()
    }

    pub fn get_roster(&self, vault: AccountId) -> Option<Vec<AccountId>> {
        self.rosters.get(&vault).cloned()
    }

    pub fn get_poster(&self, vault: AccountId, grant_id: u64) -> PosterStat {
        self.posters.get(&(vault, grant_id)).cloned().unwrap_or_default()
    }

    pub fn balance_of(&self, account_id: AccountId) -> U128 {
        U128(self.balances.get(&account_id).copied().unwrap_or(0))
    }

    pub fn config(&self) -> Config {
        Config {
            vaults: self.vaults.clone(),
            poster_bond: U128(self.poster_bond),
            min_window_ms: self.min_window_ms,
            draw_grace_ms: self.draw_grace_ms,
            tasks: self.next_task - 1,
            now_ms: now_ms(),
        }
    }

    /// What this contract owes against what it holds. `balance` is the whole account balance, which
    /// also pays for storage, so `balance - owed` is not all free.
    pub fn solvency(&self) -> Solvency {
        Solvency {
            bonds_held: U128(self.bonds_held),
            balances: U128(self.balances_total),
            held: U128(self.held_total),
            owed: U128(self.bonds_held + self.balances_total + self.held_total),
            balance: U128(env::account_balance().as_yoctonear()),
        }
    }

    /// Hex of the exact bytes a poster signs. A client can build them itself, see `post_message`.
    pub fn post_payload(&self, spec: Spec) -> String {
        hex(&post_message(&env::current_account_id(), &spec))
    }

    pub fn accept_payload(&self, task_id: u64) -> String {
        hex(&accept_message(&env::current_account_id(), task_id))
    }

    pub fn reject_payload(&self, task_id: u64, reason_hash: String) -> String {
        hex(&reject_message(&env::current_account_id(), task_id, &reason_hash))
    }

    pub fn commitment_of(&self, task_id: u64, worker: AccountId, price: U128, pk: String, salt: String) -> String {
        let salt: [u8; 32] = unhex(&salt).and_then(|b| b.try_into().ok()).unwrap_or_else(|| fail("BAD_REVEAL", "salt must be 64 hex characters"));
        hex(&bid_commitment(&env::current_account_id(), task_id, &worker, price.0, &pk, &salt))
    }

    /// The pricing rule on its own, for clients and for the live page to check against.
    pub fn price_of(&self, bids: Vec<(U128, u32)>, max_budget: U128, min_bids: u8) -> Option<(u32, U128)> {
        let raw: Vec<(u128, u32)> = bids.iter().map(|(p, s)| (p.0, *s)).collect();
        pricing::pick(&raw, max_budget.0, min_bids).map(|(i, p)| (i as u32, U128(p)))
    }
}

// ---- Internals ----------------------------------------------------------------------------------

impl Market {
    fn task(&self, id: u64) -> Task {
        self.tasks.get(&id).cloned().unwrap_or_else(|| fail("UNKNOWN_TASK", format!("no task {id}")))
    }

    fn credit(&mut self, account: &AccountId, amount: u128) {
        if amount == 0 {
            return;
        }
        let now = self.balances.get(account).copied().unwrap_or(0) + amount;
        self.balances.insert(account.clone(), now);
        self.balances_total += amount;
    }

    /// Budget a task still claims on its poster: the whole maximum until a winner is known, then
    /// the price less what the winner's branch has already spent. Nothing once the task is final.
    fn commitment(task: &Task) -> (bool, u128) {
        match task.status {
            Status::Open => (true, task.spec.max_budget.0),
            Status::Awarding | Status::Awarded | Status::Delivered | Status::Accepted => (true, task.price - task.drawn),
            _ => (false, 0),
        }
    }

    /// Brings the poster's open count and committed budget in line with the task's new state.
    fn settle_stat(&mut self, task: &Task, before: (bool, u128)) {
        let after = Self::commitment(task);
        if after == before {
            return;
        }
        let key = (task.spec.vault.clone(), task.spec.grant_id);
        let mut stat = self.posters.get(&key).cloned().unwrap_or_default();
        stat.committed = U128(stat.committed.0 - before.1 + after.1);
        if before.0 && !after.0 {
            stat.open -= 1;
        }
        self.posters.insert(key, stat);
    }

    fn release_poster_bond(&mut self, task: &mut Task) {
        let bond = std::mem::take(&mut task.poster_bond);
        self.bonds_held -= bond;
        let payer = task.bond_payer.clone();
        self.credit(&payer, bond);
    }

    /// Returns the winner's bond to the winner, or hands it to `to`.
    fn release_winner_bond(&mut self, task: &mut Task, to: Option<&AccountId>) {
        let Some(w) = task.winner else { return };
        let bond = std::mem::take(&mut task.bids[w as usize].bond);
        self.bonds_held -= bond;
        let account = to.cloned().unwrap_or_else(|| task.bids[w as usize].worker.clone());
        self.credit(&account, bond);
    }

    /// Ends a task with nobody at fault: every bond still held goes back to who posted it.
    fn void(&mut self, task: &mut Task, reason: &str) -> Res {
        let before = Self::commitment(task);
        for i in 0..task.bids.len() {
            let bond = std::mem::take(&mut task.bids[i].bond);
            if bond > 0 {
                self.bonds_held -= bond;
                let worker = task.bids[i].worker.clone();
                self.credit(&worker, bond);
            }
        }
        self.release_poster_bond(task);
        task.status = Status::Void;
        task.note = reason.to_string();
        emit("task_void", json!({"task": task.id, "reason": reason}));
        self.settle_stat(task, before);
        self.tasks.insert(task.id, task.clone());
        Res { detail: reason.to_string(), ..ok("VOID", Some(task.id)) }
    }

    /// The poster had the means to award and did not. Its bond goes to the winner it let down.
    fn poster_defaulted(&mut self, task: &mut Task, why: &str) -> Res {
        let before = Self::commitment(task);
        let worker = task.bids[task.winner.unwrap() as usize].worker.clone();
        let bond = std::mem::take(&mut task.poster_bond);
        self.bonds_held -= bond;
        self.credit(&worker, bond);
        self.release_winner_bond(task, None);
        task.status = Status::Defaulted;
        task.note = why.to_string();
        let key = (task.spec.vault.clone(), task.spec.grant_id);
        let mut stat = self.posters.get(&key).cloned().unwrap_or_default();
        stat.defaults += 1;
        self.posters.insert(key, stat);
        emit("award_defaulted", json!({"task": task.id, "poster_bond_to": worker, "why": why}));
        self.settle_stat(task, before);
        self.tasks.insert(task.id, task.clone());
        Res { detail: why.to_string(), ..ok("DEFAULTED", Some(task.id)) }
    }

    /// Checks that need no vault: shape, amounts and windows.
    fn check_spec(&self, s: &Spec) {
        if !self.vaults.contains(&s.vault) {
            fail("VAULT_NOT_LISTED", &s.vault);
        }
        if !self.rosters.contains_key(&s.vault) {
            fail("NO_ROSTER", "the vault's owner has not named any workers");
        }
        if s.spec_hash.len() != 64 || unhex(&s.spec_hash).is_none() {
            fail("BAD_HASH", "spec_hash must be 64 hex characters");
        }
        if s.max_budget.0 == 0 || s.bid_bond.0 == 0 {
            fail("BAD_SPEC", "max_budget and bid_bond must be positive");
        }
        let w = self.min_window_ms;
        let steps = [now_ms(), s.commit_end_ms, s.reveal_end_ms, s.award_end_ms, s.deliver_end_ms, s.review_end_ms];
        if steps.windows(2).any(|p| p[1] < p[0].saturating_add(w)) {
            fail("BAD_WINDOWS", format!("each window must last at least {w} ms, in order"));
        }
    }

    /// Checks against the poster's grant as the vault reports it.
    fn check_post(&self, s: &Spec, message: &[u8], digest: &[u8; 32], signature: &str) -> Result<(), (&'static str, String)> {
        let g = match read_grant(0) {
            Err(()) => return Err(("VAULT_UNAVAILABLE", "no usable answer from the vault".into())),
            Ok(None) => return Err(("UNKNOWN_GRANT", format!("grant {} does not exist", s.grant_id))),
            Ok(Some(g)) => g,
        };
        if self.posted.contains(digest) {
            return Err(("DUPLICATE_TASK", "this signed post was already used".into()));
        }
        if now_ms() >= s.commit_end_ms {
            return Err(("BAD_WINDOWS", "commit_end passed before the post landed".into()));
        }
        if !g.live {
            return Err(("GRANT_NOT_LIVE", format!("grant {} is revoked or expired, or an ancestor is", s.grant_id)));
        }
        if !verify(&g.pk, message, signature) {
            return Err(("BAD_SIGNATURE", "not signed by the poster's grant key".into()));
        }
        self.check_grant_shape(&g)?;
        if !g.methods.iter().any(|m| m == "delegate") {
            return Err(("CANNOT_DELEGATE", "the poster's key cannot create a grant".into()));
        }
        if g.max_depth < s.sub_depth.saturating_add(1) {
            return Err(("DEPTH_SHORT", format!("poster has {} levels left, the task needs {}", g.max_depth, s.sub_depth as u32 + 1)));
        }
        if g.nodes_left < s.sub_nodes.saturating_add(1) {
            return Err(("NODES_SHORT", format!("poster can create {} more grants, the task needs {}", g.nodes_left, s.sub_nodes as u64 + 1)));
        }
        if g.gas_left.0 < s.key_allowance.0.saturating_add(s.sub_gas.0) {
            return Err(("GAS_SHORT", "the poster's gas budget cannot cover the winner's key and gas budget".into()));
        }
        if g.max_denials < MIN_DENIALS {
            return Err(("DENIALS_SHORT", format!("poster's denial limit {} is under {MIN_DENIALS}", g.max_denials)));
        }
        if g.expires_at_ms < s.review_end_ms.saturating_add(self.draw_grace_ms) {
            return Err(("EXPIRES_TOO_SOON", "the poster's grant expires before the winner could draw".into()));
        }
        let max = s.max_budget.0;
        if g.per_tx.0 < max || g.window_cap.0 < max || g.approval_threshold.0 < max {
            return Err(("LIMITS_SHORT", "the poster's per-transaction, window or approval limit is under max_budget".into()));
        }
        let stat = self.posters.get(&(s.vault.clone(), s.grant_id)).cloned().unwrap_or_default();
        if stat.defaults >= MAX_DEFAULTS {
            return Err(("POSTER_BANNED", format!("{} awards were not made", stat.defaults)));
        }
        if stat.open >= MAX_OPEN_TASKS {
            return Err(("TOO_MANY_OPEN", format!("{MAX_OPEN_TASKS} tasks already open")));
        }
        if g.spendable.0 < stat.committed.0.saturating_add(max) {
            return Err((
                "OVERCOMMITTED",
                format!("{} spendable, {} already promised, {} asked", g.spendable.0, stat.committed.0, max),
            ));
        }
        Ok(())
    }

    /// What every grant in the chain must be: native NEAR, payable only to this contract, and not
    /// waiting on a clearance ticket.
    fn check_grant_shape(&self, g: &GrantLite) -> Result<(), (&'static str, String)> {
        if g.asset != json!("Native") {
            return Err(("WRONG_ASSET", "only native NEAR grants".into()));
        }
        if g.recipients.as_deref() != Some(&[env::current_account_id()][..]) {
            return Err(("RECIPIENTS_NOT_MARKET_ONLY", "the grant's only recipient must be this contract".into()));
        }
        if g.clearance_required {
            return Err(("CLEARANCE_BOUND", "the grant needs a clearance ticket for every spend".into()));
        }
        Ok(())
    }

    /// Is this grant the award the task promised its winner?
    fn check_award(&self, task: &Task, grant_id: u64, g: &GrantLite) -> Result<(), (&'static str, String)> {
        let s = &task.spec;
        let bid = &task.bids[task.winner.expect("winner") as usize];
        if g.parent != Some(s.grant_id) {
            return Err(("WRONG_PARENT", format!("grant {grant_id} is not a child of grant {}", s.grant_id)));
        }
        if Some(&g.pk) != bid.pk.as_ref() {
            return Err(("WRONG_KEY", "the grant's key is not the key in the winning bid".into()));
        }
        if !g.live || !g.key_live {
            return Err(("GRANT_NOT_LIVE", format!("grant {grant_id} is not live")));
        }
        self.check_grant_shape(g)?;
        if g.cap.0 != task.price {
            return Err(("WRONG_CAP", format!("cap {} is not the price {}", g.cap.0, task.price)));
        }
        if g.per_tx.0 != task.price || g.window_cap.0 != task.price || g.approval_threshold.0 != task.price {
            return Err(("WRONG_LIMITS", "per-transaction, window and approval limits must equal the price".into()));
        }
        let mut need = vec!["request_spend"];
        if s.sub_depth > 0 {
            need.extend(["delegate", "revoke"]);
        }
        if let Some(missing) = need.iter().find(|m| !g.methods.iter().any(|x| x == *m)) {
            return Err(("METHODS_SHORT", format!("the grant's key cannot call {missing}")));
        }
        if g.max_depth < s.sub_depth {
            return Err(("DEPTH_SHORT", format!("{} levels, the task promised {}", g.max_depth, s.sub_depth)));
        }
        if g.nodes_left < s.sub_nodes {
            return Err(("NODES_SHORT", format!("{} nodes, the task promised {}", g.nodes_left, s.sub_nodes)));
        }
        if g.gas_left.0 < s.sub_gas.0 || g.key_allowance.0 < s.key_allowance.0 {
            return Err(("GAS_SHORT", "gas budget or key allowance under what the task promised".into()));
        }
        if g.max_denials < MIN_DENIALS {
            return Err(("DENIALS_SHORT", format!("denial limit {} is under {MIN_DENIALS}", g.max_denials)));
        }
        if g.expires_at_ms < s.review_end_ms.saturating_add(self.draw_grace_ms) {
            return Err(("EXPIRES_TOO_SOON", "the grant expires before the winner could draw".into()));
        }
        if self.grant_task.contains_key(&(s.vault.clone(), grant_id)) {
            return Err(("GRANT_BOUND", format!("grant {grant_id} already belongs to a task")));
        }
        Ok(())
    }

    fn task_view(&self, t: &Task) -> TaskView {
        let now = now_ms();
        let phase = match t.status {
            Status::Open if now < t.spec.commit_end_ms => "commit".to_string(),
            Status::Open if now < t.spec.reveal_end_ms => "reveal".to_string(),
            Status::Open => "close".to_string(),
            other => format!("{other:?}").to_lowercase(),
        };
        TaskView {
            id: t.id,
            spec: t.spec.clone(),
            status: t.status,
            phase,
            note: t.note.clone(),
            posted_ms: t.posted_ms,
            bond_payer: t.bond_payer.clone(),
            poster_bond: U128(t.poster_bond),
            bids: t
                .bids
                .iter()
                .map(|b| BidView {
                    worker: b.worker.clone(),
                    commitment: hex(&b.commitment),
                    bond: U128(b.bond),
                    revealed: b.price.is_some(),
                    price: b.price.map(U128),
                    pk: b.pk.clone(),
                })
                .collect(),
            winner: t.winner.map(|w| t.bids[w as usize].worker.clone()),
            winner_pk: t.winner.and_then(|w| t.bids[w as usize].pk.clone()),
            price: U128(t.price),
            grant: t.grant,
            result_hash: t.result_hash.clone(),
            reject_hash: t.reject_hash.clone(),
            drawn: U128(t.drawn),
            paid: U128(t.paid),
            held: U128(t.held),
            refunded: U128(t.refunded),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec() -> Spec {
        Spec {
            vault: "vault.test.near".parse().unwrap(),
            grant_id: 7,
            spec_hash: "ab".repeat(32),
            max_budget: U128(100),
            bid_bond: U128(5),
            min_bids: 2,
            commit_end_ms: 1_000,
            reveal_end_ms: 2_000,
            award_end_ms: 3_000,
            deliver_end_ms: 4_000,
            review_end_ms: 5_000,
            sub_depth: 1,
            sub_nodes: 3,
            sub_gas: U128(9),
            key_allowance: U128(11),
            nonce: 42,
        }
    }

    /// The layout a client has to reproduce to sign a post: tag, then borsh of (market, spec).
    #[test]
    fn post_message_layout() {
        let market: AccountId = "market.test.near".parse().unwrap();
        let got = post_message(&market, &spec());
        let mut want = TAG_POST.to_vec();
        let s = |out: &mut Vec<u8>, x: &str| {
            out.extend((x.len() as u32).to_le_bytes());
            out.extend(x.as_bytes());
        };
        s(&mut want, "market.test.near");
        s(&mut want, "vault.test.near");
        want.extend(7u64.to_le_bytes());
        s(&mut want, &"ab".repeat(32));
        want.extend(100u128.to_le_bytes());
        want.extend(5u128.to_le_bytes());
        want.push(2);
        for t in [1_000u64, 2_000, 3_000, 4_000, 5_000] {
            want.extend(t.to_le_bytes());
        }
        want.push(1);
        want.extend(3u32.to_le_bytes());
        want.extend(9u128.to_le_bytes());
        want.extend(11u128.to_le_bytes());
        want.extend(42u64.to_le_bytes());
        assert_eq!(got, want);
        // The Python client asserts the same hash for the same spec, in client/tests/test_market.py.
        assert_eq!(hex(&near_sdk::env::sha256(&got)), "08a724c89a95c3591d5e913e127f254566f6dc42685e98ce21d17778f5a0729b");
    }

    #[test]
    fn review_messages_differ_by_tag_task_and_reason() {
        let market: AccountId = "market.test.near".parse().unwrap();
        assert_ne!(accept_message(&market, 1), accept_message(&market, 2));
        assert_ne!(accept_message(&market, 1), reject_message(&market, 1, ""));
        assert_ne!(reject_message(&market, 1, "aa"), reject_message(&market, 1, "ab"));
        let other: AccountId = "other.test.near".parse().unwrap();
        assert_ne!(accept_message(&market, 1), accept_message(&other, 1));
    }

    #[test]
    fn commitment_keeps_open_tasks_and_budget_in_step() {
        let mut t = Task {
            id: 1,
            spec: spec(),
            status: Status::Open,
            note: String::new(),
            posted_ms: 0,
            bond_payer: "p.test.near".parse().unwrap(),
            poster_bond: 0,
            bids: vec![],
            winner: None,
            price: 0,
            grant: None,
            result_hash: None,
            reject_hash: None,
            drawn: 0,
            paid: 0,
            held: 0,
            refunded: 0,
        };
        assert_eq!(Market::commitment(&t), (true, 100));
        t.status = Status::Awarded;
        t.price = 60;
        assert_eq!(Market::commitment(&t), (true, 60));
        t.status = Status::Accepted;
        t.paid = 10;
        t.drawn = 25;
        assert_eq!(Market::commitment(&t), (true, 35));
        for s in [Status::Paid, Status::Rejected, Status::Failed, Status::NoAward, Status::Defaulted, Status::Void] {
            t.status = s;
            assert_eq!(Market::commitment(&t), (false, 0));
            assert!(s.is_final());
        }
    }
}
