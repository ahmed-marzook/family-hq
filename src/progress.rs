//! Text progress bars, shared by `/todo` and `/weather`.

/// How many blocks a bar has.
const CELLS: i64 = 10;
const FILLED: &str = "■";
const EMPTY: &str = "□";

/// "■■■■□□□□□□": ten blocks, filled in proportion to `done` out of
/// `total` (rounded). Empty if `total` is 0.
pub fn bar(done: i64, total: i64) -> String {
    if total <= 0 {
        return String::new();
    }
    // Integer maths with rounding: add half the divisor before dividing.
    let filled = ((done * CELLS + total / 2) / total).clamp(0, CELLS);
    FILLED.repeat(filled as usize) + &EMPTY.repeat((CELLS - filled) as usize)
}

/// `done` out of `total` as a whole percentage, rounded: 58 of 91 is 64.
pub fn percent(done: i64, total: i64) -> i64 {
    if total <= 0 {
        return 0;
    }
    ((done * 100 + total / 2) / total).clamp(0, 100)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bar_rounds_and_handles_edges() {
        assert_eq!(bar(0, 4), "□□□□□□□□□□");
        assert_eq!(bar(1, 3), "■■■□□□□□□□");
        assert_eq!(bar(4, 4), "■■■■■■■■■■");
        assert_eq!(bar(9, 4), "■■■■■■■■■■", "never more than full");
        assert_eq!(bar(0, 0), "");
    }

    #[test]
    fn percent_rounds() {
        assert_eq!(percent(58, 91), 64);
        assert_eq!(percent(301, 365), 82);
        assert_eq!(percent(1, 3), 33);
        assert_eq!(percent(2, 3), 67);
        assert_eq!(percent(1, 0), 0);
    }
}
