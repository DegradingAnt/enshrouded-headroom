//! Checked start-up-only scales for the verified High-quality immediates.

/// Scale one verified original value.
pub fn scaled(original: f32, scale: f32) -> f32 {
    original * scale
}

/// Apply the configured High-quality startup scales.
///
/// # Safety
///
/// For any non-unit scale, `module` is a readable mapped PE image. Call once before its code executes, with no
/// concurrent image writer. Unit scales never inspect the image.
pub unsafe fn apply(module: &crate::scan::Module, cfg: &crate::config::KnobsCfg) -> Vec<String> {
    use crate::sigs::{DECO_HIGH_IMM, DISTANT_HIGH_IMM, VOXEL_HIGH_IMMS};
    [
        (
            "deco distance",
            DECO_HIGH_IMM,
            cfg.deco_distance_scale,
            0.5..=1.0,
            &[1.5f32][..],
        ),
        (
            "distant objects",
            DISTANT_HIGH_IMM,
            cfg.distant_objects_scale,
            0.5..=1.0,
            &[2.5f32][..],
        ),
        (
            "terrain rings",
            VOXEL_HIGH_IMMS,
            cfg.terrain_ring_scale,
            1.0..=1.6,
            &[32.0f32, 128.0, 160.0][..],
        ),
    ]
    .into_iter()
    .map(|(label, sig, scale, range, originals)| {
        if scale == 1.0 {
            return format!("{label}: unchanged");
        }
        if !scale.is_finite() || !range.contains(&scale) {
            return format!("{label}: skipped: scale is outside its configured range");
        }
        // SAFETY: forwarded from apply's startup-only owned-image contract.
        match unsafe { patch_group(module, &sig, originals, scale as f32) } {
            Ok(()) => format!("{label}: scale {scale} applied"),
            Err(why) => format!("{label}: skipped: {why}"),
        }
    })
    .collect()
}

/// Scan one verified pattern and check each original immediate before its startup write.
///
/// # Safety
///
/// Same readable-image, startup-only and exclusive-writer contract as [`apply`].
unsafe fn patch_group(
    module: &crate::scan::Module,
    sig: &crate::sigs::Sig,
    originals: &[f32],
    scale: f32,
) -> Result<(), String> {
    let pattern = crate::scan::Pattern::parse(sig.pattern).map_err(|e| e.to_string())?;
    let found = crate::scan::find_in_sections(module, sig.sections, &pattern)
        .map_err(|e| e.player_text())?;
    for (&offset, &original) in sig.patch_offsets.iter().zip(originals) {
        let site = found
            .checked_add(offset)
            .ok_or_else(|| "patch address overflows".to_owned())?;
        // SAFETY: the complete verified signature covers this four-byte immediate, and the
        // caller guarantees that no game thread executes or modifies it during startup.
        unsafe {
            crate::patch::write_u32_checked(
                site,
                original.to_bits(),
                scaled(original, scale).to_bits(),
            )
        }
        .map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Apply startup settings behind the effective build gate and feature panic guard.
pub fn init(cfg: &crate::config::KnobsCfg) {
    crate::guard::guard(
        crate::guard::Feature::StartupScales,
        || (),
        || {
            if !crate::entry::code_patches_allowed() {
                log::info!("startup scales: skipped: code patches are disabled for this build");
                return;
            }
            // SAFETY: entry calls once before the game's main function; main_module describes its
            // live mapped image, and no other mod write is concurrent within this startup step.
            for line in unsafe { apply(&crate::scan::main_module(), cfg) } {
                log::info!("startup scales: {line}");
            }
        },
    );
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refused_startup_scales_leave_the_entire_owned_image_unchanged() {
        for scenario in ["missing", "duplicate", "changed", "invalid"] {
            let (mem, module) = image();
            let bytes: Vec<u8> = crate::sigs::DECO_HIGH_IMM
                .pattern
                .split_whitespace()
                .map(|x| u8::from_str_radix(x, 16).unwrap_or(0))
                .collect();
            if scenario != "missing" {
                mem.put(0x1100, &bytes);
            }
            if scenario == "duplicate" {
                mem.put(0x1200, &bytes);
            }
            if scenario == "changed" {
                mem.put(0x1109, &2.0f32.to_le_bytes());
            }
            let before = mem.get(0, 0x4000);
            let mut cfg = crate::config::Config::default().knobs;
            cfg.deco_distance_scale = if scenario == "invalid" { f64::NAN } else { 0.5 };
            // SAFETY: this test owns the complete mapped image and no thread executes it.
            let notes = unsafe { apply(&module, &cfg) };
            assert!(notes[0].contains("skipped"), "{scenario}: {notes:?}");
            assert_eq!(mem.get(0, 0x4000), before, "{scenario}");
        }
    }
    use crate::patch::test_support::Mem;
    use crate::scan::Module;

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

    /// An image of 4 read-write pages with `.text` at `0x1000`, and the module that describes it.
    fn image() -> (Mem, Module) {
        let mem = Mem::rw(0x4000);
        mem.put(0, &text_only_header());
        let module = Module {
            base: mem.base,
            size: 0x4000,
        };
        (mem, module)
    }

    #[test]
    fn configured_scales_patch_only_the_five_verified_immediates() {
        let (mem, module) = image();
        for (offset, sig) in [
            (0x1100, crate::sigs::DECO_HIGH_IMM),
            (0x1200, crate::sigs::DISTANT_HIGH_IMM),
            (0x1300, crate::sigs::VOXEL_HIGH_IMMS),
        ] {
            let bytes: Vec<u8> = sig
                .pattern
                .split_whitespace()
                .map(|x| u8::from_str_radix(x, 16).unwrap_or(0))
                .collect();
            mem.put(offset, &bytes);
        }
        let mut cfg = crate::config::Config::default().knobs;
        cfg.deco_distance_scale = 0.5;
        cfg.distant_objects_scale = 0.8;
        cfg.terrain_ring_scale = 1.5;
        let mut expected = mem.get(0, 0x4000);
        for (offset, value) in [
            (0x1109, 0.75f32),
            (0x1209, 2.0),
            (0x130f, 48.0),
            (0x1316, 192.0),
            (0x131d, 240.0),
        ] {
            expected[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
        }
        // SAFETY: owned mapped synthetic image, not executed and no other writer.
        let notes = unsafe { apply(&module, &cfg) };
        assert!(
            notes.iter().all(|line| line.contains("applied")),
            "{notes:?}"
        );
        assert_eq!(mem.get(0, 0x4000), expected);
    }

    #[test]
    fn default_scales_do_not_read_or_write_the_module() {
        let cfg = crate::config::Config::default().knobs;
        let module = crate::scan::Module { base: 0, size: 0 };
        // SAFETY: all scales are one, so apply must never inspect this deliberately absent image.
        let notes = unsafe { apply(&module, &cfg) };
        assert_eq!(
            notes,
            [
                "deco distance: unchanged",
                "distant objects: unchanged",
                "terrain rings: unchanged"
            ]
        );
    }

    #[test]
    fn scaled_multiplies_the_original_value() {
        assert_eq!(scaled(1.5, 0.5), 0.75);
    }
}
