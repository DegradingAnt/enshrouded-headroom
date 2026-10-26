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
    let mut write = |site, original, new| {
        // SAFETY: apply's caller owns these verified immediate sites during startup.
        unsafe { crate::patch::write_u32_checked(site, original, new) }.map(|_| ())
    };
    // SAFETY: forwarded from apply's startup-only owned-image contract.
    unsafe { apply_with_writer(module, cfg, &mut write) }
}

/// Apply and report scales through the checked-write boundary.
///
/// # Safety
///
/// Same image and startup contract as [`apply`]. The writer must obey the checked-write contract.
unsafe fn apply_with_writer(
    module: &crate::scan::Module,
    cfg: &crate::config::KnobsCfg,
    write: &mut impl FnMut(usize, u32, u32) -> Result<(), crate::patch::PatchError>,
) -> Vec<String> {
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
        match unsafe { patch_group(module, &sig, originals, scale as f32, write) } {
            Ok(()) => format!("{label}: scale {scale} applied"),
            Err(failure) if failure.confirmed == 0 && !failure.current_may_have_changed => {
                format!("{label}: skipped: {}", failure.why)
            }
            Err(failure) => {
                let current_write = if failure.current_may_have_changed {
                    "; current write may have changed memory"
                } else {
                    ""
                };
                format!(
                    "{label}: incomplete: {}/{} writes confirmed{current_write}: {}",
                    failure.confirmed,
                    originals.len(),
                    failure.why
                )
            }
        }
    })
    .collect()
}

/// A stopped group retains the writes already confirmed by the checked writer.
struct GroupFailure {
    /// Successful writes before the failure.
    confirmed: usize,
    /// Readback failed after the current write, so its final bytes are uncertain.
    current_may_have_changed: bool,
    /// The original scan, address or checked-write error.
    why: String,
}

impl GroupFailure {
    /// Record a scan failure before any immediate was written.
    fn before_write(why: impl ToString) -> Self {
        Self {
            confirmed: 0,
            current_may_have_changed: false,
            why: why.to_string(),
        }
    }
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
    write: &mut impl FnMut(usize, u32, u32) -> Result<(), crate::patch::PatchError>,
) -> Result<(), GroupFailure> {
    let pattern = crate::scan::Pattern::parse(sig.pattern).map_err(GroupFailure::before_write)?;
    let found = crate::scan::find_in_sections(module, sig.sections, &pattern)
        .map_err(|e| GroupFailure::before_write(e.player_text()))?;
    for (confirmed, (&offset, &original)) in sig.patch_offsets.iter().zip(originals).enumerate() {
        let site = found.checked_add(offset).ok_or_else(|| GroupFailure {
            confirmed,
            current_may_have_changed: false,
            why: "patch address overflows".to_owned(),
        })?;
        write(site, original.to_bits(), scaled(original, scale).to_bits()).map_err(|e| {
            GroupFailure {
                confirmed,
                current_may_have_changed: matches!(&e, crate::patch::PatchError::ReadBack),
                why: e.to_string(),
            }
        })?;
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
    fn failed_second_startup_write_reports_the_first_write_and_preserves_the_remainder() {
        let (mem, module) = image();
        let bytes: Vec<u8> = crate::sigs::VOXEL_HIGH_IMMS
            .pattern
            .split_whitespace()
            .map(|x| u8::from_str_radix(x, 16).unwrap_or(0))
            .collect();
        mem.put(0x1300, &bytes);
        let mut expected = mem.get(0, 0x4000);
        expected[0x130f..0x1313].copy_from_slice(&48.0f32.to_le_bytes());
        let mut cfg = crate::config::Config::default().knobs;
        cfg.terrain_ring_scale = 1.5;
        let mut write = |site, original, new| {
            if site == mem.base + 0x1316 {
                return Err(crate::patch::PatchError::Protect(5));
            }
            // SAFETY: this test owns all mapped image pages; no code executes them.
            unsafe { crate::patch::write_u32_checked(site, original, new) }.map(|_| ())
        };
        // SAFETY: owned mapped image and a checked writer, with one simulated pre-write failure.
        let notes = unsafe { apply_with_writer(&module, &cfg, &mut write) };
        assert_eq!(mem.get(0, 0x4000), expected);
        assert_eq!(
            notes[2],
            "terrain rings: incomplete: 1/3 writes confirmed: VirtualProtect failed (error 5)"
        );
    }

    #[test]
    fn unconfirmed_startup_write_reports_possible_changes_and_preserves_the_remainder() {
        for (failed_site, confirmed) in [(0x130f, 0), (0x1316, 1)] {
            let (mem, module) = image();
            let bytes: Vec<u8> = crate::sigs::VOXEL_HIGH_IMMS
                .pattern
                .split_whitespace()
                .map(|x| u8::from_str_radix(x, 16).unwrap_or(0))
                .collect();
            mem.put(0x1300, &bytes);
            let mut expected = mem.get(0, 0x4000);
            expected[0x130f..0x1313].copy_from_slice(&48.0f32.to_le_bytes());
            if failed_site == 0x1316 {
                expected[0x1316..0x131a].copy_from_slice(&192.0f32.to_le_bytes());
            }
            let mut cfg = crate::config::Config::default().knobs;
            cfg.terrain_ring_scale = 1.5;
            let mut write = |site, original, new| {
                // SAFETY: this test owns all mapped image pages; no code executes them.
                unsafe { crate::patch::write_u32_checked(site, original, new) }?;
                if site == mem.base + failed_site {
                    return Err(crate::patch::PatchError::ReadBack);
                }
                Ok(())
            };
            // SAFETY: owned mapped image; the writer simulates an unconfirmed result after writing.
            let notes = unsafe { apply_with_writer(&module, &cfg, &mut write) };
            assert_eq!(mem.get(0, 0x4000), expected);
            assert_eq!(
                notes[2],
                format!(
                    "terrain rings: incomplete: {confirmed}/3 writes confirmed; current write may have changed memory: read-back after the write did not match"
                )
            );
        }
    }

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
