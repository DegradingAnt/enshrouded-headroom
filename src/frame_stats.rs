//! Pure frame-pacing statistics over positive frame intervals in microseconds.
/// Statistics for a window containing at least two positive frame times.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FrameStats {
    /// Median by nearest rank, in microseconds.
    pub p50_us: u64,
    /// 95th percentile by nearest rank, in microseconds.
    pub p95_us: u64,
    /// 99th percentile by nearest rank, in microseconds.
    pub p99_us: u64,
    /// Slowest frame, in microseconds.
    pub max_us: u64,
    /// Reciprocal of the mean frame time, expressed as frames per second.
    pub average_fps: f64,
    /// Reciprocal of the mean slowest ceil(1%) frame times.
    pub low_1_percent_fps: f64,
    /// Frames strictly longer than both twice the median and 25 milliseconds.
    pub hitches: usize,
}
/// Compute without allocating, using caller-owned scratch. Insufficient capacity gives no data.
/// Zero intervals are ignored; fewer than two positive intervals give no data.
pub fn calculate(frames_us: &[u64], scratch: &mut [u64]) -> Option<FrameStats> {
    let used = scratch.get_mut(..frames_us.len())?;
    used.copy_from_slice(frames_us);
    calculate_in_place(used)
}
/// Compute using caller-owned scratch already populated with frame intervals; order is not retained.
/// No allocation occurs. Zero intervals are ignored and at least two positive samples are needed.
pub fn calculate_in_place(scratch: &mut [u64]) -> Option<FrameStats> {
    let mut count = 0usize;
    for index in 0..scratch.len() {
        let value = scratch.get(index).copied()?;
        if value != 0 {
            *scratch.get_mut(count)? = value;
            count = count.checked_add(1)?;
        }
    }
    // Keep ignored zeros in the unused tail so repeated calls retain the same sample multiset.
    scratch.get_mut(count..)?.fill(0);
    let sorted = scratch.get_mut(..count)?;
    if count < 2 {
        return None;
    }
    sorted.sort_unstable();
    let percentile = |share: usize| {
        sorted
            .get(count.checked_mul(share)?.div_ceil(100).checked_sub(1)?)
            .copied()
    };
    let p50 = percentile(50)?;
    let sum = sorted
        .iter()
        .try_fold(0u128, |sum, &value| sum.checked_add(u128::from(value)))?;
    let low_count = count.div_ceil(100);
    let slowest = sorted.get(count.checked_sub(low_count)?..)?;
    let slow_sum = slowest
        .iter()
        .try_fold(0u128, |sum, &value| sum.checked_add(u128::from(value)))?;
    Some(FrameStats {
        p50_us: p50,
        p95_us: percentile(95)?,
        p99_us: percentile(99)?,
        max_us: sorted.last().copied()?,
        average_fps: 1_000_000.0 * (count as f64) / (sum as f64),
        low_1_percent_fps: 1_000_000.0 * (low_count as f64) / (slow_sum as f64),
        // Doubling a u64 median in u128 cannot overflow.
        hitches: sorted
            .iter()
            .filter(|&&value| value > 25_000 && u128::from(value) > u128::from(p50) * 2)
            .count(),
    })
}

/// Format debug report text. This allocates; the worker calls it only when emitting.
pub fn debug_line(stats: Option<&FrameStats>) -> String {
    match stats {
        Some(s) => format!(
            "frames: p50 {:.2} ms, p99 {:.2} ms, max {:.2} ms, hitches {}",
            s.p50_us as f64 / 1000.0,
            s.p99_us as f64 / 1000.0,
            s.max_us as f64 / 1000.0,
            s.hitches
        ),
        None => "frames: no data".into(),
    }
}
/// Info status suffix for the same last-two-second window.
pub fn info_suffix(stats: Option<&FrameStats>) -> String {
    match stats {
        Some(s) => format!(
            "1% low {:.0} fps, hitches {}",
            s.low_1_percent_fps, s.hitches
        ),
        None => "1% low no data, hitches no data".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn known_unsorted_window_gives_exact_ranks_means_and_hitches() {
        let frames = [40_000, 10_000, 20_000, 30_000, 50_000];
        let mut scratch = [0; 5];
        let stats = calculate(&frames, &mut scratch).unwrap();
        assert_eq!(
            (stats.p50_us, stats.p95_us, stats.p99_us, stats.max_us),
            (30_000, 50_000, 50_000, 50_000)
        );
        assert!((stats.average_fps - 100.0 / 3.0).abs() < 1e-10);
        assert_eq!(stats.low_1_percent_fps, 20.0);
        assert_eq!(stats.hitches, 0);
        assert_eq!(calculate(&[], &mut scratch), None);
        assert_eq!(calculate(&[1], &mut scratch), None);
    }
    #[test]
    fn formatter_names_units_and_reports_no_data_explicitly() {
        let stats = calculate(&[10_000, 20_000, 30_000], &mut [0; 3]).unwrap();
        assert_eq!(
            debug_line(Some(&stats)),
            "frames: p50 20.00 ms, p99 30.00 ms, max 30.00 ms, hitches 0"
        );
        assert_eq!(info_suffix(Some(&stats)), "1% low 33 fps, hitches 0");
        assert_eq!(debug_line(None), "frames: no data");
        assert_eq!(info_suffix(None), "1% low no data, hitches no data");
    }

    #[test]
    fn low_uses_ceil_one_percent_mean_time_and_hitches_are_strict() {
        let mut frames = [1_000; 102];
        frames[100] = 40_000;
        frames[101] = 60_000;
        let stats = calculate(&frames, &mut [0; 102]).unwrap();
        assert_eq!(
            (stats.p50_us, stats.p95_us, stats.p99_us, stats.max_us),
            (1_000, 1_000, 40_000, 60_000)
        );
        assert_eq!(stats.average_fps, 510.0);
        assert_eq!(stats.low_1_percent_fps, 20.0);
        assert_eq!(stats.hitches, 2);
        let boundary = calculate(&[12_500, 12_500, 25_000, 25_001], &mut [0; 4]).unwrap();
        assert_eq!(boundary.hitches, 1);
        let double = calculate(&[20_000, 20_000, 40_000, 40_001], &mut [0; 4]).unwrap();
        assert_eq!(double.hitches, 1);
    }
    #[test]
    fn steady_frames_with_periodic_120ms_pulses_count_each_hitch() {
        // Each group is 29 steady frames plus a pulse:505.7ms, the nearest such group to500ms.
        let mut frames = [13_300; 120];
        for index in [29, 59, 89, 119] {
            frames[index] = 120_000;
        }
        let stats = calculate(&frames, &mut [0; 120]).unwrap();
        assert_eq!(
            (stats.p50_us, stats.p95_us, stats.p99_us, stats.max_us),
            (13_300, 13_300, 120_000, 120_000)
        );
        assert_eq!(stats.hitches, 4);
        assert!((stats.low_1_percent_fps - 25.0 / 3.0).abs() < 1e-12);
    }
    #[test]
    fn zeros_short_scratch_and_extreme_intervals_remain_defined() {
        assert_eq!(calculate(&[0, 0], &mut [0; 2]), None);
        assert_eq!(calculate(&[0, 1], &mut [0; 2]), None);
        assert_eq!(calculate(&[1, 2, 3], &mut [0; 2]), None);
        let extreme = calculate(&[u64::MAX, u64::MAX], &mut [0; 2]).unwrap();
        assert_eq!(extreme.hitches, 0);
        assert!(extreme.average_fps.is_finite() && extreme.average_fps > 0.0);
        assert_eq!(extreme.low_1_percent_fps, extreme.average_fps);
        let fast = calculate(&[1, 1], &mut [0; 2]).unwrap();
        assert_eq!(fast.average_fps, 1_000_000.0);
    }
    #[test]
    fn repeating_in_place_calculation_preserves_zero_filtered_samples() {
        let mut scratch = [0, 10_000, 20_000, 0];
        let first = calculate_in_place(&mut scratch).unwrap();
        assert_eq!(calculate_in_place(&mut scratch), Some(first));
    }
}
