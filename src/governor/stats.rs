//! Frame-time statistics for the governor: pure functions over frame times in milliseconds.
//!
//! None of them panics, whatever the input: an empty slice, NaN or infinite values, or a period
//! that is zero, negative or not finite.

/// Slack for a value that sits exactly on a band edge (dispatch notes T5).
const EPS: f64 = 1e-9;

/// The multiples of the period that [`quantised`] accepts.
const MULTIPLES: [f64; 4] = [1.0, 2.0, 3.0, 4.0];

/// Half-width of the band around each multiple, as a share of that multiple.
const BAND: f64 = 0.05;

/// The 95th percentile by nearest rank: the value at rank ⌈0.95 n⌉ of a sorted copy. An empty
/// slice gives 0.0. The sort uses `f64::total_cmp`, so NaN values cannot make it panic (a positive
/// NaN sorts last).
pub fn p95(xs: &[f64]) -> f64 {
    p95_with(xs, &mut Vec::with_capacity(xs.len()))
}

/// The same nearest-rank statistic using caller-reserved storage.
pub fn p95_with(xs: &[f64], sorted: &mut Vec<f64>) -> f64 {
    sorted.clear();
    sorted.extend_from_slice(xs);
    sorted.sort_unstable_by(f64::total_cmp);
    // ⌈95 n / 100⌉ in integers, so that n = 20 gives rank 19 exactly. The rank is at most n.
    let rank = sorted.len().saturating_mul(95).div_ceil(100);
    sorted.get(rank.saturating_sub(1)).copied().unwrap_or(0.0)
}

/// True if at least 90 % of `xs` are within ±5 % of `k × period_ms` for some k in 1..=4. An
/// empty slice, or a period that is not finite or not above 0, gives false.
pub fn quantised(xs: &[f64], period_ms: f64) -> bool {
    if xs.is_empty() || !period_ms.is_finite() || period_ms <= 0.0 {
        return false;
    }
    let near = xs
        .iter()
        .filter(|&&x| near_a_multiple(x, period_ms))
        .count();
    // near / n >= 0.9, in integers.
    near.saturating_mul(10) >= xs.len().saturating_mul(9)
}

/// True if `x` is within ±5 % of 1, 2, 3 or 4 periods. NaN is never near.
fn near_a_multiple(x: f64, period_ms: f64) -> bool {
    MULTIPLES.iter().any(|k| {
        let multiple = k * period_ms;
        (x - multiple).abs() <= BAND * multiple + EPS
    })
}

/// The share (0 to 1) of `xs` that are at or below `limit_ms`. An empty slice gives 0.0. NaN is
/// never at or below a limit.
pub fn share_at_or_below(xs: &[f64], limit_ms: f64) -> f64 {
    if xs.is_empty() {
        return 0.0;
    }
    let at_or_below = xs.iter().filter(|&&x| x <= limit_ms).count();
    at_or_below as f64 / xs.len() as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn p95_of_1_to_20_is_19() {
        let xs: Vec<f64> = (1..=20).map(f64::from).collect();
        assert_eq!(p95(&xs), 19.0);
        // The order of the input does not matter.
        let reversed: Vec<f64> = xs.iter().rev().copied().collect();
        assert_eq!(p95(&reversed), 19.0);
    }

    #[test]
    fn p95_nearest_rank_examples() {
        assert_eq!(p95(&[7.0]), 7.0);
        assert_eq!(p95(&[1.0, 2.0]), 2.0);
        // 103 values: rank ceil(97.85) = 98, so index 97.
        let mut xs = vec![10.0; 100];
        xs.extend([15.0; 3]);
        assert_eq!(p95(&xs), 10.0);
        // 100 values: rank 95, so index 94.
        let xs: Vec<f64> = (1..=100).map(f64::from).collect();
        assert_eq!(p95(&xs), 95.0);
    }

    #[test]
    fn p95_of_empty_is_zero() {
        assert_eq!(p95(&[]), 0.0);
    }

    #[test]
    fn p95_with_nan_does_not_panic() {
        let _ = p95(&[f64::NAN]);
        let _ = p95(&[
            1.0,
            f64::NAN,
            3.0,
            -f64::NAN,
            f64::INFINITY,
            f64::NEG_INFINITY,
        ]);
        // Positive NaN sorts last under total_cmp, so it does not hide the finite values below it.
        let mut xs: Vec<f64> = (1..=20).map(f64::from).collect();
        xs.push(f64::NAN);
        assert!(p95(&xs).is_finite());
    }

    #[test]
    fn quantised_true_for_multiples_of_the_period() {
        let p = 1000.0 / 120.0;
        // All on time.
        assert!(quantised(&[p; 50], p));
        // 90 % at one period, 10 % at two: all are multiples.
        let mut xs = vec![p; 9];
        xs.push(2.0 * p);
        assert!(quantised(&xs, p));
        // k = 3 and k = 4 count; within ±5 % counts.
        assert!(quantised(&[3.0 * p, 4.0 * p, 1.04 * p, 0.96 * p], p));
    }

    #[test]
    fn quantised_needs_90_percent() {
        let p = 1000.0 / 120.0;
        // 9 of 10 near a multiple: exactly 90 %.
        let mut xs = vec![p; 9];
        xs.push(1.5 * p);
        assert!(quantised(&xs, p));
        // 8 of 10: not quantised.
        let mut xs = vec![p; 8];
        xs.extend([1.5 * p, 1.5 * p]);
        assert!(!quantised(&xs, p));
        // 5 × period is outside 1..=4.
        assert!(!quantised(&[5.0 * p; 10], p));
        // VRR-like times that sit between multiples.
        assert!(!quantised(&[10.0; 10], p));
    }

    #[test]
    fn quantised_false_for_empty_or_bad_period() {
        assert!(!quantised(&[], 8.0));
        for period in [0.0, -8.0, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert!(!quantised(&[8.0; 10], period), "period {period}");
        }
    }

    #[test]
    fn quantised_with_nan_values_does_not_panic() {
        let p = 8.0;
        // NaN is never near a multiple.
        assert!(!quantised(&[f64::NAN; 10], p));
        let mut xs = vec![p; 9];
        xs.push(f64::NAN);
        assert!(quantised(&xs, p));
    }

    #[test]
    fn share_at_or_below_counts_the_limit() {
        assert_eq!(share_at_or_below(&[1.0, 2.0, 3.0, 4.0], 2.0), 0.5);
        assert_eq!(share_at_or_below(&[1.0, 2.0, 3.0, 4.0], 10.0), 1.0);
        assert_eq!(share_at_or_below(&[1.0, 2.0, 3.0, 4.0], 0.5), 0.0);
    }

    #[test]
    fn share_at_or_below_of_empty_is_zero() {
        assert_eq!(share_at_or_below(&[], 10.0), 0.0);
    }

    #[test]
    fn share_at_or_below_with_nan_does_not_panic() {
        // NaN is not at or below any limit.
        assert_eq!(share_at_or_below(&[f64::NAN, 1.0], 2.0), 0.5);
        assert_eq!(share_at_or_below(&[1.0], f64::NAN), 0.0);
    }

    #[test]
    fn the_five_percent_band_includes_its_edge_with_slack() {
        assert!(near_a_multiple(21.0, 20.0));
        assert!(near_a_multiple(21.0 + EPS, 20.0));
        assert!(!near_a_multiple(21.0 + 2.0 * EPS, 20.0));
    }
}
