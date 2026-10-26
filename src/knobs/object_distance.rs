//! Object draw distance (design.md 7.1): a post-hook on the game's scene-settings function.
//!
//! The game's `FUN_1400f72b0(scene, settings, extra)` (found by `sigs::SCENE_SETTINGS_FN`) turns
//! the menu settings into the scene-settings struct the renderer reads. It runs once per frame on
//! a struct that its caller rebuilds every time, and it rewrites the fields below on every call.
//! RE facts (docs/re-notes.md; the controller's REA check, 2026-10-26): three register arguments
//! (RCX scene, RDX render settings, R8 three dwords), no return value, a leaf function.
//!
//! After the original returns, the hook ([`scene_settings_detour`]):
//! 1. always publishes the detail-scattering level (`+0x290`) to `SCATTER_LEVEL` and the time,
//!    which the governor uses as its world-rendering signal;
//! 2. publishes the game's object multiplier (`+0x33c`) as `OBJECT`'s baseline, but only when it
//!    is exactly a value the game writes (3.0, 2.0 or 1.0); any other value is counted in
//!    `OBJECT_UNKNOWN_BASELINE` and both fields are left alone (the rule in `post_work`);
//! 3. for a game value, and with `EnableObjectDistance = 1` only, multiplies the two pixel-size
//!    multipliers (`+0x33c`, `+0x340`) by `OBJECT`'s factor, clamped to `[0.5, 1.0]`. A smaller
//!    multiplier culls less, so a factor below 1.0 draws farther and is never below the menu
//!    value.
//!
//! The hook is created and queued by [`install`] and enabled with every other queued hook at the
//! end of `entry::start` (`knobs::enable_queued_hooks`).

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use log::Level;

use super::{create_and_queue, OBJECT, OBJECT_UNKNOWN_BASELINE, SCATTER_LEVEL};
use crate::config::KnobsCfg;
use crate::entry;
use crate::frame_timer::qpc_now;
use crate::guard::{guard, Feature};
use crate::scan::{find_in_sections, main_module, Module, Pattern};
use crate::sigs::SCENE_SETTINGS_FN;

/// `scene+0x33c` (f32): the pixel-size multiplier of the main GPU culling pass. The game writes
/// 3.0 / 2.0 / 1.0 for Models = Performance / Balance / Quality.
const PIXEL_SIZE_MAIN: usize = 0x33c;
/// `scene+0x340` (f32): the pixel-size multiplier of the model-chunk pass (1.5 / 1.0 / 1.0).
const PIXEL_SIZE_CHUNKS: usize = 0x340;
/// `scene+0x290` (u32): the detail-scattering level, 0..3.
const DETAIL_SCATTERING: usize = 0x290;

/// The smallest factor: `meshCount` is fixed, and a lower one could overflow it (design.md 7.1).
const MIN_FACTOR: f32 = 0.5;
/// The largest factor: the menu value itself.
const MAX_FACTOR: f32 = 1.0;

/// The type of the game function and of its detour: three register arguments, no return value.
pub type SceneSettingsFn =
    unsafe extern "system" fn(scene: *mut u8, settings: *const u8, extra: *const u32);

/// MinHook's trampoline for the game function, or 0. Stored by [`install`] before the hook is
/// enabled; read by the detour.
static TRAMPOLINE: AtomicUsize = AtomicUsize::new(0);

/// `EnableObjectDistance`. Set by [`init`] before the hook is enabled; read by the detour.
static APPLY_FACTOR: AtomicBool = AtomicBool::new(false);

/// The bits of the only values the game function writes to `scene+0x33c`: 3.0, 2.0 and 1.0
/// (models Performance / Balance / Quality; the stores at 0x1400f76e8, 0x1400f76bf and
/// 0x1400f7696, REA ev_7ac38aaf).
const GAME_MAIN_VALUES: [u32; 3] = [3.0f32.to_bits(), 2.0f32.to_bits(), 1.0f32.to_bits()];

/// Is `v`, bit for bit, one of the values the game writes to `scene+0x33c`?
fn is_game_value(v: f32) -> bool {
    GAME_MAIN_VALUES.contains(&v.to_bits())
}

/// Test only: makes `post_work` panic at its top, so a test can drive the real detour's panic
/// path. Not in a non-test build.
#[cfg(test)]
static PANIC_IN_POST: AtomicBool = AtomicBool::new(false);

/// `factor` clamped to `[MIN_FACTOR, MAX_FACTOR]`; NaN counts as 1.0 (no change).
fn clamp_factor(factor: f32) -> f32 {
    if factor.is_nan() {
        MAX_FACTOR
    } else {
        factor.clamp(MIN_FACTOR, MAX_FACTOR)
    }
}

/// Multiply the f32 at `scene + PIXEL_SIZE_MAIN` and at `scene + PIXEL_SIZE_CHUNKS` by `factor`,
/// clamped to `[0.5, 1.0]` (NaN: no change). A factor of 1.0 writes nothing.
///
/// # Safety
///
/// `scene` points to a scene-settings struct, at least `0x344` bytes, that no other thread
/// accesses meanwhile. No alignment is needed.
pub unsafe fn apply_to_scene(scene: *mut u8, factor: f32) {
    let factor = clamp_factor(factor);
    if factor == MAX_FACTOR {
        return;
    }
    for offset in [PIXEL_SIZE_MAIN, PIXEL_SIZE_CHUNKS] {
        let field = scene.wrapping_add(offset).cast::<f32>();
        // SAFETY: `field` is inside the struct at `scene` (this function's contract: offsets
        // below 0x344), which nothing else accesses meanwhile; the unaligned read and write need
        // no alignment.
        unsafe { field.write_unaligned(field.read_unaligned() * factor) };
    }
}

/// Publish what the game wrote. Reads only. See `publish_checked`, which this is.
///
/// # Safety
///
/// `scene` points to a scene-settings struct, at least `0x344` bytes, that no other thread writes
/// meanwhile. No alignment is needed.
pub unsafe fn publish_from_scene(scene: *const u8, now: u64) {
    // SAFETY: this function's contract is `publish_checked`'s.
    unsafe { publish_checked(scene, now) };
}

/// Publish what the game wrote, and say whether `scene + PIXEL_SIZE_MAIN` holds a game value.
///
/// - Always: the u32 at `scene + DETAIL_SCATTERING` to `SCATTER_LEVEL`, and `now` as `OBJECT`'s
///   last-seen time, stored last so a reader that sees it sees the rest.
/// - A game value ([`is_game_value`]): it is published as `OBJECT`'s baseline; returns `true`.
/// - Any other value: no baseline is published, `OBJECT_UNKNOWN_BASELINE` goes up by one (it
///   wraps); returns `false`.
///
/// Reads only. No allocation, lock or log.
///
/// # Safety
///
/// As [`publish_from_scene`].
unsafe fn publish_checked(scene: *const u8, now: u64) -> bool {
    // SAFETY: both fields are inside the struct at `scene` (this function's contract), which
    // nothing writes meanwhile; unaligned reads need no alignment.
    let (main, level) = unsafe {
        (
            scene
                .wrapping_add(PIXEL_SIZE_MAIN)
                .cast::<f32>()
                .read_unaligned(),
            scene
                .wrapping_add(DETAIL_SCATTERING)
                .cast::<u32>()
                .read_unaligned(),
        )
    };
    SCATTER_LEVEL.store(level, Ordering::Relaxed);
    if is_game_value(main) {
        OBJECT.publish(main, now);
        true
    } else {
        OBJECT_UNKNOWN_BASELINE.fetch_add(1, Ordering::Relaxed);
        OBJECT.seen(now);
        false
    }
}

/// The detour's work after the original returned. A null `scene` does nothing.
///
/// **The rule: the hook never scales a field it scaled before, with the `OBJECT_STEPS` factors.**
/// The game function writes only 3.0, 2.0 or 1.0 to `+0x33c` (REA ev_7ac38aaf). For a models
/// value of 3 or more it returns without writing `+0x33c`/`+0x340` at all (the `JNZ` at
/// 0x1400f7694). So:
/// - `+0x33c` holds exactly 3.0, 2.0 or 1.0: it is the game's value for this call. It is
///   published as the baseline, and with `apply` both fields are scaled by `factor`.
/// - Any other value (one this hook scaled earlier, or one the game did not write): both fields
///   are left alone and no baseline is published. The scatter level and the time are still
///   published (the world is rendering), and `OBJECT_UNKNOWN_BASELINE` counts the call.
///
/// This holds for the `OBJECT_STEPS` values, not for every factor. In the clamp range a factor
/// writes from 0.5 up to, but not including, 1.0 (1.0 writes nothing). There, exactly two factors
/// map a game value onto a game value: 0.5 (2.0 -> 1.0) and 0.6666667 (3.0 -> 2.0). An exhaustive
/// f32 sweep found them (test `only_two_factors_in_the_clamp_range_map_a_game_value_onto_one`).
/// Neither is a step, and no product of 3.0, 2.0 or 1.0 with a step is a game value (test
/// `a_scaled_field_is_never_taken_for_a_game_value`). So with the steps, a field the hook already
/// scaled is never scaled again.
///
/// # Safety
///
/// `scene` is null, or points to a scene-settings struct, at least `0x420` bytes, that the
/// original has just written and that no other thread accesses meanwhile.
unsafe fn post_work(scene: *mut u8, apply: bool, factor: f32, now: u64) {
    // SAFETY: the original just wrote the caller-owned complete scene; this work runs inside
    // the inner guard, before distance publication or writes.
    unsafe { crate::features::hdr_peak::post_scene(scene) };
    #[cfg(test)]
    if PANIC_IN_POST.load(Ordering::Relaxed) {
        panic!("PANIC_IN_POST is set");
    }
    if scene.is_null() {
        return;
    }
    // SAFETY: this function's contract.
    let game_value = unsafe { publish_checked(scene, now) };
    if game_value && apply {
        // SAFETY: this function's contract.
        unsafe { apply_to_scene(scene, factor) };
    }
}

/// The detour's shape: `original` runs exactly once, then `post` in a guard of its own.
///
/// - `Feature::ObjectDistance` off: only `original` runs (the outer guard's fallback).
/// - `post` panics: the inner guard catches it, switches the feature off and does nothing more.
///   The outer guard sees no panic, so it does not run `original` a second time.
///
/// `original` itself never panics (it only calls the trampoline), so the outer fallback can only
/// run when the feature is already off.
fn run_after_original(original: impl Fn(), post: impl FnOnce()) {
    guard(Feature::ObjectDistance, &original, || {
        original();
        // The game's function has run. A panic from here on must not make the outer guard run it
        // a second time, so the rest runs in its own guard, whose fallback does nothing.
        guard(Feature::ObjectDistance, || (), post);
    });
}

/// Call the game function through `trampoline`; nothing if it is 0 (it cannot be: [`install`]
/// stores it before the hook is enabled).
///
/// # Safety
///
/// `trampoline` is 0 or MinHook's trampoline for the game function, and the arguments are the
/// ones the game passed.
unsafe fn call_original(trampoline: usize, scene: *mut u8, settings: *const u8, extra: *const u32) {
    if trampoline == 0 {
        return;
    }
    // SAFETY: non-zero, so MinHook's trampoline, which runs the game function: of this type.
    let original = unsafe { std::mem::transmute::<usize, SceneSettingsFn>(trampoline) };
    // SAFETY: the game's own arguments, passed on unchanged.
    unsafe { original(scene, settings, extra) }
}

/// Stands in for the game's scene-settings function: runs it, then publishes and (with
/// `EnableObjectDistance = 1`) scales. See the module docs.
///
/// It runs once per frame on the game's render path, so it only touches atomics: no lock, no
/// allocation, no log.
///
/// # Safety
///
/// The game's contract: `scene` points to the caller-owned scene, at least `0x420` bytes,
/// and `settings` and `extra` are what it reads.
pub unsafe extern "system" fn scene_settings_detour(
    scene: *mut u8,
    settings: *const u8,
    extra: *const u32,
) {
    let trampoline = TRAMPOLINE.load(Ordering::Acquire);
    // SAFETY: `trampoline` is 0 or the trampoline for the game function; the arguments are the
    // game's own, passed on unchanged.
    let original = move || unsafe { call_original(trampoline, scene, settings, extra) };
    run_after_original(original, move || {
        // SAFETY: these are the original caller's settings and preferences pointers; the reader
        // checks their verified relationship before reading the options prefix.
        crate::reflex::publish_menu(unsafe { crate::reflex::read_menu(settings, extra.cast()) });
        let apply = APPLY_FACTOR.load(Ordering::Acquire);
        // SAFETY: `post` runs right after the original wrote the struct at `scene`, on the game's
        // thread, which owns that struct (a stack local of the caller).
        unsafe { post_work(scene, apply, OBJECT.value(), qpc_now()) };
    });
}

const _: SceneSettingsFn = scene_settings_detour;

/// Create the hook on `target` with [`scene_settings_detour`] and queue it; store the trampoline
/// for the detour before anything can enable the hook. The part of [`install`] that a test can
/// run on a stand-in function.
///
/// # Safety
///
/// The contract of `create_and_queue` for `target`, whose type is [`SceneSettingsFn`].
unsafe fn hook_at(target: usize) -> Result<(), String> {
    let detour = scene_settings_detour as *const () as usize;
    // SAFETY: the caller's contract; the detour has the target's type (`const _` above).
    let trampoline = unsafe { create_and_queue(target, detour) }?;
    TRAMPOLINE.store(trampoline, Ordering::Release);
    Ok(())
}

/// Find `SCENE_SETTINGS_FN` in `m` and create the hook on it, queued; [`enable_queued_hooks`]
/// enables it. `Err` is the reason for the log.
///
/// # Start-up only
///
/// Call this from `entry::start` only, like `scan::find_in_sections`, with `m` the running exe
/// (`scan::main_module()`).
///
/// [`enable_queued_hooks`]: super::enable_queued_hooks
pub fn install(m: &Module) -> Result<(), String> {
    let name = SCENE_SETTINGS_FN.name;
    let pattern =
        Pattern::parse(SCENE_SETTINGS_FN.pattern).map_err(|error| format!("{name}: {error}"))?;
    let found = find_in_sections(m, SCENE_SETTINGS_FN.sections, &pattern).map_err(|error| {
        log::debug!("object distance: {name}: {error}");
        error.player_text()
    })?;
    let target = found
        .checked_add(SCENE_SETTINGS_FN.patch_offset())
        .ok_or_else(|| format!("{name}: the hook site is past the end of memory"))?;
    log::debug!("object distance: {name} matched at {target:#x}");
    // SAFETY: `target` is the entry of the game's scene-settings function: the one match in
    // `.text` of a 70-byte signature that starts at the entry (sigs.rs; patch offset 0). Its
    // first two instructions are 11 bytes (`mov eax,[r8]`; `movss xmm1,[rip+disp32]`, which
    // MinHook relocates), and no branch in the function lands inside them (REA ev_7ac38aaf: every
    // branch target is at entry+0x37 or later). The function is a leaf with the three-register,
    // no-return type `SceneSettingsFn` (dispatch notes, verified). Start-up only: this
    // function's rule.
    unsafe { hook_at(target) }
}

/// The body of [`init`], with the code-patch answer and the install passed in. `install` runs only
/// when code patches are allowed and at least one distance knob is on. Returns the log level and
/// the one line to log.
fn start_up(
    knobs: &KnobsCfg,
    code_patches: bool,
    install: impl FnOnce() -> Result<(), String>,
) -> (Level, String) {
    if !code_patches {
        return (
            Level::Info,
            "object distance: off: code patches are off on this build".to_owned(),
        );
    }
    if !(knobs.enable_object_distance
        || knobs.enable_scatter_distance
        || knobs.enable_terrain_distance)
    {
        return (
            Level::Info,
            "object distance: off: EnableObjectDistance, EnableScatterDistance and \
             EnableTerrainDistance are all 0, so the scene-settings hook is not installed"
                .to_owned(),
        );
    }
    match install() {
        Ok(()) if knobs.enable_object_distance => (
            Level::Info,
            "object distance: on (scene-settings hook queued)".to_owned(),
        ),
        Ok(()) => (
            Level::Info,
            "object distance: off (EnableObjectDistance = 0); the scene-settings hook is queued \
             for the other knobs and only publishes"
                .to_owned(),
        ),
        Err(why) => (Level::Warn, format!("object distance: off: {why}")),
    }
}

/// The start-up step: decide, install the hook (queued) if it is right to, and log one line.
/// Called once, from `entry::start`, after `code_patches_allowed` is set and before
/// `knobs::enable_queued_hooks`.
///
/// The hook is installed when ANY distance knob is on, because its publish is the governor's
/// world-rendering signal and the scatter level; it scales only with `EnableObjectDistance = 1`.
pub fn init(knobs: &KnobsCfg) {
    // Before the hook can be enabled, so its first call sees the setting.
    APPLY_FACTOR.store(knobs.enable_object_distance, Ordering::Release);
    let (level, line) = start_up(knobs, entry::code_patches_allowed(), || {
        install(&main_module())
    });
    log::log!(level, "{line}");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::guard::is_disabled;
    use crate::knobs::{
        apply_queued, OBJECT, OBJECT_STEPS, OBJECT_UNKNOWN_BASELINE, SCATTER_LEVEL,
    };
    use crate::scan::{find_unique, main_module};
    use log::Level;
    use minhook::MinHook;
    use std::cell::Cell;
    use std::ffi::{c_void, OsStr};
    use std::process::Command;
    use std::sync::atomic::{AtomicU32, Ordering::Relaxed};
    use std::sync::{Mutex, MutexGuard, PoisonError};

    fn put_f32(s: &mut [u8], at: usize, v: f32) {
        s[at..at + 4].copy_from_slice(&v.to_le_bytes());
    }

    fn get_f32(s: &[u8], at: usize) -> f32 {
        f32::from_le_bytes(s[at..at + 4].try_into().unwrap())
    }

    fn put_u32(s: &mut [u8], at: usize, v: u32) {
        s[at..at + 4].copy_from_slice(&v.to_le_bytes());
    }

    fn get_u32(s: &[u8], at: usize) -> u32 {
        u32::from_le_bytes(s[at..at + 4].try_into().unwrap())
    }

    /// `OBJECT` and `SCATTER_LEVEL` are process-wide and tests run in parallel, so every test in
    /// this process that writes them holds this lock.
    static GLOBALS: Mutex<()> = Mutex::new(());

    fn globals() -> MutexGuard<'static, ()> {
        GLOBALS.lock().unwrap_or_else(PoisonError::into_inner)
    }

    // ---- apply_to_scene ---------------------------------------------------------------------

    #[test]
    fn scales_both_fields_and_clamps() {
        let mut s = vec![0u8; 0x424];
        put_f32(&mut s, 0x33c, 1.0);
        put_f32(&mut s, 0x340, 1.0);
        unsafe { apply_to_scene(s.as_mut_ptr(), 0.72) };
        assert_eq!((get_f32(&s, 0x33c), get_f32(&s, 0x340)), (0.72, 0.72));
        put_f32(&mut s, 0x33c, 1.0);
        unsafe { apply_to_scene(s.as_mut_ptr(), 0.1) };
        assert_eq!(get_f32(&s, 0x33c), 0.5);
    }

    #[test]
    fn a_factor_of_1_or_more_or_nan_leaves_the_fields_alone() {
        // The clamp stops at 1.0, so the hook can only lower the multipliers: never below the
        // menu. A NaN factor (a bad value from anywhere) must not reach the game.
        for factor in [1.0, 1.5, f32::INFINITY, f32::NAN] {
            let mut s = vec![0u8; 0x424];
            put_f32(&mut s, 0x33c, 3.0);
            put_f32(&mut s, 0x340, 1.5);
            unsafe { apply_to_scene(s.as_mut_ptr(), factor) };
            assert_eq!(
                (get_f32(&s, 0x33c), get_f32(&s, 0x340)),
                (3.0, 1.5),
                "factor {factor}"
            );
        }
    }

    #[test]
    fn every_object_step_survives_the_clamp() {
        for step in OBJECT_STEPS {
            let mut s = vec![0u8; 0x424];
            put_f32(&mut s, 0x33c, 1.0);
            put_f32(&mut s, 0x340, 1.0);
            unsafe { apply_to_scene(s.as_mut_ptr(), step) };
            assert_eq!(
                (get_f32(&s, 0x33c), get_f32(&s, 0x340)),
                (step, step),
                "step {step}"
            );
        }
    }

    #[test]
    fn apply_writes_only_the_two_fields() {
        let mut s: Vec<u8> = (0..0x424).map(|i| (i % 251) as u8).collect();
        put_f32(&mut s, 0x33c, 2.0);
        put_f32(&mut s, 0x340, 1.0);
        let before = s.clone();
        unsafe { apply_to_scene(s.as_mut_ptr(), 0.5) };
        for (i, (a, b)) in before.iter().zip(&s).enumerate() {
            if !(0x33c..0x344).contains(&i) {
                assert_eq!(a, b, "byte {i:#x} changed");
            }
        }
        assert_eq!((get_f32(&s, 0x33c), get_f32(&s, 0x340)), (1.0, 0.5));
    }

    #[test]
    fn an_unaligned_scene_pointer_is_fine() {
        let mut s = vec![0u8; 0x401];
        put_f32(&mut s, 1 + 0x33c, 2.0);
        put_f32(&mut s, 1 + 0x340, 1.0);
        put_u32(&mut s, 1 + 0x290, 3);
        let _g = globals();
        unsafe { publish_from_scene(s.as_ptr().wrapping_add(1), 0x55) };
        assert_eq!(SCATTER_LEVEL.load(Relaxed), 3);
        assert_eq!(OBJECT.baseline().0, 2.0);
        unsafe { apply_to_scene(s.as_mut_ptr().wrapping_add(1), 0.5) };
        assert_eq!((get_f32(&s, 1 + 0x33c), get_f32(&s, 1 + 0x340)), (1.0, 0.5));
    }

    // ---- publish_from_scene and the detour's work -------------------------------------------

    #[test]
    fn publishes_scatter_level() {
        let _g = globals();
        let mut s = vec![0u8; 0x424];
        put_u32(&mut s, 0x290, 3);
        unsafe { publish_from_scene(s.as_ptr(), 9) };
        assert_eq!(SCATTER_LEVEL.load(Relaxed), 3);
    }

    #[test]
    fn publish_from_scene_publishes_the_baseline_and_the_time_and_reads_only() {
        let _g = globals();
        let mut s = vec![0u8; 0x424];
        put_f32(&mut s, 0x33c, 3.0);
        put_u32(&mut s, 0x290, 1);
        let before = s.clone();
        unsafe { publish_from_scene(s.as_ptr(), 0x1111) };
        assert_eq!(OBJECT.baseline().0, 3.0);
        assert_eq!(OBJECT.last_seen(), 0x1111);
        assert_eq!(SCATTER_LEVEL.load(Relaxed), 1);
        assert_eq!(s, before);
    }

    #[test]
    fn hook_publishes_even_when_object_knob_off() {
        let _g = globals();
        let mut s = vec![0u8; 0x424];
        put_f32(&mut s, 0x33c, 2.0);
        put_f32(&mut s, 0x340, 1.0);
        put_u32(&mut s, 0x290, 2);
        let now = 0x0bad_cafe_u64;
        unsafe { post_work(s.as_mut_ptr(), false, 0.52, now) };
        assert_eq!(
            (get_f32(&s, 0x33c), get_f32(&s, 0x340)),
            (2.0, 1.0),
            "the scene stays unscaled"
        );
        assert_eq!(OBJECT.last_seen(), now, "the world-rendering signal");
        assert_eq!(OBJECT.baseline().0, 2.0);
        assert_eq!(SCATTER_LEVEL.load(Relaxed), 2);
    }

    #[test]
    fn post_work_publishes_the_game_value_then_scales() {
        let _g = globals();
        let mut s = vec![0u8; 0x424];
        put_f32(&mut s, 0x33c, 2.0);
        put_f32(&mut s, 0x340, 1.0);
        put_u32(&mut s, 0x290, 3);
        unsafe { post_work(s.as_mut_ptr(), true, 0.72, 0x2222) };
        assert_eq!(
            OBJECT.baseline().0,
            2.0,
            "the baseline is what the game wrote, not the scaled value"
        );
        assert_eq!((get_f32(&s, 0x33c), get_f32(&s, 0x340)), (2.0 * 0.72, 0.72));
        assert_eq!(OBJECT.last_seen(), 0x2222);
        assert_eq!(SCATTER_LEVEL.load(Relaxed), 3);
    }

    #[test]
    fn post_work_ignores_a_null_scene() {
        let _g = globals();
        let seen = OBJECT.last_seen();
        unsafe { post_work(std::ptr::null_mut(), true, 0.5, seen.wrapping_add(1)) };
        assert_eq!(OBJECT.last_seen(), seen);
    }

    /// A scene as the game function may leave it: `main` at +0x33c, 1.0 at +0x340, `level` at
    /// +0x290.
    fn scene_with(main: f32, level: u32) -> Vec<u8> {
        let mut s = vec![0u8; 0x424];
        put_f32(&mut s, 0x33c, main);
        put_f32(&mut s, 0x340, 1.0);
        put_u32(&mut s, 0x290, level);
        s
    }

    #[test]
    fn a_value_the_game_never_writes_is_left_alone_and_not_published() {
        let _g = globals();
        // 0.72 is a value the hook itself could have written; 5.0, NaN and 0.0 are not models
        // values at all.
        for main in [0.72f32, 5.0, f32::NAN, 0.0] {
            // A known game baseline first, so publishing `main` would change the bits.
            OBJECT.publish(3.0, 1);
            let (_, seq) = OBJECT.baseline();
            let unknown = OBJECT_UNKNOWN_BASELINE.load(Relaxed);
            SCATTER_LEVEL.store(0, Relaxed);
            let mut s = scene_with(main, 3);
            let before = s.clone();
            unsafe { post_work(s.as_mut_ptr(), true, 0.52, 0x4444) };
            assert_eq!(
                (get_u32(&s, 0x33c), get_u32(&s, 0x340)),
                (main.to_bits(), 1.0f32.to_bits()),
                "{main}: both fields untouched"
            );
            assert!(s == before, "{main}: nothing else written either");
            assert_eq!(
                OBJECT.baseline(),
                (3.0, seq),
                "{main}: no baseline published"
            );
            assert_eq!(OBJECT.last_seen(), 0x4444, "{main}: the world is rendering");
            assert_eq!(SCATTER_LEVEL.load(Relaxed), 3, "{main}");
            assert_eq!(
                OBJECT_UNKNOWN_BASELINE.load(Relaxed),
                unknown.wrapping_add(1),
                "{main}: counted"
            );
        }
    }

    #[test]
    fn the_three_game_values_are_published_and_scaled() {
        let _g = globals();
        for main in [3.0f32, 2.0, 1.0] {
            let unknown = OBJECT_UNKNOWN_BASELINE.load(Relaxed);
            let mut s = scene_with(main, 2);
            unsafe { post_work(s.as_mut_ptr(), true, 0.72, 0x3333) };
            assert_eq!(OBJECT.baseline().0, main);
            assert_eq!(
                (get_f32(&s, 0x33c), get_f32(&s, 0x340)),
                (main * 0.72, 0.72),
                "{main}"
            );
            assert_eq!(OBJECT.last_seen(), 0x3333);
            assert_eq!(SCATTER_LEVEL.load(Relaxed), 2);
            assert_eq!(OBJECT_UNKNOWN_BASELINE.load(Relaxed), unknown, "{main}");
        }
    }

    #[test]
    fn a_scaled_field_is_never_taken_for_a_game_value() {
        // So a field the hook already scaled is never scaled again, whatever the caller does
        // with the struct between two calls.
        for game in [3.0f32, 2.0, 1.0] {
            for step in OBJECT_STEPS.into_iter().skip(1) {
                let scaled = game * step;
                assert!(!is_game_value(scaled), "{game} x {step} = {scaled}");
            }
        }
        assert!(is_game_value(3.0) && is_game_value(2.0) && is_game_value(1.0));
        assert!(!is_game_value(-1.0) && !is_game_value(f32::NAN) && !is_game_value(-0.0));
    }

    #[test]
    fn only_two_factors_in_the_clamp_range_map_a_game_value_onto_one() {
        // The claim in `post_work`'s docs, by an exhaustive sweep: every f32 factor that writes
        // (from 0.5 up to, but not including, 1.0; 1.0 writes nothing) times every game value.
        let mut hits = Vec::new();
        for bits in MIN_FACTOR.to_bits()..MAX_FACTOR.to_bits() {
            let factor = f32::from_bits(bits);
            for game in [3.0f32, 2.0, 1.0] {
                if is_game_value(game * factor) {
                    hits.push((factor, game, game * factor));
                }
            }
        }
        assert_eq!(hits, [(0.5, 2.0, 1.0), (2.0 / 3.0, 3.0, 2.0)]);
        assert_eq!(2.0f32 / 3.0, 0.6666667);
        for (factor, _, _) in hits {
            assert!(!OBJECT_STEPS.contains(&factor), "{factor} is a step");
        }
    }

    // ---- start-up decision and install --------------------------------------------------------

    fn knobs(object: bool, scatter: bool, terrain: bool) -> KnobsCfg {
        let mut k = Config::default().knobs;
        k.enable_object_distance = object;
        k.enable_scatter_distance = scatter;
        k.enable_terrain_distance = terrain;
        k
    }

    #[test]
    fn no_code_patches_means_no_hook_and_says_so() {
        let called = Cell::new(false);
        let (level, line) = start_up(&knobs(true, true, true), false, || {
            called.set(true);
            Ok(())
        });
        assert!(!called.get(), "install must not run");
        assert_eq!(level, Level::Info);
        assert_eq!(
            line,
            "object distance: off: code patches are off on this build"
        );
    }

    #[test]
    fn all_three_distance_knobs_off_means_no_hook() {
        let called = Cell::new(false);
        let (level, line) = start_up(&knobs(false, false, false), true, || {
            called.set(true);
            Ok(())
        });
        assert!(!called.get(), "install must not run");
        assert_eq!(level, Level::Info);
        assert_eq!(
            line,
            "object distance: off: EnableObjectDistance, EnableScatterDistance and \
             EnableTerrainDistance are all 0, so the scene-settings hook is not installed"
        );
    }

    #[test]
    fn any_one_distance_knob_installs_the_hook() {
        for (o, s, t) in [
            (true, false, false),
            (false, true, false),
            (false, false, true),
        ] {
            let called = Cell::new(false);
            let (level, _) = start_up(&knobs(o, s, t), true, || {
                called.set(true);
                Ok(())
            });
            assert!(called.get(), "object {o}, scatter {s}, terrain {t}");
            assert_eq!(level, Level::Info);
        }
    }

    #[test]
    fn the_line_says_whether_the_factor_is_applied() {
        let (_, on) = start_up(&knobs(true, false, false), true, || Ok(()));
        assert_eq!(on, "object distance: on (scene-settings hook queued)");
        let (_, off) = start_up(&knobs(false, true, true), true, || Ok(()));
        assert_eq!(
            off,
            "object distance: off (EnableObjectDistance = 0); the scene-settings hook is \
             queued for the other knobs and only publishes"
        );
    }

    #[test]
    fn an_install_failure_is_a_warning_with_the_reason() {
        let (level, line) = start_up(&knobs(true, true, true), true, || {
            Err("the game code was not found".to_owned())
        });
        assert_eq!(level, Level::Warn);
        assert_eq!(line, "object distance: off: the game code was not found");
    }

    #[test]
    fn install_in_this_exe_finds_nothing_and_says_why() {
        let why = install(&main_module()).unwrap_err();
        assert!(why.contains("not found"), "{why}");
    }

    // ---- the real hook, on a stand-in for the game function -----------------------------------

    /// The constant the fake's RIP-relative load reads: the "Balance" models value.
    static FAKE_MODELS: f32 = 2.0;
    /// How many times the fake has run.
    static FAKE_CALLS: AtomicU32 = AtomicU32::new(0);

    /// A stand-in for the game's scene-settings function: the same three register arguments, no
    /// return value, and the same first three instructions (the signature's first 17 bytes). The
    /// second one is a RIP-relative load, which MinHook must relocate into its trampoline.
    ///
    /// It writes, like the game: `+0x308 = extra[0]`, `+0x33c` = the models value (read through
    /// the RIP-relative load), `+0x340 = 1.0` and `+0x290 = settings[+0x3c]`; then it counts the
    /// call.
    #[unsafe(naked)]
    unsafe extern "system" fn fake_scene_settings(
        _scene: *mut u8,
        _settings: *const u8,
        _extra: *const u32,
    ) {
        core::arch::naked_asm!(
            "mov eax, dword ptr [r8]",
            "movss xmm1, dword ptr [rip + {models}]",
            "mov dword ptr [rcx + 0x308], eax",
            "movss dword ptr [rcx + 0x33c], xmm1",
            "mov dword ptr [rcx + 0x340], 0x3f800000",
            "mov eax, dword ptr [rdx + 0x3c]",
            "mov dword ptr [rcx + 0x290], eax",
            "lock inc dword ptr [rip + {calls}]",
            "ret",
            models = sym FAKE_MODELS,
            calls = sym FAKE_CALLS,
        );
    }

    /// Call the fake through its address, as the game's caller does.
    fn call_fake(scene: &mut [u8], settings: &[u8], extra: &[u32; 3]) {
        let f: SceneSettingsFn = std::hint::black_box(fake_scene_settings);
        // SAFETY: `scene` has room for every field the fake (or the hook) writes, below 0x424;
        // `settings` is readable at +0x3c and `extra` at [0].
        unsafe { f(scene.as_mut_ptr(), settings.as_ptr(), extra.as_ptr()) };
    }

    /// Render settings with the detail-scattering level at `+0x3c`.
    fn settings_with_level(level: u32) -> Vec<u8> {
        let mut s = vec![0u8; 0x60];
        put_u32(&mut s, 0x3c, level);
        s
    }

    #[test]
    fn the_fake_starts_like_the_game_function() {
        let head: Vec<&str> = SCENE_SETTINGS_FN
            .pattern
            .split_ascii_whitespace()
            .take(17)
            .collect();
        let pattern = Pattern::parse(&head.join(" ")).unwrap();
        let at = fake_scene_settings as *const () as *const u8;
        // SAFETY: the fake is a function of this binary, in readable `.text`, and longer than 17
        // bytes.
        let bytes = unsafe { std::slice::from_raw_parts(at, 17) };
        assert_eq!(find_unique(bytes, &pattern), Ok(0), "{bytes:02x?}");
    }

    /// Names the test that a child process runs. Unset in the parent.
    const CHILD_ENV: &str = "ENSHROUDED_HEADROOM_KNOBS_CHILD";

    /// Run `scenario` in a fresh copy of this test binary, alone. There no test has switched
    /// `Feature::ObjectDistance` off yet (`guard::tests` switches it off in this process), and no
    /// hook is written into a process where other tests run. `name` is the calling test's name.
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
    fn hdr_peak_readback_uncertainty_disables_runtime_without_touching_scene() {
        in_child(
            "hdr_peak_readback_uncertainty_disables_runtime_without_touching_scene",
            || {
                crate::features::hdr::test_fail_readback_after_write();
                assert_eq!(crate::features::hdr::effective_paper_white_nits(), 250.0);
                assert!(!crate::features::hdr::paper_white_is_verified());
                let line = crate::features::hdr_peak::test_configure(true, Some(800));
                assert_eq!(
                    line,
                    "hdr peak: off: paper-white patch read-back failed; effective value is unknown"
                );
                assert!(!crate::features::hdr_peak::enabled());
                let mut scene = vec![0xa5u8; 0x424];
                scene[0x408] = 1;
                scene[0x41e..0x420].copy_from_slice(&600u16.to_le_bytes());
                let expected = scene.clone();
                // SAFETY: the fresh child exclusively owns the complete readable/writable scene.
                unsafe { crate::features::hdr_peak::post_scene(scene.as_mut_ptr()) };
                assert_eq!(scene, expected);
            },
        );
    }

    #[test]
    fn hdr_peak_runtime_bridge_uses_checked_eighty_nits_and_fresh_game_peak() {
        in_child(
            "hdr_peak_runtime_bridge_uses_checked_eighty_nits_and_fresh_game_peak",
            || {
                crate::features::hdr::test_publish_checked_eighty();
                assert_eq!(crate::features::hdr::effective_paper_white_nits(), 80.0);
                crate::features::hdr_peak::test_configure(true, None);
                let target = fake_scene_settings as *const () as usize;
                APPLY_FACTOR.store(false, Relaxed);
                // SAFETY: target is our owned executable stand-in with the detour's type.
                unsafe { hook_at(target) }.unwrap();
                assert_eq!(apply_queued(), Ok(1));
                for (colour, game_peak, want) in
                    [(1, 800u16, 800u16), (2, 1200, 1200), (0, 110, 110)]
                {
                    let mut scene = vec![0xa5u8; 0x424];
                    scene[0x408] = colour;
                    scene[0x41e..0x420].copy_from_slice(&game_peak.to_le_bytes());
                    let mut expected = scene.clone();
                    put_u32(&mut expected, 0x308, 7);
                    put_f32(&mut expected, 0x33c, 2.0);
                    put_f32(&mut expected, 0x340, 1.0);
                    put_u32(&mut expected, 0x290, 3);
                    expected[0x41e..0x420].copy_from_slice(&want.to_le_bytes());
                    call_fake(&mut scene, &settings_with_level(3), &[7, 0, 0]);
                    assert_eq!(scene, expected, "colour={colour}, fresh peak={game_peak}");
                }
                assert_eq!(FAKE_CALLS.load(Relaxed), 3);
                // Override uses the same effective getter even if the game's target differs.
                crate::features::hdr_peak::test_configure(true, Some(800));
                let mut scene = vec![0u8; 0x424];
                scene[0x408] = 1;
                scene[0x41e..0x420].copy_from_slice(&600u16.to_le_bytes());
                call_fake(&mut scene, &settings_with_level(3), &[7, 0, 0]);
                assert_eq!(&scene[0x41e..0x420], &800u16.to_le_bytes());
                assert_eq!(FAKE_CALLS.load(Relaxed), 4);
                // SAFETY: the test owns this installed hook and no thread runs it now.
                unsafe { MinHook::remove_hook(target as *mut c_void) }.unwrap();
            },
        );
    }

    #[test]
    fn hdr_peak_panic_leaves_the_complete_original_scene_and_runs_original_once() {
        in_child(
            "hdr_peak_panic_leaves_the_complete_original_scene_and_runs_original_once",
            || {
                assert!(!is_disabled(Feature::ObjectDistance));
                crate::features::hdr_peak::test_configure(true, Some(800));
                let mut scene = vec![0xa5u8; 0x424];
                scene[0x408] = 1;
                scene[0x41e..0x420].copy_from_slice(&600u16.to_le_bytes());
                let mut expected = scene.clone();
                // Capture all the fake original's writes before installing its hook.
                call_fake(&mut expected, &settings_with_level(3), &[7, 0, 0]);
                FAKE_CALLS.store(0, Relaxed);
                let target = fake_scene_settings as *const () as usize;
                APPLY_FACTOR.store(true, Relaxed);
                OBJECT.set(0.5);
                // SAFETY: target is our owned executable stand-in with the detour's type.
                unsafe { hook_at(target) }.unwrap();
                assert_eq!(apply_queued(), Ok(1));
                crate::features::hdr_peak::test_panic(true);
                call_fake(&mut scene, &settings_with_level(3), &[7, 0, 0]);
                assert_eq!(FAKE_CALLS.load(Relaxed), 1);
                assert_eq!(
                    scene, expected,
                    "no peak or distance write escaped the panic"
                );
                assert!(is_disabled(Feature::ObjectDistance));
                assert_eq!(OBJECT.last_seen(), 0);
                crate::features::hdr_peak::test_panic(false);
                call_fake(&mut scene, &settings_with_level(3), &[7, 0, 0]);
                assert_eq!(FAKE_CALLS.load(Relaxed), 2);
                assert_eq!(scene, expected);
                // SAFETY: the test owns this installed hook and no thread runs it now.
                unsafe { MinHook::remove_hook(target as *mut c_void) }.unwrap();
            },
        );
    }

    #[test]
    fn real_hook_runs_the_original_once_then_publishes_and_scales() {
        in_child(
            "real_hook_runs_the_original_once_then_publishes_and_scales",
            || {
                assert!(!is_disabled(Feature::ObjectDistance), "a fresh process");
                let target = fake_scene_settings as *const () as usize;
                let settings = settings_with_level(3);
                let extra = [7u32, 0, 0];

                // Unhooked, the fake writes the game-like values.
                let mut scene = vec![0u8; 0x424];
                call_fake(&mut scene, &settings, &extra);
                assert_eq!(FAKE_CALLS.load(Relaxed), 1);
                assert_eq!((get_f32(&scene, 0x33c), get_f32(&scene, 0x340)), (2.0, 1.0));
                assert_eq!(OBJECT.last_seen(), 0, "no hook yet");

                // Hooked through the code path `install` uses, enabled through the one that
                // `entry::start` uses.
                APPLY_FACTOR.store(true, Relaxed);
                OBJECT.set(0.72);
                // SAFETY: `target` is the fake: a function in executable memory, longer than 5
                // bytes, that nothing jumps into; the detour has its type.
                unsafe { hook_at(target) }.unwrap();
                assert_eq!(apply_queued(), Ok(1));

                let before = qpc_now();
                let mut scene = vec![0u8; 0x424];
                call_fake(&mut scene, &settings, &extra);
                let after = qpc_now();
                assert_eq!(FAKE_CALLS.load(Relaxed), 2, "the original ran exactly once");
                assert_eq!(get_u32(&scene, 0x308), 7, "the original's own first write");
                assert_eq!(
                    OBJECT.baseline().0,
                    2.0,
                    "the relocated RIP-relative load read the right constant"
                );
                assert_eq!(get_f32(&scene, 0x33c), 2.0 * 0.72);
                assert_eq!(get_f32(&scene, 0x340), 0.72);
                assert_eq!(SCATTER_LEVEL.load(Relaxed), 3);
                assert!((before..=after).contains(&OBJECT.last_seen()));

                // Take the hook out: the fake runs alone again.
                // SAFETY: the hook at `target` was created above and nothing runs it now.
                unsafe { MinHook::remove_hook(target as *mut c_void) }.unwrap();
                let seen = OBJECT.last_seen();
                let mut scene = vec![0u8; 0x424];
                call_fake(&mut scene, &settings, &extra);
                assert_eq!(FAKE_CALLS.load(Relaxed), 3);
                assert_eq!(get_f32(&scene, 0x33c), 2.0, "unscaled");
                assert_eq!(OBJECT.last_seen(), seen, "the detour did not run");
            },
        );
    }

    #[test]
    fn the_original_runs_exactly_once_when_the_post_work_panics() {
        in_child(
            "the_original_runs_exactly_once_when_the_post_work_panics",
            || {
                assert!(!is_disabled(Feature::ObjectDistance), "a fresh process");
                let calls = Cell::new(0u32);
                let post_ran = Cell::new(false);
                run_after_original(
                    || calls.set(calls.get() + 1),
                    || {
                        post_ran.set(true);
                        panic!("post-work boom");
                    },
                );
                assert_eq!(
                    calls.get(),
                    1,
                    "the original ran once, not again in a fallback"
                );
                assert!(post_ran.get());
                assert!(
                    is_disabled(Feature::ObjectDistance),
                    "the panic switched the feature off"
                );

                // From now on only the original runs, still exactly once a call.
                post_ran.set(false);
                run_after_original(|| calls.set(calls.get() + 1), || post_ran.set(true));
                assert_eq!(calls.get(), 2);
                assert!(!post_ran.get());

                // The feature is off now, so on a hooked fake the detour takes only the outer
                // guard's fallback: the trampoline runs once and nothing is published. This checks
                // that fallback, not the detour's own panic path; with the feature on, that path
                // is `the_real_detour_runs_the_original_once_when_its_post_work_panics`.
                let target = fake_scene_settings as *const () as usize;
                APPLY_FACTOR.store(true, Relaxed);
                OBJECT.set(0.5);
                // SAFETY: as in the real-hook test.
                unsafe { hook_at(target) }.unwrap();
                assert_eq!(apply_queued(), Ok(1));
                let mut scene = vec![0u8; 0x424];
                call_fake(&mut scene, &settings_with_level(3), &[1, 0, 0]);
                assert_eq!(FAKE_CALLS.load(Relaxed), 1);
                assert_eq!(get_f32(&scene, 0x33c), 2.0, "unscaled");
                assert_eq!(OBJECT.last_seen(), 0, "nothing published");
                // SAFETY: as in the real-hook test.
                unsafe { MinHook::remove_hook(target as *mut c_void) }.unwrap();
            },
        );
    }

    #[test]
    fn the_real_detour_runs_the_original_once_when_its_post_work_panics() {
        in_child(
            "the_real_detour_runs_the_original_once_when_its_post_work_panics",
            || {
                assert!(!is_disabled(Feature::ObjectDistance), "a fresh process");
                let target = fake_scene_settings as *const () as usize;
                APPLY_FACTOR.store(true, Relaxed);
                OBJECT.set(0.5);
                // SAFETY: as in the real-hook test.
                unsafe { hook_at(target) }.unwrap();
                assert_eq!(apply_queued(), Ok(1));

                // The feature is on, so the real detour runs its post-work, which panics.
                PANIC_IN_POST.store(true, Relaxed);
                let mut scene = vec![0u8; 0x424];
                call_fake(&mut scene, &settings_with_level(3), &[1, 0, 0]);
                assert_eq!(FAKE_CALLS.load(Relaxed), 1, "the original ran exactly once");
                assert!(
                    is_disabled(Feature::ObjectDistance),
                    "the panic switched the feature off"
                );
                assert_eq!(get_f32(&scene, 0x33c), 2.0, "unscaled");
                assert_eq!(OBJECT.last_seen(), 0, "nothing published");
                assert_eq!(OBJECT.baseline(), (0.0, 0), "no baseline");
                assert_eq!(SCATTER_LEVEL.load(Relaxed), 0, "no scatter level");
                assert_eq!(OBJECT_UNKNOWN_BASELINE.load(Relaxed), 0);

                // From now on the detour only runs the original, once a call.
                PANIC_IN_POST.store(false, Relaxed);
                call_fake(&mut scene, &settings_with_level(3), &[1, 0, 0]);
                assert_eq!(FAKE_CALLS.load(Relaxed), 2);
                assert_eq!(OBJECT.last_seen(), 0, "still nothing published");
                // SAFETY: as in the real-hook test.
                unsafe { MinHook::remove_hook(target as *mut c_void) }.unwrap();
            },
        );
    }
    /// The native detour publishes the guarded saved value and clears it on malformed inputs.
    #[test]
    fn native_post_hook_publishes_guarded_reflex_menu() {
        in_child("native_post_hook_publishes_guarded_reflex_menu", || {
            let target = fake_scene_settings as *const () as usize;
            // SAFETY: this isolated process owns the documented fake function and hook.
            unsafe { hook_at(target) }.unwrap();
            assert_eq!(apply_queued(), Ok(1));
            let mut scene = vec![0u8; 0x420];
            let mut options = [0u64; 32];
            let base = options.as_mut_ptr().cast::<u8>();
            // SAFETY: the aligned fake options allocation contains all accesses made by the native fake
            // and by the post-hook, including the negative settings offsets and preferences.
            unsafe {
                let render = base.add(16);
                let prefs = render.add(0x54).cast::<u32>();
                render.sub(8).cast::<u32>().write(2);
                render.sub(11).write(1);
                let f: SceneSettingsFn = std::hint::black_box(fake_scene_settings);
                f(scene.as_mut_ptr(), render, prefs);
                assert_eq!(crate::reflex::game_menu(), Some(2));
                assert_eq!(FAKE_CALLS.load(Relaxed), 1);
                render.sub(8).cast::<u32>().write(9);
                f(scene.as_mut_ptr(), render, prefs);
                assert_eq!(crate::reflex::game_menu(), None);
                render.sub(8).cast::<u32>().write(1);
                f(scene.as_mut_ptr(), render, prefs.add(1));
                assert_eq!(crate::reflex::game_menu(), None);
                assert_eq!(FAKE_CALLS.load(Relaxed), 3);
                MinHook::remove_hook(target as *mut c_void).unwrap();
            }
        });
    }
}
