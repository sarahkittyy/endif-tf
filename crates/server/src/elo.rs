//! Standard Elo rating (K = 32, ratings start at 1500).

pub const START: i32 = 1500;
pub const K: f64 = 32.0;

/// Expected score of `a` against `b`.
pub fn expected(a: i32, b: i32) -> f64 {
    1.0 / (1.0 + 10f64.powf((b - a) as f64 / 400.0))
}

/// Rating changes `(delta_a, delta_b)` after a game; `a_won` is false when `b` won.
pub fn deltas(a: i32, b: i32, a_won: bool) -> (i32, i32) {
    let ea = expected(a, b);
    let sa = if a_won { 1.0 } else { 0.0 };
    let da = (K * (sa - ea)).round() as i32;
    let db = (K * ((1.0 - sa) - (1.0 - ea))).round() as i32;
    (da, db)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn equal_ratings_move_by_half_k() {
        assert_eq!(deltas(1500, 1500, true), (16, -16));
        assert_eq!(deltas(1500, 1500, false), (-16, 16));
    }

    #[test]
    fn upsets_pay_more_than_expected_wins() {
        let (weak_wins, strong_loses) = deltas(1400, 1600, true);
        let (strong_wins, weak_loses) = deltas(1600, 1400, true);
        assert!(weak_wins > strong_wins);
        assert!(weak_loses.abs() < strong_loses.abs());
        assert_eq!(weak_wins, -strong_loses);
        assert_eq!(deltas(1400, 1600, true), (24, -24));
        assert_eq!(deltas(1600, 1400, true), (8, -8));
    }
}
