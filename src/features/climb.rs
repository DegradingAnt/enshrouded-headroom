//! EXPERIMENTAL: climb any rock or wall (branch `exp/climb-anything`; never merged; one in-game
//! test).
//!
//! The game already climbs walls (the impact program `player_locomotion_climb_wall`). It latches
//! onto a wall only when its wall probe `FUN_14024abc0` says so, and that probe casts every ray
//! with collider mask 0x100 (ColliderFlags bit 8, ClimbingWall): the iron lattices. It also needs
//! the four corner-ray normals to match the forward-ray normal within 0.001 per component. The
//! entry, move and wall-lost checks all call the same probe. Evidence: docs/re-notes.md, "Climb
//! anything (experimental)".
//!
//! With `[Experimental] ClimbAnything = 1`, [`install`] changes at start-up:
//! - the probe's two mask immediates (forward ray, corner rays): 0x100 to `ClimbMask`;
//! - the probe's two tolerance loads: they read a float in the mod's own page that holds
//!   `ClimbPlaneTolerance`. The game's 0.001 is shared by 244 loads, so it is never written;
//! - with `ClimbVoxelHold = 1`, the wall-lost check's success return: a hit with EntityId 0 (a
//!   voxel wall) returns 0xffffffff instead of 0, so the climb update does not stop the climb;
//! - with `ClimbClearance` other than `game`, the probe's clearance bit list.
//!
//! Every signature in [`sigs::CLIMB`] must match exactly once, or nothing is written. The plan
//! checks the game's bytes at each site first. Each write is a `write_checked` against them. If
//! one write fails, the writes before it are put back. The top entry and the exit-top probe stay
//! as they are. It hooks nothing, so the present path and the hook batch do not change.

use log::Level;

use crate::config::{ClimbClearance, ExperimentalCfg, CLIMB_MASK_BITS};
use crate::entry;
use crate::guard;
use crate::patch::{alloc_near_all, rel32, rel32_target, write_checked, PatchError, Patched};
use crate::scan::{find_in_sections, main_module, readable_run, walk_readable, Module, Pattern};
use crate::sigs::{
    self, Sig, CLIMB_PLANE_TOL_LOADS, CLIMB_PROBE_CLEAR_BIT5, CLIMB_PROBE_CLEAR_BITS,
    CLIMB_PROBE_CORNER_MASK, CLIMB_PROBE_FWD_MASK, CLIMB_WALL_LOST_RET,
};

/// The game's wall-probe collider mask: ColliderFlags bit 8, ClimbingWall.
pub const GAME_MASK: u32 = 0x100;

/// The bits of the game's coplanarity tolerance, 0.001f (`6f 12 83 3a` at 0x1413b825c).
pub const GAME_TOLERANCE_BITS: u32 = 0x3a83_126f;

/// The lowest and the highest tolerance the plan accepts (the `.ini` range).
const TOLERANCE_RANGE: (f32, f32) = (0.001, 0.5);

/// The wall-lost check's success return as the game has it: `mov eax,[rsp+0x70]` (the probe's hit
/// EntityId), `add rsp,0x88`, `ret`.
pub const WALL_LOST_GAME: [u8; 12] = [
    0x8b, 0x44, 0x24, 0x70, 0x48, 0x81, 0xc4, 0x88, 0x00, 0x00, 0x00, 0xc3,
];

/// The voxel-hold return, in place of [`WALL_LOST_GAME`]: `mov eax,[rsp+0x70]`; `cmp eax,1`;
/// `sbb eax,0`; `jmp` rel8 -0x14, to the failure path's `add rsp,0x88; ret` at match +18 (the
/// signature proves those bytes). EntityId 0 (a voxel hit) becomes 0xffffffff; any other id is
/// returned as it is. The 12 bytes stay inside the function (its `.pdata` range ends at the
/// original `ret`).
pub const WALL_LOST_VOXEL_HOLD: [u8; 12] = [
    0x8b, 0x44, 0x24, 0x70, 0x83, 0xf8, 0x01, 0x83, 0xd8, 0x00, 0xeb, 0xec,
];

/// The game's first clearance bit index (1, WorldNotDisplaced), low byte.
const CLEAR_FIRST_GAME: u8 = 0x01;
/// The game's third clearance bit index (5, Walkable), low byte of `mov eax, 5`.
const CLEAR_THIRD_GAME: u8 = 0x05;
/// The bit index that replaces them: 3, Solid.
const CLEAR_SOLID: u8 = 0x03;

/// Why [`check_game`] refused: the two tolerance loads read two different addresses.
const DIFFERENT_TARGETS: &str = "the two plane-tolerance loads read different addresses (changed \
                                 by another tool or a different build)";
/// Why [`check_game`] refused: the value both loads read is not exactly 0.001.
const NOT_0_001: &str = "the plane tolerance is not the game's 0.001 (changed by another tool or \
                         a different build)";
/// Why [`check_game`] refused: a load reads outside the game's image.
const OUTSIDE_IMAGE: &str = "a plane-tolerance load reads outside the game's image (changed by \
                             another tool or a different build)";
/// Why [`install`] stopped: no block near both loads.
const NO_PAGE: &str = "no free memory within 2 GiB of the game code for the plane tolerance";

/// A site the plan may write: its address and the bytes found there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Site<const N: usize> {
    /// The address of the first byte.
    addr: usize,
    /// The bytes there when the snapshot was taken.
    bytes: [u8; N],
}

/// The game's bytes that the plan needs, read once after every signature matched.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Snapshot {
    /// The exe's base, for the log (addresses are shown as `exe+offset`).
    base: usize,
    /// The forward ray's mask imm32.
    fwd_mask: Site<4>,
    /// The corner rays' mask imm32.
    corner_mask: Site<4>,
    /// Tolerance load A's disp32: the last four bytes of its `movss`.
    tol_a: Site<4>,
    /// Tolerance load B's disp32.
    tol_b: Site<4>,
    /// The u32 at each load's target (A, then B), or `None` if it is not readable inside the image.
    tol_values: [Option<u32>; 2],
    /// The wall-lost check's success return.
    wall_lost: Site<12>,
    /// The low byte of the first clearance bit index.
    clear_first: Site<1>,
    /// The low byte of the third clearance bit index.
    clear_third: Site<1>,
}

/// One checked write of the plan.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Write {
    /// What the write changes, for the log.
    what: &'static str,
    /// Where it goes.
    addr: usize,
    /// The bytes that must be there.
    old: Vec<u8>,
    /// The bytes it puts there.
    new: Vec<u8>,
}

/// The address a RIP-relative load reads, from its disp32 site (the disp32 ends the instruction).
fn load_target(load: &Site<4>) -> Option<usize> {
    rel32_target(load.addr.checked_add(4)?, i32::from_le_bytes(load.bytes))
}

/// `addr` as `exe+offset` when it is inside the exe at `base`, else as an address.
fn in_exe(base: usize, addr: usize) -> String {
    match addr.checked_sub(base) {
        Some(offset) if base != 0 => format!("exe+{offset:#x}"),
        _ => format!("{addr:#x}"),
    }
}

/// The tolerance as the game reads it, if the setting is one the plan accepts.
fn tolerance_of(cfg: &ExperimentalCfg) -> Result<f32, String> {
    // The `.ini` reader keeps the value in range; an `as` cast from f64 never panics.
    let tolerance = cfg.climb_plane_tolerance as f32;
    let (low, high) = TOLERANCE_RANGE;
    if (low..=high).contains(&tolerance) {
        Ok(tolerance)
    } else {
        Err(format!(
            "ClimbPlaneTolerance {tolerance} is outside {low}..={high}"
        ))
    }
}

/// Check the snapshot against the game's bytes for the writes `cfg` asks for. Returns the
/// address both tolerance loads read (the game's 0.001), or why not. Pure.
fn check_game(snap: &Snapshot, cfg: &ExperimentalCfg) -> Result<usize, String> {
    if cfg.climb_mask == 0 || cfg.climb_mask & !CLIMB_MASK_BITS != 0 {
        return Err(format!(
            "ClimbMask {:#x} is not a non-zero mask inside {CLIMB_MASK_BITS:#x}",
            cfg.climb_mask
        ));
    }
    tolerance_of(cfg)?;
    for (name, site) in [
        (CLIMB_PROBE_FWD_MASK.name, &snap.fwd_mask),
        (CLIMB_PROBE_CORNER_MASK.name, &snap.corner_mask),
    ] {
        let found = u32::from_le_bytes(site.bytes);
        if found != GAME_MASK {
            return Err(format!(
                "{name}: the mask at {} is {found:#x}, not the game's 0x100 (changed by another \
                 tool or a different build)",
                in_exe(snap.base, site.addr)
            ));
        }
    }
    let target_a = load_target(&snap.tol_a).ok_or(OUTSIDE_IMAGE)?;
    let target_b = load_target(&snap.tol_b).ok_or(OUTSIDE_IMAGE)?;
    if target_a != target_b {
        return Err(DIFFERENT_TARGETS.to_owned());
    }
    for value in snap.tol_values {
        match value {
            None => return Err(OUTSIDE_IMAGE.to_owned()),
            Some(bits) if bits != GAME_TOLERANCE_BITS => return Err(NOT_0_001.to_owned()),
            Some(_) => {}
        }
    }
    if cfg.climb_voxel_hold && snap.wall_lost.bytes != WALL_LOST_GAME {
        return Err(format!(
            "{}: the return at {} is not the game's (read {:02x?})",
            CLIMB_WALL_LOST_RET.name,
            in_exe(snap.base, snap.wall_lost.addr),
            snap.wall_lost.bytes
        ));
    }
    let clear_sites = match cfg.climb_clearance {
        ClimbClearance::Game => &[][..],
        ClimbClearance::SolidWalkable => &[(&snap.clear_first, CLEAR_FIRST_GAME)][..],
        ClimbClearance::Solid => &[
            (&snap.clear_first, CLEAR_FIRST_GAME),
            (&snap.clear_third, CLEAR_THIRD_GAME),
        ][..],
    };
    for (site, game) in clear_sites {
        if site.bytes != [*game] {
            return Err(format!(
                "the clearance bit index at {} is {:#x}, not the game's {game:#x}",
                in_exe(snap.base, site.addr),
                site.bytes.first().copied().unwrap_or(0)
            ));
        }
    }
    Ok(target_a)
}

/// The writes for `cfg`, in order: the page float (old: the zeros of a fresh page), the two mask
/// immediates, the two disp32s, then the voxel-hold return and the clearance bytes if asked for.
/// A mask write whose new bytes equal the old ones is left out. Every check of [`check_game`] runs
/// first, and a page that a load cannot reach is refused. Pure.
fn plan(snap: &Snapshot, cfg: &ExperimentalCfg, page: usize) -> Result<Vec<Write>, String> {
    check_game(snap, cfg)?;
    let tolerance = tolerance_of(cfg)?;
    let mut writes = vec![Write {
        what: "the plane-tolerance page",
        addr: page,
        old: vec![0; 4],
        new: tolerance.to_bits().to_le_bytes().to_vec(),
    }];
    let mask = cfg.climb_mask.to_le_bytes();
    for (what, site) in [
        ("the forward-ray mask", &snap.fwd_mask),
        ("the corner-ray mask", &snap.corner_mask),
    ] {
        if site.bytes != mask {
            writes.push(Write {
                what,
                addr: site.addr,
                old: site.bytes.to_vec(),
                new: mask.to_vec(),
            });
        }
    }
    for (what, load) in [
        ("tolerance load A", &snap.tol_a),
        ("tolerance load B", &snap.tol_b),
    ] {
        let at = in_exe(snap.base, load.addr);
        let disp = load
            .addr
            .checked_add(4)
            .and_then(|end| rel32(end, page))
            .ok_or_else(|| {
                format!("the tolerance page at {page:#x} is out of reach of {what} at {at}")
            })?;
        writes.push(Write {
            what,
            addr: load.addr,
            old: load.bytes.to_vec(),
            new: disp.to_le_bytes().to_vec(),
        });
    }
    if cfg.climb_voxel_hold {
        writes.push(Write {
            what: "the wall-lost return",
            addr: snap.wall_lost.addr,
            old: WALL_LOST_GAME.to_vec(),
            new: WALL_LOST_VOXEL_HOLD.to_vec(),
        });
    }
    let first = Write {
        what: "the first clearance bit index",
        addr: snap.clear_first.addr,
        old: vec![CLEAR_FIRST_GAME],
        new: vec![CLEAR_SOLID],
    };
    let third = Write {
        what: "the third clearance bit index",
        addr: snap.clear_third.addr,
        old: vec![CLEAR_THIRD_GAME],
        new: vec![CLEAR_SOLID],
    };
    match cfg.climb_clearance {
        ClimbClearance::Game => {}
        ClimbClearance::SolidWalkable => writes.push(first),
        ClimbClearance::Solid => writes.extend([first, third]),
    }
    Ok(writes)
}

/// Make `writes` in order with `write(addr, old, new)`. If one fails, the writes before it are
/// put back, last first. `Err` names the write that failed and says what was put back.
/// Production passes `write_checked`; a test can watch or fail each write.
fn apply_with(
    writes: &[Write],
    mut write: impl FnMut(usize, &[u8], &[u8]) -> Result<Patched, PatchError>,
) -> Result<(), String> {
    for (done, failed) in writes.iter().enumerate() {
        let Err(error) = write(failed.addr, &failed.old, &failed.new) else {
            continue;
        };
        let mut not_back = Vec::new();
        for undo in writes.get(..done).unwrap_or_default().iter().rev() {
            if let Err(undo_error) = write(undo.addr, &undo.new, &undo.old) {
                not_back.push(format!("{} at {:#x} ({undo_error})", undo.what, undo.addr));
            }
        }
        let back = if not_back.is_empty() {
            format!("the {done} writes before it were put back")
        } else {
            format!("could not put back: {}", not_back.join(", "))
        };
        return Err(format!(
            "{} at {:#x} was not written: {error}; {back}",
            failed.what, failed.addr
        ));
    }
    Ok(())
}

/// The log text of a successful install: what changed and where. Pure.
fn summary(snap: &Snapshot, cfg: &ExperimentalCfg, target: usize, page: usize) -> String {
    let at = |addr: usize| in_exe(snap.base, addr);
    let mask = if cfg.climb_mask == GAME_MASK {
        "mask 0x100 (the game's; not written)".to_owned()
    } else {
        format!(
            "mask 0x100 -> {:#x} at {} and {}",
            cfg.climb_mask,
            at(snap.fwd_mask.addr),
            at(snap.corner_mask.addr)
        )
    };
    let tolerance = format!(
        "plane tolerance 0.001 -> {} (loads at {} and {} read the mod page {page:#x}, not {})",
        cfg.climb_plane_tolerance as f32,
        at(snap.tol_a.addr),
        at(snap.tol_b.addr),
        at(target)
    );
    let hold = if cfg.climb_voxel_hold {
        format!("voxel hold on at {}", at(snap.wall_lost.addr))
    } else {
        "voxel hold off".to_owned()
    };
    let clearance = match cfg.climb_clearance {
        ClimbClearance::Game => "clearance game".to_owned(),
        ClimbClearance::SolidWalkable => {
            format!("clearance solid_walkable at {}", at(snap.clear_first.addr))
        }
        ClimbClearance::Solid => format!(
            "clearance solid at {} and {}",
            at(snap.clear_first.addr),
            at(snap.clear_third.addr)
        ),
    };
    format!(
        "{mask}; {tolerance}; {hold}; {clearance}; all {} climb signatures matched once",
        sigs::CLIMB.len()
    )
}

/// Find every signature in [`sigs::CLIMB`] in `m`. Returns each one with its match address, or
/// the name and the reason of every one that did not match exactly once.
///
/// Start-up only, as `scan::find_in_sections`.
fn find_all(m: &Module) -> Result<Vec<(&'static Sig, usize)>, String> {
    let mut found = Vec::new();
    let mut failed = Vec::new();
    for &sig in sigs::CLIMB {
        let result = Pattern::parse(sig.pattern)
            .and_then(|pattern| find_in_sections(m, sig.sections, &pattern));
        match result {
            Ok(at) => found.push((sig, at)),
            Err(error) => failed.push(format!("{}: {error}", sig.name)),
        }
    }
    if failed.is_empty() {
        Ok(found)
    } else {
        Err(format!(
            "{} of {} climb signatures did not match exactly once, so nothing was written: {}",
            failed.len(),
            sigs::CLIMB.len(),
            failed.join("; ")
        ))
    }
}

/// The site at `sig`'s patch offset number `index`, `N` bytes, read from memory. `Err` if `sig`
/// is not in `found`, or the site is not inside its match.
///
/// # Safety
///
/// Each address in `found` is where `find_in_sections` found that signature a moment ago, so the
/// whole match is committed, readable memory; and nothing has changed its protection since
/// (start-up only).
unsafe fn site_of<const N: usize>(
    found: &[(&'static Sig, usize)],
    sig: &Sig,
    index: usize,
) -> Result<Site<N>, String> {
    let name = sig.name;
    let outside = || format!("{name}: site {index} is not inside the match");
    let at = found
        .iter()
        .find(|(s, _)| s.name == name)
        .map(|&(_, at)| at)
        .ok_or_else(|| format!("{name}: not found"))?;
    let offset = *sig.patch_offsets.get(index).ok_or_else(outside)?;
    let len = sig.pattern.split_ascii_whitespace().count();
    if offset.checked_add(N).is_none_or(|end| end > len) {
        return Err(outside());
    }
    let addr = at.checked_add(offset).ok_or_else(outside)?;
    // SAFETY: `[addr, addr + N)` lies inside the match at `at` (checked above), which this
    // function's contract keeps committed and readable. `[u8; N]` has no alignment or validity
    // requirement, and an unaligned read needs no alignment.
    let bytes = unsafe { (addr as *const [u8; N]).read_unaligned() };
    Ok(Site { addr, bytes })
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

/// Read every site the plan needs, and the value each tolerance load reads.
///
/// # Safety
///
/// As [`site_of`] for `found`, and as [`read_in_image`]; `found` came from [`find_all`] on `m`.
unsafe fn read_snapshot(m: &Module, found: &[(&'static Sig, usize)]) -> Result<Snapshot, String> {
    // SAFETY: this function's contract is `site_of`'s for every call below.
    let (fwd_mask, corner_mask, tol_a, tol_b, wall_lost, clear_first, clear_third) = unsafe {
        (
            site_of::<4>(found, &CLIMB_PROBE_FWD_MASK, 0)?,
            site_of::<4>(found, &CLIMB_PROBE_CORNER_MASK, 0)?,
            site_of::<4>(found, &CLIMB_PLANE_TOL_LOADS, 0)?,
            site_of::<4>(found, &CLIMB_PLANE_TOL_LOADS, 1)?,
            site_of::<12>(found, &CLIMB_WALL_LOST_RET, 0)?,
            site_of::<1>(found, &CLIMB_PROBE_CLEAR_BITS, 0)?,
            site_of::<1>(found, &CLIMB_PROBE_CLEAR_BIT5, 0)?,
        )
    };
    let value = |load: &Site<4>| {
        let target = load_target(load)?;
        // SAFETY: this function's contract is `read_in_image`'s.
        unsafe { read_in_image(m, target) }
    };
    Ok(Snapshot {
        base: m.base,
        fwd_mask,
        corner_mask,
        tol_a,
        tol_b,
        tol_values: [value(&tol_a), value(&tol_b)],
        wall_lost,
        clear_first,
        clear_third,
    })
}

/// Find every climb signature, check the game's bytes, get a page near both tolerance loads,
/// then make the writes. `Ok` is the log text of what changed. `Err` is why nothing changed, or,
/// after a failed write, what could not be put back. The page is never freed.
///
/// # Safety
///
/// Start-up only: call it from `entry::start` (in `InitializeASI`, before the game's `WinMain`),
/// on one thread, with `m` the running exe (`scan::main_module()`). No game thread runs the
/// patched code yet, so it can be rewritten. A test may pass memory of its own process that no
/// thread executes.
pub unsafe fn install(m: &Module, cfg: &ExperimentalCfg) -> Result<String, String> {
    // SAFETY: this function's contract is `install_with`'s, and `alloc_near_all` gives a fresh,
    // zero-filled, read-write block that nothing else knows.
    unsafe { install_with(m, cfg, alloc_near_all) }
}

/// [`install`], with the page allocator passed in (`alloc_near_all`'s shape). It is asked once,
/// with the ends of both tolerance loads, for four bytes, after every check has passed.
///
/// # Safety
///
/// [`install`]'s contract. `alloc` returns `None`, or the start of a fresh, zero-filled,
/// read-write block of at least the size asked for that nothing else uses.
unsafe fn install_with(
    m: &Module,
    cfg: &ExperimentalCfg,
    alloc: impl FnOnce(&[usize], usize) -> Option<usize>,
) -> Result<String, String> {
    let found = find_all(m)?;
    // SAFETY: `found` came from `find_all` on `m` just now, at start-up (this function's
    // contract).
    let snap = unsafe { read_snapshot(m, &found) }?;
    let target = check_game(&snap, cfg)?;
    let ends = [
        snap.tol_a.addr.checked_add(4).ok_or(NO_PAGE)?,
        snap.tol_b.addr.checked_add(4).ok_or(NO_PAGE)?,
    ];
    let page = alloc(&ends, size_of::<u32>()).ok_or(NO_PAGE)?;
    let writes = plan(&snap, cfg, page)?;
    apply_with(&writes, |addr, old, new| {
        // SAFETY: `plan` gives only the fresh page (`alloc`'s contract: nothing else uses it) and
        // sites inside the climb signatures' matches in the game's `.text`, which no thread runs
        // yet (this function's start-up contract). `old` and `new` are the plan's own vectors,
        // not the target memory.
        unsafe { write_checked(addr, old, new) }
    })?;
    Ok(summary(&snap, cfg, target, page))
}

/// The body of [`init`], with the code-patch answer and the install passed in. `install` runs
/// only with `ClimbAnything = 1` and code patches allowed. Returns the log level and the line.
fn start_up(
    cfg: &ExperimentalCfg,
    code_patches: bool,
    install: impl FnOnce() -> Result<String, String>,
) -> (Level, String) {
    if !cfg.climb_anything {
        return (
            Level::Info,
            "climb anything: off: ClimbAnything = 0".to_owned(),
        );
    }
    if !code_patches {
        return (
            Level::Info,
            "climb anything: off: code patches are off on this build".to_owned(),
        );
    }
    match install() {
        Ok(what) => (
            Level::Info,
            format!("climb anything: on (EXPERIMENTAL): {what}"),
        ),
        Err(why) => (Level::Warn, format!("climb anything: off: {why}")),
    }
}

/// The start-up step: decide, install if it is right to, and log one line. Called once, from
/// `entry::start`, after `code_patches_allowed` is set and before the hook batch. A panic here
/// is caught, so the later start-up steps still run.
pub fn init(cfg: &ExperimentalCfg) {
    // Unwind safety: nothing after an `Err` uses state the closure was changing.
    let outcome = guard::catch("climb anything", || {
        start_up(cfg, entry::code_patches_allowed(), || {
            // SAFETY: `init` runs from `entry::start`, in `InitializeASI`, before the game's
            // `WinMain`: one thread, and no game thread runs the climb code yet.
            // `main_module()` is the running exe.
            unsafe { install(&main_module(), cfg) }
        })
    });
    match outcome {
        Ok((level, line)) => log::log!(level, "{line}"),
        Err(why) => {
            log::warn!("climb anything: stopped: {why}; some climb writes may be in place")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::patch::test_support::Mem;
    use crate::sigs::{CLIMB, CLIMB_ORPHAN_RAY_MASK, CLIMB_TOP_RAY_MASK};
    use std::cell::Cell;

    // ---- the constants ------------------------------------------------------------------------

    #[test]
    fn the_game_values_are_the_ones_the_notes_name() {
        assert_eq!(GAME_MASK, 1 << 8, "ColliderFlags bit 8, ClimbingWall");
        assert_eq!(GAME_TOLERANCE_BITS, 0.001f32.to_bits());
        assert_eq!(GAME_TOLERANCE_BITS.to_le_bytes(), [0x6f, 0x12, 0x83, 0x3a]);
        assert_eq!(TOLERANCE_RANGE, (0.001, 0.5));
    }

    /// The voxel-hold bytes keep the game's load of the id, and their `jmp` lands on the failure
    /// path's `add rsp,0x88; ret`, which the signature holds at +18.
    #[test]
    fn the_voxel_hold_return_jumps_to_the_shared_epilogue() {
        assert_eq!(WALL_LOST_VOXEL_HOLD.get(..4), WALL_LOST_GAME.get(..4));
        // cmp eax,1 (83 /7 ib); sbb eax,0 (83 /3 ib); jmp rel8.
        assert_eq!(
            WALL_LOST_VOXEL_HOLD.get(4..7),
            Some(&[0x83, 0xf8, 0x01][..])
        );
        assert_eq!(
            WALL_LOST_VOXEL_HOLD.get(7..10),
            Some(&[0x83, 0xd8, 0x00][..])
        );
        assert_eq!(WALL_LOST_VOXEL_HOLD.get(10), Some(&0xeb));
        let site = CLIMB_WALL_LOST_RET.patch_offset();
        let rel8 = isize::from(WALL_LOST_VOXEL_HOLD[11] as i8);
        let lands = (site + WALL_LOST_VOXEL_HOLD.len()) as isize + rel8;
        assert_eq!(lands, 18);
        let tokens: Vec<&str> = CLIMB_WALL_LOST_RET
            .pattern
            .split_ascii_whitespace()
            .collect();
        assert_eq!(
            tokens.get(18..26),
            Some(&["48", "81", "c4", "88", "00", "00", "00", "c3"][..])
        );
        assert_eq!(WALL_LOST_GAME.len(), WALL_LOST_VOXEL_HOLD.len());
    }

    /// What the voxel-hold bytes compute, step by step as the CPU does: `cmp eax,1` sets CF when
    /// eax is 0 (unsigned below 1); `sbb eax,0` subtracts CF.
    #[test]
    fn the_voxel_hold_return_maps_only_id_0() {
        let returned = |id: u32| id.wrapping_sub(u32::from(id < 1));
        assert_eq!(returned(0), 0xffff_ffff);
        for id in [1, 2, 0x1234, 0xffff_fffe, 0xffff_ffff] {
            assert_eq!(returned(id), id);
        }
    }

    // ---- the plan, on a made-up snapshot ------------------------------------------------------

    /// Where the made-up game sits.
    const BASE: usize = 0x1_4000_0000;
    /// Where its shared 0.001 is.
    const TARGET: usize = 0x1_413b_825c;
    /// A page both loads reach.
    const PAGE: usize = 0x1_5000_0000;

    /// A disp32 site at `addr` that reads `target`.
    fn load_at(addr: usize, target: usize) -> Site<4> {
        let disp = i32::try_from(target as i64 - (addr as i64 + 4)).unwrap();
        Site {
            addr,
            bytes: disp.to_le_bytes(),
        }
    }

    /// The game as the notes give it (Steam exe addresses).
    fn game() -> Snapshot {
        Snapshot {
            base: BASE,
            fwd_mask: Site {
                addr: 0x1_4024_accc,
                bytes: [0x00, 0x01, 0x00, 0x00],
            },
            corner_mask: Site {
                addr: 0x1_4024_af53,
                bytes: [0x00, 0x01, 0x00, 0x00],
            },
            tol_a: load_at(0x1_4024_afbc, TARGET),
            tol_b: load_at(0x1_4024_aff0, TARGET),
            tol_values: [Some(GAME_TOLERANCE_BITS); 2],
            wall_lost: Site {
                addr: 0x1_4024_b4a5,
                bytes: WALL_LOST_GAME,
            },
            clear_first: Site {
                addr: 0x1_4024_ad86,
                bytes: [0x01],
            },
            clear_third: Site {
                addr: 0x1_4024_ad55,
                bytes: [0x05],
            },
        }
    }

    /// The default settings with climbing on.
    fn on() -> ExperimentalCfg {
        ExperimentalCfg {
            climb_anything: true,
            ..Config::default().experimental
        }
    }

    /// The disp32 from the load at `site` to `target`.
    fn disp(site: usize, target: usize) -> Vec<u8> {
        rel32(site + 4, target).unwrap().to_le_bytes().to_vec()
    }

    #[test]
    fn the_snapshot_is_the_game_and_its_loads_read_the_shared_constant() {
        let s = game();
        // The disp32s of the Steam exe: 9c d2 16 01 and 68 d2 16 01.
        assert_eq!(s.tol_a.bytes, [0x9c, 0xd2, 0x16, 0x01]);
        assert_eq!(s.tol_b.bytes, [0x68, 0xd2, 0x16, 0x01]);
        assert_eq!(check_game(&s, &on()), Ok(TARGET));
    }

    #[test]
    fn the_default_plan_writes_the_page_two_masks_and_two_disp32s() {
        let s = game();
        let writes = plan(&s, &on(), PAGE).unwrap();
        let mask = 0x124u32.to_le_bytes().to_vec();
        assert_eq!(
            writes,
            [
                Write {
                    what: "the plane-tolerance page",
                    addr: PAGE,
                    old: vec![0; 4],
                    new: 0.05f32.to_bits().to_le_bytes().to_vec(),
                },
                Write {
                    what: "the forward-ray mask",
                    addr: s.fwd_mask.addr,
                    old: vec![0x00, 0x01, 0x00, 0x00],
                    new: mask.clone(),
                },
                Write {
                    what: "the corner-ray mask",
                    addr: s.corner_mask.addr,
                    old: vec![0x00, 0x01, 0x00, 0x00],
                    new: mask,
                },
                Write {
                    what: "tolerance load A",
                    addr: s.tol_a.addr,
                    old: vec![0x9c, 0xd2, 0x16, 0x01],
                    new: disp(s.tol_a.addr, PAGE),
                },
                Write {
                    what: "tolerance load B",
                    addr: s.tol_b.addr,
                    old: vec![0x68, 0xd2, 0x16, 0x01],
                    new: disp(s.tol_b.addr, PAGE),
                },
            ]
        );
    }

    #[test]
    fn the_plan_writes_the_tolerance_setting_into_the_page() {
        let cfg = ExperimentalCfg {
            climb_plane_tolerance: 0.2,
            ..on()
        };
        let writes = plan(&game(), &cfg, PAGE).unwrap();
        assert_eq!(writes[0].new, 0.2f32.to_bits().to_le_bytes());
    }

    #[test]
    fn voxel_hold_adds_the_wall_lost_return() {
        let s = game();
        let cfg = ExperimentalCfg {
            climb_voxel_hold: true,
            ..on()
        };
        let writes = plan(&s, &cfg, PAGE).unwrap();
        assert_eq!(writes.len(), 6);
        assert_eq!(
            writes.last(),
            Some(&Write {
                what: "the wall-lost return",
                addr: 0x1_4024_b4a5,
                old: WALL_LOST_GAME.to_vec(),
                new: WALL_LOST_VOXEL_HOLD.to_vec(),
            })
        );
    }

    #[test]
    fn clearance_writes_one_or_two_bit_indexes() {
        let s = game();
        let with = |clearance| ExperimentalCfg {
            climb_clearance: clearance,
            ..on()
        };
        let tail = |writes: &[Write]| -> Vec<(usize, Vec<u8>, Vec<u8>)> {
            writes
                .iter()
                .skip(5)
                .map(|w| (w.addr, w.old.clone(), w.new.clone()))
                .collect()
        };
        let game_list = plan(&s, &with(ClimbClearance::Game), PAGE).unwrap();
        assert!(tail(&game_list).is_empty());
        let solid_walkable = plan(&s, &with(ClimbClearance::SolidWalkable), PAGE).unwrap();
        assert_eq!(
            tail(&solid_walkable),
            [(0x1_4024_ad86, vec![0x01], vec![0x03])]
        );
        let solid = plan(&s, &with(ClimbClearance::Solid), PAGE).unwrap();
        assert_eq!(
            tail(&solid),
            [
                (0x1_4024_ad86, vec![0x01], vec![0x03]),
                (0x1_4024_ad55, vec![0x05], vec![0x03])
            ]
        );
    }

    #[test]
    fn the_game_mask_is_not_rewritten() {
        let cfg = ExperimentalCfg {
            climb_mask: GAME_MASK,
            ..on()
        };
        let writes = plan(&game(), &cfg, PAGE).unwrap();
        let what: Vec<&str> = writes.iter().map(|w| w.what).collect();
        assert_eq!(
            what,
            [
                "the plane-tolerance page",
                "tolerance load A",
                "tolerance load B"
            ]
        );
    }

    // ---- the plan refuses anything but the game's bytes ---------------------------------------

    #[test]
    fn a_mask_that_is_not_0x100_is_refused() {
        let mut s = game();
        s.fwd_mask.bytes = 0x124u32.to_le_bytes();
        let why = plan(&s, &on(), PAGE).unwrap_err();
        assert!(
            why.starts_with(
                "CLIMB_PROBE_FWD_MASK: the mask at exe+0x24accc is 0x124, not the game's 0x100"
            ),
            "{why}"
        );
        let mut s = game();
        s.corner_mask.bytes = [0x00, 0x00, 0x00, 0x00];
        let why = plan(&s, &on(), PAGE).unwrap_err();
        assert!(
            why.starts_with("CLIMB_PROBE_CORNER_MASK: the mask at exe+0x24af53 is 0x0"),
            "{why}"
        );
    }

    #[test]
    fn tolerance_loads_must_both_read_the_games_0_001() {
        let mut s = game();
        s.tol_b = load_at(s.tol_b.addr, TARGET + 4);
        assert_eq!(plan(&s, &on(), PAGE), Err(DIFFERENT_TARGETS.to_owned()));
        for other in [0.002f32.to_bits(), GAME_TOLERANCE_BITS + 1, 0] {
            let mut s = game();
            s.tol_values = [Some(GAME_TOLERANCE_BITS), Some(other)];
            assert_eq!(
                plan(&s, &on(), PAGE),
                Err(NOT_0_001.to_owned()),
                "{other:#x}"
            );
            s.tol_values = [Some(other), Some(GAME_TOLERANCE_BITS)];
            assert_eq!(
                plan(&s, &on(), PAGE),
                Err(NOT_0_001.to_owned()),
                "{other:#x}"
            );
        }
        let mut s = game();
        s.tol_values = [None, Some(GAME_TOLERANCE_BITS)];
        assert_eq!(plan(&s, &on(), PAGE), Err(OUTSIDE_IMAGE.to_owned()));
    }

    #[test]
    fn a_wall_lost_return_that_is_not_the_games_is_refused_only_when_it_is_written() {
        let mut s = game();
        s.wall_lost.bytes = WALL_LOST_VOXEL_HOLD;
        let hold = ExperimentalCfg {
            climb_voxel_hold: true,
            ..on()
        };
        let why = plan(&s, &hold, PAGE).unwrap_err();
        assert!(
            why.starts_with("CLIMB_WALL_LOST_RET: the return at exe+0x24b4a5 is not the game's"),
            "{why}"
        );
        assert_eq!(plan(&s, &on(), PAGE).map(|w| w.len()), Ok(5));
    }

    #[test]
    fn a_clearance_byte_that_is_not_the_games_is_refused_only_when_it_is_written() {
        let solid = ExperimentalCfg {
            climb_clearance: ClimbClearance::Solid,
            ..on()
        };
        let mut s = game();
        s.clear_third.bytes = [0x03];
        let why = plan(&s, &solid, PAGE).unwrap_err();
        assert_eq!(
            why,
            "the clearance bit index at exe+0x24ad55 is 0x3, not the game's 0x5"
        );
        let solid_walkable = ExperimentalCfg {
            climb_clearance: ClimbClearance::SolidWalkable,
            ..on()
        };
        assert_eq!(plan(&s, &solid_walkable, PAGE).map(|w| w.len()), Ok(6));
        let mut s = game();
        s.clear_first.bytes = [0x03];
        assert!(plan(&s, &solid_walkable, PAGE).is_err());
        assert_eq!(plan(&s, &on(), PAGE).map(|w| w.len()), Ok(5));
    }

    #[test]
    fn a_page_out_of_reach_is_refused() {
        let why = plan(&game(), &on(), 0x3_0000_0000).unwrap_err();
        assert_eq!(
            why,
            "the tolerance page at 0x300000000 is out of reach of tolerance load A at exe+0x24afbc"
        );
    }

    #[test]
    fn settings_outside_their_range_are_refused() {
        for mask in [0, 0x80_0000, 0xffff_ffff] {
            let cfg = ExperimentalCfg {
                climb_mask: mask,
                ..on()
            };
            assert!(plan(&game(), &cfg, PAGE).is_err(), "{mask:#x}");
        }
        for tolerance in [0.0, 0.0009, 0.51, f64::NAN, f64::INFINITY] {
            let cfg = ExperimentalCfg {
                climb_plane_tolerance: tolerance,
                ..on()
            };
            assert!(plan(&game(), &cfg, PAGE).is_err(), "{tolerance}");
        }
    }

    // ---- applying the writes -------------------------------------------------------------------

    /// Three made-up writes, so the order and the undo can be seen.
    fn writes3() -> Vec<Write> {
        (0..3)
            .map(|i| Write {
                what: ["first", "second", "third"][i],
                addr: 0x1000 * (i + 1),
                old: vec![i as u8],
                new: vec![0x10 + i as u8],
            })
            .collect()
    }

    #[test]
    fn writes_are_made_in_order() {
        let mut seen = Vec::new();
        let result = apply_with(&writes3(), |addr, old, new| {
            seen.push((addr, old.to_vec(), new.to_vec()));
            Ok(Patched {
                addr,
                original: old.to_vec(),
            })
        });
        assert_eq!(result, Ok(()));
        assert_eq!(
            seen,
            [
                (0x1000, vec![0], vec![0x10]),
                (0x2000, vec![1], vec![0x11]),
                (0x3000, vec![2], vec![0x12])
            ]
        );
    }

    #[test]
    fn a_failed_write_puts_the_earlier_ones_back_last_first() {
        let mut seen = Vec::new();
        let result = apply_with(&writes3(), |addr, old, new| {
            seen.push((addr, old.to_vec(), new.to_vec()));
            if addr == 0x3000 {
                return Err(PatchError::Mismatch {
                    at: addr,
                    found: vec![9],
                });
            }
            Ok(Patched {
                addr,
                original: old.to_vec(),
            })
        });
        assert_eq!(
            result,
            Err(
                "third at 0x3000 was not written: bytes at 0x3000 are not the expected ones \
                 (read [09]); the 2 writes before it were put back"
                    .to_owned()
            )
        );
        assert_eq!(
            seen,
            [
                (0x1000, vec![0], vec![0x10]),
                (0x2000, vec![1], vec![0x11]),
                (0x3000, vec![2], vec![0x12]),
                (0x2000, vec![0x11], vec![1]),
                (0x1000, vec![0x10], vec![0])
            ]
        );
    }

    #[test]
    fn a_failed_undo_is_named() {
        let calls = Cell::new(0);
        let result = apply_with(&writes3(), |addr, old, _| {
            calls.set(calls.get() + 1);
            // The third write fails, and so does the undo of the first.
            if addr == 0x3000 || (addr == 0x1000 && calls.get() > 1) {
                return Err(PatchError::Protect(5));
            }
            Ok(Patched {
                addr,
                original: old.to_vec(),
            })
        });
        let why = result.unwrap_err();
        assert!(
            why.starts_with("third at 0x3000 was not written: VirtualProtect failed (error 5)"),
            "{why}"
        );
        assert!(
            why.ends_with("could not put back: first at 0x1000 (VirtualProtect failed (error 5))"),
            "{why}"
        );
        assert_eq!(calls.get(), 5);
    }

    // ---- the log line ------------------------------------------------------------------------

    #[test]
    fn the_summary_says_what_changed_and_where() {
        let s = game();
        let cfg = ExperimentalCfg {
            climb_voxel_hold: true,
            climb_clearance: ClimbClearance::SolidWalkable,
            ..on()
        };
        assert_eq!(
            summary(&s, &cfg, TARGET, PAGE),
            "mask 0x100 -> 0x124 at exe+0x24accc and exe+0x24af53; plane tolerance 0.001 -> 0.05 \
             (loads at exe+0x24afbc and exe+0x24aff0 read the mod page 0x150000000, not \
             exe+0x13b825c); voxel hold on at exe+0x24b4a5; clearance solid_walkable at \
             exe+0x24ad86; all 16 climb signatures matched once"
        );
        let game_mask = ExperimentalCfg {
            climb_mask: GAME_MASK,
            ..on()
        };
        let plain = summary(&s, &game_mask, TARGET, PAGE);
        assert!(
            plain.starts_with("mask 0x100 (the game's; not written); "),
            "{plain}"
        );
        assert!(
            plain.contains("; voxel hold off; clearance game; "),
            "{plain}"
        );
    }

    #[test]
    fn addresses_outside_the_exe_are_shown_whole() {
        assert_eq!(in_exe(0x1000, 0x1234), "exe+0x234");
        assert_eq!(in_exe(0x1000, 0x0800), "0x800");
        assert_eq!(in_exe(0, 0x1234), "0x1234");
    }

    // ---- the start-up step ---------------------------------------------------------------------

    #[test]
    fn climbing_off_means_no_install_and_says_so() {
        let called = Cell::new(false);
        let (level, line) = start_up(&Config::default().experimental, true, || {
            called.set(true);
            Ok(String::new())
        });
        assert!(!called.get(), "install must not run");
        assert_eq!(level, Level::Info);
        assert_eq!(line, "climb anything: off: ClimbAnything = 0");
    }

    #[test]
    fn no_code_patches_means_no_install_and_says_so() {
        let called = Cell::new(false);
        let (level, line) = start_up(&on(), false, || {
            called.set(true);
            Ok(String::new())
        });
        assert!(!called.get(), "install must not run");
        assert_eq!(level, Level::Info);
        assert_eq!(
            line,
            "climb anything: off: code patches are off on this build"
        );
    }

    #[test]
    fn an_install_logs_what_changed_and_a_failure_is_a_warning() {
        let (level, line) = start_up(&on(), true, || Ok("what changed".to_owned()));
        assert_eq!(level, Level::Info);
        assert_eq!(line, "climb anything: on (EXPERIMENTAL): what changed");
        let (level, line) = start_up(&on(), true, || Err("why".to_owned()));
        assert_eq!(level, Level::Warn);
        assert_eq!(line, "climb anything: off: why");
    }

    // ---- install on a synthetic image ----------------------------------------------------------

    /// RVA of the made-up 0.001 constant (outside `.text`, inside the image).
    const CONST_RVA: usize = 0x3000;

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

    /// The bytes of `sig`'s pattern with every `??` as 0.
    fn bytes_of(sig: &Sig) -> Vec<u8> {
        sig.pattern
            .split_ascii_whitespace()
            .map(|t| u8::from_str_radix(t, 16).unwrap_or(0))
            .collect()
    }

    /// The RVA where `sig` is placed in the image. The two short tolerance forms are not placed:
    /// they match inside `CLIMB_PLANE_TOL_LOADS`.
    fn rva_of(sig: &Sig) -> Option<usize> {
        CLIMB
            .iter()
            .filter(|s| !matches!(s.name, "CLIMB_TOL_LOAD_A" | "CLIMB_TOL_LOAD_B"))
            .position(|s| s.name == sig.name)
            .map(|i| 0x1000 + i * 0x80)
    }

    /// A read-write image laid out like the game: every climb signature once, both tolerance
    /// loads reading one 0.001 at `CONST_RVA`.
    fn image() -> (Mem, Module) {
        let mem = Mem::rw(0x4000);
        mem.put(0, &text_only_header());
        for sig in CLIMB {
            let Some(rva) = rva_of(sig) else { continue };
            let mut code = bytes_of(sig);
            if sig.name == CLIMB_PLANE_TOL_LOADS.name {
                for off in [12, 64] {
                    let end = rva + off + 4;
                    let d = i32::try_from(CONST_RVA as i64 - end as i64).unwrap();
                    code[off..off + 4].copy_from_slice(&d.to_le_bytes());
                }
                // The game's next three bytes, which the short form B needs.
                code.extend([0x0f, 0x93, 0xc2]);
            }
            mem.put(rva, &code);
        }
        mem.put(CONST_RVA, &0.001f32.to_le_bytes());
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

    /// `install_with` on a test image, with pages that are freed when the returned `Mem`s drop.
    fn install_here(module: &Module, cfg: &ExperimentalCfg) -> (Result<String, String>, Vec<Mem>) {
        let mut pages = Vec::new();
        let alloc = |targets: &[usize], size: usize| {
            let page = alloc_near_all(targets, size)?;
            // SAFETY: `alloc_near_all` returns the base of a fresh VirtualAlloc block of at
            // least `size` bytes, and nothing else frees it.
            pages.push(unsafe { Mem::adopt(page, size) });
            Some(page)
        };
        // SAFETY: the tests pass a `Module` for a synthetic image (read-write memory of this
        // process, described by a header the test wrote) or for the test exe, whose `.text` has
        // no such code. No thread runs any byte that can be written.
        let result = unsafe { install_with(module, cfg, alloc) };
        (result, pages)
    }

    #[test]
    fn the_image_holds_the_game_layout() {
        let (mem, module) = image();
        let plane = rva_of(&CLIMB_PLANE_TOL_LOADS).unwrap();
        assert_eq!(target_of(&mem, plane + 12), module.base + CONST_RVA);
        assert_eq!(target_of(&mem, plane + 64), module.base + CONST_RVA);
        assert_eq!(find_all(&module).map(|f| f.len()), Ok(CLIMB.len()));
    }

    #[test]
    fn install_writes_only_the_planned_sites() {
        let (mem, module) = image();
        let before = all(&mem);
        let cfg = ExperimentalCfg {
            climb_voxel_hold: true,
            climb_clearance: ClimbClearance::Solid,
            ..on()
        };
        let (result, pages) = install_here(&module, &cfg);
        let line = result.unwrap();
        assert!(
            line.ends_with("all 16 climb signatures matched once"),
            "{line}"
        );
        let page = pages[0].base;
        // The page holds the setting, and both loads read it.
        assert_eq!(pages[0].get(0, 4), 0.05f32.to_le_bytes());
        let plane = rva_of(&CLIMB_PLANE_TOL_LOADS).unwrap();
        assert_eq!(target_of(&mem, plane + 12), page);
        assert_eq!(target_of(&mem, plane + 64), page);
        // The masks, the return and the two clearance bytes hold the new bytes.
        let fwd = rva_of(&CLIMB_PROBE_FWD_MASK).unwrap() + 8;
        let corner = rva_of(&CLIMB_PROBE_CORNER_MASK).unwrap() + 13;
        let ret = rva_of(&CLIMB_WALL_LOST_RET).unwrap() + 26;
        let first = rva_of(&CLIMB_PROBE_CLEAR_BITS).unwrap() + 4;
        let third = rva_of(&CLIMB_PROBE_CLEAR_BIT5).unwrap() + 1;
        assert_eq!(mem.get(fwd, 4), 0x124u32.to_le_bytes());
        assert_eq!(mem.get(corner, 4), 0x124u32.to_le_bytes());
        assert_eq!(mem.get(ret, 12), WALL_LOST_VOXEL_HOLD);
        assert_eq!(mem.get(first, 1), [0x03]);
        assert_eq!(mem.get(third, 1), [0x03]);
        // Nothing else changed: not the shared constant, not the checked-only sites.
        let allowed = [
            fwd..fwd + 4,
            corner..corner + 4,
            plane + 12..plane + 16,
            plane + 64..plane + 68,
            ret..ret + 12,
            first..first + 1,
            third..third + 1,
        ];
        let diff = changed(&before, &all(&mem));
        assert!(
            diff.iter().all(|i| allowed.iter().any(|r| r.contains(i))),
            "changed: {diff:x?}"
        );
        assert_eq!(mem.get(CONST_RVA, 4), 0.001f32.to_le_bytes());
    }

    #[test]
    fn a_missing_signature_refuses_and_writes_nothing() {
        let (mem, module) = image();
        // One byte of the top-entry ray mask changes: 0x100 becomes 0x101.
        let top = rva_of(&CLIMB_TOP_RAY_MASK).unwrap();
        mem.put(top + 8, &[0x01]);
        let before = all(&mem);
        let (result, pages) = install_here(&module, &on());
        let why = result.unwrap_err();
        assert_eq!(
            why,
            "1 of 16 climb signatures did not match exactly once, so nothing was written: \
             CLIMB_TOP_RAY_MASK: no match (or the section could not be read)"
        );
        assert!(pages.is_empty(), "no page is asked for");
        assert_eq!(all(&mem), before);
    }

    #[test]
    fn a_signature_that_matches_twice_refuses_and_names_it() {
        let (mem, module) = image();
        // A second copy of the orphan probe's bytes, after the placed signatures.
        mem.put(0x1f00, &bytes_of(&CLIMB_ORPHAN_RAY_MASK));
        let before = all(&mem);
        let (result, pages) = install_here(&module, &on());
        let why = result.unwrap_err();
        assert!(
            why.ends_with("CLIMB_ORPHAN_RAY_MASK: 2 matches, expected exactly one"),
            "{why}"
        );
        assert!(pages.is_empty());
        assert_eq!(all(&mem), before);
    }

    #[test]
    fn a_changed_tolerance_refuses_and_writes_nothing() {
        let (mem, module) = image();
        mem.put(CONST_RVA, &0.002f32.to_le_bytes());
        let before = all(&mem);
        let (result, pages) = install_here(&module, &on());
        assert_eq!(result, Err(NOT_0_001.to_owned()));
        assert!(
            pages.is_empty(),
            "the checks run before the page is asked for"
        );
        assert_eq!(all(&mem), before);
    }

    #[test]
    fn a_second_install_refuses_and_changes_nothing() {
        let (mem, module) = image();
        let (first, _pages) = install_here(&module, &on());
        assert!(first.is_ok(), "{first:?}");
        let after_first = all(&mem);
        // The forward-ray signature holds 0x100, and the first install wrote 0x124 there.
        let (second, pages) = install_here(&module, &on());
        let why = second.unwrap_err();
        assert!(why.contains("CLIMB_PROBE_FWD_MASK: no match"), "{why}");
        assert!(pages.is_empty());
        assert_eq!(all(&mem), after_first);
    }

    #[test]
    fn no_page_in_reach_refuses_and_changes_nothing() {
        let (mem, module) = image();
        let before = all(&mem);
        let mut asked = Vec::new();
        let no_page = |targets: &[usize], size: usize| {
            asked.push((targets.to_vec(), size));
            None
        };
        // SAFETY: as in `install_here`; the allocator gives no page, so nothing is written.
        let result = unsafe { install_with(&module, &on(), no_page) };
        assert_eq!(result, Err(NO_PAGE.to_owned()));
        let plane = module.base + rva_of(&CLIMB_PLANE_TOL_LOADS).unwrap();
        assert_eq!(asked, [(vec![plane + 16, plane + 68], 4)]);
        assert_eq!(all(&mem), before);
    }

    #[test]
    fn install_in_this_exe_finds_nothing_and_says_why() {
        let (result, pages) = install_here(&main_module(), &on());
        let why = result.unwrap_err();
        assert!(
            why.contains("climb signatures did not match exactly once"),
            "{why}"
        );
        assert!(pages.is_empty());
    }

    // ---- the real game exe (ignored: reads ENSHROUDED_STEAM_EXE, else the Steam install) -------

    /// Where each climb signature matches in the Steam exe (sha256 d64966e2...; the old build has
    /// the same `.text`). docs/re-notes.md, "Climb anything (experimental)".
    const ANCHORS: [(&str, u64); 16] = [
        ("CLIMB_PROBE_FWD_MASK", 0x1_4024_acc4),
        ("CLIMB_PROBE_CORNER_MASK", 0x1_4024_af46),
        ("CLIMB_PLANE_TOL_LOADS", 0x1_4024_afb0),
        ("CLIMB_TOL_LOAD_A", 0x1_4024_afb5),
        ("CLIMB_TOL_LOAD_B", 0x1_4024_afe8),
        ("CLIMB_WALL_LOST_RET", 0x1_4024_b48b),
        ("CLIMB_PROBE_CLEAR_BITS", 0x1_4024_ad82),
        ("CLIMB_PROBE_CLEAR_BIT5", 0x1_4024_ad54),
        ("CLIMB_TOP_RAY_MASK", 0x1_4024_c684),
        ("CLIMB_TOP_AABB_MASK", 0x1_4024_c737),
        ("CLIMB_TOP_CLEAR_BITS", 0x1_4024_c4bb),
        ("CLIMB_EXIT_CLEAR_BITS", 0x1_4024_a76e),
        ("CLIMB_EXIT_FLOOR_MASK", 0x1_4024_a917),
        ("CLIMB_ORPHAN_RAY_MASK", 0x1_4024_b230),
        ("CLIMB_CANCLIMB_TEST", 0x1_4041_9676),
        ("CLIMB_UPDATE_LOST_TEST", 0x1_4041_8d7f),
    ];

    #[test]
    #[ignore = "reads the Steam exe (ENSHROUDED_STEAM_EXE, else the Steam path); a skip shows only with -- --ignored --nocapture"]
    fn climb_sigs_unique_in_steam_exe() {
        use crate::build_id::test_support::steam_exe_or_skip;
        use crate::build_id::{classify, sha256_file};
        use crate::scan::{find_unique, parse_pe_file};
        let Some(path) = steam_exe_or_skip("climb_sigs_unique_in_steam_exe") else {
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
        let mut failures = Vec::new();
        for &sig in CLIMB {
            let anchor = ANCHORS
                .iter()
                .find(|(name, _)| *name == sig.name)
                .map(|(_, va)| *va)
                .expect("an anchor for every climb signature");
            let pattern = Pattern::parse(sig.pattern).unwrap();
            match find_unique(text, &pattern) {
                Ok(at) => {
                    let va = text_va + at as u64;
                    println!("{:<26} match {va:#x}  (anchor {anchor:#x})", sig.name);
                    if va != anchor {
                        failures.push(format!("{}: matched {va:#x}, anchor {anchor:#x}", sig.name));
                    }
                    found.push((sig, va as usize));
                }
                Err(error) => failures.push(format!("{}: {error}", sig.name)),
            }
        }
        assert!(failures.is_empty(), "{failures:?}");
        assert_eq!(found.len(), 16);

        // The plan on the file's bytes, as `read_snapshot` would take them from memory.
        let site = |sig: &Sig, index: usize, len: usize| -> (usize, Vec<u8>) {
            let at = found.iter().find(|(s, _)| s.name == sig.name).unwrap().1;
            let addr = at + sig.patch_offsets[index];
            let bytes = pe.bytes_at_va(&file, addr as u64, len).unwrap().to_vec();
            (addr, bytes)
        };
        let four = |sig: &Sig, index: usize| {
            let (addr, bytes) = site(sig, index, 4);
            Site::<4> {
                addr,
                bytes: bytes.try_into().unwrap(),
            }
        };
        let (wall_addr, wall_bytes) = site(&CLIMB_WALL_LOST_RET, 0, 12);
        let (first_addr, first_bytes) = site(&CLIMB_PROBE_CLEAR_BITS, 0, 1);
        let (third_addr, third_bytes) = site(&CLIMB_PROBE_CLEAR_BIT5, 0, 1);
        let tol_a = four(&CLIMB_PLANE_TOL_LOADS, 0);
        let tol_b = four(&CLIMB_PLANE_TOL_LOADS, 1);
        let value = |load: &Site<4>| {
            let target = load_target(load)?;
            let bytes = pe.bytes_at_va(&file, target as u64, 4).ok()?;
            Some(u32::from_le_bytes(bytes.try_into().ok()?))
        };
        let snap = Snapshot {
            base: 0x1_4000_0000,
            fwd_mask: four(&CLIMB_PROBE_FWD_MASK, 0),
            corner_mask: four(&CLIMB_PROBE_CORNER_MASK, 0),
            tol_a,
            tol_b,
            tol_values: [value(&tol_a), value(&tol_b)],
            wall_lost: Site {
                addr: wall_addr,
                bytes: wall_bytes.try_into().unwrap(),
            },
            clear_first: Site {
                addr: first_addr,
                bytes: first_bytes.try_into().unwrap(),
            },
            clear_third: Site {
                addr: third_addr,
                bytes: third_bytes.try_into().unwrap(),
            },
        };
        println!("snapshot: {snap:x?}");
        assert_eq!(
            snap,
            game(),
            "the Steam exe is the snapshot the unit tests use"
        );
        let all_on = ExperimentalCfg {
            climb_voxel_hold: true,
            climb_clearance: ClimbClearance::Solid,
            ..on()
        };
        assert_eq!(check_game(&snap, &all_on), Ok(0x1_413b_825c));
        let writes = plan(&snap, &all_on, PAGE).unwrap();
        for w in &writes {
            println!(
                "write {:<32} at {:#x}: {:02x?} -> {:02x?}",
                w.what, w.addr, w.old, w.new
            );
        }
        assert_eq!(writes.len(), 8);
    }
}
