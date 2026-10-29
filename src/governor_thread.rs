//! The guarded 50 ms governor thread and its allocation-free planning boundary.

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

/// Periodic scatter diagnostics, sampled only by the configured debug governor loop.
#[derive(Default)]
struct ScatterDiagnostics {
    /// Largest scene-view peak observed by this governor run, retained across scene resets.
    session_peak_a: u32,
    /// Time of the last emitted counter report.
    reported_at_s: f64,
}

impl ScatterDiagnostics {
    /// Observe a scene-view peak and return a value only when the two-second report is due.
    fn observe(&mut self, now_s: f64, peak_a: u32) -> Option<u32> {
        self.session_peak_a = self.session_peak_a.max(peak_a);
        if now_s - self.reported_at_s >= 2.0 {
            self.reported_at_s = now_s;
            Some(self.session_peak_a)
        } else {
            None
        }
    }
}

/// Format counters only when the debug report is emitted.
fn scatter_probe_line(counters: [u32; 6], session_peak_a: u32) -> String {
    let [a, peak_a, b, peak_b, c, peak_c] = counters;
    format!("scatter probe: A={a} peak_A={peak_a} B={b} peak_B={peak_b} C={c} peak_C={peak_c} session_peak_A={session_peak_a}")
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
    /// Positive microsecond intervals paired with their actual frame-end QPC timestamps.
    intervals_us: Vec<(u64, u64)>,
}

impl FrameReader {
    /// Reserve storage once, before the loop begins.
    fn new() -> Self {
        Self {
            sequence: 0,
            previous: None,
            qpc: Vec::with_capacity(crate::frame_timer::RING_LEN),
            ms: Vec::with_capacity(crate::frame_timer::RING_LEN),
            intervals_us: Vec::with_capacity(crate::frame_timer::RING_LEN),
        }
    }

    /// Read the current ring batch, excluding a made-up interval across any missing sequence.
    fn read(&mut self, ring: &crate::frame_timer::Ring, to_ms: impl Fn(u64) -> f64) -> &[f64] {
        self.qpc.clear();
        self.ms.clear();
        self.intervals_us.clear();
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
                    self.intervals_us
                        .push((timestamp, (ms * 1000.0).round().max(1.0) as u64));
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

/// One atomic/input snapshot. No game pointer is retained by the runtime.
struct Sample<'a> {
    now_s: f64,
    frames: &'a [f64],
    world_active: bool,
    installed: bool,
    seen: u64,
    unknown: u32,
    baseline_seq: [u32; 3],
    period_ms: f64,
    level: u32,
    probe_ok: bool,
    probe_failed: bool,
    last_a: u32,
    free_mib: Option<u64>,
}

/// A fully planned publication; all validation and logging precede the atomic writes.
struct Plan {
    steps: [u8; 3],
    values: [f32; 3],
    signals: SignalEvents,
    report_probe_failure: bool,
    state: RuntimeState,
}

/// Thread-local state and reserved storage. No field is shared with a game hook.
struct Runtime {
    cfg: crate::config::Config,
    governor: crate::governor::Governor,
    scratch: crate::governor::TickScratch,
    watch: SignalWatch,
    baseline_seq: [u32; 3],
    steps: [u8; 3],
    probe_failure_reported: bool,
    history: std::collections::VecDeque<(f64, f64)>,
    sorted: Vec<f64>,
}

impl Runtime {
    /// Allocate bounded buffers before the first tick.
    fn new(cfg: crate::config::Config) -> Self {
        let enabled = [
            cfg.knobs.enable_object_distance,
            false,
            cfg.knobs.enable_terrain_distance,
        ];
        let maxima = [
            cfg.knobs.max_object_distance,
            cfg.knobs.max_scatter_distance,
            cfg.knobs.max_terrain_distance,
        ];
        let specs = enabled
            .into_iter()
            .zip(maxima)
            .map(|(enabled, max_step)| crate::governor::KnobSpec { enabled, max_step })
            .collect();
        Self {
            cfg,
            governor: crate::governor::Governor::new(cfg.governor, specs),
            scratch: crate::governor::TickScratch::new(3, crate::frame_timer::RING_LEN),
            watch: SignalWatch::new(0.0),
            baseline_seq: [0; 3],
            steps: [0; 3],
            probe_failure_reported: false,
            history: std::collections::VecDeque::with_capacity(crate::frame_timer::RING_LEN),
            sorted: Vec::with_capacity(crate::frame_timer::RING_LEN),
        }
    }

    /// Plan one batch without I/O or allocation. A missing world signal holds every current step,
    /// including debug holds, menu changes and probe safety reductions, until the signal recovers.
    fn plan(&mut self, sample: &Sample<'_>) -> Plan {
        let signals = self.watch.observe(
            sample.now_s,
            sample.installed,
            sample.seen,
            !sample.frames.is_empty(),
            sample.unknown,
        );
        for &ms in sample
            .frames
            .iter()
            .filter(|ms| ms.is_finite() && **ms > 0.0)
        {
            if self.history.len() == crate::frame_timer::RING_LEN {
                self.history.pop_front();
            }
            self.history.push_back((sample.now_s, ms));
        }
        while self
            .history
            .front()
            .is_some_and(|(t, _)| sample.now_s - t > self.cfg.governor.window_seconds)
        {
            self.history.pop_front();
        }
        let report_probe_failure =
            !signals.freeze && sample.probe_failed && !self.probe_failure_reported;
        if report_probe_failure {
            self.probe_failure_reported = true;
        }
        if !signals.freeze {
            let limits = StepLimits {
                enabled: [
                    self.cfg.knobs.enable_object_distance,
                    scatter_enabled(
                        sample.level,
                        self.cfg.knobs.enable_scatter_distance,
                        sample.probe_ok,
                    ),
                    self.cfg.knobs.enable_terrain_distance,
                ],
                max: [
                    self.cfg.knobs.max_object_distance,
                    self.cfg.knobs.max_scatter_distance,
                    self.cfg.knobs.max_terrain_distance,
                ],
                vram_free_mib: sample.free_mib,
                min_vram_free_mib: self.cfg.governor.min_vram_free_mib,
                probe_failed: sample.probe_failed,
                last_a: sample.last_a,
                scatter_ceiling: u8::try_from(crate::features::scatter_probe::ceiling_step(
                    &crate::knobs::SCATTER_STEPS,
                    crate::features::scatter_probe::R_MAX_M,
                ))
                .unwrap_or(0),
            };
            let forced = forced_steps(self.cfg.general.log_level, &self.cfg.debug);
            let requested = std::array::from_fn(|k| {
                forced
                    .get(k)
                    .copied()
                    .flatten()
                    .unwrap_or(self.steps.get(k).copied().unwrap_or(0))
            });
            let held = limit_steps(self.steps, requested, &limits);
            for knob in 0..3 {
                let enabled = limits.enabled.get(knob) == Some(&true);
                self.governor.set_enabled(knob, enabled);
                self.governor.force_step(
                    knob,
                    forced
                        .get(knob)
                        .copied()
                        .flatten()
                        .map(|_| held.get(knob).copied().unwrap_or(0)),
                );
            }
            let changed = std::array::from_fn::<_, 3, _>(|k| {
                sample.baseline_seq.get(k) != self.baseline_seq.get(k)
            });
            self.baseline_seq = sample.baseline_seq;
            self.governor.tick_with(
                &mut self.scratch,
                &crate::governor::Inputs {
                    now_s: sample.now_s,
                    new_frames_ms: sample.frames,
                    world_active: sample.world_active,
                    period_ms: sample.period_ms,
                    vram_free_mib: sample.free_mib,
                    baseline_changed: &changed,
                },
            );
            let proposed =
                std::array::from_fn(|k| self.governor.steps().get(k).copied().unwrap_or(0));
            let next = limit_steps(self.steps, proposed, &limits);
            for knob in 0..3 {
                let wanted = next.get(knob).copied().unwrap_or(0);
                if proposed.get(knob).copied().unwrap_or(0) > wanted {
                    self.governor.veto_raise(knob);
                }
                if self.governor.steps().get(knob).copied() != Some(wanted) {
                    self.governor.force_step(knob, Some(wanted));
                    if forced.get(knob).is_none_or(Option::is_none) {
                        self.governor.force_step(knob, None);
                    }
                }
            }
            self.steps = next;
        }
        let state = if signals.freeze {
            RuntimeState::Inactive
        } else {
            match self.governor.status().state {
                crate::governor::State::Active => RuntimeState::Active,
                crate::governor::State::Inactive => RuntimeState::Inactive,
                crate::governor::State::BackingOff => RuntimeState::BackingOff,
            }
        };
        Plan {
            steps: self.steps,
            values: std::array::from_fn(|k| {
                knob_value(k, self.steps.get(k).copied().unwrap_or(0)).unwrap_or(1.0)
            }),
            signals,
            report_probe_failure,
            state,
        }
    }

    /// Percentiles of the bounded recent frame window, using reserved sort storage.
    fn percentiles(&mut self) -> (f64, f64) {
        self.sorted.clear();
        self.sorted.extend(self.history.iter().map(|(_, ms)| *ms));
        self.sorted.sort_unstable_by(f64::total_cmp);
        let at = |percent: usize| {
            self.sorted
                .get(
                    self.sorted
                        .len()
                        .saturating_mul(percent)
                        .div_ceil(100)
                        .saturating_sub(1),
                )
                .copied()
                .unwrap_or(0.0)
        };
        (at(50), at(95))
    }
}

/// Query only during the first device's live lease, using facts from its exact instance generation.
/// Unknown capabilities and missing selected commands leave every costly raise blocked.
fn query_vram_free_mib() -> Option<u64> {
    use ash::vk::{self, Handle};
    crate::vk::with_game_device_capabilities(|game, gipa, instance, api| {
        let api = api?;
        if gipa == 0 {
            return None;
        }
        // SAFETY: the lease supplies this exact parent's real loader and matching live handles.
        let gipa: vk::PFN_vkGetInstanceProcAddr = unsafe { std::mem::transmute(gipa) };
        let instance = vk::Instance::from_raw(instance as u64);
        let physical = vk::PhysicalDevice::from_raw(game.phys as u64);
        // SAFETY: this command is legal in Vulkan1.0; the lease protects its parent and output.
        let properties = unsafe { gipa(instance, c"vkGetPhysicalDeviceProperties".as_ptr()) }?;
        // SAFETY: the exact command name specifies this native signature.
        let properties: vk::PFN_vkGetPhysicalDeviceProperties =
            unsafe { std::mem::transmute(properties) };
        let mut output = vk::PhysicalDeviceProperties::default();
        // SAFETY: the matching physical device and initialized output stay live under the lease.
        unsafe { properties(physical, &mut output) };
        let route = if api.requested_api_version >= vk::API_VERSION_1_1
            && output.api_version >= vk::API_VERSION_1_1
        {
            crate::vram_budget::QueryRoute::Core11
        } else if api.properties2_enabled {
            crate::vram_budget::QueryRoute::EnabledKhr
        } else {
            crate::vram_budget::QueryRoute::Unknown
        };
        // SAFETY: the observed instance facts and Vulkan1.0 physical properties prove only this
        // route. The synchronous query ends before the lease; no alias fallback is permitted.
        unsafe { crate::vram_budget::free_mib(gipa, instance, physical, route) }
    })
    .flatten()
}

/// Read a background report only while debug output is enabled; emit only available samples.
fn report_reflex_latency_when_available(
    enabled: bool,
    report: impl FnOnce() -> Option<String>,
    emit: impl FnOnce(&str),
) {
    if enabled {
        if let Some(line) = report() {
            emit(&line);
        }
    }
}

/// Last-two-second frame samples with one bounded caller-owned sorting buffer.
struct FrameReports {
    samples: std::collections::VecDeque<(f64, u64)>,
    scratch: Vec<u64>,
    debug_at_s: f64,
}
impl FrameReports {
    /// Reserve once before entering the worker loop.
    fn new() -> Self {
        Self {
            samples: std::collections::VecDeque::with_capacity(crate::frame_timer::RING_LEN),
            scratch: Vec::with_capacity(crate::frame_timer::RING_LEN),
            debug_at_s: 0.0,
        }
    }
    /// Observe actual frame-end timestamps, never assigning old samples the current tick's time.
    fn observe(
        &mut self,
        now_s: f64,
        intervals: &[(u64, u64)],
        to_seconds: impl Fn(u64) -> Option<f64>,
    ) {
        if !now_s.is_finite() {
            return;
        }
        for &(timestamp, micros) in intervals {
            let Some(end) = to_seconds(timestamp).filter(|end| end.is_finite() && *end <= now_s)
            else {
                continue;
            };
            if micros == 0 || end < now_s - 2.0 {
                continue;
            }
            if self.samples.len() >= crate::frame_timer::RING_LEN {
                self.samples.pop_front();
            }
            self.samples.push_back((end, micros));
        }
        while self
            .samples
            .front()
            .is_some_and(|(end, _)| *end < now_s - 2.0)
        {
            self.samples.pop_front();
        }
    }
    /// Compute with caller-owned scratch only when a report is due.
    fn stats(&mut self) -> Option<crate::frame_stats::FrameStats> {
        self.scratch.clear();
        self.scratch.extend(self.samples.iter().map(|(_, us)| *us));
        crate::frame_stats::calculate_in_place(&mut self.scratch)
    }
    /// Debug logging is level-gated and runs at most once per two seconds.
    fn debug_line_due(&mut self, now_s: f64, enabled: bool) -> Option<String> {
        if !enabled || !now_s.is_finite() || now_s - self.debug_at_s < 2.0 {
            return None;
        }
        self.debug_at_s = now_s;
        Some(crate::frame_stats::debug_line(self.stats().as_ref()))
    }
}

/// Native control work is never admitted by the statistics-only worker.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ThreadMode {
    Governor,
    StatsOnly,
}
impl ThreadMode {
    /// Queries, planning and publication form one capability boundary.
    fn run_control(self, body: impl FnOnce()) {
        if self == Self::Governor {
            body();
        }
    }
}
/// Match startup to the configured log level and governor setting.
fn thread_mode(cfg: &crate::config::Config) -> Option<ThreadMode> {
    if cfg.governor.target_fps != 0 {
        Some(ThreadMode::Governor)
    } else if cfg.debug.frame_stats && cfg.general.log_level == crate::config::LogLevel::Debug {
        Some(ThreadMode::StatsOnly)
    } else {
        None
    }
}

/// Start once from entry, after queued hooks are applied. No runtime thread is started in unit
/// tests; tests drive Runtime::plan synchronously with injected snapshots instead.
pub fn spawn(cfg: crate::config::Config) {
    let Some(mode) = thread_mode(&cfg) else {
        return;
    };
    if let Err(error) = std::thread::Builder::new()
        .name("headroom-governor".to_owned())
        .spawn(move || {
            crate::guard::guard(crate::guard::Feature::Governor, || (), || run(cfg, mode));
        })
    {
        log::warn!("governor: could not start: {error}");
    }
}

/// Execute the loop entirely under the feature guard. A panic stops it with shared values held.
fn run(cfg: crate::config::Config, mode: ThreadMode) {
    use crate::features::scatter_probe as probe;
    use std::sync::atomic::Ordering::{Acquire, Relaxed};
    let mut runtime = Runtime::new(cfg);
    let mut reader = FrameReader::new();
    let mut frame_reports = FrameReports::new();
    let origin = crate::frame_timer::qpc_now();
    let mut period_ms = 0.0;
    let mut free_mib = None;
    let mut display_at = f64::NEG_INFINITY;
    let mut memory_at = f64::NEG_INFINITY;
    let mut status_at = 0.0;
    let mut latency_at = 0.0;
    let mut scatter_diagnostics = ScatterDiagnostics::default();
    let mut applied = [0; 3];
    if mode == ThreadMode::Governor {
        log::info!("governor: started; inactive");
        if cfg.governor.trial_floor_fps != 0 {
            log::info!(
            "governor: measured quality trials; p95 floor={} fps, target={} fps, terrain first={}; no GPU timing",
            cfg.governor.trial_floor_fps,
            cfg.governor.target_fps,
            cfg.governor.terrain_first
        );
        } else if cfg.governor.terrain_first {
            log::info!("governor: terrain first; legacy frame-time policy");
        }
    } else {
        log::debug!("frames: log-only sampling enabled; no governor control");
    }
    loop {
        if crate::guard::is_disabled(crate::guard::Feature::Governor) {
            return;
        }
        let now_qpc = crate::frame_timer::qpc_now();
        let now_s = crate::frame_timer::qpc_to_ms(now_qpc.saturating_sub(origin)) / 1000.0;
        reader.read(&crate::frame_timer::FRAMES, crate::frame_timer::qpc_to_ms);
        // Sample the reporting clock after copying the ring, so a concurrent new present is not
        // discarded merely because its timestamp followed the tick's earlier control clock.
        let stats_now_s =
            crate::frame_timer::qpc_to_ms(crate::frame_timer::qpc_now().saturating_sub(origin))
                / 1000.0;
        frame_reports.observe(stats_now_s, &reader.intervals_us, |timestamp| {
            timestamp
                .checked_sub(origin)
                .map(|delta| crate::frame_timer::qpc_to_ms(delta) / 1000.0)
        });
        if let Some(line) = frame_reports.debug_line_due(
            stats_now_s,
            cfg.general.log_level == crate::config::LogLevel::Debug
                && log::log_enabled!(log::Level::Debug),
        ) {
            log::debug!("{line}");
        }
        mode.run_control(|| {
        if now_s - display_at >= 5.0 {
            period_ms = crate::display::period_ms(
                crate::display::game_window().and_then(crate::display::refresh_hz),
            );
            display_at = now_s;
        }
        if crate::guard::is_disabled(crate::guard::Feature::Governor) {
            return;
        }
        if now_s - memory_at >= 2.0 {
            free_mib = query_vram_free_mib();
            memory_at = now_s;
        }
        let seen = crate::knobs::OBJECT.last_seen();
        let frames = &reader.ms;
        let plan = runtime.plan(&Sample {
            now_s,
            frames,
            world_active: seen != 0
                && now_qpc
                    .checked_sub(seen)
                    .is_some_and(|delta| crate::frame_timer::qpc_to_ms(delta) < 250.0),
            installed: crate::knobs::object_distance::hook_installed(),
            seen,
            unknown: crate::knobs::OBJECT_UNKNOWN_BASELINE.load(Relaxed),
            baseline_seq: [
                crate::knobs::OBJECT.baseline().1,
                crate::knobs::SCATTER.baseline().1,
                crate::knobs::TERRAIN.baseline().1,
            ],
            period_ms,
            level: crate::knobs::SCATTER_LEVEL.load(Relaxed),
            probe_ok: probe::PROBE_OK.load(Acquire),
            probe_failed: probe::FAILED.load(Acquire),
            last_a: probe::LAST_A.load(Relaxed),
            free_mib,
        });
        plan_then_publish(
            || {
                if plan.signals.report_missing {
                    log::warn!("world signal missing: the governor holds every knob");
                }
                if plan.signals.report_unknown {
                    log::warn!("the game wrote an unexpected object-distance value; scaling skipped for those frames");
                }
                if plan.report_probe_failure {
                    log::warn!("governor: scatter probe failed; grass reset to step 0");
                }
                for (knob, (&before, &step)) in applied.iter().zip(&plan.steps).enumerate() {
                    if before != step {
                        log::info!("governor: knob {knob} step {before} -> {step}");
                    }
                }
                if now_s - status_at >= 30.0 {
                    if log::log_enabled!(log::Level::Info) {
                    let (p50, p95) = runtime.percentiles();
                    let suffix=crate::frame_stats::info_suffix(frame_reports.stats().as_ref());
                    log::info!(
                        "{}; {suffix}",
                        status_line(
                            p50,
                            p95,
                            (1000.0 / f64::from(cfg.governor.target_fps)).max(period_ms),
                            plan.steps,
                            plan.state
                        )
                    );
                    }
                    status_at = now_s;
                }
                if cfg.general.log_level == crate::config::LogLevel::Debug {
                    if now_s - latency_at >= 5.0 {
                        report_reflex_latency_when_available(
                            log::log_enabled!(log::Level::Debug),
                            crate::reflex::latency_report,
                            |line| log::debug!("{line}"),
                        );
                        latency_at = now_s;
                    }
                    if cfg.debug.scatter_probe {
                        let peak_a = probe::PEAK_A.load(Relaxed);
                        if let Some(session_peak_a) = scatter_diagnostics.observe(now_s, peak_a) {
                            log::debug!(
                                "{}",
                                scatter_probe_line(
                                    [
                                        probe::LAST_A.load(Relaxed),
                                        peak_a,
                                        probe::LAST_B.load(Relaxed),
                                        probe::PEAK_B.load(Relaxed),
                                        probe::LAST_C.load(Relaxed),
                                        probe::PEAK_C.load(Relaxed),
                                    ],
                                    session_peak_a
                                )
                            );
                        }
                    }
                }
                plan.values
            },
            |values| {
                // Every value is already validated. These setters are only atomic stores and cannot
                // panic, allocate or call user code. No logging occurs between stores.
                let [object, scatter, terrain] = values;
                crate::knobs::OBJECT.set(object);
                crate::knobs::scatter_distance::set_distance(scatter);
                crate::knobs::TERRAIN.set(terrain);
            },
        );
        applied = plan.steps;
        });
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
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
    fn frame_sampling_and_stats_reuse_reserved_storage_without_allocating() {
        let mut reports = FrameReports::new();
        let mut scratch = [0u64; 102];
        let frames = [13_300; 102];
        let count = allocation_count(|| {
            for tick in 0..10_000u64 {
                reports.observe(tick as f64 / 1000.0, &[(tick, 13_300)], |stamp| {
                    Some(stamp as f64 / 1000.0)
                });
                let _ = reports.stats();
                assert!(crate::frame_stats::calculate(&frames, &mut scratch).is_some());
            }
        });
        assert_eq!(count, 0);
    }

    #[test]
    fn overflowing_the_bounded_window_retains_latest_samples_without_allocating() {
        let mut reports = FrameReports::new();
        let first: [(u64, u64); 4096] = std::array::from_fn(|n| (n as u64, 1_000));
        let second: [(u64, u64); 4096] = std::array::from_fn(|n| (4096 + n as u64, 2_000));
        let mut stats = None;
        let allocations = allocation_count(|| {
            reports.observe(0.5, &first, |stamp| Some(stamp as f64 / 10000.0));
            reports.observe(1.0, &second, |stamp| Some(stamp as f64 / 10000.0));
            stats = reports.stats();
        });
        assert_eq!(allocations, 0);
        assert_eq!(
            stats.unwrap().p50_us,
            2_000,
            "latest4096samples replace the older batch"
        );
    }
    #[test]
    fn stats_intervals_use_reader_lap_and_actual_timestamp_rules() {
        let ring = crate::frame_timer::Ring::new();
        let mut reader = FrameReader::new();
        ring.push(100);
        ring.push(110);
        ring.push(130);
        reader.read(&ring, |delta| delta as f64);
        assert_eq!(reader.intervals_us, [(110, 10_000), (130, 20_000)]);
        for n in 0..crate::frame_timer::RING_LEN + 1 {
            ring.push(200 + n as u64);
        }
        reader.read(&ring, |delta| delta as f64);
        assert!(reader.intervals_us.iter().all(|(_, us)| *us == 1_000));
    }
    #[test]
    fn frame_reports_use_actual_two_second_frame_ends_and_debug_cadence() {
        let mut reports = FrameReports::new();
        reports.observe(
            2.0,
            &[(0, 120_000), (1, 10_000), (2, 20_000)],
            |timestamp| Some(timestamp as f64),
        );
        assert!(reports.debug_line_due(1.9, true).is_none());
        assert_eq!(
            reports.debug_line_due(2.0, true).as_deref(),
            Some("frames: p50 20.00 ms, p99 120.00 ms, max 120.00 ms, hitches 1")
        );
        reports.observe(3.01, &[], |timestamp| Some(timestamp as f64));
        assert_eq!(
            reports.stats(),
            None,
            "only one frame remains in the last2s"
        );
        assert!(reports.debug_line_due(4.0, false).is_none());
        assert_eq!(
            reports.debug_line_due(4.0, true).as_deref(),
            Some("frames: no data")
        );
    }
    #[test]
    fn stats_only_mode_requires_debug_key_and_level_and_never_runs_control_work() {
        use crate::config::{Config, LogLevel};
        for target in [0, 75] {
            for frame_stats in [false, true] {
                for level in [LogLevel::Info, LogLevel::Debug] {
                    let mut cfg = Config::default();
                    cfg.governor.target_fps = target;
                    cfg.debug.frame_stats = frame_stats;
                    cfg.general.log_level = level;
                    let expected = if target != 0 {
                        Some(ThreadMode::Governor)
                    } else if frame_stats && level == LogLevel::Debug {
                        Some(ThreadMode::StatsOnly)
                    } else {
                        None
                    };
                    assert_eq!(thread_mode(&cfg), expected);
                }
            }
        }
        ThreadMode::StatsOnly
            .run_control(|| panic!("log-only mode must not query, plan or publish native control"));
        let mut calls = 0;
        ThreadMode::Governor.run_control(|| calls += 1);
        assert_eq!(calls, 1);
    }
    #[test]
    fn scatter_debug_reports_keep_the_observed_session_peak_across_scene_resets() {
        let mut diagnostics = ScatterDiagnostics::default();
        assert_eq!(diagnostics.observe(0.0, 0), None);
        assert_eq!(diagnostics.observe(0.05, 70_000), None);
        assert_eq!(diagnostics.observe(1.95, 3), None);
        let retained = diagnostics.observe(2.0, 0).expect("first report due");
        assert_eq!(
            scatter_probe_line([10, 0, 20, 21, 30, 31], retained),
            "scatter probe: A=10 peak_A=0 B=20 peak_B=21 C=30 peak_C=31 session_peak_A=70000"
        );
        assert_eq!(diagnostics.observe(2.05, 80_000), None);
        assert_eq!(diagnostics.observe(3.95, 1), None);
        let retained = diagnostics.observe(4.0, 0).expect("second report due");
        assert_eq!(
            scatter_probe_line([11, 0, 22, 23, 32, 33], retained),
            "scatter probe: A=11 peak_A=0 B=22 peak_B=23 C=32 peak_C=33 session_peak_A=80000"
        );
    }

    #[test]
    fn scatter_debug_session_peak_keeps_zero_and_the_u32_maximum() {
        let mut diagnostics = ScatterDiagnostics::default();
        assert_eq!(diagnostics.observe(0.0, 0), None);
        assert_eq!(diagnostics.observe(2.0, 0), Some(0));
        assert_eq!(diagnostics.observe(2.05, u32::MAX), None);
        assert_eq!(diagnostics.observe(4.0, 0), Some(u32::MAX));
        assert_eq!(diagnostics.observe(6.0, u32::MAX - 1), Some(u32::MAX));
        assert_eq!(diagnostics.observe(8.0, 0), Some(u32::MAX));
    }

    #[test]
    fn scatter_debug_sampling_allocates_only_when_formatting_a_report() {
        let mut diagnostics = ScatterDiagnostics::default();
        let sampling = allocation_count(|| {
            for tick in 0..10_000u32 {
                std::hint::black_box(diagnostics.observe(f64::from(tick) * 0.05, tick));
            }
        });
        assert_eq!(sampling, 0);
        let formatting = allocation_count(|| {
            std::hint::black_box(scatter_probe_line([1, 2, 3, 4, 5, 6], 70_000));
        });
        assert!(
            formatting > 0,
            "the emitted line is an allocation positive control"
        );
    }

    /// Band trials keep runtime memory/device/world/capacity gates and reserved-storage planning.
    #[test]
    fn band_runtime_obeys_safety_vetoes_and_keeps_planning_allocation_free() {
        let (mut cfg, warnings) =
            crate::config::parse("[Governor]\nTrialFloorFps=65\nTerrainFirst=1\n");
        assert!(warnings.is_empty());
        cfg.knobs.enable_object_distance = false;
        cfg.knobs.max_terrain_distance = 1;
        let mut runtime = Runtime::new(cfg);
        let sample = Sample {
            now_s: 0.0,
            frames: &[1000.0 / 75.0; 4],
            world_active: true,
            installed: true,
            seen: 1,
            unknown: 0,
            baseline_seq: [0; 3],
            period_ms: 0.0,
            level: 3,
            probe_ok: true,
            probe_failed: false,
            last_a: 0,
            free_mib: None,
        };
        let mut last = [0; 3];
        let allocations = allocation_count(|| {
            for n in 0..500 {
                let frozen = n >= 400;
                let plan = runtime.plan(&Sample {
                    now_s: f64::from(n) * 0.05,
                    installed: !frozen,
                    world_active: !frozen,
                    seen: n as u64 + 1,
                    free_mib: (n >= 100).then_some(4096),
                    ..sample
                });
                if n < 100 {
                    assert_eq!(plan.steps, [0; 3], "unknown device memory must veto raises");
                }
                assert_eq!(plan.steps[0], 0, "disabled object knob");
                assert_eq!(plan.steps[1], 0, "unmeasured scatter capacity");
                if frozen {
                    assert_eq!(plan.state, RuntimeState::Inactive);
                    assert_eq!(plan.steps, [0, 0, 1]);
                }
                last = plan.steps;
            }
            for n in 500..700 {
                last = runtime
                    .plan(&Sample {
                        now_s: f64::from(n) * 0.05,
                        frames: &[1000.0 / 60.0; 4],
                        seen: n as u64 + 1,
                        free_mib: None,
                        ..sample
                    })
                    .steps;
            }
        });
        assert_eq!(allocations, 0);
        assert_eq!(
            last, [0; 3],
            "normal safety backoff must still release terrain"
        );
    }
    #[test]
    fn runtime_plan_is_allocation_free_and_unknown_vram_vetoes_all_raises() {
        let mut cfg = crate::config::Config::default();
        cfg.governor.settle_seconds = 0.0;
        cfg.governor.raise_hold_seconds = 0.0;
        let mut runtime = Runtime::new(cfg);
        let sample = Sample {
            now_s: 0.0,
            frames: &[5.0, 6.0],
            world_active: true,
            installed: true,
            seen: 1,
            unknown: 0,
            baseline_seq: [0; 3],
            period_ms: 0.0,
            level: 3,
            probe_ok: true,
            probe_failed: false,
            last_a: 0,
            free_mib: None,
        };
        let allocations = allocation_count(|| {
            for n in 0..100 {
                let plan = runtime.plan(&Sample {
                    now_s: f64::from(n) * 0.05,
                    seen: n as u64 + 1,
                    ..sample
                });
                assert_eq!(plan.steps, [0; 3]);
            }
        });
        assert_eq!(allocations, 0);
    }

    #[test]
    fn reusable_tick_has_no_allocations_across_full_batches_and_percentile_sorting() {
        let mut cfg = crate::config::Config::default().governor;
        cfg.settle_seconds = 0.0;
        cfg.raise_hold_seconds = 0.0;
        let mut governor = crate::governor::Governor::new(
            cfg,
            vec![
                crate::governor::KnobSpec {
                    enabled: true,
                    max_step: 4
                };
                3
            ],
        );
        let mut scratch = crate::governor::TickScratch::new(3, crate::frame_timer::RING_LEN);
        let frames = vec![5.0; crate::frame_timer::RING_LEN];
        let allocations = allocation_count(|| {
            for n in 0..100 {
                std::hint::black_box(governor.tick_with(
                    &mut scratch,
                    &crate::governor::Inputs {
                        now_s: f64::from(n) * 0.05,
                        new_frames_ms: &frames,
                        world_active: true,
                        period_ms: 0.0,
                        vram_free_mib: Some(4096),
                        baseline_changed: &[false; 3],
                    },
                ));
            }
        });
        assert_eq!(allocations, 0);
    }

    #[test]
    fn runtime_freeze_holds_fixed_steps_and_defers_resets_until_scene_recovers() {
        let mut cfg = crate::config::Config::default();
        cfg.general.log_level = crate::config::LogLevel::Debug;
        cfg.debug.force_step_object = Some(2);
        let mut runtime = Runtime::new(cfg);
        let sample = Sample {
            now_s: 0.0,
            frames: &[5.0],
            world_active: true,
            installed: true,
            seen: 1,
            unknown: 0,
            baseline_seq: [0; 3],
            period_ms: 0.0,
            level: 3,
            probe_ok: true,
            probe_failed: false,
            last_a: 0,
            free_mib: Some(4096),
        };
        assert_eq!(runtime.plan(&sample).steps, [2, 0, 0]);
        let held = runtime.plan(&Sample {
            now_s: 10.0,
            world_active: false,
            baseline_seq: [1; 3],
            ..sample
        });
        assert!(held.signals.report_missing);
        assert_eq!(held.steps, [2, 0, 0]);
        assert_eq!(held.state, RuntimeState::Inactive);
        let held_again = runtime.plan(&Sample {
            now_s: 11.0,
            world_active: false,
            ..sample
        });
        assert!(!held_again.signals.report_missing);
        assert_eq!(held_again.steps, [2, 0, 0]);
        assert!(
            !runtime
                .plan(&Sample {
                    now_s: 11.05,
                    seen: 2,
                    ..sample
                })
                .signals
                .freeze
        );
    }

    #[test]
    fn missing_signal_freezes_an_automatic_step_across_a_menu_reset() {
        let mut cfg = crate::config::Config::default();
        cfg.governor.settle_seconds = 0.0;
        cfg.governor.raise_hold_seconds = 0.0;
        let mut runtime = Runtime::new(cfg);
        let sample = Sample {
            now_s: 0.0,
            frames: &[5.0],
            world_active: true,
            installed: true,
            seen: 1,
            unknown: 0,
            baseline_seq: [0; 3],
            period_ms: 0.0,
            level: 3,
            probe_ok: true,
            probe_failed: false,
            last_a: 0,
            free_mib: Some(4096),
        };
        let before = runtime.plan(&sample).steps;
        assert!(before[0] > 0);
        let plan = runtime.plan(&Sample {
            now_s: 10.0,
            world_active: false,
            baseline_seq: [1; 3],
            ..sample
        });
        assert!(plan.signals.freeze);
        assert_eq!(plan.steps, before);
    }

    #[test]
    fn panic_after_complete_runtime_plan_leaves_every_shared_value_unchanged() {
        let cfg = crate::config::Config::default();
        let mut runtime = Runtime::new(cfg);
        let knobs = [
            crate::knobs::KnobShared::new(0.85),
            crate::knobs::KnobShared::new(100.0),
            crate::knobs::KnobShared::new(1.5),
        ];
        let before = knobs.each_ref().map(crate::knobs::KnobShared::value);
        let result = crate::guard::catch("runtime publication test", || {
            plan_then_publish(
                || {
                    let plan = runtime.plan(&Sample {
                        now_s: 0.0,
                        frames: &[5.0],
                        world_active: true,
                        installed: true,
                        seen: 1,
                        unknown: 0,
                        baseline_seq: [0; 3],
                        period_ms: 0.0,
                        level: 3,
                        probe_ok: true,
                        probe_failed: false,
                        last_a: 0,
                        free_mib: None,
                    });
                    std::hint::black_box(plan.values);
                    panic!("injected log panic before atomic batch");
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
    fn ordinary_tick_convenience_api_is_a_positive_allocation_control() {
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
        assert!(
            allocations > 0,
            "the convenience API must remain a positive control for the allocation counter"
        );
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
    #[test]
    fn reflex_latency_hook_emits_only_available_debug_reports_without_extra_allocation() {
        let queried = Cell::new(false);
        let emitted = Cell::new(false);
        assert_eq!(
            allocation_count(|| {
                report_reflex_latency_when_available(
                    false,
                    || panic!("disabled debug logging must not query or allocate"),
                    |_| panic!("disabled debug logging must not emit"),
                );
                report_reflex_latency_when_available(
                    true,
                    || {
                        queried.set(true);
                        None
                    },
                    |_| panic!("unavailable timings must not emit a line"),
                );
            }),
            0
        );
        assert!(
            queried.get(),
            "the scheduled hook must call the report provider"
        );
        assert_eq!(
            allocation_count(|| {
                report_reflex_latency_when_available(
                    true,
                    || Some("Reflex: native timing sample".to_owned()),
                    |line| {
                        assert_eq!(line, "Reflex: native timing sample");
                        emitted.set(true);
                    },
                );
            }),
            1,
            "only the emitted provider string allocates"
        );
        assert!(
            emitted.get(),
            "available timings must reach the debug logger"
        );
    }
    /// Real wrapper publication and teardown, isolated because the Vulkan state is first-wins.
    mod live_queries {
        use super::super::query_vram_free_mib;
        use crate::guard::{guard, Feature};
        use ash::vk::{self, Handle};
        use std::cell::Cell;
        use std::ffi::{c_char, CStr};
        use std::sync::atomic::Ordering;

        /// Instance generation created and observed by this child.
        const INSTANCE: u64 = 0x2410;
        /// Physical device returned only by that instance's enumeration.
        const PHYSICAL: u64 = 0x2420;
        /// First device identity, deliberately reused after retirement in one case.
        const DEVICE: u64 = 0x2430;

        thread_local! {
            /// The recognized child case; callbacks never read the environment or allocate.
            static CASE: Cell<u8> = const { Cell::new(0) };
            /// Physical API support is independent of the observed application request.
            static PHYSICAL_API: Cell<u32> = const { Cell::new(vk::API_VERSION_1_1) };
            /// Properties calls, extension calls, alias lookups, then actual core/KHR calls.
            static CALLS: Cell<[u32; 6]> = const { Cell::new([0; 6]) };
            /// Native destruction remains observable independently of the lease.
            static DESTROYS: Cell<u32> = const { Cell::new(0) };
        }

        /// Record only a fixed counter, without allocating during the native query.
        fn note(index: usize) {
            CALLS.with(|calls| {
                let mut values = calls.get();
                values[index] += 1;
                calls.set(values);
            });
        }

        /// Return an erased native entry point using the same name-directed contract as Vulkan.
        fn entry(address: usize) -> vk::PFN_vkVoidFunction {
            // SAFETY: every caller supplies zero or a function address recovered by exact name.
            unsafe { std::mem::transmute::<usize, vk::PFN_vkVoidFunction>(address) }
        }

        /// Create one synthetic instance; all original input pointers remain owned by the child.
        /// # Safety
        /// The caller supplies one initialized writable instance output slot.
        unsafe extern "system" fn create_instance(
            _: *const vk::InstanceCreateInfo<'_>,
            _: *const vk::AllocationCallbacks<'_>,
            out: *mut vk::Instance,
        ) -> vk::Result {
            guard(
                Feature::Governor,
                || vk::Result::ERROR_INITIALIZATION_FAILED,
                || {
                    // SAFETY: the child owns this output for the complete synchronous call.
                    unsafe { out.write(vk::Instance::from_raw(INSTANCE)) };
                    vk::Result::SUCCESS
                },
            )
        }

        /// Return this exact instance's physical device through actual intercepted enumeration.
        /// # Safety
        /// The caller supplies a count and, when non-null, at least one writable output slot.
        unsafe extern "system" fn enumerate(
            instance: vk::Instance,
            count: *mut u32,
            out: *mut vk::PhysicalDevice,
        ) -> vk::Result {
            guard(
                Feature::Governor,
                || vk::Result::ERROR_INITIALIZATION_FAILED,
                || {
                    assert_eq!(instance.as_raw(), INSTANCE);
                    // SAFETY: the count/output follow this fake's synchronous enumeration contract.
                    unsafe {
                        if !out.is_null() {
                            assert!(*count >= 1);
                            out.write(vk::PhysicalDevice::from_raw(PHYSICAL));
                        }
                        count.write(1);
                    }
                    vk::Result::SUCCESS
                },
            )
        }

        /// Create the same raw bits again only after the test has retired the first generation.
        /// # Safety
        /// The caller supplies one writable device output slot and a live synthetic physical device.
        unsafe extern "system" fn create_device(
            physical: vk::PhysicalDevice,
            _: *const vk::DeviceCreateInfo<'_>,
            _: *const vk::AllocationCallbacks<'_>,
            out: *mut vk::Device,
        ) -> vk::Result {
            guard(
                Feature::Governor,
                || vk::Result::ERROR_INITIALIZATION_FAILED,
                || {
                    assert_eq!(physical.as_raw(), PHYSICAL);
                    // SAFETY: the child keeps the native output slot writable throughout this call.
                    unsafe { out.write(vk::Device::from_raw(DEVICE)) };
                    vk::Result::SUCCESS
                },
            )
        }

        /// Finish native teardown after the real wrapper has invalidated the read lease.
        /// # Safety
        /// Handles are opaque fake values and no allocation callback is read.
        unsafe extern "system" fn destroy_device(
            device: vk::Device,
            _: *const vk::AllocationCallbacks<'_>,
        ) {
            guard(
                Feature::Governor,
                || (),
                || {
                    assert_eq!(device.as_raw(), DEVICE);
                    DESTROYS.with(|n| n.set(n.get() + 1));
                },
            );
        }

        /// Supply the physical version through the legal Vulkan1.0 command.
        /// # Safety
        /// The physical handle belongs to this fake and out is one writable native struct.
        unsafe extern "system" fn properties(
            physical: vk::PhysicalDevice,
            out: *mut vk::PhysicalDeviceProperties,
        ) {
            guard(
                Feature::Governor,
                || (),
                || {
                    assert_eq!(physical.as_raw(), PHYSICAL);
                    note(0);
                    // SAFETY: the caller owns this initialized output for the native call.
                    unsafe { (*out).api_version = PHYSICAL_API.with(Cell::get) };
                },
            );
        }

        /// Advertise the budget extension independently of command-pointer presence.
        /// # Safety
        /// The caller supplies the fixed free_mib buffer and writable count.
        unsafe extern "system" fn extensions(
            physical: vk::PhysicalDevice,
            layer: *const c_char,
            count: *mut u32,
            out: *mut vk::ExtensionProperties,
        ) -> vk::Result {
            guard(
                Feature::Governor,
                || vk::Result::ERROR_INITIALIZATION_FAILED,
                || {
                    assert_eq!(physical.as_raw(), PHYSICAL);
                    assert!(layer.is_null());
                    note(1);
                    let supported = CASE.with(Cell::get) != 12;
                    let mut extension = vk::ExtensionProperties::default();
                    for (to, from) in extension
                        .extension_name
                        .iter_mut()
                        .zip(c"VK_EXT_memory_budget".to_bytes_with_nul())
                    {
                        *to = *from as c_char;
                    }
                    // SAFETY: free_mib gives capacity512; the fake writes at most one entry.
                    unsafe {
                        if supported {
                            assert!(*count >= 1);
                            out.write(extension);
                        }
                        count.write(u32::from(supported));
                    }
                    vk::Result::SUCCESS
                },
            )
        }

        /// Fill a real properties2/budget output chain owned by the caller.
        /// # Safety
        /// out points to initialized properties2 with a writable budget pNext.
        unsafe fn budget(out: *mut vk::PhysicalDeviceMemoryProperties2<'_>) {
            // SAFETY: the native query contract keeps both exact structs alive and writable.
            unsafe {
                assert_eq!(
                    (*out).s_type.as_raw(),
                    vk::StructureType::PHYSICAL_DEVICE_MEMORY_PROPERTIES_2.as_raw()
                );
                let budget = &mut *(*out)
                    .p_next
                    .cast::<vk::PhysicalDeviceMemoryBudgetPropertiesEXT<'_>>();
                assert_eq!(
                    budget.s_type.as_raw(),
                    vk::StructureType::PHYSICAL_DEVICE_MEMORY_BUDGET_PROPERTIES_EXT.as_raw()
                );
                (*out).memory_properties.memory_heap_count = 1;
                (*out).memory_properties.memory_heaps[0] = vk::MemoryHeap {
                    size: 8 << 30,
                    flags: vk::MemoryHeapFlags::DEVICE_LOCAL,
                };
                budget.heap_budget[0] = 7 << 30;
                budget.heap_usage[0] = 5 << 30;
            }
        }

        /// A non-null core address is deliberately available even for KHR-only instances.
        /// # Safety
        /// The physical handle and caller-owned native chain obey budget's contract.
        unsafe extern "system" fn core(
            physical: vk::PhysicalDevice,
            out: *mut vk::PhysicalDeviceMemoryProperties2<'_>,
        ) {
            guard(
                Feature::Governor,
                || (),
                || {
                    assert_eq!(physical.as_raw(), PHYSICAL);
                    note(4);
                    // SAFETY: the query supplies this exact live output chain.
                    unsafe { budget(out) };
                },
            );
        }

        /// The KHR alias has a separate counter and the same native output contract.
        /// # Safety
        /// The physical handle and caller-owned native chain obey budget's contract.
        unsafe extern "system" fn khr(
            physical: vk::PhysicalDevice,
            out: *mut vk::PhysicalDeviceMemoryProperties2<'_>,
        ) {
            guard(
                Feature::Governor,
                || (),
                || {
                    assert_eq!(physical.as_raw(), PHYSICAL);
                    note(5);
                    // SAFETY: the query supplies this exact live output chain.
                    unsafe { budget(out) };
                },
            );
        }

        /// Resolve the child's real native endpoints without capability-based pointer filtering.
        /// # Safety
        /// name is a synchronous NUL-terminated lookup; instance is null or this child's instance.
        unsafe extern "system" fn gipa(
            instance: vk::Instance,
            name: *const c_char,
        ) -> vk::PFN_vkVoidFunction {
            guard(
                Feature::Governor,
                || None,
                || {
                    // SAFETY: the wrapper/query passes a live static native command name.
                    let name = unsafe { CStr::from_ptr(name) }.to_bytes();
                    if name != b"vkCreateInstance" {
                        assert_eq!(instance.as_raw(), INSTANCE);
                    }
                    let case = CASE.with(Cell::get);
                    entry(match name {
                        b"vkCreateInstance" => create_instance as *const () as usize,
                        b"vkEnumeratePhysicalDevices" => enumerate as *const () as usize,
                        b"vkCreateDevice" => create_device as *const () as usize,
                        b"vkDestroyDevice" => destroy_device as *const () as usize,
                        b"vkGetPhysicalDeviceProperties" if case != 8 => {
                            properties as *const () as usize
                        }
                        b"vkEnumerateDeviceExtensionProperties" => extensions as *const () as usize,
                        b"vkGetPhysicalDeviceMemoryProperties2" => {
                            note(2);
                            if case == 6 {
                                0
                            } else {
                                core as *const () as usize
                            }
                        }
                        b"vkGetPhysicalDeviceMemoryProperties2KHR" => {
                            note(3);
                            if case == 7 {
                                0
                            } else {
                                khr as *const () as usize
                            }
                        }
                        _ => 0,
                    })
                },
            )
        }

        /// Every route is reached through the real observed instance/device lease, never raw facts.
        #[test]
        fn leased_query_uses_observed_instance_and_physical_capabilities() {
            const TEST: &str = "governor_thread::tests::live_queries::leased_query_uses_observed_instance_and_physical_capabilities";
            const ARGS: [&str; 4] = ["--exact", TEST, "--nocapture", "--test-threads=1"];
            const CASES: [&str; 14] = [
                "core",
                "khr",
                "low-physical",
                "low-physical-khr",
                "unknown",
                "no-route",
                "missing-core",
                "missing-khr",
                "missing-properties",
                "unleased",
                "retired",
                "reused",
                "budget-missing",
                "api-zero",
            ];
            let case = std::env::var("HEADROOM_W24_MEMORY").ok();
            let exact = std::env::args_os()
                .skip(1)
                .eq(ARGS.map(std::ffi::OsString::from));
            let selected = case
                .as_deref()
                .and_then(|case| CASES.iter().position(|&known| known == case));
            let Some(index) = selected.filter(|_| exact) else {
                for case in CASES {
                    let output = std::process::Command::new(std::env::current_exe().unwrap())
                        .args(ARGS)
                        .env("HEADROOM_W24_MEMORY", case)
                        .output()
                        .unwrap();
                    assert!(
                        output.status.success(),
                        "{case}: {}\n{}",
                        String::from_utf8_lossy(&output.stdout),
                        String::from_utf8_lossy(&output.stderr)
                    );
                    println!("memory capability case passed: {case}");
                }
                return;
            };
            assert!(
                exact,
                "native fixture needs the exact selected test and one worker"
            );
            CASE.with(|case| case.set(index as u8));
            PHYSICAL_API.with(|version| {
                version.set(if matches!(index, 2 | 3) {
                    vk::API_VERSION_1_0
                } else {
                    vk::API_VERSION_1_1
                })
            });
            crate::reflex::init(crate::config::ReflexMode::Off);
            crate::vk::REAL_GIPA.store(gipa as *const () as usize, Ordering::Release);
            let names = [c"VK_KHR_get_physical_device_properties2".as_ptr()];
            let app = vk::ApplicationInfo::default().api_version(if index == 13 {
                0
            } else if matches!(index, 1 | 5 | 7) {
                vk::API_VERSION_1_0
            } else {
                vk::API_VERSION_1_1
            });
            let mut info = vk::InstanceCreateInfo::default().application_info(&app);
            if matches!(index, 0 | 1 | 3 | 6 | 7 | 8) {
                info = info.enabled_extension_names(&names);
            }
            // SAFETY: every native pointer belongs to this exact child fixture. The real wrappers
            // copy creation facts, observe actual enumeration and publish the mandatory lease.
            unsafe {
                let mut instance = vk::Instance::null();
                if index == 4 {
                    assert_eq!(
                        create_instance(&info, std::ptr::null(), &mut instance),
                        vk::Result::SUCCESS
                    );
                } else {
                    let create: vk::PFN_vkCreateInstance = std::mem::transmute(
                        crate::vk::wrap_gipa(vk::Instance::null(), c"vkCreateInstance".as_ptr())
                            .unwrap(),
                    );
                    assert_eq!(
                        create(&info, std::ptr::null(), &mut instance),
                        vk::Result::SUCCESS
                    );
                }
                let enumerate: vk::PFN_vkEnumeratePhysicalDevices = std::mem::transmute(
                    crate::vk::wrap_gipa(instance, c"vkEnumeratePhysicalDevices".as_ptr()).unwrap(),
                );
                let mut count = 1;
                let mut physical = vk::PhysicalDevice::null();
                assert_eq!(
                    enumerate(instance, &mut count, &mut physical),
                    vk::Result::SUCCESS
                );
                if index != 9 {
                    let create: vk::PFN_vkCreateDevice = std::mem::transmute(
                        crate::vk::wrap_gipa(instance, c"vkCreateDevice".as_ptr()).unwrap(),
                    );
                    let mut device = vk::Device::null();
                    assert_eq!(
                        create(
                            physical,
                            &vk::DeviceCreateInfo::default(),
                            std::ptr::null(),
                            &mut device
                        ),
                        vk::Result::SUCCESS
                    );
                }
                CALLS.with(|calls| calls.set([0; 6]));
                let expected = match index {
                    0 | 10 | 11 => Some(2048),
                    1 | 3 => Some(2048),
                    _ => None,
                };
                assert_eq!(query_vram_free_mib(), expected);
                let counts = match index {
                    0 | 10 | 11 => [1, 1, 1, 0, 1, 0],
                    1 | 3 => [1, 1, 0, 1, 0, 1],
                    2 | 5 | 13 => [1, 0, 0, 0, 0, 0],
                    6 => [1, 1, 1, 0, 0, 0],
                    7 => [1, 1, 0, 1, 0, 0],
                    12 => [1, 1, 0, 0, 0, 0],
                    _ => [0; 6],
                };
                assert_eq!(CALLS.with(Cell::get), counts);
                assert!(!crate::guard::is_disabled(Feature::Governor));
                assert_eq!(
                    super::allocation_count(|| {
                        for _ in 0..100 {
                            std::hint::black_box(query_vram_free_mib());
                        }
                    }),
                    0
                );
                if matches!(index, 10 | 11) {
                    crate::vk::wrap_destroy_device(vk::Device::from_raw(DEVICE), std::ptr::null());
                    assert_eq!(DESTROYS.with(Cell::get), 1);
                    if index == 11 {
                        let mut device = vk::Device::null();
                        assert_eq!(
                            crate::vk::wrap_create_device(
                                physical,
                                &vk::DeviceCreateInfo::default(),
                                std::ptr::null(),
                                &mut device
                            ),
                            vk::Result::SUCCESS
                        );
                        assert_eq!(device.as_raw(), DEVICE);
                    }
                    CALLS.with(|calls| calls.set([0; 6]));
                    assert_eq!(query_vram_free_mib(), None);
                    assert_eq!(CALLS.with(Cell::get), [0; 6]);
                }
            }
        }
    }
}
