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
//! `guard(Feature::ScatterDistance, ...)`. At most once per 500 ms of `QueryPerformanceCounter`
//! time (the first call included), however often the grass pass runs, it follows the pointer
//! chain from `pass` with [`read_counters`], checking each block it reads once with
//! [`is_readable`] (7 checks per sample), and publishes the result to the atomics below. On every
//! other call it reads the clock and one atomic, nothing more. Then it calls the original with
//! the same arguments, exactly once. It never logs and never allocates.
//!
//! It is not installed by default: [`init`] installs it only for `[Debug] ScatterProbe = 1` with
//! `LogLevel = debug`, or for `EnableScatterDistance = 1` once the grass ceiling [`R_MAX_M`]
//! allows a raise. (An earlier version sampled every 30th call and checked every read, and it
//! was on by default; in game that caused a stutter.)
//!
//! The self-check: counter A must show the warn level `0xf000` and the cap `0x14000`. If it does
//! not, or any read is refused, the chain is wrong for this build. Then [`FAILED`] is set,
//! [`PROBE_OK`] is cleared, and the probe stops reading for the rest of the session.
//!
//! Task 15's governor consumes this module: the grass knob may rise only while [`PROBE_OK`],
//! [`ceiling_step`] and [`headroom_ok`] allow it.

use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};

use log::Level;
use windows_sys::Win32::System::Memory::{VirtualQuery, MEMORY_BASIC_INFORMATION, MEM_COMMIT};
use windows_sys::Win32::System::Performance::QueryPerformanceFrequency;

use crate::config::{Config, LogLevel};
use crate::entry;
use crate::frame_timer::qpc_now;
use crate::guard::{guard, Feature};
use crate::knobs::create_and_queue;
use crate::knobs::SCATTER_STEPS;
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

/// The hook reads the counters at most once per this many milliseconds of `QueryPerformanceCounter`
/// time, however often the grass pass runs. The first call samples.
const SAMPLE_INTERVAL_MS: u64 = 500;

/// [`SAMPLE_INTERVAL_MS`] in ticks of the usual 10 MHz `QueryPerformanceCounter`. [`hook_at`]
/// replaces it with the value for the real frequency before the hook is enabled.
const DEFAULT_INTERVAL_TICKS: u64 = 5_000_000;

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

/// The bytes of `pass` a sample reads, as (start offset, length): the pool owner pointer (+0x8)
/// through the scatter object pointer (+0x18..+0x20). One readability check covers both.
const PASS_BLOCK: (usize, usize) = (PASS_POOL_OWNER, 0x18);
/// The bytes of the scatter object a sample reads: the three handles, +0x8..+0x14.
const HANDLES_BLOCK: (usize, usize) = (0x8, 0xc);
/// The bytes of a pool entry a sample reads: last (+0x14) through the owner handle (+0x38..+0x3c).
const ENTRY_BLOCK: (usize, usize) = (ENTRY_LAST, 0x28);

/// Is the field of `size` bytes at offset `off` inside `block`? For the compile-time checks below.
const fn covers(block: (usize, usize), off: usize, size: usize) -> bool {
    match (off.checked_add(size), block.0.checked_add(block.1)) {
        (Some(field_end), Some(block_end)) => block.0 <= off && field_end <= block_end,
        _ => false,
    }
}
const _: () =
    assert!(covers(PASS_BLOCK, PASS_POOL_OWNER, 8) && covers(PASS_BLOCK, PASS_SCATTER_OBJ, 8));
const _: () = assert!(
    covers(HANDLES_BLOCK, COUNTER_HANDLES[0], 4)
        && covers(HANDLES_BLOCK, COUNTER_HANDLES[1], 4)
        && covers(HANDLES_BLOCK, COUNTER_HANDLES[2], 4)
);
const _: () = assert!(
    covers(ENTRY_BLOCK, ENTRY_LAST, 4)
        && covers(ENTRY_BLOCK, ENTRY_PEAK, 4)
        && covers(ENTRY_BLOCK, ENTRY_WARN, 4)
        && covers(ENTRY_BLOCK, ENTRY_CAP, 4)
        && covers(ENTRY_BLOCK, ENTRY_HANDLE, 4)
        && covers((0, ENTRY_SIZE), ENTRY_BLOCK.0, ENTRY_BLOCK.1)
);

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

/// The `QueryPerformanceCounter` time from which the next sample is due; 0 at start, so the first
/// call samples. A call claims a sample by moving it on with a compare-exchange.
static NEXT_DUE: AtomicU64 = AtomicU64::new(0);

/// [`SAMPLE_INTERVAL_MS`] in `QueryPerformanceCounter` ticks. Set by [`hook_at`] before the hook
/// is enabled; read only when a sample is due.
static INTERVAL_TICKS: AtomicU64 = AtomicU64::new(DEFAULT_INTERVAL_TICKS);

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

/// A range of memory that a `readable` check accepted. The probe reads only through it, and only
/// inside it, so it never reads a byte that was not checked.
struct Checked {
    /// The first byte of the range.
    addr: usize,
    /// The length of the range in bytes.
    len: usize,
}

impl Checked {
    /// Ask `readable` about `len` bytes at `addr`; the range if it says yes.
    fn new(addr: usize, len: usize, readable: &impl Fn(usize, usize) -> bool) -> Option<Checked> {
        readable(addr, len).then_some(Checked { addr, len })
    }

    /// The address of the `size` bytes at `off` in the range, or `None` if they do not all lie
    /// in it.
    fn place(&self, off: usize, size: usize) -> Option<usize> {
        let end = off.checked_add(size)?;
        if end > self.len {
            return None;
        }
        self.addr.checked_add(off)
    }

    /// The `u32` at `off` in the range, or `None` (and no read) if it does not fit in it.
    ///
    /// # Safety
    ///
    /// The `readable` check that made this range is still true (the contract of
    /// [`read_counters`]).
    unsafe fn u32_at(&self, off: usize) -> Option<u32> {
        let at = self.place(off, size_of::<u32>())?;
        // SAFETY: the 4 bytes lie in the range that `readable` accepted (`place`), and that is
        // still true (this function's contract).
        Some(unsafe { std::ptr::read_unaligned(at as *const u32) })
    }

    /// The `u64` at `off` in the range as an address, or `None` (and no read) if it does not fit
    /// in it.
    ///
    /// # Safety
    ///
    /// As for [`Checked::u32_at`].
    unsafe fn ptr_at(&self, off: usize) -> Option<usize> {
        let at = self.place(off, size_of::<u64>())?;
        // SAFETY: as in `u32_at`, for 8 bytes.
        let value = unsafe { std::ptr::read_unaligned(at as *const u64) };
        usize::try_from(value).ok()
    }
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
/// It checks each block it reads once, with `readable(addr, len)`, before it reads from it, and
/// reads only inside a checked block (`Checked`); it reads nothing after a refusal. The blocks,
/// 7 per sample: `pass` +0x8..+0x20, owner +0x20..+0x28, pool +0x48..+0x50, obj +0x8..+0x14, and
/// for each counter its entry +0x14..+0x3c. It reads with `read_unaligned`, so it needs no
/// alignment.
///
/// # Safety
///
/// `readable(addr, len)` returns true only if the `len` bytes at `addr` can be read when the
/// reads that follow the call happen ([`is_readable`], for memory that no other thread frees
/// meanwhile).
pub unsafe fn read_counters(
    pass: usize,
    readable: impl Fn(usize, usize) -> bool,
) -> Option<[CounterSample; 3]> {
    // Read the field at object offset `off` from a block that starts at object offset `start`.
    // SAFETY (both helpers): the block was accepted by `readable` just before (`Checked`),
    // and this function's contract keeps that true for the reads.
    let ptr_in = |block: &Checked, start: usize, off: usize| unsafe {
        block.ptr_at(off.checked_sub(start)?)
    };
    let u32_in = |block: &Checked, start: usize, off: usize| unsafe {
        block.u32_at(off.checked_sub(start)?)
    };
    let (pass_start, pass_len) = PASS_BLOCK;
    let pass_block = Checked::new(pass.checked_add(pass_start)?, pass_len, &readable)?;
    let owner = ptr_in(&pass_block, pass_start, PASS_POOL_OWNER)?;
    let obj = ptr_in(&pass_block, pass_start, PASS_SCATTER_OBJ)?;
    let owner_block = Checked::new(owner.checked_add(OWNER_POOL)?, size_of::<u64>(), &readable)?;
    let pool = ptr_in(&owner_block, OWNER_POOL, OWNER_POOL)?;
    let pool_block = Checked::new(pool.checked_add(POOL_ENTRIES)?, size_of::<u64>(), &readable)?;
    let entries = ptr_in(&pool_block, POOL_ENTRIES, POOL_ENTRIES)?;
    let (handles_start, handles_len) = HANDLES_BLOCK;
    let handles = Checked::new(obj.checked_add(handles_start)?, handles_len, &readable)?;
    let (entry_start, entry_len) = ENTRY_BLOCK;
    let counter = |handle_at: usize| -> Option<CounterSample> {
        let handle = u32_in(&handles, handles_start, handle_at)?;
        if handle == 0 {
            return None;
        }
        let index = usize::try_from(handle & HANDLE_INDEX_MASK).ok()?;
        let entry = entries.checked_add(index.checked_mul(ENTRY_SIZE)?)?;
        let block = Checked::new(entry.checked_add(entry_start)?, entry_len, &readable)?;
        // The game's own accessor makes this check (docs/re-notes.md): the entry must belong to
        // this handle.
        if u32_in(&block, entry_start, ENTRY_HANDLE)? != handle {
            return None;
        }
        Some(CounterSample {
            last: u32_in(&block, entry_start, ENTRY_LAST)?,
            peak: u32_in(&block, entry_start, ENTRY_PEAK)?,
            warn: u32_in(&block, entry_start, ENTRY_WARN)?,
            cap: u32_in(&block, entry_start, ENTRY_CAP)?,
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

/// [`SAMPLE_INTERVAL_MS`] in ticks of a counter with this `frequency` (ticks per second). Never 0,
/// so a due sample always moves the due time on.
fn interval_ticks(frequency: u64) -> u64 {
    // In u128, so no frequency can overflow.
    let ticks = u128::from(frequency) * u128::from(SAMPLE_INTERVAL_MS) / 1000;
    u64::try_from(ticks).unwrap_or(u64::MAX).max(1)
}

/// One hook call at `QueryPerformanceCounter` time `now`. When a sample is due (`now` has reached
/// `next_due`), claim it by moving `next_due` on by `interval` with a compare-exchange; only the
/// call whose exchange succeeds samples, so two threads never take the same sample. Then, while
/// the probe has not failed, take a sample with `read` and publish it to `out`.
///
/// When no sample is due, it loads `next_due` and returns: nothing else.
fn tick(
    now: u64,
    next_due: &AtomicU64,
    interval: &AtomicU64,
    out: &Outputs<'_>,
    read: impl FnOnce() -> Option<[CounterSample; 3]>,
) {
    let due = next_due.load(Ordering::Relaxed);
    if now < due {
        return;
    }
    // Claim this sample. A call that loses the exchange (another thread claimed it, or moved it
    // on) does nothing.
    let next = now.saturating_add(interval.load(Ordering::Relaxed));
    if next_due
        .compare_exchange(due, next, Ordering::Relaxed, Ordering::Relaxed)
        .is_err()
    {
        return;
    }
    if out.failed.load(Ordering::Relaxed) {
        return;
    }
    out.publish(read());
}

/// The hook's sample: [`tick`] on the public statics at the current time, reading the chain from
/// `pass`.
fn sample(pass: usize) {
    #[cfg(test)]
    if PANIC_IN_SAMPLE.load(Ordering::Relaxed) {
        panic!("PANIC_IN_SAMPLE is set");
    }
    // SAFETY: `is_readable` checks each range with VirtualQuery. The chain's objects belong to
    // the grass pass that is running this very call, so the game does not free them meanwhile.
    tick(
        qpc_now(),
        &NEXT_DUE,
        &INTERVAL_TICKS,
        &public(),
        || unsafe { read_counters(pass, is_readable) },
    );
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

/// Stands in for the game's grass pass function: samples the counters (at most once per 500 ms),
/// then runs the original with the same arguments. See the module docs.
///
/// It runs on the game's render path, so it only touches the clock, atomics and (in a sample)
/// `VirtualQuery`: no lock, no allocation, no log.
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
    INTERVAL_TICKS.store(interval_ticks(qpc_frequency()), Ordering::Release);
    Ok(())
}

/// `QueryPerformanceFrequency`, or 0 if Windows reports none (it does since Windows XP).
fn qpc_frequency() -> u64 {
    let mut frequency = 0i64;
    // SAFETY: `frequency` is a valid place for the one i64 the call writes; on failure it stays 0.
    unsafe { QueryPerformanceFrequency(&mut frequency) };
    u64::try_from(frequency).unwrap_or(0)
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

/// Is the probe wanted? [`wanted_with`] the ceiling [`R_MAX_M`]. While that is still the game's
/// 90 m, only the debug key installs the probe, so a default install has no probe hook.
fn wanted(cfg: &Config) -> bool {
    wanted_with(cfg, R_MAX_M)
}

/// Is the probe wanted with the grass ceiling `r_max`? With `[Debug] ScatterProbe = 1` and
/// `LogLevel = debug` (design.md section 8: the `[Debug]` keys count only with `LogLevel =
/// debug`), or with `EnableScatterDistance = 1` when `r_max` allows at least one raise (the
/// governor's headroom rule is the only other consumer, and it needs a step above 90 m).
fn wanted_with(cfg: &Config, r_max: f32) -> bool {
    let debug_key = cfg.debug.scatter_probe && cfg.general.log_level == LogLevel::Debug;
    let knob_can_rise =
        cfg.knobs.enable_scatter_distance && ceiling_step(&SCATTER_STEPS, r_max) > 0;
    debug_key || knob_can_rise
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
            "scatter probe: off: no consumer (the grass ceiling r_max is not measured yet, and no \
             [Debug] ScatterProbe = 1 with LogLevel = debug)"
                .to_owned(),
        );
    }
    match install() {
        Ok(()) => (
            Level::Info,
            "scatter probe: on (grass pass hook queued; it reads the GPU counters at most every \
             500 ms)"
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

        /// Every `(address, length)` the reader must check, in order, for a good chain: one
        /// block per object, not one check per read (ruling 3 of the stutter fix).
        fn expected_checks(&self) -> Vec<(usize, usize)> {
            let b = self.base();
            let mut v = vec![
                // pass+0x8 (owner) to pass+0x20 (obj ends): both pointers.
                (b + PASS_AT + 0x8, 0x18),
                (b + OWNER_AT + 0x20, 8),
                (b + POOL_AT + 0x48, 8),
                // The three handles, obj+0x8 to obj+0x14.
                (b + OBJ_AT + 0x8, 0xc),
            ];
            for h in HANDLES {
                // last (+0x14) to the owner handle (+0x38..+0x3c).
                v.push((b + Chain::entry_at(h) + 0x14, 0x28));
            }
            v
        }
    }

    #[test]
    fn a_sample_checks_7_blocks_instead_of_22_single_reads() {
        let chain = Chain::new();
        let asked = Cell::new(0u32);
        // SAFETY: every address the chain holds lies in its live buffer.
        let got = unsafe {
            read_counters(chain.pass(), |_, _| {
                asked.set(asked.get() + 1);
                true
            })
        };
        assert_eq!(got, Some([A, B, C]));
        assert_eq!(asked.get(), 7);
    }

    #[test]
    fn a_checked_range_never_reads_past_its_end() {
        let bytes: [u8; 12] = [1, 0, 0, 0, 2, 0, 0, 0, 3, 0, 0, 0];
        let at = bytes.as_ptr() as usize;
        let asked = RefCell::new(Vec::new());
        let record = |addr, len| {
            asked.borrow_mut().push((addr, len));
            true
        };
        let range = Checked::new(at, 12, &record).expect("accepted");
        assert_eq!(*asked.borrow(), [(at, 12)]);
        // SAFETY: the range is `bytes`, which lives to the end of the test.
        unsafe {
            assert_eq!(range.u32_at(0), Some(1));
            assert_eq!(range.u32_at(8), Some(3));
            assert_eq!(range.u32_at(9), None, "would end past the range");
            assert_eq!(range.u32_at(usize::MAX), None);
            assert_eq!(range.ptr_at(0), Some(0x0000_0002_0000_0001));
            assert_eq!(range.ptr_at(4), Some(0x0000_0003_0000_0002));
            assert_eq!(range.ptr_at(5), None, "would end past the range");
        }
        let refuse = |_, _| false;
        assert!(Checked::new(at, 12, &refuse).is_none());
    }

    /// A chain whose every checked block ends exactly at the end of a readable page, with a
    /// no-access page right after it: a read past any block faults, and fails the test.
    #[test]
    fn reads_stay_inside_the_checked_blocks_at_a_page_edge() {
        let pages: Vec<Mem> = (0..5)
            .map(|_| Mem::new(2 * PAGE, MEM_RESERVE | MEM_COMMIT, PAGE_READWRITE))
            .collect();
        let edge = |m: &Mem| m.base + PAGE;
        // Each object sits so that its block ends at the page edge.
        let pass = edge(&pages[0]) - 0x20;
        let owner = edge(&pages[1]) - 0x28;
        let pool = edge(&pages[2]) - 0x50;
        let obj = edge(&pages[3]) - 0x14;
        // Counter C has the highest index (5); its block ends at entry+0x3c.
        let entries = edge(&pages[4]) - (5 * 0x40 + 0x3c);
        let put64 = |m: &Mem, at: usize, v: usize| m.put(at - m.base, &(v as u64).to_le_bytes());
        let put32 = |m: &Mem, at: usize, v: u32| m.put(at - m.base, &v.to_le_bytes());
        put64(&pages[0], pass + 0x8, owner);
        put64(&pages[0], pass + 0x18, obj);
        put64(&pages[1], owner + 0x20, pool);
        put64(&pages[2], pool + 0x48, entries);
        for (i, (h, s)) in HANDLES.into_iter().zip([A, B, C]).enumerate() {
            put32(&pages[3], obj + 0x8 + 4 * i, h);
            let e = entries + (h & 0xffff) as usize * 0x40;
            put32(&pages[4], e + 0x14, s.last);
            put32(&pages[4], e + 0x1c, s.peak);
            put32(&pages[4], e + 0x20, s.warn);
            put32(&pages[4], e + 0x24, s.cap);
            put32(&pages[4], e + 0x38, h);
        }
        for m in &pages {
            m.protect(PAGE, PAGE, PAGE_NOACCESS);
        }
        // SAFETY: `is_readable` checks every block; the pages live to the end of the test.
        let got = unsafe { read_counters(pass, is_readable) };
        assert_eq!(got, Some([A, B, C]));
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
        assert_eq!(all.len(), 7);
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

    /// Run [`tick`] at each time in `times` on `own`, with an interval of 500 ticks; the times at
    /// which it took a sample, and the due time after each call.
    fn ticks_at(own: &Own, times: &[u64]) -> (Vec<u64>, Vec<u64>) {
        let next_due = AtomicU64::new(0);
        let interval = AtomicU64::new(500);
        let mut sampled = Vec::new();
        let mut due = Vec::new();
        for &now in times {
            tick(now, &next_due, &interval, &own.outputs(), || {
                sampled.push(now);
                Some([
                    CounterSample {
                        last: now as u32,
                        ..A
                    },
                    B,
                    C,
                ])
            });
            due.push(next_due.load(Relaxed));
        }
        (sampled, due)
    }

    #[test]
    fn tick_samples_the_first_call_then_at_most_once_per_interval() {
        let own = Own::default();
        let times = [1000, 1001, 1200, 1499, 1500, 1501, 1999, 2000, 9000, 9001];
        let (sampled, due) = ticks_at(&own, &times);
        assert_eq!(sampled, [1000, 1500, 2000, 9000]);
        assert_eq!(
            due,
            [1500, 1500, 1500, 1500, 2000, 2000, 2000, 2500, 9500, 9500]
        );
        assert_eq!(own.vals()[0], 9000);
        assert!(own.ok.load(Relaxed));
    }

    #[test]
    fn tick_samples_by_time_not_by_how_often_it_is_called() {
        // 10000 calls inside one interval: one sample, however often the grass pass runs.
        let own = Own::default();
        let times: Vec<u64> = (0..10_000).map(|i| 100 + i / 100).collect();
        let (sampled, _) = ticks_at(&own, &times);
        assert_eq!(sampled, [100]);
    }

    #[test]
    fn tick_reads_nothing_more_after_a_failure() {
        let own = Own::default();
        let next_due = AtomicU64::new(0);
        let interval = AtomicU64::new(500);
        let reads = Cell::new(0u32);
        for i in 0..100u64 {
            // Every call is due: one interval apart.
            tick(i * 500, &next_due, &interval, &own.outputs(), || {
                reads.set(reads.get() + 1);
                None
            });
        }
        assert_eq!(reads.get(), 1, "only the first sample ran");
        assert!(own.failed.load(Relaxed));
        assert!(!own.ok.load(Relaxed));
    }

    #[test]
    fn tick_with_no_sample_due_changes_nothing() {
        // A due time in the future: no sample, no publish, and the due time stays (the interval,
        // a value that would move it if it were used, is not used).
        let own = Own::default();
        let next_due = AtomicU64::new(10_000);
        let interval = AtomicU64::new(u64::MAX);
        let reads = Cell::new(0u32);
        for now in [0, 1, 9_999] {
            tick(now, &next_due, &interval, &own.outputs(), || {
                reads.set(reads.get() + 1);
                Some([A, B, C])
            });
        }
        assert_eq!(reads.get(), 0);
        assert_eq!(next_due.load(Relaxed), 10_000);
        assert!(!own.ok.load(Relaxed));
    }

    #[test]
    fn of_many_threads_at_the_same_due_time_exactly_one_samples() {
        let own = Own::default();
        let next_due = AtomicU64::new(1000);
        let interval = AtomicU64::new(500);
        let reads = AtomicU32::new(0);
        let start = std::sync::Barrier::new(8);
        std::thread::scope(|s| {
            for _ in 0..8 {
                s.spawn(|| {
                    start.wait();
                    for _ in 0..1000 {
                        tick(1200, &next_due, &interval, &own.outputs(), || {
                            reads.fetch_add(1, Relaxed);
                            Some([A, B, C])
                        });
                    }
                });
            }
        });
        assert_eq!(reads.load(Relaxed), 1, "one claim per due time");
        assert_eq!(next_due.load(Relaxed), 1700);
    }

    #[test]
    fn the_due_time_saturates_at_the_end_of_the_clock() {
        let own = Own::default();
        let next_due = AtomicU64::new(0);
        let interval = AtomicU64::new(500);
        let reads = Cell::new(0u32);
        for now in [u64::MAX - 10, u64::MAX - 1, u64::MAX] {
            tick(now, &next_due, &interval, &own.outputs(), || {
                reads.set(reads.get() + 1);
                Some([A, B, C])
            });
        }
        // The first samples and moves the due time to u64::MAX; the clock reaches it once more.
        assert_eq!(reads.get(), 2);
        assert_eq!(next_due.load(Relaxed), u64::MAX);
    }

    #[test]
    fn the_interval_is_500_ms_of_counter_ticks_and_never_0() {
        assert_eq!(SAMPLE_INTERVAL_MS, 500);
        assert_eq!(interval_ticks(10_000_000), 5_000_000);
        assert_eq!(interval_ticks(10_000_000), DEFAULT_INTERVAL_TICKS);
        assert_eq!(interval_ticks(3_000), 1_500);
        assert_eq!(interval_ticks(1), 1, "never 0");
        assert_eq!(interval_ticks(0), 1, "never 0");
        assert_eq!(interval_ticks(u64::MAX), u64::MAX / 2, "no overflow");
        assert_eq!(interval_ticks(qpc_frequency()), {
            let f = qpc_frequency();
            assert!(f > 0, "this PC has a performance counter");
            f / 2
        });
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
    fn a_default_install_has_no_probe_hook() {
        // EnableScatterDistance = 1 is the default, but r_max is still 90 m: no consumer.
        assert!(Config::default().knobs.enable_scatter_distance);
        assert!(!wanted(&Config::default()));
        assert!(!wanted(&cfg(true, false, LogLevel::Info)));
        assert!(!wanted(&cfg(true, false, LogLevel::Debug)));
    }

    #[test]
    fn the_debug_key_with_debug_logging_installs_the_probe() {
        assert!(wanted(&cfg(false, true, LogLevel::Debug)));
        assert!(wanted(&cfg(true, true, LogLevel::Debug)));
        assert!(
            !wanted(&cfg(false, true, LogLevel::Info)),
            "[Debug] needs LogLevel = debug"
        );
        assert!(!wanted(&cfg(false, false, LogLevel::Debug)));
    }

    #[test]
    fn the_grass_knob_wants_the_probe_only_once_r_max_allows_a_raise() {
        let knob = cfg(true, false, LogLevel::Info);
        assert!(!wanted_with(&knob, 90.0));
        assert!(!wanted_with(&knob, 99.9));
        assert!(wanted_with(&knob, 100.0), "step 1 (100 m) is allowed");
        assert!(wanted_with(&knob, 127.0));
        assert!(!wanted_with(&knob, f32::NAN));
        // Without the knob, a ceiling alone wants nothing.
        assert!(!wanted_with(&cfg(false, false, LogLevel::Info), 127.0));
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
            "scatter probe: off: no consumer (the grass ceiling r_max is not measured yet, and no \
             [Debug] ScatterProbe = 1 with LogLevel = debug)"
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
            "scatter probe: on (grass pass hook queued; it reads the GPU counters at most every \
             500 ms)"
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
        // Pass the child's measurements on, so `--nocapture` shows them.
        for line in stdout.lines() {
            if let Some(at) = line.find("probe cost:") {
                println!("{}", &line[at..]);
            }
        }
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

                // `hook_at` set the interval for this PC's counter: 500 ms.
                assert_eq!(INTERVAL_TICKS.load(Relaxed), qpc_frequency() / 2);
                // Calls right after the first do not sample, however many there are.
                chain.set(HANDLES[0], CounterSample { last: 0x8000, ..A });
                for _ in 0..1000 {
                    call_fake(0x5678, chain.pass(), 2.25);
                }
                assert_eq!(FAKE_CALLS.load(Relaxed), 1002);
                assert_eq!(LAST_A.load(Relaxed), A.last, "no sample within 500 ms");
                // The first call after 500 ms samples.
                std::thread::sleep(std::time::Duration::from_millis(600));
                call_fake(0x5678, chain.pass(), 2.25);
                assert_eq!(FAKE_CALLS.load(Relaxed), 1003);
                assert_eq!(LAST_A.load(Relaxed), 0x8000, "a sample after 500 ms");

                // Take the hook out: the fake runs alone again.
                unhook_the_fake();
                chain.set(HANDLES[0], CounterSample { last: 0x9000, ..A });
                std::thread::sleep(std::time::Duration::from_millis(600));
                for _ in 0..30 {
                    call_fake(0x5678, chain.pass(), 2.25);
                }
                assert_eq!(FAKE_CALLS.load(Relaxed), 1033);
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

                // A good chain later, when a sample is due again, does not turn it back on: off
                // for the session.
                let chain = Chain::new();
                for _ in 0..60 {
                    call_fake(1, chain.pass(), 0.5);
                }
                std::thread::sleep(std::time::Duration::from_millis(600));
                call_fake(1, chain.pass(), 0.5);
                assert_eq!(FAKE_CALLS.load(Relaxed), 62);
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

    // ---- measurements (ruling 4 of the stutter fix; numbers show with --nocapture) -----------
    //
    // The bounds are loose on purpose, for a slow debug build on a busy CI machine. The goal on
    // a release build is well under 1 us per call and well under 100 us per sample.

    /// Run `f` `n` times and return the mean time per run in nanoseconds.
    fn mean_ns(n: u32, mut f: impl FnMut()) -> f64 {
        let start = std::time::Instant::now();
        for _ in 0..n {
            f();
        }
        start.elapsed().as_nanos() as f64 / f64::from(n)
    }

    #[test]
    fn measure_the_per_call_cost_when_no_sample_is_due() {
        let own = Own::default();
        // Due far in the future, as on every call between two samples.
        let next_due = AtomicU64::new(u64::MAX);
        let interval = AtomicU64::new(DEFAULT_INTERVAL_TICKS);
        let reads = Cell::new(0u32);
        let per_call = mean_ns(200_000, || {
            tick(qpc_now(), &next_due, &interval, &own.outputs(), || {
                reads.set(reads.get() + 1);
                Some([A, B, C])
            })
        });
        println!("probe cost: no sample due: {per_call:.1} ns per call (QPC read + one load)");
        assert_eq!(reads.get(), 0);
        assert!(per_call < 5_000.0, "{per_call} ns per call");
    }

    #[test]
    fn measure_the_cost_of_one_sample_on_the_fake_chain() {
        let chain = Chain::new();
        let own = Own::default();
        let per_check = mean_ns(2_000, || {
            assert!(is_readable(chain.pass(), 8));
        });
        let per_sample = mean_ns(2_000, || {
            // SAFETY: `is_readable` checks every block; the chain's buffer is live.
            own.outputs()
                .publish(unsafe { read_counters(chain.pass(), is_readable) });
        });
        println!(
            "probe cost: one sample on the fake chain: {:.2} us (7 VirtualQuery checks at {:.2} us \
             each; 22 checks were {:.2} us)",
            per_sample / 1000.0,
            per_check / 1000.0,
            22.0 * per_check / 1000.0
        );
        assert!(own.ok.load(Relaxed), "the samples were good");
        assert!(per_sample < 1_000_000.0, "{per_sample} ns per sample");
    }

    #[test]
    fn measure_the_hooked_call_overhead_when_no_sample_is_due() {
        in_child(
            "measure_the_hooked_call_overhead_when_no_sample_is_due",
            || {
                let chain = Chain::new();
                const N: u32 = 100_000;
                let plain = mean_ns(N, || call_fake(1, chain.pass(), 1.0));
                hook_the_fake();
                // The first hooked call samples; the next one is due 500 ms later.
                call_fake(1, chain.pass(), 1.0);
                assert!(PROBE_OK.load(Relaxed));
                let hooked = mean_ns(N, || call_fake(1, chain.pass(), 1.0));
                let overhead = hooked - plain;
                println!(
                    "probe cost: hooked call, no sample due: {overhead:.1} ns per call over the \
                     plain call ({hooked:.1} ns hooked, {plain:.1} ns plain)"
                );
                assert_eq!(FAKE_CALLS.load(Relaxed), 2 * N + 1);
                assert!(overhead < 10_000.0, "{overhead} ns per call");
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
