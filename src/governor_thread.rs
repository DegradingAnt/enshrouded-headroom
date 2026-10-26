//! Planning and diagnostics for the pending 50 ms governor runtime.

/// The value for a validated governor action; invalid knob/step pairs are refused.
pub fn knob_value(knob: usize, step: u8) -> Option<f32> {
    let values = match knob {
        0 => &crate::knobs::OBJECT_STEPS,
        1 => &crate::knobs::SCATTER_STEPS,
        2 => &crate::knobs::TERRAIN_STEPS,
        _ => return None,
    };
    values.get(usize::from(step)).copied()
}

/// The grass knob needs the game's High setting, its opt-in setting and a validated probe.
pub fn scatter_enabled(level: u32, configured: bool, probe_ok: bool) -> bool {
    level == 3 && configured && probe_ok
}

/// Read fixed debug steps only when debug logging was explicitly configured.
pub fn forced_steps(
    level: crate::config::LogLevel,
    debug: &crate::config::DebugCfg,
) -> [Option<u8>; 3] {
    if level == crate::config::LogLevel::Debug {
        [
            debug.force_step_object,
            debug.force_step_scatter,
            debug.force_step_terrain,
        ]
    } else {
        [None; 3]
    }
}

/// State reported by the governor's periodic status line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeState {
    /// Frames and the world signal are arriving.
    Active,
    /// Presents or the world signal are absent.
    Inactive,
    /// A previous undo temporarily blocks a raise.
    BackingOff,
}

/// Format the one info status line; this allocation occurs only when the line is emitted.
pub fn status_line(
    p50_ms: f64,
    p95_ms: f64,
    target_ms: f64,
    steps: [u8; 3],
    state: RuntimeState,
) -> String {
    let state = match state {
        RuntimeState::Active => "active",
        RuntimeState::Inactive => "inactive",
        RuntimeState::BackingOff => "backing off",
    };
    let [object, scatter, terrain] = steps;
    format!("governor: p50 {p50_ms:.2} ms, p95 {p95_ms:.2} ms, target {target_ms:.2} ms; steps object={object} scatter={scatter} terrain={terrain}; {state}")
}

/// A tick's latched hold and one-time signal reports, without allocating log text.
#[derive(Debug, Clone, Copy, Default)]
struct SignalEvents {
    /// Hold every current knob until the scene signal recovers.
    freeze: bool,
    /// Emit the world-signal warning on this transition only.
    report_missing: bool,
    /// Emit the unknown-baseline warning once for each counter movement.
    report_unknown: bool,
}

/// Detect missing scene updates and changes of the hook's diagnostic counter.
struct SignalWatch {
    /// Last observed object hook timestamp.
    last_seen: u64,
    /// Time at which that timestamp last advanced, or monitoring began.
    advanced_at_s: f64,
    /// Missing signal warning/hold is latched until a fresh timestamp arrives.
    missing: bool,
    /// Last diagnostic count; inequality also catches a wrapping movement.
    unknown: u32,
}

impl SignalWatch {
    /// Begin watching when the runtime starts.
    fn new(now_s: f64) -> Self {
        Self {
            last_seen: 0,
            advanced_at_s: now_s,
            missing: false,
            unknown: 0,
        }
    }

    /// Observe atomic snapshots; this changes only thread-local state.
    fn observe(
        &mut self,
        now_s: f64,
        installed: bool,
        seen: u64,
        frames_arriving: bool,
        unknown: u32,
    ) -> SignalEvents {
        if seen != self.last_seen || now_s < self.advanced_at_s || !installed {
            self.last_seen = seen;
            self.advanced_at_s = now_s;
            self.missing = false;
        }
        let report_missing =
            !self.missing && installed && frames_arriving && now_s - self.advanced_at_s >= 10.0;
        self.missing |= report_missing;
        let report_unknown = unknown != self.unknown;
        self.unknown = unknown;
        SignalEvents {
            freeze: self.missing,
            report_missing,
            report_unknown,
        }
    }
}

/// Preallocated one-reader storage for the present ring and its converted frame intervals.
struct FrameReader {
    /// Last consumed sequence, including entries a concurrent producer made untrustworthy.
    sequence: u64,
    /// Last trusted timestamp, or None across a lapped/backwards boundary.
    previous: Option<u64>,
    /// Scratch bounded by the ring length.
    qpc: Vec<u64>,
    /// Converted finite positive intervals, bounded by the same length.
    ms: Vec<f64>,
}

impl FrameReader {
    /// Reserve storage once, before the loop begins.
    fn new() -> Self {
        Self {
            sequence: 0,
            previous: None,
            qpc: Vec::with_capacity(crate::frame_timer::RING_LEN),
            ms: Vec::with_capacity(crate::frame_timer::RING_LEN),
        }
    }

    /// Read the current ring batch, excluding a made-up interval across any missing sequence.
    fn read(&mut self, ring: &crate::frame_timer::Ring, to_ms: impl Fn(u64) -> f64) -> &[f64] {
        self.qpc.clear();
        self.ms.clear();
        let next = ring.read_since(self.sequence, &mut self.qpc);
        if next.saturating_sub(self.sequence) != self.qpc.len() as u64 {
            self.previous = None;
        }
        self.sequence = next;
        for &timestamp in &self.qpc {
            if let Some(delta) = self.previous.and_then(|old| timestamp.checked_sub(old)) {
                let ms = to_ms(delta);
                if ms.is_finite() && ms > 0.0 {
                    self.ms.push(ms);
                }
            }
            self.previous = Some(timestamp);
        }
        &self.ms
    }
}

/// Debug ForceStep parser warnings are relevant only when those test settings are honored.
pub fn warning_visible(level: crate::config::LogLevel, warning: &str) -> bool {
    level == crate::config::LogLevel::Debug
        || ![
            "[Debug] ForceStepObjectDistance:",
            "[Debug] ForceStepScatterDistance:",
            "[Debug] ForceStepTerrainDistance:",
        ]
        .iter()
        .any(|prefix| warning.starts_with(prefix))
}

/// Limits applied after automatic or fixed debug requests, before any shared atomic is written.
struct StepLimits {
    /// Configured availability; grass additionally needs the High setting and a good probe.
    enabled: [bool; 3],
    /// Configured governor maxima.
    max: [u8; 3],
    /// Unknown memory blocks all raises conservatively.
    vram_free_mib: Option<u64>,
    /// The configured minimum budget headroom.
    min_vram_free_mib: u32,
    /// A failed probe resets grass even if a debug hold requested a raise.
    probe_failed: bool,
    /// Last published counter A.
    last_a: u32,
    /// Highest measured safe scatter step.
    scatter_ceiling: u8,
}

/// Apply hard availability, capacity and memory limits to an entire proposed batch.
fn limit_steps(current: [u8; 3], requested: [u8; 3], limits: &StepLimits) -> [u8; 3] {
    let mut next = std::array::from_fn(|knob| {
        let before = current.get(knob).copied().unwrap_or(0);
        let wanted = requested
            .get(knob)
            .copied()
            .unwrap_or(0)
            .min(limits.max.get(knob).copied().unwrap_or(0))
            .min(4);
        if limits.enabled.get(knob) != Some(&true) {
            return 0;
        }
        if wanted > before
            && limits
                .vram_free_mib
                .is_none_or(|free| free < u64::from(limits.min_vram_free_mib))
        {
            before
        } else {
            wanted
        }
    });
    if let Some(grass) = next.get_mut(1) {
        let before = current.get(1).copied().unwrap_or(0);
        *grass = (*grass).min(limits.scatter_ceiling);
        if limits.probe_failed {
            *grass = 0;
        } else if limits.last_a >= crate::features::scatter_probe::WARN_A {
            *grass = (*grass).min(before.saturating_sub(1));
        } else if *grass > before {
            let radii = (knob_value(1, before), knob_value(1, *grass));
            if !radii.0.zip(radii.1).is_some_and(|(now, next)| {
                crate::features::scatter_probe::headroom_ok(limits.last_a, now, next)
            }) {
                *grass = before;
            }
        }
    }
    next
}

/// Finish all fallible/panicking planning and logging before the non-panicking atomic batch.
fn plan_then_publish<T>(plan: impl FnOnce() -> T, publish: impl FnOnce(T)) {
    let values = plan();
    publish(values);
}

#[cfg(test)]
mod tests {
    use std::alloc::{GlobalAlloc, Layout, System};
    use std::cell::Cell;

    thread_local! {
        /// Count only this test thread while its explicit measurement scope is active.
        static TRACK_ALLOCATIONS: Cell<bool> = const { Cell::new(false) };
        /// Number of allocation/reallocation attempts in that measurement scope.
        static ALLOCATIONS: Cell<usize> = const { Cell::new(0) };
    }

    /// System allocator with per-test-thread instrumentation; production builds never include it.
    struct MeasuredAllocator;

    /// Record an allocation without allocating or depending on initialized TLS during teardown.
    fn record_allocation() {
        let _ = TRACK_ALLOCATIONS.try_with(|active| {
            if active.get() {
                let _ = ALLOCATIONS.try_with(|count| count.set(count.get().saturating_add(1)));
            }
        });
    }

    // SAFETY: every operation delegates its exact pointer/layout contract to System. The only
    // extra work is thread-local Cell bookkeeping; it never changes memory ownership or layout.
    unsafe impl GlobalAlloc for MeasuredAllocator {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            record_allocation();
            // SAFETY: forwarded unchanged from GlobalAlloc's caller.
            unsafe { System.alloc(layout) }
        }
        unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
            record_allocation();
            // SAFETY: forwarded unchanged from GlobalAlloc's caller.
            unsafe { System.alloc_zeroed(layout) }
        }
        unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
            // SAFETY: forwarded unchanged from GlobalAlloc's caller.
            unsafe { System.dealloc(ptr, layout) }
        }
        unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, size: usize) -> *mut u8 {
            record_allocation();
            // SAFETY: forwarded unchanged from GlobalAlloc's caller.
            unsafe { System.realloc(ptr, layout, size) }
        }
    }

    /// Crate-test-only allocator; unrelated concurrently running tests leave their TLS flag off.
    #[global_allocator]
    static TEST_ALLOCATOR: MeasuredAllocator = MeasuredAllocator;

    /// Disable instrumentation even if a measured helper panics.
    struct AllocationScope;
    impl Drop for AllocationScope {
        fn drop(&mut self) {
            TRACK_ALLOCATIONS.with(|active| active.set(false));
        }
    }

    /// Measure one synchronous pure helper batch, without counting assertions or setup.
    fn allocation_count(body: impl FnOnce()) -> usize {
        ALLOCATIONS.with(|count| count.set(0));
        TRACK_ALLOCATIONS.with(|active| active.set(true));
        let scope = AllocationScope;
        body();
        drop(scope);
        ALLOCATIONS.with(Cell::get)
    }

    #[test]
    fn owned_tick_helpers_allocate_nothing_after_storage_is_reserved() {
        let positive = allocation_count(|| {
            std::hint::black_box(Vec::<u8>::with_capacity(16));
        });
        assert!(
            positive > 0,
            "the counter must detect a deliberate allocation"
        );
        let ring = crate::frame_timer::Ring::new();
        let mut reader = FrameReader::new();
        let mut watch = SignalWatch::new(0.0);
        let debug = crate::config::DebugCfg::default();
        let limits = StepLimits {
            enabled: [true; 3],
            max: [4; 3],
            vram_free_mib: Some(2048),
            min_vram_free_mib: 1024,
            probe_failed: false,
            last_a: 0,
            scatter_ceiling: 0,
        };
        let allocations = allocation_count(|| {
            for n in 0u64..128 {
                ring.push(n.saturating_mul(10));
                std::hint::black_box(reader.read(&ring, |delta| delta as f64));
                std::hint::black_box(watch.observe(n as f64 * 0.05, true, n, true, n as u32));
                std::hint::black_box(limit_steps([0; 3], [1; 3], &limits));
                std::hint::black_box(forced_steps(crate::config::LogLevel::Debug, &debug));
                std::hint::black_box(scatter_enabled(3, true, true));
                std::hint::black_box(knob_value(2, 4));
                plan_then_publish(
                    || [1.0f32, 90.0, 1.0],
                    |values| {
                        std::hint::black_box(values);
                    },
                );
            }
        });
        assert_eq!(allocations, 0);
    }

    #[test]
    fn current_pure_governor_allocates_and_needs_the_requested_scratch_api() {
        let mut governor = crate::governor::Governor::new(
            crate::config::Config::default().governor,
            vec![
                crate::governor::KnobSpec {
                    enabled: true,
                    max_step: 4
                };
                3
            ],
        );
        let inputs = crate::governor::Inputs {
            now_s: 1.0,
            new_frames_ms: &[10.0],
            world_active: true,
            period_ms: 0.0,
            vram_free_mib: Some(2048),
            baseline_changed: &[false; 3],
        };
        let allocations = allocation_count(|| {
            std::hint::black_box(governor.tick(&inputs));
        });
        assert!(allocations > 0,"this checkpoint must expose the current allocating API rather than claim the loop is done");
    }
    use super::*;

    #[test]
    fn a_panic_while_planning_never_publishes_any_knob_value() {
        let knobs = [
            crate::knobs::KnobShared::new(0.85),
            crate::knobs::KnobShared::new(100.0),
            crate::knobs::KnobShared::new(1.5),
        ];
        let before = knobs.each_ref().map(crate::knobs::KnobShared::value);
        let result = crate::guard::catch("injected governor tick", || {
            plan_then_publish(
                || {
                    let _planned = [0.52f32, 127.0, 2.0];
                    panic!("injected before publication")
                },
                |values: [f32; 3]| {
                    for (knob, value) in knobs.iter().zip(values) {
                        knob.set(value);
                    }
                },
            );
        });
        assert!(result.is_err());
        assert_eq!(
            knobs.each_ref().map(crate::knobs::KnobShared::value),
            before
        );
        plan_then_publish(
            || [0.72f32, 110.0, 1.75],
            |values| {
                for (knob, value) in knobs.iter().zip(values) {
                    knob.set(value);
                }
            },
        );
        assert_eq!(
            knobs.each_ref().map(crate::knobs::KnobShared::value),
            [0.72, 110.0, 1.75]
        );
    }

    #[test]
    fn hard_step_limits_block_unknown_vram_and_unsafe_grass_even_when_forced() {
        let mut limits = StepLimits {
            enabled: [true; 3],
            max: [4; 3],
            vram_free_mib: None,
            min_vram_free_mib: 1024,
            probe_failed: false,
            last_a: 0,
            scatter_ceiling: 4,
        };
        assert_eq!(limit_steps([1, 1, 1], [2, 2, 2], &limits), [1, 1, 1]);
        limits.vram_free_mib = Some(2048);
        limits.scatter_ceiling = 0;
        assert_eq!(limit_steps([1, 0, 1], [2, 4, 2], &limits), [2, 0, 2]);
        limits.scatter_ceiling = 4;
        limits.last_a = crate::features::scatter_probe::WARN_A;
        assert_eq!(limit_steps([1, 3, 1], [2, 4, 2], &limits), [2, 2, 2]);
        limits.last_a = 0;
        limits.probe_failed = true;
        assert_eq!(limit_steps([1, 3, 1], [2, 4, 2], &limits), [2, 0, 2]);
        limits.probe_failed = false;
        limits.enabled[1] = false;
        assert_eq!(limit_steps([1, 3, 1], [2, 4, 2], &limits), [2, 0, 2]);
    }

    #[test]
    fn force_step_warnings_are_emitted_only_with_debug_logging() {
        for level in [
            crate::config::LogLevel::Error,
            crate::config::LogLevel::Warn,
            crate::config::LogLevel::Info,
        ] {
            assert!(!warning_visible(
                level,
                "[Debug] ForceStepObjectDistance: bad value"
            ));
            assert!(warning_visible(level, "[General] TargetFps: bad value"));
            assert!(warning_visible(
                level,
                "[Debug] ForceStepNotReal: bad value"
            ));
            assert!(warning_visible(
                level,
                "unknown [Debug] ForceStepObjectDistance: bad value"
            ));
            let (_, warnings) = crate::config::parse("[Debug]\nForceStepObjectDistance=9\nForceStepScatterDistance=bad\nForceStepTerrainDistance=-1");
            assert_eq!(warnings.len(), 3);
            for warning in warnings {
                assert!(!warning_visible(level, &warning), "{warning}");
                assert!(warning_visible(crate::config::LogLevel::Debug, &warning));
            }
        }
        assert!(warning_visible(
            crate::config::LogLevel::Debug,
            "[Debug] ForceStepTerrainDistance: bad value"
        ));
    }

    #[test]
    fn frame_reader_keeps_intervals_across_ticks_and_discards_backwards_time() {
        let ring = crate::frame_timer::Ring::new();
        let mut reader = FrameReader::new();
        for time in [1000, 1010, 1025] {
            ring.push(time);
        }
        assert_eq!(reader.read(&ring, |delta| delta as f64), &[10.0, 15.0]);
        ring.push(1040);
        assert_eq!(reader.read(&ring, |delta| delta as f64), &[15.0]);
        ring.push(1030);
        ring.push(1045);
        assert_eq!(reader.read(&ring, |delta| delta as f64), &[15.0]);
        assert!(reader.read(&ring, |delta| delta as f64).is_empty());
        let capacities = (reader.qpc.capacity(), reader.ms.capacity());
        for n in 0..5000 {
            ring.push(2000 + n);
        }
        let _ = reader.read(&ring, |delta| delta as f64);
        assert_eq!((reader.qpc.capacity(), reader.ms.capacity()), capacities);
        assert!(reader.ms.len() < crate::frame_timer::RING_LEN);
        assert!(
            reader.ms.iter().all(|ms| *ms == 1.0),
            "no synthetic cross-gap interval: {:?}",
            reader.ms.first()
        );
    }

    #[test]
    fn missing_world_signal_freezes_at_ten_seconds_until_it_advances() {
        let mut watch = SignalWatch::new(0.0);
        assert!(!watch.observe(9.999, true, 0, true, 0).freeze);
        assert!(!watch.observe(10.0, true, 0, false, 0).freeze);
        let missing = watch.observe(10.0, true, 0, true, 0);
        assert!(missing.freeze && missing.report_missing);
        let again = watch.observe(11.0, true, 0, true, 0);
        assert!(again.freeze && !again.report_missing);
        assert!(watch.observe(12.0, true, 0, false, 0).freeze);
        assert!(!watch.observe(13.0, true, 100, true, 0).freeze);
        assert!(
            !SignalWatch::new(0.0)
                .observe(20.0, false, 0, true, 0)
                .freeze
        );
    }

    #[test]
    fn unknown_baseline_is_reported_once_per_counter_movement() {
        let mut watch = SignalWatch::new(0.0);
        assert!(!watch.observe(0.0, false, 0, false, 0).report_unknown);
        assert!(watch.observe(1.0, false, 0, false, 2).report_unknown);
        assert!(!watch.observe(2.0, false, 0, false, 2).report_unknown);
        assert!(watch.observe(3.0, false, 0, false, u32::MAX).report_unknown);
        assert!(watch.observe(4.0, false, 0, false, 0).report_unknown);
    }

    #[test]
    fn status_line_names_percentiles_target_steps_and_state() {
        assert_eq!(status_line(10.0,12.0,1000.0/75.0,[1,0,4],RuntimeState::Active), "governor: p50 10.00 ms, p95 12.00 ms, target 13.33 ms; steps object=1 scatter=0 terrain=4; active");
        assert!(
            status_line(0.0, 0.0, 13.33, [0; 3], RuntimeState::Inactive).ends_with("; inactive")
        );
        assert!(
            status_line(10.0, 12.0, 13.33, [0; 3], RuntimeState::BackingOff)
                .ends_with("; backing off")
        );
    }

    #[test]
    fn forced_steps_are_accepted_only_with_debug_logging() {
        let debug = crate::config::DebugCfg {
            force_step_object: Some(2),
            force_step_scatter: Some(3),
            force_step_terrain: Some(4),
            ..Default::default()
        };
        assert_eq!(
            forced_steps(crate::config::LogLevel::Debug, &debug),
            [Some(2), Some(3), Some(4)]
        );
        for level in [crate::config::LogLevel::Info, crate::config::LogLevel::Warn] {
            assert_eq!(forced_steps(level, &debug), [None; 3]);
        }
    }

    #[test]
    fn grass_enable_requires_high_setting_config_and_a_good_probe() {
        for level in 0..=4 {
            for configured in [false, true] {
                for probe_ok in [false, true] {
                    assert_eq!(
                        scatter_enabled(level, configured, probe_ok),
                        level == 3 && configured && probe_ok
                    );
                }
            }
        }
        assert_eq!(
            crate::features::scatter_probe::ceiling_step(
                &crate::knobs::SCATTER_STEPS,
                crate::features::scatter_probe::R_MAX_M
            ),
            0
        );
    }

    #[test]
    fn actions_map_to_knob_values() {
        assert_eq!(knob_value(2, 4), Some(2.0));
        assert_eq!(knob_value(0, 1), Some(0.85));
        assert_eq!(knob_value(1, 0), Some(90.0));
        assert_eq!(knob_value(3, 0), None);
        assert_eq!(knob_value(0, 5), None);
    }
}

