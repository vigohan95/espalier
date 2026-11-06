//! The pricing rule, as one pure function. The same vectors in `fixtures/pricing-vectors.json` are run
//! here, by the Python model in `client/espalier/market_model.py` and by the live page.
//!
//! Lowest price wins. The winner is paid the second-lowest price, or the task's maximum budget when
//! it is the only valid bid. A winner's pay therefore does not depend on its own bid, only on whether
//! it wins, so bidding its real cost is its best move.

/// `bids` holds the valid revealed bids as `(price, commit sequence)`. Returns the index of the
/// winner and what it is paid, or `None` when fewer than `min_bids` bids are valid.
pub fn pick(bids: &[(u128, u32)], max_budget: u128, min_bids: u8) -> Option<(usize, u128)> {
    if bids.is_empty() || bids.len() < min_bids.max(1) as usize {
        return None;
    }
    let mut winner = 0;
    for (i, b) in bids.iter().enumerate() {
        if (b.0, b.1) < (bids[winner].0, bids[winner].1) {
            winner = i;
        }
    }
    let second = bids.iter().enumerate().filter(|(i, _)| *i != winner).map(|(_, b)| b.0).min();
    Some((winner, second.unwrap_or(max_budget).min(max_budget)))
}

#[cfg(test)]
mod tests {
    use super::pick;
    use serde_json::Value;

    #[test]
    fn shared_vectors() {
        let doc: Value = serde_json::from_str(include_str!("../fixtures/pricing-vectors.json")).unwrap();
        let cases = doc["cases"].as_array().unwrap();
        assert!(cases.len() >= 10);
        for c in cases {
            let bids: Vec<(u128, u32)> = c["bids"]
                .as_array()
                .unwrap()
                .iter()
                .map(|b| (b[0].as_str().unwrap().parse().unwrap(), b[1].as_u64().unwrap() as u32))
                .collect();
            let max: u128 = c["max_budget"].as_str().unwrap().parse().unwrap();
            let got = pick(&bids, max, c["min_bids"].as_u64().unwrap() as u8);
            let want = if c["winner"].is_null() {
                None
            } else {
                Some((c["winner"].as_u64().unwrap() as usize, c["price"].as_str().unwrap().parse::<u128>().unwrap()))
            };
            assert_eq!(got, want, "{}", c["name"]);
        }
    }

    /// Small deterministic generator, so the test needs no extra crate.
    fn next(state: &mut u64) -> u64 {
        *state ^= *state << 13;
        *state ^= *state >> 7;
        *state ^= *state << 17;
        *state
    }

    #[test]
    fn properties_on_random_bids() {
        let mut s = 0x9E3779B97F4A7C15u64;
        for _ in 0..20_000 {
            let n = (next(&mut s) % 9) as usize;
            let max = 1 + (next(&mut s) % 1000) as u128;
            let min_bids = (next(&mut s) % 4) as u8;
            let bids: Vec<(u128, u32)> = (0..n).map(|i| (1 + (next(&mut s) as u128 % max), i as u32)).collect();
            match pick(&bids, max, min_bids) {
                None => assert!(n == 0 || n < min_bids.max(1) as usize),
                Some((w, price)) => {
                    assert!(n >= min_bids.max(1) as usize);
                    // Nobody bid lower than the winner, and among equals the winner committed first.
                    assert!(bids.iter().all(|b| (b.0, b.1) >= (bids[w].0, bids[w].1)));
                    // The price covers the winner's own bid and never exceeds the budget.
                    assert!(price >= bids[w].0 && price <= max);
                    // With a rival, the price is the best rival bid: the winner's own bid does not set it.
                    if n >= 2 {
                        let rival = bids.iter().enumerate().filter(|(i, _)| *i != w).map(|(_, b)| b.0).min().unwrap();
                        assert_eq!(price, rival);
                        let mut lower = bids.clone();
                        lower[w].0 = 1;
                        assert_eq!(pick(&lower, max, min_bids), Some((w, price)), "bidding lower must not change the pay");
                    } else {
                        assert_eq!(price, max);
                    }
                }
            }
        }
    }
}
