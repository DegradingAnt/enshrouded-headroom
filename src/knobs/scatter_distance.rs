//! Grass and detail-scatter distance (design.md 7.2): the two High-level distance loads read a
//! float in the mod's own page.
//!
//! The game checks the grass distance in two places. At the High level (3) both load it with a
//! RIP-relative `movss` from one shared `.rdata` constant, 90.0 (0x1413b65fc in build f68140a3;
//! docs/re-notes.md):
//! - per terrain brick: `SCATTER_BRICK_LOAD`, disp32 at match +9;
//! - per instance: `SCATTER_INSTANCE_LOADS`, disp32 at match +30. The two other loads in that
//!   pattern (1.0 and 14.0) are left alone.
//!
//! [`install`] refuses anything but that vanilla state. Then it allocates a page within reach of
//! both loads, writes 90.0 into it, publishes its address in [`SCATTER_FLOAT`], and rewrites both
//! disp32s to point at it. So installing changes nothing visible. The shared constant is never
//! written, because other code reads it too. [`set_distance`] changes the page float later, with
//! one aligned 4-byte store.
//!
//! **Capacity (the audit in docs/re-notes.md):** no scatter capacity headroom is proven. So
//! [`CAP_OK`] is false, [`CAP_SITES`] is empty and [`max_step`] gives 0: the knob stays at 90 m in
//! this version. Nothing here writes a capacity site. The counter probe (plan Task 13b) can raise
//! the ceiling later without another code patch.
//!
//! The page float replaces only the High-level loads. Below High (`SCATTER_LEVEL` < 3) the game
//! reads its own Low and Medium constants, so the governor treats this knob as disabled there.

use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};

use log::Level;

use super::{SCATTER, SCATTER_STEPS};
use crate::config::KnobsCfg;
use crate::entry;
use crate::patch::{alloc_near_all, rel32, rel32_target, write_checked, PatchError, Patched};
use crate::scan::{find_in_sections, main_module, readable_run, walk_readable, Module, Pattern};
use crate::sigs::{Sig, SCATTER_BRICK_LOAD, SCATTER_INSTANCE_LOADS};

/// May the scatter capacity be treated as larger than the game's? No: the audit proved no
/// headroom (docs/re-notes.md, "Scatter capacity audit" and its correction), so the knob's
/// ceiling is the game's 90 m.
pub const CAP_OK: bool = false;

/// The capacity sites the audit accepted, as offsets from their signatures. Empty: the audit
/// accepted none, and nothing writes a capacity site.
pub const CAP_SITES: &[usize] = &[];

/// Address of the float both loads read once [`install`] has run, or 0 before. `install` stores
/// it (`Release`) after the float holds 90.0 and before any load points at it; [`set_distance`]
/// loads it (`Acquire`).
pub static SCATTER_FLOAT: AtomicUsize = AtomicUsize::new(0);

/// The bits of 90.0: the game's High distance, and the only value [`install`] accepts.
const VANILLA_BITS: u32 = 90.0f32.to_bits();

/// The highest step index of [`SCATTER_STEPS`].
const LAST_STEP: u8 = 4;
const _: () = assert!(SCATTER_STEPS.len() == LAST_STEP as usize + 1);

/// The nearest and the farthest distance, in metres, that the page float may hold: the first and
/// the last step.
const DISTANCE_RANGE: (f32, f32) = {
    let [first, .., last] = SCATTER_STEPS;
    (first, last)
};
const _: () = assert!(DISTANCE_RANGE.0 <= DISTANCE_RANGE.1);

/// Why [`install`] refused: the two loads read two different addresses.
const DIFFERENT_VALUES: &str = "the two grass distance loads read different values (changed by \
                                another tool or a different build)";
/// Why [`install`] refused: the value both loads read is not exactly 90.0.
const NOT_90: &str = "the grass distance value is not the game's 90 m (changed by another tool \
                      or a different build)";
/// Why [`install`] refused: a load reads an address that is not readable inside the game's image
/// (for example a page of another tool, or of this mod if it was installed already).
const OUTSIDE_IMAGE: &str = "a grass distance load reads outside the game's image (changed by \
                             another tool or a different build)";
/// Why [`install`] stopped: `alloc_near_all` found no block that a rel32 from both loads reaches.
const NO_PAGE: &str = "no free memory within 2 GiB of the game code for the distance value";

/// The little-endian disp32 that makes a RIP-relative load read `new_target`. Pure.
///
/// `insn_end` is the address after the load instruction, and `disp_addr` the address of its
/// disp32. In both distance loads the disp32 is the last four bytes of the instruction, so
/// `disp_addr + 4` must be `insn_end`. `None` if it is not, or if `new_target` is more than 2 GiB
/// behind or 2 GiB - 1 ahead of `insn_end`.
pub fn redirect_disp32(insn_end: usize, disp_addr: usize, new_target: usize) -> Option<[u8; 4]> {
    if disp_addr.checked_add(4)? != insn_end {
        return None;
    }
    Some(rel32(insn_end, new_target)?.to_le_bytes())
}

/// The highest step the governor may use for this knob: `cfg_max` (`MaxScatterDistance`), cut at
/// the last step. 0 when `!cap_ok`, because 90 m is the limit without proven capacity headroom.
///
/// The governor also treats the knob as disabled unless `SCATTER_LEVEL` is 3 (module docs).
pub fn max_step(cap_ok: bool, cfg_max: u8) -> u8 {
    if cap_ok {
        cfg_max.min(LAST_STEP)
    } else {
        0
    }
}

/// `m` kept inside [`DISTANCE_RANGE`]; NaN gives the first step.
fn clamp_distance(m: f32) -> f32 {
    let (near, far) = DISTANCE_RANGE;
    if m.is_nan() {
        near
    } else {
        m.clamp(near, far)
    }
}

/// Set the grass distance in metres. The game reads only the page float: this is one aligned
/// 4-byte store to it (nothing if [`install`] has not run), mirrored into `SCATTER` for the log.
///
/// The value is kept inside the steps, 90 m to 127 m (NaN: 90 m), so it is never below the
/// game's own High distance. Safe from any thread: an atomic store, no lock, no allocation.
pub fn set_distance(m: f32) {
    let m = clamp_distance(m);
    let page = SCATTER_FLOAT.load(Ordering::Acquire);
    if page != 0 {
        // SAFETY: a non-zero `SCATTER_FLOAT` is the address `point_loads_at_page_with` stored: the
        // start of a block that `alloc_near_all` committed read-write, 64 KiB aligned (so aligned
        // for a u32), and never freed, so it is valid for reads and writes for the rest of the
        // process. Its only other writer is that function's first write, which comes before the
        // address is published (`Release` there, `Acquire` above). Its only readers are the
        // game's two distance loads: each a 4-byte `movss` from this aligned address, which
        // x86-64 performs as one access, so the game sees the old float or the new one.
        let float = unsafe { AtomicU32::from_ptr(page as *mut u32) };
        float.store(m.to_bits(), Ordering::Relaxed);
    }
    SCATTER.set(m);
}

/// Ruling 7 of the plan: patch only the game's own state. Pure.
///
/// `brick` and `instance` are each load's current target and the f32 bits found there. Returns
/// the shared target if both read the same address and it holds exactly 90.0 (bits 0x42b40000);
/// otherwise why not.
fn vanilla_target(brick: (usize, u32), instance: (usize, u32)) -> Result<usize, &'static str> {
    if brick.0 != instance.0 {
        return Err(DIFFERENT_VALUES);
    }
    if brick.1 != VANILLA_BITS || instance.1 != VANILLA_BITS {
        return Err(NOT_90);
    }
    Ok(brick.0)
}

/// One distance load, as [`find_loads`] found it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Load {
    /// The signature's name, for the log.
    name: &'static str,
    /// Address of the load's disp32: the last four bytes of the instruction.
    disp_addr: usize,
    /// The disp32 as it was read.
    old: [u8; 4],
}

impl Load {
    /// The address after the load instruction.
    fn insn_end(&self) -> Option<usize> {
        self.disp_addr.checked_add(4)
    }

    /// The address the load reads with its disp32 as it was read.
    fn target(&self) -> Option<usize> {
        rel32_target(self.insn_end()?, i32::from_le_bytes(self.old))
    }
}

/// Find `sig` in `m` and read the disp32 at its patch offset.
///
/// # Safety
///
/// Start-up only, as `scan::find_in_sections`: no other thread writes or re-protects `m`'s
/// sections meanwhile.
unsafe fn find_load(m: &Module, sig: &Sig) -> Result<Load, String> {
    let name = sig.name;
    let pattern = Pattern::parse(sig.pattern).map_err(|error| format!("{name}: {error}"))?;
    let offset = sig.patch_offset();
    // The disp32 must lie inside the match, so that the scan has proved it readable.
    if offset.checked_add(4).is_none_or(|end| end > pattern.len()) {
        return Err(format!("{name}: the disp32 is not inside the signature"));
    }
    let found = find_in_sections(m, sig.sections, &pattern).map_err(|error| {
        log::debug!("grass distance: {name}: {error}");
        error.player_text()
    })?;
    let disp_addr = found
        .checked_add(offset)
        .ok_or_else(|| format!("{name}: the disp32 is past the end of memory"))?;
    // SAFETY: `find_in_sections` found the pattern at `found`, in a section it checked committed
    // and readable, and `[disp_addr, disp_addr + 4)` lies inside the match (checked above). The
    // protection does not change meanwhile (start-up only: this function's contract). An
    // unaligned read needs no alignment.
    let old = unsafe { (disp_addr as *const [u8; 4]).read_unaligned() };
    log::debug!("grass distance: {name} matched at {found:#x}, disp32 at {disp_addr:#x}");
    Ok(Load {
        name,
        disp_addr,
        old,
    })
}

/// Find both distance loads in `m`: the brick load, then the instance load.
///
/// # Safety
///
/// As [`find_load`].
unsafe fn find_loads(m: &Module) -> Result<[Load; 2], String> {
    // SAFETY: this function's contract is `find_load`'s.
    unsafe {
        Ok([
            find_load(m, &SCATTER_BRICK_LOAD)?,
            find_load(m, &SCATTER_INSTANCE_LOADS)?,
        ])
    }
}

/// The u32 at `addr` if all four bytes are committed and readable inside `m`'s own allocation
/// (for an exe, its image); `None` otherwise.
///
/// # Safety
///
/// Start-up only, as `scan::find_in_sections`: the protection does not change between the check
/// and the read.
unsafe fn read_in_image(m: &Module, addr: usize) -> Option<u32> {
    if !walk_readable(addr, 4, |at| readable_run(at, m.base)) {
        return None;
    }
    // SAFETY: `walk_readable` just reported all four bytes committed and readable; this
    // function's contract keeps them so for the read. An unaligned read needs no alignment.
    Some(unsafe { (addr as *const u32).read_unaligned() })
}

/// Each load's current target and the bits there, then [`vanilla_target`]: the shared target, or
/// why not.
///
/// # Safety
///
/// As [`read_in_image`].
unsafe fn check_vanilla(m: &Module, loads: &[Load; 2]) -> Result<usize, String> {
    let read = |load: &Load| -> Result<(usize, u32), &'static str> {
        let target = load.target().ok_or(OUTSIDE_IMAGE)?;
        // SAFETY: this function's contract is `read_in_image`'s.
        let bits = unsafe { read_in_image(m, target) }.ok_or(OUTSIDE_IMAGE)?;
        Ok((target, bits))
    };
    let [brick, instance] = loads;
    Ok(vanilla_target(read(brick)?, read(instance)?)?)
}

/// [`point_loads_at_page_with`], every write done by `write_checked`.
///
/// # Safety
///
/// As [`point_loads_at_page_with`].
unsafe fn point_loads_at_page(loads: &[Load; 2], page: usize) -> Result<(), String> {
    let write = |addr: usize, old: &[u8; 4], new: &[u8; 4]| {
        // SAFETY: `point_loads_at_page_with` calls this only for the page float and the two
        // loads' disp32s, which this function's contract covers.
        unsafe { write_checked(addr, old, new) }
    };
    // SAFETY: this function's contract is `point_loads_at_page_with`'s.
    unsafe { point_loads_at_page_with(loads, page, write) }
}

/// Write 90.0 into the page float, publish its address in [`SCATTER_FLOAT`], then point each
/// load's disp32 at it, in that order (plan ruling 9). A thread that ran a load meanwhile would
/// read 90.0 either way. `write(addr, old, new)` does each write: the page float (old: the
/// zeros of a fresh page), then the brick disp32, then the instance disp32, then (only if the
/// instance write failed) the brick disp32 back. Production passes `write_checked`; a test can
/// watch or fail each write.
///
/// Every new disp32 is worked out first, so a page out of reach writes nothing at all. A page
/// that is not zero is refused before anything is published. If a disp32 write fails, the address
/// is taken back (0) and a load already redirected is put back, so the governor never moves just
/// one of the two loads. `Err` is the reason.
///
/// # Safety
///
/// - `page` is the start of a block from `alloc_near_all` that nothing else uses (at least 4
///   bytes, read-write, zero-filled), or an address that no rel32 from the loads reaches (refused
///   before any write).
/// - `write_checked`'s contract for the page float and each load's disp32: start-up only, and no
///   thread runs the load instructions meanwhile. `write` must be safe to call on those ranges.
unsafe fn point_loads_at_page_with(
    loads: &[Load; 2],
    page: usize,
    mut write: impl FnMut(usize, &[u8; 4], &[u8; 4]) -> Result<Patched, PatchError>,
) -> Result<(), String> {
    let mut new = [[0u8; 4]; 2];
    for (slot, load) in new.iter_mut().zip(loads) {
        let (name, at) = (load.name, load.disp_addr);
        let insn_end = load
            .insn_end()
            .ok_or_else(|| format!("{name}: the load at {at:#x} ends past the end of memory"))?;
        *slot = redirect_disp32(insn_end, at, page).ok_or_else(|| {
            format!("{name}: the distance page at {page:#x} is out of reach of the load at {at:#x}")
        })?;
    }
    // The page float first. `page` is a fresh block (this function's contract), so it holds zeros
    // and no load reads it yet; anything else is refused before the address is published.
    write(page, &[0; 4], &VANILLA_BITS.to_le_bytes())
        .map_err(|error| format!("the distance page at {page:#x} could not be written: {error}"))?;
    SCATTER_FLOAT.store(page, Ordering::Release);

    let [brick, instance] = loads;
    let [brick_new, instance_new] = new;
    // The disp32 writes and the undo: each range is the four disp32 bytes of a load instruction
    // in the game's `.text`. A disp32 store is not atomic (these are not 4-byte aligned:
    // 0x14007950b and 0x14007693a), and that is fine here: this runs from `InitializeASI`, before
    // the game's `WinMain`, so no game thread runs these instructions yet (this function's
    // contract). `old` and the new bytes are locals, not the target memory.
    if let Err(error) = write(brick.disp_addr, &brick.old, &brick_new) {
        SCATTER_FLOAT.store(0, Ordering::Release);
        return Err(format!(
            "{}: the load at {:#x} was not redirected: {error}",
            brick.name, brick.disp_addr
        ));
    }
    if let Err(error) = write(instance.disp_addr, &instance.old, &instance_new) {
        SCATTER_FLOAT.store(0, Ordering::Release);
        // The brick disp32 holds `brick_new`, written a moment ago.
        let undo = write(brick.disp_addr, &brick_new, &brick.old);
        let brick_now = match undo {
            Ok(_) => "the brick load was put back".to_owned(),
            Err(undo_error) => format!(
                "the brick load could not be put back ({undo_error}), so it reads the mod's page, \
                 which holds 90.0"
            ),
        };
        return Err(format!(
            "{}: the load at {:#x} was not redirected: {error}; {brick_now}",
            instance.name, instance.disp_addr
        ));
    }
    Ok(())
}

/// Point both High-level grass distance loads at a float in the mod's own page, which holds 90.0.
///
/// In order: find both loads and read their disp32s; refuse unless both read the same address
/// inside the image and it holds exactly 90.0 (`vanilla_target`); allocate a page that both loads
/// reach; then `point_loads_at_page`. `Err` is the reason for the log. On an `Err` the game code
/// is as it was, except in the one case the text names. No capacity site is written. The page
/// is never freed, also when a later step fails (`alloc_near_all`'s rule).
///
/// # Safety
///
/// Start-up only: call it from `entry::start` (in `InitializeASI`, before the game's `WinMain`),
/// on one thread, with `m` the running exe (`scan::main_module()`). No game thread runs the two
/// loads yet, so their disp32s can be rewritten. A test may pass memory of its own process that no
/// thread executes.
pub unsafe fn install(m: &Module) -> Result<(), String> {
    // SAFETY: this function's contract is `install_with`'s, and `alloc_near_all` gives a fresh,
    // zero-filled, read-write block that nothing else knows.
    unsafe { install_with(m, alloc_near_all) }
}

/// [`install`], with the page allocator passed in: `alloc(targets, size)` is `alloc_near_all`'s
/// shape. It is asked once, with the ends of both load instructions, so a test can see that a
/// page must be in reach of both.
///
/// # Safety
///
/// [`install`]'s contract. `alloc` returns `None`, or the start of a fresh, zero-filled,
/// read-write block of at least `size` bytes that nothing else uses.
unsafe fn install_with(
    m: &Module,
    alloc: impl FnOnce(&[usize], usize) -> Option<usize>,
) -> Result<(), String> {
    // SAFETY: this function's start-up contract is the one `find_loads` and `check_vanilla` need.
    let loads = unsafe { find_loads(m) }?;
    // SAFETY: as above.
    let target = unsafe { check_vanilla(m, &loads) }?;
    log::debug!("grass distance: both loads read 90.0 at {target:#x}");
    // Both loads must reach the page: their ends are 0x2bd1 bytes apart in build f68140a3.
    let [brick, instance] = &loads;
    let ends = [
        instance.insn_end().ok_or(NO_PAGE)?,
        brick.insn_end().ok_or(NO_PAGE)?,
    ];
    let page = alloc(&ends, size_of::<u32>()).ok_or(NO_PAGE)?;
    log::debug!("grass distance: page at {page:#x}");
    // SAFETY: `page` is a fresh, zero-filled block that nothing else knows (this function's
    // contract for `alloc`); the rest is this function's start-up contract.
    unsafe { point_loads_at_page(&loads, page) }
}

/// The body of [`init`], with the code-patch answer and the install passed in. `install` runs only
/// when code patches are allowed and `EnableScatterDistance = 1`. Returns the log level and the one
/// line to log.
fn start_up(
    knobs: &KnobsCfg,
    code_patches: bool,
    install: impl FnOnce() -> Result<(), String>,
) -> (Level, String) {
    if !code_patches {
        return (
            Level::Info,
            "grass distance: off: code patches are off on this build".to_owned(),
        );
    }
    if !knobs.enable_scatter_distance {
        return (
            Level::Info,
            "grass distance: off: EnableScatterDistance = 0".to_owned(),
        );
    }
    match install() {
        Ok(()) => (
            Level::Info,
            "grass distance: redirect installed at 90 m (the game's value); the knob stays at \
             90 m in this version, because no scatter capacity headroom is proven"
                .to_owned(),
        ),
        Err(why) => (Level::Warn, format!("grass distance: off: {why}")),
    }
}

/// The start-up step: decide, install if it is right to, and log one line. Called once, from
/// `entry::start`, after `code_patches_allowed` is set. It uses no MinHook hook.
pub fn init(knobs: &KnobsCfg) {
    let (level, line) = start_up(knobs, entry::code_patches_allowed(), || {
        // SAFETY: `init` runs from `entry::start`, in `InitializeASI`, before the game's
        // `WinMain`: one thread, and no game thread runs the two loads yet. `main_module()` is
        // the running exe.
        unsafe { install(&main_module()) }
    });
    log::log!(level, "{line}");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::knobs::{SCATTER, SCATTER_STEPS};
    use crate::patch::test_support::Mem;
    use crate::patch::{alloc_near_all, PatchError};
    use crate::scan::{main_module, Module};
    use crate::sigs::{
        Sig, SCATTER_BRICK_LOAD, SCATTER_CAP_ALLOC, SCATTER_CAP_CONST, SCATTER_INSTANCE_LOADS,
    };
    use log::Level;
    use std::cell::Cell;
    use std::sync::atomic::Ordering::{Acquire, Relaxed};
    use std::sync::{Mutex, MutexGuard, PoisonError};

    /// `SCATTER_FLOAT` and `SCATTER` are process-wide and tests run in parallel, so every test
    /// that uses them holds this lock.
    static GLOBALS: Mutex<()> = Mutex::new(());

    /// Holds `GLOBALS`. A test starts with no page published. On drop the address goes back to 0
    /// before the lock is let go, so no other test sees the address of a page this test freed.
    struct Globals {
        _lock: MutexGuard<'static, ()>,
    }

    impl Drop for Globals {
        fn drop(&mut self) {
            SCATTER_FLOAT.store(0, Relaxed);
        }
    }

    fn globals() -> Globals {
        let lock = GLOBALS.lock().unwrap_or_else(PoisonError::into_inner);
        SCATTER_FLOAT.store(0, Relaxed);
        SCATTER.set(90.0);
        Globals { _lock: lock }
    }

    // ---- the values the plan gives verbatim ---------------------------------------------------

    #[test]
    fn disp32_points_at_new_target() {
        // The plan's text has `target = 0x1500_0000`: 5.0 GB below `insn_end`, which no disp32
        // can reach, so its assert could never hold. 0x1_5000_0000 is 256 MiB above instead.
        let insn_end = 0x1400_7693e;
        let target = 0x1_5000_0000;
        let d = redirect_disp32(insn_end, insn_end - 4, target).unwrap();
        assert_eq!(
            i32::from_le_bytes(d) as i64,
            target as i64 - insn_end as i64
        );
    }

    #[test]
    fn out_of_range_target_refused() {
        assert!(redirect_disp32(0x1400_0000, 0x1400_0000 - 4, 0x3_0000_0000).is_none());
    }

    #[test]
    fn a_disp32_that_does_not_end_its_instruction_is_refused() {
        // Both loads end with their disp32 (no immediate after it), so the disp32 is the last
        // four bytes. Anything else is a caller's mistake, not a site to write.
        let (insn_end, reachable) = (0x1400_7693e, 0x1_5000_0000);
        assert!(redirect_disp32(insn_end, insn_end - 4, reachable).is_some());
        assert!(redirect_disp32(insn_end, insn_end - 5, reachable).is_none());
        assert!(redirect_disp32(insn_end, insn_end, reachable).is_none());
        assert!(redirect_disp32(2, usize::MAX - 1, 0).is_none());
    }

    #[test]
    fn no_capacity_means_no_steps() {
        assert_eq!(max_step(false, 4), 0);
        assert_eq!(max_step(true, 4), 4);
    }

    #[test]
    fn max_step_never_passes_the_last_step() {
        assert_eq!(max_step(true, 200), 4);
        assert_eq!(max_step(false, 200), 0);
        assert_eq!(max_step(true, 0), 0);
    }

    // ---- ruling 1-3: no capacity patch -------------------------------------------------------

    #[test]
    fn scatter_step_0_is_the_vanilla_distance() {
        // Replaces the plan's `scatter_steps_respect_capacity_rule` (dispatch ruling 3).
        assert_eq!(SCATTER_STEPS[0], 90.0);
        assert_eq!(VANILLA_BITS, 0x42b4_0000);
        assert_eq!(f32::from_bits(VANILLA_BITS), SCATTER_STEPS[0]);
    }

    #[test]
    fn the_capacity_audit_gives_no_capacity_site() {
        // A constant, so it is checked when the test builds: with `CAP_OK = true` it does not.
        const { assert!(!CAP_OK, "no scatter capacity headroom is proven (re-notes)") };
        assert!(CAP_SITES.is_empty());
        assert_eq!(
            max_step(CAP_OK, Config::default().knobs.max_scatter_distance),
            0
        );
    }

    // ---- ruling 7: patch only the vanilla state ----------------------------------------------

    #[test]
    fn only_two_loads_of_the_same_90_are_vanilla() {
        let t = 0x1_413b_65fc;
        let v = VANILLA_BITS;
        assert_eq!(vanilla_target((t, v), (t, v)), Ok(t));
        assert_eq!(vanilla_target((t, v), (t + 4, v)), Err(DIFFERENT_VALUES));
        assert_eq!(vanilla_target((t + 4, v), (t, v)), Err(DIFFERENT_VALUES));
        for other in [100.0f32, 80.0, 0.0, -90.0, f32::NAN] {
            let b = other.to_bits();
            assert_eq!(vanilla_target((t, b), (t, b)), Err(NOT_90), "{other}");
        }
        // Bit for bit: the float next to 90.0 is not 90.0.
        let next = v + 1;
        assert_eq!(vanilla_target((t, next), (t, next)), Err(NOT_90));
        assert!(DIFFERENT_VALUES.ends_with("(changed by another tool or a different build)"));
        assert!(NOT_90.ends_with("(changed by another tool or a different build)"));
    }

    // ---- a synthetic image with the four scatter signatures ----------------------------------

    /// RVA of the brick load code, the instance load code, and the two capacity sites.
    const BRICK_RVA: usize = 0x1100;
    const INSTANCE_RVA: usize = 0x1200;
    const CAP_CONST_RVA: usize = 0x1300;
    const CAP_ALLOC_RVA: usize = 0x1400;
    /// RVA of the shared 90.0 constant (outside `.text`, inside the image), and of the 1.0 and
    /// 14.0 that the instance code's two other loads read.
    const CONST_RVA: usize = 0x3000;
    const ONE_RVA: usize = 0x3010;
    const FOURTEEN_RVA: usize = 0x3014;
    /// RVAs of the two disp32s that install may change.
    const BRICK_DISP: usize = BRICK_RVA + 9;
    const INSTANCE_DISP: usize = INSTANCE_RVA + 30;

    /// A PE32+ header block with one section, `.text`, at RVA 0x1000, `0x1000` bytes long.
    fn text_only_header() -> Vec<u8> {
        let mut b = vec![0u8; 0x400];
        b[0..2].copy_from_slice(b"MZ");
        b[0x3c..0x40].copy_from_slice(&0x80u32.to_le_bytes());
        b[0x80..0x84].copy_from_slice(b"PE\0\0");
        let fh = 0x84;
        b[fh..fh + 2].copy_from_slice(&0x8664u16.to_le_bytes());
        b[fh + 2..fh + 4].copy_from_slice(&1u16.to_le_bytes());
        b[fh + 16..fh + 18].copy_from_slice(&240u16.to_le_bytes());
        let opt = fh + 20;
        b[opt..opt + 2].copy_from_slice(&0x20bu16.to_le_bytes());
        b[opt + 24..opt + 32].copy_from_slice(&0x1_4000_0000u64.to_le_bytes());
        b[opt + 56..opt + 60].copy_from_slice(&0x4000u32.to_le_bytes());
        b[opt + 60..opt + 64].copy_from_slice(&0x400u32.to_le_bytes());
        let section = opt + 240;
        b[section..section + 5].copy_from_slice(b".text");
        b[section + 8..section + 12].copy_from_slice(&0x1000u32.to_le_bytes());
        b[section + 12..section + 16].copy_from_slice(&0x1000u32.to_le_bytes());
        b[section + 16..section + 20].copy_from_slice(&0x1000u32.to_le_bytes());
        b[section + 20..section + 24].copy_from_slice(&0x400u32.to_le_bytes());
        b
    }

    /// The bytes of `sig`'s pattern with every `??` as 0, placed at `code_rva`, and each disp32
    /// in `disps` (`(offset in the pattern, target RVA)`) set to reach its target.
    fn code(sig: &Sig, code_rva: usize, disps: &[(usize, usize)]) -> Vec<u8> {
        let mut b: Vec<u8> = sig
            .pattern
            .split_ascii_whitespace()
            .map(|t| {
                if t == "??" {
                    0
                } else {
                    u8::from_str_radix(t, 16).unwrap()
                }
            })
            .collect();
        for &(at, target_rva) in disps {
            let end = code_rva + at + 4;
            let d = i32::try_from(target_rva as i64 - end as i64).unwrap();
            b[at..at + 4].copy_from_slice(&d.to_le_bytes());
        }
        b
    }

    /// Four read-write pages laid out like the game: both loads read the one 90.0 constant, and
    /// the two capacity sites hold 0x14000.
    fn image() -> (Mem, Module) {
        let mem = Mem::rw(0x4000);
        mem.put(0, &text_only_header());
        mem.put(
            BRICK_RVA,
            &code(&SCATTER_BRICK_LOAD, BRICK_RVA, &[(9, CONST_RVA)]),
        );
        mem.put(
            INSTANCE_RVA,
            &code(
                &SCATTER_INSTANCE_LOADS,
                INSTANCE_RVA,
                &[(13, ONE_RVA), (21, FOURTEEN_RVA), (30, CONST_RVA)],
            ),
        );
        mem.put(CAP_CONST_RVA, &code(&SCATTER_CAP_CONST, CAP_CONST_RVA, &[]));
        mem.put(CAP_ALLOC_RVA, &code(&SCATTER_CAP_ALLOC, CAP_ALLOC_RVA, &[]));
        mem.put(CONST_RVA, &90.0f32.to_le_bytes());
        mem.put(ONE_RVA, &1.0f32.to_le_bytes());
        mem.put(FOURTEEN_RVA, &14.0f32.to_le_bytes());
        let module = Module {
            base: mem.base,
            size: 0x4000,
        };
        (mem, module)
    }

    /// Every byte of the image.
    fn all(mem: &Mem) -> Vec<u8> {
        mem.get(0, 0x4000)
    }

    /// The offsets where `before` and `after` differ.
    fn changed(before: &[u8], after: &[u8]) -> Vec<usize> {
        (0..before.len())
            .filter(|&i| before[i] != after[i])
            .collect()
    }

    /// The address a disp32 at `disp_rva` reaches now.
    fn target_of(mem: &Mem, disp_rva: usize) -> usize {
        let d = i32::from_le_bytes(mem.get(disp_rva, 4).try_into().unwrap());
        (mem.base + disp_rva + 4)
            .checked_add_signed(d as isize)
            .unwrap()
    }

    fn read_f32(addr: usize) -> f32 {
        // SAFETY: test code; the callers pass the address of a live, readable f32.
        unsafe { (addr as *const f32).read_unaligned() }
    }

    /// `install`, on memory of this test process.
    fn install_here(m: &Module) -> Result<(), String> {
        // SAFETY: the tests pass a `Module` for a synthetic image (read-write memory of this
        // process, described by a header the test wrote) or for the test exe, whose `.text` has
        // no such code. No thread runs any byte that can be written.
        unsafe { install(m) }
    }

    #[test]
    fn the_image_holds_the_game_layout() {
        let (mem, module) = image();
        assert_eq!(target_of(&mem, BRICK_DISP), module.base + CONST_RVA);
        assert_eq!(target_of(&mem, INSTANCE_DISP), module.base + CONST_RVA);
        assert_eq!(read_f32(module.base + CONST_RVA), 90.0);
        assert_eq!(mem.get(CAP_CONST_RVA + 12, 4), [0x00, 0x40, 0x01, 0x00]);
        assert_eq!(mem.get(CAP_ALLOC_RVA + 12, 4), [0x00, 0x40, 0x01, 0x00]);
    }

    // ---- ruling 8 and 9: the install ---------------------------------------------------------

    #[test]
    fn install_points_both_loads_at_a_page_that_holds_90_and_writes_nothing_else() {
        let _g = globals();
        let (mem, module) = image();
        let before = all(&mem);
        assert_eq!(install_here(&module), Ok(()));
        let page = SCATTER_FLOAT.load(Relaxed);
        assert_ne!(page, 0, "the page is published");
        assert_eq!(read_f32(page), 90.0, "installing changes nothing visible");
        assert_eq!(target_of(&mem, BRICK_DISP), page);
        assert_eq!(target_of(&mem, INSTANCE_DISP), page);
        // Only bytes of the two disp32s changed: not the capacity sites (ruling 3), not the
        // shared 90.0 constant (ruling 7), not the instance code's other two loads. (The page is
        // near, so a disp32's high bytes can stay the same.)
        let diff = changed(&before, &all(&mem));
        let brick = BRICK_DISP..BRICK_DISP + 4;
        let instance = INSTANCE_DISP..INSTANCE_DISP + 4;
        assert!(
            diff.iter()
                .all(|i| brick.contains(i) || instance.contains(i)),
            "changed: {diff:x?}"
        );
        assert!(diff.iter().any(|i| brick.contains(i)), "{diff:x?}");
        assert!(diff.iter().any(|i| instance.contains(i)), "{diff:x?}");
        assert_eq!(read_f32(module.base + CONST_RVA), 90.0);
    }

    #[test]
    fn install_writes_no_capacity_site() {
        let _g = globals();
        let (mem, module) = image();
        let cap = [0x00, 0x40, 0x01, 0x00];
        assert_eq!(install_here(&module), Ok(()));
        assert_eq!(mem.get(CAP_CONST_RVA + 12, 4), cap);
        assert_eq!(mem.get(CAP_ALLOC_RVA + 12, 4), cap);
    }

    #[test]
    fn install_refuses_a_value_that_is_not_90_and_writes_nothing() {
        let _g = globals();
        let (mem, module) = image();
        mem.put(CONST_RVA, &100.0f32.to_le_bytes());
        let before = all(&mem);
        assert_eq!(install_here(&module), Err(NOT_90.to_owned()));
        assert_eq!(all(&mem), before);
        assert_eq!(SCATTER_FLOAT.load(Relaxed), 0);
    }

    #[test]
    fn install_refuses_two_different_targets_and_writes_nothing() {
        let _g = globals();
        let (mem, module) = image();
        // A second 90.0, and the brick load pointed at it.
        mem.put(CONST_RVA + 0x20, &90.0f32.to_le_bytes());
        mem.put(
            BRICK_RVA,
            &code(&SCATTER_BRICK_LOAD, BRICK_RVA, &[(9, CONST_RVA + 0x20)]),
        );
        let before = all(&mem);
        assert_eq!(install_here(&module), Err(DIFFERENT_VALUES.to_owned()));
        assert_eq!(all(&mem), before);
        assert_eq!(SCATTER_FLOAT.load(Relaxed), 0);
    }

    #[test]
    fn install_refuses_a_load_that_reads_outside_the_image() {
        let _g = globals();
        let (mem, module) = image();
        // The brick load points past the end of the image: not readable inside its allocation.
        mem.put(
            BRICK_RVA,
            &code(&SCATTER_BRICK_LOAD, BRICK_RVA, &[(9, 0x4000)]),
        );
        let before = all(&mem);
        assert_eq!(install_here(&module), Err(OUTSIDE_IMAGE.to_owned()));
        assert_eq!(all(&mem), before);
        assert_eq!(SCATTER_FLOAT.load(Relaxed), 0);
    }

    #[test]
    fn a_second_install_refuses_because_the_loads_read_the_mods_page() {
        let _g = globals();
        let (mem, module) = image();
        assert_eq!(install_here(&module), Ok(()));
        let page = SCATTER_FLOAT.load(Relaxed);
        let after_first = all(&mem);
        // Another copy of the mod, or a second call: the loads read a page outside the image.
        assert_eq!(install_here(&module), Err(OUTSIDE_IMAGE.to_owned()));
        assert_eq!(all(&mem), after_first);
        assert_eq!(SCATTER_FLOAT.load(Relaxed), page, "the first page stays");
    }

    #[test]
    fn install_in_this_exe_finds_nothing_and_says_why() {
        let _g = globals();
        let why = install_here(&main_module()).unwrap_err();
        assert!(why.contains("not found"), "{why}");
        assert_eq!(SCATTER_FLOAT.load(Relaxed), 0);
    }

    #[test]
    fn install_asks_for_a_page_that_both_loads_reach() {
        let _g = globals();
        let (mem, module) = image();
        let mut asked = Vec::new();
        let alloc = |targets: &[usize], size: usize| {
            asked.push((targets.to_vec(), size));
            alloc_near_all(targets, size)
        };
        // SAFETY: as in `install_here`; `alloc` hands on the fresh block `alloc_near_all` gives.
        assert_eq!(unsafe { install_with(&module, alloc) }, Ok(()));
        assert_eq!(asked.len(), 1, "one page");
        let (targets, size) = &asked[0];
        // The ends of both load instructions (0x14007950f and 0x14007693e in the game).
        let mut sorted = targets.clone();
        sorted.sort_unstable();
        assert_eq!(
            sorted,
            [
                module.base + BRICK_DISP + 4,
                module.base + INSTANCE_DISP + 4
            ]
        );
        assert_eq!(*size, 4, "one f32");
        let page = SCATTER_FLOAT.load(Acquire);
        assert_eq!(target_of(&mem, BRICK_DISP), page);
        assert_eq!(target_of(&mem, INSTANCE_DISP), page);
    }

    #[test]
    fn no_page_in_reach_refuses_and_changes_nothing() {
        let _g = globals();
        let (mem, module) = image();
        let before = all(&mem);
        let no_page = |_: &[usize], _: usize| None;
        // SAFETY: as in `install_here`; the allocator gives no page, so nothing is written.
        let result = unsafe { install_with(&module, no_page) };
        assert_eq!(result, Err(NO_PAGE.to_owned()));
        assert_eq!(all(&mem), before);
        assert_eq!(SCATTER_FLOAT.load(Acquire), 0);
    }

    /// A page that both of the image's loads reach, as `install` gets one. The `Mem` frees it when
    /// the test drops it.
    fn page_near(module: &Module) -> Mem {
        let ends = [
            module.base + BRICK_DISP + 4,
            module.base + INSTANCE_DISP + 4,
        ];
        let base = alloc_near_all(&ends, 4).unwrap();
        // SAFETY: `alloc_near_all` returns the base of a fresh VirtualAlloc block of at least 4
        // bytes, and nothing else frees it.
        unsafe { Mem::adopt(base, 4) }
    }

    #[test]
    fn a_disp32_past_the_end_of_the_signature_is_refused_before_the_scan() {
        let (mem, module) = image();
        let before = all(&mem);
        // The match is 17 bytes, so a disp32 at +14 would end one byte past it.
        let outside = Sig {
            patch_offsets: &[14],
            ..SCATTER_BRICK_LOAD
        };
        // SAFETY: as in `install_here`.
        let why = unsafe { find_load(&module, &outside) }.unwrap_err();
        assert_eq!(
            why,
            "SCATTER_BRICK_LOAD: the disp32 is not inside the signature"
        );
        // The last four bytes of the match are inside it: accepted, and read from the image.
        let last = Sig {
            patch_offsets: &[13],
            ..SCATTER_BRICK_LOAD
        };
        // SAFETY: as in `install_here`.
        let load = unsafe { find_load(&module, &last) }.unwrap();
        assert_eq!(load.disp_addr, module.base + BRICK_RVA + 13);
        assert_eq!(load.old, [0x48, 0x83, 0xc4, 0x28]);
        assert_eq!(all(&mem), before);
    }

    /// One write that `point_loads_at_page_with` asked for, and what had been published then.
    #[derive(Debug, PartialEq)]
    struct Seen {
        /// Where the write goes.
        addr: usize,
        /// `SCATTER_FLOAT` just before the write.
        published: usize,
        /// The page float just before the write.
        page_value: f32,
    }

    #[test]
    fn the_page_holds_90_and_is_published_before_either_disp32_is_written() {
        // Plan ruling 9: page float, then the publish, then the two disp32s.
        let _g = globals();
        let (mem, module) = image();
        // SAFETY: as in `install_here`.
        let loads = unsafe { find_loads(&module) }.unwrap();
        let page = page_near(&module);
        let mut seen = Vec::new();
        let write = |addr: usize, old: &[u8; 4], new: &[u8; 4]| {
            seen.push(Seen {
                addr,
                published: SCATTER_FLOAT.load(Acquire),
                page_value: read_f32(page.base),
            });
            // SAFETY: `point_loads_at_page_with` passes the page float or a disp32 of the
            // synthetic image; nothing runs either.
            unsafe { write_checked(addr, old, new) }
        };
        // SAFETY: as in `install_here`; `page` is a fresh block that both loads reach, and the
        // write function passes every write on to `write_checked` unchanged.
        let result = unsafe { point_loads_at_page_with(&loads, page.base, write) };
        assert_eq!(result, Ok(()));
        let (brick, instance) = (module.base + BRICK_DISP, module.base + INSTANCE_DISP);
        assert_eq!(
            seen,
            [
                // The page float, before anything is published.
                Seen {
                    addr: page.base,
                    published: 0,
                    page_value: 0.0
                },
                // Then each disp32, with the page published and holding 90.0.
                Seen {
                    addr: brick,
                    published: page.base,
                    page_value: 90.0
                },
                Seen {
                    addr: instance,
                    published: page.base,
                    page_value: 90.0
                },
            ]
        );
        assert_eq!(SCATTER_FLOAT.load(Acquire), page.base);
        assert_eq!(target_of(&mem, BRICK_DISP), page.base);
        assert_eq!(target_of(&mem, INSTANCE_DISP), page.base);
    }

    #[test]
    fn a_failed_undo_leaves_the_brick_on_the_page_and_publishes_nothing() {
        let _g = globals();
        let (mem, module) = image();
        // SAFETY: as in `install_here`.
        let loads = unsafe { find_loads(&module) }.unwrap();
        let page = page_near(&module);
        let (brick, instance) = (module.base + BRICK_DISP, module.base + INSTANCE_DISP);
        let mut brick_writes = 0;
        // Fails the instance disp32 and the second brick write (the undo); passes the rest on.
        let write = |addr: usize, old: &[u8; 4], new: &[u8; 4]| {
            if addr == brick {
                brick_writes += 1;
                if brick_writes == 2 {
                    return Err(PatchError::Protect(5));
                }
            }
            if addr == instance {
                return Err(PatchError::Protect(5));
            }
            // SAFETY: `addr` is the page float or the brick disp32 of the synthetic image;
            // nothing runs either.
            unsafe { write_checked(addr, old, new) }
        };
        // SAFETY: as in the write-order test.
        let why = unsafe { point_loads_at_page_with(&loads, page.base, write) }.unwrap_err();
        assert_eq!(brick_writes, 2, "the redirect, then the undo");
        assert!(why.contains("SCATTER_INSTANCE_LOADS"), "{why}");
        assert!(why.contains("could not be put back"), "{why}");
        assert_eq!(SCATTER_FLOAT.load(Acquire), 0, "the address is taken back");
        assert_eq!(
            target_of(&mem, BRICK_DISP),
            page.base,
            "the brick reads the page"
        );
        assert_eq!(read_f32(page.base), 90.0, "which holds the game's value");
        assert_eq!(target_of(&mem, INSTANCE_DISP), module.base + CONST_RVA);
    }

    #[test]
    fn a_page_that_is_not_fresh_refuses_and_changes_nothing() {
        let _g = globals();
        let (mem, module) = image();
        // SAFETY: as in `install_here`.
        let loads = unsafe { find_loads(&module) }.unwrap();
        let before = all(&mem);
        let page = page_near(&module);
        page.put(0, &[1, 2, 3, 4]);
        // SAFETY: as in `install_here`; the page is refused before it is written.
        let why = unsafe { point_loads_at_page(&loads, page.base) }.unwrap_err();
        assert!(why.contains("the distance page"), "{why}");
        assert!(why.contains("not the expected ones"), "{why}");
        assert_eq!(page.get(0, 4), [1, 2, 3, 4]);
        assert_eq!(all(&mem), before);
        assert_eq!(SCATTER_FLOAT.load(Acquire), 0);
    }

    #[test]
    fn a_brick_disp32_changed_after_the_read_refuses_and_changes_nothing() {
        let _g = globals();
        let (mem, module) = image();
        // SAFETY: as in `install_here`.
        let loads = unsafe { find_loads(&module) }.unwrap();
        // Another thread or tool changes the brick disp32 between the read and the write.
        mem.put(BRICK_DISP, &[1, 2, 3, 4]);
        let before = all(&mem);
        let page = page_near(&module);
        // SAFETY: as in `install_here`; `page` is a fresh block that both loads reach.
        let why = unsafe { point_loads_at_page(&loads, page.base) }.unwrap_err();
        assert!(why.contains("SCATTER_BRICK_LOAD"), "{why}");
        assert!(why.contains("not the expected ones"), "{why}");
        assert_eq!(all(&mem), before, "nothing written");
        assert_eq!(SCATTER_FLOAT.load(Relaxed), 0, "no page published");
    }

    #[test]
    fn an_instance_disp32_changed_after_the_read_refuses_and_puts_the_brick_back() {
        let _g = globals();
        let (mem, module) = image();
        // SAFETY: as in `install_here`.
        let loads = unsafe { find_loads(&module) }.unwrap();
        mem.put(INSTANCE_DISP, &[1, 2, 3, 4]);
        let before = all(&mem);
        let page = page_near(&module);
        // SAFETY: as in the brick test.
        let why = unsafe { point_loads_at_page(&loads, page.base) }.unwrap_err();
        assert!(why.contains("SCATTER_INSTANCE_LOADS"), "{why}");
        assert!(why.contains("put back"), "{why}");
        assert_eq!(all(&mem), before, "the brick disp32 is the original again");
        assert_eq!(SCATTER_FLOAT.load(Relaxed), 0, "no page published");
    }

    #[test]
    fn a_page_out_of_reach_refuses_and_changes_nothing() {
        let _g = globals();
        let (mem, module) = image();
        // SAFETY: as in `install_here`.
        let loads = unsafe { find_loads(&module) }.unwrap();
        let before = all(&mem);
        // A "page" 4 GiB above the image: no rel32 reaches it. Nothing may be written there.
        let far = module.base + 0x1_0000_0000;
        // SAFETY: as in `install_here`; the address is refused before anything is written.
        let why = unsafe { point_loads_at_page(&loads, far) }.unwrap_err();
        assert!(why.contains("out of reach"), "{why}");
        assert_eq!(all(&mem), before);
        assert_eq!(SCATTER_FLOAT.load(Relaxed), 0);
    }

    // ---- set_distance ------------------------------------------------------------------------

    #[test]
    fn set_distance_with_no_page_only_mirrors() {
        let _g = globals();
        set_distance(110.0);
        assert_eq!(SCATTER.value(), 110.0);
        assert_eq!(SCATTER_FLOAT.load(Relaxed), 0);
    }

    #[test]
    fn set_distance_stores_into_the_page_and_mirrors() {
        let _g = globals();
        let (mem, module) = image();
        assert_eq!(install_here(&module), Ok(()));
        let page = SCATTER_FLOAT.load(Relaxed);
        set_distance(120.0);
        assert_eq!(read_f32(page), 120.0);
        assert_eq!(SCATTER.value(), 120.0);
        // What the game's loads read now.
        assert_eq!(read_f32(target_of(&mem, BRICK_DISP)), 120.0);
        assert_eq!(read_f32(target_of(&mem, INSTANCE_DISP)), 120.0);
        assert_eq!(
            read_f32(module.base + CONST_RVA),
            90.0,
            "the shared constant"
        );
    }

    #[test]
    fn set_distance_stays_inside_the_steps() {
        let _g = globals();
        let page = Mem::rw(0x1000);
        SCATTER_FLOAT.store(page.base, Relaxed);
        for (asked, got) in [
            (50.0f32, 90.0f32),
            (-1.0, 90.0),
            (f32::NAN, 90.0),
            (f32::NEG_INFINITY, 90.0),
            (127.0, 127.0),
            (500.0, 127.0),
            (f32::INFINITY, 127.0),
            (100.0, 100.0),
        ] {
            set_distance(asked);
            assert_eq!(read_f32(page.base), got, "{asked}");
            assert_eq!(SCATTER.value(), got, "{asked}");
        }
        SCATTER_FLOAT.store(0, Relaxed);
    }

    // ---- ruling 10: the start-up step --------------------------------------------------------

    fn knobs(scatter: bool) -> crate::config::KnobsCfg {
        let mut k = Config::default().knobs;
        k.enable_scatter_distance = scatter;
        k
    }

    #[test]
    fn no_code_patches_means_no_install_and_says_so() {
        let called = Cell::new(false);
        let (level, line) = start_up(&knobs(true), false, || {
            called.set(true);
            Ok(())
        });
        assert!(!called.get(), "install must not run");
        assert_eq!(level, Level::Info);
        assert_eq!(
            line,
            "grass distance: off: code patches are off on this build"
        );
    }

    #[test]
    fn the_knob_off_means_no_install_and_says_so() {
        let called = Cell::new(false);
        let (level, line) = start_up(&knobs(false), true, || {
            called.set(true);
            Ok(())
        });
        assert!(!called.get(), "install must not run");
        assert_eq!(level, Level::Info);
        assert_eq!(line, "grass distance: off: EnableScatterDistance = 0");
    }

    #[test]
    fn an_install_says_it_stays_at_90() {
        let called = Cell::new(false);
        let (level, line) = start_up(&knobs(true), true, || {
            called.set(true);
            Ok(())
        });
        assert!(called.get());
        assert_eq!(level, Level::Info);
        assert_eq!(
            line,
            "grass distance: redirect installed at 90 m (the game's value); the knob stays at \
             90 m in this version, because no scatter capacity headroom is proven"
        );
    }

    #[test]
    fn an_install_failure_is_a_warning_with_the_reason() {
        for why in [
            "the game code was not found (another tool or a different build may have changed it)",
            NOT_90,
            DIFFERENT_VALUES,
            OUTSIDE_IMAGE,
            NO_PAGE,
        ] {
            let (level, line) = start_up(&knobs(true), true, || Err(why.to_owned()));
            assert_eq!(level, Level::Warn);
            assert_eq!(line, format!("grass distance: off: {why}"));
        }
    }

    // ---- the real game exe (ignored: reads ENSHROUDED_EXE, else the Steam install) -------------

    /// Where the signatures match, and the constant both loads read, in build f68140a3 (the
    /// Steam exe has the same code at the same addresses). Dispatch ruling 6 (controller, from
    /// the bytes) and docs/re-notes.md.
    const BRICK_MATCH_VA: u64 = 0x1_4007_9502;
    const INSTANCE_MATCH_VA: u64 = 0x1_4007_691c;
    const DISTANCE_CONST_VA: usize = 0x1_413b_65fc;

    #[test]
    #[ignore = "reads the game exe (ENSHROUDED_EXE, else the Steam path); a skip shows only with -- --ignored --nocapture"]
    fn scatter_sigs_unique_in_game_exe() {
        use crate::build_id::test_support::game_exe_or_skip;
        use crate::build_id::{classify, sha256_file};
        use crate::scan::{find_unique, parse_pe_file, Pattern};
        let Some(path) = game_exe_or_skip("scatter_sigs_unique_in_game_exe") else {
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
        let mut found = Vec::new();
        for (sig, anchor) in [
            (&SCATTER_BRICK_LOAD, BRICK_MATCH_VA),
            (&SCATTER_INSTANCE_LOADS, INSTANCE_MATCH_VA),
        ] {
            let pattern = Pattern::parse(sig.pattern).unwrap();
            let at = find_unique(text, &pattern).expect("the signature is in .text exactly once");
            let va = text_va + at as u64;
            assert_eq!(va, anchor, "{}", sig.name);
            let disp_va = va + sig.patch_offset() as u64;
            // The load as `find_load` would make it, so `target` is the production code's.
            let load = Load {
                name: sig.name,
                disp_addr: disp_va as usize,
                old: pe
                    .bytes_at_va(&file, disp_va, 4)
                    .unwrap()
                    .try_into()
                    .unwrap(),
            };
            let (insn_end, target) = (load.insn_end().unwrap(), load.target().unwrap());
            let bytes = pe.bytes_at_va(&file, target as u64, 4).unwrap();
            let bits = u32::from_le_bytes(bytes.try_into().unwrap());
            println!(
                "{:<24} match {va:#x}  disp32 at {disp_va:#x}  insn end {insn_end:#x}  \
                 reads {target:#x} = {} (bits {bits:#010x})",
                sig.name,
                f32::from_bits(bits)
            );
            assert_eq!(target, DISTANCE_CONST_VA, "{}", sig.name);
            assert_eq!(bits, VANILLA_BITS, "{}", sig.name);
            found.push((target, bits));
        }
        let decision = vanilla_target(found[0], found[1]);
        println!("vanilla decision: {decision:x?}");
        assert_eq!(decision, Ok(DISTANCE_CONST_VA));
    }
}
