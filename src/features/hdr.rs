//! HDR composite paper white, applied once at start-up.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use log::Level;

use crate::config::PaperWhiteNits;
use crate::entry;
use crate::patch::{write_checked, PatchError};
use crate::scan::{find_in_sections, main_module, Module, Pattern};
use crate::sigs::HDR_PAPER_WHITE;

/// The game's 250.0f immediate, from the verified instruction.
const ORIGINAL: [u8; 4] = [0x00, 0x00, 0x7a, 0x43];

/// Find the unique HDR store and replace its checked immediate with `nits`.
///
/// # Safety
///
/// `module` must describe mapped, readable memory. Call only at start-up before game
/// threads run this code, with no concurrent writer to the image.
pub unsafe fn apply(module: &Module, nits: f32) -> Result<(), String> {
    // SAFETY: this function's mapped-image and start-up contract is passed to the checked writer.
    unsafe {
        apply_with_writer(module, nits, |site, bytes| {
            write_checked(site, &ORIGINAL, bytes).map(|_| ())
        })
    }
}

/// The checked patch with its writer supplied for a failed-read-back test on an owned image.
///
/// # Safety
/// Same mapped-image and start-up contract as `apply`; `write` must obey `write_checked`'s contract.
unsafe fn apply_with_writer(
    module: &Module,
    nits: f32,
    write: impl FnOnce(usize, &[u8]) -> Result<(), PatchError>,
) -> Result<(), String> {
    if !nits.is_finite() || !(40.0..=400.0).contains(&nits) {
        return Err("skipped: value is not finite or outside 40..=400 nits".to_owned());
    }
    let sig = HDR_PAPER_WHITE;
    let pattern = Pattern::parse(sig.pattern).map_err(|error| format!("skipped: {error}"))?;
    let found = find_in_sections(module, sig.sections, &pattern)
        .map_err(|error| format!("skipped: {}", error.player_text()))?;
    let site = found
        .checked_add(sig.patch_offset())
        .ok_or_else(|| "skipped: patch address overflows".to_owned())?;
    // SAFETY: the unique match contains all four target bytes in readable module memory.
    // The caller ensures start-up only and no concurrent execution or writes. The original
    // and replacement byte arrays live separately from that target memory.
    write(site, &encode(nits)).map_err(|error| match error {
        PatchError::Mismatch { .. } => {
            "skipped: immediate changed by another tool; left unchanged".to_owned()
        }
        PatchError::ReadBack => {
            UNCERTAIN.store(true, Ordering::Release);
            "patch written, but read-back failed; effective value was not published".to_owned()
        }
        other => format!("skipped: cannot patch the immediate: {other}"),
    })
}

/// A failed read-back means the immediate may have changed without a verified value.
static UNCERTAIN: AtomicBool = AtomicBool::new(false);

/// Whether the effective paper-white value is usable by dependent features.
/// A failed read-back leaves the numeric getter unchanged but makes this false.
pub fn paper_white_is_verified() -> bool {
    !UNCERTAIN.load(Ordering::Acquire)
}

/// The last successfully checked paper-white patch, initially the stock 250 nits.
static EFFECTIVE: AtomicU32 = AtomicU32::new(250.0f32.to_bits());

/// Read an effective value published after a successful checked write.
fn read_effective(effective: &AtomicU32) -> f32 {
    f32::from_bits(effective.load(Ordering::Acquire))
}

/// The value this mod successfully patched, or 250 nits if it did not apply a patch.
/// This does not infer what another tool may have written after a mismatch or failure.
pub fn effective_paper_white_nits() -> f32 {
    read_effective(&EFFECTIVE)
}

/// Start-up gate and one-line outcome, with the image writer passed in for owned-page tests.
fn start_up(
    cfg: PaperWhiteNits,
    code_patches: bool,
    effective: &AtomicU32,
    apply: impl FnOnce(f32) -> Result<(), String>,
) -> (Level, String) {
    let PaperWhiteNits::Nits(nits) = cfg else {
        return (
            Level::Info,
            "hdr paper white: game value (setting)".to_owned(),
        );
    };
    if !code_patches {
        return (
            Level::Info,
            "hdr paper white: skipped: code patches are disabled for this build".to_owned(),
        );
    }
    match apply(nits) {
        Ok(()) => {
            effective.store(nits.to_bits(), Ordering::Release);
            (
                Level::Info,
                format!("hdr paper white: {nits} nits (was 250)"),
            )
        }
        Err(why) => (Level::Warn, format!("hdr paper white: {why}")),
    }
}

/// Run once from `entry::start`, before the game's threads execute the patched instruction.
pub fn init(cfg: &PaperWhiteNits) {
    let (level, line) = start_up(*cfg, entry::code_patches_allowed(), &EFFECTIVE, |nits| {
        // SAFETY: entry calls this once before the game's threads run the patched code.
        // The gate permits code patches and main_module describes the mapped, readable exe.
        unsafe { apply(&main_module(), nits) }
    });
    log::log!(level, "{line}");
}

/// The setting's IEEE-754 bits, in the byte order of the game's immediate.
pub fn encode(nits: f32) -> [u8; 4] {
    nits.to_le_bytes()
}

/// Simulate a writer that changed the immediate but returned the structured read-back failure.
/// Only the fresh-child uncertainty test calls this helper.
#[cfg(test)]
pub(crate) fn test_fail_readback_after_write() {
    let (mem, module) = tests::image();
    mem.put(0x1100, tests::CODE);
    let (level, _) = start_up(PaperWhiteNits::Nits(80.0), true, &EFFECTIVE, |nits| {
        // SAFETY: the image is owned, readable memory; the callback uses the checked writer
        // on that image, then injects its typed failure after the bytes changed.
        unsafe {
            apply_with_writer(&module, nits, |site, bytes| {
                write_checked(site, &ORIGINAL, bytes)?;
                Err(PatchError::ReadBack)
            })
        }
    });
    assert_eq!(level, Level::Warn);
    assert_eq!(mem.get(0x1103, 4), [0x00, 0x00, 0xa0, 0x42]);
    assert_eq!(effective_paper_white_nits(), 250.0);
}

/// Publish 80 nits through the real checked-write start-up path, on an owned test image.
/// Only fresh-child runtime bridge tests call this helper.
#[cfg(test)]
pub(crate) fn test_publish_checked_eighty() {
    let (mem, module) = tests::image();
    mem.put(0x1100, tests::CODE);
    let (level, _) = start_up(PaperWhiteNits::Nits(80.0), true, &EFFECTIVE, |nits| {
        // SAFETY: this test owns the readable image and no thread executes its bytes.
        unsafe { apply(&module, nits) }
    });
    assert_eq!(level, Level::Info);
    assert_eq!(mem.get(0x1103, 4), [0x00, 0x00, 0xa0, 0x42]);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::patch::test_support::Mem;
    use crate::scan::Module;

    #[test]
    fn encodes_eighty_nits_as_little_endian_f32() {
        assert_eq!(encode(80.0), [0x00, 0x00, 0xa0, 0x42]);
    }

    /// A synthetic PE header; no game executable is read.
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
    pub(super) fn image() -> (Mem, Module) {
        let mem = Mem::rw(0x4000);
        mem.put(0, &text_only_header());
        let module = Module {
            base: mem.base,
            size: 0x4000,
        };
        (mem, module)
    }

    /// Verified instruction and following context from `docs/re-notes.md`.
    pub(super) const CODE: &[u8] = &[
        0xc7, 0x45, 0x88, 0x00, 0x00, 0x7a, 0x43, 0x49, 0x8b, 0x16, 0xc7, 0x45, 0x8c, 0x9a, 0x99,
        0x19, 0x3f,
    ];

    #[test]
    fn apply_patches_only_the_four_byte_immediate_in_a_copy() {
        let (mem, module) = image();
        mem.put(0x1100, CODE);
        let before = mem.get(0, 0x4000);
        // SAFETY: this image is owned readable memory; no thread executes its bytes.
        assert_eq!(unsafe { apply(&module, 80.0) }, Ok(()));
        let mut expected = before;
        expected[0x1103..0x1107].copy_from_slice(&[0x00, 0x00, 0xa0, 0x42]);
        assert_eq!(mem.get(0, 0x4000), expected);
    }

    #[test]
    fn startup_gate_keeps_game_value_and_never_touches_the_image() {
        for (cfg, allowed, line) in [
            (
                PaperWhiteNits::Game,
                true,
                "hdr paper white: game value (setting)",
            ),
            (
                PaperWhiteNits::Nits(80.0),
                false,
                "hdr paper white: skipped: code patches are disabled for this build",
            ),
        ] {
            let effective = AtomicU32::new(250.0f32.to_bits());
            let got = start_up(cfg, allowed, &effective, |_| {
                panic!("skip must not scan or write")
            });
            assert_eq!(got, (Level::Info, line.to_owned()));
            assert_eq!(read_effective(&effective), 250.0);
        }
    }

    #[test]
    fn startup_publishes_eighty_only_after_the_checked_copy_patch_succeeds() {
        let (mem, module) = image();
        mem.put(0x1100, CODE);
        let effective = AtomicU32::new(250.0f32.to_bits());
        let got = start_up(PaperWhiteNits::Nits(80.0), true, &effective, |nits| {
            assert_eq!(
                read_effective(&effective),
                250.0,
                "not published before the write"
            );
            // SAFETY: an owned image whose bytes no thread executes.
            unsafe { apply(&module, nits) }
        });
        assert_eq!(
            got,
            (Level::Info, "hdr paper white: 80 nits (was 250)".to_owned())
        );
        assert_eq!(mem.get(0x1103, 4), [0x00, 0x00, 0xa0, 0x42]);
        assert_eq!(read_effective(&effective), 80.0);
    }

    #[test]
    fn changed_immediate_is_left_alone_and_effective_stays_game() {
        let (mem, module) = image();
        mem.put(0x1100, CODE);
        mem.put(0x1103, &[0x00, 0x00, 0x48, 0x43]); // another tool's 200.0f
        let before = mem.get(0, 0x4000);
        let effective = AtomicU32::new(250.0f32.to_bits());
        let got = start_up(PaperWhiteNits::Nits(80.0), true, &effective, |nits| {
            // SAFETY: an owned image whose bytes no thread executes.
            unsafe { apply(&module, nits) }
        });
        assert_eq!(
            got,
            (
                Level::Warn,
                "hdr paper white: skipped: immediate changed by another tool; left unchanged"
                    .to_owned()
            )
        );
        assert_eq!(mem.get(0, 0x4000), before);
        assert_eq!(read_effective(&effective), 250.0);
    }

    #[test]
    fn missing_or_ambiguous_signature_writes_nothing_and_keeps_game_effective() {
        for copies in [0, 2] {
            let (mem, module) = image();
            for i in 0..copies {
                mem.put(0x1100 + i * 0x100, CODE);
            }
            let before = mem.get(0, 0x4000);
            let effective = AtomicU32::new(250.0f32.to_bits());
            let (level, line) = start_up(PaperWhiteNits::Nits(80.0), true, &effective, |nits| {
                // SAFETY: an owned image whose bytes no thread executes.
                unsafe { apply(&module, nits) }
            });
            assert_eq!(level, Level::Warn);
            if copies == 0 {
                assert!(line.contains("game code was not found"), "{line}");
            } else {
                assert!(line.contains("matched 2 times"), "{line}");
            }
            assert!(!line.contains('\n'));
            assert_eq!(mem.get(0, 0x4000), before);
            assert_eq!(read_effective(&effective), 250.0);
        }
    }

    #[test]
    fn invalid_direct_values_cannot_patch_or_be_published() {
        for nits in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, 39.0, 401.0] {
            let (mem, module) = image();
            mem.put(0x1100, CODE);
            let before = mem.get(0, 0x4000);
            let effective = AtomicU32::new(250.0f32.to_bits());
            let (level, line) = start_up(PaperWhiteNits::Nits(nits), true, &effective, |nits| {
                // SAFETY: an owned image whose bytes no thread executes.
                unsafe { apply(&module, nits) }
            });
            assert_eq!(level, Level::Warn);
            assert!(line.contains("not finite or outside 40..=400"));
            assert_eq!(mem.get(0, 0x4000), before);
            assert_eq!(read_effective(&effective), 250.0);
        }
    }

    #[test]
    fn failed_checked_write_never_publishes_the_configured_value() {
        let effective = AtomicU32::new(250.0f32.to_bits());
        let got = start_up(PaperWhiteNits::Nits(80.0), true, &effective, |_| {
            Err("patch written, but read-back failed; effective value was not published".to_owned())
        });
        assert_eq!(got.0, Level::Warn);
        assert_eq!(read_effective(&effective), 250.0);
    }

    #[test]
    fn hdr_bytes_outside_text_are_not_a_patch_site() {
        let (mem, module) = image();
        mem.put(0x2100, CODE);
        let before = mem.get(0, 0x4000);
        // SAFETY: an owned image whose bytes no thread executes.
        assert!(unsafe { apply(&module, 80.0) }.is_err());
        assert_eq!(mem.get(0, 0x4000), before);
    }
}
