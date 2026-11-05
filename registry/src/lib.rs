//! A denylist that many agents share and that no single reporter can control.
//!
//! Anyone can file a report, with a deposit and a hash of the evidence. The registry itself decides
//! nothing: each consumer names the reporters it trusts and a quorum, and only those reports count,
//! so a thousand fresh accounts have no effect on a vault that does not trust them. Inside the quorum
//! a piece of evidence counts once, so a reporter who copies another's hash adds nothing. Reports
//! expire, and the subject can file a public response. See docs/RECIPIENT-CHECK.md.

use near_sdk::store::LookupMap;
use near_sdk::{env, near, require, AccountId, BorshStorageKey, NearToken, PanicOnDefault, Promise};
use std::collections::BTreeSet;

/// Refunded when the report is retracted or pruned. A report never outgrows this much storage.
pub const REPORT_DEPOSIT: NearToken = NearToken::from_millinear(10);
pub const MAX_URI_BYTES: usize = 256;
pub const MAX_CATEGORY_BYTES: usize = 32;
pub const MAX_TTL_SECS: u32 = 180 * 86_400;
pub const MAX_REPORTERS_PER_CHECK: usize = 16;
/// Whatever a consumer asks for, one piece of evidence never lists anyone.
pub const MIN_QUORUM: u8 = 2;

#[derive(BorshStorageKey)]
#[near]
enum Key {
    Reports,
    Responses,
    Totals,
    Reporters,
}

#[near(serializers = [borsh, json])]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Report {
    pub category: String,
    /// Hex sha256 of the evidence file. The file lives at `evidence_uri`, anyone can re-hash it.
    pub evidence_sha256: String,
    pub evidence_uri: String,
    pub filed_at_ms: u64,
    pub expires_at_ms: u64,
}

#[near(serializers = [borsh, json])]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Response {
    pub evidence_sha256: String,
    pub evidence_uri: String,
    pub filed_at_ms: u64,
}

/// Public track record, so that a consumer can judge a reporter before trusting it.
#[near(serializers = [borsh, json])]
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ReporterStats {
    pub filed: u32,
    pub retracted: u32,
}

#[near(serializers = [json])]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Check {
    /// True when the trusted reporters hold at least `quorum` different pieces of live evidence.
    pub listed: bool,
    /// Live reports from the named reporters, before identical evidence is merged.
    pub trusted_reports: u32,
    pub distinct_evidence: u32,
    /// Live and expired reports from anyone, trusted or not. Never used to decide `listed`.
    pub all_reports: u32,
    /// The subject has filed a response. A human should read it; it changes nothing here.
    pub responded: bool,
}

#[near(contract_state)]
#[derive(PanicOnDefault)]
pub struct Registry {
    reports: LookupMap<(AccountId, AccountId), Report>,
    responses: LookupMap<AccountId, Response>,
    totals: LookupMap<AccountId, u32>,
    reporters: LookupMap<AccountId, ReporterStats>,
}

fn require_hash(hash: &str) {
    let ok = hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
    require!(ok, "evidence must be a lowercase hex sha256");
    require!(hash.bytes().any(|b| b != b'0'), "evidence hash is empty");
}

#[near]
impl Registry {
    #[init]
    pub fn new() -> Self {
        Self {
            reports: LookupMap::new(Key::Reports),
            responses: LookupMap::new(Key::Responses),
            totals: LookupMap::new(Key::Totals),
            reporters: LookupMap::new(Key::Reporters),
        }
    }

    /// One live report per reporter and subject. Changing it means retracting it first, which shows
    /// in the reporter's record.
    #[payable]
    pub fn report(
        &mut self,
        subject: AccountId,
        category: String,
        evidence_sha256: String,
        evidence_uri: String,
        ttl_secs: u32,
    ) {
        let reporter = env::predecessor_account_id();
        require!(env::attached_deposit() >= REPORT_DEPOSIT, "attach 0.01 NEAR");
        require!(subject != reporter, "cannot report yourself");
        require_hash(&evidence_sha256);
        require!(!evidence_uri.is_empty() && evidence_uri.len() <= MAX_URI_BYTES, "evidence_uri must be 1 to 256 bytes");
        require!(!category.is_empty() && category.len() <= MAX_CATEGORY_BYTES, "category must be 1 to 32 bytes");
        require!(ttl_secs > 0 && ttl_secs <= MAX_TTL_SECS, "ttl_secs must be 1 second to 180 days");
        let key = (subject.clone(), reporter.clone());
        require!(!self.reports.contains_key(&key), "already reported: retract first");
        let now = env::block_timestamp_ms();
        let report = Report {
            category,
            evidence_sha256,
            evidence_uri,
            filed_at_ms: now,
            expires_at_ms: now + ttl_secs as u64 * 1000,
        };
        env::log_str(&format!(
            "EVENT_JSON:{}",
            near_sdk::serde_json::json!({
                "standard": "espalier-registry", "version": "1.0.0", "event": "reported",
                "data": [{"subject": subject, "reporter": reporter, "report": report}],
            })
        ));
        self.reports.insert(key, report);
        *self.totals.entry(subject).or_insert(0) += 1;
        self.reporters.entry(reporter).or_default().filed += 1;
    }

    pub fn retract(&mut self, subject: AccountId) -> Promise {
        let reporter = env::predecessor_account_id();
        require!(self.reports.remove(&(subject.clone(), reporter.clone())).is_some(), "no such report");
        self.drop_total(&subject);
        self.reporters.entry(reporter.clone()).or_default().retracted += 1;
        env::log_str(&format!(
            "EVENT_JSON:{}",
            near_sdk::serde_json::json!({
                "standard": "espalier-registry", "version": "1.0.0", "event": "retracted",
                "data": [{"subject": subject, "reporter": reporter}],
            })
        ));
        Promise::new(reporter).transfer(REPORT_DEPOSIT)
    }

    /// Anyone may clear an expired report. The deposit goes back to its reporter.
    pub fn prune(&mut self, subject: AccountId, reporter: AccountId) -> Promise {
        let key = (subject.clone(), reporter.clone());
        let report = self.reports.get(&key).expect("no such report");
        require!(env::block_timestamp_ms() >= report.expires_at_ms, "report has not expired");
        self.reports.remove(&key);
        self.drop_total(&subject);
        Promise::new(reporter).transfer(REPORT_DEPOSIT)
    }

    /// The subject's answer. It is public and it cannot unlist the subject: that would let any bad
    /// account clear itself. Consumers see `responded` and a human reads it.
    #[payable]
    pub fn respond(&mut self, evidence_sha256: String, evidence_uri: String) {
        require!(env::attached_deposit() >= REPORT_DEPOSIT, "attach 0.01 NEAR");
        require_hash(&evidence_sha256);
        require!(!evidence_uri.is_empty() && evidence_uri.len() <= MAX_URI_BYTES, "evidence_uri must be 1 to 256 bytes");
        self.responses.insert(
            env::predecessor_account_id(),
            Response { evidence_sha256, evidence_uri, filed_at_ms: env::block_timestamp_ms() },
        );
    }

    /// Reads at most 16 reports whatever the size of the world, so a flood of reports from accounts
    /// a consumer does not trust cannot raise the gas of its check.
    pub fn check(&self, subject: AccountId, reporters: Vec<AccountId>, quorum: u8) -> Check {
        require!(reporters.len() <= MAX_REPORTERS_PER_CHECK, "at most 16 reporters");
        let now = env::block_timestamp_ms();
        let named: BTreeSet<AccountId> = reporters.into_iter().collect();
        let mut evidence = BTreeSet::new();
        let mut trusted_reports = 0;
        for reporter in named {
            if let Some(r) = self.reports.get(&(subject.clone(), reporter)) {
                if now < r.expires_at_ms {
                    trusted_reports += 1;
                    evidence.insert(r.evidence_sha256.clone());
                }
            }
        }
        let distinct_evidence = evidence.len() as u32;
        Check {
            listed: distinct_evidence >= quorum.max(MIN_QUORUM) as u32,
            trusted_reports,
            distinct_evidence,
            all_reports: self.totals.get(&subject).copied().unwrap_or(0),
            responded: self.responses.contains_key(&subject),
        }
    }

    pub fn get_report(&self, subject: AccountId, reporter: AccountId) -> Option<Report> {
        self.reports.get(&(subject, reporter)).cloned()
    }

    pub fn get_response(&self, subject: AccountId) -> Option<Response> {
        self.responses.get(&subject).cloned()
    }

    pub fn reporter_stats(&self, reporter: AccountId) -> ReporterStats {
        self.reporters.get(&reporter).cloned().unwrap_or_default()
    }
}

impl Registry {
    fn drop_total(&mut self, subject: &AccountId) {
        if let Some(n) = self.totals.get_mut(subject) {
            *n = n.saturating_sub(1);
        }
    }
}
