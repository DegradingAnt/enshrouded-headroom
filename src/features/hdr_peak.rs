//! HDR tonemapper peak correction after the game rebuilds its scene settings.

use std::sync::atomic::{AtomicU32, Ordering};

use crate::config::{HdrCfg, KnobsCfg};
use crate::{entry, features::hdr};

/// One publication for the enable bit and optional target (zero means game).
static SETTINGS: AtomicU32 = AtomicU32::new(0);
/// Bit outside the u16 target range, so enable and target share one snapshot.
const ON: u32 = 1 << 16;

/// Whether peak correction was enabled at start-up.
pub fn enabled() -> bool {
    SETTINGS.load(Ordering::Acquire) & ON != 0
}

/// Configure before queued hooks are enabled and return the single start-up line.
fn start_up(cfg: &HdrCfg, code_patches: bool, scene_hook: bool, settings: &AtomicU32) -> String {
    settings.store(0, Ordering::Release);
    if !cfg.fix_peak {
        return "hdr peak: off (setting)".to_owned();
    }
    if !code_patches {
        return "hdr peak: off: code patches are off on this build".to_owned();
    }
    if !hdr::paper_white_is_verified() {
        return "hdr peak: off: paper-white patch read-back failed; effective value is unknown"
            .to_owned();
    }
    if !scene_hook {
        return "hdr peak: off: all distance knobs are off; scene-settings hook is not installed"
            .to_owned();
    }
    settings.store(
        ON | u32::from(cfg.display_peak_nits.unwrap_or(0)),
        Ordering::Release,
    );
    let target = cfg
        .display_peak_nits
        .map_or_else(|| "game".to_owned(), |nits| nits.to_string());
    format!(
        "hdr peak: on (target {target} nits, paper white {})",
        hdr::effective_paper_white_nits()
    )
}

/// Run after HDR paper white and before queued hook enable. The shared hook needs a distance knob.
pub fn init(cfg: &HdrCfg, knobs: &KnobsCfg) {
    let scene_hook = knobs.enable_object_distance
        || knobs.enable_scatter_distance
        || knobs.enable_terrain_distance;
    let line = start_up(cfg, entry::code_patches_allowed(), scene_hook, &SETTINGS);
    log::info!("{line}");
}

/// Apply the published settings using W13's successfully patched paper white.
/// No allocation, lock or log occurs on this path.
///
/// # Safety
/// `scene` is null or owns at least `0x420` readable, writable bytes with no concurrent access.
/// The game has just rebuilt it; pass its fresh peak on each call.
pub unsafe fn post_scene(scene: *mut u8) {
    let settings = SETTINGS.load(Ordering::Acquire);
    if settings & ON == 0 {
        return;
    }
    #[cfg(test)]
    if PANIC.load(Ordering::Relaxed) {
        panic!("HDR peak test panic before any scene write");
    }
    let target = settings as u16;
    // SAFETY: the caller owns the complete scene; correct_scene reads only documented fields.
    unsafe {
        correct_scene(
            scene,
            true,
            (target != 0).then_some(target),
            hdr::effective_paper_white_nits(),
        )
    };
}

/// Test-only failure injection at the beginning of the new hook work.
#[cfg(test)]
static PANIC: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Configure the real atomic state in a fresh test child before enabling its fake hook.
#[cfg(test)]
pub(crate) fn test_configure(on: bool, target: Option<u16>) -> String {
    let mut cfg = crate::config::Config::default().hdr;
    cfg.fix_peak = on;
    cfg.display_peak_nits = target;
    start_up(&cfg, true, true, &SETTINGS)
}

/// Set the new work's panic injection in an isolated child.
#[cfg(test)]
pub(crate) fn test_panic(on: bool) {
    PANIC.store(on, Ordering::Relaxed);
}

/// Convert the panel target to the game's 80-nit tonemapper units.
/// Invalid paper white skips the write; a valid result never reaches zero.
pub fn gt_peak_field(target_nits: u16, paper_white: f32) -> Option<u16> {
    if !paper_white.is_finite() || paper_white <= 0.0 {
        return None;
    }
    Some(
        (f32::from(target_nits) * 80.0 / paper_white)
            .round()
            .clamp(1.0, 10000.0) as u16,
    )
}

/// The verified scene colour-space byte (`docs/re-notes.md`, route B2).
const COLOUR_SPACE: usize = 0x408;
/// The verified scene peak u16, in nits; it need not be aligned.
const PEAK: usize = 0x41e;

/// Correct one freshly rebuilt HDR scene; leave SDR and disabled scenes untouched.
///
/// # Safety
/// `scene` is null or owns at least `0x420` readable, writable bytes with no concurrent access.
unsafe fn correct_scene(scene: *mut u8, on: bool, target: Option<u16>, paper_white: f32) {
    if !on || scene.is_null() {
        return;
    }
    // SAFETY: COLOUR_SPACE is inside the caller's owned scene; u8 needs no alignment.
    let colour = unsafe { scene.wrapping_add(COLOUR_SPACE).read() };
    if !matches!(colour, 1 | 2) {
        return;
    }
    let field = scene.wrapping_add(PEAK).cast::<u16>();
    // SAFETY: PEAK..PEAK+2 is inside the caller's owned scene; alignment is not required.
    let game_peak = unsafe { field.read_unaligned() };
    if let Some(value) = gt_peak_field(target.unwrap_or(game_peak), paper_white) {
        // SAFETY: the same owned field; no other thread accesses it and alignment is not required.
        unsafe { field.write_unaligned(value) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn startup_gates_publish_one_complete_setting_and_explain_off() {
        let mut cfg = crate::config::Config::default().hdr;
        for (fix, allowed, hook, reason) in [
            (false, true, true, "off (setting)"),
            (true, false, true, "off: code patches are off"),
            (true, true, false, "off: all distance knobs are off"),
        ] {
            cfg.fix_peak = fix;
            let settings = AtomicU32::new(u32::MAX);
            let line = start_up(&cfg, allowed, hook, &settings);
            assert!(line.starts_with("hdr peak: "));
            assert!(line.contains(reason), "{line}");
            assert!(!line.contains('\n'));
            assert_eq!(settings.load(Ordering::Acquire), 0);
        }
        cfg.fix_peak = true;
        for (target, text) in [(None, "game"), (Some(800), "800")] {
            cfg.display_peak_nits = target;
            let settings = AtomicU32::new(0);
            let line = start_up(&cfg, true, true, &settings);
            assert_eq!(
                line,
                format!("hdr peak: on (target {text} nits, paper white 250)")
            );
            assert_eq!(
                settings.load(Ordering::Acquire),
                ON | u32::from(target.unwrap_or(0))
            );
        }
    }

    #[test]
    fn fresh_game_peak_and_override_are_read_per_call_and_invalid_white_writes_nothing() {
        let mut scene = vec![0xa5u8; 0x424];
        scene[0x408] = 1;
        for (game, target, white, want) in [
            (800u16, None, 250.0, 256u16),
            (1200, None, 250.0, 384),
            (600, Some(800), 250.0, 256),
            (600, Some(800), 80.0, 800),
            (600, None, f32::NAN, 600),
        ] {
            scene[0x41e..0x420].copy_from_slice(&game.to_le_bytes());
            let mut expected = scene.clone();
            expected[0x41e..0x420].copy_from_slice(&want.to_le_bytes());
            // SAFETY: this test exclusively owns the complete scene for each call.
            unsafe { correct_scene(scene.as_mut_ptr(), true, target, white) };
            assert_eq!(scene, expected);
        }
        // SAFETY: null is explicitly supported and no field can be read.
        unsafe { correct_scene(std::ptr::null_mut(), true, None, 250.0) };
    }

    /// An odd base also exercises the unaligned u16 store.
    #[test]
    fn scene_correction_preserves_sdr_and_every_other_byte() {
        for colour in [0, 1, 2, 3, 255] {
            for on in [false, true] {
                let mut storage = vec![0xa5u8; 0x425];
                let scene = &mut storage[1..];
                scene[0x408] = colour;
                scene[0x41e..0x420].copy_from_slice(&800u16.to_le_bytes());
                let mut expected = scene.to_vec();
                if on && matches!(colour, 1 | 2) {
                    expected[0x41e..0x420].copy_from_slice(&256u16.to_le_bytes());
                }
                // SAFETY: owned scene has every documented field and no concurrent user.
                unsafe { correct_scene(scene.as_mut_ptr(), on, None, 250.0) };
                assert_eq!(scene, expected, "colour={colour}, on={on}");
                assert_eq!(storage[0], 0xa5);
            }
        }
    }

    #[test]
    fn peak_field_uses_effective_paper_white_and_safe_bounds() {
        for (target, white, want) in [
            (800, 250.0, Some(256)),
            (800, 200.0, Some(320)),
            (800, 80.0, Some(800)),
            (801, 250.0, Some(256)),
            (802, 250.0, Some(257)),
            (0, 250.0, Some(1)),
            (1, f32::MAX, Some(1)),
            (u16::MAX, f32::MIN_POSITIVE, Some(10000)),
            (800, f32::NAN, None),
            (800, f32::INFINITY, None),
            (800, f32::NEG_INFINITY, None),
            (800, 0.0, None),
            (800, -1.0, None),
        ] {
            assert_eq!(gt_peak_field(target, white), want, "{target}/{white}");
        }
    }
}
