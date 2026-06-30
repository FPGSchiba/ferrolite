//! Pure, non-cyclic neighbour selection for image-to-image navigation in the
//! viewer. Left arrow = `Prev`, Right arrow = `Next`; no wraparound.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Prev,
    Next,
}

/// The index of the neighbour of `current` within a list of `len` items, or
/// `None` at the ends / when `current` is out of range / the list is empty.
pub fn neighbor_index(current: usize, len: usize, dir: Step) -> Option<usize> {
    if len == 0 || current >= len {
        return None;
    }
    match dir {
        Step::Prev => current.checked_sub(1),
        Step::Next => {
            let next = current + 1;
            (next < len).then_some(next)
        }
    }
}

/// Prev/next image id within `ids`. If `current` is not in `ids` (e.g. it was
/// filtered out), Next falls back to the first id and Prev to the last, so
/// arrow-keys still move into the filtered set. Non-cyclic at the ends.
pub fn neighbor_in_set(ids: &[i64], current: i64, dir: Step) -> Option<i64> {
    if ids.is_empty() {
        return None;
    }
    match ids.iter().position(|id| *id == current) {
        Some(pos) => neighbor_index(pos, ids.len(), dir).map(|n| ids[n]),
        None => match dir {
            Step::Next => ids.first().copied(),
            Step::Prev => ids.last().copied(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn next_and_prev_in_the_middle() {
        assert_eq!(neighbor_index(2, 5, Step::Next), Some(3));
        assert_eq!(neighbor_index(2, 5, Step::Prev), Some(1));
    }

    #[test]
    fn clamps_non_cyclic_at_both_ends() {
        assert_eq!(neighbor_index(4, 5, Step::Next), None); // last → no next
        assert_eq!(neighbor_index(0, 5, Step::Prev), None); // first → no prev
    }

    #[test]
    fn empty_and_single_yield_none() {
        assert_eq!(neighbor_index(0, 0, Step::Next), None);
        assert_eq!(neighbor_index(0, 0, Step::Prev), None);
        assert_eq!(neighbor_index(0, 1, Step::Next), None);
        assert_eq!(neighbor_index(0, 1, Step::Prev), None);
    }

    #[test]
    fn out_of_range_current_is_none() {
        assert_eq!(neighbor_index(9, 5, Step::Next), None);
    }

    #[test]
    fn neighbor_in_set_walks_and_clamps() {
        let ids = vec![10, 20, 30];
        assert_eq!(neighbor_in_set(&ids, 20, Step::Next), Some(30));
        assert_eq!(neighbor_in_set(&ids, 20, Step::Prev), Some(10));
        assert_eq!(neighbor_in_set(&ids, 30, Step::Next), None); // at end
        assert_eq!(neighbor_in_set(&ids, 10, Step::Prev), None); // at start
    }

    #[test]
    fn neighbor_in_set_falls_back_when_current_absent() {
        let ids = vec![10, 20, 30];
        // 99 is not in the set: Next → first, Prev → last.
        assert_eq!(neighbor_in_set(&ids, 99, Step::Next), Some(10));
        assert_eq!(neighbor_in_set(&ids, 99, Step::Prev), Some(30));
    }

    #[test]
    fn neighbor_in_set_empty_and_single() {
        assert_eq!(neighbor_in_set(&[], 1, Step::Next), None);
        assert_eq!(neighbor_in_set(&[10], 10, Step::Next), None);
        assert_eq!(neighbor_in_set(&[10], 10, Step::Prev), None);
        // single element, current absent → fallback to that element.
        assert_eq!(neighbor_in_set(&[10], 99, Step::Next), Some(10));
    }
}
