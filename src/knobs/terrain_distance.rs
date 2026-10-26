//! Terrain detail distance (design.md 7.3): a pre-hook on the terrain update.

use super::{create_and_queue, TERRAIN};
use crate::config::KnobsCfg;
use crate::entry;
use crate::frame_timer::qpc_now;
use crate::guard::{guard, Feature};
use crate::scan::{find_in_sections, main_module, Module, Pattern};
use crate::sigs::TERRAIN_UPDATE_FN;
use log::Level;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Four register arguments, no return value. R9 is forwarded even if unused by the game.
/// The camera points at three i64 values; the hook never reads it. Its type stays opaque.
/// The params buffer has at least 40 bytes; the hook reads and writes only the first 32.
pub type TerrainUpdateFn = unsafe extern "system" fn(*mut u8, *const f32, *mut [f32; 8], usize);
/// MinHook's trampoline, stored before queued hooks can be enabled.
static TRAMPOLINE: AtomicUsize = AtomicUsize::new(0);
/// Test only: panic before publishing (1) or after scaling (2).
#[cfg(test)]
static PANIC_IN_PRE: AtomicUsize = AtomicUsize::new(0);
/// Test only: panic after the original to exercise the nested guard.
#[cfg(test)]
static PANIC_IN_POST: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Forward the original register arguments through MinHook's trampoline.
/// # Safety
/// The trampoline is zero or a live TerrainUpdateFn trampoline. The arguments meet its contract.
unsafe fn call_original(
    trampoline: usize,
    mgr: *mut u8,
    cam: *const f32,
    params: *mut [f32; 8],
    r9: usize,
) {
    if trampoline == 0 {
        return;
    }
    // SAFETY: the nonzero address has the type stated in this function's contract.
    let original = unsafe { std::mem::transmute::<usize, TerrainUpdateFn>(trampoline) };
    // SAFETY: these are the game's unchanged register arguments.
    unsafe { original(mgr, cam, params, r9) };
}

/// Publish the menu value, then scale the temporary caller buffer.
/// # Safety
/// params is null or an aligned, uniquely accessed live buffer of at least 40 bytes;
/// the hook reads and writes only the first 32.
unsafe fn pre_work(params: *mut [f32; 8]) {
    #[cfg(test)]
    if PANIC_IN_PRE.load(Ordering::Relaxed) == 1 {
        panic!("pre-work before scale");
    }
    // SAFETY: the caller guarantees validity, alignment and exclusive access if nonnull.
    let Some(params) = (unsafe { params.as_mut() }) else {
        return;
    };
    let [dist0, ..] = params;
    TERRAIN.publish(*dist0, qpc_now());
    scale_params(params, TERRAIN.value());
    #[cfg(test)]
    if PANIC_IN_PRE.load(Ordering::Relaxed) == 2 {
        panic!("pre-work after scale");
    }
}

/// Publish and scale before the original; restore the caller's 32-byte hook prefix afterward.
/// The pre-work and all fallible post-work have nested guards, so the outer fallback can never
/// call the original again after it ran. Restoration is a plain nonpanicking raw copy and runs
/// even when a nested guard switched the feature off. The original is native and cannot unwind.
/// No allocation, lock or log occurs on the normal render path.
/// # Safety
/// The game's four-argument contract: params is null or an aligned, exclusively accessed,
/// readable and writable buffer of at least 40 bytes; the hook reads and writes only the first 32.
/// cam points at three i64 values; the hook never reads it. mgr and cam remain unchanged.
/// All four arguments also satisfy the original's full buffer contract.
/// The original does not free params, which remains live until this detour returns.
pub unsafe extern "system" fn terrain_update_detour(
    mgr: *mut u8,
    cam: *const f32,
    params: *mut [f32; 8],
    r9: usize,
) {
    let trampoline = TRAMPOLINE.load(Ordering::Acquire);
    // SAFETY: install stores a matching trampoline before enabling; arguments are unchanged.
    let original = || unsafe { call_original(trampoline, mgr, cam, params, r9) };
    guard(Feature::TerrainDistance, original, || {
        let saved = if params.is_null() {
            None
        } else {
            // SAFETY: the caller owns all 32 bytes throughout this call; a byte array preserves
            // NaN payloads and signed zero without alignment or float conversions.
            Some(unsafe { params.cast::<[u8; 32]>().read_unaligned() })
        };
        guard(
            Feature::TerrainDistance,
            || (),
            || {
                // SAFETY: the detour contract guarantees the buffer's validity and exclusive access.
                unsafe { pre_work(params) };
            },
        );
        original();
        // Every potentially panicking action after the original must stay inside this guard.
        guard(
            Feature::TerrainDistance,
            || (),
            || {
                #[cfg(test)]
                if PANIC_IN_POST.load(Ordering::Relaxed) {
                    panic!("post-work boom");
                }
            },
        );
        if let Some(saved) = saved {
            // SAFETY: the original has returned and params is still the same live writable
            // 32-byte caller buffer. This copy cannot panic, including when a guard skipped work.
            unsafe { params.cast::<[u8; 32]>().write_unaligned(saved) };
        }
    });
}
const _: TerrainUpdateFn = terrain_update_detour;

/// Create and queue this detour, storing its trampoline before the enable batch.
/// # Safety
/// target meets create_and_queue's contract and has the TerrainUpdateFn ABI.
unsafe fn hook_at(target: usize) -> Result<(), String> {
    // SAFETY: the caller guarantees a hookable entry with the detour's exact type.
    let trampoline =
        unsafe { create_and_queue(target, terrain_update_detour as *const () as usize) }?;
    TRAMPOLINE.store(trampoline, Ordering::Release);
    Ok(())
}
/// Verified mov eax,[rsi+0xc440], required by plan Task 14 and issue 25.
const BODY_BYTES: [u8; 6] = [0x8b, 0x86, 0x40, 0xc4, 0x00, 0x00];
/// Number of bytes checked immediately after the signature's prologue.
const BODY_WINDOW: usize = 0x80;

/// Resolve one prologue and require the complete body window in its readable mapped section.
/// Start-up only, under find_in_sections's no-concurrent-write/protection-change contract.
fn checked_target(m: &Module) -> Result<usize, String> {
    let sig = TERRAIN_UPDATE_FN;
    let pattern = Pattern::parse(sig.pattern).map_err(|error| error.player_text())?;
    let scan_error = |error: crate::scan::ScanError| {
        log::debug!("terrain distance: {}: {error}", sig.name);
        error.player_text()
    };
    // First require uniqueness of the prologue itself, including truncated candidates.
    let found = find_in_sections(m, sig.sections, &pattern).map_err(scan_error)?;
    // Appending wildcards makes the scanner verify that the WHOLE following window belongs to
    // the same section and committed readable module allocation. It also rejects a truncated
    // window even when its first bytes happen to hold BODY_BYTES.
    let bounded = Pattern::parse(&format!("{} {}", sig.pattern, "?? ".repeat(BODY_WINDOW)))
        .map_err(|error| error.player_text())?;
    let bounded_found = find_in_sections(m, sig.sections, &bounded).map_err(scan_error)?;
    if bounded_found != found {
        return Err("the terrain body window moved during the scan".into());
    }
    let body_start = found
        .checked_add(pattern.len())
        .ok_or("terrain body address overflow")?;
    // SAFETY: bounded's unique match proved these 0x80 bytes lie in the same fully readable
    // mapped section, inside both SizeOfImage and Module.size (find_in_sections). u8 has no
    // alignment or validity requirement. The start-up contract forbids concurrent modification
    // or a protection change between that validation and this short read.
    let body = unsafe { std::slice::from_raw_parts(body_start as *const u8, BODY_WINDOW) };
    if !body
        .windows(BODY_BYTES.len())
        .any(|window| window == BODY_BYTES)
    {
        log::debug!("terrain distance: body mismatch at {body_start:#x}: {body:02x?}");
        return Err("the terrain body check did not match the verified game code".into());
    }
    found
        .checked_add(sig.patch_offset())
        .ok_or_else(|| "terrain hook address overflow".into())
}

/// Find and check the terrain function, create its hook and queue it for the shared batch.
/// Call only at start-up on the running exe, with no concurrent code or protection changes.
pub fn install(m: &Module) -> Result<(), String> {
    let target = checked_target(m)?;
    log::debug!(
        "terrain distance: {} matched at {target:#x}",
        TERRAIN_UPDATE_FN.name
    );
    // SAFETY: the unique verified prologue starts the terrain update function (re-notes anchor
    // 0x1401e2e80, patch offset 0), and the exact documented body instruction matched in its
    // first 0x80 bytes after the prologue. MinHook relocates the entry instructions; the first
    // instruction is five bytes. The four-register no-return ABI is issue 25's required ABI.
    // This is start-up, before any queued hook is enabled.
    unsafe { hook_at(target) }
}

/// Decide once, then return the single player-facing status line and level.
fn start_up(
    knobs: &KnobsCfg,
    code_patches: bool,
    install: impl FnOnce() -> Result<(), String>,
) -> (Level, String) {
    if !code_patches {
        return (
            Level::Info,
            "terrain distance: off: code patches are off on this build".into(),
        );
    }
    if !knobs.enable_terrain_distance {
        return (
            Level::Info,
            "terrain distance: off: EnableTerrainDistance = 0".into(),
        );
    }
    match install() {
        Ok(()) => (
            Level::Info,
            "terrain distance: on (terrain update hook queued)".into(),
        ),
        Err(why) => (Level::Warn, format!("terrain distance: off: {why}")),
    }
}

/// Install only when this build and the terrain setting allow code hooks, then log one line.
pub fn init(knobs: &KnobsCfg) {
    let (level, line) = start_up(knobs, entry::code_patches_allowed(), || {
        install(&main_module())
    });
    log::log!(level, "{line}");
}

/// Scale the first two terrain distances, bounded to the menu or farther.
pub fn scale_params(p: &mut [f32; 8], factor: f32) {
    let factor = if factor.is_nan() {
        1.0
    } else {
        factor.clamp(1.0, 2.0)
    };
    let [dist0, dist1, ..] = p;
    *dist0 *= factor;
    if *dist1 < *dist0 {
        *dist1 *= factor;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn doubles_dist0_keeps_order() {
        let mut p = [32.0, 128.0, 256.0, 0.0, 0.0, 0.0, 0.0, 0.0];
        scale_params(&mut p, 2.0);
        assert_eq!(&p[..3], &[64.0, 128.0, 256.0]);
    }
    #[test]
    fn raises_dist1_when_overtaken() {
        let mut p = [32.0, 40.0, 256.0, 0.0, 0.0, 0.0, 0.0, 0.0];
        scale_params(&mut p, 1.5);
        assert_eq!(&p[..3], &[48.0, 60.0, 256.0]);
    }
    #[test]
    fn factor_clamped() {
        let mut p = [32.0; 8];
        scale_params(&mut p, 9.0);
        assert_eq!(p[0], 64.0);
    }
}

#[cfg(test)]
mod hook_tests {
    use super::*;
    use crate::guard::is_disabled;
    use crate::knobs::apply_queued;
    use minhook::MinHook;
    use std::ffi::{c_void, OsStr};
    use std::process::Command;
    use std::sync::atomic::{AtomicU32, Ordering::Relaxed};

    /// Number of calls made by the native fake.
    static CALLS: AtomicU32 = AtomicU32::new(0);

    /// Native stand-in: copy params to mgr, preserve the fourth argument, and read cam.
    /// # Safety
    /// All buffers must be valid for their declared writes and reads.
    #[unsafe(naked)]
    unsafe extern "system" fn fake(
        _mgr: *mut u8,
        _cam: *const f32,
        _params: *mut [f32; 8],
        _r9: usize,
    ) {
        core::arch::naked_asm!(
            "movups xmm0, [r8]",
            "movups [rcx], xmm0",
            "movups xmm0, [r8 + 16]",
            "movups [rcx + 16], xmm0",
            "mov [rcx + 32], r9",
            "mov eax, [rdx]",
            "mov [rcx + 40], eax",
            "lock inc dword ptr [rip + {calls}]",
            "ret",
            calls = sym CALLS,
        );
    }

    /// Run one test alone, with fresh hook and guard globals.
    fn in_child(name: &str, body: impl FnOnce()) {
        const ENV: &str = "ENSHROUDED_HEADROOM_TERRAIN_CHILD";
        if std::env::var_os(ENV).as_deref() == Some(OsStr::new(name)) {
            body();
            return;
        }
        let module = module_path!().split_once("::").unwrap().1;
        let full = format!("{module}::{name}");
        let output = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", &full, "--nocapture", "--test-threads=1"])
            .env(ENV, name)
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            output.status.success(),
            "{full}: {stdout}\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(stdout.contains("1 passed"), "{stdout}");
    }

    /// Exercise the real detour through MinHook and check the caller's entire buffer.
    fn scenario(pre_panic: usize, post_panic: bool, disable_before: bool) {
        assert!(!is_disabled(Feature::TerrainDistance));
        TERRAIN.set(1.5);
        let target = fake as *const () as usize;
        // SAFETY: fake is a leaf function with this detour's ABI and at least five entry bytes;
        // nothing jumps into those instructions or executes it during installation.
        unsafe { hook_at(target) }.unwrap();
        assert_eq!(apply_queued(), Ok(1));
        if disable_before {
            crate::guard::disable(Feature::TerrainDistance, "test");
        }
        PANIC_IN_PRE.store(pre_panic, Relaxed);
        PANIC_IN_POST.store(post_panic, Relaxed);
        let before = [
            32.0,
            40.0,
            256.0,
            f32::from_bits(0x7fc01234),
            -0.0,
            6.0,
            7.0,
            8.0,
        ];
        let mut params = before;
        let mut out = [0u8; 48];
        let cam = 17.0f32;
        let function: TerrainUpdateFn = std::hint::black_box(fake);
        // SAFETY: out has room for fake's stores, cam and params are live and uniquely owned.
        unsafe { function(out.as_mut_ptr(), &cam, &mut params, 0xfedcba9876543210) };
        assert_eq!(CALLS.load(Relaxed), 1, "original must run exactly once");
        assert_eq!(
            params.map(f32::to_bits),
            before.map(f32::to_bits),
            "restore all 32 bytes"
        );
        let observed = f32::from_le_bytes(out[..4].try_into().unwrap());
        assert_eq!(
            observed,
            if disable_before || pre_panic == 1 {
                32.0
            } else {
                48.0
            }
        );
        assert_eq!(
            usize::from_le_bytes(out[32..40].try_into().unwrap()),
            0xfedcba9876543210
        );
        assert_eq!(f32::from_le_bytes(out[40..44].try_into().unwrap()), 17.0);
        if !disable_before && pre_panic != 1 {
            assert_eq!(TERRAIN.baseline().0, 32.0, "publish before scale");
            assert_ne!(TERRAIN.last_seen(), 0);
        } else {
            assert_eq!(TERRAIN.last_seen(), 0);
        }
        if pre_panic != 0 || post_panic || disable_before {
            assert!(is_disabled(Feature::TerrainDistance));
            // Once disabled, a second call must forward unchanged, without another publish.
            let seen = TERRAIN.last_seen();
            // SAFETY: the same owned buffers remain valid.
            unsafe { function(out.as_mut_ptr(), &cam, &mut params, 9) };
            assert_eq!(CALLS.load(Relaxed), 2);
            assert_eq!(f32::from_le_bytes(out[..4].try_into().unwrap()), 32.0);
            assert_eq!(TERRAIN.last_seen(), seen);
        }
        // SAFETY: the hook was created above, and no call is in flight.
        unsafe { MinHook::remove_hook(target as *mut c_void) }.unwrap();
    }

    #[test]
    fn publishes_before_scale_and_restores_after_the_trampoline() {
        in_child(
            "publishes_before_scale_and_restores_after_the_trampoline",
            || scenario(0, false, false),
        );
    }
    #[test]
    fn the_original_runs_exactly_once_when_the_post_work_panics() {
        in_child(
            "the_original_runs_exactly_once_when_the_post_work_panics",
            || scenario(0, true, false),
        );
    }
    #[test]
    fn restores_when_pre_work_panics_after_scaling() {
        in_child("restores_when_pre_work_panics_after_scaling", || {
            scenario(2, false, false)
        });
    }
    #[test]
    fn restores_when_pre_work_panics_before_scaling() {
        in_child("restores_when_pre_work_panics_before_scaling", || {
            scenario(1, false, false)
        });
    }
    #[test]
    fn disabled_feature_forwards_once_without_scaling_or_publish() {
        in_child(
            "disabled_feature_forwards_once_without_scaling_or_publish",
            || scenario(0, false, true),
        );
    }
}

#[cfg(test)]
mod install_tests {
    use super::*;
    use std::cell::Cell;
    use windows_sys::Win32::System::Memory::{
        VirtualAlloc, VirtualFree, VirtualProtect, MEM_COMMIT, MEM_RELEASE, MEM_RESERVE,
        PAGE_NOACCESS, PAGE_READWRITE,
    };
    /// A synthetic mapped PE allocation, with a bounded .text section.
    struct Image(*mut u8);
    impl Image {
        fn new(text_len: usize) -> Self {
            // SAFETY: request a private writable allocation; no pointers are supplied.
            let ptr = unsafe {
                VirtualAlloc(
                    std::ptr::null(),
                    0x3000,
                    MEM_COMMIT | MEM_RESERVE,
                    PAGE_READWRITE,
                )
            }
            .cast::<u8>();
            assert!(!ptr.is_null());
            let image = Self(ptr);
            // SAFETY: VirtualAlloc gave this test exclusive ownership of 0x3000 writable bytes.
            let b = unsafe { std::slice::from_raw_parts_mut(ptr, 0x3000) };
            b[..2].copy_from_slice(b"MZ");
            b[0x3c..0x40].copy_from_slice(&0x80u32.to_le_bytes());
            b[0x80..0x84].copy_from_slice(b"PE\0\0");
            b[0x86..0x88].copy_from_slice(&1u16.to_le_bytes());
            b[0x94..0x96].copy_from_slice(&0xf0u16.to_le_bytes());
            b[0x98..0x9a].copy_from_slice(&0x20bu16.to_le_bytes());
            b[0xd0..0xd4].copy_from_slice(&0x3000u32.to_le_bytes());
            b[0x188..0x18d].copy_from_slice(b".text");
            b[0x190..0x194].copy_from_slice(&(text_len as u32).to_le_bytes());
            b[0x194..0x198].copy_from_slice(&0x1000u32.to_le_bytes());
            image
        }
        fn module(&self) -> Module {
            Module {
                base: self.0 as usize,
                size: 0x3000,
            }
        }
        fn put(&self, offset: usize, bytes: &[u8]) {
            assert!(offset + bytes.len() <= 0x3000);
            // SAFETY: the destination is within this test-owned writable allocation.
            unsafe {
                std::ptr::copy_nonoverlapping(bytes.as_ptr(), self.0.add(offset), bytes.len())
            };
        }
        fn signature(&self, offset: usize) {
            let bytes: Vec<u8> = TERRAIN_UPDATE_FN
                .pattern
                .split_whitespace()
                .map(|v| u8::from_str_radix(v, 16).unwrap())
                .collect();
            self.put(offset, &bytes);
        }
    }
    impl Drop for Image {
        fn drop(&mut self) {
            // SAFETY: this is the original VirtualAlloc pointer, freed exactly once.
            assert_ne!(unsafe { VirtualFree(self.0.cast(), 0, MEM_RELEASE) }, 0);
        }
    }
    #[test]
    fn body_check_is_bounded_after_the_prologue() {
        let plen = Pattern::parse(TERRAIN_UPDATE_FN.pattern).unwrap().len();
        for offset in [0, 0x80 - BODY_BYTES.len()] {
            let image = Image::new(plen + 0x80);
            image.signature(0x1000);
            image.put(0x1000 + plen + offset, &BODY_BYTES);
            assert_eq!(
                checked_target(&image.module()).unwrap(),
                image.module().base + 0x1000
            );
        }
        for offset in [0x80 - BODY_BYTES.len() + 1, 0x80] {
            let image = Image::new(0x300);
            image.signature(0x1000);
            image.put(0x1000 + plen + offset, &BODY_BYTES);
            assert!(checked_target(&image.module()).is_err(), "offset {offset}");
        }
    }
    #[test]
    fn refuses_absent_duplicate_shifted_and_truncated_ranges() {
        let plen = Pattern::parse(TERRAIN_UPDATE_FN.pattern).unwrap().len();
        let absent = Image::new(0x300);
        assert!(checked_target(&absent.module()).is_err());
        absent.signature(0x1000);
        assert!(
            checked_target(&absent.module()).is_err(),
            "missing body bytes"
        );
        absent.put(0x1000 + plen, &BODY_BYTES);
        absent.signature(0x1100);
        assert!(checked_target(&absent.module())
            .unwrap_err()
            .contains("2 times"));
        let truncated = Image::new(plen + 0x7f);
        truncated.signature(0x1000);
        truncated.put(0x1000 + plen, &BODY_BYTES);
        assert!(
            checked_target(&truncated.module()).is_err(),
            "full window must fit"
        );
        let shifted = Image::new(plen + 0x80);
        shifted.signature(0x1001);
        shifted.put(0x1001 + plen, &BODY_BYTES);
        assert!(
            checked_target(&shifted.module()).is_err(),
            "shifted range leaves section"
        );
        let mut module = truncated.module();
        module.size = 0x1000 + plen;
        assert!(checked_target(&module).is_err(), "module bound before body");
        let inaccessible = Image::new(0x1100);
        inaccessible.signature(0x1000);
        inaccessible.put(0x1000 + plen, &BODY_BYTES);
        let mut previous = 0;
        // SAFETY: the final page belongs to this allocation; it is not accessed afterward.
        assert_ne!(
            unsafe {
                VirtualProtect(
                    inaccessible.0.add(0x2000).cast(),
                    0x1000,
                    PAGE_NOACCESS,
                    &mut previous,
                )
            },
            0
        );
        assert!(
            checked_target(&inaccessible.module()).is_err(),
            "unreadable section"
        );
    }
    #[test]
    fn startup_checks_build_and_setting_before_install() {
        for (code, enabled, expected) in [
            (false, true, "code patches"),
            (true, false, "EnableTerrainDistance"),
        ] {
            let mut knobs = crate::config::Config::default().knobs;
            knobs.enable_terrain_distance = enabled;
            let called = Cell::new(false);
            let (level, line) = start_up(&knobs, code, || {
                called.set(true);
                Ok(())
            });
            assert!(!called.get());
            assert_eq!(level, Level::Info);
            assert!(line.contains(expected));
        }
        let knobs = crate::config::Config::default().knobs;
        assert!(knobs.enable_terrain_distance);
        assert_eq!(start_up(&knobs, true, || Ok(())).0, Level::Info);
        let (level, line) = start_up(&knobs, true, || Err("refused".into()));
        assert_eq!(level, Level::Warn);
        assert!(line.contains("refused"));
    }
    #[test]
    #[ignore = "reads the real Steam game exe"]
    fn steam_terrain_signature_and_body_match_the_verified_anchor() {
        let Some(path) = crate::build_id::test_support::steam_exe_or_skip(
            "steam_terrain_signature_and_body_match_the_verified_anchor",
        ) else {
            return;
        };
        let bytes = std::fs::read(path).unwrap();
        let pe = crate::scan::parse_pe_file(&bytes).unwrap();
        let (va, text) = pe.section_va_bytes(&bytes, ".text").unwrap();
        let pattern = Pattern::parse(TERRAIN_UPDATE_FN.pattern).unwrap();
        let at = crate::scan::find_unique(text, &pattern).unwrap();
        assert_eq!(va + at as u64, 0x1401e2e80);
        let body = &text[at + pattern.len()..at + pattern.len() + 0x80];
        assert!(
            body.windows(BODY_BYTES.len()).any(|w| w == BODY_BYTES),
            "body bytes {body:02x?}"
        );
    }
}

#[cfg(test)]
mod scale_edge_tests {
    use super::scale_params;
    #[test]
    fn nan_and_low_factors_keep_the_menu_value_and_tail_bits() {
        for factor in [f32::NAN, f32::NEG_INFINITY, -10.0, 0.0, 0.9, 1.0] {
            let mut p = [
                32.0,
                128.0,
                f32::from_bits(0x7fc01234),
                -0.0,
                4.0,
                5.0,
                6.0,
                7.0,
            ];
            let before = p.map(f32::to_bits);
            scale_params(&mut p, factor);
            assert_eq!(p.map(f32::to_bits), before, "factor {factor}");
        }
    }
    #[test]
    fn upper_clamp_and_each_terrain_step_leave_the_tail_unchanged() {
        for (factor, expected) in [
            (1.25, 40.0),
            (1.5, 48.0),
            (1.75, 56.0),
            (2.0, 64.0),
            (f32::INFINITY, 64.0),
        ] {
            let mut p = [32.0, 128.0, 256.0, -0.0, 4.0, 5.0, 6.0, 7.0];
            scale_params(&mut p, factor);
            assert_eq!(p, [expected, 128.0, 256.0, -0.0, 4.0, 5.0, 6.0, 7.0]);
            assert_eq!(p[3].to_bits(), (-0.0f32).to_bits());
        }
        let mut p = [32.0, 48.0, 256.0, 0.0, 0.0, 0.0, 0.0, 0.0];
        scale_params(&mut p, 1.5);
        assert_eq!(p[1], 48.0, "equality must not scale dist1");
    }
}
