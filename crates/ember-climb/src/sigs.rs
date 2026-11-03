//! Every byte pattern this mod needs. Each one must match exactly once in `.text`, or the whole
//! mod stays off. The source of each is in docs/re-notes.md, "Free climbing". The addresses in
//! these docs are the client's (Steam exe sha256 d64966e2...). The server's are found by scan.

/// One byte pattern.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sig {
    /// Its name in the log.
    pub name: &'static str,
    /// Hex bytes, with `??` for any byte.
    pub pattern: &'static str,
}

/// The wall probe `FUN_14024abc0(out, ctx, pos, dir, len) -> out`. Hooked. 0x14024abc0.
pub const PROBE_FN: Sig = Sig {
    name: "PROBE_FN",
    pattern: "48 8b c4 48 89 58 08 55 56 57 41 54 41 55 41 56 41 57 48 8d a8 c8 fe ff ff",
};

/// The wall-lost check `FUN_14024b390(ctx) -> id`. Hooked. 0x14024b390.
pub const WALL_LOST_FN: Sig = Sig {
    name: "WALL_LOST_FN",
    pattern: "48 81 ec 88 00 00 00 48 8b 41 08 0f 57 ed f2 0f 10 40 04",
};

/// The wall-lost check's success return, at +26 of this match, and the shared epilogue at +18.
/// Written. Match 0x14024b48b.
pub const WALL_LOST_RET: Sig = Sig {
    name: "WALL_LOST_RET",
    pattern:
        "e8 ?? ?? ?? ?? 80 7c 24 50 00 74 0e 8b 05 ?? ?? ?? ?? 48 81 c4 88 00 00 00 c3 8b 44 24 \
              70 48 81 c4 88 00 00 00 c3",
};

/// The climb update's store of the attachment id. Written. Match 0x140418db8.
pub const ATTACH_STORE: Sig = Sig {
    name: "ATTACH_STORE",
    pattern: "48 8b 87 a0 00 00 00 39 58 18 74 0f 48 8d 8f a0 00 00 00 e8",
};

/// The move check `FUN_140417d90(velocity, ctx, pos, dt) -> bool`. Hooked. 0x140417d90.
pub const MOVE_CHECK_FN: Sig = Sig {
    name: "MOVE_CHECK_FN",
    pattern: "48 8b c4 48 89 58 08 48 89 70 10 48 89 78 18 55 48 8d 68 a1 48 81 ec 00 01 00 00 0f \
              29 78 d8",
};

/// The exit query `FUN_14024a6f0(out, ctx, pos) -> out`. Hooked. 0x14024a6f0.
pub const EXIT_QUERY_FN: Sig = Sig {
    name: "EXIT_QUERY_FN",
    pattern: "48 8b c4 48 89 58 08 48 89 70 18 48 89 78 20 55 41 56 41 57 48 8d a8 58 ff ff ff 48 \
              81 ec 90 01",
};

/// The start check `FUN_140419570(iterator)`. Hooked for the glider and the diagnostics.
/// 0x140419570.
pub const START_FN: Sig = Sig {
    name: "START_FN",
    pattern: "40 55 57 48 8d 6c 24 d8 48 81 ec 28 01 00 00 48 8b f9",
};

/// The layer of the request to start climbing, one byte at +18 of this match. Written only with
/// `Glide = 1`. Match 0x140419935.
pub const ENTER_LAYER: Sig = Sig {
    name: "ENTER_LAYER",
    pattern: "c6 44 24 28 38 4c 8d 44 24 30 48 8b d0 66 c7 44 24 20 13 00 48 8d 4d d0 e8",
};

/// The game's raycast `FUN_1409fff60`. Called, not changed. 0x1409fff60.
pub const RAYCAST_FN: Sig = Sig {
    name: "RAYCAST_FN",
    pattern: "40 53 48 81 ec b0 00 00 00 48 8b d9 48 8b 8a a0 01 00 00 48 85 c9 74 6c",
};

/// The bottom or middle entry `FUN_14024c0d0(out, ctx, input, time) -> out`. Hooked. 0x14024c0d0.
pub const LOW_ENTRY_FN: Sig = Sig {
    name: "LOW_ENTRY_FN",
    pattern: "48 8b c4 48 89 58 08 48 89 68 18 48 89 70 20 57 48 81 ec 90 00 00 00",
};

/// The top entry `FUN_14024c320(out, ctx, p3, input, p5) -> out`. Hooked for the diagnostics.
/// 0x14024c320.
pub const TOP_ENTRY_FN: Sig = Sig {
    name: "TOP_ENTRY_FN",
    pattern: "48 8b c4 48 89 58 08 48 89 70 18 55 57 41 54 41 56 41 57 48 8d a8 d8 fe ff ff",
};

/// Stop climbing `FUN_14024a090(ctx, time)`. Hooked for the diagnostics. 0x14024a090.
pub const STOP_FN: Sig = Sig {
    name: "STOP_FN",
    pattern: "48 89 5c 24 08 57 48 83 ec 20 48 8b da 48 8b f9 48 8b 51 08",
};

/// The exit-top check `FUN_14024c9f0(ctx, input) -> bool`. Hooked for the diagnostics.
/// 0x14024c9f0.
pub const EXIT_TOP_FN: Sig = Sig {
    name: "EXIT_TOP_FN",
    pattern: "48 89 5c 24 08 57 48 83 ec 20 48 8b d9 48 8b fa 48 8b 49 28",
};

/// The bottom exit check `FUN_14024c8f0(out, ctx, input) -> out`. Hooked for the diagnostics.
/// 0x14024c8f0.
pub const EXIT_LOW_FN: Sig = Sig {
    name: "EXIT_LOW_FN",
    pattern: "48 89 5c 24 08 48 89 74 24 10 57 48 83 ec 50 48 8b d9 0f 29 74 24 40 48 8b 4a 28",
};

/// The climb stamina program's `call` of the stamina cost per metre, `FUN_14024a360(data) -> f32`.
/// That function has two siblings with the same body and another key, so no pattern can name it.
/// This call can. The function is hooked only when the multiplier is not 1. Match 0x1402eb77e,
/// `call` at +[`STAMINA_CALL_AT`], function 0x14024a360.
pub const STAMINA_CALL: Sig = Sig {
    name: "STAMINA_CALL",
    pattern: "45 0f 57 c0 45 0f 28 c5 33 c9 e8 ?? ?? ?? ?? f3 0f 59 f6 0f 28 f8 f3 45 0f 59 d2 0f \
              57 c0 f3 45 0f 59 c9",
};

/// Where the `call` is in a [`STAMINA_CALL`] match.
pub const STAMINA_CALL_AT: usize = 10;
/// What the function behind that `call` starts with: `sub rsp,0x28`, `test rcx,rcx`.
pub const STAMINA_PROLOGUE: [u8; 7] = [0x48, 0x83, 0xec, 0x28, 0x48, 0x85, 0xc9];

/// Every signature.
pub const ALL: [&Sig; 15] = [
    &PROBE_FN,
    &WALL_LOST_FN,
    &WALL_LOST_RET,
    &ATTACH_STORE,
    &MOVE_CHECK_FN,
    &EXIT_QUERY_FN,
    &START_FN,
    &ENTER_LAYER,
    &RAYCAST_FN,
    &LOW_ENTRY_FN,
    &TOP_ENTRY_FN,
    &STOP_FN,
    &EXIT_TOP_FN,
    &EXIT_LOW_FN,
    &STAMINA_CALL,
];

/// Where each signature matched.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Found(Vec<(&'static Sig, usize)>);

impl Found {
    /// Where `sig` matched. `Err` names it, for the rare case of a signature left out of [`ALL`].
    pub fn address(&self, sig: &Sig) -> Result<usize, String> {
        self.0
            .iter()
            .find(|(found, _)| found.name == sig.name)
            .map(|(_, address)| *address)
            .ok_or_else(|| format!("{} was not looked for", sig.name))
    }
}

/// Look for every signature in `sigs` with `find`, which gives the address of a single match or
/// the reason there is none. `Err` names every signature that failed, not only the first, so one
/// log line says everything that changed in a new build.
pub fn find_all(
    sigs: &[&'static Sig],
    find: impl Fn(&Sig) -> Result<usize, String>,
) -> Result<Found, String> {
    let mut found = Vec::new();
    let mut failed = Vec::new();
    for &sig in sigs {
        match find(sig) {
            Ok(address) => found.push((sig, address)),
            Err(why) => failed.push(format!("{}: {why}", sig.name)),
        }
    }
    if failed.is_empty() {
        Ok(Found(found))
    } else {
        Err(format!(
            "{} of {} signatures did not match exactly once: {}",
            failed.len(),
            sigs.len(),
            failed.join("; ")
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use emberkit::scan::Pattern;

    /// Every pattern parses, and the table has no two entries with one name.
    #[test]
    fn every_pattern_parses_and_every_name_is_unique() {
        let mut names: Vec<&str> = ALL.iter().map(|sig| sig.name).collect();
        for sig in ALL {
            Pattern::parse(sig.pattern).unwrap_or_else(|e| panic!("{}: {e}", sig.name));
        }
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), ALL.len());
    }

    /// One signature that fails stops everything, and the error names it.
    #[test]
    fn a_failed_signature_is_named() {
        let error = find_all(&ALL, |sig| {
            if sig.name == "MOVE_CHECK_FN" {
                Err("not found".to_owned())
            } else {
                Ok(0x1000)
            }
        })
        .unwrap_err();
        assert!(error.contains("MOVE_CHECK_FN: not found"), "{error}");
        assert!(error.starts_with("1 of 15 signatures"), "{error}");
    }

    /// All the failures are in one message, so a new build shows its whole damage at once.
    #[test]
    fn every_failed_signature_is_named() {
        let error = find_all(&ALL, |sig| Err(format!("no {}", sig.name))).unwrap_err();
        for sig in ALL {
            assert!(error.contains(sig.name), "{error}");
        }
    }

    /// When all match, each address is there by name.
    #[test]
    fn matches_are_found_by_name() {
        let found = find_all(&[&PROBE_FN, &STOP_FN], |sig| {
            Ok(if sig.name == "STOP_FN" { 2 } else { 1 })
        })
        .unwrap();
        assert_eq!(found.address(&PROBE_FN), Ok(1));
        assert_eq!(found.address(&STOP_FN), Ok(2));
        assert!(found
            .address(&RAYCAST_FN)
            .unwrap_err()
            .contains("RAYCAST_FN"));
    }
}
