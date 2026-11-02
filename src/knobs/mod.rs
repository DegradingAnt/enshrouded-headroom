//! Draw-distance knobs: the state that the game-thread hooks and the governor thread share.
//!
//! Each knob is a [`KnobShared`]. Two threads use it:
//! - the game thread, in a hook: it reads the knob's value ([`KnobShared::value`]) and publishes
//!   the game's own value for the player's menu setting, the baseline ([`KnobShared::publish`]);
//! - the governor thread (Task 15): it sets the value ([`KnobShared::set`]) and reads the
//!   baseline, its change counter and the time the hook last ran.
//!
//! Everything is an atomic, so a hook never blocks, allocates or takes a lock. An `f32` is kept as
//! its bits (`to_bits` / `from_bits`) in an `AtomicU32`.
//!
//! This module also holds the MinHook helpers: `create_and_queue` creates a hook and queues it,
//! and [`enable_queued_hooks`] enables every queued hook at once. `entry::start` calls that once,
//! as its last step, so every code hook goes live in one batch.
//!
//! # Loom models
//!
//! Run `RUSTFLAGS="--cfg loom" cargo test --release --lib knobs::loom`.
//! Local model knobs instrument every field and execute these same methods and orderings.
//! Existing const globals stay standard-backed in the model build; normal builds use only the
//! exact standard atomic types. The models cover two publishes and one concurrent reader, not
//! sequence-counter wraparound or the native hooks. Loom has its documented relaxed-order limits:
//! <https://docs.rs/loom/0.7.2/loom/>.

pub mod object_distance;
pub mod scatter_distance;
mod sync;
pub mod terrain_distance;

use std::sync::atomic::AtomicU32;
use sync::{AtomicU32 as SharedAtomicU32, AtomicU64 as SharedAtomicU64, Ordering};

#[cfg(test)]
use log::Level;
#[cfg(test)]
use minhook::MH_STATUS;

/// One knob's shared state.
///
/// # Memory orderings
///
/// - The value: `Relaxed` on both sides. It is one self-contained number; nothing else is
///   published with it. A hook can see the old value for one more call.
/// - [`publish`](KnobShared::publish) (game thread) stores the baseline, then bumps the change
///   counter with `Release` when the baseline changed, then stores the time with `Release`.
///   [`baseline`](KnobShared::baseline) loads the counter with `Acquire` before the baseline, so
///   a reader that sees change `n` reads the baseline of change `n` or a newer one; the newer
///   one's change shows on its next read, so no change is missed.
/// - [`seen`](KnobShared::seen) (game thread) stores only the time, with `Release`. The hook's
///   `Relaxed` stores before it (for example `OBJECT_UNKNOWN_BASELINE` and `SCATTER_LEVEL`) are
///   therefore ordered before that time. It leaves the baseline and the change counter alone.
/// - [`last_seen`](KnobShared::last_seen) loads the time with `Acquire`, so a reader that sees the
///   time of a publish or a seen also sees what that call (and its caller, before it) stored.
pub struct KnobShared {
    /// The value the hook applies (a factor, or metres for the scatter knob), as `f32` bits.
    value_bits: SharedAtomicU32,
    /// The game's own value from the last publish, as `f32` bits; 0 (0.0) before the first.
    baseline_bits: SharedAtomicU32,
    /// How many times the baseline has changed (wraps). 0 before the first change.
    baseline_seq: SharedAtomicU32,
    /// `QueryPerformanceCounter` time passed to the last call of publish or seen; 0 before the
    /// first such call.
    last_seen_qpc: SharedAtomicU64,
}

impl KnobShared {
    /// A knob with value `v` and no baseline yet: `baseline()` is `(0.0, 0)`, `last_seen()` is 0.
    pub const fn new(v: f32) -> Self {
        KnobShared {
            value_bits: SharedAtomicU32::new(v.to_bits()),
            baseline_bits: SharedAtomicU32::new(0),
            baseline_seq: SharedAtomicU32::new(0),
            last_seen_qpc: SharedAtomicU64::new(0),
        }
    }

    /// The value the hook applies. Game thread.
    pub fn value(&self) -> f32 {
        f32::from_bits(self.value_bits.load(Ordering::Relaxed))
    }

    /// Set the value the hook applies. Governor thread.
    pub fn set(&self, v: f32) {
        self.value_bits.store(v.to_bits(), Ordering::Relaxed);
    }

    /// Publish the game's own value and the time. Game thread, from a hook.
    ///
    /// The change counter moves only when the baseline's bits differ from the last publish. It
    /// wraps, so compare it with `!=`.
    pub fn publish(&self, baseline: f32, now_qpc: u64) {
        let bits = baseline.to_bits();
        if self.baseline_bits.swap(bits, Ordering::Relaxed) != bits {
            self.baseline_seq.fetch_add(1, Ordering::Release);
        }
        self.last_seen_qpc.store(now_qpc, Ordering::Release);
    }

    /// Record the time only, without a baseline: the hook ran, but what it read was not a value
    /// the game writes (see `object_distance`). Game thread, from a hook. `Release`, like
    /// [`publish`](KnobShared::publish).
    pub fn seen(&self, now_qpc: u64) {
        self.last_seen_qpc.store(now_qpc, Ordering::Release);
    }

    /// The last published baseline and the change counter. Governor thread.
    pub fn baseline(&self) -> (f32, u32) {
        let seq = self.baseline_seq.load(Ordering::Acquire);
        let bits = self.baseline_bits.load(Ordering::Relaxed);
        (f32::from_bits(bits), seq)
    }

    /// The `QueryPerformanceCounter` time of the last publish or seen, or 0 before the first such
    /// call. Governor thread.
    pub fn last_seen(&self) -> u64 {
        self.last_seen_qpc.load(Ordering::Acquire)
    }
}

/// Object draw distance (design.md 7.1): a factor on the game's pixel-size multipliers. Its
/// baseline is the game's `scene+0x33c`, and its `last_seen` is the governor's world-rendering
/// signal.
pub static OBJECT: KnobShared = KnobShared::new(1.0);

/// Grass and detail-scatter distance (design.md 7.2), in metres. A mirror for the log:
/// `scatter_distance::set_distance` sets it together with the float the game reads.
pub static SCATTER: KnobShared = KnobShared::new(90.0);

/// Terrain detail distance (design.md 7.3): a factor on the terrain LOD base distance. Task 14's
/// hook applies it.
pub static TERRAIN: KnobShared = KnobShared::new(1.0);

/// The game's detail-scattering level (`scene+0x290`, 0..3; 3 is High), published by the
/// scene-settings hook (`Relaxed`; see [`KnobShared`] for how a reader orders it with
/// `OBJECT.last_seen()`). 0 before the first publish, which keeps the scatter knob off.
pub static SCATTER_LEVEL: AtomicU32 = AtomicU32::new(0);

/// How many calls of the scene-settings hook found a value at `scene+0x33c` that the game never
/// writes, so they published no baseline and scaled nothing (`object_distance`, rule in
/// `post_work`). Task 15 reports it. `Relaxed`, and it wraps; it is stored before the call's
/// `OBJECT.seen()`, so a reader that sees that time also sees the count.
pub static OBJECT_UNKNOWN_BASELINE: AtomicU32 = AtomicU32::new(0);

/// Object factor for each step. Step 0 is the player's menu value.
pub const OBJECT_STEPS: [f32; 5] = [1.0, 0.85, 0.72, 0.61, 0.52];

/// Scatter distance in metres for each step. Step 0 is the game's High distance.
pub const SCATTER_STEPS: [f32; 5] = [90.0, 100.0, 110.0, 120.0, 127.0];

/// Terrain factor for each step. Step 0 is the player's menu value.
pub const TERRAIN_STEPS: [f32; 5] = [1.0, 1.25, 1.5, 1.75, 2.0];

// ---- MinHook --------------------------------------------------------------------------------

use emberkit::hooks::hooks_line;
#[cfg(test)]
use emberkit::hooks::not_all_enabled_text;
pub use emberkit::hooks::{apply_queued, create_and_queue};

/// The last start-up step: enable every hook queued before it, and log one line if any was.
/// Called once, from the end of `entry::start`.
pub fn enable_queued_hooks() {
    let result = apply_queued();
    object_distance::complete_hook_batch(&result);
    if let Some((level, line)) = hooks_line(&result) {
        log::log!(level, "{line}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::Ordering;

    #[test]
    fn publish_bumps_seq_only_on_change() {
        let k = KnobShared::new(1.0);
        k.publish(1.0, 1);
        let (_, a) = k.baseline();
        k.publish(1.0, 2);
        assert_eq!(k.baseline().1, a);
        k.publish(2.0, 3);
        assert_eq!(k.baseline().1, a + 1);
    }

    #[test]
    fn a_new_knob_has_its_value_and_no_baseline_yet() {
        let k = KnobShared::new(0.85);
        assert_eq!(k.value(), 0.85);
        assert_eq!(k.baseline(), (0.0, 0));
        assert_eq!(k.last_seen(), 0);
    }

    #[test]
    fn set_replaces_the_value_bit_for_bit_and_leaves_the_baseline() {
        let k = KnobShared::new(1.0);
        for v in [0.52f32, -0.0, 127.0, f32::MIN_POSITIVE] {
            k.set(v);
            assert_eq!(k.value().to_bits(), v.to_bits(), "{v}");
        }
        assert_eq!(k.baseline(), (0.0, 0));
        assert_eq!(k.last_seen(), 0);
    }

    #[test]
    fn publish_stores_the_baseline_and_the_time_but_not_the_value() {
        let k = KnobShared::new(1.0);
        k.publish(2.0, 100);
        assert_eq!(k.baseline(), (2.0, 1));
        assert_eq!(k.last_seen(), 100);
        // The same baseline again: the time moves, the counter does not.
        k.publish(2.0, 250);
        assert_eq!(k.baseline(), (2.0, 1));
        assert_eq!(k.last_seen(), 250);
        // The baseline is the game's value; the value the hook applies is the governor's.
        assert_eq!(k.value(), 1.0);
    }

    #[test]
    fn the_change_counter_wraps_instead_of_panicking() {
        let k = KnobShared::new(1.0);
        k.baseline_seq.store(u32::MAX, Ordering::Relaxed);
        k.publish(3.0, 1);
        assert_eq!(k.baseline(), (3.0, 0));
    }

    #[test]
    fn steps_are_the_global_constraint_values() {
        assert_eq!(OBJECT_STEPS, [1.0, 0.85, 0.72, 0.61, 0.52]);
        assert_eq!(SCATTER_STEPS, [90.0, 100.0, 110.0, 120.0, 127.0]);
        assert_eq!(TERRAIN_STEPS, [1.0, 1.25, 1.5, 1.75, 2.0]);
    }

    #[test]
    fn nothing_queued_means_nothing_to_enable_and_no_log_line() {
        // No test in this process queues a hook (the hooking tests run in child processes).
        assert_eq!(apply_queued(), Ok(0));
        assert_eq!(hooks_line(&Ok(0)), None);
    }

    #[test]
    fn the_hooks_line_says_how_many_were_enabled_or_why_not() {
        assert_eq!(
            hooks_line(&Ok(2)),
            Some((Level::Info, "hooks: 2 enabled".to_owned()))
        );
        assert_eq!(
            hooks_line(&Err("MinHook says no".to_owned())),
            Some((Level::Warn, "hooks: MinHook says no".to_owned()))
        );
    }

    #[test]
    fn a_target_minhook_refuses_is_an_error_and_nothing_is_queued() {
        // Address 0 is not executable: MinHook refuses it before it touches any code.
        // SAFETY: MinHook checks the target and writes nothing for an address it refuses.
        let why = unsafe { create_and_queue(0, 0) }.unwrap_err();
        assert!(why.contains("MH_ERROR_NOT_EXECUTABLE"), "{why}");
        assert_eq!(apply_queued(), Ok(0));
    }

    #[test]
    fn a_failed_apply_says_that_hooks_before_the_failing_one_are_live() {
        // MH_ApplyQueued enables in order and stops at the first failure (hook.c).
        assert_eq!(
            not_all_enabled_text(2, MH_STATUS::MH_ERROR_MEMORY_PROTECT),
            "MinHook could not enable all 2 queued hook(s) (MH_ERROR_MEMORY_PROTECT); hooks \
             queued before the failing one are live"
        );
    }

    #[test]
    fn seen_records_the_time_only() {
        let k = KnobShared::new(1.0);
        k.publish(2.0, 5);
        k.seen(9);
        assert_eq!(k.last_seen(), 9);
        assert_eq!(k.baseline(), (2.0, 1), "no baseline, no change counted");
        assert_eq!(k.value(), 1.0);
    }
}

/// Model only local knobs; runtime globals retain const standard-atomic construction.
#[cfg(all(test, loom))]
mod loom {
    use super::{sync, KnobShared};
    use ::loom::sync::Arc;
    use ::loom::thread;

    /// Every field of this local knob is instrumented; no runtime-global constructor is modeled.
    fn model_knob(value: f32) -> KnobShared {
        KnobShared {
            value_bits: sync::AtomicU32::modeled(value.to_bits()),
            baseline_bits: sync::AtomicU32::modeled(0),
            baseline_seq: sync::AtomicU32::modeled(0),
            last_seen_qpc: sync::AtomicU64::modeled(0),
        }
    }
    /// Check every two-thread execution without permutation, duration or preemption sampling.
    fn check(model: impl Fn() + Send + Sync + 'static) {
        let mut builder = ::loom::model::Builder::new();
        builder.max_threads = 2;
        builder.max_permutations = None;
        builder.max_duration = None;
        builder.preemption_bound = None;
        builder.check(model);
    }

    #[test]
    fn counter_never_publishes_an_older_baseline() {
        check(|| {
            let knob = Arc::new(model_knob(1.0));
            let writer_knob = Arc::clone(&knob);
            let writer = thread::spawn(move || {
                writer_knob.publish(1.0, 10);
                writer_knob.publish(2.0, 20);
            });
            let (baseline, counter) = knob.baseline();
            assert!(counter <= 2);
            assert!(
                baseline >= counter as f32,
                "counter {counter} exposed older baseline {baseline}"
            );
            writer.join().unwrap();
            assert_eq!(knob.baseline(), (2.0, 2));
        });
    }
    #[test]
    fn publishing_identical_bits_twice_moves_the_counter_only_once() {
        check(|| {
            let knob = Arc::new(model_knob(1.0));
            let writer_knob = Arc::clone(&knob);
            let writer = thread::spawn(move || {
                // Negative zero differs from the initial baseline bits, then repeats exactly.
                writer_knob.publish(-0.0, 10);
                let first_counter = writer_knob.baseline().1;
                writer_knob.publish(-0.0, 20);
                assert_eq!(writer_knob.baseline().1, first_counter);
            });
            let (baseline, counter) = knob.baseline();
            assert!(counter <= 1, "identical bits counted as {counter} changes");
            if counter == 1 {
                assert_eq!(baseline.to_bits(), (-0.0f32).to_bits());
            }
            writer.join().unwrap();
            assert_eq!(knob.baseline().0.to_bits(), (-0.0f32).to_bits());
            assert_eq!(knob.baseline().1, 1);
            assert_eq!(knob.last_seen(), 20);
        });
    }

    #[test]
    fn seeing_a_publish_time_orders_its_counter_or_a_later_one() {
        check(|| {
            let knob = Arc::new(model_knob(1.0));
            let writer_knob = Arc::clone(&knob);
            let writer = thread::spawn(move || {
                writer_knob.publish(1.0, 10);
                writer_knob.publish(2.0, 20);
            });
            let seen = knob.last_seen();
            let (_, counter) = knob.baseline();
            let minimum = match seen {
                0 => 0,
                10 => 1,
                20 => 2,
                other => panic!("unexpected time {other}"),
            };
            assert!(
                counter >= minimum,
                "time {seen} exposed earlier counter {counter}"
            );
            // A newer counter with an older time is allowed: there is no reverse promise.
            writer.join().unwrap();
            assert_eq!(knob.last_seen(), 20);
            assert_eq!(knob.baseline(), (2.0, 2));
        });
    }
}
