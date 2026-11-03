//! Startup-only preflight for the optional read-only Jump observers.
//!
//! Native bytes and layouts are recorded in docs/re-notes.md under
//! "W52 diagnostic request and impulse observation". No observer is installed by this module.

use emberkit::scan::{find_unique, Pattern};

use crate::sigs::Sig;

/// The entity table layout admitted by the exact executable build.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Layout {
    /// The previous supported client/server native layout.
    Previous,
    /// The current supported client/server native layout.
    Current,
}

impl Layout {
    /// The documented native EntityId table pointer offset for this exact image family.
    pub const fn entity_table_offset(self) -> usize {
        match self {
            Self::Previous => 0x828,
            Self::Current => 0x830,
        }
    }

    /// Require the exact build identity already admitted by startup's executable hash gate.
    fn from_label(label: &str) -> Result<Self, String> {
        match label {
            "f68140a3" | "f68140a3-steam" | "d7baa768-server" => Ok(Self::Previous),
            "1283531-steam" | "1283531-server" => Ok(Self::Current),
            _ => Err(format!(
                "Jump observation has no verified layout for {label}"
            )),
        }
    }
}

/// All targets must be verified together before any optional observer is installed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Targets {
    /// The native acceptance helper entry.
    pub accepted: usize,
    /// The accessor reached by both validated native acceptance calls.
    pub accessor: usize,
    /// The native Jump dispatcher entry.
    pub dispatcher: usize,
    /// The exact executable's entity table layout.
    pub layout: Layout,
}

/// The native accepted helper's four-image unique entry prefix.
pub const ACCEPTED: Sig = Sig {
    name: "JUMP_ACCEPTED_OBSERVER",
    pattern: "48 89 4c 24 08 55 53 56 57 41 54 41 55 41 56 41 57 48 8d ac 24 a8 fe ff ff \
              48 81 ec 58 02 00 00 4c 8b 81 18 04 00 00 4c 8b f1 48 8b 91 88 00 00 00",
};

/// The native calculated-delta dispatcher's four-image unique entry prefix.
pub const DISPATCHER: Sig = Sig {
    name: "JUMP_DELTA_OBSERVER",
    pattern: "48 8b c4 48 89 58 08 55 56 57 41 54 41 55 41 56 41 57 48 8d 68 98 48 81 ec \
              30 01 00 00 0f 29 70 b8 48 8b f2 0f 29 78 a8 48 8b f9 44 0f 29 40 98 44 0f 29 \
              48 88 44 0f 29 90 78 ff ff ff 44 0f 29 98 68 ff ff ff 44 0f 29 a0 58 ff ff ff \
              44 0f 29 a8 48 ff ff ff 44 0f 29 b0 38 ff ff ff 44 0f 29 b8 28 ff ff ff 45 0f b6 f8",
};

/// Full accessor body, checked after following both accepted-helper calls, never scanned alone.
const ACCESSOR_BODY: Sig = Sig {
    name: "JUMP_INPUT_OBSERVER",
    pattern: "40 53 48 83 ec 30 48 8b d9 48 8b 49 28 48 85 c9 74 22 0f b6 43 22 4c 8b 4b \
              18 4c 8b 43 10 48 8b 53 08 88 44 24 28 0f b7 43 20 66 89 44 24 20 e8 ?? ?? ?? \
              ?? 48 8b 03 48 83 c4 30 5b c3",
};

/// Only these two entry patterns are unique; accessor resolution follows validated native calls.
pub const ALL: [Sig; 2] = [ACCEPTED, DISPATCHER];

/// Preflight the whole optional set without writing any native memory.
///
/// Startup must first identify the executable by its admitted exact SHA256. `find` searches its
/// executable code; `read` returns checked owned bytes for validation. Failure admits no sites.
pub fn preflight(
    label: &str,
    mut find: impl FnMut(&Sig) -> Result<Vec<usize>, String>,
    mut read: impl FnMut(usize, usize) -> Option<Vec<u8>>,
) -> Result<Targets, String> {
    let layout = Layout::from_label(label)?;
    let accepted = unique(ACCEPTED, &mut find)?;
    let dispatcher = unique(DISPATCHER, &mut find)?;
    verify_body(accepted, ACCEPTED, &mut read)?;
    verify_body(dispatcher, DISPATCHER, &mut read)?;
    let accessor = follow_call(accepted, 0x730, &mut read)?;
    if accessor != follow_call(accepted, 0x79f, &mut read)? {
        return Err("Jump accepted helper's ActorInput calls disagree".to_owned());
    }
    if accepted == dispatcher || accessor == accepted || accessor == dispatcher {
        return Err("Jump observer function entries overlap".to_owned());
    }
    verify_body(accessor, ACCESSOR_BODY, &mut read)?;
    Ok(Targets {
        accepted,
        accessor,
        dispatcher,
        layout,
    })
}

/// Refuse missing, duplicate or null entries even if a caller's scanner reports them as success.
fn unique(
    sig: Sig,
    find: &mut impl FnMut(&Sig) -> Result<Vec<usize>, String>,
) -> Result<usize, String> {
    let found = find(&sig).map_err(|why| format!("{}: {why}", sig.name))?;
    match found.as_slice() {
        [address] if *address != 0 => Ok(*address),
        _ => Err(format!(
            "{}: {} matches, expected one nonzero entry",
            sig.name,
            found.len()
        )),
    }
}

/// Check an entire documented prefix/body, including exact length and its first byte.
fn verify_body(
    address: usize,
    sig: Sig,
    read: &mut impl FnMut(usize, usize) -> Option<Vec<u8>>,
) -> Result<(), String> {
    let pattern = Pattern::parse(sig.pattern).map_err(|why| format!("{}: {why}", sig.name))?;
    address
        .checked_add(pattern.len())
        .ok_or_else(|| format!("{}: address overflow", sig.name))?;
    let bytes = read(address, pattern.len())
        .filter(|bytes| bytes.len() == pattern.len())
        .ok_or_else(|| format!("{}: bytes unavailable", sig.name))?;
    match find_unique(&bytes, &pattern) {
        Ok(0) => Ok(()),
        _ => Err(format!("{}: native bytes differ", sig.name)),
    }
}

/// Follow one exact direct CALL with checked signed-relative arithmetic.
fn follow_call(
    accepted: usize,
    offset: usize,
    read: &mut impl FnMut(usize, usize) -> Option<Vec<u8>>,
) -> Result<usize, String> {
    let at = accepted
        .checked_add(offset)
        .ok_or("Jump accessor call address overflow")?;
    let next = at
        .checked_add(5)
        .ok_or("Jump accessor call address overflow")?;
    let bytes = read(at, 5)
        .filter(|bytes| bytes.len() == 5)
        .ok_or("Jump accessor call unavailable")?;
    if bytes.first() != Some(&0xe8) {
        return Err("Jump accessor call opcode differs".to_owned());
    }
    let displacement: [u8; 4] = bytes
        .get(1..5)
        .and_then(|bytes| bytes.try_into().ok())
        .ok_or("Jump accessor call displacement unavailable")?;
    let displacement = i32::from_le_bytes(displacement);
    let magnitude = usize::try_from(displacement.unsigned_abs())
        .map_err(|_| "Jump accessor displacement exceeds address range")?;
    let target = if displacement < 0 {
        next.checked_sub(magnitude)
    } else {
        next.checked_add(magnitude)
    };
    target
        .filter(|target| *target != 0)
        .ok_or_else(|| "Jump accessor target overflow/null".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    /// An owned startup seam with signed backward calls and no native execution.
    struct Image {
        /// Exact bytes indexed by the address expected by a startup read.
        ranges: BTreeMap<usize, Vec<u8>>,
    }

    impl Image {
        /// All documented patterns and the two calls to one native accessor.
        fn valid() -> Self {
            let mut ranges = BTreeMap::from([
                (0x2000, literal(ACCEPTED)),
                (0x5000, literal(DISPATCHER)),
                (0x1000, literal(ACCESSOR_BODY)),
            ]);
            for offset in [0x730usize, 0x79f] {
                let at = 0x2000 + offset;
                let displacement =
                    i32::try_from(0x1000i64 - i64::try_from(at + 5).unwrap()).unwrap();
                ranges.insert(
                    at,
                    [vec![0xe8], displacement.to_le_bytes().to_vec()].concat(),
                );
            }
            Self { ranges }
        }

        /// Run the complete production preflight against owned byte ranges.
        fn preflight(&self) -> Result<Targets, String> {
            preflight(
                "1283531-steam",
                |sig| Ok(vec![if *sig == ACCEPTED { 0x2000 } else { 0x5000 }]),
                |at, len| {
                    self.ranges
                        .get(&at)
                        .filter(|bytes| bytes.len() == len)
                        .cloned()
                },
            )
        }
    }

    /// Materialize documented bytes; wildcard displacement values are arbitrary owned bytes.
    fn literal(sig: Sig) -> Vec<u8> {
        sig.pattern
            .split_ascii_whitespace()
            .map(|cell| {
                if cell == "??" {
                    0
                } else {
                    u8::from_str_radix(cell, 16).unwrap()
                }
            })
            .collect()
    }

    /// Both call sites, both unique prefixes and the whole accessor body admit one set together.
    #[test]
    fn whole_set_requires_two_matching_calls_and_complete_accessor() {
        let mut image = Image::valid();
        assert_eq!(
            image.preflight(),
            Ok(Targets {
                accepted: 0x2000,
                accessor: 0x1000,
                dispatcher: 0x5000,
                layout: Layout::Current,
            })
        );
        image.ranges.get_mut(&0x1000).unwrap()[60] ^= 1;
        assert!(image
            .preflight()
            .unwrap_err()
            .contains("native bytes differ"));
    }

    /// A valid-looking generic accessor alone cannot substitute for matching both native calls.
    #[test]
    fn differing_call_target_or_opcode_refuses_whole_set() {
        let mut image = Image::valid();
        image.ranges.get_mut(&0x279f).unwrap()[1] ^= 1;
        assert!(image.preflight().unwrap_err().contains("calls disagree"));
        image = Image::valid();
        image.ranges.get_mut(&0x2730).unwrap()[0] = 0x90;
        assert!(image.preflight().unwrap_err().contains("opcode differs"));
    }

    /// Scanner multiplicity and changed dispatcher bytes fail before any set can be returned.
    #[test]
    fn ambiguous_scan_changed_entry_and_missing_bytes_refuse() {
        let mut image = Image::valid();
        assert!(preflight("1283531-steam", |_| Ok(vec![1, 2]), |_, _| None).is_err());
        image.ranges.get_mut(&0x5000).unwrap()[40] ^= 1;
        assert!(image
            .preflight()
            .unwrap_err()
            .contains("native bytes differ"));
        image = Image::valid();
        image.ranges.remove(&0x1000);
        assert!(image.preflight().unwrap_err().contains("bytes unavailable"));
    }

    /// Unknown builds do not invoke scan/read, and signed-relative overflow is refused.
    #[test]
    fn unknown_layout_and_address_overflow_never_admit_sites() {
        assert!(preflight(
            "unknown",
            |_| panic!("must not scan"),
            |_, _| panic!("must not read")
        )
        .is_err());
        assert_eq!(Layout::Previous.entity_table_offset(), 0x828);
        assert_eq!(Layout::Current.entity_table_offset(), 0x830);
        assert!(follow_call(usize::MAX, 0x730, &mut |_, _| None).is_err());
        assert!(follow_call(1, 0, &mut |_, _| Some(vec![0xe8, 0, 0, 0, 0x80])).is_err());
    }
}
