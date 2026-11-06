//! Throwaway probe, not the market. It answers one question: can a second contract learn, from the
//! unmodified Espalier vault, (a) how much a grant has paid it, (b) whether the grant is still live,
//! (c) which key speaks for the grant, and can it send money back to the vault.
use near_sdk::json_types::U128;
use near_sdk::store::LookupMap;
use near_sdk::{env, near, AccountId, Gas, NearToken, Promise, PublicKey};

const GAS_GET_GRANT: Gas = Gas::from_tgas(10);
const GAS_CONTACT: Gas = Gas::from_tgas(5);
const GAS_CALLBACK: Gas = Gas::from_tgas(15);

/// The fields of the vault's GrantView this probe reads. Unknown fields are ignored.
#[near(serializers = [json])]
pub struct GrantLite {
    pub pk: PublicKey,
    pub live: bool,
    pub revoked: bool,
    pub spendable: U128,
    pub spent: U128,
    pub recipients: Option<Vec<AccountId>>,
    pub expires_at_ms: u64,
    pub parent: Option<u64>,
}

#[near(serializers = [json])]
pub struct ContactLite {
    pub count: u32,
    pub total: U128,
}

#[near(serializers = [json])]
pub struct Answer {
    pub grant_found: bool,
    pub live: bool,
    pub revoked: bool,
    pub spendable: U128,
    pub spent: U128,
    pub parent: Option<u64>,
    pub probe_in_allowlist: Option<bool>,
    pub sig_ok: bool,
    pub contact_total: U128,
    pub newly_credited: U128,
    pub credit: U128,
    pub grant_view_bytes: u32,
}

#[near(contract_state)]
pub struct Probe {
    seen: LookupMap<(AccountId, u64), u128>,
    credit: LookupMap<(AccountId, u64), u128>,
}

impl Default for Probe {
    fn default() -> Self {
        Self { seen: LookupMap::new(b"s"), credit: LookupMap::new(b"c") }
    }
}

fn unhex(s: &str) -> Option<Vec<u8>> {
    if s.len() % 2 != 0 {
        return None;
    }
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok()).collect()
}

pub fn message(probe: &AccountId, vault: &AccountId, grant_id: u64, payload: &str) -> Vec<u8> {
    let mut out = b"espalier-market-probe-v1".to_vec();
    out.extend(near_sdk::borsh::to_vec(&(probe, vault, grant_id, payload)).expect("borsh"));
    out
}

#[near]
impl Probe {
    /// Anyone can relay this. `signature` is hex ed25519 by the grant's key over `message(...)`.
    pub fn check(&mut self, vault: AccountId, grant_id: u64, payload: String, signature: String) -> Promise {
        let me = env::current_account_id();
        let grant = Promise::new(vault.clone()).function_call(
            "get_grant".to_string(),
            format!("{{\"grant_id\":{grant_id}}}").into_bytes(),
            NearToken::from_yoctonear(0),
            GAS_GET_GRANT,
        );
        let contact = Promise::new(vault.clone()).function_call(
            "contact".to_string(),
            format!("{{\"grant_id\":{grant_id},\"recipient\":\"{me}\"}}").into_bytes(),
            NearToken::from_yoctonear(0),
            GAS_CONTACT,
        );
        grant.and(contact).then(
            Self::ext(me).with_static_gas(GAS_CALLBACK).on_check(vault, grant_id, payload, signature),
        )
    }

    #[private]
    pub fn on_check(&mut self, vault: AccountId, grant_id: u64, payload: String, signature: String) -> Answer {
        let me = env::current_account_id();
        let raw = env::promise_result_checked(0, 8192).unwrap_or_default();
        let grant: Option<GrantLite> = near_sdk::serde_json::from_slice(&raw).ok().flatten();
        let contact: Option<ContactLite> = env::promise_result_checked(1, 1024)
            .ok()
            .and_then(|b| near_sdk::serde_json::from_slice::<Option<ContactLite>>(&b).ok())
            .flatten();
        let key = (vault.clone(), grant_id);
        let total = contact.map_or(0, |c| c.total.0);
        let seen = self.seen.get(&key).copied().unwrap_or(0);
        let newly = total.saturating_sub(seen);
        let Some(g) = grant else {
            return Answer {
                grant_found: false,
                live: false,
                revoked: false,
                spendable: U128(0),
                spent: U128(0),
                parent: None,
                probe_in_allowlist: None,
                sig_ok: false,
                contact_total: U128(total),
                newly_credited: U128(0),
                credit: U128(self.credit.get(&key).copied().unwrap_or(0)),
                grant_view_bytes: raw.len() as u32,
            };
        };
        let sig_ok = match (unhex(&signature).and_then(|b| <[u8; 64]>::try_from(b).ok()), g.pk.as_bytes()) {
            (Some(sig), pk) if pk.len() == 33 && pk[0] == 0 => {
                let pk32: [u8; 32] = pk[1..].try_into().expect("32 bytes");
                env::ed25519_verify(&sig, &message(&me, &vault, grant_id, &payload), &pk32)
            }
            _ => false,
        };
        // Credit only what the vault says it settled to this account, and only once.
        if newly > 0 {
            self.seen.insert(key.clone(), total);
            let c = self.credit.get(&key).copied().unwrap_or(0);
            self.credit.insert(key.clone(), c + newly);
        }
        Answer {
            grant_found: true,
            live: g.live,
            revoked: g.revoked,
            spendable: g.spendable,
            spent: g.spent,
            parent: g.parent,
            probe_in_allowlist: g.recipients.map(|l| l.contains(&me)),
            sig_ok,
            contact_total: U128(total),
            newly_credited: U128(newly),
            credit: U128(self.credit.get(&key).copied().unwrap_or(0)),
            grant_view_bytes: raw.len() as u32,
        }
    }

    /// Sends native NEAR back to a vault and takes it off the grant's credit.
    pub fn refund(&mut self, vault: AccountId, grant_id: u64, amount: U128) -> Promise {
        let key = (vault.clone(), grant_id);
        let c = self.credit.get(&key).copied().unwrap_or(0);
        assert!(amount.0 <= c, "more than the credit");
        self.credit.insert(key, c - amount.0);
        Promise::new(vault).transfer(NearToken::from_yoctonear(amount.0))
    }

    /// Asks a vault who its owner is, the way the market will before it accepts a mandate.
    pub fn owner_of(&mut self, vault: AccountId) -> Promise {
        Promise::new(vault)
            .function_call("vault_info".to_string(), b"{}".to_vec(), NearToken::from_yoctonear(0), GAS_CONTACT)
            .then(Self::ext(env::current_account_id()).with_static_gas(GAS_CONTACT).on_owner(env::predecessor_account_id()))
    }

    #[private]
    pub fn on_owner(&mut self, caller: AccountId) -> (String, bool) {
        let raw = env::promise_result_checked(0, 4096).unwrap_or_default();
        let info: near_sdk::serde_json::Value = near_sdk::serde_json::from_slice(&raw).unwrap_or_default();
        let owner = info["owner"].as_str().unwrap_or("").to_string();
        let is_owner = owner == caller.as_str();
        (owner, is_owner)
    }

    pub fn credit_of(&self, vault: AccountId, grant_id: u64) -> U128 {
        U128(self.credit.get(&(vault, grant_id)).copied().unwrap_or(0))
    }
}
