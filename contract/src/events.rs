//! NEP-297 events. Amounts are strings because u128 does not fit a JSON number.
use near_sdk::env;
use near_sdk::serde_json::{json, Value};

pub const STANDARD: &str = "espalier";
pub const VERSION: &str = "1.0.0";

pub fn emit(event: &str, data: Value) {
    let line = json!({"standard": STANDARD, "version": VERSION, "event": event, "data": [data]});
    env::log_str(&format!("EVENT_JSON:{line}"));
}
