//! Pure, non-cyclic neighbour selection for image-to-image navigation in the
//! viewer. Left arrow = `Prev`, Right arrow = `Next`; no wraparound.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub enum Step {
    Prev,
    Next,
}

/// The index of the neighbour of `current` within a list of `len` items, or
/// `None` at the ends / when `current` is out of range / the list is empty.
#[allow(dead_code)]
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
}
