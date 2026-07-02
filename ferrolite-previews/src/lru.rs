//! Pure LRU eviction planner: given cache entries and a byte cap, decides
//! which digests to evict (oldest last-access first) so the remaining
//! total fits under the cap. No I/O here — [`crate::PreviewStore::evict_to`]
//! supplies the entries from disk (via `.key` mtimes) and performs the
//! actual deletion.

/// Given `(digest, bytes, last_access_ns)` entries and a cap, returns the
/// digests to delete — oldest `last_access` first — so the remaining
/// total is `<= cap_bytes`.
///
/// Ties in `last_access_ns` break by digest (ascending) so the result is
/// deterministic. Entries are considered newest-first: an entry is kept
/// while the running total (of everything newer, already kept) stays
/// within the cap; the moment an entry would push the total over the
/// cap, that entry *and every older entry* are evicted — a smaller,
/// older entry is never kept in place of a larger, newer one that didn't
/// fit, so the surviving set is always a contiguous newest-first prefix.
pub fn plan_eviction(entries: &[(String, u64, i64)], cap_bytes: u64) -> Vec<String> {
    let mut sorted: Vec<&(String, u64, i64)> = entries.iter().collect();
    sorted.sort_by(|a, b| a.2.cmp(&b.2).then_with(|| a.0.cmp(&b.0)));

    let mut running_total: u64 = 0;
    let mut still_fits = true;
    let mut evicted_newest_first: Vec<String> = Vec::new();

    for (digest, bytes, _last_access_ns) in sorted.iter().rev() {
        if still_fits && running_total.saturating_add(*bytes) <= cap_bytes {
            running_total += bytes;
        } else {
            still_fits = false;
            evicted_newest_first.push(digest.clone());
        }
    }

    evicted_newest_first.reverse();
    evicted_newest_first
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(digest: &str, bytes: u64, last_access_ns: i64) -> (String, u64, i64) {
        (digest.to_string(), bytes, last_access_ns)
    }

    #[test]
    fn no_eviction_when_under_cap() {
        let entries = vec![entry("a", 100, 1_000), entry("b", 200, 2_000)];

        let evicted = plan_eviction(&entries, 1_000);

        assert_eq!(evicted, Vec::<String>::new());
    }

    #[test]
    fn evicts_oldest_first_until_under_cap() {
        // Oldest -> newest by last_access_ns: a (1), b (2), c (3).
        let entries = vec![entry("a", 100, 1), entry("b", 100, 2), entry("c", 100, 3)];

        // Cap only fits one 100-byte entry, so the newest (c) survives
        // and the two oldest are evicted, oldest first.
        let evicted = plan_eviction(&entries, 100);

        assert_eq!(evicted, vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn evicts_all_when_cap_zero() {
        let entries = vec![entry("a", 100, 1), entry("b", 200, 2)];

        let evicted = plan_eviction(&entries, 0);

        assert_eq!(evicted, vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn ties_break_deterministically() {
        // All entries share the same last_access_ns; ascending digest
        // order breaks the tie, so "z" is treated as newest.
        let entries = vec![entry("z", 100, 5), entry("a", 100, 5), entry("m", 100, 5)];

        let evicted = plan_eviction(&entries, 100);

        assert_eq!(evicted, vec!["a".to_string(), "m".to_string()]);
    }
}
