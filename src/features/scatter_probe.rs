//! Scatter counter probe (design.md 7.2a): reads the game's own grass GPU counters, so the
//! governor can tell how close the grass is to its fixed budget.
//!
//! The game keeps three GPU counters for detail scatter (grass). Its pool update reads each one
//! back to the CPU and keeps the last and the peak value next to a warn level and a cap. Counter
//! A counts the scattered objects of both grass passes against one budget of `0x14000`; the game
//! sets its warn level at `0xf000` (75 %). RE facts and evidence ids: docs/re-notes.md, "Scatter
//! counter probe".
//!
//! [`install`] puts a MinHook pre-hook on `FUN_1400767a0(rcx, pass, f)` (found by
//! `sigs::SCATTER_FRAME_FN`). The detour ([`scatter_frame_detour`]) runs inside
//! `guard(Feature::ScatterDistance, ...)`. On every 30th call it follows the pointer chain from
//! `pass` with [`read_counters`], checking every address with [`is_readable`] before it reads it,
//! and publishes the result to the atomics below. Then it calls the original with the same
//! arguments, exactly once. It never logs and never allocates.
//!
//! The self-check: counter A must show the warn level `0xf000` and the cap `0x14000`. If it does
//! not, or any read is refused, the chain is wrong for this build. Then [`FAILED`] is set,
//! [`PROBE_OK`] is cleared, and the probe stops reading for the rest of the session.
//!
//! Task 15's governor consumes this module: the grass knob may rise only while [`PROBE_OK`],
//! [`ceiling_step`] and [`headroom_ok`] allow it.

use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};

use log::Level;
use windows_sys::Win32::System::Memory::{VirtualQuery, MEMORY_BASIC_INFORMATION, MEM_COMMIT};

use crate::config::{Config, LogLevel};
use crate::entry;
use crate::guard::{guard, Feature};
use crate::knobs::create_and_queue;
use crate::scan::{find_in_sections, main_module, protect_is_readable, Module, Pattern};
use crate::sigs::SCATTER_FRAME_FN;

/// Counter A's warn level, as the game allocates it (`0x1400745eb`): 75 % of [`CAP_A`]. The
/// self-check and the headroom rule use it.
pub const WARN_A: u32 = 0xf000;

/// Counter A's cap, as the game allocates it (`0x1400745e0`): the size of the scattered-object
/// arrays.
pub const CAP_A: u32 = 0x14000;

/// The highest grass distance in metres that is known to be safe. It stays at the game's own
/// 90 m until the author's measurement session sets it (plan Task 13b, Step 6).
pub const R_MAX_M: f32 = 90.0;

/// The hook reads the counters on one call in this many (the first call included).
const SAMPLE_EVERY: u32 = 30;

/// `[pass+0x8]`: the object that holds the counter pool.
const PASS_POOL_OWNER: usize = 0x8;
/// `[pass+0x18]`: the scatter object that holds the three counter handles.
const PASS_SCATTER_OBJ: usize = 0x18;
/// `[owner+0x20]`: the GPU counter pool.
const OWNER_POOL: usize = 0x20;
/// `[pool+0x48]`: the pool's entry array.
const POOL_ENTRIES: usize = 0x48;
/// `[obj+0x8]`, `[obj+0xc]`, `[obj+0x10]`: the handles of counters A, B and C.
const COUNTER_HANDLES: [usize; 3] = [0x8, 0xc, 0x10];
/// The size of one pool entry.
const ENTRY_SIZE: usize = 0x40;
/// Entry field: the last value read back.
const ENTRY_LAST: usize = 0x14;
/// Entry field: the highest value read back since the counter was allocated.
const ENTRY_PEAK: usize = 0x1c;
/// Entry field: the warn level the game allocated the counter with.
const ENTRY_WARN: usize = 0x20;
/// Entry field: the cap the game allocated the counter with.
const ENTRY_CAP: usize = 0x24;
/// Entry field: the handle of the counter that owns the entry (the game's own check).
const ENTRY_HANDLE: usize = 0x38;
/// A handle's low 16 bits are its entry index.
const HANDLE_INDEX_MASK: u32 = 0xffff;

/// One counter's values, as the game keeps them in its pool entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CounterSample {
    /// The last value read back from the GPU (entry+0x14).
    pub last: u32,
    /// The highest value read back since the scene view was built (entry+0x1c).
    pub peak: u32,
    /// The warn level the game set (entry+0x20).
    pub warn: u32,
    /// The cap the game set (entry+0x24).
    pub cap: u32,
}

/// True once a sample passed the self-check; false before that, and false for good after a
/// failure. Stored with `Release` after the values below, so a reader that loads it with
/// `Acquire` and sees true also sees the values of that sample (or newer ones).
pub static PROBE_OK: AtomicBool = AtomicBool::new(false);
/// True once a sample failed (a refused read, a handle that does not match its entry, or a
/// failed self-check). The probe stops reading then. Stored with `Release` after `PROBE_OK` was
/// cleared.
pub static FAILED: AtomicBool = AtomicBool::new(false);
/// Counter A's last value from the latest good sample. `Relaxed`; see [`PROBE_OK`].
pub static LAST_A: AtomicU32 = AtomicU32::new(0);
/// Counter A's peak from the latest good sample. `Relaxed`; see [`PROBE_OK`].
pub static PEAK_A: AtomicU32 = AtomicU32::new(0);
/// Counter B's last value from the latest good sample. `Relaxed`; see [`PROBE_OK`].
pub static LAST_B: AtomicU32 = AtomicU32::new(0);
/// Counter B's peak from the latest good sample (for the debug log). `Relaxed`.
pub static PEAK_B: AtomicU32 = AtomicU32::new(0);
/// Counter C's last value from the latest good sample. `Relaxed`; see [`PROBE_OK`].
pub static LAST_C: AtomicU32 = AtomicU32::new(0);
/// Counter C's peak from the latest good sample (for the debug log). `Relaxed`.
pub static PEAK_C: AtomicU32 = AtomicU32::new(0);

/// How many times the hook has run (wraps). It picks the calls that take a sample.
static CALLS: AtomicU32 = AtomicU32::new(0);

/// MinHook's trampoline for the game function, or 0. Stored by [`install`] before the hook is
/// enabled; read by the detour.
static TRAMPOLINE: AtomicUsize = AtomicUsize::new(0);

/// Test only: makes the hook's sample panic, so a test can drive the real detour's panic path.
/// Not in a non-test build.
#[cfg(test)]
static PANIC_IN_SAMPLE: AtomicBool = AtomicBool::new(false);

/// The type of the game function and of its detour: RCX, RDX and an `f32` in XMM2, no stack
/// argument, no return value (docs/re-notes.md, "Scatter counter probe").
pub type ScatterFrameFn = unsafe extern "system" fn(rcx: *mut u8, pass: *const u8, f: f32);

/// Can the `len` bytes at `addr` be read now? `VirtualQuery` must report committed memory with a
/// readable protection that is not a guard page (the rule of `scan::protect_is_readable`), and
/// the whole range must lie in that one region. An overflowing range gives false.
///
/// This is a check, not a lock: another thread can free the memory after it. The probe reads
/// game objects that the call it hooks is using, so they stay alive during that call.
pub fn is_readable(addr: usize, len: usize) -> bool {
    let Some(end) = addr.checked_add(len) else {
        return false;
    };
    let mut info = MEMORY_BASIC_INFORMATION::default();
    // SAFETY: `info` is a valid buffer of exactly the size passed. VirtualQuery accepts any
    // address, reads no memory at it, and reports failure by returning 0.
    let written = unsafe {
        VirtualQuery(
            addr as *const c_void,
            &mut info,
            size_of::<MEMORY_BASIC_INFORMATION>(),
        )
    };
    if written < size_of::<MEMORY_BASIC_INFORMATION>()
        || info.State != MEM_COMMIT
        || !protect_is_readable(info.Protect)
    {
        return false;
    }
    let region = info.BaseAddress as usize;
    region <= addr && end <= region.saturating_add(info.RegionSize)
}

/// Follow the counter chain from `pass` and return counters A, B and C. `None` if any read is
/// refused, a handle is 0 or does not match its entry, or counter A fails the self-check (warn
/// [`WARN_A`], cap [`CAP_A`]).
///
/// The chain (docs/re-notes.md): obj = `[pass+0x18]`; pool = `[[pass+0x8]+0x20]`; base =
/// `[pool+0x48]`; for each handle h at `[obj+0x8]` (A), `[obj+0xc]` (B), `[obj+0x10]` (C), the
/// entry is base + (h & 0xffff) × 0x40, and `[entry+0x38]` must equal h. Each sample is last
/// (entry+0x14), peak (+0x1c), warn (+0x20) and cap (+0x24).
///
/// It calls `readable(addr, len)` before EVERY read, and reads nothing after a refusal. It reads
/// with `read_unaligned`, so it needs no alignment.
///
/// # Safety
///
/// `readable(addr, len)` returns true only if the `len` bytes at `addr` can be read when the
/// read that follows the call happens ([`is_readable`], for memory that no other thread frees
/// meanwhile).
pub unsafe fn read_counters(
    pass: usize,
    readable: impl Fn(usize, usize) -> bool,
) -> Option<[CounterSample; 3]> {
    let read_ptr = |at: usize| -> Option<usize> {
        if !readable(at, size_of::<u64>()) {
            return None;
        }
        // SAFETY: `readable` has just accepted these 8 bytes (this function's contract).
        let value = unsafe { std::ptr::read_unaligned(at as *const u64) };
        usize::try_from(value).ok()
    };
    let read_u32 = |at: usize| -> Option<u32> {
        if !readable(at, size_of::<u32>()) {
            return None;
        }
        // SAFETY: `readable` has just accepted these 4 bytes (this function's contract).
        Some(unsafe { std::ptr::read_unaligned(at as *const u32) })
    };
    let obj = read_ptr(pass.checked_add(PASS_SCATTER_OBJ)?)?;
    let owner = read_ptr(pass.checked_add(PASS_POOL_OWNER)?)?;
    let pool = read_ptr(owner.checked_add(OWNER_POOL)?)?;
    let entries = read_ptr(pool.checked_add(POOL_ENTRIES)?)?;
    let counter = |handle_at: usize| -> Option<CounterSample> {
        let handle = read_u32(obj.checked_add(handle_at)?)?;
        if handle == 0 {
            return None;
        }
        let index = usize::try_from(handle & HANDLE_INDEX_MASK).ok()?;
        let entry = entries.checked_add(index.checked_mul(ENTRY_SIZE)?)?;
        // The game's own accessor makes this check (docs/re-notes.md): the entry must belong to
        // this handle.
        if read_u32(entry.checked_add(ENTRY_HANDLE)?)? != handle {
            return None;
        }
        Some(CounterSample {
            last: read_u32(entry.checked_add(ENTRY_LAST)?)?,
            peak: read_u32(entry.checked_add(ENTRY_PEAK)?)?,
            warn: read_u32(entry.checked_add(ENTRY_WARN)?)?,
            cap: read_u32(entry.checked_add(ENTRY_CAP)?)?,
        })
    };
    let [at_a, at_b, at_c] = COUNTER_HANDLES;
    let a = counter(at_a)?;
    if a.warn != WARN_A || a.cap != CAP_A {
        return None;
    }
    Some([a, counter(at_b)?, counter(at_c)?])
}

/// The live headroom rule (design.md 7.2): may the grass distance go from `r_now` to `r_next`
/// metres? Only if counter A's last value, scaled by the area (`(r_next / r_now)²`), stays below
/// the game's warn level: `last × (r_next / r_now)² < WARN_A`. False for a non-finite or
/// non-positive `r_now`, and for a NaN result.
pub fn headroom_ok(last: u32, r_now: f32, r_next: f32) -> bool {
    if !(r_now.is_finite() && r_now > 0.0) {
        return false;
    }
    let ratio = f64::from(r_next) / f64::from(r_now);
    // A NaN on the left makes the comparison false.
    f64::from(last) * ratio * ratio < f64::from(WARN_A)
}

/// The highest index in `steps` whose distance is at most `r_max`; 0 if none is.
pub fn ceiling_step(steps: &[f32], r_max: f32) -> usize {
    steps
        .iter()
        .rposition(|&distance| distance <= r_max)
        .unwrap_or(0)
}

/// The atomics a sample is published to: the public statics, or a test's own.
struct Outputs<'a> {
    /// See [`PROBE_OK`].
    ok: &'a AtomicBool,
    /// See [`FAILED`].
    failed: &'a AtomicBool,
    /// See [`LAST_A`].
    last_a: &'a AtomicU32,
    /// See [`PEAK_A`].
    peak_a: &'a AtomicU32,
    /// See [`LAST_B`].
    last_b: &'a AtomicU32,
    /// See [`PEAK_B`].
    peak_b: &'a AtomicU32,
    /// See [`LAST_C`].
    last_c: &'a AtomicU32,
    /// See [`PEAK_C`].
    peak_c: &'a AtomicU32,
}

impl Outputs<'_> {
    /// Publish one sample: the values and `ok` for a good one; `failed` (and `ok` cleared) for
    /// `None`. After a failure nothing changes any more.
    fn publish(&self, sample: Option<[CounterSample; 3]>) {
        if self.failed.load(Ordering::Acquire) {
            return;
        }
        match sample {
            Some([a, b, c]) => {
                self.last_a.store(a.last, Ordering::Relaxed);
                self.peak_a.store(a.peak, Ordering::Relaxed);
                self.last_b.store(b.last, Ordering::Relaxed);
                self.peak_b.store(b.peak, Ordering::Relaxed);
                self.last_c.store(c.last, Ordering::Relaxed);
                self.peak_c.store(c.peak, Ordering::Relaxed);
                // After the values: a reader that sees `ok` with `Acquire` sees them.
                self.ok.store(true, Ordering::Release);
            }
            None => {
                self.ok.store(false, Ordering::Release);
                // After `ok` is cleared: a reader that sees `failed` also sees `ok` false.
                self.failed.store(true, Ordering::Release);
            }
        }
    }
}

/// The public statics as [`Outputs`].
fn public() -> Outputs<'static> {
    Outputs {
        ok: &PROBE_OK,
        failed: &FAILED,
        last_a: &LAST_A,
        peak_a: &PEAK_A,
        last_b: &LAST_B,
        peak_b: &PEAK_B,
        last_c: &LAST_C,
        peak_c: &PEAK_C,
    }
}

/// One hook call: count it in `calls`; on every 30th call (the first included), and only while
/// the probe has not failed, take a sample with `read` and publish it to `out`.
fn tick(calls: &AtomicU32, out: &Outputs<'_>, read: impl FnOnce() -> Option<[CounterSample; 3]>) {
    if out.failed.load(Ordering::Relaxed) {
        return;
    }
    // `fetch_add` wraps; a wrap only shifts the rhythm once.
    if calls
        .fetch_add(1, Ordering::Relaxed)
        .is_multiple_of(SAMPLE_EVERY)
    {
        out.publish(read());
    }
}

/// The hook's sample: [`tick`] on the public statics, reading the chain from `pass`.
fn sample(pass: usize) {
    #[cfg(test)]
    if PANIC_IN_SAMPLE.load(Ordering::Relaxed) {
        panic!("PANIC_IN_SAMPLE is set");
    }
    // SAFETY: `is_readable` checks each range with VirtualQuery. The chain's objects belong to
    // the grass pass that is running this very call, so the game does not free them meanwhile.
    tick(&CALLS, &public(), || unsafe {
        read_counters(pass, is_readable)
    });
}

/// The detour's shape: `sample` in a guard of its own, then `original` exactly once.
///
/// - `Feature::ScatterDistance` off: only `original` runs (the outer guard's fallback).
/// - `sample` panics: the inner guard catches it and switches the feature off; then `original`
///   runs. The outer guard sees no panic, so it does not run `original` a second time.
///
/// `original` itself never panics (it only calls the trampoline), so the outer fallback can only
/// run when the feature is already off.
fn run_before_original(sample: impl FnOnce(), original: impl Fn()) {
    guard(Feature::ScatterDistance, &original, || {
        // A panic in the sample stops in this inner guard, whose fallback does nothing. So the
        // outer guard never sees one and never runs the original a second time.
        guard(Feature::ScatterDistance, || (), sample);
        original();
    });
}

/// Call the game function through `trampoline`; nothing if it is 0 (it cannot be: [`install`]
/// stores it before the hook is enabled).
///
/// # Safety
///
/// `trampoline` is 0 or MinHook's trampoline for the game function, and the arguments are the
/// ones the game passed.
unsafe fn call_original(trampoline: usize, rcx: *mut u8, pass: *const u8, f: f32) {
    if trampoline == 0 {
        return;
    }
    // SAFETY: non-zero, so MinHook's trampoline, which runs the game function: of this type.
    let original = unsafe { std::mem::transmute::<usize, ScatterFrameFn>(trampoline) };
    // SAFETY: the game's own arguments, passed on unchanged.
    unsafe { original(rcx, pass, f) }
}

/// Stands in for the game's grass pass function: samples the counters (every 30th call), then
/// runs the original with the same arguments. See the module docs.
///
/// It runs on the game's render path, so it only touches atomics and `VirtualQuery`: no lock, no
/// allocation, no log.
///
/// # Safety
///
/// The game's contract for the function: `rcx` and `pass` are what the game passes, and `f` is
/// the float in XMM2.
pub unsafe extern "system" fn scatter_frame_detour(rcx: *mut u8, pass: *const u8, f: f32) {
    let trampoline = TRAMPOLINE.load(Ordering::Acquire);
    // SAFETY: `trampoline` is 0 or the trampoline for the game function; the arguments are the
    // game's own, passed on unchanged.
    let original = move || unsafe { call_original(trampoline, rcx, pass, f) };
    run_before_original(move || sample(pass as usize), original);
}

const _: ScatterFrameFn = scatter_frame_detour;

/// Create the hook on `target` with [`scatter_frame_detour`] and queue it; store the trampoline
/// for the detour before anything can enable the hook. The part of [`install`] that a test can
/// run on a stand-in function.
///
/// # Safety
///
/// The contract of `create_and_queue` for `target`, whose type is [`ScatterFrameFn`].
unsafe fn hook_at(target: usize) -> Result<(), String> {
    let detour = scatter_frame_detour as *const () as usize;
    // SAFETY: the caller's contract; the detour has the target's type (`const _` above).
    let trampoline = unsafe { create_and_queue(target, detour) }?;
    TRAMPOLINE.store(trampoline, Ordering::Release);
    Ok(())
}

/// Find `SCATTER_FRAME_FN` in `m` and create the pre-hook on it, queued;
/// `knobs::enable_queued_hooks` enables it. `Err` is the reason for the log.
///
/// # Safety
///
/// Start-up only: call it from `entry::start` (in `InitializeASI`, before the game's `WinMain`),
/// on one thread, with `m` the running exe (`scan::main_module()`), like
/// `scan::find_in_sections`.
pub unsafe fn install(m: &Module) -> Result<(), String> {
    let name = SCATTER_FRAME_FN.name;
    let pattern =
        Pattern::parse(SCATTER_FRAME_FN.pattern).map_err(|error| format!("{name}: {error}"))?;
    let found = find_in_sections(m, SCATTER_FRAME_FN.sections, &pattern).map_err(|error| {
        log::debug!("scatter probe: {name}: {error}");
        error.player_text()
    })?;
    let target = found
        .checked_add(SCATTER_FRAME_FN.patch_offset())
        .ok_or_else(|| format!("{name}: the hook site is past the end of memory"))?;
    log::debug!("scatter probe: {name} matched at {target:#x}");
    // SAFETY: `target` is the entry of the game's grass pass function: the one match in `.text`
    // of its 14-byte prologue (sigs.rs; patch offset 0). Its first instruction is exactly 5 bytes
    // (`mov [rsp+0x10],rdx`), not RIP-relative, and no branch in the function lands inside it
    // (lowest target 0x1400767f9). Its type is `ScatterFrameFn` (docs/re-notes.md, "Scatter
    // counter probe"). Start-up only: this function's contract.
    unsafe { hook_at(target) }
}

/// Is the probe wanted? With `EnableScatterDistance = 1` (the governor's headroom rule needs
/// it), or with `[Debug] ScatterProbe = 1` and `LogLevel = debug` (design.md section 8: the
/// `[Debug]` keys count only with `LogLevel = debug`).
fn wanted(cfg: &Config) -> bool {
    cfg.knobs.enable_scatter_distance
        || (cfg.debug.scatter_probe && cfg.general.log_level == LogLevel::Debug)
}

/// The body of [`init`], with the decision, the code-patch answer and the install passed in.
/// `install` runs only when code patches are allowed and the probe is wanted. Returns the log
/// level and the one line to log.
fn start_up(
    wanted: bool,
    code_patches: bool,
    install: impl FnOnce() -> Result<(), String>,
) -> (Level, String) {
    if !code_patches {
        return (
            Level::Info,
            "scatter probe: off: code patches are off on this build".to_owned(),
        );
    }
    if !wanted {
        return (
            Level::Info,
            "scatter probe: off: EnableScatterDistance = 0, and no [Debug] ScatterProbe = 1 with \
             LogLevel = debug"
                .to_owned(),
        );
    }
    match install() {
        Ok(()) => (
            Level::Info,
            "scatter probe: on (grass pass hook queued; it reads the GPU counters every 30th call)"
                .to_owned(),
        ),
        Err(why) => (Level::Warn, format!("scatter probe: off: {why}")),
    }
}

/// The start-up step: decide, install the hook (queued) if it is right to, and log one line.
/// Called once, from `entry::start`, after `code_patches_allowed` is set and before
/// `knobs::enable_queued_hooks`.
pub fn init(cfg: &Config) {
    let (level, line) = start_up(wanted(cfg), entry::code_patches_allowed(), || {
        // SAFETY: `init` runs from `entry::start`, in `InitializeASI`, before the game's
        // `WinMain`: one thread. `main_module()` is the running exe.
        unsafe { install(&main_module()) }
    });
    log::log!(level, "{line}");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::guard::is_disabled;
    use crate::knobs::{apply_queued, SCATTER_STEPS};
    use crate::patch::test_support::Mem;
    use crate::scan::find_unique;
    use minhook::MinHook;
    use std::cell::{Cell, RefCell};
    use std::ffi::OsStr;
    use std::process::Command;
    use std::sync::atomic::Ordering::Relaxed;
    use windows_sys::Win32::System::Memory::{
        MEM_COMMIT, MEM_RESERVE, PAGE_GUARD, PAGE_NOACCESS, PAGE_READONLY, PAGE_READWRITE,
    };

    /// One page.
    const PAGE: usize = 0x1000;

    // ---- the headroom rule and the ceiling ----------------------------------------------------

    #[test]
    fn headroom_ok_scales_the_last_value_by_the_area() {
        // 0x7800 × (100 / 90)² = 37926 < 0xf000 = 61440.
        assert!(headroom_ok(0xf000 / 2, 90.0, 100.0));
        // 0xe000 × (100 / 90)² = 70795 > 61440.
        assert!(!headroom_ok(0xe000, 90.0, 100.0));
        // Staying at the same distance only needs last < warn.
        assert!(headroom_ok(WARN_A - 1, 90.0, 90.0));
        assert!(!headroom_ok(WARN_A, 90.0, 90.0));
    }

    #[test]
    fn headroom_ok_is_false_for_a_bad_current_distance_or_a_nan() {
        assert!(!headroom_ok(1, 0.0, 100.0));
        assert!(!headroom_ok(1, -90.0, 100.0));
        assert!(!headroom_ok(1, f32::NAN, 100.0));
        assert!(!headroom_ok(1, f32::INFINITY, 100.0));
        assert!(!headroom_ok(1, 90.0, f32::NAN));
        assert!(!headroom_ok(1, 90.0, f32::INFINITY));
        assert!(!headroom_ok(0, 90.0, f32::INFINITY), "0 × inf is NaN");
    }

    #[test]
    fn ceiling_step_is_the_highest_step_within_r_max() {
        let steps = [90.0, 100.0, 110.0, 120.0, 127.0];
        assert_eq!(ceiling_step(&steps, 112.0), 2);
        assert_eq!(ceiling_step(&steps, 80.0), 0);
        assert_eq!(ceiling_step(&steps, 127.0), 4);
        assert_eq!(ceiling_step(&steps, 1000.0), 4);
        assert_eq!(ceiling_step(&steps, 100.0), 1);
        assert_eq!(ceiling_step(&steps, f32::NAN), 0);
        assert_eq!(ceiling_step(&[], 100.0), 0);
    }

    #[test]
    fn the_measured_ceiling_is_still_the_games_90_m_so_the_knob_stays_at_step_0() {
        assert_eq!(R_MAX_M, 90.0);
        assert_eq!(ceiling_step(&SCATTER_STEPS, R_MAX_M), 0);
        assert_eq!((WARN_A, CAP_A), (0xf000, 0x14000));
    }

    // ---- read_counters on a fake chain ----------------------------------------------------------

    /// Where each object of the fake chain sits in its buffer.
    const PASS_AT: usize = 0x000;
    const OWNER_AT: usize = 0x040;
    const POOL_AT: usize = 0x080;
    const OBJ_AT: usize = 0x100;
    const ENTRIES_AT: usize = 0x200;
    /// The handles: entry index | 0x10000, as the game makes them.
    const HANDLES: [u32; 3] = [0x1_0001, 0x1_0002, 0x1_0005];
    /// Counter A, B and C as the fake chain holds them.
    const A: CounterSample = CounterSample {
        last: 0x7800,
        peak: 0x9000,
        warn: 0xf000,
        cap: 0x14000,
    };
    const B: CounterSample = CounterSample {
        last: 0x123,
        peak: 0x456,
        warn: 0xf000,
        cap: 0x14000,
    };
    const C: CounterSample = CounterSample {
        last: 0x3000,
        peak: 0x4000,
        warn: 0x10_0000,
        cap: 0x10_0000,
    };

    /// The game's pointer chain, built in one test-owned buffer at real addresses.
    struct Chain {
        mem: Vec<u8>,
    }

    impl Chain {
        fn new() -> Chain {
            let mut chain = Chain {
                mem: vec![0u8; 0x400],
            };
            let base = chain.base() as u64;
            chain.put_u64(PASS_AT + 0x8, base + OWNER_AT as u64);
            chain.put_u64(PASS_AT + 0x18, base + OBJ_AT as u64);
            chain.put_u64(OWNER_AT + 0x20, base + POOL_AT as u64);
            chain.put_u64(POOL_AT + 0x48, base + ENTRIES_AT as u64);
            for (i, (h, s)) in HANDLES.into_iter().zip([A, B, C]).enumerate() {
                chain.put_u32(OBJ_AT + 0x8 + 4 * i, h);
                chain.put_u32(Chain::entry_at(h) + 0x38, h);
                chain.set(h, s);
            }
            chain
        }

        fn base(&self) -> usize {
            self.mem.as_ptr() as usize
        }

        fn pass(&self) -> usize {
            self.base() + PASS_AT
        }

        fn entry_at(h: u32) -> usize {
            ENTRIES_AT + (h & 0xffff) as usize * 0x40
        }

        fn set(&mut self, h: u32, s: CounterSample) {
            let e = Chain::entry_at(h);
            self.put_u32(e + 0x14, s.last);
            self.put_u32(e + 0x1c, s.peak);
            self.put_u32(e + 0x20, s.warn);
            self.put_u32(e + 0x24, s.cap);
        }

        fn put_u64(&mut self, at: usize, v: u64) {
            self.mem[at..at + 8].copy_from_slice(&v.to_le_bytes());
        }

        fn put_u32(&mut self, at: usize, v: u32) {
            self.mem[at..at + 4].copy_from_slice(&v.to_le_bytes());
        }

        /// Every `(address, length)` the reader must check, in order, for a good chain.
        fn expected_checks(&self) -> Vec<(usize, usize)> {
            let b = self.base();
            let mut v = vec![
                (b + PASS_AT + 0x18, 8),
                (b + PASS_AT + 0x8, 8),
                (b + OWNER_AT + 0x20, 8),
                (b + POOL_AT + 0x48, 8),
            ];
            for (i, h) in HANDLES.into_iter().enumerate() {
                let e = b + Chain::entry_at(h);
                v.push((b + OBJ_AT + 0x8 + 4 * i, 4));
                for field in [0x38, 0x14, 0x1c, 0x20, 0x24] {
                    v.push((e + field, 4));
                }
            }
            v
        }
    }

    #[test]
    fn a_good_chain_gives_all_three_counters() {
        let chain = Chain::new();
        // SAFETY: `is_readable` checks every read, and the chain's buffer lives to the end.
        let got = unsafe { read_counters(chain.pass(), is_readable) };
        assert_eq!(got, Some([A, B, C]));
    }

    #[test]
    fn every_read_is_checked_first_with_its_address_and_length() {
        let chain = Chain::new();
        let asked = RefCell::new(Vec::new());
        // SAFETY: the closure allows only addresses inside the chain's live buffer (asserted).
        let got = unsafe {
            read_counters(chain.pass(), |addr, len| {
                assert!(addr >= chain.base() && addr + len <= chain.base() + chain.mem.len());
                asked.borrow_mut().push((addr, len));
                true
            })
        };
        assert_eq!(got, Some([A, B, C]));
        assert_eq!(*asked.borrow(), chain.expected_checks());
    }

    #[test]
    fn a_refused_check_gives_none_and_that_link_and_everything_after_it_are_never_read() {
        let chain = Chain::new();
        let all = chain.expected_checks();
        assert_eq!(all.len(), 22);
        for (i, &refused) in all.iter().enumerate() {
            let asked = RefCell::new(Vec::new());
            // SAFETY: as above; the one refused address is never read (that is the point).
            let got = unsafe {
                read_counters(chain.pass(), |addr, len| {
                    asked.borrow_mut().push((addr, len));
                    (addr, len) != refused
                })
            };
            // Every link holds a good value, so reading the refused one anyway would succeed.
            assert_eq!(got, None, "refusing check {i} {refused:x?}");
            assert_eq!(*asked.borrow(), all[..=i], "refusing check {i}");
        }
    }

    #[test]
    fn the_self_check_needs_counter_a_warn_and_cap() {
        let mut chain = Chain::new();
        chain.set(HANDLES[0], CounterSample { warn: 0xe000, ..A });
        // SAFETY: `is_readable` checks every read; the buffer is live.
        assert_eq!(unsafe { read_counters(chain.pass(), is_readable) }, None);
        chain.set(HANDLES[0], CounterSample { cap: 0x28000, ..A });
        // SAFETY: as above.
        assert_eq!(unsafe { read_counters(chain.pass(), is_readable) }, None);
        // B and C are not part of the self-check.
        chain.set(HANDLES[0], A);
        chain.set(
            HANDLES[1],
            CounterSample {
                warn: 1,
                cap: 2,
                ..B
            },
        );
        // SAFETY: as above.
        let got = unsafe { read_counters(chain.pass(), is_readable) };
        assert_eq!(got.map(|[a, _, _]| a), Some(A));
    }

    #[test]
    fn a_handle_that_does_not_own_its_entry_or_is_0_gives_none() {
        for (i, h) in HANDLES.into_iter().enumerate() {
            let mut chain = Chain::new();
            chain.put_u32(Chain::entry_at(h) + 0x38, h ^ 0x2_0000);
            // SAFETY: `is_readable` checks every read; the buffer is live.
            assert_eq!(
                unsafe { read_counters(chain.pass(), is_readable) },
                None,
                "{i}"
            );

            // Handle 0, with an entry 0 whose owner field is 0 too.
            let mut chain = Chain::new();
            chain.put_u32(OBJ_AT + 0x8 + 4 * i, 0);
            // SAFETY: as above.
            assert_eq!(
                unsafe { read_counters(chain.pass(), is_readable) },
                None,
                "{i}"
            );
        }
    }

    #[test]
    fn unreadable_memory_in_the_chain_gives_none_without_a_fault() {
        let none = Mem::new(PAGE, MEM_RESERVE | MEM_COMMIT, PAGE_NOACCESS);
        // SAFETY: `is_readable` refuses the no-access page before any read.
        assert_eq!(unsafe { read_counters(none.base, is_readable) }, None);
        let mut chain = Chain::new();
        chain.put_u64(PASS_AT + 0x18, none.base as u64);
        // SAFETY: as above.
        assert_eq!(unsafe { read_counters(chain.pass(), is_readable) }, None);
        let mut chain = Chain::new();
        chain.put_u64(POOL_AT + 0x48, 0);
        // SAFETY: as above; address 0 is never committed.
        assert_eq!(unsafe { read_counters(chain.pass(), is_readable) }, None);
        // SAFETY: as above.
        assert_eq!(unsafe { read_counters(0, is_readable) }, None);
        // SAFETY: as above; the address arithmetic must not overflow.
        assert_eq!(unsafe { read_counters(usize::MAX - 4, |_, _| true) }, None);
    }

    // ---- is_readable --------------------------------------------------------------------------

    #[test]
    fn is_readable_follows_the_page_protection() {
        let none = Mem::new(PAGE, MEM_RESERVE | MEM_COMMIT, PAGE_NOACCESS);
        assert!(!is_readable(none.base, 4));
        let rw = Mem::new(PAGE, MEM_RESERVE | MEM_COMMIT, PAGE_READWRITE);
        assert!(is_readable(rw.base, 4));
        assert!(is_readable(rw.base, PAGE));
        assert!(is_readable(rw.base + PAGE - 4, 4));
        let ro = Mem::new(PAGE, MEM_RESERVE | MEM_COMMIT, PAGE_READONLY);
        assert!(is_readable(ro.base + 8, 8));
        let guard = Mem::new(PAGE, MEM_RESERVE | MEM_COMMIT, PAGE_READWRITE | PAGE_GUARD);
        assert!(!is_readable(guard.base, 4));
        let reserved = Mem::new(PAGE, MEM_RESERVE, PAGE_READWRITE);
        assert!(!is_readable(reserved.base, 4));
    }

    #[test]
    fn is_readable_needs_the_whole_range_in_one_readable_region() {
        let two = Mem::new(2 * PAGE, MEM_RESERVE | MEM_COMMIT, PAGE_READWRITE);
        assert!(
            is_readable(two.base + PAGE - 4, 8),
            "two like pages are one region"
        );
        two.protect(PAGE, PAGE, PAGE_NOACCESS);
        assert!(is_readable(two.base + PAGE - 4, 4));
        assert!(
            !is_readable(two.base + PAGE - 4, 8),
            "runs into the no-access page"
        );
        assert!(!is_readable(two.base + PAGE, 4));
    }

    #[test]
    fn is_readable_refuses_null_and_an_overflowing_range() {
        assert!(!is_readable(0, 8));
        let rw = Mem::new(PAGE, MEM_RESERVE | MEM_COMMIT, PAGE_READWRITE);
        assert!(!is_readable(rw.base, usize::MAX));
        assert!(!is_readable(usize::MAX - 2, 8));
    }

    // ---- publishing and the 30-call rhythm ------------------------------------------------------

    /// A test's own set of output atomics.
    #[derive(Default)]
    struct Own {
        ok: AtomicBool,
        failed: AtomicBool,
        vals: [AtomicU32; 6],
    }

    impl Own {
        fn outputs(&self) -> Outputs<'_> {
            Outputs {
                ok: &self.ok,
                failed: &self.failed,
                last_a: &self.vals[0],
                peak_a: &self.vals[1],
                last_b: &self.vals[2],
                peak_b: &self.vals[3],
                last_c: &self.vals[4],
                peak_c: &self.vals[5],
            }
        }

        fn vals(&self) -> [u32; 6] {
            self.vals.each_ref().map(|v| v.load(Relaxed))
        }
    }

    #[test]
    fn a_good_sample_publishes_the_values_and_ok() {
        let own = Own::default();
        own.outputs().publish(Some([A, B, C]));
        assert!(own.ok.load(Relaxed));
        assert!(!own.failed.load(Relaxed));
        assert_eq!(own.vals(), [A.last, A.peak, B.last, B.peak, C.last, C.peak]);
        // A newer good sample replaces the values.
        let a2 = CounterSample { last: 0x100, ..A };
        own.outputs().publish(Some([a2, B, C]));
        assert_eq!(own.vals()[0], 0x100);
        assert!(own.ok.load(Relaxed));
    }

    #[test]
    fn a_failure_clears_ok_sets_failed_and_is_for_good() {
        let own = Own::default();
        own.outputs().publish(Some([A, B, C]));
        own.outputs().publish(None);
        assert!(!own.ok.load(Relaxed));
        assert!(own.failed.load(Relaxed));
        let before = own.vals();
        own.outputs()
            .publish(Some([CounterSample { last: 1, ..A }, B, C]));
        assert!(!own.ok.load(Relaxed), "never ok again after a failure");
        assert!(own.failed.load(Relaxed));
        assert_eq!(own.vals(), before, "nothing changes after a failure");
    }

    #[test]
    fn a_failure_before_any_good_sample_leaves_ok_false() {
        let own = Own::default();
        own.outputs().publish(None);
        assert!(!own.ok.load(Relaxed));
        assert!(own.failed.load(Relaxed));
        assert_eq!(own.vals(), [0; 6]);
    }

    #[test]
    fn tick_samples_the_first_call_and_then_every_30th() {
        let own = Own::default();
        let calls = AtomicU32::new(0);
        let reads = Cell::new(Vec::new());
        for call in 0..91u32 {
            tick(&calls, &own.outputs(), || {
                let mut r = reads.take();
                r.push(call);
                reads.set(r);
                Some([CounterSample { last: call, ..A }, B, C])
            });
        }
        assert_eq!(reads.take(), [0, 30, 60, 90]);
        assert_eq!(own.vals()[0], 90);
        assert_eq!(calls.load(Relaxed), 91);
    }

    #[test]
    fn tick_reads_nothing_more_after_a_failure() {
        let own = Own::default();
        let calls = AtomicU32::new(0);
        let reads = Cell::new(0u32);
        for _ in 0..100 {
            tick(&calls, &own.outputs(), || {
                reads.set(reads.get() + 1);
                None
            });
        }
        assert_eq!(reads.get(), 1, "only the first sample ran");
        assert!(own.failed.load(Relaxed));
        assert!(!own.ok.load(Relaxed));
    }

    #[test]
    fn the_call_counter_wraps_instead_of_panicking() {
        let own = Own::default();
        let calls = AtomicU32::new(u32::MAX);
        let reads = Cell::new(0u32);
        tick(&calls, &own.outputs(), || {
            reads.set(reads.get() + 1);
            Some([A, B, C])
        });
        assert_eq!(calls.load(Relaxed), 0);
        // u32::MAX % 30 == 15: not a sample call.
        assert_eq!(reads.get(), 0);
        tick(&calls, &own.outputs(), || {
            reads.set(reads.get() + 1);
            Some([A, B, C])
        });
        assert_eq!(reads.get(), 1, "0 is a sample call");
    }

    // ---- the start-up decision ------------------------------------------------------------------

    fn cfg(scatter_knob: bool, probe_key: bool, level: LogLevel) -> Config {
        let mut c = Config::default();
        c.knobs.enable_scatter_distance = scatter_knob;
        c.debug.scatter_probe = probe_key;
        c.general.log_level = level;
        c
    }

    #[test]
    fn the_probe_is_wanted_for_the_grass_knob_or_the_debug_key_with_debug_logging() {
        assert!(
            wanted(&Config::default()),
            "EnableScatterDistance = 1 is the default"
        );
        assert!(wanted(&cfg(true, false, LogLevel::Info)));
        assert!(wanted(&cfg(false, true, LogLevel::Debug)));
        assert!(
            !wanted(&cfg(false, true, LogLevel::Info)),
            "[Debug] needs LogLevel = debug"
        );
        assert!(!wanted(&cfg(false, false, LogLevel::Debug)));
    }

    #[test]
    fn no_code_patches_means_no_hook_and_says_so() {
        let called = Cell::new(false);
        let (level, line) = start_up(true, false, || {
            called.set(true);
            Ok(())
        });
        assert!(!called.get(), "install must not run");
        assert_eq!(level, Level::Info);
        assert_eq!(
            line,
            "scatter probe: off: code patches are off on this build"
        );
    }

    #[test]
    fn not_wanted_means_no_hook_and_says_so() {
        let called = Cell::new(false);
        let (level, line) = start_up(false, true, || {
            called.set(true);
            Ok(())
        });
        assert!(!called.get(), "install must not run");
        assert_eq!(level, Level::Info);
        assert_eq!(
            line,
            "scatter probe: off: EnableScatterDistance = 0, and no [Debug] ScatterProbe = 1 with \
             LogLevel = debug"
        );
    }

    #[test]
    fn a_wanted_probe_installs_and_says_so() {
        let called = Cell::new(false);
        let (level, line) = start_up(true, true, || {
            called.set(true);
            Ok(())
        });
        assert!(called.get());
        assert_eq!(level, Level::Info);
        assert_eq!(
            line,
            "scatter probe: on (grass pass hook queued; it reads the GPU counters every 30th call)"
        );
    }

    #[test]
    fn an_install_failure_is_a_warning_with_the_reason() {
        let (level, line) = start_up(true, true, || Err("the game code was not found".into()));
        assert_eq!(level, Level::Warn);
        assert_eq!(line, "scatter probe: off: the game code was not found");
    }

    // ---- the real hook, on a stand-in for the game function -----------------------------------

    /// How many times the fake has run.
    static FAKE_CALLS: AtomicU32 = AtomicU32::new(0);
    /// The fake's last RCX, RDX and XMM2 (as `f32` bits).
    static FAKE_RCX: AtomicUsize = AtomicUsize::new(0);
    static FAKE_RDX: AtomicUsize = AtomicUsize::new(0);
    static FAKE_F: AtomicU32 = AtomicU32::new(0);
    /// `PROBE_OK` as the fake saw it when it ran (1 or 0).
    static FAKE_SAW_OK: AtomicU32 = AtomicU32::new(0);

    /// A stand-in for the game's grass pass function: the same three arguments, no return value,
    /// and the game's first 13 bytes (`mov [rsp+0x10],rdx`, the first five pushes). Its last push
    /// is `r12` where the game pushes `r15`, so the 14-byte signature never matches the fake and
    /// `install` never finds it in this test binary. MinHook moves the same 5-byte first
    /// instruction as in the game.
    ///
    /// It records RCX, RDX, XMM2 and `PROBE_OK` (so a test can see that the sample ran before
    /// it), then counts the call.
    #[unsafe(naked)]
    unsafe extern "system" fn fake_scatter_frame(_rcx: *mut u8, _pass: *const u8, _f: f32) {
        core::arch::naked_asm!(
            "mov qword ptr [rsp + 0x10], rdx",
            "push rbp",
            "push rbx",
            "push rdi",
            "push r13",
            "push r14",
            "push r12",
            "mov qword ptr [rip + {rcx}], rcx",
            "mov qword ptr [rip + {rdx}], rdx",
            "movss dword ptr [rip + {f}], xmm2",
            "movzx eax, byte ptr [rip + {ok}]",
            "mov dword ptr [rip + {saw}], eax",
            "lock inc dword ptr [rip + {calls}]",
            "pop r12",
            "pop r14",
            "pop r13",
            "pop rdi",
            "pop rbx",
            "pop rbp",
            "ret",
            rcx = sym FAKE_RCX,
            rdx = sym FAKE_RDX,
            f = sym FAKE_F,
            ok = sym PROBE_OK,
            saw = sym FAKE_SAW_OK,
            calls = sym FAKE_CALLS,
        );
    }

    /// Call the fake through its address, as the game's caller does.
    fn call_fake(rcx: usize, pass: usize, f: f32) {
        let fake: ScatterFrameFn = std::hint::black_box(fake_scatter_frame);
        // SAFETY: the fake only records its arguments; the hook (if any) reads `pass` only after
        // `is_readable` accepted each address.
        unsafe { fake(rcx as *mut u8, pass as *const u8, f) };
    }

    #[test]
    fn the_fake_starts_like_the_game_function_but_never_matches_the_signature() {
        let at = fake_scatter_frame as *const () as *const u8;
        // SAFETY: the fake is a function of this binary, in readable `.text`, longer than 14
        // bytes.
        let bytes = unsafe { std::slice::from_raw_parts(at, 14) };
        let tokens: Vec<&str> = SCATTER_FRAME_FN.pattern.split_ascii_whitespace().collect();
        let head = Pattern::parse(&tokens[..13].join(" ")).unwrap();
        assert_eq!(find_unique(&bytes[..13], &head), Ok(0), "{bytes:02x?}");
        let full = Pattern::parse(SCATTER_FRAME_FN.pattern).unwrap();
        assert!(find_unique(bytes, &full).is_err(), "{bytes:02x?}");
    }

    /// Names the test that a child process runs. Unset in the parent.
    const CHILD_ENV: &str = "ENSHROUDED_HEADROOM_PROBE_CHILD";

    /// Run `scenario` in a fresh copy of this test binary, alone. There no test has switched
    /// `Feature::ScatterDistance` off yet, and no hook is written into a process where other
    /// tests run. `name` is the calling test's name.
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

    /// Hook the fake through the code path `install` uses, and enable it through the one that
    /// `entry::start` uses.
    fn hook_the_fake() {
        let target = fake_scatter_frame as *const () as usize;
        // SAFETY: `target` is the fake: a function in executable memory, longer than 5 bytes,
        // whose first instruction is 5 bytes and which nothing jumps into; the detour has its
        // type.
        unsafe { hook_at(target) }.unwrap();
        assert_eq!(apply_queued(), Ok(1));
    }

    /// Take the hook off the fake again.
    fn unhook_the_fake() {
        let target = fake_scatter_frame as *const () as usize;
        // SAFETY: the hook at `target` was created by `hook_the_fake` and nothing runs it now.
        unsafe { MinHook::remove_hook(target as *mut c_void) }.unwrap();
    }

    #[test]
    fn real_hook_samples_before_the_original_and_passes_every_argument_on() {
        in_child(
            "real_hook_samples_before_the_original_and_passes_every_argument_on",
            || {
                assert!(!is_disabled(Feature::ScatterDistance), "a fresh process");
                let mut chain = Chain::new();
                // Unhooked, the fake records its arguments and nothing is published.
                call_fake(0x1234, chain.pass(), 1.5);
                assert_eq!(FAKE_CALLS.load(Relaxed), 1);
                assert!(!PROBE_OK.load(Relaxed));

                hook_the_fake();
                call_fake(0x5678, chain.pass(), 2.25);
                assert_eq!(FAKE_CALLS.load(Relaxed), 2, "the original ran exactly once");
                assert_eq!(FAKE_RCX.load(Relaxed), 0x5678, "RCX passed on");
                assert_eq!(FAKE_RDX.load(Relaxed), chain.pass(), "RDX passed on");
                assert_eq!(f32::from_bits(FAKE_F.load(Relaxed)), 2.25, "XMM2 passed on");
                assert_eq!(
                    FAKE_SAW_OK.load(Relaxed),
                    1,
                    "the sample ran before the original"
                );
                assert!(PROBE_OK.load(Relaxed));
                assert!(!FAILED.load(Relaxed));
                let published =
                    [&LAST_A, &PEAK_A, &LAST_B, &PEAK_B, &LAST_C, &PEAK_C].map(|v| v.load(Relaxed));
                assert_eq!(published, [A.last, A.peak, B.last, B.peak, C.last, C.peak]);

                // The next 29 calls do not sample; the 30th after the first does.
                chain.set(HANDLES[0], CounterSample { last: 0x8000, ..A });
                for _ in 0..29 {
                    call_fake(0x5678, chain.pass(), 2.25);
                }
                assert_eq!(FAKE_CALLS.load(Relaxed), 31);
                assert_eq!(LAST_A.load(Relaxed), A.last, "no sample on calls 2 to 30");
                call_fake(0x5678, chain.pass(), 2.25);
                assert_eq!(FAKE_CALLS.load(Relaxed), 32);
                assert_eq!(LAST_A.load(Relaxed), 0x8000, "call 31 samples");

                // Take the hook out: the fake runs alone again.
                unhook_the_fake();
                chain.set(HANDLES[0], CounterSample { last: 0x9000, ..A });
                for _ in 0..30 {
                    call_fake(0x5678, chain.pass(), 2.25);
                }
                assert_eq!(FAKE_CALLS.load(Relaxed), 62);
                assert_eq!(LAST_A.load(Relaxed), 0x8000, "the detour did not run");
            },
        );
    }

    #[test]
    fn a_bad_chain_turns_the_probe_off_for_good_and_the_original_still_runs_once() {
        in_child(
            "a_bad_chain_turns_the_probe_off_for_good_and_the_original_still_runs_once",
            || {
                assert!(!is_disabled(Feature::ScatterDistance), "a fresh process");
                hook_the_fake();
                // `pass` points at a no-access page: the probe must refuse it, not fault.
                let none = Mem::new(PAGE, MEM_RESERVE | MEM_COMMIT, PAGE_NOACCESS);
                call_fake(1, none.base, 0.5);
                assert_eq!(FAKE_CALLS.load(Relaxed), 1, "the original ran exactly once");
                assert_eq!(FAKE_RDX.load(Relaxed), none.base);
                assert!(FAILED.load(Relaxed));
                assert!(!PROBE_OK.load(Relaxed));
                assert!(
                    !is_disabled(Feature::ScatterDistance),
                    "a failure is not a panic"
                );

                // A good chain later does not turn it back on: off for the session.
                let chain = Chain::new();
                for _ in 0..60 {
                    call_fake(1, chain.pass(), 0.5);
                }
                assert_eq!(FAKE_CALLS.load(Relaxed), 61);
                assert!(FAILED.load(Relaxed));
                assert!(!PROBE_OK.load(Relaxed));
                assert_eq!(LAST_A.load(Relaxed), 0);
                unhook_the_fake();
            },
        );
    }

    #[test]
    fn the_original_runs_exactly_once_when_the_sample_panics() {
        in_child(
            "the_original_runs_exactly_once_when_the_sample_panics",
            || {
                assert!(!is_disabled(Feature::ScatterDistance), "a fresh process");
                let order = RefCell::new(Vec::new());
                run_before_original(
                    || order.borrow_mut().push("sample"),
                    || order.borrow_mut().push("original"),
                );
                assert_eq!(*order.borrow(), ["sample", "original"]);

                order.borrow_mut().clear();
                run_before_original(
                    || {
                        order.borrow_mut().push("sample");
                        panic!("sample boom");
                    },
                    || order.borrow_mut().push("original"),
                );
                assert_eq!(
                    *order.borrow(),
                    ["sample", "original"],
                    "the original ran once, not again in a fallback"
                );
                assert!(
                    is_disabled(Feature::ScatterDistance),
                    "the panic switched the feature off"
                );

                // From now on only the original runs, still exactly once a call.
                order.borrow_mut().clear();
                run_before_original(
                    || order.borrow_mut().push("sample"),
                    || order.borrow_mut().push("original"),
                );
                assert_eq!(*order.borrow(), ["original"]);
            },
        );
    }

    #[test]
    fn the_real_detour_runs_the_original_once_when_its_sample_panics() {
        in_child(
            "the_real_detour_runs_the_original_once_when_its_sample_panics",
            || {
                assert!(!is_disabled(Feature::ScatterDistance), "a fresh process");
                let chain = Chain::new();
                hook_the_fake();
                PANIC_IN_SAMPLE.store(true, Relaxed);
                call_fake(7, chain.pass(), 3.0);
                assert_eq!(FAKE_CALLS.load(Relaxed), 1, "the original ran exactly once");
                assert_eq!(f32::from_bits(FAKE_F.load(Relaxed)), 3.0);
                assert!(
                    is_disabled(Feature::ScatterDistance),
                    "the panic switched the feature off"
                );
                assert!(!PROBE_OK.load(Relaxed), "nothing published");
                assert!(!FAILED.load(Relaxed), "a panic is not a failed sample");

                // From now on the detour only runs the original, once a call.
                PANIC_IN_SAMPLE.store(false, Relaxed);
                call_fake(7, chain.pass(), 3.0);
                assert_eq!(FAKE_CALLS.load(Relaxed), 2);
                assert!(!PROBE_OK.load(Relaxed), "still nothing published");
                unhook_the_fake();
            },
        );
    }

    #[test]
    fn install_in_this_exe_finds_nothing_and_says_why() {
        // In a child: if the signature ever matched code in this test binary, the hook it queues
        // must not reach a process where other tests enable queued hooks.
        in_child("install_in_this_exe_finds_nothing_and_says_why", || {
            // SAFETY: one thread (`--test-threads=1`); `main_module()` is this test binary, and
            // the scan finds no match in it, so nothing is hooked.
            let why = unsafe { install(&main_module()) }.unwrap_err();
            assert!(why.contains("not found"), "{why}");
            assert_eq!(apply_queued(), Ok(0), "nothing was queued");
        });
    }

    // ---- the real game exe (ignored: reads ENSHROUDED_EXE, else the Steam install) -------------

    /// Where `SCATTER_FRAME_FN` matches in build f68140a3 (the Steam exe has the same code at the
    /// same addresses; docs/re-notes.md).
    const FRAME_FN_VA: u64 = 0x1_4007_67a0;

    #[test]
    #[ignore = "reads the game exe (ENSHROUDED_EXE, else the Steam path); a skip shows only with -- --ignored --nocapture"]
    fn scatter_frame_fn_is_unique_in_game_exe_at_its_anchor() {
        use crate::build_id::test_support::game_exe_or_skip;
        use crate::build_id::{classify, sha256_file};
        use crate::scan::parse_pe_file;
        let Some(path) = game_exe_or_skip("scatter_frame_fn_is_unique_in_game_exe_at_its_anchor")
        else {
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
        let pattern = Pattern::parse(SCATTER_FRAME_FN.pattern).unwrap();
        let at = find_unique(text, &pattern).expect("the signature is in .text exactly once");
        let va = text_va + at as u64;
        println!(
            "{:<24} match {va:#x}  hook at {:#x}",
            SCATTER_FRAME_FN.name,
            va + SCATTER_FRAME_FN.patch_offset() as u64
        );
        assert_eq!(va, FRAME_FN_VA);
        // The first instruction, which MinHook moves into the trampoline: `mov [rsp+0x10],rdx`.
        let head = pe.bytes_at_va(&file, va, 5).unwrap();
        assert_eq!(head, [0x48, 0x89, 0x54, 0x24, 0x10]);
    }
}
