//! Randomised model test, in process. This is the real contract code running on near-sdk's
//! MockedBlockchain, with state written and read back around every call the way the wasm entry
//! points do it. It is not the NEAR runtime: receipts and promise results are driven by this file.
//! `harness sandbox-property` runs the same kind of workload on a real node.
//!
//! The oracle below keeps its own ledger and shares no accounting code with the contract.
//! After every operation it asserts:
//!   I1 spent + reserved <= cap on every grant
//!   I2 each grant's counters equal its direct counters plus its children's counters
//!   I3 each grant's counters equal the oracle's ledger for that subtree
//!   I4 no window-length interval of non-rolled-back spends exceeds any lineage window cap
//!   I5 a denied operation changes no financial state
//!   I6 no spend is ever reserved once the oracle has seen the grant or an ancestor revoked
//!   I7 every delegation the contract accepts is narrower than its parent, and every delegation
//!      widened in exactly one dimension is refused with that dimension's code
//!   I8 when everything has settled, nothing is reserved and nothing is in flight
//!   I9 funds that left the vault equal the settled total; the vault never pays more than it holds
//!
//! Env: PROPERTY_SEEDS, PROPERTY_OPS, PROPERTY_REPORT (path of the JSON report).

use super::*;
use near_sdk::test_utils::VMContextBuilder;
use near_sdk::{test_vm_config, testing_env, CurveType, PromiseResult, RuntimeFeesConfig};
use rand::rngs::SmallRng;
use rand::{Rng, SeedableRng};
use std::collections::{BTreeMap, HashMap, HashSet};

const NATIVE_START: u128 = 2_000_000_000_000_000_000_000_000_000;

fn acct(s: &str) -> AccountId {
    s.parse().unwrap()
}
fn vault_id() -> AccountId {
    acct("vault.test")
}
fn owner_id() -> AccountId {
    acct("owner.test")
}
fn token_id() -> AccountId {
    acct("token.test")
}
fn recipients_pool() -> Vec<AccountId> {
    (0..12).map(|i| acct(&format!("merchant-{i}.test"))).collect()
}

fn pk_from(n: u64) -> PublicKey {
    let mut bytes = vec![0u8; 32];
    bytes[..8].copy_from_slice(&n.to_le_bytes());
    PublicKey::from_parts(CurveType::ED25519, bytes).unwrap()
}

#[derive(Clone)]
struct Node {
    parent: Option<GrantId>,
    terms: Terms,
    pk: PublicKey,
    dead: bool,
    denied: u32,
    settled: u128,
    reserved: u128,
    /// (time, amount, spend id) of every spend that has not rolled back.
    log: Vec<(u64, u128, u64)>,
}

#[derive(Clone, Copy, PartialEq)]
enum Fate {
    InFlight,
    Settled,
    RolledBack,
}

struct Spend {
    id: u64,
    leaf: GrantId,
    amount: u128,
    native: bool,
    fate: Fate,
}

#[derive(Default)]
struct Stats {
    ops: u64,
    by_op: BTreeMap<String, u64>,
    by_code: BTreeMap<String, u64>,
    window_conservative: u64,
    max_depth_seen: u8,
    max_nodes: usize,
    invariant_checks: u64,
}

struct Sim {
    rng: SmallRng,
    now: u64,
    balance: u128,
    nodes: BTreeMap<GrantId, Node>,
    spends: Vec<Spend>,
    next_pk: u64,
    blocked: HashSet<AccountId>,
    paused: bool,
    native_out: u128,
    stats: Stats,
    /// approval id -> (grant, recipient, amount, hash, approved)
    queue: BTreeMap<u64, (GrantId, AccountId, u128, String, bool)>,
}

enum Who {
    Owner,
    Key(PublicKey),
    Callback,
}

impl Sim {
    fn new(seed: u64) -> Self {
        // testing_env! carries storage over, so each seed has to start from an empty trie.
        testing_env!(VMContextBuilder::new().build());
        near_sdk::mock::with_mocked_blockchain(|b| drop(b.take_storage()));
        let mut sim = Sim {
            rng: SmallRng::seed_from_u64(seed),
            now: 1_000_000,
            balance: NATIVE_START,
            nodes: BTreeMap::new(),
            spends: vec![],
            next_pk: seed * 1_000_000 + 1,
            blocked: HashSet::new(),
            paused: false,
            native_out: 0,
            stats: Stats::default(),
            queue: BTreeMap::new(),
        };
        sim.context(Who::Owner, vec![]);
        let mut vault = Vault::new(owner_id(), U128(0), 300);
        vault.tokens.insert(token_id(), 0);
        env::state_write(&vault);
        drop(vault);
        sim.call(Who::Owner, vec![], |v| {
            v.ft_credit_for_test(50_000_000);
        });
        sim
    }

    fn context(&self, who: Who, results: Vec<PromiseResult>) {
        let (pred, signer, pk) = match who {
            Who::Owner => (owner_id(), owner_id(), pk_from(u64::MAX)),
            Who::Key(pk) => (vault_id(), vault_id(), pk),
            Who::Callback => (vault_id(), vault_id(), pk_from(u64::MAX - 1)),
        };
        let ctx = VMContextBuilder::new()
            .current_account_id(vault_id())
            .signer_account_id(signer)
            .signer_account_pk(pk)
            .predecessor_account_id(pred)
            .block_timestamp(self.now * 1_000_000)
            .account_balance(NearToken::from_yoctonear(self.balance))
            .prepaid_gas(Gas::from_tgas(300))
            .build();
        testing_env!(ctx, test_vm_config(), RuntimeFeesConfig::test(), HashMap::default(), results);
    }

    /// One contract call with the same life cycle as a wasm entry point: read state, run, write.
    fn call<T>(&mut self, who: Who, results: Vec<PromiseResult>, f: impl FnOnce(&mut Vault) -> T) -> T {
        self.context(who, results);
        let mut vault: Vault = env::state_read().expect("state");
        let out = f(&mut vault);
        env::state_write(&vault);
        out
    }

    fn view<T>(&mut self, f: impl FnOnce(&Vault) -> T) -> T {
        self.context(Who::Owner, vec![]);
        let vault: Vault = env::state_read().expect("state");
        f(&vault)
    }

    fn fresh_pk(&mut self) -> PublicKey {
        self.next_pk += 1;
        pk_from(self.next_pk)
    }

    fn lineage(&self, id: GrantId) -> Vec<GrantId> {
        let mut out = vec![];
        let mut cur = Some(id);
        while let Some(i) = cur {
            out.push(i);
            cur = self.nodes[&i].parent;
        }
        out
    }

    fn mark_subtree_dead(&mut self, id: GrantId) {
        let ids: Vec<GrantId> = self.nodes.keys().copied().collect();
        for n in ids {
            if self.lineage(n).contains(&id) {
                self.nodes.get_mut(&n).unwrap().dead = true;
            }
        }
    }

    fn model_dead(&self, id: GrantId) -> bool {
        self.lineage(id).iter().any(|i| {
            let n = &self.nodes[i];
            n.dead || self.now >= n.terms.expires_at_ms
        })
    }

    fn window_sum(&self, id: GrantId, from_exclusive: u64) -> u128 {
        self.nodes[&id].log.iter().filter(|(t, _, _)| *t > from_exclusive).map(|(_, a, _)| *a).sum()
    }

    /// Any state that a refusal must leave alone. Denial bookkeeping is blanked out on purpose:
    /// the counter, and the revoked and key flags it can flip, are the point of not panicking.
    fn financial_snapshot(&mut self) -> Vec<u8> {
        let ids: Vec<GrantId> = self.nodes.keys().copied().collect();
        let queue: Vec<u64> = self.queue.keys().copied().collect();
        let spends: Vec<u64> = self.spends.iter().map(|s| s.id).collect();
        self.view(|v| {
            let mut out = vec![];
            for id in &ids {
                let mut g = v.grants.get(id).unwrap().clone();
                g.denied = 0;
                g.revoked = false;
                g.key_live = false;
                out.extend(near_sdk::borsh::to_vec(&g).unwrap());
                out.extend(near_sdk::borsh::to_vec(&v.allowlists.get(id)).unwrap());
            }
            for id in &queue {
                out.extend(near_sdk::borsh::to_vec(&v.approvals.get(id)).unwrap());
            }
            for id in &spends {
                out.extend(near_sdk::borsh::to_vec(&v.pending.get(id)).unwrap());
            }
            out.extend(v.next_nonce.to_le_bytes());
            out.extend(v.next_grant.to_le_bytes());
            out.extend(v.in_flight.to_le_bytes());
            out.extend(v.tokens.get(&token_id()).copied().unwrap_or(0).to_le_bytes());
            out
        })
    }

    fn check_invariants(&mut self) {
        let ids: Vec<GrantId> = self.nodes.keys().copied().collect();
        let nodes = self.nodes.clone();
        self.stats.invariant_checks += 1;
        self.view(|v| {
            let mut sum_spent: HashMap<GrantId, u128> = HashMap::new();
            let mut sum_reserved: HashMap<GrantId, u128> = HashMap::new();
            for id in &ids {
                let g = v.grants.get(id).unwrap();
                if let Some(p) = g.parent {
                    *sum_spent.entry(p).or_default() += g.spent;
                    *sum_reserved.entry(p).or_default() += g.reserved;
                }
            }
            for id in &ids {
                let g = v.grants.get(id).unwrap();
                let n = &nodes[id];
                assert!(g.spent + g.reserved <= g.cap, "I1 grant {id}");
                assert_eq!(
                    g.spent,
                    g.direct_spent + sum_spent.get(id).copied().unwrap_or(0),
                    "I2 spent conservation on grant {id}"
                );
                assert_eq!(
                    g.reserved,
                    g.direct_reserved + sum_reserved.get(id).copied().unwrap_or(0),
                    "I2 reserved conservation on grant {id}"
                );
                assert_eq!(g.spent, n.settled, "I3 settled on grant {id}");
                assert_eq!(g.reserved, n.reserved, "I3 reserved on grant {id}");
            }
        });
    }

    fn note_denial(&mut self, grant: Option<GrantId>, code: &str) {
        *self.stats.by_code.entry(code.to_string()).or_default() += 1;
        let Some(id) = grant else { return };
        if matches!(code, "PAUSED" | "ALREADY_REVOKED") {
            return;
        }
        let n = self.nodes.get_mut(&id).unwrap();
        n.denied += 1;
        let (denied, max) = (n.denied, n.terms.max_denials);
        if denied >= max {
            self.mark_subtree_dead(id);
        }
    }

    fn pick_actor(&mut self) -> Option<GrantId> {
        let live: Vec<GrantId> = {
            let ids: Vec<GrantId> = self.nodes.keys().copied().collect();
            self.view(|v| ids.into_iter().filter(|id| v.grants.get(id).unwrap().key_live).collect())
        };
        if live.is_empty() {
            None
        } else {
            Some(live[self.rng.gen_range(0..live.len())])
        }
    }

    fn pick_recipient(&mut self, id: GrantId) -> AccountId {
        let n = &self.nodes[&id];
        if self.rng.gen_bool(0.1) {
            return recipients_pool()[self.rng.gen_range(0..12)].clone();
        }
        match &n.terms.recipients {
            Some(list) => list[self.rng.gen_range(0..list.len())].clone(),
            None => recipients_pool()[self.rng.gen_range(0..12)].clone(),
        }
    }

    /// What must happen to this spend, decided from the oracle's own ledger.
    /// Err(code) is a refusal the contract has to reproduce exactly.
    fn expected_spend(&self, id: GrantId, recipient: &AccountId, amount: u128) -> Result<(), &'static str> {
        if self.model_dead(id) {
            return Err("dead");
        }
        if self.paused {
            return Err("PAUSED");
        }
        if amount == 0 {
            return Err("BAD_AMOUNT");
        }
        if self.blocked.contains(recipient) {
            return Err("RECIPIENT_BLOCKED");
        }
        let n = &self.nodes[&id];
        if let Some(list) = &n.terms.recipients {
            if !list.contains(recipient) {
                return Err("RECIPIENT_NOT_ALLOWED");
            }
        }
        if amount > n.terms.per_tx.0 {
            return Err("PER_TX");
        }
        for a in self.lineage(id) {
            let m = &self.nodes[&a];
            if m.settled + m.reserved + amount > m.terms.cap.0 {
                return Err("cap");
            }
            let from = self.now.saturating_sub(m.terms.window_secs as u64 * 1000);
            if self.window_sum(a, from) + amount > m.terms.window_cap.0 {
                return Err("window");
            }
        }
        Ok(())
    }

    fn assert_refusal_matches(&mut self, id: GrantId, recipient: &AccountId, amount: u128, d: &Decision) {
        let expected = self.expected_spend(id, recipient, amount);
        match (expected, d.code.as_str()) {
            (Err("dead"), "REVOKED" | "ANCESTOR_REVOKED" | "EXPIRED" | "ANCESTOR_EXPIRED") => {}
            (Err("cap"), "CAP" | "ANCESTOR_CAP") => {}
            (Err("window"), "WINDOW" | "ANCESTOR_WINDOW" | "CAP" | "ANCESTOR_CAP") => {}
            // Cap and window can both fail on one lineage. The contract reports the first one it
            // meets walking up, the oracle checks in the same order but may stop at another level.
            (Err("cap"), "WINDOW" | "ANCESTOR_WINDOW") => {}
            (Err(e), code) if e == code => {}
            // A bucket window is allowed to refuse early, by at most one bucket width.
            (Ok(()), "WINDOW" | "ANCESTOR_WINDOW") => {
                let g = d.blocking_grant.unwrap();
                let m = &self.nodes[&g];
                let w = m.terms.window_secs as u64 * 1000;
                let slack = w.div_ceil(window::BUCKETS);
                let from = self.now.saturating_sub(w + 2 * slack);
                assert!(
                    self.window_sum(g, from) + amount > m.terms.window_cap.0,
                    "window refused beyond one bucket of slack on grant {g}"
                );
                self.stats.window_conservative += 1;
            }
            (Ok(()), "VAULT_BALANCE") | (_, "VAULT_BALANCE") => {}
            (Ok(()), "QUEUE_FULL") => {
                assert!(amount > self.nodes[&id].terms.approval_threshold.0, "queue refusal below threshold");
            }
            (e, code) => panic!("oracle expected {e:?}, contract said {code}: {}", d.detail),
        }
    }

    fn record_reservation(&mut self, leaf: GrantId, spend_id: u64, amount: u128) {
        for a in self.lineage(leaf) {
            let now = self.now;
            let n = self.nodes.get_mut(&a).unwrap();
            n.reserved += amount;
            n.log.push((now, amount, spend_id));
        }
        // I6 and I4, measured on the oracle's ledger rather than on contract state.
        assert!(!self.model_dead(leaf), "I6 spend reserved on a dead lineage of grant {leaf}");
        for a in self.lineage(leaf) {
            let n = &self.nodes[&a];
            let from = self.now.saturating_sub(n.terms.window_secs as u64 * 1000);
            assert!(self.window_sum(a, from) <= n.terms.window_cap.0, "I4 window on grant {a}");
            assert!(n.settled + n.reserved <= n.terms.cap.0, "I1 oracle cap on grant {a}");
        }
        let native = self.nodes[&leaf].terms.asset == Asset::Native;
        if native {
            assert!(amount <= self.balance, "I9 vault paid more than it holds");
            self.balance -= amount;
        }
        self.spends.push(Spend { id: spend_id, leaf, amount, native, fate: Fate::InFlight });
    }

    fn op_spend(&mut self) {
        let Some(id) = self.pick_actor() else { return };
        // A key without this method never reaches the contract: the protocol rejects it (B09).
        if !self.nodes[&id].terms.methods.iter().any(|m| m == "request_spend") {
            return;
        }
        let recipient = self.pick_recipient(id);
        let per_tx = self.nodes[&id].terms.per_tx.0;
        let amount = match self.rng.gen_range(0..10) {
            0 => per_tx + self.rng.gen_range(1..1000),
            1 => 0,
            _ => self.rng.gen_range(1..=per_tx),
        };
        let pk = self.nodes[&id].pk.clone();
        let before = self.financial_snapshot();
        let d = self.call(Who::Key(pk), vec![], |v| {
            match v.request_spend(recipient.clone(), U128(amount), None) {
                PromiseOrValue::Value(d) => (d, None),
                PromiseOrValue::Promise(p) => {
                    let spend_id = v.next_nonce - 1;
                    p.detach();
                    (Decision::default(), Some(spend_id))
                }
            }
        });
        match d {
            (_, Some(spend_id)) => {
                self.expected_spend(id, &recipient, amount)
                    .unwrap_or_else(|e| panic!("oracle expected {e} but the contract reserved"));
                self.record_reservation(id, spend_id, amount);
                self.count("spend_reserved");
            }
            (d, None) if d.outcome == "queued" => {
                self.expected_spend(id, &recipient, amount)
                    .unwrap_or_else(|e| panic!("oracle expected {e} but the contract queued"));
                assert!(amount > self.nodes[&id].terms.approval_threshold.0, "queued below threshold");
                self.queue.insert(
                    d.approval_id.unwrap(),
                    (id, recipient, amount, d.spend_hash.unwrap(), false),
                );
                self.count("spend_queued");
            }
            (d, None) => {
                assert_eq!(d.outcome, "denied");
                self.assert_refusal_matches(id, &recipient, amount, &d);
                let after = self.financial_snapshot();
                assert_eq!(before, after, "I5 denied spend changed state: {}", d.code);
                self.note_denial(Some(id), &d.code);
                self.count("spend_denied");
            }
        }
    }

    fn op_settle(&mut self) {
        let open: Vec<usize> =
            self.spends.iter().enumerate().filter(|(_, s)| s.fate == Fate::InFlight).map(|(i, _)| i).collect();
        if open.is_empty() {
            return;
        }
        // Any order at all: callbacks of different spends interleave freely.
        let idx = open[self.rng.gen_range(0..open.len())];
        let ok = self.rng.gen_bool(0.8);
        self.settle(idx, ok);
    }

    fn settle(&mut self, idx: usize, ok: bool) {
        let (id, leaf, amount, native) = {
            let s = &self.spends[idx];
            (s.id, s.leaf, s.amount, s.native)
        };
        let result = if ok { PromiseResult::Successful(vec![]) } else { PromiseResult::Failed };
        let d = self.call(Who::Callback, vec![result], |v| v.on_spend_settled(id));
        assert_eq!(d.outcome, if ok { "settled" } else { "rolled_back" });
        for a in self.lineage(leaf) {
            let n = self.nodes.get_mut(&a).unwrap();
            n.reserved -= amount;
            if ok {
                n.settled += amount;
            } else {
                n.log.retain(|(_, _, s)| *s != id);
            }
        }
        if native {
            if ok {
                self.native_out += amount;
            } else {
                self.balance += amount;
            }
        }
        self.spends[idx].fate = if ok { Fate::Settled } else { Fate::RolledBack };
        self.count(if ok { "settled" } else { "rolled_back" });
    }

    fn narrow_terms(&mut self, parent: GrantId) -> Terms {
        let p = self.nodes[&parent].terms.clone();
        let remaining = self.lineage(parent).iter().map(|a| {
            let m = &self.nodes[a];
            m.terms.cap.0 - m.settled - m.reserved
        }).min().unwrap();
        let (gas_left, nodes_left) = self.view(|v| {
            let g = v.grants.get(&parent).unwrap();
            (g.gas_left, g.nodes_left)
        });
        let rng = &mut self.rng;
        let cap = rng.gen_range(1..=remaining.max(1)).min(remaining);
        let per_tx = rng.gen_range(1..=p.per_tx.0);
        let window_cap = rng.gen_range(1..=p.window_cap.0);
        let recipients = match &p.recipients {
            None => {
                if rng.gen_bool(0.3) {
                    None
                } else {
                    let pool = recipients_pool();
                    let n = rng.gen_range(1..=pool.len());
                    Some(pool[..n].to_vec())
                }
            }
            Some(list) => {
                let n = rng.gen_range(1..=list.len());
                Some(list[..n].to_vec())
            }
        };
        let methods: Vec<String> =
            p.methods.iter().filter(|_| rng.gen_bool(0.8)).cloned().collect();
        let methods = if methods.is_empty() { vec![p.methods[0].clone()] } else { methods };
        let remaining_life = p.expires_at_ms.saturating_sub(self.now);
        Terms {
            asset: p.asset.clone(),
            cap: U128(cap),
            per_tx: U128(per_tx),
            window_cap: U128(window_cap),
            window_secs: p.window_secs + rng.gen_range(0..30),
            approval_threshold: U128(rng.gen_range(p.approval_threshold.0 / 2..=p.approval_threshold.0)),
            recipients,
            methods,
            expires_at_ms: self.now + rng.gen_range(1..=remaining_life.max(2)).min(remaining_life.max(1)),
            max_depth: rng.gen_range(0..=p.max_depth.saturating_sub(1)),
            max_denials: rng.gen_range(p.max_denials.div_ceil(2)..=p.max_denials),
            key_allowance: U128(1),
            gas_budget: U128(gas_left.saturating_sub(1) / 3),
            node_budget: nodes_left.saturating_sub(1) / 2,
        }
    }

    /// Takes valid terms and widens exactly one dimension. Returns the code that must come back.
    fn widen(&mut self, parent: GrantId, t: &mut Terms) -> Option<&'static str> {
        let p = self.nodes[&parent].terms.clone();
        let (gas_left, nodes_left, remaining) = self.view(|v| {
            let g = v.grants.get(&parent).unwrap();
            let mut remaining = u128::MAX;
            for a in v.get_lineage(parent) {
                let m = v.grants.get(&a).unwrap();
                remaining = remaining.min(m.cap - m.spent - m.reserved);
            }
            (g.gas_left, g.nodes_left, remaining)
        });
        match self.rng.gen_range(0..13) {
            0 => {
                t.asset = if p.asset == Asset::Native { Asset::Ft(token_id()) } else { Asset::Native };
                Some("ATT_ASSET")
            }
            1 => {
                t.cap = U128(remaining + 1);
                t.per_tx = U128(t.per_tx.0.min(t.cap.0));
                Some("ATT_CAP")
            }
            2 => {
                t.per_tx = U128(p.per_tx.0 + 1);
                t.cap = U128(remaining.max(1));
                (remaining >= 1).then_some("ATT_PER_TX")
            }
            3 => {
                t.window_cap = U128(p.window_cap.0 + 1);
                Some("ATT_WINDOW_CAP")
            }
            4 => {
                if p.window_secs < 2 {
                    return None;
                }
                t.window_secs = p.window_secs - 1;
                Some("ATT_WINDOW_LEN")
            }
            5 => {
                t.approval_threshold = U128(p.approval_threshold.0 + 1);
                Some("ATT_THRESHOLD")
            }
            6 => {
                let list = p.recipients.as_ref()?;
                let extra = recipients_pool().into_iter().find(|a| !list.contains(a))?;
                let mut asked = list.clone();
                asked.truncate(3);
                asked.push(extra);
                t.recipients = Some(asked);
                Some("ATT_RECIPIENTS")
            }
            7 => {
                let list = p.recipients.as_ref()?;
                let _ = list;
                t.recipients = None;
                Some("ATT_RECIPIENTS")
            }
            8 => {
                let extra = METHODS.iter().find(|m| !p.methods.iter().any(|x| x == *m))?;
                t.methods.push(extra.to_string());
                Some("ATT_METHODS")
            }
            9 => {
                t.expires_at_ms = p.expires_at_ms + 1;
                Some("ATT_EXPIRY")
            }
            10 => {
                t.max_depth = p.max_depth.min(MAX_TREE_DEPTH);
                Some("ATT_DEPTH")
            }
            11 => {
                t.max_denials = p.max_denials + 1;
                Some("ATT_DENIALS")
            }
            _ => {
                if self.rng.gen_bool(0.5) {
                    t.gas_budget = U128(gas_left);
                    t.key_allowance = U128(1);
                    Some("ATT_GAS")
                } else {
                    t.node_budget = nodes_left;
                    Some("ATT_NODES")
                }
            }
        }
    }

    fn op_delegate(&mut self, widen: bool) {
        let Some(parent) = self.pick_actor() else { return };
        if self.paused {
            return;
        }
        let bit_ok = self.nodes[&parent].terms.methods.iter().any(|m| m == "delegate");
        if self.model_dead(parent) {
            if bit_ok {
                self.delegate_from_dead(parent);
            }
            return;
        }
        let (nodes_left, depth_left, kids) = self.view(|v| {
            let g = v.grants.get(&parent).unwrap();
            (g.nodes_left, g.max_depth, v.children.get(&parent).map_or(0, Vec::len))
        });
        if !bit_ok || depth_left == 0 || nodes_left == 0 || kids >= MAX_CHILDREN {
            return;
        }
        let spendable = self.lineage(parent).iter().map(|a| {
            let m = &self.nodes[a];
            m.terms.cap.0 - m.settled - m.reserved
        }).min().unwrap();
        if spendable == 0 {
            return;
        }
        let gas_left = self.view(|v| v.grants.get(&parent).unwrap().gas_left);
        if gas_left == 0 {
            return;
        }
        let mut terms = self.narrow_terms(parent);
        let expect_code = if widen { self.widen(parent, &mut terms) } else { None };
        if widen && expect_code.is_none() {
            return;
        }
        let pk = self.nodes[&parent].pk.clone();
        let child_pk = self.fresh_pk();
        let before = self.financial_snapshot();
        let new_id = self.view(|v| v.next_grant);
        let t2 = terms.clone();
        let cpk = child_pk.clone();
        let refused = self.call(Who::Key(pk), vec![], |v| match v.delegate(cpk, t2) {
            PromiseOrValue::Value(d) => Some(d),
            PromiseOrValue::Promise(p) => {
                p.detach();
                None
            }
        });
        match (expect_code, refused) {
            (Some(code), Some(d)) => {
                assert_eq!(d.code, code, "I7 wrong refusal: {}", d.detail);
                let after = self.financial_snapshot();
                assert_eq!(before, after, "I5 denied delegation changed state");
                self.note_denial(Some(parent), &d.code);
                self.count("delegate_denied");
            }
            (Some(code), None) => panic!("I7 widened {code} was accepted"),
            (None, Some(d)) => panic!("valid narrower terms refused: {} {}", d.code, d.detail),
            (None, None) => {
                self.assert_narrower(parent, &terms);
                self.call(Who::Callback, vec![PromiseResult::Successful(vec![])], |v| {
                    v.on_key_added(new_id);
                });
                self.nodes.insert(new_id, Node {
                    parent: Some(parent),
                    terms,
                    pk: child_pk,
                    dead: false,
                    denied: 0,
                    settled: 0,
                    reserved: 0,
                    log: vec![],
                });
                self.stats.max_nodes = self.stats.max_nodes.max(self.nodes.len());
                let depth = self.view(|v| v.grants.get(&new_id).unwrap().depth);
                self.stats.max_depth_seen = self.stats.max_depth_seen.max(depth);
                self.count("delegated");
            }
        }
    }

    /// A key that is still on the account but sits on a dead lineage must not extend the tree.
    fn delegate_from_dead(&mut self, parent: GrantId) {
        let pk = self.nodes[&parent].pk.clone();
        let child_pk = self.fresh_pk();
        let mut terms = self.nodes[&parent].terms.clone();
        terms.expires_at_ms = self.now + 1_000;
        let before = self.financial_snapshot();
        let d = self.call(Who::Key(pk), vec![], |v| match v.delegate(child_pk, terms) {
            PromiseOrValue::Value(d) => d,
            PromiseOrValue::Promise(_) => panic!("I6 dead lineage created a child"),
        });
        assert!(
            matches!(d.code.as_str(), "REVOKED" | "ANCESTOR_REVOKED" | "EXPIRED" | "ANCESTOR_EXPIRED"),
            "dead parent refused for the wrong reason: {}",
            d.code
        );
        let after = self.financial_snapshot();
        assert_eq!(before, after, "I5 denied delegation changed state");
        self.note_denial(Some(parent), &d.code);
        self.count("delegate_from_dead_denied");
    }

    /// Independent restatement of "narrower", phrased against the oracle's terms.
    fn assert_narrower(&self, parent: GrantId, c: &Terms) {
        let p = &self.nodes[&parent].terms;
        assert_eq!(c.asset, p.asset);
        assert!(c.per_tx.0 <= p.per_tx.0);
        assert!(c.window_cap.0 <= p.window_cap.0);
        assert!(c.window_secs >= p.window_secs);
        assert!(c.approval_threshold.0 <= p.approval_threshold.0);
        assert!(c.expires_at_ms <= p.expires_at_ms);
        assert!(c.max_depth < p.max_depth);
        assert!(c.max_denials <= p.max_denials);
        assert!(c.methods.iter().all(|m| p.methods.contains(m)));
        if let Some(list) = &p.recipients {
            let asked = c.recipients.as_ref().expect("I7 list parent, unlimited child");
            assert!(asked.iter().all(|a| list.contains(a)));
        }
        for a in self.lineage(parent) {
            let m = &self.nodes[&a];
            assert!(c.cap.0 <= m.terms.cap.0 - m.settled - m.reserved, "I7 cap over remaining");
        }
    }

    fn op_revoke(&mut self) {
        let ids: Vec<GrantId> = self.nodes.keys().copied().collect();
        if ids.is_empty() {
            return;
        }
        let target = ids[self.rng.gen_range(0..ids.len())];
        let by_ancestor = self.rng.gen_bool(0.5);
        let lineage = self.lineage(target);
        let who = if by_ancestor && lineage.len() > 1 {
            let a = lineage[self.rng.gen_range(1..lineage.len())];
            let live = self.view(|v| v.grants.get(&a).unwrap().key_live);
            if !live || !self.nodes[&a].terms.methods.iter().any(|m| m == "revoke") {
                return;
            }
            Some(a)
        } else {
            None
        };
        let already = self.view(|v| v.grants.get(&target).unwrap().revoked);
        let before = self.financial_snapshot();
        let caller = match who {
            Some(a) => Who::Key(self.nodes[&a].pk.clone()),
            None => Who::Owner,
        };
        let d = self.call(caller, vec![], |v| v.revoke(target));
        if d.outcome == "denied" {
            let after = self.financial_snapshot();
            assert_eq!(before, after, "I5 denied revoke changed state");
            match d.code.as_str() {
                "ALREADY_REVOKED" => assert!(already),
                // The ancestor caller itself may be dead by now.
                "REVOKED" | "ANCESTOR_REVOKED" | "EXPIRED" | "ANCESTOR_EXPIRED" => {
                    assert!(self.model_dead(who.unwrap()));
                }
                other => panic!("unexpected revoke refusal {other}"),
            }
            self.note_denial(who, &d.code);
            self.count("revoke_denied");
        } else {
            self.mark_subtree_dead(target);
            self.count("revoked");
        }
    }

    fn op_child_revokes_upward(&mut self) {
        let Some(id) = self.pick_actor() else { return };
        let Some(parent) = self.nodes[&id].parent else { return };
        if !self.nodes[&id].terms.methods.iter().any(|m| m == "revoke") || self.model_dead(id) {
            return;
        }
        let pk = self.nodes[&id].pk.clone();
        let before = self.financial_snapshot();
        let d = self.call(Who::Key(pk), vec![], |v| v.revoke(parent));
        assert_eq!(d.code, "NOT_ANCESTOR", "a child revoked its parent");
        let after = self.financial_snapshot();
        assert_eq!(before, after, "I5");
        self.note_denial(Some(id), &d.code);
        self.count("revoke_upward_denied");
    }

    fn op_approve(&mut self) {
        let Some((&aid, entry)) = self.queue.iter().next() else { return };
        let (grant, recipient, amount, hash, approved) = entry.clone();
        if !approved {
            let tamper = self.rng.gen_bool(0.2);
            let sent = if tamper { "00".repeat(32) } else { hash.clone() };
            let d = self.call(Who::Owner, vec![], |v| v.approve(aid, sent));
            if tamper {
                assert_eq!(d.outcome, "denied", "tampered hash was approved");
                self.note_denial(None, &d.code);
                self.count("approve_tampered");
            } else if d.outcome == "denied" {
                // An expired entry is either still there or was purged by a later enqueue.
                assert!(matches!(d.code.as_str(), "APPROVAL_EXPIRED" | "APPROVAL_NOT_FOUND"), "{}", d.code);
                self.queue.remove(&aid);
            } else {
                self.queue.get_mut(&aid).unwrap().4 = true;
                self.count("approved");
            }
            return;
        }
        let live = self.view(|v| v.grants.get(&grant).unwrap().key_live);
        if !live || !self.nodes[&grant].terms.methods.iter().any(|m| m == "execute_approved") {
            self.queue.remove(&aid);
            return;
        }
        let pk = self.nodes[&grant].pk.clone();
        let tamper = self.rng.gen_bool(0.2);
        let sent_amount = if tamper { amount + 1 } else { amount };
        let before = self.financial_snapshot();
        let r = recipient.clone();
        let out = self.call(Who::Key(pk), vec![], |v| {
            match v.execute_approved(aid, r, U128(sent_amount), None) {
                PromiseOrValue::Value(d) => (Some(d), 0),
                PromiseOrValue::Promise(p) => {
                    let id = v.next_nonce - 1;
                    p.detach();
                    (None, id)
                }
            }
        });
        match out {
            (Some(d), _) => {
                let after = self.financial_snapshot();
                assert_eq!(before, after, "I5 denied execution changed state");
                let gone = matches!(d.code.as_str(), "APPROVAL_EXPIRED" | "APPROVAL_NOT_FOUND");
                if tamper {
                    assert!(
                        d.code == "APPROVAL_HASH_MISMATCH" || gone,
                        "tampered execution refused for the wrong reason: {}",
                        d.code
                    );
                } else if !gone {
                    self.assert_refusal_matches(grant, &recipient, amount, &d);
                }
                self.note_denial(Some(grant), &d.code);
                if gone {
                    self.queue.remove(&aid);
                }
                self.count("execute_denied");
            }
            (None, spend_id) => {
                assert!(!tamper, "tampered amount executed");
                self.expected_spend(grant, &recipient, amount)
                    .unwrap_or_else(|e| panic!("execution passed but the oracle expected {e}"));
                self.record_reservation(grant, spend_id, amount);
                self.queue.remove(&aid);
                // Replay of the same approval.
                let pk = self.nodes[&grant].pk.clone();
                let before = self.financial_snapshot();
                let r = recipient.clone();
                let d = self.call(Who::Key(pk), vec![], |v| match v.execute_approved(aid, r, U128(amount), None) {
                    PromiseOrValue::Value(d) => d,
                    PromiseOrValue::Promise(_) => panic!("approval replay executed"),
                });
                assert_eq!(d.code, "APPROVAL_NOT_FOUND");
                let after = self.financial_snapshot();
                assert_eq!(before, after, "I5 replay changed state");
                self.note_denial(Some(grant), &d.code);
                self.count("executed_then_replay_denied");
            }
        }
    }

    fn op_time(&mut self) {
        self.now += match self.rng.gen_range(0..20) {
            0 => self.rng.gen_range(5_000..90_000),
            _ => self.rng.gen_range(1..2_000),
        };
        self.count("time");
    }

    fn op_block(&mut self) {
        let r = recipients_pool()[self.rng.gen_range(0..12)].clone();
        let r = self.blocked.iter().next().cloned().filter(|_| self.blocked.len() >= 2).unwrap_or(r);
        if self.blocked.contains(&r) {
            self.call(Who::Owner, vec![], |v| v.unblock_recipient(r.clone()));
            self.blocked.remove(&r);
        } else {
            self.call(Who::Owner, vec![], |v| v.block_recipient(r.clone()));
            self.blocked.insert(r);
        }
        self.count("block_toggle");
    }

    fn op_pause(&mut self) {
        if self.paused {
            self.call(Who::Owner, vec![], |v| v.unpause());
        } else {
            self.call(Who::Owner, vec![], |v| v.pause());
        }
        self.paused = !self.paused;
        self.count("pause_toggle");
    }

    fn op_sweep(&mut self) {
        let limit = self.rng.gen_range(1..=4);
        self.call(Who::Owner, vec![], |v| v.sweep(limit));
        self.count("sweep");
    }

    fn op_new_root(&mut self) {
        let live_roots = self
            .nodes
            .values()
            .filter(|n| n.parent.is_none() && !n.dead && self.now < n.terms.expires_at_ms)
            .count();
        if live_roots >= 3 {
            return;
        }
        let id = self.view(|v| v.next_grant);
        let pk = self.fresh_pk();
        let rng = &mut self.rng;
        let terms = Terms {
            asset: if rng.gen_bool(0.7) { Asset::Native } else { Asset::Ft(token_id()) },
            cap: U128(rng.gen_range(60_000..500_000)),
            per_tx: U128(rng.gen_range(10_000..80_000)),
            window_cap: U128(rng.gen_range(50_000..400_000)),
            window_secs: rng.gen_range(5..60),
            approval_threshold: U128(rng.gen_range(5_000..60_000)),
            recipients: if rng.gen_bool(0.5) { None } else { Some(recipients_pool()[..8].to_vec()) },
            methods: METHODS.iter().map(|m| m.to_string()).collect(),
            expires_at_ms: self.now + rng.gen_range(200_000..2_000_000),
            max_depth: rng.gen_range(3..=MAX_TREE_DEPTH),
            max_denials: rng.gen_range(5..60),
            key_allowance: U128(1),
            gas_budget: U128(1_000_000),
            node_budget: rng.gen_range(10..40),
        };
        let t = terms.clone();
        let k = pk.clone();
        self.call(Who::Owner, vec![], |v| match v.create_root(k, t) {
            PromiseOrValue::Promise(p) => p.detach(),
            PromiseOrValue::Value(d) => panic!("root refused: {} {}", d.code, d.detail),
        });
        self.call(Who::Callback, vec![PromiseResult::Successful(vec![])], |v| {
            v.on_key_added(id);
        });
        self.nodes.insert(id, Node {
            parent: None,
            terms,
            pk,
            dead: false,
            denied: 0,
            settled: 0,
            reserved: 0,
            log: vec![],
        });
        self.count("root_created");
    }

    fn count(&mut self, what: &str) {
        *self.stats.by_op.entry(what.to_string()).or_default() += 1;
    }

    fn step(&mut self) {
        self.stats.ops += 1;
        if self.paused && self.rng.gen_bool(0.3) {
            self.op_pause();
        }
        match self.rng.gen_range(0..100) {
            0..=44 => self.op_spend(),
            45..=62 => self.op_settle(),
            63..=74 => self.op_delegate(false),
            75..=82 => self.op_delegate(true),
            83..=84 => self.op_revoke(),
            85 => self.op_child_revokes_upward(),
            86..=90 => self.op_approve(),
            91..=95 => self.op_time(),
            96 => self.op_block(),
            97 => self.op_sweep(),
            98 => {
                if !self.paused && self.rng.gen_bool(0.25) {
                    self.op_pause()
                }
            }
            _ => self.op_new_root(),
        }
        self.check_invariants();
    }

    fn drain(&mut self) {
        for idx in 0..self.spends.len() {
            if self.spends[idx].fate == Fate::InFlight {
                let ok = self.rng.gen_bool(0.8);
                self.settle(idx, ok);
            }
        }
        self.check_invariants();
        let ids: Vec<GrantId> = self.nodes.keys().copied().collect();
        self.view(|v| {
            assert_eq!(v.in_flight, 0, "I8 in flight after drain");
            for id in &ids {
                assert_eq!(v.grants.get(id).unwrap().reserved, 0, "I8 reservation leaked on grant {id}");
            }
        });
        let settled: u128 = self.spends.iter().filter(|s| s.native && s.fate == Fate::Settled).map(|s| s.amount).sum();
        assert_eq!(settled, self.native_out, "I9 settled total");
        assert_eq!(self.balance + self.native_out, NATIVE_START, "I9 balance conservation");
    }
}

impl Vault {
    fn ft_credit_for_test(&mut self, amount: u128) {
        *self.tokens.get_mut(&token_id()).unwrap() += amount;
    }
}

fn env_num(name: &str, default: u64) -> u64 {
    std::env::var(name).ok().and_then(|v| v.parse().ok()).unwrap_or(default)
}

#[test]
fn random_operations_hold_every_invariant() {
    let seeds = env_num("PROPERTY_SEEDS", 10);
    let ops = env_num("PROPERTY_OPS", 1_000);
    let base = env_num("PROPERTY_SEED_BASE", 1);
    let started = std::time::Instant::now();
    let mut total = Stats::default();
    for seed in base..base + seeds {
        let mut sim = Sim::new(seed);
        sim.op_new_root();
        for _ in 0..ops {
            sim.step();
        }
        sim.drain();
        total.ops += sim.stats.ops;
        total.invariant_checks += sim.stats.invariant_checks;
        total.window_conservative += sim.stats.window_conservative;
        total.max_depth_seen = total.max_depth_seen.max(sim.stats.max_depth_seen);
        total.max_nodes = total.max_nodes.max(sim.stats.max_nodes);
        for (k, v) in &sim.stats.by_op {
            *total.by_op.entry(k.clone()).or_default() += v;
        }
        for (k, v) in &sim.stats.by_code {
            *total.by_code.entry(k.clone()).or_default() += v;
        }
    }
    let report = serde_json::json!({
        "harness": "in-process: real contract code on near-sdk MockedBlockchain, not the NEAR runtime",
        "seeds": seeds,
        "seed_base": base,
        "ops_per_seed": ops,
        "total_ops": total.ops,
        "invariant_sweeps": total.invariant_checks,
        "seconds": started.elapsed().as_secs_f64(),
        "max_tree_depth_seen": total.max_depth_seen,
        "max_grants_in_one_run": total.max_nodes,
        "window_refusals_within_bucket_slack": total.window_conservative,
        "operations": total.by_op,
        "denial_codes": total.by_code,
        "invariants": ["I1","I2","I3","I4","I5","I6","I7","I8","I9"],
    });
    println!("{}", serde_json::to_string_pretty(&report).unwrap());
    if let Ok(path) = std::env::var("PROPERTY_REPORT") {
        std::fs::write(path, serde_json::to_string_pretty(&report).unwrap()).unwrap();
    }
    // A run that never reached the interesting paths proves nothing, so coverage is asserted too.
    if total.ops >= 10_000 {
        for op in ["spend_reserved", "spend_queued", "delegated", "delegate_denied", "revoked", "settled", "rolled_back", "executed_then_replay_denied"] {
            assert!(total.by_op.get(op).copied().unwrap_or(0) > 0, "operation {op} never happened");
        }
        for code in ["ANCESTOR_CAP", "ANCESTOR_REVOKED", "PER_TX", "RECIPIENT_NOT_ALLOWED", "ATT_CAP", "ATT_METHODS", "ATT_RECIPIENTS"] {
            assert!(total.by_code.get(code).copied().unwrap_or(0) > 0, "code {code} never happened");
        }
        assert!(total.max_depth_seen >= 3, "tree stayed too shallow");
    }
}
