//! Optional Ultra shadow constants and their extra texture memory.

use crate::config::{ShadowValue, ShadowsCfg};

use crate::patch::write_u32_checked;
use crate::scan::{find_in_sections, Module, Pattern};
use crate::sigs::SUN_SHADOW_ULTRA;

/// Replace the Ultra sun resolution after a unique `.text` match.
///
/// # Safety
///
/// Start-up only, before game threads execute or use the image. See
/// [`crate::patch::write_checked`] and [`find_in_sections`].
pub unsafe fn apply_sun(module: &Module, resolution: u32) -> Result<(), String> {
    if resolution != 4096 {
        return Err("unsupported sun resolution".to_owned());
    }
    let sig = &SUN_SHADOW_ULTRA;
    let pattern = Pattern::parse(sig.pattern).map_err(|e| e.to_string())?;
    let found = find_in_sections(module, sig.sections, &pattern).map_err(|e| e.player_text())?;
    let site = found
        .checked_add(sig.patch_offset())
        .ok_or("sun address overflow")?;
    // SAFETY: the caller guarantees start-up with no game threads. The unique full signature
    // proves this four-byte immediate is inside `.text`; the write checks old bytes again.
    unsafe { write_u32_checked(site, 2048, resolution) }
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// Additional texture bytes requested above Ultra, using integer texture dimensions.
///
/// The RGBA16F point-light atlas has 32 by 32 tiles at 8 bytes per texel. Ultra uses
/// 192-pixel tiles. The six D32 sun cascades use 4 bytes per texel, at 2048 pixels in Ultra.
/// This estimate omits the two optional small sun textures described in the RE notes.
/// It reserves requested memory even if patching later refuses; it is not measured use.
pub fn extra_vram_bytes(cfg: &ShadowsCfg) -> u64 {
    let tile = match cfg.point_light_tile {
        ShadowValue::Override(224) => 224_u64,
        ShadowValue::Override(256) => 256_u64,
        _ => 192_u64,
    };
    let atlas = tile
        .saturating_mul(tile)
        .saturating_sub(192 * 192)
        .saturating_mul(32 * 32 * 8);
    let sun = if cfg.sun_resolution == ShadowValue::Override(4096) {
        (4096_u64 * 4096 - 2048 * 2048) * 6 * 4
    } else {
        0
    };
    atlas.saturating_add(sun)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{ShadowValue, ShadowsCfg};

    use crate::patch::test_support::Mem;
    use crate::scan::Module;

    /// A mapped PE with independent literal instruction bytes in `.text`.
    fn image() -> Mem {
        let mem = Mem::rw(0x4000);
        let mut bytes = vec![0; 0x4000];
        bytes[0..2].copy_from_slice(b"MZ");
        bytes[0x3c..0x40].copy_from_slice(&0x40u32.to_le_bytes());
        bytes[0x40..0x44].copy_from_slice(b"PE\0\0");
        bytes[0x46..0x48].copy_from_slice(&2u16.to_le_bytes());
        bytes[0x54..0x56].copy_from_slice(&0xf0u16.to_le_bytes());
        bytes[0x58..0x5a].copy_from_slice(&0x20bu16.to_le_bytes());
        bytes[0x90..0x94].copy_from_slice(&0x4000u32.to_le_bytes());
        for (at, name, rva) in [
            (0x148, &b".text"[..], 0x1000u32),
            (0x170, &b".rdata"[..], 0x2000u32),
        ] {
            bytes[at..at + name.len()].copy_from_slice(name);
            bytes[at + 8..at + 12].copy_from_slice(&0x1000u32.to_le_bytes());
            bytes[at + 12..at + 16].copy_from_slice(&rva.to_le_bytes());
        }
        bytes[0x1100..0x111c].copy_from_slice(&[
            0xc7, 0x41, 0x5c, 6, 0, 0, 0, 0xc7, 0x41, 0x78, 0, 8, 0, 0, 0xc7, 0x41, 0x2c, 4, 0, 0,
            0, 0xc7, 0x41, 0x30, 12, 0, 0, 0,
        ]);
        mem.put(0, &bytes);
        mem
    }

    #[test]
    fn sun_changes_only_the_ultra_immediate_in_the_whole_image() {
        let mem = image();
        let mut expected = mem.get(0, 0x4000);
        expected[0x110a..0x110e].copy_from_slice(&4096u32.to_le_bytes());
        let module = Module {
            base: mem.base,
            size: 0x4000,
        };
        // SAFETY: the image is owned by this test and no code is executing from it.
        unsafe { apply_sun(&module, 4096) }.unwrap();
        assert_eq!(mem.get(0, 0x4000), expected);
    }

    #[test]
    fn sun_refuses_duplicate_missing_and_changed_instruction_bytes() {
        for scenario in 0..3 {
            let mem = image();
            match scenario {
                0 => mem.put(0x1200, &mem.get(0x1100, 28)),
                1 => mem.put(0x1100, &[0]),
                _ => mem.put(0x110b, &[16]),
            }
            let expected = mem.get(0, 0x4000);
            let module = Module {
                base: mem.base,
                size: 0x4000,
            };
            // SAFETY: this test owns the entire mapped image and it is not executing.
            assert!(unsafe { apply_sun(&module, 4096) }.is_err());
            assert_eq!(mem.get(0, 0x4000), expected);
        }
    }

    #[test]
    fn vram_combinations_match_the_verified_texture_layouts() {
        for (tile, atlas) in [
            (ShadowValue::Game, 0),
            (ShadowValue::Override(224), 109_051_904),
            (ShadowValue::Override(256), 234_881_024),
        ] {
            for (sun, cascades) in [
                (ShadowValue::Game, 0),
                (ShadowValue::Override(4096), 301_989_888),
            ] {
                let cfg = ShadowsCfg {
                    point_light_tile: tile,
                    sun_resolution: sun,
                    ..ShadowsCfg::default()
                };
                assert_eq!(extra_vram_bytes(&cfg), atlas + cascades);
                assert_eq!(
                    extra_vram_bytes(&ShadowsCfg {
                        point_light_rays: ShadowValue::Override(256),
                        point_light_static_distance: ShadowValue::Override(400.0),
                        ..cfg
                    }),
                    atlas + cascades
                );
            }
        }
    }
}
