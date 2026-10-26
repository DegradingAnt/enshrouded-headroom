//! EXPERIMENTAL: the climb diagnostic (branch `exp/climb-anything`; never merged; one play
//! session).
//!
//! `[Experimental] ClimbDiag = 1` queues eight post-hooks on the game's own climb functions, so
//! that one session shows which step of a climb fails. Each hook runs the game function exactly
//! once with the game's arguments, then only bumps atomic counters and stores a last value. It
//! allocates nothing, takes no lock and logs nothing on the game thread, and it changes no
//! argument and no result. A `climb-diag` thread wakes once a second and logs one `climb diag:`
//! line when a count that moves only with climb activity has changed.
//!
//! The hooked functions (docs/re-notes.md, "Climb diagnostic"):
//! - the wall probe `FUN_14024abc0`: its result code (byte +0 of its result: 0 ok, 0x15 plane,
//!   0x1e clearance, 0x1f miss) and, on success, the hit EntityId (+0x20; 0 is a voxel surface);
//! - the start check `FUN_140419570`, which runs every frame while the player does not climb;
//! - the top entry `FUN_14024c320` and the bottom/middle entry `FUN_14024c0d0`: their result
//!   codes. The probe that ran inside the bottom entry splits its code 6 into "angle" (the probe
//!   passed) and "no input" (no probe ran);
//! - the wall-lost check `FUN_14024b390`: 0 drops the climb, and 0xffffffff is the voxel-hold
//!   value. The probe that ran inside it says why a climb was dropped;
//! - stop climbing `FUN_14024a090`, the exit-top check `FUN_14024c9f0` and the bottom exit check
//!   `FUN_14024c8f0`.
//!
//! Every signature in [`sigs::CLIMB_DIAG`] must match exactly once, or nothing is hooked. The
//! hooks are only queued here; `knobs::enable_queued_hooks` enables them with the others.

use std::cell::Cell;
use std::sync::atomic::{AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::time::Duration;

use log::Level;

use crate::config::ExperimentalCfg;
use crate::entry;
use crate::guard::{self, guard, Feature};
use crate::knobs::create_and_queue;
use crate::scan::{find_in_sections, main_module, Module, Pattern};
use crate::sigs::{
    self, Sig, CLIMB_DIAG_EXIT_LOW_FN, CLIMB_DIAG_EXIT_TOP_FN, CLIMB_DIAG_LOW_ENTRY_FN,
    CLIMB_DIAG_PROBE_FN, CLIMB_DIAG_START_FN, CLIMB_DIAG_STOP_FN, CLIMB_DIAG_TOP_ENTRY_FN,
    CLIMB_DIAG_WALL_LOST_FN,
};

/// The result code for success: the probe's, the entries' and the bottom exit's.
const OK: u8 = 0;
/// The probe's code: a corner ray's normal is off the forward ray's plane (the tolerance).
const PLANE: u8 = 0x15;
/// The probe's code: the clearance ray hit something. The bottom entry's: the re-grab cooldown.
const CLEAR: u8 = 0x1e;
/// The probe's code: the forward ray or a corner ray hit nothing. The bottom entry's: the probe
/// failed (with any probe code).
const MISS: u8 = 0x1f;
/// The bottom entry's code for "no move input, the input is more than 45 degrees off the wall,
/// or the building spark is out".
const NOT_NOW: u8 = 6;
/// What the voxel-hold patch makes the wall-lost check return for a voxel wall
/// (`features::climb::WALL_LOST_VOXEL_HOLD`).
const HELD: u32 = 0xffff_ffff;
/// The code for a result the hook could not read (a null pointer). No game path writes it, so it
/// counts as "other".
const UNREAD: u8 = 0xff;
/// The last probe code before any probe ran (shown as "none").
const NO_CODE: u32 = 0x100;
/// The thread-local mark for "no probe ran"; above every `u8` code.
const NO_PROBE: u16 = 0x100;
/// Where the probe's result holds the hit EntityId.
const ENTITY_AT: usize = 0x20;
/// How often the log thread looks at the counts.
const LOG_INTERVAL: Duration = Duration::from_secs(1);

// ---- the counts -------------------------------------------------------------------------------

/// Every count the hooks keep. The one instance is [`DIAG`]. Every load and store is `Relaxed`:
/// each count stands alone, and a log line may mix counts from two moments a few nanoseconds
/// apart.
struct Counters {
    /// Wall-probe calls that passed.
    probe_ok: AtomicU64,
    /// Wall-probe calls that failed with 0x15 (plane).
    probe_plane: AtomicU64,
    /// Wall-probe calls that failed with 0x1e (clearance).
    probe_clear: AtomicU64,
    /// Wall-probe calls that failed with 0x1f (miss).
    probe_miss: AtomicU64,
    /// Wall-probe calls with any other code.
    probe_other: AtomicU64,
    /// The code of the last wall-probe call, or [`NO_CODE`].
    probe_last_code: AtomicU32,
    /// The hit EntityId of the last wall-probe call that passed.
    probe_last_entity: AtomicU32,
    /// Start-check calls (one a frame while the player does not climb).
    start_calls: AtomicU64,
    /// Top-entry calls.
    top_calls: AtomicU64,
    /// Top-entry calls that passed.
    top_ok: AtomicU64,
    /// Bottom-entry calls.
    low_calls: AtomicU64,
    /// Bottom-entry calls that passed.
    low_ok: AtomicU64,
    /// Bottom-entry code 6 with no probe: no move input (or the building spark is out).
    low_no_input: AtomicU64,
    /// Bottom-entry code 6 after a probe that passed: the input is more than 45 degrees off.
    low_angle: AtomicU64,
    /// Bottom-entry code 0x1e: the re-grab cooldown after a drop.
    low_cooldown: AtomicU64,
    /// Bottom-entry code 0x1f: the probe failed.
    low_probe: AtomicU64,
    /// Bottom-entry calls with any other code, or a code 6 after a failed probe.
    low_other: AtomicU64,
    /// Wall-lost calls (one a frame while the player climbs).
    lost_calls: AtomicU64,
    /// Wall-lost calls that returned the voxel-hold value.
    lost_held: AtomicU64,
    /// Drops (wall-lost returned 0) after a probe that passed on a voxel (EntityId 0).
    lost_voxel: AtomicU64,
    /// Drops after a probe that failed with 0x15.
    lost_plane: AtomicU64,
    /// Drops after a probe that failed with 0x1e.
    lost_clear: AtomicU64,
    /// Drops after a probe that failed with 0x1f.
    lost_miss: AtomicU64,
    /// Drops after a probe with any other code.
    lost_other: AtomicU64,
    /// Drops with no probe at all (the climb normal was 0).
    lost_no_probe: AtomicU64,
    /// Stop-climbing calls.
    stop: AtomicU64,
    /// Exit-top checks that started the mantle at the top.
    exit_top: AtomicU64,
    /// Bottom exit checks that started the step off at the bottom.
    exit_bottom: AtomicU64,
}

/// The counts at one moment, for the log line. The fields mirror [`Counters`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct Snapshot {
    /// See [`Counters::probe_ok`].
    probe_ok: u64,
    /// See [`Counters::probe_plane`].
    probe_plane: u64,
    /// See [`Counters::probe_clear`].
    probe_clear: u64,
    /// See [`Counters::probe_miss`].
    probe_miss: u64,
    /// See [`Counters::probe_other`].
    probe_other: u64,
    /// See [`Counters::probe_last_code`].
    probe_last_code: u32,
    /// See [`Counters::probe_last_entity`].
    probe_last_entity: u32,
    /// See [`Counters::start_calls`].
    start_calls: u64,
    /// See [`Counters::top_calls`].
    top_calls: u64,
    /// See [`Counters::top_ok`].
    top_ok: u64,
    /// See [`Counters::low_calls`].
    low_calls: u64,
    /// See [`Counters::low_ok`].
    low_ok: u64,
    /// See [`Counters::low_no_input`].
    low_no_input: u64,
    /// See [`Counters::low_angle`].
    low_angle: u64,
    /// See [`Counters::low_cooldown`].
    low_cooldown: u64,
    /// See [`Counters::low_probe`].
    low_probe: u64,
    /// See [`Counters::low_other`].
    low_other: u64,
    /// See [`Counters::lost_calls`].
    lost_calls: u64,
    /// See [`Counters::lost_held`].
    lost_held: u64,
    /// See [`Counters::lost_voxel`].
    lost_voxel: u64,
    /// See [`Counters::lost_plane`].
    lost_plane: u64,
    /// See [`Counters::lost_clear`].
    lost_clear: u64,
    /// See [`Counters::lost_miss`].
    lost_miss: u64,
    /// See [`Counters::lost_other`].
    lost_other: u64,
    /// See [`Counters::lost_no_probe`].
    lost_no_probe: u64,
    /// See [`Counters::stop`].
    stop: u64,
    /// See [`Counters::exit_top`].
    exit_top: u64,
    /// See [`Counters::exit_bottom`].
    exit_bottom: u64,
}

/// Add one to `counter`. It wraps at `u64::MAX`, which a session never reaches.
fn bump(counter: &AtomicU64) {
    counter.fetch_add(1, Ordering::Relaxed);
}

impl Counters {
    /// Every count 0, and no probe yet.
    const fn new() -> Counters {
        Counters {
            probe_ok: AtomicU64::new(0),
            probe_plane: AtomicU64::new(0),
            probe_clear: AtomicU64::new(0),
            probe_miss: AtomicU64::new(0),
            probe_other: AtomicU64::new(0),
            probe_last_code: AtomicU32::new(NO_CODE),
            probe_last_entity: AtomicU32::new(0),
            start_calls: AtomicU64::new(0),
            top_calls: AtomicU64::new(0),
            top_ok: AtomicU64::new(0),
            low_calls: AtomicU64::new(0),
            low_ok: AtomicU64::new(0),
            low_no_input: AtomicU64::new(0),
            low_angle: AtomicU64::new(0),
            low_cooldown: AtomicU64::new(0),
            low_probe: AtomicU64::new(0),
            low_other: AtomicU64::new(0),
            lost_calls: AtomicU64::new(0),
            lost_held: AtomicU64::new(0),
            lost_voxel: AtomicU64::new(0),
            lost_plane: AtomicU64::new(0),
            lost_clear: AtomicU64::new(0),
            lost_miss: AtomicU64::new(0),
            lost_other: AtomicU64::new(0),
            lost_no_probe: AtomicU64::new(0),
            stop: AtomicU64::new(0),
            exit_top: AtomicU64::new(0),
            exit_bottom: AtomicU64::new(0),
        }
    }

    /// One wall-probe call with result `code`. `entity` is the hit EntityId; it is kept only
    /// when the probe passed (a failed probe writes 0 there).
    fn probe(&self, code: u8, entity: u32) {
        match code {
            OK => {
                self.probe_last_entity.store(entity, Ordering::Relaxed);
                bump(&self.probe_ok);
            }
            PLANE => bump(&self.probe_plane),
            CLEAR => bump(&self.probe_clear),
            MISS => bump(&self.probe_miss),
            _ => bump(&self.probe_other),
        }
        self.probe_last_code
            .store(u32::from(code), Ordering::Relaxed);
    }

    /// One start-check call.
    fn start(&self) {
        bump(&self.start_calls);
    }

    /// One top-entry call with result `code`.
    fn top_entry(&self, code: u8) {
        bump(&self.top_calls);
        if code == OK {
            bump(&self.top_ok);
        }
    }

    /// One bottom-entry call with result `code`. `nested` is the code of the probe that ran
    /// inside it, or `None` if none ran.
    fn low_entry(&self, code: u8, nested: Option<u8>) {
        bump(&self.low_calls);
        let count = match (code, nested) {
            (OK, _) => &self.low_ok,
            (NOT_NOW, None) => &self.low_no_input,
            (NOT_NOW, Some(OK)) => &self.low_angle,
            (CLEAR, _) => &self.low_cooldown,
            (MISS, _) => &self.low_probe,
            _ => &self.low_other,
        };
        bump(count);
    }

    /// One wall-lost call that returned `ret`. `nested` is the code of the probe that ran inside
    /// it, or `None` if none ran.
    fn wall_lost(&self, ret: u32, nested: Option<u8>) {
        bump(&self.lost_calls);
        let count = match (ret, nested) {
            (HELD, _) => &self.lost_held,
            (0, None) => &self.lost_no_probe,
            (0, Some(OK)) => &self.lost_voxel,
            (0, Some(PLANE)) => &self.lost_plane,
            (0, Some(CLEAR)) => &self.lost_clear,
            (0, Some(MISS)) => &self.lost_miss,
            (0, Some(_)) => &self.lost_other,
            // A hit EntityId: the climb goes on.
            _ => return,
        };
        bump(count);
    }

    /// One stop-climbing call.
    fn stop(&self) {
        bump(&self.stop);
    }

    /// One exit-top check that returned `taken`; non-zero starts the mantle at the top.
    fn exit_top(&self, taken: u8) {
        if taken != 0 {
            bump(&self.exit_top);
        }
    }

    /// One bottom exit check with result `code`; 0 starts the step off at the bottom.
    fn exit_bottom(&self, code: u8) {
        if code == OK {
            bump(&self.exit_bottom);
        }
    }

    /// Every count now.
    fn snapshot(&self) -> Snapshot {
        let get = |count: &AtomicU64| count.load(Ordering::Relaxed);
        Snapshot {
            probe_ok: get(&self.probe_ok),
            probe_plane: get(&self.probe_plane),
            probe_clear: get(&self.probe_clear),
            probe_miss: get(&self.probe_miss),
            probe_other: get(&self.probe_other),
            probe_last_code: self.probe_last_code.load(Ordering::Relaxed),
            probe_last_entity: self.probe_last_entity.load(Ordering::Relaxed),
            start_calls: get(&self.start_calls),
            top_calls: get(&self.top_calls),
            top_ok: get(&self.top_ok),
            low_calls: get(&self.low_calls),
            low_ok: get(&self.low_ok),
            low_no_input: get(&self.low_no_input),
            low_angle: get(&self.low_angle),
            low_cooldown: get(&self.low_cooldown),
            low_probe: get(&self.low_probe),
            low_other: get(&self.low_other),
            lost_calls: get(&self.lost_calls),
            lost_held: get(&self.lost_held),
            lost_voxel: get(&self.lost_voxel),
            lost_plane: get(&self.lost_plane),
            lost_clear: get(&self.lost_clear),
            lost_miss: get(&self.lost_miss),
            lost_other: get(&self.lost_other),
            lost_no_probe: get(&self.lost_no_probe),
            stop: get(&self.stop),
            exit_top: get(&self.exit_top),
            exit_bottom: get(&self.exit_bottom),
        }
    }
}

impl Snapshot {
    /// The counts that move only with climb activity. The start check's per-frame counts (its
    /// calls, the entries' calls and the bottom entry's "no input") are zeroed, and so are the
    /// last code and EntityId, which change only together with a probe count.
    fn activity(&self) -> Snapshot {
        Snapshot {
            start_calls: 0,
            top_calls: 0,
            low_calls: 0,
            low_no_input: 0,
            probe_last_code: 0,
            probe_last_entity: 0,
            ..*self
        }
    }

    /// Drops: wall-lost calls that returned 0, for any reason.
    fn drops(&self) -> u64 {
        [
            self.lost_voxel,
            self.lost_plane,
            self.lost_clear,
            self.lost_miss,
            self.lost_other,
            self.lost_no_probe,
        ]
        .into_iter()
        .fold(0, u64::saturating_add)
    }
}

/// The text of a probe code: `0x1f`, or `none` before the first probe.
fn code_text(code: u32) -> String {
    if code == NO_CODE {
        "none".to_owned()
    } else {
        format!("{code:#04x}")
    }
}

/// The `climb diag:` log line for `s`.
fn line(s: &Snapshot) -> String {
    let entity = if s.probe_ok == 0 {
        "none".to_owned()
    } else {
        s.probe_last_entity.to_string()
    };
    format!(
        "climb diag: probe ok {}, 0x15 {}, 0x1e {}, 0x1f {}, other {} (last code {}, last entity \
         {entity}) | start {}/{} (top {}/{}, low {}/{}: no input {}, angle {}, cooldown {}, \
         probe {}, other {}) | lost {}/{} (sentinel {}; voxel {}, 0x15 {}, 0x1e {}, 0x1f {}, \
         other {}, no probe {}) | stop {} | exit top {} bottom {}",
        s.probe_ok,
        s.probe_plane,
        s.probe_clear,
        s.probe_miss,
        s.probe_other,
        code_text(s.probe_last_code),
        s.top_ok.saturating_add(s.low_ok),
        s.start_calls,
        s.top_ok,
        s.top_calls,
        s.low_ok,
        s.low_calls,
        s.low_no_input,
        s.low_angle,
        s.low_cooldown,
        s.low_probe,
        s.low_other,
        s.drops(),
        s.lost_calls,
        s.lost_held,
        s.lost_voxel,
        s.lost_plane,
        s.lost_clear,
        s.lost_miss,
        s.lost_other,
        s.lost_no_probe,
        s.stop,
        s.exit_top,
        s.exit_bottom,
    )
}

/// The log line when the activity counts changed since `last`, else `None`. `last` becomes
/// `now` either way.
fn changed_line(last: &mut Snapshot, now: Snapshot) -> Option<String> {
    let changed = now.activity() != last.activity();
    *last = now;
    changed.then(|| line(&now))
}

/// The counts of this process.
static DIAG: Counters = Counters::new();

// ---- the probe that ran inside a hooked function ----------------------------------------------

thread_local! {
    /// The code of this thread's last wall-probe call, or [`NO_PROBE`]. The probe hook sets it.
    /// The bottom-entry and wall-lost hooks clear it before their game function and read it
    /// after, so they see the probe that ran inside it. Const-initialised and without `Drop`:
    /// no allocation and no lazy set-up on a game thread.
    static NESTED: Cell<u16> = const { Cell::new(NO_PROBE) };
}

/// Note `code` as this thread's last wall-probe code.
fn note_probe(code: u8) {
    let _ = NESTED.try_with(|nested| nested.set(u16::from(code)));
}

/// Forget this thread's last wall-probe code.
fn clear_probe() {
    let _ = NESTED.try_with(|nested| nested.set(NO_PROBE));
}

/// This thread's last wall-probe code since [`clear_probe`], or `None` if no probe ran.
fn nested_probe() -> Option<u8> {
    NESTED
        .try_with(Cell::get)
        .ok()
        .and_then(|code| u8::try_from(code).ok())
}

// ---- the hooks --------------------------------------------------------------------------------

/// Run `original` exactly once, then `record` its result in an inner guard, and return the
/// result.
///
/// - The feature is off: only `original` runs.
/// - `record` panics: the inner guard catches it and switches the feature off; the result of
///   the one `original` call is still returned.
/// - Anything else panics after `original` ran: the outer fallback returns the stored result
///   and does not run `original` again.
fn run_then_record<T: Copy>(original: impl Fn() -> T, record: impl FnOnce(T)) -> T {
    let ran = Cell::new(None);
    guard(
        Feature::ClimbDiag,
        || ran.get().unwrap_or_else(&original),
        || {
            let value = original();
            ran.set(Some(value));
            guard(Feature::ClimbDiag, || (), || record(value));
            value
        },
    )
}

/// The result code at `result` (byte +0), or [`UNREAD`] for a null pointer.
///
/// # Safety
///
/// `result` is null, or points to a game result whose first byte the game function just wrote.
unsafe fn code_at(result: *const u8) -> u8 {
    if result.is_null() {
        return UNREAD;
    }
    // SAFETY: the caller's contract: one readable byte that the game function just wrote.
    unsafe { result.read() }
}

/// The wall probe's hit EntityId (+0x20), or 0 for a null pointer.
///
/// # Safety
///
/// `result` is null, or points to a wall-probe result that the probe just wrote: its 0x24
/// bytes, so +0x20..+0x24 is inside it.
unsafe fn entity_at(result: *const u8) -> u32 {
    if result.is_null() {
        return 0;
    }
    // SAFETY: the caller's contract: +0x20..+0x24 is inside the result the probe just wrote. An
    // unaligned read needs no alignment.
    unsafe { result.add(ENTITY_AT).cast::<u32>().read_unaligned() }
}

/// The wall probe `FUN_14024abc0`. It writes its result to `out` (code at +0, hit EntityId at
/// +0x20) and returns `out`. `len` is the fifth argument, a float on the stack, which lengthens
/// the forward ray.
type ProbeFn = unsafe extern "system" fn(
    out: *mut u8,
    ctx: *mut u8,
    pos: *const f32,
    dir: *const f32,
    len: f32,
) -> *mut u8;

/// The start check `FUN_140419570`: one argument (the climb program's iterator), no result.
type StartFn = unsafe extern "system" fn(it: *mut u8);

/// The top entry `FUN_14024c320`. It writes its result to `out` (code at +0) and returns `out`.
/// `arg3` and `arg5` (the fifth, on the stack) are passed on unchanged.
type TopEntryFn = unsafe extern "system" fn(
    out: *mut u8,
    ctx: *mut u8,
    arg3: u64,
    input: *mut u8,
    arg5: u64,
) -> *mut u8;

/// The bottom/middle entry `FUN_14024c0d0`. It writes its result to `out` (code at +0) and
/// returns `out`. `time` is the game time, compared with the re-grab cooldown.
type LowEntryFn =
    unsafe extern "system" fn(out: *mut u8, ctx: *mut u8, input: *mut u8, time: i64) -> *mut u8;

/// The wall-lost check `FUN_14024b390`: the probe's hit EntityId, 0 for "wall lost", or the
/// voxel-hold value.
type WallLostFn = unsafe extern "system" fn(ctx: *mut u8) -> u32;

/// Stop climbing `FUN_14024a090`: no result.
type StopFn = unsafe extern "system" fn(ctx: *mut u8, time: i64);

/// The exit-top check `FUN_14024c9f0`: 1 starts the mantle at the top, 0 does not (in `al`).
type ExitTopFn = unsafe extern "system" fn(ctx: *mut u8, input: *mut u8) -> u8;

/// The bottom exit check `FUN_14024c8f0`. It writes its result to `out` (code at +0; 0 starts the
/// step off at the bottom, 6 does not) and returns `out`.
type ExitLowFn = unsafe extern "system" fn(out: *mut u8, ctx: *mut u8, input: *mut u8) -> *mut u8;

/// MinHook's trampoline for the wall probe, or 0. [`install`] stores it before the hook is
/// enabled; the detour reads it.
static PROBE_ORIGINAL: AtomicUsize = AtomicUsize::new(0);
/// MinHook's trampoline for the start check, or 0. As [`PROBE_ORIGINAL`].
static START_ORIGINAL: AtomicUsize = AtomicUsize::new(0);
/// MinHook's trampoline for the top entry, or 0. As [`PROBE_ORIGINAL`].
static TOP_ENTRY_ORIGINAL: AtomicUsize = AtomicUsize::new(0);
/// MinHook's trampoline for the bottom entry, or 0. As [`PROBE_ORIGINAL`].
static LOW_ENTRY_ORIGINAL: AtomicUsize = AtomicUsize::new(0);
/// MinHook's trampoline for the wall-lost check, or 0. As [`PROBE_ORIGINAL`].
static WALL_LOST_ORIGINAL: AtomicUsize = AtomicUsize::new(0);
/// MinHook's trampoline for stop climbing, or 0. As [`PROBE_ORIGINAL`].
static STOP_ORIGINAL: AtomicUsize = AtomicUsize::new(0);
/// MinHook's trampoline for the exit-top check, or 0. As [`PROBE_ORIGINAL`].
static EXIT_TOP_ORIGINAL: AtomicUsize = AtomicUsize::new(0);
/// MinHook's trampoline for the bottom exit check, or 0. As [`PROBE_ORIGINAL`].
static EXIT_LOW_ORIGINAL: AtomicUsize = AtomicUsize::new(0);

/// Post-hook on the wall probe: run it, then count its code, keep a hit's EntityId and note the
/// code for an enclosing bottom-entry or wall-lost hook.
///
/// # Safety
///
/// The game's contract for `FUN_14024abc0`: `out` is its 0x24-byte result buffer, and every
/// argument is the game's own.
unsafe extern "system" fn probe_detour(
    out: *mut u8,
    ctx: *mut u8,
    pos: *const f32,
    dir: *const f32,
    len: f32,
) -> *mut u8 {
    let trampoline = PROBE_ORIGINAL.load(Ordering::Acquire);
    let original = move || {
        if trampoline == 0 {
            return out;
        }
        // SAFETY: non-zero, so MinHook's trampoline for the wall probe, which has this type.
        let probe = unsafe { std::mem::transmute::<usize, ProbeFn>(trampoline) };
        // SAFETY: the game's own arguments, passed on unchanged.
        unsafe { probe(out, ctx, pos, dir, len) }
    };
    run_then_record(original, |result| {
        if trampoline == 0 {
            return;
        }
        // SAFETY: the probe writes its code (+0) and its EntityId (+0x20) in every path, and it
        // returns `out`, the caller's 0x24-byte buffer (docs/re-notes.md, "Climb diagnostic").
        let (code, entity) = unsafe { (code_at(result), entity_at(result)) };
        DIAG.probe(code, entity);
        note_probe(code);
    })
}

/// Post-hook on the start check: run it, then count the call.
///
/// # Safety
///
/// The game's contract for `FUN_140419570`: `it` is the game's own argument.
unsafe extern "system" fn start_detour(it: *mut u8) {
    let trampoline = START_ORIGINAL.load(Ordering::Acquire);
    let original = move || {
        if trampoline == 0 {
            return;
        }
        // SAFETY: non-zero, so MinHook's trampoline for the start check, which has this type.
        let start = unsafe { std::mem::transmute::<usize, StartFn>(trampoline) };
        // SAFETY: the game's own argument, passed on unchanged.
        unsafe { start(it) }
    };
    run_then_record(original, |()| {
        if trampoline != 0 {
            DIAG.start();
        }
    });
}

/// Post-hook on the top entry: run it, then count its code.
///
/// # Safety
///
/// The game's contract for `FUN_14024c320`: `out` is its result buffer, and every argument is
/// the game's own.
unsafe extern "system" fn top_entry_detour(
    out: *mut u8,
    ctx: *mut u8,
    arg3: u64,
    input: *mut u8,
    arg5: u64,
) -> *mut u8 {
    let trampoline = TOP_ENTRY_ORIGINAL.load(Ordering::Acquire);
    let original = move || {
        if trampoline == 0 {
            return out;
        }
        // SAFETY: non-zero, so MinHook's trampoline for the top entry, which has this type.
        let entry = unsafe { std::mem::transmute::<usize, TopEntryFn>(trampoline) };
        // SAFETY: the game's own arguments, passed on unchanged.
        unsafe { entry(out, ctx, arg3, input, arg5) }
    };
    run_then_record(original, |result| {
        if trampoline == 0 {
            return;
        }
        // SAFETY: the top entry writes its code (+0) in every path and returns `out`.
        DIAG.top_entry(unsafe { code_at(result) });
    })
}

/// Post-hook on the bottom entry: run it, then count its code, split by the probe that ran
/// inside it.
///
/// # Safety
///
/// The game's contract for `FUN_14024c0d0`: `out` is its result buffer, and every argument is
/// the game's own.
unsafe extern "system" fn low_entry_detour(
    out: *mut u8,
    ctx: *mut u8,
    input: *mut u8,
    time: i64,
) -> *mut u8 {
    let trampoline = LOW_ENTRY_ORIGINAL.load(Ordering::Acquire);
    let original = move || {
        if trampoline == 0 {
            return (out, None);
        }
        // SAFETY: non-zero, so MinHook's trampoline for the bottom entry, which has this type.
        let entry = unsafe { std::mem::transmute::<usize, LowEntryFn>(trampoline) };
        clear_probe();
        // SAFETY: the game's own arguments, passed on unchanged.
        let result = unsafe { entry(out, ctx, input, time) };
        (result, nested_probe())
    };
    run_then_record(original, |(result, nested)| {
        if trampoline == 0 {
            return;
        }
        // SAFETY: the bottom entry writes its code (+0) in every path and returns `out`.
        DIAG.low_entry(unsafe { code_at(result) }, nested);
    })
    .0
}

/// Post-hook on the wall-lost check: run it, then count its result, and for a drop the probe
/// that ran inside it.
///
/// # Safety
///
/// The game's contract for `FUN_14024b390`: `ctx` is the game's own argument.
unsafe extern "system" fn wall_lost_detour(ctx: *mut u8) -> u32 {
    let trampoline = WALL_LOST_ORIGINAL.load(Ordering::Acquire);
    let original = move || {
        if trampoline == 0 {
            return (0, None);
        }
        // SAFETY: non-zero, so MinHook's trampoline for the wall-lost check, which has this type.
        let lost = unsafe { std::mem::transmute::<usize, WallLostFn>(trampoline) };
        clear_probe();
        // SAFETY: the game's own argument, passed on unchanged.
        let ret = unsafe { lost(ctx) };
        (ret, nested_probe())
    };
    run_then_record(original, |(ret, nested)| {
        if trampoline != 0 {
            DIAG.wall_lost(ret, nested);
        }
    })
    .0
}

/// Post-hook on stop climbing: run it, then count the call.
///
/// # Safety
///
/// The game's contract for `FUN_14024a090`: both arguments are the game's own.
unsafe extern "system" fn stop_detour(ctx: *mut u8, time: i64) {
    let trampoline = STOP_ORIGINAL.load(Ordering::Acquire);
    let original = move || {
        if trampoline == 0 {
            return;
        }
        // SAFETY: non-zero, so MinHook's trampoline for stop climbing, which has this type.
        let stop = unsafe { std::mem::transmute::<usize, StopFn>(trampoline) };
        // SAFETY: the game's own arguments, passed on unchanged.
        unsafe { stop(ctx, time) }
    };
    run_then_record(original, |()| {
        if trampoline != 0 {
            DIAG.stop();
        }
    });
}

/// Post-hook on the exit-top check: run it, then count a taken exit.
///
/// # Safety
///
/// The game's contract for `FUN_14024c9f0`: both arguments are the game's own.
unsafe extern "system" fn exit_top_detour(ctx: *mut u8, input: *mut u8) -> u8 {
    let trampoline = EXIT_TOP_ORIGINAL.load(Ordering::Acquire);
    let original = move || {
        if trampoline == 0 {
            return 0;
        }
        // SAFETY: non-zero, so MinHook's trampoline for the exit-top check, which has this type.
        let exit = unsafe { std::mem::transmute::<usize, ExitTopFn>(trampoline) };
        // SAFETY: the game's own arguments, passed on unchanged.
        unsafe { exit(ctx, input) }
    };
    run_then_record(original, |taken| {
        if trampoline != 0 {
            DIAG.exit_top(taken);
        }
    })
}

/// Post-hook on the bottom exit check: run it, then count a taken exit.
///
/// # Safety
///
/// The game's contract for `FUN_14024c8f0`: `out` is its result buffer, and every argument is
/// the game's own.
unsafe extern "system" fn exit_low_detour(out: *mut u8, ctx: *mut u8, input: *mut u8) -> *mut u8 {
    let trampoline = EXIT_LOW_ORIGINAL.load(Ordering::Acquire);
    let original = move || {
        if trampoline == 0 {
            return out;
        }
        // SAFETY: non-zero, so MinHook's trampoline for the bottom exit check, which has this
        // type.
        let exit = unsafe { std::mem::transmute::<usize, ExitLowFn>(trampoline) };
        // SAFETY: the game's own arguments, passed on unchanged.
        unsafe { exit(out, ctx, input) }
    };
    run_then_record(original, |result| {
        if trampoline == 0 {
            return;
        }
        // SAFETY: the bottom exit check writes its code (+0) in every path and returns `out`.
        DIAG.exit_bottom(unsafe { code_at(result) });
    })
}

// Each detour has exactly the type of the function it replaces.
const _: ProbeFn = probe_detour;
const _: StartFn = start_detour;
const _: TopEntryFn = top_entry_detour;
const _: LowEntryFn = low_entry_detour;
const _: WallLostFn = wall_lost_detour;
const _: StopFn = stop_detour;
const _: ExitTopFn = exit_top_detour;
const _: ExitLowFn = exit_low_detour;

// ---- install ----------------------------------------------------------------------------------

/// One hook: the signature of its game function, its detour, and where its trampoline goes.
#[derive(Clone, Copy)]
struct Hook {
    /// The signature that finds the function's entry.
    sig: &'static Sig,
    /// The detour's address.
    detour: usize,
    /// Where the trampoline goes; the detour reads it.
    original: &'static AtomicUsize,
}

/// The eight hooks, in the order of [`sigs::CLIMB_DIAG`].
fn hooks() -> [Hook; 8] {
    [
        Hook {
            sig: &CLIMB_DIAG_PROBE_FN,
            detour: probe_detour as *const () as usize,
            original: &PROBE_ORIGINAL,
        },
        Hook {
            sig: &CLIMB_DIAG_START_FN,
            detour: start_detour as *const () as usize,
            original: &START_ORIGINAL,
        },
        Hook {
            sig: &CLIMB_DIAG_TOP_ENTRY_FN,
            detour: top_entry_detour as *const () as usize,
            original: &TOP_ENTRY_ORIGINAL,
        },
        Hook {
            sig: &CLIMB_DIAG_LOW_ENTRY_FN,
            detour: low_entry_detour as *const () as usize,
            original: &LOW_ENTRY_ORIGINAL,
        },
        Hook {
            sig: &CLIMB_DIAG_WALL_LOST_FN,
            detour: wall_lost_detour as *const () as usize,
            original: &WALL_LOST_ORIGINAL,
        },
        Hook {
            sig: &CLIMB_DIAG_STOP_FN,
            detour: stop_detour as *const () as usize,
            original: &STOP_ORIGINAL,
        },
        Hook {
            sig: &CLIMB_DIAG_EXIT_TOP_FN,
            detour: exit_top_detour as *const () as usize,
            original: &EXIT_TOP_ORIGINAL,
        },
        Hook {
            sig: &CLIMB_DIAG_EXIT_LOW_FN,
            detour: exit_low_detour as *const () as usize,
            original: &EXIT_LOW_ORIGINAL,
        },
    ]
}

/// Find the entry of every hooked function in `m`. `Err` names every signature that did not
/// match exactly once.
///
/// Start-up only, as `scan::find_in_sections`.
fn find_all(m: &Module) -> Result<Vec<(Hook, usize)>, String> {
    let all = hooks();
    let mut found = Vec::new();
    let mut failed = Vec::new();
    for hook in all {
        let sig = hook.sig;
        let entry = Pattern::parse(sig.pattern)
            .and_then(|pattern| find_in_sections(m, sig.sections, &pattern))
            .map_err(|error| format!("{}: {error}", sig.name))
            .and_then(|at| {
                at.checked_add(sig.patch_offset())
                    .ok_or_else(|| format!("{}: the entry is past the end of memory", sig.name))
            });
        match entry {
            Ok(at) => found.push((hook, at)),
            Err(why) => failed.push(why),
        }
    }
    if failed.is_empty() {
        Ok(found)
    } else {
        Err(format!(
            "{} of {} climb diag signatures did not match exactly once, so nothing was hooked: {}",
            failed.len(),
            all.len(),
            failed.join("; ")
        ))
    }
}

/// Create `hook` on the function at `entry` and queue it, then store its trampoline, before
/// anything can enable the hook.
///
/// # Safety
///
/// The contract of `create_and_queue` for `entry`: the entry of the game function that
/// `hook.sig` names, whose type is that of `hook.detour`.
unsafe fn queue(hook: &Hook, entry: usize) -> Result<(), String> {
    // SAFETY: the caller's contract.
    let trampoline = unsafe { create_and_queue(entry, hook.detour) }?;
    hook.original.store(trampoline, Ordering::Release);
    Ok(())
}

/// What [`install`] queued: how many hooks, and why each other one was not.
#[derive(Debug, Default, PartialEq, Eq)]
struct Installed {
    /// Hooks queued; `knobs::enable_queued_hooks` enables them.
    queued: usize,
    /// One text per hook that could not be queued.
    failed: Vec<String>,
}

/// Find every climb diag signature in `m`, then queue a hook on each function. `Err` is why
/// nothing was hooked.
///
/// # Safety
///
/// Start-up only: call it from `entry::start` (in `InitializeASI`, before the game's `WinMain`),
/// on one thread, with `m` the running exe (`scan::main_module()`).
unsafe fn install(m: &Module) -> Result<Installed, String> {
    let found = find_all(m)?;
    let mut done = Installed::default();
    for (hook, entry) in found {
        // SAFETY: `entry` is the entry of the game function that `hook.sig` names: the one match
        // of its prologue in `.text`, at patch offset 0. Its first instructions cover at least
        // five bytes, none is RIP-relative or a branch, and no branch in the function lands
        // inside them (docs/re-notes.md, "Climb diagnostic"). `hook.detour` has the function's
        // type (the `const _` checks above). Start-up only: this function's contract.
        match unsafe { queue(&hook, entry) } {
            Ok(()) => done.queued = done.queued.saturating_add(1),
            Err(why) => done.failed.push(format!("{}: {why}", hook.sig.name)),
        }
    }
    if done.queued == 0 {
        return Err(format!(
            "no hook could be queued: {}",
            done.failed.join("; ")
        ));
    }
    Ok(done)
}

/// The body of [`init`], with the code-patch answer, the install and the log thread passed in.
/// Returns the log level and the line.
fn start_up(
    cfg: &ExperimentalCfg,
    code_patches: bool,
    install: impl FnOnce() -> Result<Installed, String>,
    start_logger: impl FnOnce() -> Result<(), String>,
) -> (Level, String) {
    if !cfg.climb_diag {
        return (Level::Info, "climb diag: off: ClimbDiag = 0".to_owned());
    }
    if !code_patches {
        return (
            Level::Info,
            "climb diag: off: code patches are off on this build".to_owned(),
        );
    }
    let installed = match install() {
        Ok(installed) => installed,
        Err(why) => return (Level::Warn, format!("climb diag: off: {why}")),
    };
    let total = sigs::CLIMB_DIAG.len();
    let hooks = if installed.failed.is_empty() {
        format!("all {total} hooks queued")
    } else {
        format!(
            "{} of {total} hooks queued; not hooked: {}",
            installed.queued,
            installed.failed.join("; ")
        )
    };
    match start_logger() {
        Ok(()) => {
            let level = if installed.failed.is_empty() {
                Level::Info
            } else {
                Level::Warn
            };
            (
                level,
                format!(
                    "climb diag: on (EXPERIMENTAL): {hooks}; one line a second while the climb \
                     counts change"
                ),
            )
        }
        Err(why) => (
            Level::Warn,
            format!("climb diag: counting, but no log lines: {why} ({hooks})"),
        ),
    }
}

/// The log thread: once a second, log the line when the activity counts changed. It holds only
/// local state. A panic switches the feature off and ends the thread; so does a hook's panic.
fn logger_main() {
    guard(
        Feature::ClimbDiag,
        || (),
        || {
            let mut last = Snapshot::default();
            loop {
                std::thread::sleep(LOG_INTERVAL);
                if guard::is_disabled(Feature::ClimbDiag) {
                    return;
                }
                if let Some(line) = changed_line(&mut last, DIAG.snapshot()) {
                    log::info!("{line}");
                }
            }
        },
    );
}

/// Start the detached `climb-diag` log thread.
fn start_logger() -> Result<(), String> {
    std::thread::Builder::new()
        .name("climb-diag".into())
        .spawn(logger_main)
        .map(|_| ())
        .map_err(|error| format!("cannot start the log thread: {error}"))
}

/// The start-up step: decide, queue the hooks, start the log thread, and log one line. Called
/// once, from `entry::start`, after `code_patches_allowed` is set and before
/// `knobs::enable_queued_hooks`. A panic here is caught, so the later start-up steps still run.
pub fn init(cfg: &ExperimentalCfg) {
    // Unwind safety: each queued hook stores its trampoline before the next one is created, and
    // nothing after an `Err` uses the state the closure was changing.
    let outcome = guard::catch("climb diag", || {
        start_up(
            cfg,
            entry::code_patches_allowed(),
            || {
                // SAFETY: `init` runs from `entry::start`, in `InitializeASI`, before the game's
                // `WinMain`: one thread. `main_module()` is the running exe.
                unsafe { install(&main_module()) }
            },
            start_logger,
        )
    });
    match outcome {
        Ok((level, line)) => log::log!(level, "{line}"),
        Err(why) => log::warn!("climb diag: stopped: {why}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::knobs::apply_queued;
    use minhook::MinHook;
    use std::cell::RefCell;
    use std::ffi::{c_void, OsStr};
    use std::process::Command;
    use std::ptr::{null, null_mut};
    use std::sync::atomic::Ordering::Relaxed;
    use std::sync::atomic::{AtomicBool, AtomicU8};

    // ---- the counts (a local `Counters`, never `DIAG`) -----------------------------------------

    #[test]
    fn probe_codes_go_to_their_counts_and_a_pass_keeps_the_entity() {
        let c = Counters::new();
        assert_eq!(c.snapshot().probe_last_code, NO_CODE);
        c.probe(OK, 0);
        c.probe(OK, 4242);
        c.probe(PLANE, 0);
        c.probe(CLEAR, 0);
        c.probe(CLEAR, 0);
        c.probe(MISS, 0);
        c.probe(MISS, 0);
        c.probe(MISS, 0);
        c.probe(0x07, 99);
        c.probe(UNREAD, 0);
        let s = c.snapshot();
        assert_eq!(
            (
                s.probe_ok,
                s.probe_plane,
                s.probe_clear,
                s.probe_miss,
                s.probe_other
            ),
            (2, 1, 2, 3, 2)
        );
        assert_eq!(s.probe_last_code, u32::from(UNREAD));
        assert_eq!(
            s.probe_last_entity, 4242,
            "a failed probe does not replace the entity"
        );
        c.probe(OK, 0);
        assert_eq!(c.snapshot().probe_last_entity, 0, "a voxel hit is entity 0");
        assert_eq!(c.snapshot().probe_last_code, 0);
    }

    #[test]
    fn the_start_counts_calls_and_each_entry_result() {
        let c = Counters::new();
        c.start();
        c.start();
        c.top_entry(OK);
        c.top_entry(CLEAR);
        c.top_entry(NOT_NOW);
        c.low_entry(OK, Some(OK));
        c.low_entry(NOT_NOW, None);
        c.low_entry(NOT_NOW, None);
        c.low_entry(NOT_NOW, Some(OK));
        c.low_entry(CLEAR, None);
        c.low_entry(MISS, Some(PLANE));
        c.low_entry(MISS, Some(MISS));
        c.low_entry(0x42, None);
        c.low_entry(NOT_NOW, Some(PLANE));
        let s = c.snapshot();
        assert_eq!(s.start_calls, 2);
        assert_eq!((s.top_ok, s.top_calls), (1, 3));
        assert_eq!((s.low_ok, s.low_calls), (1, 9));
        assert_eq!(s.low_no_input, 2, "code 6 with no probe");
        assert_eq!(s.low_angle, 1, "code 6 after a probe that passed");
        assert_eq!(s.low_cooldown, 1);
        assert_eq!(s.low_probe, 2);
        assert_eq!(
            s.low_other, 2,
            "an unknown code, and code 6 after a failed probe"
        );
        assert_eq!(s.probe_ok, 0, "the entry counts are not probe counts");
    }

    #[test]
    fn a_drop_is_named_by_the_probe_that_ran_inside_the_wall_lost_check() {
        let c = Counters::new();
        c.wall_lost(0, None);
        c.wall_lost(0, Some(OK));
        c.wall_lost(0, Some(OK));
        c.wall_lost(0, Some(PLANE));
        c.wall_lost(0, Some(CLEAR));
        c.wall_lost(0, Some(MISS));
        c.wall_lost(0, Some(0x33));
        c.wall_lost(HELD, Some(OK));
        c.wall_lost(HELD, Some(OK));
        c.wall_lost(HELD, Some(OK));
        c.wall_lost(1234, Some(OK));
        let s = c.snapshot();
        assert_eq!(s.lost_calls, 11);
        assert_eq!(s.lost_held, 3);
        assert_eq!(
            (
                s.lost_voxel,
                s.lost_plane,
                s.lost_clear,
                s.lost_miss,
                s.lost_other,
                s.lost_no_probe
            ),
            (2, 1, 1, 1, 1, 1)
        );
    }

    #[test]
    fn stops_and_only_taken_exits_are_counted() {
        let c = Counters::new();
        c.stop();
        c.exit_top(1);
        c.exit_top(0);
        c.exit_top(0);
        c.exit_bottom(OK);
        c.exit_bottom(NOT_NOW);
        c.exit_bottom(NOT_NOW);
        c.exit_bottom(OK);
        let s = c.snapshot();
        assert_eq!((s.stop, s.exit_top, s.exit_bottom), (1, 1, 2));
    }

    // ---- the line ------------------------------------------------------------------------------

    /// Every field different, so the test sees each one in its place.
    fn busy() -> Snapshot {
        Snapshot {
            probe_ok: 1,
            probe_plane: 2,
            probe_clear: 3,
            probe_miss: 4,
            probe_other: 5,
            probe_last_code: 0x1f,
            probe_last_entity: 77,
            start_calls: 100,
            top_calls: 90,
            top_ok: 6,
            low_calls: 84,
            low_ok: 7,
            low_no_input: 50,
            low_angle: 8,
            low_cooldown: 9,
            low_probe: 10,
            low_other: 20,
            lost_calls: 300,
            lost_held: 200,
            lost_voxel: 11,
            lost_plane: 12,
            lost_clear: 13,
            lost_miss: 14,
            lost_other: 15,
            lost_no_probe: 16,
            stop: 17,
            exit_top: 18,
            exit_bottom: 19,
        }
    }

    #[test]
    fn the_line_names_every_count() {
        assert_eq!(
            line(&busy()),
            "climb diag: probe ok 1, 0x15 2, 0x1e 3, 0x1f 4, other 5 (last code 0x1f, last \
             entity 77) | start 13/100 (top 6/90, low 7/84: no input 50, angle 8, cooldown 9, \
             probe 10, other 20) | lost 81/300 (sentinel 200; voxel 11, 0x15 12, 0x1e 13, 0x1f \
             14, other 15, no probe 16) | stop 17 | exit top 18 bottom 19"
        );
    }

    #[test]
    fn before_any_probe_the_line_says_none() {
        assert_eq!(
            line(&Counters::new().snapshot()),
            "climb diag: probe ok 0, 0x15 0, 0x1e 0, 0x1f 0, other 0 (last code none, last \
             entity none) | start 0/0 (top 0/0, low 0/0: no input 0, angle 0, cooldown 0, probe \
             0, other 0) | lost 0/0 (sentinel 0; voxel 0, 0x15 0, 0x1e 0, 0x1f 0, other 0, no \
             probe 0) | stop 0 | exit top 0 bottom 0"
        );
        assert_eq!(code_text(NO_CODE), "none");
        assert_eq!(code_text(0), "0x00");
        assert_eq!(code_text(6), "0x06");
        assert_eq!(code_text(0x1f), "0x1f");
        assert_eq!(code_text(0xff), "0xff");
    }

    #[test]
    fn the_last_entity_shows_once_a_probe_passed() {
        let s = Snapshot {
            probe_ok: 1,
            probe_last_code: 0,
            probe_last_entity: 0,
            ..Snapshot::default()
        };
        assert!(
            line(&s).contains("(last code 0x00, last entity 0)"),
            "{}",
            line(&s)
        );
    }

    #[test]
    fn sums_saturate_instead_of_overflowing() {
        let s = Snapshot {
            top_ok: u64::MAX,
            low_ok: 1,
            lost_voxel: u64::MAX,
            lost_no_probe: 5,
            ..Snapshot::default()
        };
        let text = line(&s);
        assert!(text.contains(&format!("start {}/0", u64::MAX)), "{text}");
        assert!(text.contains(&format!("lost {}/0", u64::MAX)), "{text}");
    }

    // ---- when a line is written ----------------------------------------------------------------

    #[test]
    fn the_per_frame_counts_alone_write_no_line() {
        let mut last = Snapshot::default();
        let idle = Snapshot {
            start_calls: 60,
            top_calls: 60,
            low_calls: 60,
            low_no_input: 60,
            probe_last_code: NO_CODE,
            ..Snapshot::default()
        };
        assert_eq!(changed_line(&mut last, idle), None);
        assert_eq!(last, idle, "last follows now");
        let walking = Snapshot {
            probe_miss: 1,
            low_probe: 1,
            probe_last_code: u32::from(MISS),
            ..idle
        };
        assert_eq!(changed_line(&mut last, walking), Some(line(&walking)));
        assert_eq!(changed_line(&mut last, walking), None, "no change, no line");
        let more_idle = Snapshot {
            start_calls: 120,
            top_calls: 120,
            low_calls: 120,
            low_no_input: 120,
            ..walking
        };
        assert_eq!(changed_line(&mut last, more_idle), None);
    }

    #[test]
    fn every_activity_count_writes_a_line() {
        let base = busy();
        let bumped: [fn(&mut Snapshot); 23] = [
            |s| s.probe_ok += 1,
            |s| s.probe_plane += 1,
            |s| s.probe_clear += 1,
            |s| s.probe_miss += 1,
            |s| s.probe_other += 1,
            |s| s.top_ok += 1,
            |s| s.low_ok += 1,
            |s| s.low_angle += 1,
            |s| s.low_cooldown += 1,
            |s| s.low_probe += 1,
            |s| s.low_other += 1,
            |s| s.lost_calls += 1,
            |s| s.lost_held += 1,
            |s| s.lost_voxel += 1,
            |s| s.lost_plane += 1,
            |s| s.lost_clear += 1,
            |s| s.lost_miss += 1,
            |s| s.lost_other += 1,
            |s| s.lost_no_probe += 1,
            |s| s.stop += 1,
            |s| s.exit_top += 1,
            |s| s.exit_bottom += 1,
            |s| s.probe_ok += 2,
        ];
        for (i, bump) in bumped.iter().enumerate() {
            let mut last = base;
            let mut now = base;
            bump(&mut now);
            assert!(changed_line(&mut last, now).is_some(), "change {i}");
        }
        let quiet: [fn(&mut Snapshot); 6] = [
            |s| s.start_calls += 1,
            |s| s.top_calls += 1,
            |s| s.low_calls += 1,
            |s| s.low_no_input += 1,
            |s| s.probe_last_code = 0x15,
            |s| s.probe_last_entity = 5,
        ];
        for (i, bump) in quiet.iter().enumerate() {
            let mut last = base;
            let mut now = base;
            bump(&mut now);
            assert!(changed_line(&mut last, now).is_none(), "quiet {i}");
        }
    }

    // ---- the thread-local probe code -----------------------------------------------------------

    #[test]
    fn the_nested_probe_code_is_per_call_and_per_thread() {
        clear_probe();
        assert_eq!(nested_probe(), None);
        note_probe(PLANE);
        assert_eq!(nested_probe(), Some(PLANE));
        note_probe(OK);
        assert_eq!(nested_probe(), Some(OK), "the last probe wins");
        assert_eq!(
            std::thread::spawn(nested_probe).join().unwrap(),
            None,
            "another thread has its own"
        );
        clear_probe();
        assert_eq!(nested_probe(), None);
    }

    // ---- start-up ------------------------------------------------------------------------------

    /// The `[Experimental]` settings with the diagnostic on.
    fn on() -> ExperimentalCfg {
        ExperimentalCfg {
            climb_diag: true,
            ..Config::default().experimental
        }
    }

    /// An install that queued every hook.
    fn all_queued() -> Result<Installed, String> {
        Ok(Installed {
            queued: 8,
            failed: Vec::new(),
        })
    }

    #[test]
    fn the_diagnostic_off_means_no_install_and_says_so() {
        let called = Cell::new(false);
        let (level, line) = start_up(
            &Config::default().experimental,
            true,
            || {
                called.set(true);
                all_queued()
            },
            || {
                called.set(true);
                Ok(())
            },
        );
        assert!(!called.get(), "neither install nor the log thread may run");
        assert_eq!(level, Level::Info);
        assert_eq!(line, "climb diag: off: ClimbDiag = 0");
    }

    #[test]
    fn no_code_patches_means_no_install_and_says_so() {
        let called = Cell::new(false);
        let (level, line) = start_up(
            &on(),
            false,
            || {
                called.set(true);
                all_queued()
            },
            || {
                called.set(true);
                Ok(())
            },
        );
        assert!(!called.get(), "neither install nor the log thread may run");
        assert_eq!(level, Level::Info);
        assert_eq!(line, "climb diag: off: code patches are off on this build");
    }

    #[test]
    fn a_full_install_starts_the_log_thread_and_says_so() {
        let order = RefCell::new(Vec::new());
        let (level, line) = start_up(
            &on(),
            true,
            || {
                order.borrow_mut().push("install");
                all_queued()
            },
            || {
                order.borrow_mut().push("logger");
                Ok(())
            },
        );
        assert_eq!(*order.borrow(), ["install", "logger"]);
        assert_eq!(level, Level::Info);
        assert_eq!(
            line,
            "climb diag: on (EXPERIMENTAL): all 8 hooks queued; one line a second while the \
             climb counts change"
        );
    }

    #[test]
    fn a_partial_install_is_a_warning_that_names_the_missing_hooks() {
        let (level, line) = start_up(
            &on(),
            true,
            || {
                Ok(Installed {
                    queued: 7,
                    failed: vec!["CLIMB_DIAG_STOP_FN: MinHook says no".to_owned()],
                })
            },
            || Ok(()),
        );
        assert_eq!(level, Level::Warn);
        assert_eq!(
            line,
            "climb diag: on (EXPERIMENTAL): 7 of 8 hooks queued; not hooked: CLIMB_DIAG_STOP_FN: \
             MinHook says no; one line a second while the climb counts change"
        );
    }

    #[test]
    fn a_failed_install_starts_no_log_thread() {
        let logger = Cell::new(false);
        let (level, line) = start_up(
            &on(),
            true,
            || Err("why".to_owned()),
            || {
                logger.set(true);
                Ok(())
            },
        );
        assert!(!logger.get());
        assert_eq!(level, Level::Warn);
        assert_eq!(line, "climb diag: off: why");
    }

    #[test]
    fn a_log_thread_that_cannot_start_is_a_warning() {
        let (level, line) = start_up(&on(), true, all_queued, || Err("no thread".to_owned()));
        assert_eq!(level, Level::Warn);
        assert_eq!(
            line,
            "climb diag: counting, but no log lines: no thread (all 8 hooks queued)"
        );
    }

    #[test]
    fn the_hooks_follow_the_signature_table() {
        let names: Vec<&str> = hooks().iter().map(|h| h.sig.name).collect();
        let table: Vec<&str> = sigs::CLIMB_DIAG.iter().map(|s| s.name).collect();
        assert_eq!(names, table);
        let mut slots: Vec<usize> = hooks()
            .iter()
            .map(|h| h.original as *const AtomicUsize as usize)
            .collect();
        slots.sort_unstable();
        slots.dedup();
        assert_eq!(slots.len(), 8, "every hook has its own trampoline slot");
        let mut detours: Vec<usize> = hooks().iter().map(|h| h.detour).collect();
        detours.sort_unstable();
        detours.dedup();
        assert_eq!(detours.len(), 8, "every hook has its own detour");
    }

    #[test]
    fn install_in_this_exe_finds_nothing_and_hooks_nothing() {
        // SAFETY: the test binary is the running exe; no signature matches in it, so `install`
        // stops before it creates any hook.
        let why = unsafe { install(&main_module()) }.unwrap_err();
        assert!(
            why.contains("climb diag signatures did not match exactly once, so nothing was hooked"),
            "{why}"
        );
    }

    /// The climb feature writes these bytes in the probe and the wall-lost check (docs/re-notes.md,
    /// "Climb anything (experimental)"): start and length. A diag pattern must end before them,
    /// or a scan after the climb install would not match.
    const CLIMB_WRITES: [(u64, u64); 7] = [
        (0x1_4024_accc, 4),
        (0x1_4024_af53, 4),
        (0x1_4024_afbc, 4),
        (0x1_4024_aff0, 4),
        (0x1_4024_b4a5, 12),
        (0x1_4024_ad86, 1),
        (0x1_4024_ad55, 1),
    ];

    /// Where each climb diag signature matches in the Steam exe (sha256 d64966e2...; the old
    /// build has the same `.text`): the function entries. docs/re-notes.md, "Climb diagnostic".
    const ANCHORS: [(&str, u64); 8] = [
        ("CLIMB_DIAG_PROBE_FN", 0x1_4024_abc0),
        ("CLIMB_DIAG_START_FN", 0x1_4041_9570),
        ("CLIMB_DIAG_TOP_ENTRY_FN", 0x1_4024_c320),
        ("CLIMB_DIAG_LOW_ENTRY_FN", 0x1_4024_c0d0),
        ("CLIMB_DIAG_WALL_LOST_FN", 0x1_4024_b390),
        ("CLIMB_DIAG_STOP_FN", 0x1_4024_a090),
        ("CLIMB_DIAG_EXIT_TOP_FN", 0x1_4024_c9f0),
        ("CLIMB_DIAG_EXIT_LOW_FN", 0x1_4024_c8f0),
    ];

    #[test]
    fn no_diag_pattern_covers_a_byte_the_climb_feature_writes() {
        for &sig in sigs::CLIMB_DIAG {
            let anchor = ANCHORS
                .iter()
                .find(|(name, _)| *name == sig.name)
                .map(|(_, va)| *va)
                .expect("an anchor for every signature");
            let end = anchor + sig.pattern.split_ascii_whitespace().count() as u64;
            for (site, len) in CLIMB_WRITES {
                assert!(
                    end <= site || site + len <= anchor,
                    "{} [{anchor:#x}, {end:#x}) covers the climb write at {site:#x}",
                    sig.name
                );
            }
        }
    }

    // ---- the detours, in a child process --------------------------------------------------------

    /// Names the test that a child process runs. Unset in the parent.
    const CHILD_ENV: &str = "ENSHROUDED_HEADROOM_CLIMB_DIAG_CHILD";

    /// Run `scenario` in a fresh copy of this test binary, alone. There no test has switched
    /// `Feature::ClimbDiag` off yet, [`DIAG`] and the trampoline slots are fresh, and no hook is
    /// written into a process where other tests run. `name` is the calling test's name.
    fn in_child(name: &str, scenario: impl FnOnce()) {
        if std::env::var_os(CHILD_ENV).as_deref() == Some(OsStr::new(name)) {
            scenario();
            return;
        }
        let module = module_path!()
            .split_once("::")
            .map_or(module_path!(), |(_, rest)| rest);
        let full = format!("{module}::{name}");
        let output = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", &full, "--nocapture", "--test-threads=1"])
            .env(CHILD_ENV, name)
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            output.status.success(),
            "child {full} failed: {:?}\nstdout:\n{stdout}\nstderr:\n{stderr}",
            output.status
        );
        assert!(
            stdout.contains("1 passed"),
            "the child must have run {full}\nstdout:\n{stdout}"
        );
    }

    #[test]
    fn the_original_runs_exactly_once_whatever_the_record_does() {
        in_child(
            "the_original_runs_exactly_once_whatever_the_record_does",
            || {
                assert!(!guard::is_disabled(Feature::ClimbDiag), "a fresh process");
                let order = RefCell::new(Vec::new());
                let value = run_then_record(
                    || {
                        order.borrow_mut().push("original".to_owned());
                        7
                    },
                    |v| order.borrow_mut().push(format!("record {v}")),
                );
                assert_eq!(value, 7);
                assert_eq!(*order.borrow(), ["original", "record 7"]);

                order.borrow_mut().clear();
                let value = run_then_record(
                    || {
                        order.borrow_mut().push("original".to_owned());
                        8
                    },
                    |_| panic!("record boom"),
                );
                assert_eq!(value, 8, "the one result, not a second call's");
                assert_eq!(*order.borrow(), ["original"], "the original ran once");
                assert!(
                    guard::is_disabled(Feature::ClimbDiag),
                    "the panic switched the feature off"
                );

                order.borrow_mut().clear();
                let value = run_then_record(
                    || {
                        order.borrow_mut().push("original".to_owned());
                        9
                    },
                    |v| order.borrow_mut().push(format!("record {v}")),
                );
                assert_eq!(value, 9);
                assert_eq!(*order.borrow(), ["original"], "off: only the original runs");
            },
        );
    }

    /// The test's 0x24-byte result buffers, like the game's.
    type Result24 = [u8; 0x24];

    static FAKE_PROBE_CALLS: AtomicU32 = AtomicU32::new(0);
    static FAKE_PROBE_CODE: AtomicU8 = AtomicU8::new(0);
    static FAKE_PROBE_ENTITY: AtomicU32 = AtomicU32::new(0);
    static FAKE_ARGS: [AtomicUsize; 4] = [const { AtomicUsize::new(0) }; 4];
    static FAKE_LEN: AtomicU32 = AtomicU32::new(0);

    /// A stand-in for the wall probe: it records its arguments and writes the code and EntityId
    /// that the test set.
    unsafe extern "system" fn fake_probe(
        out: *mut u8,
        ctx: *mut u8,
        pos: *const f32,
        dir: *const f32,
        len: f32,
    ) -> *mut u8 {
        FAKE_PROBE_CALLS.fetch_add(1, Relaxed);
        for (slot, arg) in
            FAKE_ARGS
                .iter()
                .zip([out as usize, ctx as usize, pos as usize, dir as usize])
        {
            slot.store(arg, Relaxed);
        }
        FAKE_LEN.store(len.to_bits(), Relaxed);
        // SAFETY: the tests pass a 0x24-byte buffer.
        unsafe {
            out.write(FAKE_PROBE_CODE.load(Relaxed));
            out.add(ENTITY_AT)
                .cast::<u32>()
                .write_unaligned(FAKE_PROBE_ENTITY.load(Relaxed));
        }
        out
    }

    static FAKE_INNER_PROBE: AtomicBool = AtomicBool::new(false);
    static FAKE_CODE: AtomicU8 = AtomicU8::new(0);
    static FAKE_RET: AtomicU32 = AtomicU32::new(0);
    static FAKE_CALLS: AtomicU32 = AtomicU32::new(0);

    /// Call the hooked probe from inside a stand-in, as the game does, when the test asks.
    fn maybe_probe_inside() {
        if FAKE_INNER_PROBE.load(Relaxed) {
            let mut inner: Result24 = [0; 0x24];
            // SAFETY: the probe detour with a 0x24-byte buffer; its trampoline is `fake_probe`.
            unsafe { probe_detour(inner.as_mut_ptr(), null_mut(), null(), null(), 0.0) };
        }
    }

    /// A stand-in for the bottom entry: it may run the probe, then writes the test's code.
    unsafe extern "system" fn fake_low_entry(
        out: *mut u8,
        _ctx: *mut u8,
        _input: *mut u8,
        _time: i64,
    ) -> *mut u8 {
        FAKE_CALLS.fetch_add(1, Relaxed);
        maybe_probe_inside();
        // SAFETY: the tests pass a 0x24-byte buffer.
        unsafe { out.write(FAKE_CODE.load(Relaxed)) };
        out
    }

    /// A stand-in for the wall-lost check: it may run the probe, then returns the test's value.
    unsafe extern "system" fn fake_wall_lost(_ctx: *mut u8) -> u32 {
        FAKE_CALLS.fetch_add(1, Relaxed);
        maybe_probe_inside();
        FAKE_RET.load(Relaxed)
    }

    /// A stand-in for the top entry: it writes the test's code.
    unsafe extern "system" fn fake_top_entry(
        out: *mut u8,
        _ctx: *mut u8,
        _arg3: u64,
        _input: *mut u8,
        arg5: u64,
    ) -> *mut u8 {
        FAKE_CALLS.fetch_add(1, Relaxed);
        FAKE_LEN.store(arg5 as u32, Relaxed);
        // SAFETY: the tests pass a 0x24-byte buffer.
        unsafe { out.write(FAKE_CODE.load(Relaxed)) };
        out
    }

    /// A stand-in for the start check.
    unsafe extern "system" fn fake_start(_it: *mut u8) {
        FAKE_CALLS.fetch_add(1, Relaxed);
    }

    /// A stand-in for stop climbing.
    unsafe extern "system" fn fake_stop(_ctx: *mut u8, time: i64) {
        FAKE_CALLS.fetch_add(1, Relaxed);
        FAKE_RET.store(time as u32, Relaxed);
    }

    /// A stand-in for the exit-top check: it returns the test's code.
    unsafe extern "system" fn fake_exit_top(_ctx: *mut u8, _input: *mut u8) -> u8 {
        FAKE_CALLS.fetch_add(1, Relaxed);
        FAKE_CODE.load(Relaxed)
    }

    /// A stand-in for the bottom exit check: it writes the test's code.
    unsafe extern "system" fn fake_exit_low(
        out: *mut u8,
        _ctx: *mut u8,
        _input: *mut u8,
    ) -> *mut u8 {
        FAKE_CALLS.fetch_add(1, Relaxed);
        // SAFETY: the tests pass a 0x24-byte buffer.
        unsafe { out.write(FAKE_CODE.load(Relaxed)) };
        out
    }

    const _: ProbeFn = fake_probe;
    const _: LowEntryFn = fake_low_entry;
    const _: WallLostFn = fake_wall_lost;
    const _: TopEntryFn = fake_top_entry;
    const _: StartFn = fake_start;
    const _: StopFn = fake_stop;
    const _: ExitTopFn = fake_exit_top;
    const _: ExitLowFn = fake_exit_low;

    /// Point every trampoline slot at its stand-in.
    fn use_the_fakes() {
        PROBE_ORIGINAL.store(fake_probe as *const () as usize, Relaxed);
        START_ORIGINAL.store(fake_start as *const () as usize, Relaxed);
        TOP_ENTRY_ORIGINAL.store(fake_top_entry as *const () as usize, Relaxed);
        LOW_ENTRY_ORIGINAL.store(fake_low_entry as *const () as usize, Relaxed);
        WALL_LOST_ORIGINAL.store(fake_wall_lost as *const () as usize, Relaxed);
        STOP_ORIGINAL.store(fake_stop as *const () as usize, Relaxed);
        EXIT_TOP_ORIGINAL.store(fake_exit_top as *const () as usize, Relaxed);
        EXIT_LOW_ORIGINAL.store(fake_exit_low as *const () as usize, Relaxed);
    }

    #[test]
    fn the_probe_detour_passes_everything_on_and_counts_the_result() {
        in_child(
            "the_probe_detour_passes_everything_on_and_counts_the_result",
            || {
                use_the_fakes();
                let mut out: Result24 = [0xcc; 0x24];
                FAKE_PROBE_CODE.store(PLANE, Relaxed);
                // SAFETY: the detour's trampoline is `fake_probe`, which only records the
                // pointers and writes into `out`.
                let ret = unsafe {
                    probe_detour(
                        out.as_mut_ptr(),
                        0x1111 as *mut u8,
                        0x2222 as *const f32,
                        0x3333 as *const f32,
                        2.5,
                    )
                };
                assert_eq!(ret, out.as_mut_ptr(), "the result is passed on");
                assert_eq!(FAKE_PROBE_CALLS.load(Relaxed), 1, "the probe ran once");
                let args: Vec<usize> = FAKE_ARGS.iter().map(|a| a.load(Relaxed)).collect();
                assert_eq!(args, [out.as_ptr() as usize, 0x1111, 0x2222, 0x3333]);
                assert_eq!(FAKE_LEN.load(Relaxed), 2.5f32.to_bits());
                assert_eq!(out[0], PLANE, "the detour writes nothing");
                let s = DIAG.snapshot();
                assert_eq!((s.probe_plane, s.probe_ok), (1, 0));
                assert_eq!(s.probe_last_code, u32::from(PLANE));
                assert_eq!(nested_probe(), Some(PLANE));

                FAKE_PROBE_CODE.store(OK, Relaxed);
                FAKE_PROBE_ENTITY.store(0x00c0_ffee, Relaxed);
                // SAFETY: as above.
                unsafe { probe_detour(out.as_mut_ptr(), null_mut(), null(), null(), 0.0) };
                let s = DIAG.snapshot();
                assert_eq!(s.probe_ok, 1);
                assert_eq!(s.probe_last_entity, 0x00c0_ffee);
                assert_eq!(FAKE_PROBE_CALLS.load(Relaxed), 2);
            },
        );
    }

    #[test]
    fn the_bottom_entry_detour_splits_its_code_by_the_probe_inside() {
        in_child(
            "the_bottom_entry_detour_splits_its_code_by_the_probe_inside",
            || {
                use_the_fakes();
                let run = |inner: Option<u8>, code: u8| {
                    FAKE_INNER_PROBE.store(inner.is_some(), Relaxed);
                    FAKE_PROBE_CODE.store(inner.unwrap_or(OK), Relaxed);
                    FAKE_CODE.store(code, Relaxed);
                    let mut out: Result24 = [0; 0x24];
                    // SAFETY: the trampolines are the stand-ins; `out` is 0x24 bytes.
                    let ret =
                        unsafe { low_entry_detour(out.as_mut_ptr(), null_mut(), null_mut(), 5) };
                    assert_eq!(ret, out.as_mut_ptr());
                    assert_eq!(out[0], code);
                };
                note_probe(PLANE); // a probe from before must not count
                run(None, NOT_NOW);
                run(Some(OK), NOT_NOW);
                run(Some(MISS), MISS);
                run(Some(OK), OK);
                run(None, CLEAR);
                let s = DIAG.snapshot();
                assert_eq!(FAKE_CALLS.load(Relaxed), 5, "the entry ran once a call");
                assert_eq!(s.low_calls, 5);
                assert_eq!(s.low_no_input, 1);
                assert_eq!(s.low_angle, 1);
                assert_eq!(s.low_probe, 1);
                assert_eq!(s.low_ok, 1);
                assert_eq!(s.low_cooldown, 1);
                assert_eq!(s.low_other, 0);
                assert_eq!(
                    (s.probe_ok, s.probe_miss),
                    (2, 1),
                    "the inner probes count too"
                );
            },
        );
    }

    #[test]
    fn the_wall_lost_detour_names_each_drop_by_the_probe_inside() {
        in_child(
            "the_wall_lost_detour_names_each_drop_by_the_probe_inside",
            || {
                use_the_fakes();
                let run = |inner: Option<u8>, ret: u32| {
                    FAKE_INNER_PROBE.store(inner.is_some(), Relaxed);
                    FAKE_PROBE_CODE.store(inner.unwrap_or(OK), Relaxed);
                    FAKE_RET.store(ret, Relaxed);
                    // SAFETY: the trampolines are the stand-ins.
                    let got = unsafe { wall_lost_detour(null_mut()) };
                    assert_eq!(got, ret, "the result is passed on");
                };
                note_probe(MISS); // a probe from before must not count
                run(None, 0);
                run(Some(OK), 0);
                run(Some(PLANE), 0);
                run(Some(CLEAR), 0);
                run(Some(MISS), 0);
                run(Some(OK), HELD);
                run(Some(OK), 77);
                let s = DIAG.snapshot();
                assert_eq!(FAKE_CALLS.load(Relaxed), 7);
                assert_eq!(s.lost_calls, 7);
                assert_eq!(s.lost_no_probe, 1);
                assert_eq!(s.lost_voxel, 1);
                assert_eq!(s.lost_plane, 1);
                assert_eq!(s.lost_clear, 1);
                assert_eq!(s.lost_miss, 1);
                assert_eq!(s.lost_held, 1);
                assert_eq!(s.lost_other, 0);
            },
        );
    }

    #[test]
    fn the_other_detours_pass_their_results_on_and_count() {
        in_child("the_other_detours_pass_their_results_on_and_count", || {
            use_the_fakes();
            let mut out: Result24 = [0; 0x24];
            // SAFETY: every trampoline is a stand-in; `out` is 0x24 bytes.
            unsafe {
                start_detour(null_mut());
                FAKE_CODE.store(OK, Relaxed);
                let ret = top_entry_detour(out.as_mut_ptr(), null_mut(), 3, null_mut(), 0x55);
                assert_eq!(ret, out.as_mut_ptr());
                assert_eq!(FAKE_LEN.load(Relaxed), 0x55, "the fifth argument");
                stop_detour(null_mut(), 0x66);
                assert_eq!(FAKE_RET.load(Relaxed), 0x66);
                FAKE_CODE.store(1, Relaxed);
                assert_eq!(exit_top_detour(null_mut(), null_mut()), 1);
                FAKE_CODE.store(0, Relaxed);
                assert_eq!(exit_top_detour(null_mut(), null_mut()), 0);
                FAKE_CODE.store(OK, Relaxed);
                let ret = exit_low_detour(out.as_mut_ptr(), null_mut(), null_mut());
                assert_eq!(ret, out.as_mut_ptr());
                FAKE_CODE.store(NOT_NOW, Relaxed);
                exit_low_detour(out.as_mut_ptr(), null_mut(), null_mut());
            }
            let s = DIAG.snapshot();
            assert_eq!(FAKE_CALLS.load(Relaxed), 7, "each stand-in ran once a call");
            assert_eq!(s.start_calls, 1);
            assert_eq!((s.top_ok, s.top_calls), (1, 1));
            assert_eq!(s.stop, 1);
            assert_eq!(s.exit_top, 1);
            assert_eq!(s.exit_bottom, 1);
        });
    }

    // ---- a real MinHook hook on a stand-in with the probe's exact prologue shape ---------------

    static NAKED_RCX: AtomicUsize = AtomicUsize::new(0);
    static NAKED_RDX: AtomicUsize = AtomicUsize::new(0);
    static NAKED_R8: AtomicUsize = AtomicUsize::new(0);
    static NAKED_R9: AtomicUsize = AtomicUsize::new(0);
    static NAKED_LEN: AtomicU32 = AtomicU32::new(0);
    static NAKED_CODE: AtomicU8 = AtomicU8::new(0);
    static NAKED_ENTITY: AtomicU32 = AtomicU32::new(0);
    static NAKED_CALLS: AtomicU32 = AtomicU32::new(0);

    /// A stand-in for the wall probe in plain machine code, so the bytes MinHook moves are known:
    /// its first instruction is the 5-byte `mov [rsp+8],rbx`. It records RCX, RDX, R8, R9 and the
    /// fifth argument's bits (the stack slot at `[rsp+0x28]`), writes the test's code at +0 and
    /// EntityId at +0x20, and returns RCX.
    #[unsafe(naked)]
    unsafe extern "system" fn naked_probe(
        _out: *mut u8,
        _ctx: *mut u8,
        _pos: *const f32,
        _dir: *const f32,
        _len: f32,
    ) -> *mut u8 {
        core::arch::naked_asm!(
            "mov qword ptr [rsp + 8], rbx",
            "mov qword ptr [rip + {rcx}], rcx",
            "mov qword ptr [rip + {rdx}], rdx",
            "mov qword ptr [rip + {r8}], r8",
            "mov qword ptr [rip + {r9}], r9",
            "mov eax, dword ptr [rsp + 0x28]",
            "mov dword ptr [rip + {len}], eax",
            "movzx eax, byte ptr [rip + {code}]",
            "mov byte ptr [rcx], al",
            "mov eax, dword ptr [rip + {entity}]",
            "mov dword ptr [rcx + 0x20], eax",
            "lock inc dword ptr [rip + {calls}]",
            "mov rax, rcx",
            "ret",
            rcx = sym NAKED_RCX,
            rdx = sym NAKED_RDX,
            r8 = sym NAKED_R8,
            r9 = sym NAKED_R9,
            len = sym NAKED_LEN,
            code = sym NAKED_CODE,
            entity = sym NAKED_ENTITY,
            calls = sym NAKED_CALLS,
        );
    }

    #[test]
    fn a_real_hook_passes_every_argument_and_the_result_on() {
        in_child(
            "a_real_hook_passes_every_argument_and_the_result_on",
            || {
                let target = naked_probe as *const () as usize;
                let hook = Hook {
                    sig: &CLIMB_DIAG_PROBE_FN,
                    detour: probe_detour as *const () as usize,
                    original: &PROBE_ORIGINAL,
                };
                // SAFETY: `target` is the stand-in: executable, longer than five bytes, its first
                // instruction is five bytes, and nothing jumps into it; the detour has its type.
                unsafe { queue(&hook, target) }.unwrap();
                assert_eq!(apply_queued(), Ok(1));
                let probe: ProbeFn = std::hint::black_box(naked_probe);
                for (code, entity, len) in [(MISS, 0, 0.75f32), (OK, 31337, -0.0), (OK, 0, 2.5)] {
                    NAKED_CODE.store(code, Relaxed);
                    NAKED_ENTITY.store(entity, Relaxed);
                    let mut out: Result24 = [0xcc; 0x24];
                    // SAFETY: the hooked stand-in only records the pointers and writes into `out`.
                    let ret = unsafe {
                        probe(
                            out.as_mut_ptr(),
                            0x1234 as *mut u8,
                            0x5678 as *const f32,
                            0x9abc as *const f32,
                            len,
                        )
                    };
                    assert_eq!(ret, out.as_mut_ptr());
                    assert_eq!(NAKED_RCX.load(Relaxed), out.as_ptr() as usize);
                    assert_eq!(NAKED_RDX.load(Relaxed), 0x1234);
                    assert_eq!(NAKED_R8.load(Relaxed), 0x5678);
                    assert_eq!(NAKED_R9.load(Relaxed), 0x9abc);
                    assert_eq!(NAKED_LEN.load(Relaxed), len.to_bits(), "the stack float");
                    assert_eq!(out[0], code);
                }
                assert_eq!(NAKED_CALLS.load(Relaxed), 3, "the stand-in ran once a call");
                let s = DIAG.snapshot();
                assert_eq!((s.probe_ok, s.probe_miss), (2, 1));
                assert_eq!(s.probe_last_entity, 0, "the last pass was a voxel");
                // SAFETY: the hook at `target` was made above and nothing runs it now.
                unsafe { MinHook::remove_hook(target as *mut c_void) }.unwrap();
            },
        );
    }

    // ---- the real game exe (ignored: reads ENSHROUDED_STEAM_EXE, else the Steam install) -------

    #[test]
    #[ignore = "reads the Steam exe (ENSHROUDED_STEAM_EXE, else the Steam path); a skip shows only with -- --ignored --nocapture"]
    fn climb_diag_sigs_unique_in_steam_exe() {
        use crate::build_id::test_support::steam_exe_or_skip;
        use crate::build_id::{classify, sha256_file};
        use crate::scan::{find_unique, parse_pe_file};
        let Some(path) = steam_exe_or_skip("climb_diag_sigs_unique_in_steam_exe") else {
            return;
        };
        println!(
            "exe {}: {:?}",
            path.display(),
            classify(sha256_file(&path).expect("hash the game exe"))
        );
        let file = std::fs::read(&path).expect("read the game exe");
        let pe = parse_pe_file(&file).expect("PE headers of the game exe");
        let (text_va, text) = pe.section_va_bytes(&file, ".text").unwrap();
        let mut failures = Vec::new();
        for &sig in sigs::CLIMB_DIAG {
            let anchor = ANCHORS
                .iter()
                .find(|(name, _)| *name == sig.name)
                .map(|(_, va)| *va)
                .expect("an anchor for every climb diag signature");
            let pattern = Pattern::parse(sig.pattern).unwrap();
            match find_unique(text, &pattern) {
                Ok(at) => {
                    let va = text_va + at as u64;
                    println!("{:<26} match {va:#x}  (anchor {anchor:#x})", sig.name);
                    if va != anchor {
                        failures.push(format!("{}: matched {va:#x}, anchor {anchor:#x}", sig.name));
                    }
                }
                Err(error) => failures.push(format!("{}: {error}", sig.name)),
            }
        }
        assert!(failures.is_empty(), "{failures:?}");
    }
}
