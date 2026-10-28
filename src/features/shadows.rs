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

/// Exact 220-byte vanilla table, statically verified on the known Steam client.
const VANILLA: [u8; 220] = [
    0x10, 0x0, 0x0, 0x0, 0x8, 0x0, 0x0, 0x0, 0x60, 0x0, 0x0, 0x0, 0x9a, 0x99, 0x99, 0x3e, 0x96,
    0x43, 0xb, 0x3c, 0x33, 0x33, 0xd3, 0x3f, 0xcd, 0xcc, 0x4c, 0x3e, 0xec, 0x51, 0x38, 0x3d, 0x10,
    0x0, 0x0, 0x0, 0x0, 0x0, 0xa0, 0x41, 0x0, 0x0, 0x0, 0x0, 0x18, 0x0, 0x0, 0x0, 0x10, 0x0, 0x0,
    0x0, 0x80, 0x0, 0x0, 0x0, 0x0, 0x0, 0x80, 0x3e, 0xe0, 0x2d, 0x10, 0x3a, 0x0, 0x0, 0x0, 0x40,
    0xcd, 0xcc, 0x4c, 0x3e, 0xec, 0x51, 0x38, 0x3d, 0x18, 0x0, 0x0, 0x0, 0x0, 0x0, 0xf0, 0x41, 0x0,
    0x0, 0x0, 0x0, 0x20, 0x0, 0x0, 0x0, 0x10, 0x0, 0x0, 0x0, 0xa0, 0x0, 0x0, 0x0, 0x0, 0x0, 0x80,
    0x3e, 0xe0, 0x2d, 0x10, 0x3a, 0x0, 0x0, 0x0, 0x40, 0xcd, 0xcc, 0x4c, 0x3e, 0xec, 0x51, 0x38,
    0x3d, 0x20, 0x0, 0x0, 0x0, 0x0, 0x0, 0x20, 0x42, 0x1, 0x0, 0x0, 0x0, 0x20, 0x0, 0x0, 0x0, 0x20,
    0x0, 0x0, 0x0, 0xa0, 0x0, 0x0, 0x0, 0x0, 0x0, 0x80, 0x3e, 0xe0, 0x2d, 0x10, 0x3a, 0x0, 0x0,
    0x0, 0x40, 0xcd, 0xcc, 0x4c, 0x3e, 0xec, 0x51, 0x38, 0x3d, 0x30, 0x0, 0x0, 0x0, 0x0, 0x0, 0x48,
    0x42, 0x1, 0x0, 0x0, 0x0, 0x20, 0x0, 0x0, 0x0, 0x20, 0x0, 0x0, 0x0, 0xc0, 0x0, 0x0, 0x0, 0xcd,
    0xcc, 0xcc, 0x3d, 0x34, 0x80, 0xb7, 0x39, 0x0, 0x0, 0x0, 0x40, 0x8f, 0xc2, 0xf5, 0x3d, 0xec,
    0x51, 0x38, 0x3d, 0x40, 0x0, 0x0, 0x0, 0x0, 0x0, 0xc8, 0x42, 0x1, 0x0, 0x0, 0x0,
];

/// Apply requested startup overrides only when code patches are permitted.
/// # Safety
/// The caller owns a stable module before game threads or other writers run.
pub unsafe fn apply(module: &Module, cfg: &ShadowsCfg, allowed: bool) -> Result<(), String> {
    // SAFETY: forwards the caller's stable startup image contract.
    unsafe { apply_with_report(module, cfg, allowed, |_| {}) }
}
/// Apply checked writes and report each actual success, including before a later failure.
/// # Safety
/// Same startup image contract as apply.
unsafe fn apply_with_report(
    module: &Module,
    cfg: &ShadowsCfg,
    allowed: bool,
    mut applied: impl FnMut(&'static str),
) -> Result<(), String> {
    if !allowed {
        return Err("code patches are off on this build".into());
    }
    let point = !matches!(cfg.point_light_rays, ShadowValue::Game)
        || !matches!(cfg.point_light_tile, ShadowValue::Game)
        || !matches!(cfg.point_light_static_distance, ShadowValue::Game);
    let table = if point {
        // SAFETY: caller guarantees a stable startup module; the helper validates reads first.
        let table = unsafe { table_site(module) }?;
        // SAFETY: the complete target was bounded to the module's .rdata before reading.
        let original = unsafe { crate::patch::read_checked(table, VANILLA.len()) }
            .map_err(|e| e.to_string())?;
        if original != VANILLA {
            return Err(
                "point-light table differs (changed by another tool or a different build)".into(),
            );
        }
        Some(table)
    } else {
        None
    };
    if let Some(table) = table {
        let ultra = table.checked_add(176).ok_or("Ultra address overflow")?;
        for (offset, old, new) in [
            (
                8usize,
                192u32,
                match cfg.point_light_tile {
                    ShadowValue::Override(v) if matches!(v, 224 | 256) => Some(v),
                    ShadowValue::Game => None,
                    _ => return Err("unsupported tile".into()),
                },
            ),
            (
                32,
                64,
                match cfg.point_light_rays {
                    ShadowValue::Override(v) if (65..=256).contains(&v) => Some(v),
                    ShadowValue::Game => None,
                    _ => return Err("unsupported rays".into()),
                },
            ),
            (
                36,
                100f32.to_bits(),
                match cfg.point_light_static_distance {
                    ShadowValue::Override(v) if (101.0..=400.0).contains(&v) => Some(v.to_bits()),
                    ShadowValue::Game => None,
                    _ => return Err("unsupported static distance".into()),
                },
            ),
        ] {
            if let Some(value) = new {
                let site = ultra.checked_add(offset).ok_or("field address overflow")?;
                // SAFETY: whole-table preflight and bounds proved the field; caller excludes writers.
                unsafe { write_u32_checked(site, old, value) }.map_err(|e| e.to_string())?;
                applied(match offset {
                    8 => "point-light tile",
                    32 => "point-light rays",
                    _ => "point-light static distance",
                });
            }
        }
    }
    if let ShadowValue::Override(value) = cfg.sun_resolution {
        // SAFETY: caller guarantees startup with no executing game threads.
        unsafe { apply_sun(module, value) }?;
        applied("sun resolution");
    }
    Ok(())
}

/// Locate and bound the complete table before copying any table bytes.
/// # Safety
/// Same stable startup image contract as apply.
unsafe fn table_site(module: &Module) -> Result<usize, String> {
    let sig = &crate::sigs::MLS_QUALITY_TABLE_LEA;
    let pattern = Pattern::parse(sig.pattern).map_err(|e| e.to_string())?;
    let found = find_in_sections(module, sig.sections, &pattern).map_err(|e| e.player_text())?;
    let displacement_site = found.checked_add(12).ok_or("displacement overflow")?;
    // SAFETY: the unique matched instruction includes these four displacement bytes.
    let displacement =
        unsafe { crate::patch::read_checked(displacement_site, 4) }.map_err(|e| e.to_string())?;
    let disp = i32::from_le_bytes(
        displacement
            .as_slice()
            .try_into()
            .map_err(|_| "displacement length")?,
    );
    let target = found
        .checked_add(16)
        .and_then(|p| p.checked_add_signed(disp as isize))
        .ok_or("RIP target overflow")?;
    // SAFETY: each copy below is size-bounded to the module and checked for readability.
    let copy = |len: usize| -> Result<Vec<u8>, String> {
        if len > module.size || len > 65536 {
            return Err("headers outside module".into());
        }
        // SAFETY: caller keeps image stable; read_checked validates every copied byte.
        unsafe { crate::patch::read_checked(module.base, len) }.map_err(|e| e.to_string())
    };
    let dos = copy(64)?;
    let pe = crate::scan::u32_at(&dos, 0x3c).ok_or("DOS header")? as usize;
    let fixed = copy(pe.checked_add(24).ok_or("header overflow")?)?;
    let count = crate::scan::u16_at(&fixed, pe.checked_add(6).ok_or("header overflow")?)
        .ok_or("section count")? as usize;
    let optional = crate::scan::u16_at(&fixed, pe.checked_add(20).ok_or("header overflow")?)
        .ok_or("optional header")? as usize;
    let end = pe
        .checked_add(24)
        .and_then(|v| v.checked_add(optional))
        .and_then(|v| count.checked_mul(40).and_then(|size| v.checked_add(size)))
        .ok_or("section table overflow")?;
    let headers = copy(end)?;
    let info = crate::scan::parse_pe(&headers)?;
    let limit = module.size.min(info.size_of_image as usize);
    let relative = target
        .checked_sub(module.base)
        .ok_or("table before module")?;
    let table_end = relative
        .checked_add(VANILLA.len())
        .ok_or("table end overflow")?;
    if table_end > limit {
        return Err("table outside module".into());
    }
    let in_rdata = info
        .sections
        .iter()
        .filter(|s| s.is_named(".rdata"))
        .filter_map(|s| s.mem_range(limit))
        .any(|(rva, len)| {
            rva <= relative && rva.checked_add(len).is_some_and(|end| table_end <= end)
        });
    if !in_rdata {
        return Err("table outside .rdata".into());
    }
    Ok(target)
}

/// Startup only: every selected key gets exactly one outcome line; game defaults stay silent.
pub fn init(cfg: &ShadowsCfg) {
    let selected = [
        (
            "point-light rays",
            !matches!(cfg.point_light_rays, ShadowValue::Game),
            format!("{:?}", cfg.point_light_rays),
        ),
        (
            "point-light static distance",
            !matches!(cfg.point_light_static_distance, ShadowValue::Game),
            format!("{:?}", cfg.point_light_static_distance),
        ),
        (
            "point-light tile",
            !matches!(cfg.point_light_tile, ShadowValue::Game),
            format!("{:?}", cfg.point_light_tile),
        ),
        (
            "sun resolution",
            !matches!(cfg.sun_resolution, ShadowValue::Game),
            format!("{:?}", cfg.sun_resolution),
        ),
    ];
    if !selected.iter().any(|(_, on, _)| *on) {
        return;
    }
    let module = crate::scan::main_module();
    // SAFETY: entry calls this before WinMain and before enabling hooks; all reads/writes are checked.
    let mut applied = Vec::new();
    let result = unsafe {
        apply_with_report(&module, cfg, crate::entry::code_patches_allowed(), |key| {
            applied.push(key)
        })
    };
    for (key, on, value) in selected {
        if on {
            if applied.contains(&key) {
                log::info!("shadows {key}: set {value}");
            } else if let Err(error) = &result {
                log::warn!("shadows {key}: off: {error}");
            }
        }
    }
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

    /// The literal fixture is independent of production expected bytes.
    fn table_image(negative: bool) -> (Mem, usize) {
        let mem = image();
        let (code, table) = if negative {
            (0x2100, 0x1100)
        } else {
            (0x1200, 0x2200)
        };
        if negative {
            mem.put(0x148 + 12, &0x2000u32.to_le_bytes());
            mem.put(0x170 + 12, &0x1000u32.to_le_bytes());
        }
        let mut instruction = vec![
            0x8b, 2, 0x48, 0x8b, 0xf1, 0x4c, 0x6b, 0xc0, 0x2c, 0x48, 0x8d, 5, 0, 0, 0, 0, 0xc7,
            0x45, 0xe0, 1, 0, 1, 1,
        ];
        instruction[12..16].copy_from_slice(&((table as i32) - (code as i32 + 16)).to_le_bytes());
        mem.put(code, &instruction);
        let raw = TABLE_FIXTURE
            .split_whitespace()
            .flat_map(|word| u32::from_str_radix(word, 16).unwrap().to_be_bytes())
            .collect::<Vec<_>>();
        assert_eq!(raw.len(), 220);
        mem.put(table, &raw);
        (mem, table)
    }
    const TABLE_FIXTURE: &str = "10000000 08000000 60000000 9a99993e 96430b3c 3333d33f cdcc4c3e ec51383d 10000000 0000a041 00000000 18000000 10000000 80000000 0000803e e02d103a 00000040 cdcc4c3e ec51383d 18000000 0000f041 00000000 20000000 10000000 a0000000 0000803e e02d103a 00000040 cdcc4c3e ec51383d 20000000 00002042 01000000 20000000 20000000 a0000000 0000803e e02d103a 00000040 cdcc4c3e ec51383d 30000000 00004842 01000000 20000000 20000000 c0000000 cdcccc3d 3480b739 00000040 8fc2f53d ec51383d 40000000 0000c842 01000000";
    #[test]
    fn table_fields_change_only_their_owned_bytes_with_signed_rip() {
        for negative in [false, true] {
            for (offset, cfg, new) in [
                (
                    8,
                    ShadowsCfg {
                        point_light_tile: ShadowValue::Override(256),
                        ..Default::default()
                    },
                    256u32,
                ),
                (
                    32,
                    ShadowsCfg {
                        point_light_rays: ShadowValue::Override(128),
                        ..Default::default()
                    },
                    128,
                ),
                (
                    36,
                    ShadowsCfg {
                        point_light_static_distance: ShadowValue::Override(200.0),
                        ..Default::default()
                    },
                    200f32.to_bits(),
                ),
            ] {
                let (mem, table) = table_image(negative);
                let mut expected = mem.get(0, 0x4000);
                expected[table + 176 + offset..table + 180 + offset]
                    .copy_from_slice(&new.to_le_bytes());
                // SAFETY: only this test accesses its mapped fixture; no executing code.
                unsafe {
                    apply(
                        &Module {
                            base: mem.base,
                            size: 0x4000,
                        },
                        &cfg,
                        true,
                    )
                }
                .unwrap();
                assert_eq!(mem.get(0, 0x4000), expected);
            }
        }
    }
    #[test]
    fn every_table_byte_mismatch_refuses_all_shadow_writes() {
        for changed in 0..220 {
            let (mem, table) = table_image(false);
            let byte = mem.get(table + changed, 1)[0];
            mem.put(table + changed, &[byte ^ 1]);
            let cfg = ShadowsCfg {
                point_light_tile: ShadowValue::Override(256),
                point_light_rays: ShadowValue::Override(128),
                point_light_static_distance: ShadowValue::Override(200.0),
                sun_resolution: ShadowValue::Override(4096),
            };
            let original = mem.get(0, 0x4000);
            // SAFETY: this test owns the stable fixture.
            let error = unsafe {
                apply(
                    &Module {
                        base: mem.base,
                        size: 0x4000,
                    },
                    &cfg,
                    true,
                )
            }
            .unwrap_err();
            assert!(error.ends_with("(changed by another tool or a different build)"));
            assert_eq!(mem.get(0, 0x4000), original);
        }
    }
    #[test]
    fn unknown_build_and_outside_module_or_section_write_nothing() {
        for scenario in 0..4 {
            let (mem, table) = table_image(false);
            if scenario == 2 {
                mem.put(0x170 + 8, &0x100u32.to_le_bytes());
            }
            if scenario == 3 {
                mem.put(0x120c, &i32::MAX.to_le_bytes());
            }
            let original = mem.get(0, 0x4000);
            let cfg = ShadowsCfg {
                point_light_rays: ShadowValue::Override(128),
                ..Default::default()
            };
            // SAFETY: stable owned image; rejecting a shortened module must not read past it.
            assert!(unsafe {
                apply(
                    &Module {
                        base: mem.base,
                        size: if scenario == 1 { table + 100 } else { 0x4000 },
                    },
                    &cfg,
                    scenario != 0,
                )
            }
            .is_err());
            assert_eq!(mem.get(0, 0x4000), original);
        }
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

    #[test]
    fn unreadable_owned_table_page_is_refused_without_a_fault_or_write() {
        use windows_sys::Win32::System::Memory::{PAGE_NOACCESS, PAGE_READWRITE};
        let (mem, _) = table_image(false);
        let original = mem.get(0, 0x4000);
        mem.protect(0x2000, 0x1000, PAGE_NOACCESS);
        let cfg = ShadowsCfg {
            point_light_rays: ShadowValue::Override(128),
            ..Default::default()
        };
        // SAFETY: stable owned image; inaccessible table must be refused before copying it.
        let result = unsafe {
            apply(
                &Module {
                    base: mem.base,
                    size: 0x4000,
                },
                &cfg,
                true,
            )
        };
        mem.protect(0x2000, 0x1000, PAGE_READWRITE);
        assert!(result.is_err());
        assert_eq!(mem.get(0, 0x4000), original);
    }
    #[test]
    fn outcome_reports_preserve_successes_if_a_later_sun_scan_refuses() {
        let (mem, _) = table_image(false);
        mem.put(0x1100, &[0]);
        let cfg = ShadowsCfg {
            point_light_rays: ShadowValue::Override(128),
            sun_resolution: ShadowValue::Override(4096),
            ..Default::default()
        };
        let mut applied = Vec::new();
        // SAFETY: this test owns its stable mapped image and no game code executes.
        let result = unsafe {
            apply_with_report(
                &Module {
                    base: mem.base,
                    size: 0x4000,
                },
                &cfg,
                true,
                |key| applied.push(key),
            )
        };
        assert!(result.is_err());
        assert_eq!(applied, ["point-light rays"]);
        assert_eq!(mem.get(0x2200 + 176 + 32, 4), 128u32.to_le_bytes());
    }
    /// Maintainer-only file check; never part of ordinary validation.
    #[test]
    #[ignore = "reads the real game exe; maintainer verification only"]
    fn shadow_signatures_and_all_five_rows_match_known_client() {
        let Some(path) = crate::build_id::test_support::game_exe_or_skip(
            "shadow_signatures_and_all_five_rows_match_known_client",
        ) else {
            return;
        };
        let file = std::fs::read(path).unwrap();
        let info = crate::scan::parse_pe_file(&file).unwrap();
        let (text_va, text) = info.section_va_bytes(&file, ".text").unwrap();
        let lea = crate::scan::find_unique(
            text,
            &Pattern::parse(crate::sigs::MLS_QUALITY_TABLE_LEA.pattern).unwrap(),
        )
        .unwrap();
        crate::scan::find_unique(text, &Pattern::parse(SUN_SHADOW_ULTRA.pattern).unwrap()).unwrap();
        let disp = i32::from_le_bytes(text[lea + 12..lea + 16].try_into().unwrap());
        let target = (text_va as usize + lea + 16)
            .checked_add_signed(disp as isize)
            .unwrap();
        let (rdata_va, rdata) = info.section_va_bytes(&file, ".rdata").unwrap();
        let offset = target.checked_sub(rdata_va as usize).unwrap();
        assert_eq!(&rdata[offset..offset + 220], VANILLA);
    }
}
