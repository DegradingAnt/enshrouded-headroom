//! The signature table for game build f68140a3 (exe sha256 b28b448a...).
//!
//! Each entry is a byte pattern (`??` is a wildcard for rel32 / rip-relative displacements) that
//! must match exactly once in the named sections, plus the offsets inside the match where later
//! tasks patch. Source: `docs/re-notes.md`, section "Signature table".
//!
//! Where a pattern ends before its patch site in the RE-notes table, it is extended here so a
//! match also proves the original bytes at the patch offsets (`VOXEL_HIGH_IMMS`, and
//! `ASYNC_OVERRIDE` except its one `??` byte at +11). `TEXTURE_TABLE` is not in this file: Task 7
//! builds it from data.

/// One signature: where to look, what to look for, and where inside the match to patch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sig {
    /// The constant's name, for log lines.
    pub name: &'static str,
    /// Space-separated hex bytes, `??` for a wildcard; see [`crate::scan::Pattern::parse`].
    pub pattern: &'static str,
    /// PE section names to search; a match must be unique across all of them together.
    pub sections: &'static [&'static str],
    /// Byte offsets from the start of the match to each patch site. Never empty for an entry in
    /// [`ALL`] (a test checks it). Single-site entries hold one element.
    pub patch_offsets: &'static [usize],
}

impl Sig {
    /// The first patch offset: the only one for a single-site entry. Zero for an entry with no
    /// offsets, which [`ALL`] has none of.
    pub fn patch_offset(&self) -> usize {
        self.patch_offsets.first().copied().unwrap_or(0)
    }
}

/// Scene-settings function (post-hook). The pattern starts at the function entry, so the match
/// VA is the function (0x1400f72b0 in f68140a3); hook at match + `patch_offset()` (0).
pub const SCENE_SETTINGS_FN: Sig = Sig {
    name: "SCENE_SETTINGS_FN",
    pattern: "41 8b 00 f3 0f 10 0d ?? ?? ?? ?? 89 81 08 03 00 00 f3 41 0f 10 40 04 f3 0f 58 c1 f3 0f 11 81 0c 03 00 00 0f 57 c0 41 0f 2f 40 08 76 ?? f3 0f 10 05 ?? ?? ?? ?? eb ?? f3 0f 10 05 ?? ?? ?? ?? f3 41 0f 59 40 08 45 33 c9",
    sections: &[".text"],
    patch_offsets: &[0],
};

/// Brick scatter High-quality distance load (rip-relative disp32 at +9). Anchor 0x140079502.
pub const SCATTER_BRICK_LOAD: Sig = Sig {
    name: "SCATTER_BRICK_LOAD",
    pattern: "83 f9 01 75 ?? f3 0f 10 05 ?? ?? ?? ?? 48 83 c4 28",
    sections: &[".text"],
    patch_offsets: &[9],
};

/// Per-instance scatter High-quality distance loads (distance disp32 at +30). Anchor 0x14007691c.
pub const SCATTER_INSTANCE_LOADS: Sig = Sig {
    name: "SCATTER_INSTANCE_LOADS",
    pattern: "83 f9 01 0f 85 ?? ?? ?? ?? f3 0f 10 35 ?? ?? ?? ?? f3 0f 10 3d ?? ?? ?? ?? f3 44 0f 10 05 ?? ?? ?? ?? eb ??",
    sections: &[".text"],
    patch_offsets: &[30],
};

/// Scatter capacity shader constant `scatteredObjectCapacity` (imm32 0x14000 at +12). Anchor
/// 0x140074d4c.
pub const SCATTER_CAP_CONST: Sig = Sig {
    name: "SCATTER_CAP_CONST",
    pattern: "89 85 80 05 00 00 c7 85 84 05 00 00 00 40 01 00 41 8b 55 10",
    sections: &[".text"],
    patch_offsets: &[12],
};

/// Scatter capacity allocation argument (imm32 0x14000 at +12). Anchor 0x1400745d8.
pub const SCATTER_CAP_ALLOC: Sig = Sig {
    name: "SCATTER_CAP_ALLOC",
    pattern: "49 8b 56 20 48 8d 4d 20 c7 44 24 30 00 40 01 00",
    sections: &[".text"],
    patch_offsets: &[12],
};

/// Deco-object High distance (imm32 1.5f at +9). Anchor 0x1400d77c3.
pub const DECO_HIGH_IMM: Sig = Sig {
    name: "DECO_HIGH_IMM",
    pattern: "83 f9 01 75 ?? c7 44 24 30 00 00 c0 3f",
    sections: &[".text"],
    patch_offsets: &[9],
};

/// Distant-object High distance (imm32 2.5f at +9). Anchor 0x1400d7816.
pub const DISTANT_HIGH_IMM: Sig = Sig {
    name: "DISTANT_HIGH_IMM",
    pattern: "83 f9 01 75 ?? c7 44 24 30 00 00 20 40",
    sections: &[".text"],
    patch_offsets: &[9],
};

/// Voxel mesh quality High immediates (imm32 floats 32.0 / 128.0 / 160.0 at +15 / +22 / +29).
/// Anchor 0x1400f7f1f. Extended past the RE-notes table so the match proves the original imms.
pub const VOXEL_HIGH_IMMS: Sig = Sig {
    name: "VOXEL_HIGH_IMMS",
    pattern: "8b 4f 40 85 c9 74 ?? 83 f9 01 75 ?? c7 43 60 00 00 00 42 c7 43 64 00 00 00 43 c7 43 68 00 00 20 43",
    sections: &[".text"],
    patch_offsets: &[15, 22, 29],
};

/// Async compute override. Task 8 writes 16 bytes at +3, which end at +19. Anchor 0x1408490d2.
/// Extended past the RE-notes table so the match covers the whole patch. It proves every byte
/// of the 16-byte patch EXCEPT +11 (the `jz rel8` displacement, a `??`): Task 8 must compare all
/// 16 ORIGINAL bytes itself before it writes, and not rely on this match for that byte.
pub const ASYNC_OVERRIDE: Sig = Sig {
    name: "ASYNC_OVERRIDE",
    pattern: "88 4d 3e 44 38 b6 89 52 03 00 74 ?? 0f b6 86 88 52 03 00",
    sections: &[".text"],
    patch_offsets: &[3],
};

/// Terrain update function (pre-hook; prologue only, so fragile across updates). Anchor
/// 0x1401e2e80.
pub const TERRAIN_UPDATE_FN: Sig = Sig {
    name: "TERRAIN_UPDATE_FN",
    pattern:
        "48 89 5c 24 10 4c 89 44 24 18 55 56 57 41 54 41 55 41 56 41 57 48 8d ac 24 50 e0 ff ff",
    sections: &[".text"],
    patch_offsets: &[0],
};

/// The grass pass function `FUN_1400767a0(rcx, pass, f)` (counter-probe pre-hook; prologue only,
/// so fragile across updates). The pattern is its first 14 bytes, so the match VA is the function
/// (0x1400767a0 in f68140a3); hook at match + `patch_offset()` (0). It has no immediate site.
/// docs/re-notes.md, "Scatter counter probe".
pub const SCATTER_FRAME_FN: Sig = Sig {
    name: "SCATTER_FRAME_FN",
    pattern: "48 89 54 24 10 55 53 57 41 55 41 56 41 57",
    sections: &[".text"],
    patch_offsets: &[0],
};

/// Every signature in this file, in table order.
pub const ALL: &[&Sig] = &[
    &SCENE_SETTINGS_FN,
    &SCATTER_BRICK_LOAD,
    &SCATTER_INSTANCE_LOADS,
    &SCATTER_CAP_CONST,
    &SCATTER_CAP_ALLOC,
    &DECO_HIGH_IMM,
    &DISTANT_HIGH_IMM,
    &VOXEL_HIGH_IMMS,
    &ASYNC_OVERRIDE,
    &TERRAIN_UPDATE_FN,
    &SCATTER_FRAME_FN,
];

#[cfg(test)]
mod tests {
    use super::*;

    fn tokens(s: &Sig) -> Vec<&'static str> {
        s.pattern.split_ascii_whitespace().collect()
    }

    #[test]
    fn all_has_the_eleven_entries_with_distinct_names_and_patterns() {
        assert_eq!(ALL.len(), 11);
        for (i, a) in ALL.iter().enumerate() {
            for b in ALL.iter().skip(i + 1) {
                assert_ne!(a.name, b.name);
                assert_ne!(a.pattern, b.pattern, "{} and {}", a.name, b.name);
            }
        }
    }

    #[test]
    fn name_field_matches_the_constant() {
        // The constant names are in `ALL` order; the names are what the log prints.
        let names: Vec<&str> = ALL.iter().map(|s| s.name).collect();
        assert_eq!(
            names,
            [
                "SCENE_SETTINGS_FN",
                "SCATTER_BRICK_LOAD",
                "SCATTER_INSTANCE_LOADS",
                "SCATTER_CAP_CONST",
                "SCATTER_CAP_ALLOC",
                "DECO_HIGH_IMM",
                "DISTANT_HIGH_IMM",
                "VOXEL_HIGH_IMMS",
                "ASYNC_OVERRIDE",
                "TERRAIN_UPDATE_FN",
                "SCATTER_FRAME_FN",
            ]
        );
    }

    /// The probe hooks the function entry: the pattern is the 14-byte prologue, with no wildcard
    /// and no immediate site, and it is in `ALL`, so the game-exe test checks it.
    #[test]
    fn scatter_frame_fn_is_the_entry_prologue() {
        assert!(ALL.contains(&&SCATTER_FRAME_FN));
        assert_eq!(SCATTER_FRAME_FN.patch_offsets, &[0]);
        assert_eq!(SCATTER_FRAME_FN.patch_offset(), 0);
        assert_eq!(
            tokens(&SCATTER_FRAME_FN),
            ["48", "89", "54", "24", "10", "55", "53", "57", "41", "55", "41", "56", "41", "57"]
        );
    }

    #[test]
    fn every_entry_has_offsets_that_fit_in_the_pattern() {
        for s in ALL {
            assert!(
                !s.patch_offsets.is_empty(),
                "{} has no patch offsets",
                s.name
            );
            let len = tokens(s).len();
            for off in s.patch_offsets {
                assert!(
                    off + 4 <= len,
                    "{}: offset {off} + 4 > pattern length {len}",
                    s.name
                );
            }
        }
    }

    #[test]
    fn every_entry_searches_text_only() {
        for s in ALL {
            assert_eq!(s.sections, &[".text"], "{}", s.name);
        }
    }

    #[test]
    fn patch_offset_is_the_first_entry() {
        assert_eq!(VOXEL_HIGH_IMMS.patch_offset(), 15);
        assert_eq!(VOXEL_HIGH_IMMS.patch_offsets, &[15, 22, 29]);
        assert_eq!(ASYNC_OVERRIDE.patch_offset(), 3);
        assert_eq!(SCENE_SETTINGS_FN.patch_offset(), 0);
        assert_eq!(TERRAIN_UPDATE_FN.patch_offset(), 0);
        assert_eq!(SCATTER_INSTANCE_LOADS.patch_offset(), 30);
        let empty = Sig {
            name: "x",
            pattern: "00",
            sections: &[],
            patch_offsets: &[],
        };
        assert_eq!(empty.patch_offset(), 0);
    }

    /// The table in the brief: single-site offsets, the exact original bytes the pattern holds at
    /// each immediate site, and `??` at each displacement site.
    #[test]
    fn patch_sites_hold_the_expected_original_bytes() {
        let imm = |s: &Sig, off: usize, expect: &[&str]| {
            let t = tokens(s);
            assert_eq!(t.get(off..off + 4), Some(expect), "{} at +{off}", s.name);
        };
        imm(&SCATTER_CAP_CONST, 12, &["00", "40", "01", "00"]);
        imm(&SCATTER_CAP_ALLOC, 12, &["00", "40", "01", "00"]);
        imm(&DECO_HIGH_IMM, 9, &["00", "00", "c0", "3f"]);
        imm(&DISTANT_HIGH_IMM, 9, &["00", "00", "20", "40"]);
        imm(&VOXEL_HIGH_IMMS, 15, &["00", "00", "00", "42"]);
        imm(&VOXEL_HIGH_IMMS, 22, &["00", "00", "00", "43"]);
        imm(&VOXEL_HIGH_IMMS, 29, &["00", "00", "20", "43"]);
        imm(&SCATTER_BRICK_LOAD, 9, &["??", "??", "??", "??"]);
        imm(&SCATTER_INSTANCE_LOADS, 30, &["??", "??", "??", "??"]);
    }

    #[test]
    fn async_override_covers_its_sixteen_byte_patch() {
        // Task 8 writes 16 bytes at +3, which end at +19: the pattern must reach that far.
        assert_eq!(tokens(&ASYNC_OVERRIDE).len(), 19);
        assert_eq!(ASYNC_OVERRIDE.patch_offset() + 16, 19);
    }

    /// Of the 16 patch bytes (+3..+19) the match fixes every one but +11, the `jz rel8`
    /// displacement. Task 8 checks all 16 original bytes itself, so this test pins the gap.
    #[test]
    fn async_override_has_exactly_one_wildcard_in_its_patch() {
        let t = tokens(&ASYNC_OVERRIDE);
        let start = ASYNC_OVERRIDE.patch_offset();
        let patch = t.get(start..start + 16).expect("pattern reaches +19");
        let wild: Vec<usize> = patch
            .iter()
            .enumerate()
            .filter(|(_, tok)| **tok == "??")
            .map(|(i, _)| start + i)
            .collect();
        assert_eq!(wild, [11]);
        assert_eq!(
            t.get(10),
            Some(&"74"),
            "the jz opcode just before the displacement"
        );
    }
}
