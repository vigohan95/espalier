//! Rolling spend window with bounded storage.
//!
//! The window is split into `BUCKETS` buckets of width `w = ceil(W / BUCKETS)`. `used()` sums the
//! current bucket and the `BUCKETS` before it, so `BUCKETS + 1` buckets in total. Those buckets
//! always cover the whole interval `(now - W, now]`:
//!
//!   now - W >= now - BUCKETS * w  =>  floor((now - W) / w) >= floor(now / w) - BUCKETS
//!
//! So `used()` is never below the true rolling sum, which gives the guarantee that matters: no
//! interval of length W ever holds more than the window cap. There is no period boundary to
//! straddle, so the 2x burst of a fixed reset cannot happen. The price is that a spend can stay
//! counted for up to one bucket width longer than W, which only ever denies, never over-allows.

use near_sdk::near;

pub const BUCKETS: u64 = 16;

#[near(serializers = [borsh])]
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Window {
    /// (bucket index, amount), oldest first, at most BUCKETS + 1 entries after pruning.
    entries: Vec<(u64, u128)>,
}

fn width(window_ms: u64) -> u64 {
    window_ms.div_ceil(BUCKETS).max(1)
}

impl Window {
    fn oldest_live(now_ms: u64, window_ms: u64) -> u64 {
        (now_ms / width(window_ms)).saturating_sub(BUCKETS)
    }

    pub fn used(&self, now_ms: u64, window_ms: u64) -> u128 {
        let oldest = Self::oldest_live(now_ms, window_ms);
        self.entries.iter().filter(|(idx, _)| *idx >= oldest).map(|(_, amount)| *amount).sum()
    }

    pub fn add(&mut self, now_ms: u64, window_ms: u64, amount: u128) {
        let oldest = Self::oldest_live(now_ms, window_ms);
        self.entries.retain(|(idx, _)| *idx >= oldest);
        let idx = now_ms / width(window_ms);
        match self.entries.iter_mut().find(|(i, _)| *i == idx) {
            Some((_, total)) => *total += amount,
            None => self.entries.push((idx, amount)),
        }
    }

    /// Undoes `add` for a spend that rolled back. `at_ms` is the time of the original reservation,
    /// so the amount leaves the same bucket it entered. A pruned bucket needs no correction.
    pub fn sub(&mut self, at_ms: u64, window_ms: u64, amount: u128) {
        let idx = at_ms / width(window_ms);
        if let Some(pos) = self.entries.iter().position(|(i, _)| *i == idx) {
            self.entries[pos].1 = self.entries[pos].1.saturating_sub(amount);
            if self.entries[pos].1 == 0 {
                self.entries.remove(pos);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_window_length_interval_exceeds_the_cap() {
        let (window_ms, cap) = (1_000u64, 100u128);
        let mut w = Window::default();
        let mut log: Vec<(u64, u128)> = vec![];
        let mut now = 0u64;
        for step in 0..20_000u64 {
            now += 1 + (step * 7919) % 97;
            let amount = 1 + (step as u128 * 31) % 40;
            if w.used(now, window_ms) + amount <= cap {
                w.add(now, window_ms, amount);
                log.push((now, amount));
            }
            let exact: u128 = log.iter().filter(|(t, _)| *t + window_ms > now).map(|(_, a)| *a).sum();
            assert!(exact <= cap, "exact rolling sum {exact} over cap at {now}");
            assert!(w.used(now, window_ms) >= exact, "bucket sum undercounts at {now}");
        }
    }

    #[test]
    fn fixed_period_double_burst_is_denied() {
        let (window_ms, cap) = (1_000u64, 100u128);
        let mut w = Window::default();
        w.add(999, window_ms, cap);
        assert_eq!(w.used(1_001, window_ms), cap, "a period boundary must not reset the window");
    }

    #[test]
    fn rollback_returns_to_the_same_bucket() {
        let mut w = Window::default();
        w.add(10, 1_000, 60);
        w.add(500, 1_000, 40);
        w.sub(10, 1_000, 60);
        assert_eq!(w.used(500, 1_000), 40);
    }
}
