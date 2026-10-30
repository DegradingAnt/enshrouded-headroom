//! The fps governor (spec section 6.4): a pure state machine that raises draw-distance knobs one
//! step at a time using legacy headroom thresholds or opt-in measured quality trials.
//!
//! Task 15 calls [`Governor::tick`] every 50 ms on the governor thread, inside
//! `guard(Feature::Governor, ..)`, and applies the [`Action`]s it returns. The governor reads no
//! clock (time comes only from [`Inputs::now_s`]), does no I/O and knows no Windows types. It must
//! not panic on any input: a panic turns it off for the session.
//!
//! The labels T1-T7 and R1-R11 in the comments are the items of the Task 11 dispatch notes
//! (revision 4), which refine spec 6.4.
//!
//! # One tick (R1)
//!
//! - (o) Clamp the time (T5) and drop frame times that are not finite or not above 0.
//! - (i) `TargetFps = 0`: do nothing.
//! - (ii) Menu resets, then the display-period change check (R3).
//! - (iii) The hitch undo (R7), else the stall undo (R8). At most one undo.
//! - (iv) The inactive check (R2). While inactive, stop here.
//! - (v) Add the frames to the window, unless a reset point in (ii)/(iii) or the undo grace (T2)
//!   says skip.
//! - (vi) Decide: an overload lower, else a raise. Not on a tick that already changed a step in
//!   (iii).
//!
//! # Reset points (T1)
//!
//! Each change the governor makes, each menu or `set_enabled` reset, a display-period change, a
//! time re-base and each change from inactive to active clears the window, restarts the hold
//! timers and starts a quiet period of `SettleSeconds` (T3) with no raise and no overload lower.

pub mod stats;

use std::collections::VecDeque;

use crate::config::GovernorCfg;

/// Slack for values that sit exactly on a limit (T5): `x + EPS >= limit` for an at-least limit,
/// `x > limit + EPS` for an over limit, `x < limit - EPS` for an under limit.
const EPS: f64 = 1e-9;

/// A frame longer than this (ms) is "long": kept in the window, but not in p95 or the
/// quantised test (R4).
const LONG_MS: f64 = 100.0;

/// Trials tolerate the larger of a half millisecond and three percent of reference frame time.
const TRIAL_NOISE_MS: f64 = 0.5;
/// Relative part of the fixed trial-noise allowance.
const TRIAL_NOISE_SHARE: f64 = 0.03;

/// Terrain keeps its runtime identity when the optional priority order changes.
const TERRAIN_KNOB: usize = 2;

/// A frame longer than this (ms) is a gap (R2) and, in the world, a stall (R8). It never enters
/// the window.
const STALL_MS: f64 = 250.0;

/// After a gap frame the governor is inactive for this long (R2).
const GAP_PAUSE_S: f64 = 2.0;

/// No frame for longer than this means presents stopped: inactive (R2).
const PRESENTS_STOPPED_S: f64 = 0.25;

/// After an undo, frames that arrive within this long are not added to the window (T2). In the
/// game the undo takes effect with a lag. Tuned in Task 17.
const UNDO_GRACE_S: f64 = 0.5;

/// A second stall within this long of the first undoes the top of the stack (R8).
const STALL_WINDOW_S: f64 = 10.0;

/// The most frames the window holds; the oldest go first (T6).
const MAX_WINDOW_FRAMES: usize = 4096;

/// A step of `now_s` back by more than this is a re-base, not a clamp (T5).
const REBASE_S: f64 = 1.0;

/// A display period (ms) outside this range is no display period (R3): 20 to 1000 Hz.
const MIN_PERIOD_MS: f64 = 1.0;
const MAX_PERIOD_MS: f64 = 50.0;

/// A change of more than this share in the display period is a reset point (R3).
const PERIOD_CHANGE: f64 = 0.05;

/// Quantised mode needs a display period of at least this share of the target (R5).
const QUANTISED_MIN_PERIOD: f64 = 0.5;

/// In quantised mode a frame is on time at or below `target × ON_TIME` (R6). The same factor
/// widens the vsync multiple in the hitch threshold (R7).
const ON_TIME: f64 = 1.05;

/// Quantised headroom: at most this share of late frames (R6).
const LATE_HEADROOM: f64 = 0.01;

/// Quantised overload: more than this share of late frames (R6).
const LATE_OVERLOAD: f64 = 0.02;

/// Overload in both modes: more than this share of long frames (R4) ...
const LONG_SHARE_OVERLOAD: f64 = 0.02;

/// ... or at least this many long frames in the window (R4).
const LONG_COUNT_OVERLOAD: usize = 2;

/// One draw-distance knob in runtime identity order. Configuration may give terrain first priority.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KnobSpec {
    /// A disabled knob is never raised.
    pub enabled: bool,
    /// The highest step. Step 0 is the player's menu value.
    pub max_step: u8,
}

/// What the governor sees on one tick.
#[derive(Debug, Clone, Copy)]
pub struct Inputs<'a> {
    /// The time of this tick, in seconds, from any fixed origin.
    pub now_s: f64,
    /// The frame times (ms) that arrived since the last tick.
    pub new_frames_ms: &'a [f64],
    /// The 7.1 hook ran recently: the world is rendering.
    pub world_active: bool,
    /// The display's refresh period (ms), or 0.0 when it is not known.
    pub period_ms: f64,
    /// Free VRAM (budget − usage), or `None` without `VK_EXT_memory_budget`.
    pub vram_free_mib: Option<u64>,
    /// `baseline_changed[k]`: the player changed knob k's menu value since the last tick.
    pub baseline_changed: &'a [bool],
}

/// A change for Task 15 to apply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Set `knob` to `step`.
    Set { knob: usize, step: u8 },
}

/// How headroom and overload are judged (R5). Remembered between ticks, with its period.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Mode {
    /// Free-running frames (VRR, or no vsync): the p95 thresholds.
    P95,
    /// Fixed-refresh vsync at display period `p` (ms, always a valid period).
    Quantised { p: f64 },
}

/// One raise on the stack.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Raise {
    knob: usize,
    /// Tells this raise apart from later raises of the same knob (R7).
    id: u64,
    /// When it was made.
    t: f64,
    /// It has stayed for BlockSeconds, and its knob's undo count was reset for it (R9).
    held: bool,
}

/// One frame in the window.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Frame {
    /// The `now_s` of the tick it arrived on.
    t: f64,
    ms: f64,
}

/// A display period that can be trusted, or `None` (R3).
fn valid_period(period_ms: f64) -> Option<f64> {
    (period_ms.is_finite() && (MIN_PERIOD_MS..=MAX_PERIOD_MS).contains(&period_ms))
        .then_some(period_ms)
}

/// The effective target frame time: the display period when the target fps is above the refresh
/// rate (R3).
fn effective_target(base_ms: f64, period: Option<f64>) -> f64 {
    period.map_or(base_ms, |p| base_ms.max(p))
}

/// True if a timer started at `since` has run for at least `hold` seconds (T4).
fn held(since: Option<f64>, now: f64, hold: f64) -> bool {
    since.is_some_and(|since| now - since + EPS >= hold)
}

/// True while a knob blocked until `until` is still blocked.
fn blocked(now: f64, until: f64) -> bool {
    now + EPS < until
}

/// The block after the n-th undo of a knob, in BlockSeconds: 1, 2, 4, then 8 for ever (R9).
fn backoff_factor(undos: u32) -> f64 {
    match undos {
        0 | 1 => 1.0,
        2 => 2.0,
        3 => 4.0,
        _ => 8.0,
    }
}

/// Reusable buffers for a tick. Reserve for the largest input batch before the loop.
#[derive(Debug)]
pub struct TickScratch {
    frames: Vec<f64>,
    actions: Vec<Action>,
    non_long: Vec<f64>,
    sorted: Vec<f64>,
}

impl TickScratch {
    /// Reserve actions for all menu resets plus one decision, and bounded frame/stat buffers.
    pub fn new(knobs: usize, max_frames: usize) -> Self {
        Self {
            frames: Vec::with_capacity(max_frames),
            actions: Vec::with_capacity(knobs.saturating_add(1)),
            non_long: Vec::with_capacity(MAX_WINDOW_FRAMES),
            sorted: Vec::with_capacity(MAX_WINDOW_FRAMES),
        }
    }
}

/// Read-only runtime activity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// World rendering and presents are available.
    Active,
    /// A required rendering signal is absent.
    Inactive,
    /// Active, with a knob temporarily blocked after an undo.
    BackingOff,
}

/// Borrowed steps and activity without allocating a status snapshot.
pub struct Status<'a> {
    /// Current applied steps, including fixed debug holds.
    pub steps: &'a [u8],
    /// Activity at the last tick.
    pub state: State,
}

/// The narrow bookkeeping needed to roll back a proposed raise.
#[derive(Debug, Clone, Copy)]
struct ProposedRaise {
    knob: usize,
    previous_raise: Option<(u64, f64)>,
    previous_time: Option<f64>,
}

/// One unaccepted quality step, measured against the retained pre-trial performance reference.
#[derive(Debug, Clone, Copy)]
struct BandTrial {
    /// Identity of the only pending step on the raise stack.
    id: u64,
    /// Target time or slower observed baseline, before spending any trial tolerance.
    reference_ms: f64,
}

/// The governor's state. See the module documentation.
#[derive(Debug, Clone)]
pub struct Governor {
    cfg: GovernorCfg,
    /// Fastest sustained reference with boosts present; accepted steps never move it upward.
    band_reference_ms: Option<f64>,
    /// At most one added step may be awaiting post-settle measurement.
    band_trial: Option<BandTrial>,
    /// First fresh frame tick after the current settle interval.
    band_window_since: Option<f64>,
    knobs: Vec<KnobSpec>,
    /// One per knob, in `0..=max_step`.
    steps: Vec<u8>,
    /// One per knob: the knob is not raised before this time (R9).
    blocked_until: Vec<f64>,
    /// One per knob: undos since the count last reset (R9).
    undo_count: Vec<u32>,
    /// The raises still in place, the most recent last (LIFO). Each knob's step equals the number
    /// of its entries.
    stack: Vec<Raise>,
    /// Debug holds are excluded from automatic raises, lowers and menu resets.
    forced: Vec<Option<u8>>,
    /// Only the most recent tick's proposed raise may be vetoed.
    proposed: Option<ProposedRaise>,
    next_id: u64,
    /// The most recent raise while it is on the stack: (id, time). Its post-raise period is open
    /// while this is `Some` (R7).
    last_raise: Option<(u64, f64)>,
    window: VecDeque<Frame>,
    mode: Mode,
    /// The clamped time of the last tick (T5).
    last_now: Option<f64>,
    /// The last tick that had a frame (R2).
    last_frame_tick: f64,
    /// The last tick that had a frame over STALL_MS (R2).
    last_gap_frame: Option<f64>,
    /// The last tick was active (R2).
    active: bool,
    /// The last reset point (T1, T3).
    reset_at: f64,
    headroom_since: Option<f64>,
    overload_since: Option<f64>,
    /// The last valid display period (R3).
    last_valid_period: Option<f64>,
    /// The last stall that did not cause an undo (R8).
    last_stall: Option<f64>,
    t_last_raise: Option<f64>,
    t_last_undo: Option<f64>,
}

impl Governor {
    /// A governor with all knobs at step 0.
    pub fn new(cfg: GovernorCfg, knobs: Vec<KnobSpec>) -> Self {
        let n = knobs.len();
        let stack_capacity = knobs
            .iter()
            .fold(0usize, |sum, k| sum.saturating_add(usize::from(k.max_step)));
        Governor {
            cfg,
            band_reference_ms: None,
            band_trial: None,
            band_window_since: None,
            knobs,
            steps: vec![0; n],
            blocked_until: vec![f64::NEG_INFINITY; n],
            undo_count: vec![0; n],
            stack: Vec::with_capacity(stack_capacity),
            forced: vec![None; n],
            proposed: None,
            next_id: 0,
            last_raise: None,
            window: VecDeque::with_capacity(MAX_WINDOW_FRAMES.saturating_mul(2)),
            mode: Mode::P95,
            last_now: None,
            last_frame_tick: 0.0,
            last_gap_frame: None,
            active: false,
            reset_at: f64::NEG_INFINITY,
            headroom_since: None,
            overload_since: None,
            last_valid_period: None,
            last_stall: None,
            t_last_raise: None,
            t_last_undo: None,
        }
    }

    /// The current step of each knob.
    pub fn steps(&self) -> &[u8] {
        &self.steps
    }

    /// Hold a validated debug step, or release its hold with `None`. Availability wins over
    /// debug: disabled knobs stay at zero. Fixed steps keep held stack entries so releasing a
    /// hold can still undo them; automatic decisions skip held knobs.
    pub fn force_step(&mut self, knob: usize, step: Option<u8>) {
        let Some(spec) = self.knobs.get(knob) else {
            return;
        };
        let step = step.map(|s| {
            if spec.enabled {
                s.min(spec.max_step)
            } else {
                0
            }
        });
        if self.forced.get(knob) == Some(&step)
            && step.is_none_or(|s| self.steps.get(knob) == Some(&s))
        {
            return;
        }
        // A later explicit control supersedes the previous tick's pending proposal.
        self.proposed = None;
        self.band_trial = None;
        self.band_reference_ms = None;
        if let Some(held) = self.forced.get_mut(knob) {
            *held = step;
        }
        if let Some(s) = step {
            if let Some(current) = self.steps.get_mut(knob) {
                *current = s;
            }
            self.remove_raises_of(knob);
            let now = self.last_now.unwrap_or(f64::NEG_INFINITY);
            for _ in 0..s {
                let id = self.next_id;
                self.next_id = self.next_id.wrapping_add(1);
                self.stack.push(Raise {
                    knob,
                    id,
                    t: now,
                    held: true,
                });
            }
            self.reset_point(now);
        }
    }

    /// Reject only a raise just proposed by the last tick. Roll back its step and stack entry,
    /// retaining the quiet period so an unavailable raise cannot spin on every frame.
    pub fn veto_raise(&mut self, knob: usize) -> bool {
        let Some(ProposedRaise {
            knob: proposed,
            previous_raise,
            previous_time,
        }) = self.proposed
        else {
            return false;
        };
        if proposed != knob || self.stack.last().map(|r| r.knob) != Some(knob) {
            return false;
        }
        self.stack.pop();
        self.band_trial = None;
        if let Some(step) = self.steps.get_mut(knob) {
            *step = step.saturating_sub(1);
        }
        self.last_raise = previous_raise;
        self.t_last_raise = previous_time;
        self.proposed = None;
        true
    }

    /// Report current state without modifying timers, steps or the raise stack.
    pub fn status(&self) -> Status<'_> {
        let now = self.last_now.unwrap_or(f64::NEG_INFINITY);
        let state = if !self.active {
            State::Inactive
        } else if self.blocked_until.iter().any(|&until| blocked(now, until)) {
            State::BackingOff
        } else {
            State::Active
        };
        Status {
            steps: &self.steps,
            state,
        }
    }

    /// Turn a knob on or off. Disabling a knob that is above step 0 resets it to 0, removes all
    /// its raises from the stack and is a reset point (T1) at the last tick's time; the returned
    /// action says so. Every other call (enabling, disabling a knob at 0, the current value, a
    /// knob out of range) returns `None`. A disabled knob is never raised.
    pub fn set_enabled(&mut self, knob: usize, enabled: bool) -> Option<Action> {
        let spec = self.knobs.get_mut(knob)?;
        if spec.enabled == enabled {
            return None;
        }
        spec.enabled = enabled;
        if !enabled {
            if let Some(held) = self.forced.get_mut(knob) {
                *held = None;
            }
        }
        if enabled {
            return None;
        }
        let step = self.steps.get_mut(knob)?;
        if *step == 0 {
            return None;
        }
        *step = 0;
        self.remove_raises_of(knob);
        self.reset_point(self.last_now.unwrap_or(f64::NEG_INFINITY));
        Some(Action::Set { knob, step: 0 })
    }

    /// One tick: take the inputs, return the knob changes to apply (in order).
    pub fn tick(&mut self, i: &Inputs) -> Vec<Action> {
        let mut scratch = TickScratch::new(self.knobs.len(), i.new_frames_ms.len());
        self.tick_with(&mut scratch, i).to_vec()
    }

    /// Run the unchanged tick using reserved storage. Inputs larger than the reserved bound may
    /// grow the buffers; callers reserve for their maximum batch before entering the loop.
    pub fn tick_with<'a>(&mut self, scratch: &'a mut TickScratch, i: &Inputs) -> &'a [Action] {
        let TickScratch {
            frames,
            actions,
            non_long,
            sorted,
        } = scratch;
        actions.clear();
        self.proposed = None;
        frames.clear();
        let now = self.clamp_time(i.now_s);
        frames.extend(
            i.new_frames_ms
                .iter()
                .copied()
                .filter(|ms| ms.is_finite() && *ms > 0.0),
        );
        if !frames.is_empty() {
            self.last_frame_tick = now;
        }
        let gap_frame = frames.iter().any(|&ms| ms > STALL_MS + EPS);
        if gap_frame {
            self.last_gap_frame = Some(now);
        }

        // (i)
        if self.cfg.target_fps == 0 {
            return actions;
        }
        let base_ms = 1000.0 / f64::from(self.cfg.target_fps);
        let period = valid_period(i.period_ms);
        self.reset_held_backoff(now);

        // (ii) Menu resets, also while the world is inactive (the player is in a menu).
        let mut skip_window = false;
        for knob in 0..self.knobs.len() {
            if i.baseline_changed.get(knob) == Some(&true)
                && self.forced.get(knob).is_none_or(Option::is_none)
            {
                self.menu_reset(knob, now);
                actions.push(Action::Set { knob, step: 0 });
                skip_window = true;
            }
        }
        if self.period_changed(period) {
            self.reset_point(now);
            skip_window = true;
        }

        // (iii) The hitch undo, else the stall undo. Nothing counts while the world is inactive.
        let mut undone = None;
        if i.world_active {
            undone = self.hitch_undo(frames, now, base_ms, period);
            if undone.is_none() && gap_frame {
                undone = self.stall(now);
            }
        }
        if let Some(action) = undone {
            actions.push(action);
            skip_window = true;
        }

        // (iv) Inactive: hold the steps, decide nothing.
        let recent_gap = self
            .last_gap_frame
            .is_some_and(|t| now - t <= GAP_PAUSE_S + EPS);
        let presents_stopped = now - self.last_frame_tick > PRESENTS_STOPPED_S + EPS;
        if !i.world_active || recent_gap || presents_stopped {
            self.active = false;
            self.headroom_since = None;
            self.overload_since = None;
            return actions;
        }
        if !self.active {
            self.active = true;
            self.reset_point(now);
        }

        // (v) The window. Frames measured under the old steps are not added.
        let in_grace = self
            .t_last_undo
            .is_some_and(|t| now - t <= UNDO_GRACE_S + EPS);
        let band_settling =
            self.cfg.trial_floor_fps != 0 && now - self.reset_at + EPS < self.cfg.settle_seconds;
        if !skip_window && !in_grace && !band_settling {
            if self.cfg.trial_floor_fps != 0 && frames.iter().any(|&ms| ms <= STALL_MS + EPS) {
                self.band_window_since.get_or_insert(now);
            }
            self.window.extend(
                frames
                    .iter()
                    .filter(|&&ms| ms <= STALL_MS + EPS)
                    .map(|&ms| Frame { t: now, ms }),
            );
        }
        self.evict(now);

        // (vi)
        if undone.is_none() {
            if let Some(action) =
                self.decide_with(now, base_ms, period, i.vram_free_mib, non_long, sorted)
            {
                actions.push(action);
            }
        }
        actions
    }

    /// T5: the first tick starts the clock; a time that is not finite is the last tick's; a small
    /// step back is clamped; a step back of more than REBASE_S is a re-base and a reset point; a
    /// step forward of more than REBASE_S is a reset point.
    fn clamp_time(&mut self, raw: f64) -> f64 {
        let now = match self.last_now {
            None => {
                let now = if raw.is_finite() { raw } else { 0.0 };
                self.last_frame_tick = now;
                now
            }
            Some(last) if !raw.is_finite() => last,
            Some(last) if raw >= last => {
                // A step forward of more than REBASE_S is a reset point too (review T11-2): one
                // bad future time must not end the quiet period and the hold timers on its own.
                if raw - last > REBASE_S + EPS {
                    self.reset_point(raw);
                }
                raw
            }
            Some(last) if last - raw <= REBASE_S + EPS => last,
            Some(last) => {
                self.rebase(raw - last);
                self.reset_point(raw);
                raw
            }
        };
        self.last_now = Some(now);
        now
    }

    /// Move every stored time by `delta`, so that the durations already running (blocks, the
    /// probation, the stall record) carry over to the new time base.
    fn rebase(&mut self, delta: f64) {
        self.last_frame_tick += delta;
        for until in &mut self.blocked_until {
            *until += delta;
        }
        for raise in &mut self.stack {
            raise.t += delta;
        }
        if let Some((_, t)) = self.last_raise.as_mut() {
            *t += delta;
        }
        let times = [
            &mut self.last_gap_frame,
            &mut self.last_stall,
            &mut self.t_last_raise,
            &mut self.t_last_undo,
        ];
        for t in times.into_iter().flatten() {
            *t += delta;
        }
    }

    /// T1: clear the window, restart the hold timers, start the quiet period.
    fn reset_point(&mut self, now: f64) {
        self.window.clear();
        self.headroom_since = None;
        self.overload_since = None;
        self.reset_at = now;
        self.band_window_since = None;
    }

    /// R9: a raise that has stayed on the stack for BlockSeconds resets its knob's undo count.
    /// That is an event, once per raise: a raise that goes on staying does not reset the count
    /// again, so the back-off for a higher step of the same knob still engages. A raise that was
    /// already on the stack when its knob was undone never resets it (`undo` marks it held),
    /// because a lower step that was holding says nothing new about the step that failed.
    fn reset_held_backoff(&mut self, now: f64) {
        let pending = self.band_trial.map(|trial| trial.id);
        for raise in &mut self.stack {
            if !raise.held
                && pending != Some(raise.id)
                && now - raise.t + EPS >= self.cfg.block_seconds
            {
                raise.held = true;
                if let Some(count) = self.undo_count.get_mut(raise.knob) {
                    *count = 0;
                }
            }
        }
    }

    /// R1 (ii): the player changed knob `knob`'s menu value.
    fn menu_reset(&mut self, knob: usize, now: f64) {
        if let Some(step) = self.steps.get_mut(knob) {
            *step = 0;
        }
        self.remove_raises_of(knob);
        if let Some(until) = self.blocked_until.get_mut(knob) {
            *until = f64::NEG_INFINITY;
        }
        if let Some(count) = self.undo_count.get_mut(knob) {
            *count = 0;
        }
        self.reset_point(now);
    }

    /// Remove every stack entry of `knob`. If the most recent raise was one of them, its
    /// post-raise period ends (R7).
    fn remove_raises_of(&mut self, knob: usize) {
        self.stack.retain(|raise| raise.knob != knob);
        if let Some((id, _)) = self.last_raise {
            if !self.stack.iter().any(|raise| raise.id == id) {
                self.last_raise = None;
            }
        }
        if self
            .band_trial
            .is_some_and(|trial| !self.stack.iter().any(|raise| raise.id == trial.id))
        {
            self.band_trial = None;
        }
    }

    /// R3: true if `period` is valid and more than 5 % away from the last valid period.
    fn period_changed(&mut self, period: Option<f64>) -> bool {
        let Some(p) = period else {
            return false;
        };
        let changed = self
            .last_valid_period
            .is_some_and(|last| (p - last).abs() > PERIOD_CHANGE * last + EPS);
        self.last_valid_period = Some(p);
        changed
    }

    /// R7: a frame over the hitch threshold in the post-raise period undoes that raise at once,
    /// if it is still the top of the stack.
    fn hitch_undo(
        &mut self,
        frames: &[f64],
        now: f64,
        base_ms: f64,
        period: Option<f64>,
    ) -> Option<Action> {
        let (id, t_raise) = self.last_raise?;
        let period_end = t_raise + self.cfg.settle_seconds + self.cfg.raise_hold_seconds;
        if now > period_end + EPS && self.band_trial.is_none_or(|trial| trial.id != id) {
            return None;
        }
        let limit = self.hitch_limit(base_ms, period);
        if !frames.iter().any(|&ms| ms > limit + EPS) {
            return None;
        }
        if self
            .stack
            .iter()
            .rev()
            .find(|raise| self.forced.get(raise.knob).is_none_or(Option::is_none))
            .map(|raise| raise.id)
            != Some(id)
        {
            return None;
        }
        self.last_raise = None;
        self.undo(now)
    }

    /// R7: the hitch threshold in the remembered mode (R5). In quantised mode the stored period
    /// is used, so a tick whose own period is not valid still gets the quantised threshold.
    fn hitch_limit(&self, base_ms: f64, period: Option<f64>) -> f64 {
        match self.mode {
            Mode::P95 => self.cfg.hitch_factor * effective_target(base_ms, period),
            Mode::Quantised { p } => {
                let target = base_ms.max(p);
                // m: the vsync multiple that is on time. One more is one missed vsync, which is
                // not a hitch.
                let m = (target * ON_TIME / p).floor();
                let m = if m.is_finite() {
                    m.clamp(1.0, 4.0)
                } else {
                    1.0
                };
                (self.cfg.hitch_factor * target).max((m + 1.0) * p * ON_TIME)
            }
        }
    }

    /// R8: a stall (a frame over STALL_MS in the world while a knob is raised). The second stall
    /// within STALL_WINDOW_S undoes the top of the stack and uses the record up.
    fn stall(&mut self, now: f64) -> Option<Action> {
        if self.steps.iter().all(|&step| step == 0) {
            return None;
        }
        match self.last_stall {
            Some(t) if now - t <= STALL_WINDOW_S + EPS => {
                self.last_stall = None;
                self.undo(now)
            }
            _ => {
                self.last_stall = Some(now);
                None
            }
        }
    }

    /// R9: undo the most recent raise and block its knob. A reset point; starts the undo grace
    /// (T2) and the probation (R10). An empty stack gives `None`.
    fn undo(&mut self, now: f64) -> Option<Action> {
        self.undo_with_priority(now, false)
    }

    /// Normal backoff retains terrain until every other automatic boost has been undone.
    fn undo_normal(&mut self, now: f64) -> Option<Action> {
        self.undo_with_priority(now, self.cfg.terrain_first)
    }

    /// Remove one automatic stack entry. Immediate safety and failed-trial rollback use LIFO.
    fn undo_with_priority(&mut self, now: f64, terrain_last: bool) -> Option<Action> {
        let forced = &self.forced;
        let automatic = |r: &Raise| forced.get(r.knob).is_none_or(Option::is_none);
        while let Some(index) = terrain_last
            .then(|| {
                self.stack
                    .iter()
                    .rposition(|r| r.knob != TERRAIN_KNOB && automatic(r))
            })
            .flatten()
            .or_else(|| self.stack.iter().rposition(automatic))
        {
            // rposition returned an index in this same, unmodified vector.
            let raise = self.stack.remove(index);
            if self.band_trial.is_some_and(|trial| trial.id == raise.id) {
                self.band_trial = None;
            }
            // The entry leaves the stack on every path below, the skips included (R7).
            if self.last_raise.is_some_and(|(id, _)| id == raise.id) {
                self.last_raise = None;
            }
            let Some(step) = self.steps.get_mut(raise.knob) else {
                continue;
            };
            if *step == 0 {
                continue;
            }
            *step = step.saturating_sub(1);
            let step = *step;
            let undos = match self.undo_count.get_mut(raise.knob) {
                Some(count) => {
                    *count = count.saturating_add(1);
                    *count
                }
                None => 1,
            };
            if let Some(until) = self.blocked_until.get_mut(raise.knob) {
                *until = now + self.cfg.block_seconds * backoff_factor(undos);
            }
            // R9: a raise of this knob that was already on the stack says nothing new about the
            // step that failed, so it can never reset the count. Only a raise made after this
            // undo that then holds, or a menu reset, resets it.
            for lower in self.stack.iter_mut().filter(|r| r.knob == raise.knob) {
                lower.held = true;
            }
            self.t_last_undo = Some(now);
            self.reset_point(now);
            return Some(Action::Set {
                knob: raise.knob,
                step,
            });
        }
        None
    }

    /// T6: drop frames older than WindowSeconds, then the oldest beyond MAX_WINDOW_FRAMES.
    fn evict(&mut self, now: f64) {
        let keep_s = self.cfg.window_seconds + EPS;
        while self.window.front().is_some_and(|f| now - f.t > keep_s) {
            self.window.pop_front();
        }
        while self.window.len() > MAX_WINDOW_FRAMES {
            self.window.pop_front();
        }
    }

    /// R5 + R6: decide the mode and judge the window. Returns (headroom, overload); an empty
    /// window gives (false, false) and keeps the remembered mode.
    fn assess_with(
        &mut self,
        target: f64,
        period: Option<f64>,
        non_long: &mut Vec<f64>,
        sorted: &mut Vec<f64>,
    ) -> (bool, bool) {
        let total = self.window.len();
        if total == 0 {
            return (false, false);
        }
        non_long.clear();
        non_long.extend(
            self.window
                .iter()
                .map(|f| f.ms)
                .filter(|&ms| ms <= LONG_MS + EPS),
        );
        let long_count = total.saturating_sub(non_long.len());
        let long_share = long_count as f64 / total as f64;
        let long_overload =
            long_share > LONG_SHARE_OVERLOAD + EPS || long_count >= LONG_COUNT_OVERLOAD;
        let no_long = long_count == 0;

        self.mode = match period {
            Some(p)
                if p + EPS >= QUANTISED_MIN_PERIOD * target && stats::quantised(non_long, p) =>
            {
                Mode::Quantised { p }
            }
            _ => Mode::P95,
        };

        match self.mode {
            Mode::Quantised { .. } => {
                let late = 1.0 - stats::share_at_or_below(non_long, target * ON_TIME + EPS);
                let headroom = late <= LATE_HEADROOM + EPS && no_long;
                let overload = late > LATE_OVERLOAD + EPS || long_overload;
                (headroom, overload)
            }
            Mode::P95 => {
                if non_long.is_empty() {
                    return (false, long_overload);
                }
                let p95 = stats::p95_with(non_long, sorted);
                let headroom = p95 < self.cfg.raise_below * target - EPS && no_long;
                let overload = p95 > self.cfg.lower_above * target + EPS || long_overload;
                (headroom, overload)
            }
        }
    }

    /// Compatibility seam for the unchanged ordinary assessment tests.
    #[cfg(test)]
    fn assess(&mut self, target: f64, period: Option<f64>) -> (bool, bool) {
        self.assess_with(target, period, &mut Vec::new(), &mut Vec::new())
    }

    /// Compatibility seam for the unchanged ordinary decision tests.
    #[cfg(test)]
    fn decide(
        &mut self,
        now: f64,
        base_ms: f64,
        period: Option<f64>,
        vram: Option<u64>,
    ) -> Option<Action> {
        self.decide_with(now, base_ms, period, vram, &mut Vec::new(), &mut Vec::new())
    }

    /// R1 (vi): an overload lower, else a raise.
    fn decide_with(
        &mut self,
        now: f64,
        base_ms: f64,
        period: Option<f64>,
        vram_free_mib: Option<u64>,
        non_long: &mut Vec<f64>,
        sorted: &mut Vec<f64>,
    ) -> Option<Action> {
        let target = effective_target(base_ms, period);
        if self.cfg.trial_floor_fps != 0 {
            return self.decide_band_with(now, target, period, vram_free_mib, non_long, sorted);
        }
        let (headroom, overload) = self.assess_with(target, period, non_long, sorted);
        // T4: a timer starts on the first tick its condition is true and stops when it is false.
        self.headroom_since = headroom.then(|| self.headroom_since.unwrap_or(now));
        self.overload_since = overload.then(|| self.overload_since.unwrap_or(now));

        // T3: the quiet period after a reset point.
        if now - self.reset_at + EPS < self.cfg.settle_seconds {
            return None;
        }
        if held(self.overload_since, now, self.cfg.lower_hold_seconds) {
            if let Some(action) = self.undo_normal(now) {
                return Some(action);
            }
        }
        let vram_ok =
            vram_free_mib.is_none_or(|free| free >= u64::from(self.cfg.min_vram_free_mib));
        if headroom
            && !overload
            && held(self.headroom_since, now, self.cfg.raise_hold_seconds)
            && vram_ok
            && self.raise_allowed(now)
        {
            return self.raise(now);
        }
        None
    }

    /// Probe one step using fresh p95 observations. The floor stops trials; the retained reference
    /// prevents deliberately trading target FPS down to the floor or spending tolerance repeatedly.
    fn decide_band_with(
        &mut self,
        now: f64,
        target: f64,
        period: Option<f64>,
        vram_free_mib: Option<u64>,
        non_long: &mut Vec<f64>,
        sorted: &mut Vec<f64>,
    ) -> Option<Action> {
        // Preserve remembered vsync mode for the independent hitch safety check.
        self.assess_with(target, period, non_long, sorted);
        let total = self.window.len();
        let long_count = total.saturating_sub(non_long.len());
        let long_overload = long_count >= LONG_COUNT_OVERLOAD
            || (total > 0 && long_count as f64 / total as f64 > LONG_SHARE_OVERLOAD + EPS);
        let full_window = held(self.band_window_since, now, self.cfg.window_seconds);
        let p95 = (!non_long.is_empty()).then(|| stats::p95_with(non_long, sorted));
        let floor = 1000.0 / f64::from(self.cfg.trial_floor_fps);
        let baseline = self.steps.iter().all(|&step| step == 0);
        let reference = self
            .band_trial
            .map(|trial| trial.reference_ms)
            .or_else(|| (!baseline).then_some(self.band_reference_ms).flatten())
            .or(p95)
            .unwrap_or(target)
            .max(target);
        let cost_limit = reference + TRIAL_NOISE_MS.max(reference * TRIAL_NOISE_SHARE);
        let costly = p95.is_some_and(|ms| ms > cost_limit + EPS);
        let at_or_below_floor = p95.is_some_and(|ms| ms + EPS >= floor);
        let overload = long_overload || (full_window && (at_or_below_floor || costly));
        let stable =
            full_window && long_count == 0 && !costly && p95.is_some_and(|ms| ms < floor - EPS);
        self.headroom_since = stable.then(|| self.headroom_since.unwrap_or(now));
        self.overload_since = overload.then(|| self.overload_since.unwrap_or(now));
        if now - self.reset_at + EPS < self.cfg.settle_seconds {
            return None;
        }
        if held(self.overload_since, now, self.cfg.lower_hold_seconds) {
            let action = if self.band_trial.is_some() {
                self.undo(now)
            } else {
                self.undo_normal(now)
            };
            if action.is_some() {
                return action;
            }
        }
        if self.band_trial.is_some() {
            if held(self.headroom_since, now, self.cfg.raise_hold_seconds) {
                self.band_trial = None;
                self.band_reference_ms = p95.map(|ms| reference.min(ms.max(target)));
            }
            // Accepting a step never proposes a second step on the same tick.
            return None;
        }
        if held(self.headroom_since, now, self.cfg.raise_hold_seconds) {
            // Recovering the target tightens the reference even when no new step can be tried.
            self.band_reference_ms = p95.map(|ms| reference.min(ms.max(target)));
        }
        let probation = self
            .t_last_undo
            .is_some_and(|t| now - t + EPS < self.cfg.block_seconds);
        if stable
            && held(self.headroom_since, now, self.cfg.raise_hold_seconds)
            && !probation
            && vram_free_mib.is_some_and(|free| free >= u64::from(self.cfg.min_vram_free_mib))
            && self.raise_allowed(now)
        {
            self.band_reference_ms = p95.map(|ms| reference.min(ms.max(target)));
            let action = self.raise(now)?;
            if let Some((id, _)) = self.last_raise {
                self.band_trial = Some(BandTrial {
                    id,
                    reference_ms: self.band_reference_ms.unwrap_or(reference),
                });
            }
            return Some(action);
        }
        None
    }

    /// R10: after an undo the governor is on probation until BlockSeconds pass with no undo. On
    /// probation a raise also needs BlockSeconds since the last raise.
    fn raise_allowed(&self, now: f64) -> bool {
        let block = self.cfg.block_seconds;
        let on_probation = self.t_last_undo.is_some_and(|t| now - t + EPS < block);
        !on_probation || self.t_last_raise.is_none_or(|t| now - t + EPS >= block)
    }

    /// R11: raise the first knob, in configured priority order, that is below its maximum and not
    /// blocked. A reset point; opens the raise's post-raise period (R7).
    fn raise(&mut self, now: f64) -> Option<Action> {
        let available = |knob: usize| {
            self.knobs
                .get(knob)
                .zip(self.steps.get(knob))
                .zip(self.blocked_until.get(knob))
                .is_some_and(|((spec, &step), &until)| {
                    self.forced.get(knob).is_none_or(Option::is_none)
                        && spec.enabled
                        && step < spec.max_step
                        && !blocked(now, until)
                })
        };
        let knob = self
            .cfg
            .terrain_first
            .then_some(TERRAIN_KNOB)
            .filter(|&knob| available(knob))
            .or_else(|| (0..self.knobs.len()).find(|&knob| available(knob)))?;
        self.proposed = Some(ProposedRaise {
            knob,
            previous_raise: self.last_raise,
            previous_time: self.t_last_raise,
        });
        let step = self.steps.get_mut(knob)?;
        *step = step.saturating_add(1);
        let step = *step;
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1);
        self.stack.push(Raise {
            knob,
            id,
            t: now,
            held: false,
        });
        self.last_raise = Some((id, now));
        self.t_last_raise = Some(now);
        self.reset_point(now);
        Some(Action::Set { knob, step })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, GovernorCfg};
    use std::cell::RefCell;
    use std::collections::HashMap;

    const P240: f64 = 1000.0 / 240.0;
    const P144: f64 = 1000.0 / 144.0;
    const P120: f64 = 1000.0 / 120.0;
    const P60: f64 = 1000.0 / 60.0;
    const TICK_S: f64 = 0.05;
    const TICK_MS: f64 = 50.0;
    /// The default BlockSeconds (20 s) in ticks.
    const BLOCK_TICKS: u64 = 400;

    fn gov() -> Governor {
        Governor::new(
            Config::default().governor,
            vec![
                KnobSpec {
                    enabled: true,
                    max_step: 4
                };
                3
            ],
        )
    }

    fn knob(max_step: u8) -> KnobSpec {
        KnobSpec {
            enabled: true,
            max_step,
        }
    }

    fn gov_with(knobs: Vec<KnobSpec>) -> Governor {
        Governor::new(Config::default().governor, knobs)
    }

    fn with_target(target_fps: u32, knobs: Vec<KnobSpec>) -> Governor {
        let cfg = GovernorCfg {
            target_fps,
            ..Config::default().governor
        };
        Governor::new(cfg, knobs)
    }

    // ---- Harness (pinned by the dispatch notes, "Tests") ----
    //
    // Tick n is at now = n × 0.05 s, from n = 1, and the counter continues across calls. A frame
    // clock (ms) emits each frame on the first tick at which its cumulative end time ≤ n × 50.
    // The counter and the clock belong to one governor (keyed by its address); a governor that
    // has never ticked starts again from n = 1 and clock 0.

    #[derive(Debug, Default)]
    struct Sim {
        n: u64,
        clock_ms: f64,
        period: f64,
    }

    thread_local! {
        static SIMS: RefCell<HashMap<usize, Sim>> = RefCell::new(HashMap::new());
    }

    fn key(g: &Governor) -> usize {
        std::ptr::from_ref(g) as usize
    }

    /// The next tick: returns (n, frame clock, period). `Some(period)` replaces the stored period.
    ///
    /// RULE: never mix the direct-time helpers (`at`, or a direct `g.tick`) with the harness
    /// helpers (`run`, `run_pattern`, `run_core`, `feed`, `empty_tick`, `tick_with`) on one
    /// governor. The harness keys its tick counter and frame clock by the governor's address and
    /// starts them again only for a governor that has never ticked. After a direct tick it would
    /// carry on from a counter that does not match the governor's clock, and its `now` could go
    /// backwards.
    fn advance(g: &Governor, period: Option<f64>) -> (u64, f64, f64) {
        let fresh = g.last_now.is_none();
        SIMS.with(|sims| {
            let mut sims = sims.borrow_mut();
            let sim = sims.entry(key(g)).or_default();
            if fresh {
                *sim = Sim::default();
            }
            sim.n += 1;
            if let Some(p) = period {
                sim.period = p;
            }
            (sim.n, sim.clock_ms, sim.period)
        })
    }

    fn set_clock(g: &Governor, clock_ms: f64) {
        SIMS.with(|sims| {
            if let Some(sim) = sims.borrow_mut().get_mut(&key(g)) {
                sim.clock_ms = clock_ms;
            }
        });
    }

    /// One action, with the tick it came on and whether it raised its knob.
    #[derive(Debug, Clone, Copy, PartialEq)]
    struct Ev {
        n: u64,
        knob: usize,
        step: u8,
        raise: bool,
    }

    /// One tick at n; each action is classed against the knob's step before it.
    fn tick_ev(
        g: &mut Governor,
        n: u64,
        frames: &[f64],
        world_active: bool,
        period_ms: f64,
        vram_free_mib: Option<u64>,
        baseline_changed: &[bool],
    ) -> Vec<Ev> {
        let mut prev = g.steps().to_vec();
        let inputs = Inputs {
            now_s: n as f64 * TICK_S,
            new_frames_ms: frames,
            world_active,
            period_ms,
            vram_free_mib,
            baseline_changed,
        };
        g.tick(&inputs)
            .into_iter()
            .map(|Action::Set { knob, step }| {
                let before = prev.get(knob).copied().unwrap_or(0);
                if let Some(p) = prev.get_mut(knob) {
                    *p = step;
                }
                Ev {
                    n,
                    knob,
                    step,
                    raise: step > before,
                }
            })
            .collect()
    }

    /// EXACTLY round(secs / 0.05) ticks. `frame(clock_ms, steps, index)` gives the length of the
    /// next frame; `index` counts the frames this call has emitted.
    fn run_core(
        g: &mut Governor,
        secs: f64,
        world_active: bool,
        period: f64,
        vram_free_mib: Option<u64>,
        frame: &mut dyn FnMut(f64, &[u8], usize) -> f64,
    ) -> Vec<Ev> {
        let ticks = (secs / TICK_S).round() as u64;
        let mut events = Vec::new();
        let mut index = 0;
        for _ in 0..ticks {
            let (n, mut clock, _) = advance(g, Some(period));
            let end = n as f64 * TICK_MS;
            let mut frames = Vec::new();
            while frames.len() < 100_000 {
                let ms = frame(clock, g.steps(), index);
                if clock + ms > end {
                    break;
                }
                frames.push(ms);
                clock += ms;
                index += 1;
            }
            set_clock(g, clock);
            let baseline = vec![false; g.steps().len()];
            events.extend(tick_ev(
                g,
                n,
                &frames,
                world_active,
                period,
                vram_free_mib,
                &baseline,
            ));
        }
        events
    }

    fn actions(events: Vec<Ev>) -> Vec<Action> {
        events
            .into_iter()
            .map(|e| Action::Set {
                knob: e.knob,
                step: e.step,
            })
            .collect()
    }

    /// `frame_ms` frames until `secs` have passed.
    fn run(
        g: &mut Governor,
        secs: f64,
        frame_ms: f64,
        world_active: bool,
        period: f64,
    ) -> Vec<Action> {
        actions(run_core(
            g,
            secs,
            world_active,
            period,
            None,
            &mut |_, _, _| frame_ms,
        ))
    }

    /// Frames from `pattern`, repeated, until `secs` have passed.
    fn run_pattern(
        g: &mut Governor,
        secs: f64,
        pattern: &[f64],
        world_active: bool,
        period: f64,
    ) -> Vec<Action> {
        actions(run_core(
            g,
            secs,
            world_active,
            period,
            None,
            &mut |_, _, i| pattern[i % pattern.len()],
        ))
    }

    /// ONE tick with exactly these frames: world active, the previous call's period, no VRAM
    /// figure, no menu change. It does not advance the frame clock.
    fn feed(g: &mut Governor, frames: &[f64]) -> Vec<Action> {
        let (n, _, period) = advance(g, None);
        let baseline = vec![false; g.steps().len()];
        actions(tick_ev(g, n, frames, true, period, None, &baseline))
    }

    /// One tick with no frames and the world active.
    fn empty_tick(g: &mut Governor) -> Vec<Action> {
        feed(g, &[])
    }

    /// One tick with every input given. It does not advance the frame clock.
    fn tick_with(
        g: &mut Governor,
        frames: &[f64],
        world_active: bool,
        period: f64,
        vram_free_mib: Option<u64>,
        baseline_changed: &[bool],
    ) -> Vec<Action> {
        let (n, _, _) = advance(g, Some(period));
        actions(tick_ev(
            g,
            n,
            frames,
            world_active,
            period,
            vram_free_mib,
            baseline_changed,
        ))
    }

    /// Replays the actions from all-zero steps: a Set is a raise when its step is above that
    /// knob's previous step.
    fn count_raises(a: &[Action]) -> usize {
        let mut prev: Vec<u8> = Vec::new();
        let mut raises = 0;
        for &Action::Set { knob, step } in a {
            if prev.len() <= knob {
                prev.resize(knob + 1, 0);
            }
            if step > prev[knob] {
                raises += 1;
            }
            prev[knob] = step;
        }
        raises
    }

    /// One tick at an explicit time (no harness), 240 Hz, world active.
    fn at(g: &mut Governor, now_s: f64, frames: &[f64]) -> Vec<Action> {
        g.tick(&Inputs {
            now_s,
            new_frames_ms: frames,
            world_active: true,
            period_ms: P240,
            vram_free_mib: None,
            baseline_changed: &[],
        })
    }

    /// Frames of 15 ms while knob 0 is raised, else 10 ms: frames that respond to the steps.
    fn responds(_clock: f64, steps: &[u8], _i: usize) -> f64 {
        if steps[0] > 0 {
            15.0
        } else {
            10.0
        }
    }

    // ---- The brief's 9 tests ----

    #[test]
    fn raises_one_step_per_settle_with_headroom() {
        let mut g = gov();
        let a = run(&mut g, 2.05, 10.0, true, 1000.0 / 240.0); // 100 fps vs 75 target, 240 Hz
        assert_eq!(g.steps(), &[1, 0, 0]);
        assert_eq!(a.len(), 1);
    }

    #[test]
    fn lowers_after_hold_when_overloaded() {
        let mut g = gov();
        run(&mut g, 2.05, 10.0, true, 1000.0 / 240.0);
        run(&mut g, 3.0, 15.0, true, 1000.0 / 240.0); // 66 fps
        assert_eq!(g.steps(), &[0, 0, 0]);
    }

    #[test]
    fn hitch_after_raise_undoes_it() {
        let mut g = gov();
        run(&mut g, 2.05, 10.0, true, 1000.0 / 240.0);
        feed(&mut g, &[10.0, 10.0, 120.0, 10.0]); // 120 ms > 2x13.3 ms, within settle+hold
        run(&mut g, 1.2, 10.0, true, 1000.0 / 240.0);
        // The hitch undoes the raise at 2.10. No raise follows in run(1.2): the R10 raise limit
        // and the T2 undo grace each prevent it on their own. With both off, k1 would be raised at
        // 3.15 and the steps would be [0, 1, 0].
        assert_eq!(g.steps(), &[0, 0, 0]);
    }

    #[test]
    fn pauses_when_world_not_active() {
        let mut g = gov();
        let a = run(&mut g, 5.0, 10.0, false, 1000.0 / 240.0);
        assert!(a.is_empty());
        assert_eq!(g.steps(), &[0, 0, 0]);
    }

    #[test]
    fn fifo_120hz_holds_refresh_without_flapping() {
        let mut g = gov();
        let p = 1000.0 / 120.0;
        let a = run_pattern(&mut g, 20.0, &[p, p, p, p, p, p, p, p, p, 2.0 * p], true, p); // 10 % at 60 fps
        assert!(a.len() <= 2); // no flapping
    }

    #[test]
    fn target_above_refresh_uses_refresh() {
        let mut g = Governor::new(
            GovernorCfg {
                target_fps: 144,
                ..Config::default().governor
            },
            vec![KnobSpec {
                enabled: true,
                max_step: 4,
            }],
        );
        run(&mut g, 2.05, 1000.0 / 120.0, true, 1000.0 / 120.0); // locked at 120 Hz = full headroom
        assert_eq!(g.steps(), &[1]);
    }

    #[test]
    fn no_raise_when_vram_low() {
        // vram_free_mib Some(512) -> steps stay [0,0,0]
        let mut g = gov();
        run_core(&mut g, 5.0, true, P240, Some(512), &mut |_, _, _| 10.0);
        assert_eq!(g.steps(), &[0, 0, 0]);
        // Positive control: exactly MinVramFreeMiB free is enough.
        let mut g = gov();
        run_core(&mut g, 1.05, true, P240, Some(1024), &mut |_, _, _| 10.0);
        assert_eq!(g.steps(), &[1, 0, 0]);
    }

    #[test]
    fn menu_change_resets_knob() {
        // raise knob 0 to 1, then baseline_changed=[true,false,false] -> Set{0,0}
        let mut g = gov();
        run(&mut g, 1.05, 10.0, true, P240);
        assert_eq!(g.steps(), &[1, 0, 0]);
        let a = tick_with(&mut g, &[10.0; 5], true, P240, None, &[true, false, false]);
        assert_eq!(a, vec![Action::Set { knob: 0, step: 0 }]);
        assert_eq!(g.steps(), &[0, 0, 0]);
        assert!(g.stack.is_empty());
    }

    #[test]
    fn runaway_guard_when_frames_do_not_respond() {
        // frame time independent of steps but oscillating across thresholds -> at most 1 raise per BlockSeconds
        // Under these rules this pattern makes no raise at all: every 2 s window holds 16 ms
        // frames, so headroom never holds for RaiseHoldSeconds. The real guard test is
        // `rate_limit_bounds_raises_after_an_undo`.
        let mut g = gov();
        let a = run_pattern(
            &mut g,
            60.0,
            &[10.0; 30]
                .iter()
                .chain([16.0; 30].iter())
                .copied()
                .collect::<Vec<_>>(),
            true,
            1000.0 / 240.0,
        );
        let raises = count_raises(&a);
        assert!(raises <= 60 / 20 + 1);
        assert!(g.steps().iter().all(|&s| s <= 4));
    }

    // ---- Strengthening tests (dispatch notes) ----

    /// An event: (tick, knob, step, raise).
    fn ev(n: u64, knob: usize, step: u8, raise: bool) -> Ev {
        Ev {
            n,
            knob,
            step,
            raise,
        }
    }

    #[test]
    fn t7_worked_check_times() {
        // The tick-by-tick times of the dispatch notes' worked check (T7). n = now / 0.05.
        let ten = &mut |_: f64, _: &[u8], _: usize| 10.0;
        // raises_one_step_per_settle_with_headroom: a raise at 1.05; the next at 2.10.
        let mut g = gov();
        let a = run_core(&mut g, 2.05, true, P240, None, ten);
        assert_eq!(a, vec![ev(21, 0, 1, true)]);
        let a = run_core(&mut g, 0.05, true, P240, None, ten);
        assert_eq!(a, vec![ev(42, 0, 2, true)]);
        // lowers_after_hold_when_overloaded: a second raise at 2.10 on the first 15 ms tick, then
        // an overload lower at the end of each quiet period (3.10 and 4.10).
        let mut g = gov();
        let mut a = run_core(&mut g, 2.05, true, P240, None, ten);
        a.extend(run_core(&mut g, 3.0, true, P240, None, &mut |_, _, _| 15.0));
        assert_eq!(
            a,
            vec![
                ev(21, 0, 1, true),
                ev(42, 0, 2, true),
                ev(62, 0, 1, false),
                ev(82, 0, 0, false)
            ]
        );
        // hitch_after_raise_undoes_it: the feed tick at 2.10 undoes the raise at once.
        let mut g = gov();
        run(&mut g, 2.05, 10.0, true, P240);
        let (n, _, period) = advance(&g, None);
        let a = tick_ev(
            &mut g,
            n,
            &[10.0, 10.0, 120.0, 10.0],
            true,
            period,
            None,
            &[],
        );
        assert_eq!(a, vec![ev(42, 0, 0, false)]);
        assert!(run(&mut g, 1.2, 10.0, true, P240).is_empty());
        // target_above_refresh_uses_refresh: a raise at 1.05, the next at 2.10.
        let mut g = with_target(144, vec![knob(4)]);
        let a = run_core(&mut g, 2.10, true, P120, None, &mut |_, _, _| P120);
        assert_eq!(a, vec![ev(21, 0, 1, true), ev(42, 0, 2, true)]);
    }

    #[test]
    fn no_cascade_after_an_undo() {
        // T1: the window is cleared at every change.
        let mut g = gov_with(vec![knob(1), knob(4), knob(4)]);
        run(&mut g, 2.50, 10.0, true, P240);
        assert_eq!(g.steps(), &[1, 1, 0]); // raises at 1.05 and 2.10
        let slow = run_core(&mut g, 0.5, true, P240, None, &mut |_, _, _| 15.0); // 2.55-3.00
        assert!(slow.is_empty());
        let ev = run_core(&mut g, 2.0, true, P240, None, &mut |_, _, _| 10.0); // 3.05-5.00
        assert_eq!(
            ev,
            vec![Ev {
                n: 62,
                knob: 1,
                step: 0,
                raise: false
            }]
        ); // the undo of k1 at 3.10, and only that
        assert_eq!(g.steps(), &[1, 0, 0]);
    }

    #[test]
    fn slow_frames_after_an_undo_do_not_cascade() {
        // T2: frames in the UNDO_GRACE_S after an undo are not added to the window.
        let mut g = gov_with(vec![knob(1), knob(4), knob(4)]);
        run(&mut g, 2.50, 10.0, true, P240);
        assert_eq!(g.steps(), &[1, 1, 0]);
        let mut ev = run_core(&mut g, 0.9, true, P240, None, &mut |_, _, _| 15.0); // 2.55-3.40
        ev.extend(run_core(&mut g, 2.0, true, P240, None, &mut |_, _, _| 10.0)); // 3.45-5.40
        assert_eq!(
            ev,
            vec![Ev {
                n: 62,
                knob: 1,
                step: 0,
                raise: false
            }]
        );
        assert_eq!(g.steps(), &[1, 0, 0]);
    }

    #[test]
    fn rate_limit_tries_the_next_knob() {
        // R10: after an undo, the next raise waits until BlockSeconds after the last raise. The
        // knob that was undone is still blocked then, so the raise goes to the next knob.
        let mut g = gov();
        let ev = run_core(&mut g, 120.0, true, P240, None, &mut responds);
        let k0_raise = ev.iter().find(|e| e.raise && e.knob == 0).copied().unwrap();
        let undo = ev
            .iter()
            .find(|e| !e.raise && e.knob == 0)
            .copied()
            .unwrap();
        let next = ev
            .iter()
            .find(|e| e.raise && e.n > undo.n)
            .copied()
            .unwrap();
        assert!(
            next.n - k0_raise.n >= BLOCK_TICKS,
            "raise {k0_raise:?}, undo {undo:?}, next raise {next:?}"
        );
        assert_eq!(next.knob, 1, "{ev:?}");
        // k1 and k2 climb later; k0 is undone each time it is retried.
        assert_eq!(g.steps(), &[0, 4, 4], "{ev:?}");
    }

    #[test]
    fn rate_limit_bounds_raises_after_an_undo() {
        // R10 (spec rule 9): frames that do not respond to the steps.
        for (good_ms, bad_ms) in [(3000.0, 1000.0), (5000.0, 1000.0)] {
            let mut g = gov();
            let cycle = good_ms + bad_ms;
            let ev = run_core(&mut g, 60.0, true, P240, None, &mut |clock, _, _| {
                if clock % cycle < good_ms {
                    10.0
                } else {
                    16.0
                }
            });
            let undos: Vec<u64> = ev.iter().filter(|e| !e.raise).map(|e| e.n).collect();
            let raises: Vec<u64> = ev.iter().filter(|e| e.raise).map(|e| e.n).collect();
            let first_undo = *undos.first().expect("an undo");
            let mut checked = 0;
            for pair in raises.windows(2) {
                let (prev, next) = (pair[0], pair[1]);
                let on_probation = undos.iter().any(|&u| u < next && next - u < BLOCK_TICKS);
                if next > first_undo && on_probation {
                    assert!(
                        next - prev >= BLOCK_TICKS,
                        "{good_ms}/{bad_ms}: raises at {prev} and {next}: {ev:?}"
                    );
                    checked += 1;
                }
            }
            assert!(
                checked >= 1,
                "{good_ms}/{bad_ms}: no raise on probation: {ev:?}"
            );
        }
    }

    #[test]
    fn backoff_doubles_the_block() {
        // R9: each undo of the same knob doubles its block, up to 8 × BlockSeconds.
        let mut g = gov_with(vec![knob(4)]);
        let ev = run_core(&mut g, 470.0, true, P240, None, &mut responds);
        let waits: Vec<u64> = ev
            .windows(2)
            .filter(|w| !w[0].raise && w[1].raise)
            .map(|w| w[1].n - w[0].n)
            .collect();
        assert_eq!(waits, vec![400, 800, 1600, 3200, 3200], "{ev:?}");
    }

    #[test]
    fn backoff_count_resets_when_a_raise_holds() {
        // R9: a raise that stays on the stack for BlockSeconds resets the knob's undo count.
        let mut g = gov_with(vec![knob(1)]);
        let ev = run_core(&mut g, 100.0, true, P240, None, &mut |clock, steps, _| {
            if clock < 22_000.0 {
                if steps[0] > 0 {
                    15.0
                } else {
                    10.0
                }
            } else if (50_000.0..53_000.0).contains(&clock) {
                15.0
            } else {
                10.0
            }
        });
        let undos: Vec<Ev> = ev.iter().filter(|e| !e.raise).copied().collect();
        assert_eq!(undos.len(), 2, "{ev:?}");
        let second = undos[1];
        let next = ev
            .iter()
            .find(|e| e.raise && e.n > second.n)
            .copied()
            .unwrap();
        // The count was reset, so the block after the second undo is 20 s again, not 40 s.
        assert_eq!(next.n - second.n, BLOCK_TICKS, "{ev:?}");
    }

    #[test]
    fn backoff_doubles_for_a_higher_step_while_a_lower_step_holds() {
        // R9 (review T11-1): step 1 of the knob holds and step 2 is the one that fails. The
        // count resets once, when a raise has stayed for BlockSeconds; a raise that goes on
        // staying does not keep resetting it, so the back-off for step 2 engages.
        let mut g = gov_with(vec![knob(4)]);
        let ev = run_core(&mut g, 470.0, true, P240, None, &mut |_, steps, _| {
            if steps[0] >= 2 {
                15.0
            } else {
                10.0
            }
        });
        let waits: Vec<u64> = ev
            .windows(2)
            .filter(|w| !w[0].raise && w[1].raise)
            .map(|w| w[1].n - w[0].n)
            .collect();
        assert_eq!(waits, vec![400, 800, 1600, 3200, 3200], "{ev:?}");
    }

    #[test]
    fn hitch_after_raise_already_undone_does_nothing() {
        // R7: a hitch only undoes its own raise.
        let mut g = gov_with(vec![knob(1), knob(4), knob(4)]);
        run(&mut g, 2.10, 10.0, true, P240);
        assert_eq!(g.steps(), &[1, 1, 0]); // k0 at 1.05, k1 at 2.10
        let mut a = feed(&mut g, &[40.0]);
        a.extend(feed(&mut g, &[40.0]));
        assert_eq!(a, vec![Action::Set { knob: 1, step: 0 }]);
        assert_eq!(g.steps(), &[1, 0, 0]);
    }

    #[test]
    fn menu_reset_ends_the_post_raise_period() {
        // R7: the post-raise period ends when its entry leaves the stack by a menu reset.
        let mut g = gov_with(vec![knob(1), knob(4), knob(4)]);
        run(&mut g, 2.10, 10.0, true, P240);
        assert_eq!(g.steps(), &[1, 1, 0]);
        let a = tick_with(&mut g, &[], true, P240, None, &[false, true, false]);
        assert_eq!(a, vec![Action::Set { knob: 1, step: 0 }]);
        assert!(feed(&mut g, &[40.0]).is_empty());
        assert_eq!(g.steps(), &[1, 0, 0]);
    }

    #[test]
    fn one_missed_vsync_is_not_a_hitch() {
        // R5 + R7: 120 Hz with target 144. The mode is remembered across the raise, and one
        // missed vsync (17.0 ms, under 2 × 8.33 × 1.05 = 17.5 ms) is not a hitch.
        let mut g = with_target(144, vec![knob(4)]);
        run(&mut g, 1.05, P120, true, P120);
        assert_eq!(g.steps(), &[1]);
        assert!(g.window.is_empty());
        assert!(feed(&mut g, &[17.0]).is_empty(), "no hitch undo");
        assert_eq!(g.steps(), &[1]);
        let a = run(&mut g, 2.0, P120, true, P120);
        assert_eq!(count_raises(&a), a.len(), "nothing undone later: {a:?}");
    }

    #[test]
    fn sustained_8fps_lowers() {
        // R4: frames over 100 ms count as overload.
        let mut g = gov_with(vec![knob(1)]);
        run(&mut g, 3.10, 10.0, true, P240); // raise at 1.05; its post-raise period ends at 3.05
        assert_eq!(g.steps(), &[1]);
        let ev = run_core(&mut g, 5.0, true, P240, None, &mut |_, _, _| 125.0);
        assert_eq!(ev.len(), 1, "{ev:?}");
        assert_eq!((ev[0].knob, ev[0].step), (0, 0));
        // The first 125 ms frame arrives at n = 65 (3.25). A hitch undo would act on that tick;
        // an overload lower needs LowerHoldSeconds (6 ticks) of overload first.
        assert!(ev[0].n >= 65 + 6, "{ev:?}");
    }

    #[test]
    fn stutter_does_not_raise() {
        // R4: headroom needs no long frames.
        let mut g = gov();
        let mut pattern = vec![10.0; 60];
        pattern.push(150.0);
        let a = run_pattern(&mut g, 10.0, &pattern, true, P240);
        assert_eq!(count_raises(&a), 0, "{a:?}");
    }

    #[test]
    fn two_hitches_in_a_window_undo_a_raise() {
        // R4: two long frames in one window are overload, even under a 2 % long share.
        let mut g = gov_with(vec![knob(1)]);
        run(&mut g, 3.10, 10.0, true, P240);
        assert_eq!(g.steps(), &[1]);
        let mut pattern = vec![10.0; 75];
        pattern.push(150.0); // one 150 ms frame every 0.9 s
        let a = run_pattern(&mut g, 10.0, &pattern, true, P240);
        assert_eq!(a, vec![Action::Set { knob: 0, step: 0 }]);
    }

    #[test]
    fn menu_frames_after_raise_are_not_hitches() {
        // R7: nothing counts as a hitch on a tick with the world inactive.
        let mut g = gov();
        run(&mut g, 1.05, 10.0, true, P240);
        assert_eq!(g.steps(), &[1, 0, 0]);
        let a = run(&mut g, 1.0, 33.0, false, P240);
        assert!(a.is_empty(), "{a:?}");
        assert_eq!(g.steps(), &[1, 0, 0]);
    }

    #[test]
    fn recurring_stalls_after_settle_undo_a_raise() {
        // R8: a second stall within STALL_WINDOW_S undoes the top of the stack.
        let mut g = gov_with(vec![knob(1)]);
        run(&mut g, 3.10, 10.0, true, P240); // post-raise period over at 3.05
        assert_eq!(g.steps(), &[1]);
        // Two stall frames on one tick are one stall.
        assert!(feed(&mut g, &[300.0, 300.0]).is_empty());
        assert!(g.last_stall.is_some());
        // A stall frame while the world is inactive is not a stall.
        assert!(tick_with(&mut g, &[300.0], false, P240, None, &[false]).is_empty());
        assert!(run(&mut g, 3.0, 10.0, true, P240).is_empty());
        assert_eq!(g.steps(), &[1]);
        // The second stall, within 10 s of the first.
        assert_eq!(
            feed(&mut g, &[300.0]),
            vec![Action::Set { knob: 0, step: 0 }]
        );
        // The undo used the record up.
        assert!(g.last_stall.is_none());
    }

    #[test]
    fn quantised_needs_99_percent_on_time() {
        // R6: 120 Hz FIFO, target 75. Late frames are at 2 × 8.33 = 16.7 ms (> 14.0 ms).
        let mut g = gov_with(vec![knob(1)]);
        let mut late_1_5 = vec![P120; 65];
        late_1_5.push(2.0 * P120); // 1 in 66 late: 1.5 %
        let mut late_3 = vec![P120; 32];
        late_3.push(2.0 * P120); // 1 in 33 late: 3 %

        // 1.5 % late: no raise.
        let a = run_pattern(&mut g, 10.0, &late_1_5, true, P120);
        assert_eq!(count_raises(&a), 0, "{a:?}");
        // All on time: a raise; then a full window of on-time frames.
        run(&mut g, 6.0, P120, true, P120);
        assert_eq!(g.steps(), &[1]);
        // 1.5 % late: no lower either.
        let a = run_pattern(&mut g, 10.0, &late_1_5, true, P120);
        assert!(a.is_empty(), "{a:?}");
        // 3 % late: a lower.
        let a = run_pattern(&mut g, 5.0, &late_3, true, P120);
        assert_eq!(a, vec![Action::Set { knob: 0, step: 0 }]);
    }

    #[test]
    fn fifo_144hz_counts_72fps_as_on_time() {
        // R6: c = target × 1.05, so 2 × 6.94 = 13.9 ms is on time for a 13.3 ms target.
        let mut g = gov();
        run(&mut g, 1.05, 2.0 * P144, true, P144);
        assert_eq!(g.steps(), &[1, 0, 0]);
    }

    #[test]
    fn high_refresh_uses_p95_not_quantised() {
        // R5 guard: at 240 Hz, 12.9 ms frames sit within 5 % of 3 × 4.17 ms by chance.
        let mut g = gov();
        let a = run(&mut g, 5.0, 12.9, true, P240);
        assert_eq!(count_raises(&a), 0, "{a:?}");
    }

    #[test]
    fn bogus_period_is_ignored() {
        // R3: a period outside 1..=50 ms, or not finite, is no display period.
        for period in [0.0, -8.0, f64::NAN, f64::INFINITY, 0.5, 60.0, 1e6] {
            let mut g = with_target(144, vec![knob(4)]);
            // 120 fps against a 144 target: overload, unless the bogus period raised the target.
            let a = run(&mut g, 3.0, P120, true, period);
            assert!(a.is_empty(), "period {period}: {a:?}");
            // A short period would not change the target above (review T11-5), so check that it
            // was not taken as a display period at all.
            assert!(
                g.last_valid_period.is_none(),
                "period {period}: {:?}",
                g.last_valid_period
            );
        }
    }

    #[test]
    fn period_change_is_a_reset_point() {
        // R3.
        let mut g = gov_with(vec![knob(1), knob(4), knob(4)]);
        // The first valid period is not a change: the raise comes at 1.05 as usual.
        run(&mut g, 0.5, 10.0, true, 0.0);
        run(&mut g, 0.55, 10.0, true, P120);
        assert_eq!(g.steps(), &[1, 0, 0]);
        // An invalid period in between is not a change: 120 Hz, 0.0, 120 Hz gives no reset, so
        // the raise comes at 2.10.
        run(&mut g, 0.05, 10.0, true, 0.0);
        run(&mut g, 1.0, 10.0, true, P120);
        assert_eq!(g.steps(), &[1, 1, 0]);
        // Nor does it clear the last valid period: 120 Hz, 0.0 at 2.55, then 60 Hz at 2.60 is a
        // change from 120 Hz, so a reset point, and there is no raise at 3.15.
        run(&mut g, 0.40, 10.0, true, P120);
        run(&mut g, 0.05, 10.0, true, 0.0);
        run(&mut g, 1.0, 10.0, true, P60);
        assert_eq!(g.steps(), &[1, 1, 0]);
        // The quiet period after the change ends at 3.60; the raise follows at 3.65.
        let a = run(&mut g, 0.1, 10.0, true, P60);
        assert_eq!(a, vec![Action::Set { knob: 1, step: 2 }]);
    }

    #[test]
    fn menu_change_while_inactive_is_not_lost() {
        // R1 (ii): menu resets run before the inactive check.
        let mut g = gov();
        run(&mut g, 1.05, 10.0, true, P240);
        assert_eq!(g.steps(), &[1, 0, 0]);
        let a = tick_with(&mut g, &[], false, P240, None, &[true, false, false]);
        assert_eq!(a, vec![Action::Set { knob: 0, step: 0 }]);
        assert_eq!(g.steps(), &[0, 0, 0]);
    }

    #[test]
    fn menu_change_clears_the_block_and_backoff() {
        // R1 (ii): a menu reset clears the knob's block and undo count, and always emits Set.
        let mut g = gov();
        let ev = run_core(&mut g, 3.0, true, P240, None, &mut responds);
        assert_eq!(ev.len(), 2, "{ev:?}"); // raise k0 at 1.05, undo at 2.05
        assert!(g.blocked_until[0] > 3.0);
        assert_eq!(g.undo_count[0], 1);
        let a = tick_with(&mut g, &[10.0; 5], true, P240, None, &[true, false, false]);
        assert_eq!(a, vec![Action::Set { knob: 0, step: 0 }]); // even at step 0
        assert_eq!(g.blocked_until[0], f64::NEG_INFINITY);
        assert_eq!(g.undo_count[0], 0);
    }

    #[test]
    fn set_enabled_false_resets_a_raised_knob() {
        let mut g = gov();
        run(&mut g, 1.05, 10.0, true, P240);
        assert_eq!(g.steps(), &[1, 0, 0]);
        assert_eq!(
            g.set_enabled(0, false),
            Some(Action::Set { knob: 0, step: 0 })
        );
        assert_eq!(g.steps(), &[0, 0, 0]);
        assert!(g.stack.is_empty());
        assert!(g.last_raise.is_none());
        // Repeated calls, calls at step 0 and out-of-range knobs do nothing.
        assert_eq!(g.set_enabled(0, false), None);
        assert_eq!(g.set_enabled(2, false), None);
        assert_eq!(g.set_enabled(2, true), None);
        assert_eq!(g.set_enabled(2, true), None);
        assert_eq!(g.set_enabled(7, false), None);
        // A disabled knob is never raised: the next raise (2.10, a quiet period after the reset
        // at 1.05) goes to k1.
        let a = run(&mut g, 1.05, 10.0, true, P240);
        assert_eq!(a, vec![Action::Set { knob: 1, step: 1 }]);
        // Enabling is not a reset and returns None.
        assert_eq!(g.set_enabled(0, true), None);
        assert_eq!(g.steps(), &[0, 1, 0]);
    }

    #[test]
    fn disabled_and_zero_max_knobs_are_never_raised() {
        // R11.
        let off = KnobSpec {
            enabled: false,
            max_step: 4,
        };
        let mut g = gov_with(vec![off, knob(0), knob(2)]);
        let a = run(&mut g, 10.0, 10.0, true, P240);
        assert_eq!(g.steps(), &[0, 0, 2], "{a:?}");
        assert_eq!(count_raises(&a), 2);
    }

    #[test]
    fn stalled_presents_pause_the_governor() {
        // R2: no present for over 250 ms is inactive.
        let mut g = gov();
        run(&mut g, 1.80, 10.0, true, P240);
        assert_eq!(g.steps(), &[1, 0, 0]);
        let mut a = Vec::new();
        for _ in 0..6 {
            a.extend(empty_tick(&mut g)); // 1.85-2.10
        }
        // 2.10 would be the next raise; it is 300 ms after the last frame.
        assert!(a.is_empty(), "{a:?}");
        assert_eq!(g.steps(), &[1, 0, 0]);
    }

    #[test]
    fn long_gap_pauses_for_two_seconds() {
        // R2: a frame over 250 ms makes the governor inactive for 2 s.
        let mut g = gov();
        run(&mut g, 0.5, 10.0, true, P240);
        // 0.55. All steps are 0, so it is not a stall.
        assert!(feed(&mut g, &[300.0]).is_empty());
        // Inactive to 2.55, active (a reset point) at 2.60, so no raise before 3.60.
        let a = run(&mut g, 3.0, 10.0, true, P240); // 0.60-3.55
        assert!(a.is_empty(), "{a:?}");
        let a = run(&mut g, 0.05, 10.0, true, P240); // 3.60
        assert_eq!(a, vec![Action::Set { knob: 0, step: 1 }]);
    }

    #[test]
    fn nan_and_negative_inputs_do_not_panic() {
        let mut g = gov();
        // Frames that are not finite or not above 0 are dropped before anything else.
        let weird = [f64::NAN, f64::NEG_INFINITY, -5.0, 0.0, -0.0];
        assert!(at(&mut g, 0.05, &weird).is_empty());
        assert!(g.window.is_empty());
        // So an infinite frame is not a gap frame either.
        assert!(at(&mut g, 0.10, &[f64::INFINITY, 10.0]).is_empty());
        assert!(g.last_gap_frame.is_none());
        for n in 3..=30 {
            at(
                &mut g,
                f64::from(n) * TICK_S,
                &[10.0, f64::NAN, 10.0, -1.0, 10.0],
            );
        }
        assert!(!g.window.is_empty());
        assert!(g.window.iter().all(|f| f.ms == 10.0));
        // A now_s that is not finite is the last tick's; a small step back is clamped.
        at(&mut g, f64::NAN, &[10.0]);
        at(&mut g, f64::INFINITY, &[10.0]);
        at(&mut g, f64::NEG_INFINITY, &[10.0]);
        assert_eq!(g.last_now, Some(1.5));
        at(&mut g, 1.0, &[10.0]);
        assert_eq!(g.last_now, Some(1.5));
        // Odd periods, VRAM figures and menu flags.
        for period in [f64::NAN, -1.0, 0.0, f64::INFINITY, 1e300] {
            g.tick(&Inputs {
                now_s: 1.55,
                new_frames_ms: &[10.0, 1e300, f64::MIN_POSITIVE],
                world_active: true,
                period_ms: period,
                vram_free_mib: Some(0),
                baseline_changed: &[false; 9],
            });
        }
        assert!(g.steps().iter().all(|&s| s <= 4));
        // A first tick whose time is not finite starts the clock at 0.
        let mut g = gov();
        at(&mut g, f64::NAN, &[10.0]);
        assert_eq!(g.last_now, Some(0.0));
    }

    #[test]
    fn a_bogus_future_time_does_not_freeze_the_clock() {
        // T5: a jump back of more than 1 s is a re-base, not a clamp.
        let mut g = gov();
        for n in 1..=10 {
            at(&mut g, f64::from(n) * TICK_S, &[10.0; 5]);
        }
        at(&mut g, 1e9, &[10.0; 5]); // one bad timestamp
        let mut raised = false;
        for n in 11..=70 {
            let before = g.steps().to_vec();
            let a = at(&mut g, f64::from(n) * TICK_S, &[10.0; 5]);
            raised |= a
                .iter()
                .any(|&Action::Set { knob, step }| step > before[knob]);
        }
        assert!(raised, "the governor still decides after the bad timestamp");
        // Old frames are evicted: about 2 s of frames, not all of them.
        assert!(g.window.len() <= 205, "{}", g.window.len());
    }

    #[test]
    fn a_far_future_tick_is_a_reset_point() {
        // T5 (review T11-2): a step forward of more than 1 s is a reset point, so one bogus
        // future tick cannot end the quiet period and the hold timers on its own.
        let mut g = gov();
        for n in 1..=10 {
            at(&mut g, f64::from(n) * TICK_S, &[10.0; 5]);
        }
        assert!(at(&mut g, 1e9, &[10.0; 5]).is_empty());
        assert_eq!(g.reset_at, 1e9);
    }

    #[test]
    fn window_holds_at_most_4096_frames() {
        // T6.
        let mut g = gov();
        at(&mut g, 0.05, &[0.5; 10_000]);
        assert_eq!(g.window.len(), 4096);
    }

    #[test]
    fn undo_skips_a_stack_entry_whose_knob_is_at_zero() {
        // R9 (defensive: the public API cannot produce such an entry).
        let mut g = gov();
        run(&mut g, 1.05, 10.0, true, P240);
        assert_eq!(g.steps(), &[1, 0, 0]);
        g.stack.push(Raise {
            knob: 1,
            id: 1000,
            t: 1.05,
            held: false,
        });
        g.stack.push(Raise {
            knob: 9,
            id: 1001,
            t: 1.05,
            held: false,
        });
        assert_eq!(g.undo(1.10), Some(Action::Set { knob: 0, step: 0 }));
        assert!(g.stack.is_empty());
        assert_eq!(g.undo(1.15), None); // an empty stack: nothing to do
    }

    #[test]
    fn an_entry_that_undo_skips_still_ends_its_post_raise_period() {
        // R7 + R9 (review T11-4): an entry leaves the stack by any path, so a skipped entry
        // that carries the most recent raise's id ends that raise's post-raise period.
        for (knob, label) in [(9, "knob out of range"), (1, "knob at step 0")] {
            let mut g = gov();
            run(&mut g, 1.05, 10.0, true, P240);
            assert_eq!(g.steps(), &[1, 0, 0]);
            g.stack.push(Raise {
                knob,
                id: 1000,
                t: 1.05,
                held: false,
            });
            g.last_raise = Some((1000, 1.05));
            assert_eq!(g.undo(1.10), Some(Action::Set { knob: 0, step: 0 }));
            assert!(g.last_raise.is_none(), "{label}: {:?}", g.last_raise);
        }
    }

    #[test]
    fn target_fps_zero_never_acts() {
        let mut g = with_target(0, vec![knob(4); 3]);
        let mut a = run(&mut g, 5.0, 5.0, true, P240);
        a.extend(tick_with(
            &mut g,
            &[5.0],
            true,
            P240,
            None,
            &[true, true, true],
        ));
        assert!(a.is_empty(), "{a:?}");
        assert_eq!(g.steps(), &[0, 0, 0]);
    }

    #[test]
    fn baseline_changed_of_wrong_length_does_not_panic() {
        let mut g = gov();
        run(&mut g, 1.05, 10.0, true, P240);
        assert_eq!(g.steps(), &[1, 0, 0]);
        assert!(tick_with(&mut g, &[10.0], true, P240, None, &[]).is_empty());
        assert!(tick_with(&mut g, &[10.0], true, P240, None, &[false]).is_empty());
        let a = tick_with(&mut g, &[10.0], true, P240, None, &[true; 10]);
        assert_eq!(
            a,
            vec![
                Action::Set { knob: 0, step: 0 },
                Action::Set { knob: 1, step: 0 },
                Action::Set { knob: 2, step: 0 },
            ]
        );
    }

    #[test]
    fn fifo_raises_when_on_time() {
        // POSITIVE CONTROL (exempt from RED): 120 Hz FIFO with target 120, every frame on time.
        // p95 mode would not raise here (8.33 ms is not under 0.9 × 8.33 ms); quantised mode does.
        let mut g = with_target(120, vec![knob(4); 3]);
        run(&mut g, 1.05, P120, true, P120);
        assert_eq!(g.steps(), &[1, 0, 0]);
    }

    #[test]
    fn empty_knobs_is_a_no_op() {
        // R11 guard.
        let mut g = gov_with(Vec::new());
        let mut a = run(&mut g, 5.0, 10.0, true, P240);
        a.extend(tick_with(&mut g, &[300.0], true, P240, None, &[true]));
        a.extend(run(&mut g, 3.0, 15.0, true, P240));
        assert!(a.is_empty());
        assert!(g.steps().is_empty());
        assert_eq!(g.set_enabled(0, false), None);
    }

    #[test]
    fn odd_configs_do_not_panic() {
        // R11 guard: the governor does not trust the parser's ranges.
        let d = Config::default().governor;
        let configs = [
            GovernorCfg {
                window_seconds: 0.0,
                raise_hold_seconds: 0.0,
                lower_hold_seconds: 0.0,
                settle_seconds: 0.0,
                block_seconds: 0.0,
                ..d
            },
            GovernorCfg {
                window_seconds: f64::NAN,
                raise_below: f64::NAN,
                lower_above: f64::NAN,
                hitch_factor: f64::NAN,
                block_seconds: f64::NAN,
                ..d
            },
            GovernorCfg {
                window_seconds: -1.0,
                raise_hold_seconds: f64::INFINITY,
                settle_seconds: -5.0,
                block_seconds: f64::INFINITY,
                hitch_factor: -1.0,
                min_vram_free_mib: u32::MAX,
                ..d
            },
            GovernorCfg {
                target_fps: u32::MAX,
                ..d
            },
            GovernorCfg { target_fps: 1, ..d },
        ];
        for cfg in configs {
            let mut g = Governor::new(cfg, vec![knob(4), knob(255), knob(0)]);
            run_core(&mut g, 30.0, true, P240, Some(4096), &mut responds);
            run(&mut g, 2.0, 400.0, true, P240);
            run(&mut g, 2.0, 10.0, true, P120);
            let max = [4, 255, 0];
            assert!(
                g.steps().iter().zip(max).all(|(&s, m)| s <= m),
                "{cfg:?}: {:?}",
                g.steps()
            );
        }
    }

    #[test]
    fn a_block_ends_at_its_slack_boundary() {
        assert!(!blocked(0.0, EPS));
        assert!(blocked(0.0, 2.0 * EPS));
    }

    #[test]
    fn stall_boundary_frames_are_kept_without_starting_a_gap() {
        for ms in [STALL_MS, STALL_MS + EPS] {
            let mut g = gov();
            g.active = true;
            g.last_now = Some(0.0);
            at(&mut g, 0.0, &[ms]);
            assert_eq!(g.last_gap_frame, None, "{ms}");
            assert_eq!(g.window, VecDeque::from([Frame { t: 0.0, ms }]));
        }
    }

    #[test]
    fn presents_are_still_active_at_the_slack_boundary() {
        for now in [PRESENTS_STOPPED_S, PRESENTS_STOPPED_S + EPS] {
            let mut g = gov();
            g.active = true;
            g.last_now = Some(0.0);
            at(&mut g, now, &[]);
            assert!(g.active, "{now}");
        }
    }

    #[test]
    fn an_active_world_starts_a_quiet_period_on_its_first_tick() {
        let mut g = gov();
        at(&mut g, 5.0, &[8.0]);
        assert!(g.active);
        assert_eq!(g.reset_at, 5.0);
    }

    #[test]
    fn undo_grace_keeps_frames_out_through_its_slack_boundary() {
        for now in [UNDO_GRACE_S, UNDO_GRACE_S + EPS] {
            let mut g = gov();
            g.active = true;
            g.last_now = Some(0.0);
            g.t_last_undo = Some(0.0);
            at(&mut g, now, &[8.0]);
            assert!(g.window.is_empty(), "{now}");
        }
    }

    #[test]
    fn clock_steps_inside_the_slack_do_not_reset_the_window() {
        for raw in [REBASE_S, REBASE_S + EPS, -REBASE_S, -REBASE_S - EPS] {
            let mut g = gov();
            g.last_now = Some(0.0);
            g.window.push_back(Frame { t: 0.0, ms: 8.0 });
            assert_eq!(g.clamp_time(raw), raw.max(0.0));
            assert_eq!(g.window.len(), 1, "{raw}");
        }
    }

    #[test]
    fn a_backward_rebase_moves_every_running_clock_by_the_same_amount() {
        let mut g = gov();
        g.raise(9.0).unwrap();
        g.last_now = Some(10.0);
        g.last_frame_tick = 9.0;
        g.blocked_until.fill(12.0);
        g.last_gap_frame = Some(7.0);
        g.last_stall = Some(6.0);
        g.t_last_undo = Some(8.0);
        assert_eq!(g.clamp_time(8.0), 8.0);
        assert_eq!(g.last_frame_tick, 7.0);
        assert_eq!(g.blocked_until, [10.0; 3]);
        assert_eq!(g.stack[0].t, 7.0);
        assert_eq!(g.last_raise, Some((0, 7.0)));
        assert_eq!(g.last_gap_frame, Some(5.0));
        assert_eq!(g.last_stall, Some(4.0));
        assert_eq!(g.t_last_raise, Some(7.0));
        assert_eq!(g.t_last_undo, Some(6.0));
        assert_eq!(g.reset_at, 8.0);
    }

    #[test]
    fn a_raise_resets_its_backoff_once_at_the_hold_boundary() {
        let mut g = gov();
        g.raise(0.0).unwrap();
        g.undo_count[0] = 3;
        g.reset_held_backoff(g.cfg.block_seconds - EPS);
        assert_eq!(g.undo_count[0], 0);
        assert!(g.stack[0].held);
        g.undo_count[0] = 2;
        g.reset_held_backoff(g.cfg.block_seconds + 1.0);
        assert_eq!(g.undo_count[0], 2);
    }

    #[test]
    fn removing_an_older_knob_keeps_the_latest_post_raise_period() {
        let mut g = gov_with(vec![knob(1), knob(1)]);
        g.raise(0.0).unwrap();
        g.raise(1.0).unwrap();
        g.remove_raises_of(0);
        assert_eq!(g.stack.len(), 1);
        assert_eq!(g.last_raise, Some((1, 1.0)));
    }

    #[test]
    fn period_changes_need_more_than_five_percent_plus_slack() {
        for (period, changed) in [(21.0, false), (21.0 + EPS, false), (22.0, true)] {
            let mut g = gov();
            g.last_valid_period = Some(20.0);
            assert_eq!(g.period_changed(Some(period)), changed, "{period}");
            assert_eq!(g.last_valid_period, Some(period));
        }
    }

    #[test]
    fn a_hitch_at_the_end_of_the_post_raise_period_still_undoes() {
        for now in [2.0, 2.0 + EPS] {
            let mut g = gov();
            g.raise(0.0).unwrap();
            assert_eq!(
                g.hitch_undo(&[100.0], now, 10.0, None),
                Some(Action::Set { knob: 0, step: 0 })
            );
        }
    }

    #[test]
    fn a_frame_at_the_hitch_limit_plus_slack_is_not_a_hitch() {
        for ms in [20.0, 20.0 + EPS] {
            let mut g = gov();
            g.raise(0.0).unwrap();
            assert_eq!(g.hitch_undo(&[ms], 1.0, 10.0, None), None, "{ms}");
            assert_eq!(g.steps(), [1, 0, 0]);
        }
    }

    #[test]
    fn hitch_limits_use_the_factor_and_allow_one_missed_vsync() {
        let mut g = gov();
        assert_eq!(g.hitch_limit(13.0, None), 26.0);
        g.mode = Mode::Quantised { p: 10.0 };
        for (factor, target, expected) in [
            (2.0, 10.0, 21.0),
            (2.0, 20.0, 40.0),
            (1.2, 30.0, 42.0),
            (1.2, 50.0, 60.0),
        ] {
            g.cfg.hitch_factor = factor;
            assert!(
                (g.hitch_limit(target, None) - expected).abs() < EPS,
                "factor {factor}, target {target}"
            );
        }
    }

    #[test]
    fn only_a_second_stall_inside_the_window_undoes() {
        for (first, next, undo) in [
            (0.0, STALL_WINDOW_S + EPS, true),
            (3.0, 12.0, true),
            (3.0, 15.0, false),
        ] {
            let mut g = gov();
            g.raise(0.0).unwrap();
            assert_eq!(g.stall(first), None);
            assert_eq!(g.stall(next).is_some(), undo, "{first} then {next}");
            assert_eq!(g.last_stall, if undo { None } else { Some(next) });
        }
    }

    #[test]
    fn an_undo_ends_the_post_raise_period_it_removes() {
        let mut g = gov();
        g.raise(0.0).unwrap();
        assert_eq!(g.undo(1.0), Some(Action::Set { knob: 0, step: 0 }));
        assert_eq!(g.last_raise, None);
    }

    #[test]
    fn eviction_keeps_frames_at_the_window_age_plus_slack() {
        for now in [2.0, 2.0 + EPS] {
            let mut g = gov();
            g.window.push_back(Frame { t: 0.0, ms: 8.0 });
            g.evict(now);
            assert_eq!(g.window.len(), 1, "{now}");
        }
    }

    #[test]
    fn one_long_frame_overloads_only_above_two_percent() {
        for (total, overload) in [(40, true), (50, false), (100, false)] {
            let mut g = gov();
            g.window
                .extend((1..total).map(|_| Frame { t: 0.0, ms: 10.0 }));
            g.window.push_back(Frame { t: 0.0, ms: 120.0 });
            assert_eq!(g.assess(20.0, None), (false, overload), "{total}");
        }
    }

    #[test]
    fn frames_at_the_long_limit_plus_slack_are_not_long() {
        for ms in [LONG_MS, LONG_MS + EPS] {
            let mut g = gov();
            g.window.extend((0..99).map(|_| Frame { t: 0.0, ms: 10.0 }));
            g.window.push_back(Frame { t: 0.0, ms });
            assert_eq!(g.assess(20.0, None), (true, false), "{ms}");
        }
    }

    #[test]
    fn quantised_mode_accepts_a_period_at_half_the_target_with_slack() {
        let mut g = gov();
        let p = 10.0 - EPS;
        g.window.push_back(Frame { t: 0.0, ms: p });
        assert_eq!(g.assess(20.0, Some(p)), (true, false));
        assert_eq!(g.mode, Mode::Quantised { p });
    }

    #[test]
    fn quantised_headroom_and_overload_count_late_frames_with_slack() {
        for (late, headroom, overload) in [(1, true, false), (2, false, false), (3, false, true)] {
            let mut g = gov();
            g.window
                .extend((0..100 - late).map(|_| Frame { t: 0.0, ms: 20.0 }));
            g.window
                .extend((0..late).map(|_| Frame { t: 0.0, ms: 21.02 }));
            assert_eq!(
                g.assess(20.0, Some(10.0)),
                (headroom, overload),
                "{late} late"
            );
        }
    }

    #[test]
    fn quantised_frames_at_the_on_time_limit_are_on_time() {
        let mut g = gov();
        g.window.extend((0..98).map(|_| Frame { t: 0.0, ms: 20.0 }));
        g.window.extend((0..2).map(|_| Frame { t: 0.0, ms: 21.0 }));
        assert_eq!(g.assess(20.0, Some(10.0)), (true, false));
    }

    #[test]
    fn p95_thresholds_are_strict_and_include_slack() {
        for (ms, expected) in [
            (9.0 - EPS, (false, false)),
            (9.0, (false, false)),
            (9.7, (false, false)),
            (9.7 + EPS, (false, false)),
            (10.0, (false, true)),
        ] {
            let mut g = gov();
            g.window.push_back(Frame { t: 0.0, ms });
            assert_eq!(g.assess(10.0, None), expected, "{ms}");
        }
    }

    #[test]
    fn a_raise_is_allowed_at_the_end_of_the_quiet_period_with_slack() {
        let mut g = gov();
        g.window.push_back(Frame { t: 0.0, ms: 1.0 });
        g.headroom_since = Some(-10.0);
        g.reset_at = 0.0;
        assert_eq!(
            g.decide(1.0 - EPS, 10.0, None, None),
            Some(Action::Set { knob: 0, step: 1 })
        );
    }

    #[test]
    fn probation_and_the_raise_interval_end_at_their_slack_boundaries() {
        let mut g = gov();
        g.t_last_undo = Some(0.0);
        g.t_last_raise = Some(1.0);
        assert!(g.raise_allowed(20.0 - EPS), "probation has ended");
        g.t_last_undo = Some(1.0);
        g.t_last_raise = Some(0.0);
        assert!(g.raise_allowed(20.0 - EPS), "the last raise is old enough");
    }
}

#[cfg(test)]
mod runtime_api_tests {
    use super::*;

    #[test]
    fn reusable_tick_matches_actions_and_complete_state() {
        let cfg = crate::config::Config::default().governor;
        let mut ordinary = Governor::new(
            cfg,
            vec![
                KnobSpec {
                    enabled: true,
                    max_step: 4
                };
                3
            ],
        );
        let mut reused = ordinary.clone();
        let mut scratch = TickScratch::new(3, 4096);
        let mut changed_steps = 0;
        for n in 0..2400 {
            let frames: &[f64] = match if n < 700 { 3 } else { n % 53 } {
                0 => &[300.0],
                1 => &[f64::NAN, -1.0, 12.0],
                2 => &[],
                _ => &[6.0, 7.0, 8.0],
            };
            let changed = [n == 280, n == 400, false];
            let i = Inputs {
                now_s: f64::from(n) * 0.05,
                new_frames_ms: frames,
                world_active: n % 99 != 0,
                period_ms: if n < 300 { 0.0 } else { 8.333 },
                vram_free_mib: Some(4096),
                baseline_changed: &changed,
            };
            let actions = ordinary.tick(&i);
            changed_steps += actions.len();
            assert_eq!(actions, reused.tick_with(&mut scratch, &i));
            assert_eq!(format!("{ordinary:?}"), format!("{reused:?}"));
        }
        assert!(
            changed_steps > 3,
            "the equivalence sequence must exercise changes"
        );
    }
}

#[cfg(test)]
mod control_tests {
    use super::*;
    #[test]
    fn forced_and_vetoed_steps_remain_coherent_for_one_hundred_ticks() {
        let mut cfg = crate::config::Config::default().governor;
        cfg.settle_seconds = 0.0;
        cfg.raise_hold_seconds = 0.0;
        let mut g = Governor::new(
            cfg,
            vec![
                KnobSpec {
                    enabled: true,
                    max_step: 4
                };
                3
            ],
        );
        g.force_step(0, Some(2));
        let mut applied = [2, 0, 0];
        for n in 0..100 {
            let actions = g.tick(&Inputs {
                now_s: f64::from(n) * 0.05,
                new_frames_ms: &[5.0],
                world_active: true,
                period_ms: 0.0,
                vram_free_mib: Some(4096),
                baseline_changed: &[false; 3],
            });
            for Action::Set { knob, step } in actions {
                assert_ne!(
                    knob, 0,
                    "a fixed knob must be excluded from automatic changes"
                );
                if step > applied[knob] {
                    assert!(g.veto_raise(knob));
                } else {
                    applied[knob] = step;
                }
            }
            assert_eq!(g.steps(), applied);
            assert_eq!(g.status().steps, applied);
        }
        assert_eq!(applied, [2, 0, 0]);
        g.force_step(0, None);
        assert!(!g.veto_raise(99));
    }
    #[test]
    fn status_tracks_inactive_active_and_undo_backoff() {
        let mut cfg = crate::config::Config::default().governor;
        cfg.settle_seconds = 0.0;
        cfg.raise_hold_seconds = 0.1;
        let mut g = Governor::new(
            cfg,
            vec![KnobSpec {
                enabled: true,
                max_step: 4,
            }],
        );
        assert_eq!(g.status().state, State::Inactive);
        for n in 0..3 {
            g.tick(&Inputs {
                now_s: f64::from(n) * 0.05,
                new_frames_ms: &[5.0],
                world_active: true,
                period_ms: 0.0,
                vram_free_mib: Some(4096),
                baseline_changed: &[false],
            });
        }
        assert_eq!(g.status().state, State::Active);
        assert!(g.steps()[0] > 0);
        g.tick(&Inputs {
            now_s: 0.15,
            new_frames_ms: &[100.0],
            world_active: true,
            period_ms: 0.0,
            vram_free_mib: Some(4096),
            baseline_changed: &[false],
        });
        assert_eq!(g.status().state, State::BackingOff);
    }

    #[test]
    fn a_later_fixed_step_supersedes_a_pending_raise_veto() {
        let mut cfg = crate::config::Config::default().governor;
        cfg.settle_seconds = 0.0;
        cfg.raise_hold_seconds = 0.0;
        let mut g = Governor::new(
            cfg,
            vec![KnobSpec {
                enabled: true,
                max_step: 4,
            }],
        );
        g.tick(&Inputs {
            now_s: 0.0,
            new_frames_ms: &[5.0],
            world_active: true,
            period_ms: 0.0,
            vram_free_mib: Some(4096),
            baseline_changed: &[false],
        });
        assert_eq!(g.steps(), &[1]);
        g.force_step(0, Some(3));
        assert!(!g.veto_raise(0));
        assert_eq!(g.steps(), &[3]);
    }

    #[test]
    fn a_fixed_knob_does_not_hide_another_knobs_hitch_undo() {
        let mut cfg = crate::config::Config::default().governor;
        cfg.settle_seconds = 0.0;
        cfg.raise_hold_seconds = 0.1;
        let mut g = Governor::new(
            cfg,
            vec![
                KnobSpec {
                    enabled: true,
                    max_step: 4
                };
                2
            ],
        );
        let input = |now_s, frames| Inputs {
            now_s,
            new_frames_ms: frames,
            world_active: true,
            period_ms: 0.0,
            vram_free_mib: Some(4096),
            baseline_changed: &[false; 2],
        };
        g.tick(&input(0.0, &[5.0]));
        g.tick(&input(0.1, &[5.0]));
        assert_eq!(g.steps(), &[1, 0]);
        g.force_step(1, Some(2));
        assert_eq!(
            g.tick(&input(0.15, &[100.0])),
            vec![Action::Set { knob: 0, step: 0 }]
        );
        assert_eq!(g.steps(), &[0, 2]);
    }

    #[test]
    fn releasing_a_forced_step_preserves_the_undo_stack() {
        let cfg = crate::config::Config::default().governor;
        let mut g = Governor::new(
            cfg,
            vec![KnobSpec {
                enabled: true,
                max_step: 4,
            }],
        );
        g.force_step(0, Some(3));
        g.force_step(0, None);
        for expected in [2, 1, 0] {
            assert_eq!(
                g.undo(1.0),
                Some(Action::Set {
                    knob: 0,
                    step: expected
                })
            );
            assert_eq!(g.steps(), &[expected]);
        }
    }

    #[test]
    fn forced_step_survives_hitches_overload_and_baseline_resets() {
        let cfg = crate::config::Config::default().governor;
        let mut g = Governor::new(
            cfg,
            vec![KnobSpec {
                enabled: true,
                max_step: 4,
            }],
        );
        g.force_step(0, Some(3));
        for n in 0..100 {
            g.tick(&Inputs {
                now_s: f64::from(n) * 0.05,
                new_frames_ms: &[300.0],
                world_active: true,
                period_ms: 0.0,
                vram_free_mib: None,
                baseline_changed: &[true],
            });
            assert_eq!(g.steps(), &[3]);
        }
        assert_eq!(g.status().state, State::Inactive);
        assert_eq!(
            g.set_enabled(0, false),
            Some(Action::Set { knob: 0, step: 0 })
        );
        assert_eq!(g.steps(), &[0]);
    }
}

#[cfg(test)]
mod band_tests {
    use super::*;

    /// Run the opt-in policy through the same public parser and tick boundary as the runtime.
    fn band_governor(maxima: &[u8]) -> Governor {
        let (cfg, _) = crate::config::parse("[Governor]\nTrialFloorFps=65\n");
        Governor::new(
            cfg.governor,
            maxima
                .iter()
                .map(|&max_step| KnobSpec {
                    enabled: true,
                    max_step,
                })
                .collect(),
        )
    }

    /// Supply steady 50 ms ticks with frame times chosen from the currently applied quality.
    fn drive(
        governor: &mut Governor,
        now: &mut f64,
        seconds: f64,
        frame_ms: impl Fn(&[u8]) -> f64,
    ) -> Vec<Action> {
        let mut actions = Vec::new();
        for _ in 0..(seconds / 0.05).round() as usize {
            *now += 0.05;
            let frames = [frame_ms(governor.steps()); 4];
            actions.extend(governor.tick(&Inputs {
                now_s: *now,
                new_frames_ms: &frames,
                world_active: true,
                period_ms: 0.0,
                vram_free_mib: Some(4096),
                baseline_changed: &[],
            }));
        }
        actions
    }

    /// CPU-limited frames above the opt-in floor may test quality without pretending to measure GPU time.
    #[test]
    fn unchanged_67fps_accepts_a_quality_trial_without_target_threshold_bounce() {
        let mut governor = band_governor(&[1]);
        let mut now = 0.0;
        let actions = drive(&mut governor, &mut now, 12.0, |_| 1000.0 / 67.0);
        assert_eq!(actions, [Action::Set { knob: 0, step: 1 }]);
        assert_eq!(governor.steps(), [1]);
    }

    /// The opt-in ordering spends terrain quality first and releases it only after other boosts.
    #[test]
    fn terrain_first_is_added_first_and_is_the_final_normal_cut() {
        let (cfg, _) = crate::config::parse("[Governor]\nTrialFloorFps=65\nTerrainFirst=1\n");
        let mut governor = Governor::new(
            cfg.governor,
            vec![
                KnobSpec {
                    enabled: true,
                    max_step: 1
                };
                3
            ],
        );
        let mut now = 0.0;
        assert_eq!(
            drive(&mut governor, &mut now, 13.0, |_| 1000.0 / 75.0),
            [
                Action::Set { knob: 2, step: 1 },
                Action::Set { knob: 0, step: 1 },
                Action::Set { knob: 1, step: 1 },
            ]
        );
        assert_eq!(
            drive(&mut governor, &mut now, 15.0, |_| 20.0),
            [
                Action::Set { knob: 1, step: 0 },
                Action::Set { knob: 0, step: 0 },
                Action::Set { knob: 2, step: 0 },
            ]
        );
        assert_eq!(governor.steps(), [0; 3]);
    }

    /// A quality cost that still stays above the floor must be rolled back to recover the target.
    #[test]
    fn a_75_to_68fps_trial_rolls_back_and_blocks_all_knobs_during_probation() {
        let mut governor = band_governor(&[1, 1]);
        let mut now = 0.0;
        let actions = drive(&mut governor, &mut now, 10.0, |steps| {
            1000.0
                / if steps.iter().any(|&step| step > 0) {
                    68.0
                } else {
                    75.0
                }
        });
        assert_eq!(
            actions,
            [
                Action::Set { knob: 0, step: 1 },
                Action::Set { knob: 0, step: 0 },
            ]
        );
        assert!(drive(&mut governor, &mut now, 16.0, |_| 1000.0 / 75.0).is_empty());
        assert_eq!(governor.steps(), [0, 0]);
        assert!(!drive(&mut governor, &mut now, 4.0, |_| 1000.0 / 75.0).is_empty());
    }

    /// The floor stops all raises and repeatedly releases existing quality without crossing step zero.
    #[test]
    fn below_65fps_backs_off_to_baseline_and_recovers_after_stable_frames_and_cooldown() {
        let mut governor = band_governor(&[1]);
        let mut now = 0.0;
        drive(&mut governor, &mut now, 10.0, |_| 1000.0 / 75.0);
        assert_eq!(governor.steps(), [1]);
        assert_eq!(
            drive(&mut governor, &mut now, 8.0, |_| 1000.0 / 60.0),
            [Action::Set { knob: 0, step: 0 }]
        );
        assert!(drive(&mut governor, &mut now, 10.0, |_| 1000.0 / 60.0).is_empty());
        assert_eq!(governor.steps(), [0]);
        assert_eq!(
            drive(&mut governor, &mut now, 10.0, |_| 1000.0 / 75.0),
            [Action::Set { knob: 0, step: 1 }]
        );
        for fps in [64.0, 65.0] {
            let mut stopped = band_governor(&[4]);
            let mut clock = 0.0;
            assert!(drive(&mut stopped, &mut clock, 20.0, |_| 1000.0 / fps).is_empty());
            assert_eq!(stopped.steps(), [0]);
        }
    }

    /// Accepted small steps share a reference, so they cannot repeatedly spend the same noise budget.
    #[test]
    fn repeated_small_quality_costs_do_not_walk_fps_down_to_the_floor() {
        let mut governor = band_governor(&[4]);
        let mut now = 0.0;
        assert_eq!(
            drive(&mut governor, &mut now, 20.0, |steps| 1000.0 / 75.0
                + 0.3 * f64::from(steps[0])),
            [
                Action::Set { knob: 0, step: 1 },
                Action::Set { knob: 0, step: 2 },
                Action::Set { knob: 0, step: 1 },
            ]
        );
        assert_eq!(governor.steps(), [1]);
    }

    /// Terrain priority does not depend on the order in which enabled knobs became available.
    #[test]
    fn terrain_readded_after_an_object_boost_is_still_the_final_normal_cut() {
        let (cfg, _) = crate::config::parse("[Governor]\nTrialFloorFps=65\nTerrainFirst=1\n");
        let mut governor = Governor::new(
            cfg.governor,
            vec![
                KnobSpec {
                    enabled: true,
                    max_step: 1,
                },
                KnobSpec {
                    enabled: false,
                    max_step: 0,
                },
                KnobSpec {
                    enabled: false,
                    max_step: 1,
                },
            ],
        );
        let mut now = 0.0;
        assert_eq!(
            drive(&mut governor, &mut now, 10.0, |_| 1000.0 / 75.0),
            [Action::Set { knob: 0, step: 1 }]
        );
        governor.set_enabled(2, true);
        assert_eq!(
            drive(&mut governor, &mut now, 10.0, |_| 1000.0 / 75.0),
            [Action::Set { knob: 2, step: 1 }]
        );
        assert_eq!(
            drive(&mut governor, &mut now, 12.0, |_| 20.0),
            [
                Action::Set { knob: 0, step: 0 },
                Action::Set { knob: 2, step: 0 },
            ]
        );
    }

    /// Terrain is a preference, so a failed terrain step is undone even while other boosts remain.
    #[test]
    fn a_failed_terrain_trial_is_rolled_back_before_an_older_object_boost() {
        let (cfg, _) = crate::config::parse("[Governor]\nTrialFloorFps=65\nTerrainFirst=1\n");
        let mut governor = Governor::new(
            cfg.governor,
            vec![
                KnobSpec {
                    enabled: true,
                    max_step: 1,
                },
                KnobSpec {
                    enabled: false,
                    max_step: 0,
                },
                KnobSpec {
                    enabled: false,
                    max_step: 1,
                },
            ],
        );
        let mut now = 0.0;
        drive(&mut governor, &mut now, 10.0, |_| 1000.0 / 75.0);
        governor.set_enabled(2, true);
        assert_eq!(
            drive(&mut governor, &mut now, 8.0, |steps| if steps[2] > 0 {
                1000.0 / 68.0
            } else {
                1000.0 / 75.0
            }),
            [
                Action::Set { knob: 2, step: 1 },
                Action::Set { knob: 2, step: 0 }
            ]
        );
        assert_eq!(governor.steps(), [1, 0, 0]);
    }

    /// A newly recovered target becomes the reference even if VRAM or maximum steps prevent raises.
    #[test]
    fn recovered_75fps_is_retained_as_reference_while_memory_blocks_new_trials() {
        let mut governor = band_governor(&[1]);
        let mut now = 0.0;
        drive(&mut governor, &mut now, 10.0, |_| 1000.0 / 67.0);
        for _ in 0..100 {
            now += 0.05;
            assert!(governor
                .tick(&Inputs {
                    now_s: now,
                    new_frames_ms: &[1000.0 / 75.0; 4],
                    world_active: true,
                    period_ms: 0.0,
                    vram_free_mib: None,
                    baseline_changed: &[],
                })
                .is_empty());
        }
        let mut actions = Vec::new();
        for _ in 0..100 {
            now += 0.05;
            actions.extend(governor.tick(&Inputs {
                now_s: now,
                new_frames_ms: &[1000.0 / 68.0; 4],
                world_active: true,
                period_ms: 0.0,
                vram_free_mib: None,
                baseline_changed: &[],
            }));
        }
        assert_eq!(actions, [Action::Set { knob: 0, step: 0 }]);
    }

    /// A slower display may limit the target, but cannot silently weaken the configured safety floor.
    #[test]
    fn a_60hz_display_does_not_allow_a_trial_below_the_65fps_floor() {
        let mut governor = band_governor(&[1]);
        let mut actions = Vec::new();
        for n in 0..400 {
            actions.extend(governor.tick(&Inputs {
                now_s: f64::from(n) * 0.05,
                new_frames_ms: &[1000.0 / 64.0; 4],
                world_active: true,
                period_ms: 1000.0 / 60.0,
                vram_free_mib: Some(4096),
                baseline_changed: &[],
            }));
        }
        assert!(actions.is_empty(), "{actions:?}");
    }

    /// Cap-limited 75 FPS trials remain at most one unaccepted step apart, including quantised vsync.
    #[test]
    fn capped_75fps_accepts_steps_only_after_fresh_window_and_hold() {
        let mut governor = band_governor(&[4]);
        let mut now = 0.0;
        assert_eq!(
            drive(&mut governor, &mut now, 4.1, |_| 1000.0 / 75.0),
            [Action::Set { knob: 0, step: 1 }]
        );
        assert!(drive(&mut governor, &mut now, 3.9, |_| 1000.0 / 75.0).is_empty());
        assert_eq!(governor.steps(), [1]);
        assert_eq!(
            drive(&mut governor, &mut now, 0.2, |_| 1000.0 / 75.0),
            [Action::Set { knob: 0, step: 2 }]
        );
        let mut quantised = band_governor(&[4]);
        let mut actions = Vec::new();
        for n in 0..410 {
            actions.extend(quantised.tick(&Inputs {
                now_s: f64::from(n) * 0.05,
                new_frames_ms: &[1000.0 / 75.0; 4],
                world_active: true,
                period_ms: 1000.0 / 75.0,
                vram_free_mib: Some(4096),
                baseline_changed: &[],
            }));
        }
        assert_eq!(
            actions,
            (1..=4)
                .map(|step| Action::Set { knob: 0, step })
                .collect::<Vec<_>>()
        );
        assert_eq!(quantised.steps(), [4]);
    }

    /// Quality may spend performance that is still faster than the target without spending the FPS band.
    #[test]
    fn quality_cost_above_the_target_is_allowed() {
        let mut governor = band_governor(&[1]);
        let mut now = 0.0;
        assert_eq!(
            drive(&mut governor, &mut now, 20.0, |steps| if steps[0] == 0 {
                6.0
            } else {
                12.0
            }),
            [Action::Set { knob: 0, step: 1 }]
        );
    }

    /// Settling frames do not poison a trial; small persistent jitter does not cause a raise/undo loop.
    #[test]
    fn settling_and_ordinary_noise_do_not_reject_a_stable_trial() {
        let mut governor = band_governor(&[1]);
        let mut now = 0.0;
        drive(&mut governor, &mut now, 4.1, |_| 1000.0 / 67.0);
        assert!(drive(&mut governor, &mut now, 0.7, |_| 25.0).is_empty());
        let tick = std::cell::Cell::new(0usize);
        let actions = drive(&mut governor, &mut now, 45.0, |_| {
            tick.set(tick.get() + 1);
            1000.0 / 67.0
                + if tick.get().is_multiple_of(2) {
                    0.2
                } else {
                    -0.2
                }
        });
        assert!(actions.is_empty(), "{actions:?}");
        assert_eq!(governor.steps(), [1]);
    }

    /// Hitch safety is immediate even late in an unaccepted trial and despite terrain preference.
    #[test]
    fn a_hitch_undoes_pending_terrain_without_waiting_for_the_normal_hold() {
        let (cfg, _) = crate::config::parse("[Governor]\nTrialFloorFps=65\nTerrainFirst=1\n");
        let mut governor = Governor::new(
            cfg.governor,
            vec![
                KnobSpec {
                    enabled: true,
                    max_step: 1
                };
                3
            ],
        );
        let mut now = 0.0;
        drive(&mut governor, &mut now, 6.5, |_| 1000.0 / 75.0);
        now += 0.05;
        assert_eq!(
            governor.tick(&Inputs {
                now_s: now,
                new_frames_ms: &[35.0],
                world_active: true,
                period_ms: 0.0,
                vram_free_mib: Some(4096),
                baseline_changed: &[],
            }),
            [Action::Set { knob: 2, step: 0 }]
        );
        assert_eq!(governor.steps(), [0; 3]);
    }

    /// World, live-device memory, availability and maxima remain hard vetoes above the trial floor.
    #[test]
    fn world_memory_and_capacity_vetoes_prevent_trials() {
        for (world_active, vram_free_mib) in [(false, Some(4096)), (true, None), (true, Some(1023))]
        {
            let mut governor = band_governor(&[4, 4, 4]);
            for n in 0..400 {
                assert!(governor
                    .tick(&Inputs {
                        now_s: f64::from(n) * 0.05,
                        new_frames_ms: &[1000.0 / 75.0; 4],
                        world_active,
                        period_ms: 0.0,
                        vram_free_mib,
                        baseline_changed: &[],
                    })
                    .is_empty());
            }
            assert_eq!(governor.steps(), [0; 3]);
        }
        let mut governor = band_governor(&[0, 0, 0]);
        let mut now = 0.0;
        assert!(drive(&mut governor, &mut now, 20.0, |_| 5.0).is_empty());
        assert_eq!(governor.steps(), [0; 3]);
    }

    /// A capacity veto must discard the pending trial and allow a later valid proposal.
    #[test]
    fn vetoed_and_menu_reset_trials_leave_no_orphaned_probation() {
        let mut governor = band_governor(&[1]);
        let mut now = 0.0;
        drive(&mut governor, &mut now, 4.05, |_| 1000.0 / 75.0);
        assert!(governor.veto_raise(0));
        assert_eq!(governor.steps(), [0]);
        assert_eq!(
            drive(&mut governor, &mut now, 4.1, |_| 1000.0 / 67.0),
            [Action::Set { knob: 0, step: 1 }]
        );
        now += 0.05;
        assert_eq!(
            governor.tick(&Inputs {
                now_s: now,
                new_frames_ms: &[1000.0 / 67.0; 4],
                world_active: true,
                period_ms: 0.0,
                vram_free_mib: Some(4096),
                baseline_changed: &[true],
            }),
            [Action::Set { knob: 0, step: 0 }]
        );
        assert_eq!(
            drive(&mut governor, &mut now, 5.0, |_| 1000.0 / 67.0),
            [Action::Set { knob: 0, step: 1 }]
        );
    }

    /// With both opt-ins off, the original p95 thresholds and object/grass/terrain stack still apply.
    #[test]
    fn disabled_trial_policy_preserves_legacy_thresholds_and_action_order() {
        let (cfg, warnings) = crate::config::parse("[Governor]\nTrialFloorFps=0\nTerrainFirst=0\n");
        assert!(warnings.is_empty());
        assert_eq!(cfg, crate::config::Config::default());
        let mut governor = Governor::new(
            cfg.governor,
            vec![
                KnobSpec {
                    enabled: true,
                    max_step: 1
                };
                3
            ],
        );
        let mut now = 0.0;
        assert!(drive(&mut governor, &mut now, 10.0, |_| 1000.0 / 67.0).is_empty());
        assert!(drive(&mut governor, &mut now, 10.0, |_| 1000.0 / 75.0).is_empty());
        assert_eq!(
            drive(&mut governor, &mut now, 10.0, |_| 6.0),
            [
                Action::Set { knob: 0, step: 1 },
                Action::Set { knob: 1, step: 1 },
                Action::Set { knob: 2, step: 1 },
            ]
        );
        assert_eq!(
            drive(&mut governor, &mut now, 10.0, |_| 20.0),
            [
                Action::Set { knob: 2, step: 0 },
                Action::Set { knob: 1, step: 0 },
                Action::Set { knob: 0, step: 0 },
            ]
        );
        assert_eq!(governor.steps(), [0; 3]);
    }

    /// Accepted quality reaching the stop boundary backs off even inside the frame-noise allowance.
    #[test]
    fn an_accepted_boost_at_exactly_the_floor_is_undone_and_blocks_new_trials() {
        let mut governor = band_governor(&[1]);
        let mut now = 0.0;
        assert_eq!(
            drive(&mut governor, &mut now, 12.0, |_| 1000.0 / 67.0),
            [Action::Set { knob: 0, step: 1 }]
        );
        assert!(
            governor.band_trial.is_none(),
            "the boost must already be accepted"
        );
        // 67 -> 65 FPS costs less than 0.5 ms, so only the floor boundary can trigger this undo.
        assert!(1000.0 / 65.0 < governor.band_reference_ms.unwrap() + TRIAL_NOISE_MS);
        assert_eq!(
            drive(&mut governor, &mut now, 8.0, |_| 1000.0 / 65.0),
            [Action::Set { knob: 0, step: 0 }]
        );
        assert_eq!(governor.steps(), [0]);
        assert!(drive(&mut governor, &mut now, 10.0, |_| 1000.0 / 67.0).is_empty());
        assert_eq!(
            governor.steps(),
            [0],
            "floor backoff must preserve probation"
        );
    }

    /// Reaching the stop boundary cannot leave a trial permanently pending without enough FPS to accept.
    #[test]
    fn a_trial_that_reaches_exactly_the_floor_is_rolled_back() {
        let mut governor = band_governor(&[2]);
        let mut now = 0.0;
        assert_eq!(
            drive(&mut governor, &mut now, 12.0, |steps| {
                1000.0 / if steps[0] == 0 { 67.0 } else { 65.0 }
            }),
            [
                Action::Set { knob: 0, step: 1 },
                Action::Set { knob: 0, step: 0 }
            ]
        );
        assert_eq!(governor.steps(), [0]);
    }

    /// An unaccepted trial cannot reset failure history merely by outlasting a short block setting.
    #[test]
    fn long_pending_trials_keep_exponential_failure_backoff() {
        let (mut cfg, warnings) = crate::config::parse(
            "[Governor]\nTrialFloorFps=65\nWindowSeconds=0.5\nSettleSeconds=0.1\nRaiseHoldSeconds=2\nLowerHoldSeconds=0.1\nBlockSeconds=1\n",
        );
        assert!(warnings.is_empty());
        cfg.knobs.max_object_distance = 1;
        let mut governor = Governor::new(
            cfg.governor,
            vec![KnobSpec {
                enabled: true,
                max_step: 1,
            }],
        );
        let mut raised_at = None;
        let mut raises = Vec::new();
        let mut undos = Vec::new();
        for n in 0..1_200 {
            let now = f64::from(n) * 0.05;
            let raised = governor.steps() == [1];
            let fps = if raised && raised_at.is_some_and(|t| now - t > 1.2) {
                68.0
            } else {
                75.0
            };
            for action in governor.tick(&Inputs {
                now_s: now,
                new_frames_ms: &[1000.0 / fps; 4],
                world_active: true,
                period_ms: 0.0,
                vram_free_mib: Some(4096),
                baseline_changed: &[],
            }) {
                match action {
                    Action::Set { step: 1, .. } => {
                        raises.push(now);
                        raised_at = Some(now);
                    }
                    Action::Set { step: 0, .. } => {
                        undos.push(now);
                        raised_at = None;
                    }
                    _ => unreachable!(),
                }
            }
        }
        assert!(raises.len() >= 4 && undos.len() >= 3);
        assert!(
            raises[3] - undos[2] >= 4.0 - 1e-9,
            "raises={raises:?}, undos={undos:?}"
        );
    }
}
