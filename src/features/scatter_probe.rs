//! Scatter counter probe (design.md 7.2a). The render hook only publishes the latest pass
//! pointer and increments a call counter, then calls the original exactly once.
//!
//! A detached `scatter-probe` thread wakes every 500 ms. When the call counter changed,
//! it copies seven fixed-size game-memory blocks with ReadProcessMemory and parses only
//! those local bytes. No game address is dereferenced by Rust. A failed or short copy, zero
//! handle or ownership mismatch is a transient miss: nine are tolerated, the tenth latches
//! FAILED. A good sample resets misses; a copied A.warn/A.cap layout mismatch fails at once.
//! No incomplete sample is published. A panic stops the guarded worker and latches failure.
//!
//! Installation remains opt-in until R_MAX_M allows a raise: Debug ScatterProbe with debug
//! logging, or an enabled scatter knob whose measured ceiling permits at least one step.
//! This removes sampling work from the game thread after the observed in-game pulse; its
//! address-space-lock cause is a hypothesis. Not verified in game; the maintainer boot-tests
//! it with the probe on. Counter offsets and ABI evidence are in docs/re-notes.md.

use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};

use log::Level;
use windows_sys::Win32::System::Diagnostics::Debug::ReadProcessMemory;
use windows_sys::Win32::System::Threading::GetCurrentProcess;

use crate::config::{Config, LogLevel};
use crate::entry;
use crate::guard::{guard, Feature};
use crate::knobs::create_and_queue;
use crate::knobs::SCATTER_STEPS;
use crate::scan::{find_in_sections, main_module, Module, Pattern};
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

/// The sampler sleeps this many milliseconds between opportunities, holding no lock.
const SAMPLE_INTERVAL_MS: u64 = 500;

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
/// through the scatter object pointer (+0x18..+0x20). One local copy covers both.
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
/// True after ten consecutive transient misses, an immediate copied-layout failure, or a
/// sampler panic/start failure. The probe stops reading for the session. Stored with Release
/// after PROBE_OK is cleared.
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

/// The latest grass-pass argument, published without reading the pointed-to object.
static LAST_PASS: AtomicUsize = AtomicUsize::new(0);
/// Number of hook publications; the sampler skips unchanged observations.
static CALL_COUNT: AtomicU64 = AtomicU64::new(0);

/// MinHook's trampoline for the game function, or 0. Stored by [`install`] before the hook is
/// enabled; read by the detour.
static TRAMPOLINE: AtomicUsize = AtomicUsize::new(0);

/// Test only: panic after publication to exercise the detour's original-once fallback.
/// Not in a non-test build.
#[cfg(test)]
static PANIC_IN_PUBLISH: AtomicBool = AtomicBool::new(false);

/// Test only: count actual OS-reader calls in isolated native-hook tests.
#[cfg(test)]
static READ_CALLS: AtomicUsize = AtomicUsize::new(0);
/// Test only: any OS-reader call panics, proving the hook never reaches it.
#[cfg(test)]
static PANIC_IN_READ: AtomicBool = AtomicBool::new(false);

/// The type of the game function and of its detour: RCX, RDX and an `f32` in XMM2, no stack
/// argument, no return value (docs/re-notes.md, "Scatter counter probe").
pub type ScatterFrameFn = unsafe extern "system" fn(rcx: *mut u8, pass: *const u8, f: f32);

/// Copy a complete process-memory block into an owned local buffer.
fn read_process(addr: usize, bytes: &mut [u8]) -> bool {
    #[cfg(test)]
    {
        READ_CALLS.fetch_add(1, Ordering::Relaxed);
        if PANIC_IN_READ.load(Ordering::Relaxed) {
            panic!("reader reached on hook");
        }
    }
    if addr == 0 || addr.checked_add(bytes.len()).is_none() {
        return false;
    }
    let mut copied = 0;
    // SAFETY: the destination is the live mutable slice and has the supplied size. Windows
    // validates the source range; no Rust reference or dereference is made from that address.
    let result = unsafe {
        ReadProcessMemory(
            GetCurrentProcess(),
            addr as *const c_void,
            bytes.as_mut_ptr().cast(),
            bytes.len(),
            &mut copied,
        )
    };
    complete_read(result, copied, bytes.len())
}

/// Accept an OS copy only when it succeeds and reports every requested byte.
fn complete_read(result: i32, copied: usize, wanted: usize) -> bool {
    result != 0 && copied == wanted
}

/// Why a sampler could not accept a copied counter chain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadError {
    /// An object disappeared, a copy failed, or its handle no longer owns the entry.
    Miss,
    /// Counter A was read correctly but its warn/cap metadata disagrees with the layout.
    Layout,
}

/// Copy and parse the seven counter blocks without dereferencing a game address.
pub fn read_counters(
    pass: usize,
    mut read: impl FnMut(usize, &mut [u8]) -> bool,
) -> Result<[CounterSample; 3], ReadError> {
    let pass_bytes = copy_block::<0x18>(pass, PASS_BLOCK.0, &mut read).ok_or(ReadError::Miss)?;
    let owner = pointer_field(&pass_bytes, PASS_BLOCK.0, PASS_POOL_OWNER).ok_or(ReadError::Miss)?;
    let obj = pointer_field(&pass_bytes, PASS_BLOCK.0, PASS_SCATTER_OBJ).ok_or(ReadError::Miss)?;
    let owner_bytes = copy_block::<8>(owner, OWNER_POOL, &mut read).ok_or(ReadError::Miss)?;
    let pool = pointer_field(&owner_bytes, OWNER_POOL, OWNER_POOL).ok_or(ReadError::Miss)?;
    let pool_bytes = copy_block::<8>(pool, POOL_ENTRIES, &mut read).ok_or(ReadError::Miss)?;
    let entries = pointer_field(&pool_bytes, POOL_ENTRIES, POOL_ENTRIES).ok_or(ReadError::Miss)?;
    let handles = copy_block::<0xc>(obj, HANDLES_BLOCK.0, &mut read).ok_or(ReadError::Miss)?;
    let mut counter = |offset| -> Result<CounterSample, ReadError> {
        let handle = u32_field(&handles, HANDLES_BLOCK.0, offset).ok_or(ReadError::Miss)?;
        if handle == 0 {
            return Err(ReadError::Miss);
        }
        let index = usize::try_from(handle & HANDLE_INDEX_MASK).map_err(|_| ReadError::Miss)?;
        let entry = index
            .checked_mul(ENTRY_SIZE)
            .and_then(|offset| entries.checked_add(offset))
            .ok_or(ReadError::Miss)?;
        let bytes = copy_block::<0x28>(entry, ENTRY_BLOCK.0, &mut read).ok_or(ReadError::Miss)?;
        if u32_field(&bytes, ENTRY_BLOCK.0, ENTRY_HANDLE) != Some(handle) {
            return Err(ReadError::Miss);
        }
        Ok(CounterSample {
            last: u32_field(&bytes, ENTRY_BLOCK.0, ENTRY_LAST).ok_or(ReadError::Miss)?,
            peak: u32_field(&bytes, ENTRY_BLOCK.0, ENTRY_PEAK).ok_or(ReadError::Miss)?,
            warn: u32_field(&bytes, ENTRY_BLOCK.0, ENTRY_WARN).ok_or(ReadError::Miss)?,
            cap: u32_field(&bytes, ENTRY_BLOCK.0, ENTRY_CAP).ok_or(ReadError::Miss)?,
        })
    };
    let [at_a, at_b, at_c] = COUNTER_HANDLES;
    let a = counter(at_a)?;
    if a.warn != WARN_A || a.cap != CAP_A {
        return Err(ReadError::Layout);
    }
    Ok([a, counter(at_b)?, counter(at_c)?])
}

/// Copy one fixed-size block, rejecting null bases and all address arithmetic overflow.
fn copy_block<const N: usize>(
    base: usize,
    offset: usize,
    read: &mut impl FnMut(usize, &mut [u8]) -> bool,
) -> Option<[u8; N]> {
    if base == 0 {
        return None;
    }
    let address = base.checked_add(offset)?;
    address.checked_add(N)?;
    let mut bytes = [0; N];
    read(address, &mut bytes).then_some(bytes)
}

/// Extract a field only from the local copied block, with checked range arithmetic.
fn field<const N: usize>(bytes: &[u8], block_start: usize, offset: usize) -> Option<[u8; N]> {
    let start = offset.checked_sub(block_start)?;
    bytes.get(start..start.checked_add(N)?)?.try_into().ok()
}

/// Parse one copied little-endian u32.
fn u32_field(bytes: &[u8], block_start: usize, offset: usize) -> Option<u32> {
    Some(u32::from_le_bytes(field(bytes, block_start, offset)?))
}

/// Parse one copied little-endian 64-bit address.
fn pointer_field(bytes: &[u8], block_start: usize, offset: usize) -> Option<usize> {
    usize::try_from(u64::from_le_bytes(field(bytes, block_start, offset)?)).ok()
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

/// The sampler's local state, independent of threads and sleeping.
#[derive(Default)]
struct Sampler {
    /// The last observed hook call count.
    last_calls: u64,
    /// Consecutive samples whose objects could not be copied or owned.
    misses: u8,
}

impl Sampler {
    /// One sampling opportunity with injected pointer and memory readers.
    fn step(
        &mut self,
        calls: u64,
        pass: impl FnOnce() -> usize,
        out: &Outputs<'_>,
        read: &mut impl FnMut(usize, &mut [u8]) -> bool,
    ) {
        if out.failed.load(Ordering::Acquire) || calls == self.last_calls {
            return;
        }
        self.last_calls = calls;
        match read_counters(pass(), read) {
            Ok(sample) => {
                self.misses = 0;
                out.publish(Some(sample));
            }
            Err(ReadError::Layout) => out.publish(None),
            Err(ReadError::Miss) => {
                self.misses = self.misses.saturating_add(1);
                if self.misses >= 10 {
                    out.publish(None);
                }
            }
        }
    }
}

/// Stop and latch failure if the sampler panics or its feature was already disabled.
fn guarded_sampler(out: &Outputs<'_>, body: impl FnOnce()) {
    guard(Feature::ScatterDistance, || out.publish(None), body);
}

/// Publish in an inner guard, then call the native original exactly once.
/// A publication panic is caught by the inner guard, so it cannot reach the outer fallback
/// and call the original twice. An already disabled feature uses only the outer fallback.
fn run_before_original(publish: impl FnOnce(), original: impl Fn()) {
    guard(Feature::ScatterDistance, &original, || {
        guard(Feature::ScatterDistance, || (), publish);
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

/// Publish the pass pointer (Release), bump the call counter (Relaxed), then call the original.
/// No clock, game-memory read, allocation, lock or log is added to the render path.
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
    run_before_original(
        move || {
            LAST_PASS.store(pass as usize, Ordering::Release);
            CALL_COUNT.fetch_add(1, Ordering::Relaxed);
            #[cfg(test)]
            if PANIC_IN_PUBLISH.load(Ordering::Relaxed) {
                panic!("publication panic");
            }
        },
        original,
    );
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

/// Name both failed halves of the consumer rule, without guessing why the ceiling is low.
fn not_wanted_reason(cfg: &Config, r_max: f32) -> String {
    let debug = if !cfg.debug.scatter_probe {
        "[Debug] ScatterProbe = 0"
    } else {
        "LogLevel is not debug"
    };
    let knob = if !cfg.knobs.enable_scatter_distance {
        "EnableScatterDistance = 0".to_owned()
    } else {
        format!("the grass ceiling r_max = {r_max} m allows no raise")
    };
    format!("scatter probe: off: no consumer ({debug}; {knob})")
}

/// Start only after the wanted hook was installed; both effects are injectable.
fn start_up(
    cfg: &Config,
    code_patches: bool,
    install: impl FnOnce() -> Result<(), String>,
    start_sampler: impl FnOnce() -> Result<(), String>,
) -> (Level, String) {
    if !code_patches {
        return (
            Level::Info,
            "scatter probe: off: code patches are off on this build".into(),
        );
    }
    if !wanted(cfg) {
        return (Level::Info, not_wanted_reason(cfg, R_MAX_M));
    }
    match install().and_then(|()| start_sampler()) {
        Ok(()) => (
            Level::Info,
            "scatter probe: on (grass pass hook queued; sampler thread copies GPU counters every 500 ms)".into(),
        ),
        Err(why) => (Level::Warn, format!("scatter probe: off: {why}")),
    }
}

/// Run the worker under the feature guard; it holds only local state while sleeping.
fn sampler_main() {
    let out = public();
    guarded_sampler(&out, || {
        let mut state = Sampler::default();
        let mut reader = read_process;
        loop {
            std::thread::sleep(std::time::Duration::from_millis(SAMPLE_INTERVAL_MS));
            if crate::guard::is_disabled(Feature::ScatterDistance) {
                out.publish(None);
                return;
            }
            if out.failed.load(Ordering::Acquire) {
                return;
            }
            state.step(
                CALL_COUNT.load(Ordering::Relaxed),
                || LAST_PASS.load(Ordering::Acquire),
                &out,
                &mut reader,
            );
        }
    });
}

/// Start one named, detached sampler only after installation succeeded.
fn start_sampler_thread() -> Result<(), String> {
    std::thread::Builder::new()
        .name("scatter-probe".into())
        .spawn(sampler_main)
        .map(|_| ())
        .map_err(|error| {
            public().publish(None);
            format!("cannot start sampler thread: {error}")
        })
}

/// Decide, install the queued hook, start its detached sampler, and log one outcome line.
/// Called once, from `entry::start`, after `code_patches_allowed` is set and before
/// `knobs::enable_queued_hooks`.
pub fn init(cfg: &Config) {
    let (level, line) = start_up(
        cfg,
        entry::code_patches_allowed(),
        || {
            // SAFETY: `init` runs from `entry::start`, in `InitializeASI`, before the game's
            // `WinMain`: one thread. `main_module()` is the running exe.
            unsafe { install(&main_module()) }
        },
        start_sampler_thread,
    );
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

        /// Copy only from this fixture's owned buffer, without dereferencing its fake links.
        fn copy(&self, address: usize, bytes: &mut [u8]) -> bool {
            let Some(start) = address.checked_sub(self.base()) else {
                return false;
            };
            let Some(end) = start.checked_add(bytes.len()) else {
                return false;
            };
            let Some(source) = self.mem.get(start..end) else {
                return false;
            };
            bytes.copy_from_slice(source);
            true
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
    fn copied_chain_reads_seven_exact_blocks_and_parses_all_counters() {
        let chain = Chain::new();
        let asked = RefCell::new(Vec::new());
        let result = read_counters(chain.pass(), |address, bytes| {
            asked.borrow_mut().push((address, bytes.len()));
            chain.copy(address, bytes)
        });
        assert_eq!(result, Ok([A, B, C]));
        assert_eq!(*asked.borrow(), chain.expected_checks());
    }

    #[test]
    fn rpm_requires_success_and_exact_byte_count() {
        assert!(complete_read(1, 8, 8));
        assert!(complete_read(-1, 8, 8));
        assert!(!complete_read(0, 8, 8));
        assert!(!complete_read(1, 7, 8));
        assert!(!complete_read(1, 9, 8));
        assert!(!complete_read(0, 0, 8));
    }

    #[test]
    fn rpm_copies_full_block_and_refuses_noaccess_boundary() {
        let pages = Mem::new(2 * PAGE, MEM_RESERVE | MEM_COMMIT, PAGE_READWRITE);
        pages.put(PAGE - 8, &[1, 2, 3, 4, 5, 6, 7, 8]);
        pages.protect(PAGE, PAGE, PAGE_NOACCESS);
        let mut bytes = [0; 8];
        assert!(read_process(pages.base + PAGE - 8, &mut bytes));
        assert_eq!(bytes, [1, 2, 3, 4, 5, 6, 7, 8]);
        assert!(!read_process(pages.base + PAGE - 8, &mut [0; 16]));
        assert!(!read_process(pages.base + PAGE, &mut [0; 8]));
        assert!(!read_process(0, &mut [0; 8]));
        assert!(!read_process(usize::MAX - 2, &mut [0; 8]));
    }

    /// A chain whose every checked block ends exactly at the end of a readable page, with a
    /// no-access page right after it: a copy past a block must fail safely.
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
        let got = read_counters(pass, read_process);
        assert_eq!(got, Ok([A, B, C]));
    }

    #[test]
    fn every_refused_copy_stops_at_that_block_and_publishes_no_partial_sample() {
        let mut chain = Chain::new();
        chain.set(HANDLES[0], CounterSample { last: 99, ..A });
        let all = chain.expected_checks();
        for (i, &refused) in all.iter().enumerate() {
            let asked = RefCell::new(Vec::new());
            let own = Own::default();
            own.outputs().publish(Some([A, B, C]));
            let mut state = Sampler::default();
            let mut reader = |address, bytes: &mut [u8]| {
                asked.borrow_mut().push((address, bytes.len()));
                // Even a refusing reader fills valid bytes: ignoring its status must fail.
                chain.copy(address, bytes) && (address, bytes.len()) != refused
            };
            state.step(1, || chain.pass(), &own.outputs(), &mut reader);
            assert_eq!(*asked.borrow(), all[..=i], "refused copy {i}");
            assert_eq!(state.misses, 1);
            assert!(!own.failed.load(Relaxed));
            assert!(own.ok.load(Relaxed));
            assert_eq!(own.vals(), [A.last, A.peak, B.last, B.peak, C.last, C.peak]);
        }
    }

    #[test]
    fn zero_and_mismatched_handles_are_transient_and_stop_before_later_entries() {
        for (i, handle) in HANDLES.into_iter().enumerate() {
            for zero in [false, true] {
                let mut chain = Chain::new();
                if zero {
                    chain.put_u32(OBJ_AT + 0x8 + 4 * i, 0);
                } else {
                    chain.put_u32(Chain::entry_at(handle) + 0x38, handle ^ 0x2_0000);
                }
                let asked = Cell::new(0);
                let result = read_counters(chain.pass(), |a, b| {
                    asked.set(asked.get() + 1);
                    chain.copy(a, b)
                });
                assert_eq!(result, Err(ReadError::Miss));
                assert_eq!(asked.get(), 4 + i + usize::from(!zero));
            }
        }
    }

    #[test]
    fn only_counter_a_warn_and_cap_are_the_layout_self_check() {
        let mut chain = Chain::new();
        chain.set(
            HANDLES[1],
            CounterSample {
                warn: 1,
                cap: 2,
                ..B
            },
        );
        assert_eq!(
            read_counters(chain.pass(), |a, b| chain.copy(a, b)),
            Ok([
                A,
                CounterSample {
                    warn: 1,
                    cap: 2,
                    ..B
                },
                C
            ])
        );
    }

    #[test]
    fn local_fields_and_addresses_use_checked_bounds_and_never_read_invalid_links() {
        let bytes = [1, 0, 0, 0, 2, 0, 0, 0, 3, 0, 0, 0];
        assert_eq!(u32_field(&bytes, 0, 8), Some(3));
        assert_eq!(u32_field(&bytes, 0, 9), None);
        assert_eq!(u32_field(&bytes, 8, 7), None);
        assert_eq!(u32_field(&bytes, 0, usize::MAX), None);
        assert_eq!(pointer_field(&bytes, 0, 4), Some(0x0000_0003_0000_0002));
        assert_eq!(pointer_field(&bytes, 0, 5), None);
        for pass in [0, usize::MAX - 4] {
            assert_eq!(
                read_counters(pass, |_, _| panic!("invalid address reached reader")),
                Err(ReadError::Miss)
            );
        }
        for (offset, value, expected) in [
            (PASS_AT + 0x8, 0, 1),
            (PASS_AT + 0x8, usize::MAX - 4, 1),
            (PASS_AT + 0x18, usize::MAX - 4, 3),
            (POOL_AT + 0x48, usize::MAX - 4, 4),
        ] {
            let mut chain = Chain::new();
            chain.put_u64(offset, value as u64);
            let calls = Cell::new(0);
            assert_eq!(
                read_counters(chain.pass(), |a, b| {
                    calls.set(calls.get() + 1);
                    chain.copy(a, b)
                }),
                Err(ReadError::Miss)
            );
            assert_eq!(calls.get(), expected);
        }
    }

    #[test]
    fn rpm_refuses_bad_pages_and_reads_readonly_memory() {
        let ro = Mem::new(PAGE, MEM_RESERVE | MEM_COMMIT, PAGE_READWRITE);
        ro.put(8, &[11, 12, 13, 14]);
        ro.protect(0, PAGE, PAGE_READONLY);
        let mut bytes = [0; 4];
        assert!(read_process(ro.base + 8, &mut bytes));
        assert_eq!(bytes, [11, 12, 13, 14]);
        for flags in [PAGE_NOACCESS, PAGE_READWRITE | PAGE_GUARD] {
            let memory = Mem::new(PAGE, MEM_RESERVE | MEM_COMMIT, flags);
            assert!(!read_process(memory.base, &mut bytes));
        }
        let reserved = Mem::new(PAGE, MEM_RESERVE, PAGE_READWRITE);
        assert!(!read_process(reserved.base, &mut bytes));
    }

    // ---- publishing and the pure sampler ------------------------------------------------------

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
    fn a_good_sample_resets_misses_and_preserves_values_during_later_misses() {
        let own = Own::default();
        let chain = Chain::new();
        let mut state = Sampler::default();
        for calls in 1..=9 {
            state.step(calls, || 0, &own.outputs(), &mut |_, _| false);
        }
        state.step(10, || chain.pass(), &own.outputs(), &mut |a, b| {
            chain.copy(a, b)
        });
        assert_eq!(state.misses, 0);
        assert!(own.ok.load(Relaxed));
        let values = [A.last, A.peak, B.last, B.peak, C.last, C.peak];
        assert_eq!(own.vals(), values);
        for calls in 11..=19 {
            state.step(calls, || 0, &own.outputs(), &mut |_, _| false);
            assert!(!own.failed.load(Relaxed));
            assert!(own.ok.load(Relaxed));
            assert_eq!(own.vals(), values);
        }
        state.step(20, || 0, &own.outputs(), &mut |_, _| false);
        assert!(own.failed.load(Relaxed));
        assert!(!own.ok.load(Relaxed));
        assert_eq!(own.vals(), values);
    }

    #[test]
    fn copied_bad_warn_or_cap_fails_immediately_without_publishing() {
        for bad in [
            CounterSample {
                warn: WARN_A - 1,
                ..A
            },
            CounterSample {
                cap: CAP_A + 1,
                ..A
            },
        ] {
            let mut chain = Chain::new();
            chain.set(HANDLES[0], bad);
            let own = Own::default();
            let mut state = Sampler::default();
            state.step(1, || chain.pass(), &own.outputs(), &mut |a, b| {
                chain.copy(a, b)
            });
            assert!(own.failed.load(Relaxed));
            assert!(!own.ok.load(Relaxed));
            assert_eq!(own.vals(), [0; 6]);
        }
    }

    #[test]
    fn unchanged_counter_skips_pointer_and_memory_reads_and_miss_counting() {
        let own = Own::default();
        let mut state = Sampler::default();
        state.step(
            0,
            || panic!("no pass read before any call"),
            &own.outputs(),
            &mut |_, _| panic!("no memory read"),
        );
        let chain = Chain::new();
        state.step(5, || chain.pass(), &own.outputs(), &mut |a, b| {
            chain.copy(a, b)
        });
        let values = own.vals();
        state.step(
            5,
            || panic!("unchanged pass"),
            &own.outputs(),
            &mut |_, _| panic!("unchanged read"),
        );
        assert_eq!(own.vals(), values);
        state.step(6, || 0, &own.outputs(), &mut |_, _| false);
        assert_eq!(state.misses, 1);
        state.step(
            6,
            || panic!("unchanged miss"),
            &own.outputs(),
            &mut |_, _| panic!("unchanged read"),
        );
        assert_eq!(state.misses, 1);
        state.last_calls = u64::MAX;
        state.step(0, || chain.pass(), &own.outputs(), &mut |a, b| {
            chain.copy(a, b)
        });
        assert_eq!(state.misses, 0, "a wrapped counter still changed");
    }

    #[test]
    fn a_latched_failure_never_reads_or_recovers() {
        let own = Own::default();
        own.outputs().publish(None);
        let mut state = Sampler::default();
        state.step(
            1,
            || panic!("failed pointer"),
            &own.outputs(),
            &mut |_, _| panic!("failed read"),
        );
        assert!(own.failed.load(Relaxed));
        assert!(!own.ok.load(Relaxed));
        assert_eq!(own.vals(), [0; 6]);
    }

    #[test]
    fn a_sampler_panic_stops_work_and_latches_failed() {
        in_child("a_sampler_panic_stops_work_and_latches_failed", || {
            let own = Own::default();
            own.outputs().publish(Some([A, B, C]));
            let reached = Cell::new(false);
            guarded_sampler(&own.outputs(), || {
                panic!("sampler reader panic");
            });
            assert!(own.failed.load(Relaxed));
            assert!(!own.ok.load(Relaxed));
            assert!(is_disabled(Feature::ScatterDistance));
            guarded_sampler(&own.outputs(), || reached.set(true));
            assert!(!reached.get());
        });
    }

    #[test]
    fn sampler_retries_nine_misses_then_latches_the_tenth() {
        let own = Own::default();
        let mut state = Sampler::default();
        for calls in 1..=9 {
            state.step(calls, || 0, &own.outputs(), &mut |_, _| false);
            assert!(!own.failed.load(Relaxed), "miss {calls} is transient");
        }
        assert_eq!(state.misses, 9);
        state.step(10, || 0, &own.outputs(), &mut |_, _| false);
        assert!(own.failed.load(Relaxed));
        assert!(!own.ok.load(Relaxed));
        assert_eq!(own.vals(), [0; 6]);
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
    fn sampler_starts_once_only_after_a_wanted_hook_installs() {
        let effects = RefCell::new(Vec::new());
        let wanted_cfg = cfg(false, true, LogLevel::Debug);
        let (level, _) = start_up(
            &wanted_cfg,
            true,
            || {
                effects.borrow_mut().push("install");
                Ok(())
            },
            || {
                effects.borrow_mut().push("sampler");
                Ok(())
            },
        );
        assert_eq!(level, Level::Info);
        assert_eq!(*effects.borrow(), ["install", "sampler"]);
        for (config, patches, installation) in [
            (wanted_cfg, false, Ok(())),
            (Config::default(), true, Ok(())),
            (wanted_cfg, true, Err("missing signature".to_owned())),
        ] {
            effects.borrow_mut().clear();
            let (_, line) = start_up(
                &config,
                patches,
                || {
                    effects.borrow_mut().push("install");
                    installation
                },
                || {
                    effects.borrow_mut().push("sampler");
                    Ok(())
                },
            );
            assert!(!effects.borrow().contains(&"sampler"), "{line}");
        }
        let (level, line) = start_up(
            &wanted_cfg,
            true,
            || Ok(()),
            || Err("cannot start worker".into()),
        );
        assert_eq!(level, Level::Warn);
        assert!(line.contains("cannot start worker"));
    }

    #[test]
    fn startup_reasons_name_the_actual_failed_halves() {
        let (level, line) = start_up(
            &cfg(true, true, LogLevel::Debug),
            false,
            || panic!("no install"),
            || panic!("no sampler"),
        );
        assert_eq!(level, Level::Info);
        assert_eq!(
            line,
            "scatter probe: off: code patches are off on this build"
        );
        let (_, line) = start_up(
            &Config::default(),
            true,
            || panic!("no install"),
            || panic!("no sampler"),
        );
        assert!(line.contains("ScatterProbe = 0"));
        assert!(line.contains("r_max = 90 m allows no raise"));
        let line = not_wanted_reason(&cfg(false, true, LogLevel::Info), 127.0);
        assert!(line.contains("LogLevel is not debug"));
        assert!(line.contains("EnableScatterDistance = 0"));
        assert!(!line.contains("not measured"));
        let (level, line) = start_up(
            &cfg(false, true, LogLevel::Debug),
            true,
            || Err("missing signature".into()),
            || panic!("no worker"),
        );
        assert_eq!(level, Level::Warn);
        assert_eq!(line, "scatter probe: off: missing signature");
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
    /// It records RCX, RDX, XMM2 and PROBE_OK, then counts the call. Plain and hooked
    /// measurements use exactly the same original side effects.
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
        // SAFETY: the fake only records its arguments; the detour only publishes the numeric
        // pointer, so even an invalid pass value is never dereferenced.
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
    fn real_hook_only_publishes_and_passes_every_argument_on() {
        in_child(
            "real_hook_only_publishes_and_passes_every_argument_on",
            || {
                let chain = Chain::new();
                hook_the_fake();
                PANIC_IN_READ.store(true, Relaxed);
                for (i, (pass, float)) in [
                    (chain.pass(), 2.25),
                    (usize::MAX - 8, f32::from_bits(0x7fc12345)),
                    (0, -0.0),
                ]
                .into_iter()
                .enumerate()
                {
                    call_fake(0x5678 + i, pass, float);
                    assert_eq!(FAKE_CALLS.load(Relaxed), i as u32 + 1);
                    assert_eq!(FAKE_RCX.load(Relaxed), 0x5678 + i);
                    assert_eq!(FAKE_RDX.load(Relaxed), pass);
                    assert_eq!(FAKE_F.load(Relaxed), float.to_bits());
                    assert_eq!(LAST_PASS.load(Ordering::Acquire), pass);
                    assert_eq!(CALL_COUNT.load(Relaxed), i as u64 + 1);
                    assert!(!PROBE_OK.load(Relaxed));
                    assert!(!FAILED.load(Relaxed));
                    assert_eq!(LAST_A.load(Relaxed), 0);
                    assert_eq!(READ_CALLS.load(Relaxed), 0);
                    assert!(!is_disabled(Feature::ScatterDistance));
                }
                unhook_the_fake();
            },
        );
    }

    #[test]
    fn the_original_runs_exactly_once_when_publication_panics() {
        in_child(
            "the_original_runs_exactly_once_when_publication_panics",
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
    fn the_real_detour_runs_the_original_once_when_publication_panics() {
        in_child(
            "the_real_detour_runs_the_original_once_when_publication_panics",
            || {
                assert!(!is_disabled(Feature::ScatterDistance), "a fresh process");
                let chain = Chain::new();
                hook_the_fake();
                PANIC_IN_PUBLISH.store(true, Relaxed);
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
                PANIC_IN_PUBLISH.store(false, Relaxed);
                call_fake(7, chain.pass(), 3.0);
                assert_eq!(FAKE_CALLS.load(Relaxed), 2);
                assert!(!PROBE_OK.load(Relaxed), "still nothing published");
                unhook_the_fake();
            },
        );
    }

    /// Measure fixed batches without imposing a wall-clock assertion on CI.
    fn mean_ns(n: u32, mut call: impl FnMut()) -> f64 {
        let start = std::time::Instant::now();
        for _ in 0..n {
            call();
        }
        start.elapsed().as_secs_f64() * 1e9 / f64::from(n)
    }

    #[test]
    fn measure_the_hooked_call_overhead_against_the_same_plain_fake() {
        in_child(
            "measure_the_hooked_call_overhead_against_the_same_plain_fake",
            || {
                let n = if cfg!(debug_assertions) {
                    10_000
                } else {
                    1_000_000
                };
                let batches = if cfg!(debug_assertions) { 2 } else { 7 };
                let mut calls = 0u32;
                for batch in 0..batches {
                    let mut measure = |hooked| {
                        if hooked {
                            hook_the_fake();
                        }
                        for _ in 0..10_000 {
                            call_fake(
                                std::hint::black_box(1),
                                std::hint::black_box(0),
                                std::hint::black_box(1.0),
                            );
                        }
                        let result = mean_ns(n, || {
                            call_fake(
                                std::hint::black_box(1),
                                std::hint::black_box(0),
                                std::hint::black_box(1.0),
                            )
                        });
                        calls += n + 10_000;
                        if hooked {
                            unhook_the_fake();
                        }
                        result
                    };
                    let (plain, hooked) = if batch % 2 == 0 {
                        let plain = measure(false);
                        (plain, measure(true))
                    } else {
                        let hooked = measure(true);
                        (measure(false), hooked)
                    };
                    println!("probe cost: batch {batch}: {plain:.3} ns plain, {hooked:.3} ns hooked, {:.3} ns overhead",hooked-plain);
                }
                assert_eq!(FAKE_CALLS.load(Relaxed), calls);
                assert_eq!(READ_CALLS.load(Relaxed), 0);
                assert!(!PROBE_OK.load(Relaxed));
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
