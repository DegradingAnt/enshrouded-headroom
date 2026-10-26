//! The VRAM-sized texture streaming budget.
//!
//! The game keeps a table of five rows in `.rdata`: a VRAM threshold and the texture streaming
//! budget for it, `u64` pairs, 80 bytes. The last row's budget is 2 GiB, which is what every card
//! with 10 GiB or more gets, however much more it has. This feature raises that one `u64` to a
//! size that fits the card (see [`budget_bytes`]).
//!
//! # Steps
//!
//! 1. [`init`], from `entry::start`: [`prepare`] finds the table in `.rdata`, checks it and keeps
//!    where the top value is. The table must be present exactly once, in full, with the original
//!    2 GiB top value; anything else is logged and the feature stays off.
//! 2. [`apply`], later, from `vk::wrap_create_device` (Task 9): the device is created, so the
//!    card's memory heaps are known. [`budget_bytes`] picks the size and `apply` writes it.
//!
//! This is a data patch, not a code patch. So it does not wait for `entry::code_patches_allowed`:
//! on an unknown build it still runs when the whole 80-byte table is found, since a table that
//! matches all 80 bytes is the table whatever the code around it looks like.
//!
//! # Timing
//!
//! The game reads the table about 4.6 s after the Vulkan device exists (game log, build
//! f68140a3: device created at 4.708 s, `Setting texture streaming budget` at 9.363 s). The write
//! in `wrap_create_device` is therefore in time, and no thread reads the value while it is
//! written.
//!
//! # Other tools
//!
//! The 2026-10-23 launcher patched this same value to 6 GiB. With it still in place [`prepare`]
//! finds a top value that is not the original and returns [`TbError::ChangedByOtherTool`]; the
//! feature stays off and says so, instead of writing over the other tool's work.

use std::fmt;
use std::sync::OnceLock;

use log::Level;

use crate::config::TextureBudget;
use crate::patch::{read_checked, write_checked, PatchError};
use crate::scan::{find_in_sections, find_unique, main_module, Module, Pattern, ScanError};

/// The game's table: `(VRAM threshold, texture streaming budget)`, five rows, in bytes. The
/// budget of the last row is the one this feature changes.
pub const TABLE: [(u64, u64); 5] = [
    (0, 0x12C00000),
    (0x11F400000, 0x25800000),
    (0x180000000, 0x40000000),
    (0x200000000, 0x5F400000),
    (0x280000000, 0x80000000),
];

const GIB: u64 = 1 << 30;
/// The table in memory: five pairs of `u64`.
const TABLE_LEN: usize = 80;
/// The table without its top value: the five thresholds and the first four budgets. A table is
/// recognised by these bytes (see [`prepare_in`]).
const PREFIX_LEN: usize = 72;
/// The top value as the game ships it: 2 GiB.
const ORIGINAL_TOP: u64 = 0x8000_0000;
/// The least and the most this feature sets, in bytes.
const MIN_BYTES: u64 = 2 * GIB;
const MAX_BYTES: u64 = 8 * GIB;

/// The table as it sits in memory: each `(threshold, budget)` pair, little-endian.
pub fn table_bytes() -> [u8; TABLE_LEN] {
    let mut out = [0u8; TABLE_LEN];
    let words = TABLE
        .iter()
        .flat_map(|&(threshold, budget)| [threshold, budget]);
    let bytes = words.flat_map(u64::to_le_bytes);
    for (slot, byte) in out.iter_mut().zip(bytes) {
        *slot = byte;
    }
    out
}

/// The budget in bytes to write, or `None` to leave the game's alone.
///
/// - An integrated GPU gives `None` in every mode. Its "device-local" heap may be shared system
///   memory.
/// - Mode `Off` gives `None`.
/// - Mode `Auto` gives the card's device-local heap minus `reserve_gib`, clamped to 2..=8 GiB, or
///   `None` if that is no more than the game's own 2 GiB. A negative reserve counts as zero and
///   a huge one leaves nothing; a NaN reserve is not a number, so `None`.
/// - Mode `Gib(x)` gives `x` GiB, clamped to 2..=8, or `None` for NaN.
///
/// Never panics, whatever the input.
pub fn budget_bytes(
    device_local: u64,
    reserve_gib: f64,
    integrated: bool,
    mode: &TextureBudget,
) -> Option<u64> {
    if integrated {
        return None;
    }
    match *mode {
        TextureBudget::Off => None,
        TextureBudget::Auto => {
            let reserve = gib_to_bytes(reserve_gib)?;
            let budget = device_local
                .saturating_sub(reserve)
                .clamp(MIN_BYTES, MAX_BYTES);
            (budget > MIN_BYTES).then_some(budget)
        }
        TextureBudget::Gib(gib) => Some(gib_to_bytes(gib)?.clamp(MIN_BYTES, MAX_BYTES)),
    }
}

/// `gib` GiB in bytes. A negative number is 0 and one too big for a `u64` is `u64::MAX` (a
/// float-to-integer `as` cast saturates). NaN is `None`.
fn gib_to_bytes(gib: f64) -> Option<u64> {
    (!gib.is_nan()).then_some((gib * GIB as f64) as u64)
}

/// Why the table could not be used.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TbError {
    /// The table is not in `.rdata` (a different build, or not Enshrouded), or `.rdata` or the
    /// table could not be read. The underlying error is in the debug log.
    NotFound,
    /// The table is in `.rdata` this many times. Which one the game reads is not known.
    Multiple(usize),
    /// The table is there once, but its top value is not the original 2 GiB: another tool has
    /// already changed it.
    ChangedByOtherTool,
}

impl fmt::Display for TbError {
    /// The texts hold no parentheses, so that `texture budget: off: <text>` has no nested ones.
    /// The words "changed by another tool" are the spec's (design.md); Task 17 looks for them in
    /// the log.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TbError::NotFound => f.write_str(
                "the texture budget table was not found in .rdata (or .rdata could not be read)",
            ),
            TbError::Multiple(n) => write!(
                f,
                "{n} copies of the texture budget table in .rdata, expected exactly one"
            ),
            TbError::ChangedByOtherTool => write!(
                f,
                "the top value is not the original {} GiB: changed by another tool, such as the \
                 old launcher or another plugin",
                ORIGINAL_TOP / GIB
            ),
        }
    }
}

impl std::error::Error for TbError {}

/// Fold a scan failure into a [`TbError`]. A failure that carries more than `NotFound` can say
/// (no match, an unreadable section, a pattern that does not parse) is noted at debug level
/// first, so the detail is not lost.
fn from_scan(error: ScanError) -> TbError {
    match error {
        ScanError::Multiple(n) => TbError::Multiple(n),
        ScanError::NotFound | ScanError::BadPattern(_) => {
            log::debug!("texture budget: scan failed: {error}");
            TbError::NotFound
        }
    }
}

/// The 72 bytes that identify the table, then 8 wildcards for the top value. A match therefore
/// has all 80 bytes of the table inside the searched memory, and a changed top value does not
/// hide a second copy.
fn table_pattern() -> Result<Pattern, TbError> {
    let tokens: Vec<String> = table_bytes()
        .iter()
        .enumerate()
        .map(|(i, byte)| {
            if i < PREFIX_LEN {
                format!("{byte:02x}")
            } else {
                "??".to_owned()
            }
        })
        .collect();
    Pattern::parse(&tokens.join(" ")).map_err(from_scan)
}

/// `table` is the 80 bytes of a table whose first 72 bytes matched. Is its top value the
/// original?
fn check_top(table: &[u8]) -> Result<(), TbError> {
    if table.get(PREFIX_LEN..TABLE_LEN) == Some(ORIGINAL_TOP.to_le_bytes().as_slice()) {
        Ok(())
    } else {
        Err(TbError::ChangedByOtherTool)
    }
}

/// The pure part of [`prepare`]: the offset of the table in `hay`.
///
/// The table is recognised by its first 72 bytes (the five thresholds and the first four
/// budgets). Those must occur exactly once ([`TbError::NotFound`] for none, [`TbError::Multiple`]
/// for more) and have 8 bytes after them; so a changed top value cannot hide a second table.
/// Then the top value (bytes 72..80) must be the original, or the result is
/// [`TbError::ChangedByOtherTool`].
pub fn prepare_in(hay: &[u8]) -> Result<usize, TbError> {
    let at = find_unique(hay, &table_pattern()?).map_err(from_scan)?;
    let end = at.checked_add(TABLE_LEN).ok_or(TbError::NotFound)?;
    check_top(hay.get(at..end).ok_or(TbError::NotFound)?)?;
    Ok(at)
}

/// Where the table's top value is, once [`prepare`] has checked the table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Prepared {
    top_value_addr: usize,
}

impl Prepared {
    /// Address of the top value (the last 8 bytes of the table).
    pub fn top_value_addr(&self) -> usize {
        self.top_value_addr
    }

    /// Address of the table.
    pub fn table_addr(&self) -> usize {
        self.top_value_addr.saturating_sub(PREFIX_LEN)
    }
}

/// Find the table in `.rdata` of `m` and check it: see [`prepare_in`] for the rules.
///
/// Start-up only, because it scans live image memory with [`find_in_sections`] and reads it with
/// `patch::read_checked`; see the rule on both. Call it from `entry::start`.
pub fn prepare(m: &Module) -> Result<Prepared, TbError> {
    let at = find_in_sections(m, &[".rdata"], &table_pattern()?).map_err(from_scan)?;
    // SAFETY: start-up only (see above), so nothing else changes or unmaps the memory between
    // the check inside `read_checked` and the read. The scan has just matched 80 bytes at `at`
    // inside `.rdata`.
    let table = unsafe { read_checked(at, TABLE_LEN) }.map_err(|error| {
        log::debug!("texture budget: cannot read the table at {at:#x}: {error}");
        TbError::NotFound
    })?;
    check_top(&table)?;
    let top_value_addr = at.checked_add(PREFIX_LEN).ok_or(TbError::NotFound)?;
    Ok(Prepared { top_value_addr })
}

/// Write `bytes` over the original top value.
///
/// This is a data write to `.rdata`, and it is the one patch of this mod that is allowed after
/// start-up (Task 5's rule for data nothing is using): the call site in `vk::wrap_create_device`
/// runs while the game's threads run, and says so. The game reads the value about 4.6 s later
/// (see the module docs), so no thread is using it now.
///
/// `bytes` equal to the original 2 GiB (an explicit `TextureBudget = 2`, say) is `Ok` without any
/// access to the table: there is nothing to change, so the page is not made writable for a moment
/// to write the value that is already there. [`outcome`] says "unchanged" for that.
///
/// Otherwise `write_checked` writes only if memory still holds the original 2 GiB. So a second
/// call, or a value changed by another tool since [`prepare`], gives [`PatchError::Mismatch`] and
/// changes nothing; [`outcome`] words that for the log.
///
/// # Safety
///
/// `p` must come from [`prepare`] in this process (the field is private, so it can come from
/// nowhere else), and the caller must run before the game reads the table, as the module docs
/// describe. Then the 8 bytes are data that no thread is using.
pub unsafe fn apply(p: &Prepared, bytes: u64) -> Result<(), PatchError> {
    if bytes == ORIGINAL_TOP {
        return Ok(());
    }
    // SAFETY: `p.top_value_addr` is the top value of the table that `prepare` matched in the
    // game's `.rdata`. It is data, not code, and by the contract above nothing reads it yet.
    // `expected` and `new` are temporaries on this stack frame, not the target.
    unsafe {
        write_checked(
            p.top_value_addr,
            &ORIGINAL_TOP.to_le_bytes(),
            &bytes.to_le_bytes(),
        )
    }
    .map(|_| ())
}

/// `bytes` as GiB with one decimal, for the log.
fn gib_text(bytes: u64) -> String {
    format!("{:.1} GiB", bytes as f64 / GIB as f64)
}

/// The log level and line for the result of [`apply`], with the numbers behind it.
///
/// `mode`, `device_local` and `reserve_gib` are what `budget_bytes` was asked; `bytes` is what it
/// answered and `apply` was given.
///
/// - Success is `texture budget: 2.0 GiB -> 4.6 GiB (device-local 15.6 GiB, reserve 11 GiB)`. The
///   reserve is named only for `Auto`, where it plays a part; an explicit size leaves it out.
/// - `bytes` equal to the original 2 GiB is `texture budget: unchanged (the budget is already 2
///   GiB)`, since `apply` wrote nothing.
/// - A `Mismatch` is information, not a failure: it is what a second call finds (the value is
///   already changed), and the line says what the table holds now.
/// - `ReadBack` is a warning that says the bytes were written, since they were.
/// - Any other error is a warning that nothing was applied.
pub fn outcome(
    result: &Result<(), PatchError>,
    bytes: u64,
    device_local: u64,
    reserve_gib: f64,
    mode: &TextureBudget,
) -> (Level, String) {
    match result {
        Ok(()) if bytes == ORIGINAL_TOP => (
            Level::Info,
            format!(
                "texture budget: unchanged (the budget is already {} GiB)",
                ORIGINAL_TOP / GIB
            ),
        ),
        Ok(()) => {
            let reserve = if matches!(mode, TextureBudget::Auto) {
                format!(", reserve {reserve_gib} GiB")
            } else {
                String::new()
            };
            (
                Level::Info,
                format!(
                    "texture budget: {} -> {} (device-local {}{reserve})",
                    gib_text(ORIGINAL_TOP),
                    gib_text(bytes),
                    gib_text(device_local)
                ),
            )
        }
        Err(PatchError::Mismatch { found, .. }) => {
            let now = <[u8; 8]>::try_from(found.as_slice())
                .map(u64::from_le_bytes)
                .map_or_else(
                    |_| "something else".to_owned(),
                    |value| format!("{value:#x} ({})", gib_text(value)),
                );
            (
                Level::Info,
                format!(
                    "texture budget: left as it is, the table already holds {now} instead of the \
                     original {}",
                    gib_text(ORIGINAL_TOP)
                ),
            )
        }
        Err(PatchError::ReadBack) => (
            Level::Warn,
            format!(
                "texture budget: {} written, but the read-back did not match",
                gib_text(bytes)
            ),
        ),
        Err(error) => (
            Level::Warn,
            format!("texture budget: not applied ({error})"),
        ),
    }
}

/// What [`init`] found, kept for [`prepared`].
static PREPARED: OnceLock<Prepared> = OnceLock::new();

/// The table [`init`] found and checked, or `None` if it did not (or has not run).
pub fn prepared() -> Option<&'static Prepared> {
    PREPARED.get()
}

/// The start-up step: look for the table and say what came of it. Called once, from
/// `entry::start`, so it may scan the exe (see [`prepare`]).
pub fn init(mode: &TextureBudget) {
    let (level, line) = start_up(mode, || prepare(&main_module()), &PREPARED);
    log::log!(level, "{line}");
}

/// The body of [`init`], with the search and the cell passed in. With `TextureBudget = off`
/// nothing is searched. On success the table is stored in `cell`.
fn start_up(
    mode: &TextureBudget,
    search: impl FnOnce() -> Result<Prepared, TbError>,
    cell: &OnceLock<Prepared>,
) -> (Level, String) {
    if matches!(mode, TextureBudget::Off) {
        return (
            Level::Info,
            "texture budget: off: TextureBudget = off in the settings file".to_owned(),
        );
    }
    match search() {
        Ok(prepared) => {
            // `entry::start` runs once, so the cell is empty; an `Err` could only mean the
            // table was stored already.
            let _ = cell.set(prepared);
            (
                Level::Info,
                format!(
                    "texture budget: table at {:#x} (top {} GiB)",
                    prepared.table_addr(),
                    ORIGINAL_TOP / GIB
                ),
            )
        }
        Err(error) => (Level::Warn, format!("texture budget: off: {error}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::patch::test_support::{protect_of, Mem};
    use windows_sys::Win32::System::Memory::PAGE_READONLY;

    // ---- the values the plan gives verbatim ---------------------------------------------------

    #[test]
    fn rtx5080_gets_4_6_gib() {
        assert_eq!(
            budget_bytes(16_753_098_752, 11.0, false, &TextureBudget::Auto),
            Some(16_753_098_752 - 11 * GIB)
        );
    }

    #[test]
    fn twelve_gb_unchanged() {
        assert_eq!(
            budget_bytes(12 * GIB, 11.0, false, &TextureBudget::Auto),
            None
        );
    }

    #[test]
    fn twentyfour_gb_capped_at_8() {
        assert_eq!(
            budget_bytes(24 * GIB, 11.0, false, &TextureBudget::Auto),
            Some(8 * GIB)
        );
    }

    #[test]
    fn integrated_gpu_unchanged() {
        assert_eq!(
            budget_bytes(32 * GIB, 11.0, true, &TextureBudget::Gib(6.0)),
            None
        );
    }

    #[test]
    fn explicit_gib_clamped() {
        assert_eq!(
            budget_bytes(16 * GIB, 11.0, false, &TextureBudget::Gib(20.0)),
            Some(8 * GIB)
        );
    }

    #[test]
    fn prepare_refuses_changed_row() {
        let mut t = table_bytes();
        t[72..80].copy_from_slice(&(6 * GIB).to_le_bytes());
        assert!(matches!(prepare_in(&t), Err(TbError::ChangedByOtherTool)));
    }

    // ---- the table ------------------------------------------------------------------------------

    #[test]
    fn table_bytes_are_little_endian_pairs() {
        let t = table_bytes();
        assert_eq!(t.len(), 80);
        assert_eq!(t[0..8], 0u64.to_le_bytes());
        assert_eq!(t[8..16], 0x12C0_0000u64.to_le_bytes());
        assert_eq!(t[16..24], 0x1_1F40_0000u64.to_le_bytes());
        assert_eq!(t[24..32], 0x2580_0000u64.to_le_bytes());
        assert_eq!(t[64..72], (10 * GIB).to_le_bytes());
        assert_eq!(t[72..80], (2 * GIB).to_le_bytes());
        assert_eq!(t[72..80], ORIGINAL_TOP.to_le_bytes());
    }

    // ---- budget_bytes ---------------------------------------------------------------------------

    #[test]
    fn auto_boundaries() {
        let auto = TextureBudget::Auto;
        // Exactly 2 GiB left over is "no change"; one byte more is a change.
        assert_eq!(budget_bytes(13 * GIB, 11.0, false, &auto), None);
        assert_eq!(
            budget_bytes(13 * GIB + 1, 11.0, false, &auto),
            Some(2 * GIB + 1)
        );
        // Exactly 8 GiB left over, and well beyond it.
        assert_eq!(budget_bytes(19 * GIB, 11.0, false, &auto), Some(8 * GIB));
        assert_eq!(budget_bytes(64 * GIB, 11.0, false, &auto), Some(8 * GIB));
        // A card smaller than the reserve.
        assert_eq!(budget_bytes(4 * GIB, 11.0, false, &auto), None);
        assert_eq!(budget_bytes(0, 11.0, false, &auto), None);
    }

    #[test]
    fn cards_of_8_and_10_gb_are_unchanged_with_the_default_reserve() {
        // Device-local heaps in MiB: about 7.6 GiB and 9.6 GiB (a card's usable share of its
        // memory), and the full 8 GiB and 10 GiB. All are under the 13 GiB that the default
        // 11 GiB reserve plus the game's own 2 GiB need.
        let mib = |n: u64| n * 1024 * 1024;
        for (card, heap) in [
            ("8 GB, usable", mib(7_782)),
            ("8 GB, full", mib(8 * 1024)),
            ("10 GB, usable", mib(9_830)),
            ("10 GB, full", mib(10 * 1024)),
        ] {
            assert_eq!(
                budget_bytes(heap, 11.0, false, &TextureBudget::Auto),
                None,
                "{card}"
            );
        }
    }

    #[test]
    fn integrated_gpu_is_unchanged_in_every_mode() {
        for mode in [
            TextureBudget::Auto,
            TextureBudget::Off,
            TextureBudget::Gib(6.0),
        ] {
            assert_eq!(budget_bytes(32 * GIB, 11.0, true, &mode), None, "{mode:?}");
        }
    }

    #[test]
    fn off_is_unchanged() {
        assert_eq!(
            budget_bytes(24 * GIB, 11.0, false, &TextureBudget::Off),
            None
        );
    }

    #[test]
    fn explicit_gib_is_exact_inside_the_range_and_clamped_outside_it() {
        let gib = |x| budget_bytes(16 * GIB, 11.0, false, &TextureBudget::Gib(x));
        assert_eq!(gib(6.0), Some(6 * GIB));
        assert_eq!(gib(2.0), Some(2 * GIB));
        assert_eq!(gib(8.0), Some(8 * GIB));
        assert_eq!(gib(2.5), Some(2 * GIB + GIB / 2));
        assert_eq!(gib(1.0), Some(2 * GIB));
        assert_eq!(gib(0.0), Some(2 * GIB));
        assert_eq!(gib(-3.0), Some(2 * GIB));
        assert_eq!(gib(9.0), Some(8 * GIB));
        // An explicit size does not depend on the card or the reserve.
        assert_eq!(
            budget_bytes(0, f64::NAN, false, &TextureBudget::Gib(5.0)),
            Some(5 * GIB)
        );
    }

    #[test]
    fn explicit_gib_with_extreme_values_clamps_and_does_not_panic() {
        let gib = |x| budget_bytes(16 * GIB, 11.0, false, &TextureBudget::Gib(x));
        assert_eq!(gib(f64::INFINITY), Some(8 * GIB));
        assert_eq!(gib(f64::MAX), Some(8 * GIB));
        assert_eq!(gib(f64::NEG_INFINITY), Some(2 * GIB));
        assert_eq!(gib(f64::MIN), Some(2 * GIB));
        // NaN has no meaning: leave the game's budget alone.
        assert_eq!(gib(f64::NAN), None);
    }

    #[test]
    fn nan_reserve_leaves_the_budget_alone() {
        assert_eq!(
            budget_bytes(24 * GIB, f64::NAN, false, &TextureBudget::Auto),
            None
        );
    }

    #[test]
    fn negative_reserve_counts_as_no_reserve() {
        let auto = TextureBudget::Auto;
        assert_eq!(budget_bytes(6 * GIB, -4.0, false, &auto), Some(6 * GIB));
        assert_eq!(budget_bytes(6 * GIB, 0.0, false, &auto), Some(6 * GIB));
        assert_eq!(
            budget_bytes(6 * GIB, f64::NEG_INFINITY, false, &auto),
            Some(6 * GIB)
        );
        assert_eq!(
            budget_bytes(24 * GIB, f64::MIN, false, &auto),
            Some(8 * GIB)
        );
    }

    #[test]
    fn huge_reserve_leaves_the_budget_alone() {
        let auto = TextureBudget::Auto;
        assert_eq!(budget_bytes(24 * GIB, 1e18, false, &auto), None);
        assert_eq!(budget_bytes(24 * GIB, f64::MAX, false, &auto), None);
        assert_eq!(budget_bytes(24 * GIB, f64::INFINITY, false, &auto), None);
        assert_eq!(budget_bytes(u64::MAX, f64::INFINITY, false, &auto), None);
    }

    #[test]
    fn the_largest_device_heap_is_capped() {
        let auto = TextureBudget::Auto;
        assert_eq!(budget_bytes(u64::MAX, 11.0, false, &auto), Some(8 * GIB));
        assert_eq!(budget_bytes(u64::MAX, 0.0, false, &auto), Some(8 * GIB));
    }

    // ---- prepare_in: the pure search --------------------------------------------------------------

    /// `lead` bytes of non-zero filler, then the table, then `tail` bytes of filler.
    fn hay_with_table(lead: usize, tail: usize) -> Vec<u8> {
        let mut hay = vec![0xAB; lead];
        hay.extend_from_slice(&table_bytes());
        hay.extend(std::iter::repeat_n(0xCD, tail));
        hay
    }

    #[test]
    fn prepare_in_returns_the_offset_of_the_table() {
        assert_eq!(prepare_in(&hay_with_table(0, 0)), Ok(0));
        assert_eq!(prepare_in(&hay_with_table(37, 0)), Ok(37));
        assert_eq!(prepare_in(&hay_with_table(8, 500)), Ok(8));
    }

    #[test]
    fn prepare_in_finds_nothing_in_other_data() {
        assert_eq!(prepare_in(&[]), Err(TbError::NotFound));
        assert_eq!(prepare_in(&[0u8; 4096]), Err(TbError::NotFound));
        let short = table_bytes();
        assert_eq!(prepare_in(&short[..79]), Err(TbError::NotFound));
        assert_eq!(prepare_in(&short[..72]), Err(TbError::NotFound));
    }

    #[test]
    fn prepare_in_needs_the_whole_table_to_fit() {
        // The 72-byte prefix at the very end, with fewer than 8 bytes after it: no top value to
        // check, so this is not a table.
        let mut hay = vec![0xAB; 16];
        hay.extend_from_slice(&table_bytes()[..75]);
        assert_eq!(prepare_in(&hay), Err(TbError::NotFound));
    }

    #[test]
    fn prepare_in_refuses_two_tables() {
        let mut hay = hay_with_table(0, 20);
        hay.extend_from_slice(&table_bytes());
        assert_eq!(prepare_in(&hay), Err(TbError::Multiple(2)));
    }

    #[test]
    fn a_changed_top_value_cannot_hide_a_second_table() {
        let mut changed = table_bytes();
        changed[72..80].copy_from_slice(&(6 * GIB).to_le_bytes());
        let mut hay = hay_with_table(0, 20);
        hay.extend_from_slice(&changed);
        assert_eq!(prepare_in(&hay), Err(TbError::Multiple(2)));
    }

    #[test]
    fn any_top_value_but_the_original_is_a_change_by_another_tool() {
        for top in [
            0u64,
            1,
            6 * GIB,
            8 * GIB,
            ORIGINAL_TOP - 1,
            ORIGINAL_TOP + 1,
        ] {
            let mut hay = hay_with_table(24, 24);
            hay[24 + 72..24 + 80].copy_from_slice(&top.to_le_bytes());
            assert_eq!(
                prepare_in(&hay),
                Err(TbError::ChangedByOtherTool),
                "top {top:#x}"
            );
        }
    }

    #[test]
    fn a_change_in_the_first_72_bytes_is_not_our_table() {
        // Only the top value may differ; a different threshold means a different table.
        for at in [0usize, 8, 16, 40, 64, 71] {
            let mut hay = hay_with_table(0, 0);
            hay[at] ^= 0x01;
            assert_eq!(prepare_in(&hay), Err(TbError::NotFound), "byte {at}");
        }
    }

    // ---- prepare and apply on a synthetic image ---------------------------------------------------

    const IMAGE_SIZE: usize = 0x4000;
    const TEXT_RVA: usize = 0x1000;
    const RDATA_RVA: usize = 0x2000;

    /// PE32+ headers with a `.text` (page 1) and a `.rdata` (page 2) section.
    fn pe_headers() -> Vec<u8> {
        let mut b = vec![0u8; 0x400];
        let mut set = |off: usize, bytes: &[u8]| b[off..off + bytes.len()].copy_from_slice(bytes);
        set(0, b"MZ");
        set(0x3c, &0x80u32.to_le_bytes());
        set(0x80, b"PE\0\0");
        let file_header = 0x84;
        set(file_header + 2, &2u16.to_le_bytes());
        set(file_header + 16, &240u16.to_le_bytes());
        let optional = file_header + 20;
        set(optional, &0x20bu16.to_le_bytes());
        set(optional + 24, &0x1_4000_0000u64.to_le_bytes());
        set(optional + 56, &(IMAGE_SIZE as u32).to_le_bytes());
        let table = optional + 240;
        for (i, (name, rva)) in [(".text", TEXT_RVA), (".rdata", RDATA_RVA)]
            .into_iter()
            .enumerate()
        {
            let at = table + i * 40;
            set(at, name.as_bytes());
            set(at + 8, &0x1000u32.to_le_bytes());
            set(at + 12, &(rva as u32).to_le_bytes());
            set(at + 16, &0x1000u32.to_le_bytes());
        }
        b
    }

    /// A 4-page image in committed read-write memory.
    fn image() -> Mem {
        let mem = Mem::rw(IMAGE_SIZE);
        mem.put(0, &pe_headers());
        mem
    }

    fn module(mem: &Mem) -> Module {
        Module {
            base: mem.base,
            size: IMAGE_SIZE,
        }
    }

    #[test]
    fn prepare_finds_the_table_in_rdata() {
        let mem = image();
        mem.put(RDATA_RVA + 0x100, &table_bytes());
        let p = prepare(&module(&mem)).unwrap();
        assert_eq!(p.table_addr(), mem.base + RDATA_RVA + 0x100);
        assert_eq!(p.top_value_addr(), mem.base + RDATA_RVA + 0x100 + 72);
    }

    #[test]
    fn prepare_finds_nothing_in_an_image_without_the_table() {
        let mem = image();
        assert_eq!(prepare(&module(&mem)), Err(TbError::NotFound));
        // A copy outside .rdata does not count.
        mem.put(TEXT_RVA + 0x100, &table_bytes());
        assert_eq!(prepare(&module(&mem)), Err(TbError::NotFound));
    }

    #[test]
    fn prepare_refuses_two_tables_in_rdata() {
        let mem = image();
        mem.put(RDATA_RVA + 0x100, &table_bytes());
        mem.put(RDATA_RVA + 0x200, &table_bytes());
        assert_eq!(prepare(&module(&mem)), Err(TbError::Multiple(2)));
    }

    #[test]
    fn prepare_refuses_a_table_another_tool_changed() {
        let mem = image();
        mem.put(RDATA_RVA + 0x100, &table_bytes());
        mem.put(RDATA_RVA + 0x100 + 72, &(6 * GIB).to_le_bytes());
        assert_eq!(prepare(&module(&mem)), Err(TbError::ChangedByOtherTool));
    }

    #[test]
    fn apply_writes_the_top_value_and_only_that() {
        let mem = image();
        mem.put(RDATA_RVA + 0x100, &table_bytes());
        // The game's .rdata is read-only; so is this page.
        mem.protect(RDATA_RVA, 0x1000, PAGE_READONLY);
        let p = prepare(&module(&mem)).unwrap();
        let budget = 5 * GIB;
        // SAFETY: `p` points into `mem`, which this test owns and nothing else uses.
        unsafe { apply(&p, budget) }.unwrap();
        assert_eq!(
            mem.get(RDATA_RVA + 0x100 + 72, 8),
            budget.to_le_bytes(),
            "the top value"
        );
        assert_eq!(
            mem.get(RDATA_RVA + 0x100, 72),
            table_bytes()[..72],
            "the other rows"
        );
        assert_eq!(
            protect_of(mem.base + RDATA_RVA),
            PAGE_READONLY,
            "the page is read-only again"
        );
    }

    #[test]
    fn applying_the_original_size_does_not_touch_the_page() {
        // An address that is not mapped: a `write_checked` call would give `NotMapped`. So `Ok`
        // means there was no call.
        let nowhere = Prepared { top_value_addr: 0 };
        // SAFETY: `apply` of the original size makes no memory access at all.
        assert_eq!(unsafe { apply(&nowhere, ORIGINAL_TOP) }, Ok(()));
        // SAFETY: as above, to show that a write of any other size does reach `write_checked`.
        let other = unsafe { apply(&nowhere, 5 * GIB) };
        assert_eq!(other, Err(PatchError::NotMapped));

        // In an image: the bytes stay as they were, and a later real change still works.
        let mem = image();
        mem.put(RDATA_RVA, &table_bytes());
        mem.protect(RDATA_RVA, 0x1000, PAGE_READONLY);
        let p = prepare(&module(&mem)).unwrap();
        // SAFETY: `p` points into `mem`, which this test owns and nothing else uses.
        assert_eq!(unsafe { apply(&p, ORIGINAL_TOP) }, Ok(()));
        assert_eq!(mem.get(RDATA_RVA, 80), table_bytes());
        assert_eq!(protect_of(mem.base + RDATA_RVA), PAGE_READONLY);
        // SAFETY: as above.
        assert_eq!(unsafe { apply(&p, 5 * GIB) }, Ok(()));
        assert_eq!(mem.get(RDATA_RVA + 72, 8), (5 * GIB).to_le_bytes());
    }

    #[test]
    fn a_second_apply_finds_the_new_value_and_changes_nothing() {
        let mem = image();
        mem.put(RDATA_RVA, &table_bytes());
        let p = prepare(&module(&mem)).unwrap();
        // SAFETY: `p` points into `mem`, which this test owns and nothing else uses.
        unsafe { apply(&p, 5 * GIB) }.unwrap();
        // SAFETY: as above.
        match unsafe { apply(&p, 7 * GIB) } {
            Err(PatchError::Mismatch { at, found }) => {
                // `at` is the first byte that differs from the original, inside the 8 bytes.
                assert!((p.top_value_addr()..p.top_value_addr() + 8).contains(&at));
                assert_eq!(found, (5 * GIB).to_le_bytes());
            }
            other => panic!("expected Mismatch, got {other:?}"),
        }
        assert_eq!(mem.get(RDATA_RVA + 72, 8), (5 * GIB).to_le_bytes());
        // And a later start-up sees a changed table.
        assert_eq!(prepare(&module(&mem)), Err(TbError::ChangedByOtherTool));
    }

    #[test]
    fn apply_refuses_when_another_tool_changed_the_value_after_prepare() {
        let mem = image();
        mem.put(RDATA_RVA, &table_bytes());
        let p = prepare(&module(&mem)).unwrap();
        mem.put(RDATA_RVA + 72, &(6 * GIB).to_le_bytes());
        // SAFETY: `p` points into `mem`, which this test owns and nothing else uses.
        let result = unsafe { apply(&p, 5 * GIB) };
        assert!(
            matches!(result, Err(PatchError::Mismatch { .. })),
            "{result:?}"
        );
        assert_eq!(mem.get(RDATA_RVA + 72, 8), (6 * GIB).to_le_bytes());
    }

    // ---- log lines ----------------------------------------------------------------------------------

    #[test]
    fn the_applied_line_names_the_real_numbers() {
        let (level, line) = outcome(
            &Ok(()),
            16_753_098_752 - 11 * GIB,
            16_753_098_752,
            11.0,
            &TextureBudget::Auto,
        );
        assert_eq!(level, Level::Info);
        assert_eq!(
            line,
            "texture budget: 2.0 GiB -> 4.6 GiB (device-local 15.6 GiB, reserve 11 GiB)"
        );
    }

    #[test]
    fn an_explicit_size_leaves_the_reserve_out_of_the_line() {
        let (level, line) = outcome(
            &Ok(()),
            6 * GIB,
            16_753_098_752,
            11.0,
            &TextureBudget::Gib(6.0),
        );
        assert_eq!(level, Level::Info);
        assert_eq!(
            line,
            "texture budget: 2.0 GiB -> 6.0 GiB (device-local 15.6 GiB)"
        );
    }

    #[test]
    fn writing_the_original_size_is_logged_as_unchanged() {
        for mode in [TextureBudget::Auto, TextureBudget::Gib(2.0)] {
            let (level, line) = outcome(&Ok(()), ORIGINAL_TOP, 16 * GIB, 11.0, &mode);
            assert_eq!(level, Level::Info, "{mode:?}");
            assert_eq!(
                line, "texture budget: unchanged (the budget is already 2 GiB)",
                "{mode:?}"
            );
        }
    }

    #[test]
    fn a_second_apply_is_logged_as_information_not_as_a_failure() {
        let mismatch = PatchError::Mismatch {
            at: 0x1000,
            found: (5 * GIB).to_le_bytes().to_vec(),
        };
        let (level, line) = outcome(
            &Err(mismatch),
            7 * GIB,
            16 * GIB,
            11.0,
            &TextureBudget::Auto,
        );
        assert_eq!(level, Level::Info);
        assert!(line.starts_with("texture budget: "), "{line}");
        assert!(line.contains("already"), "{line}");
        assert!(line.contains("5.0 GiB"), "{line}");
    }

    #[test]
    fn other_patch_errors_are_warnings() {
        for error in [
            PatchError::Protect(5),
            PatchError::NotMapped,
            PatchError::BadLength,
        ] {
            let (level, line) = outcome(&Err(error), 5 * GIB, 16 * GIB, 11.0, &TextureBudget::Auto);
            assert_eq!(level, Level::Warn, "{line}");
            assert!(line.starts_with("texture budget: not applied ("), "{line}");
        }
    }

    #[test]
    fn a_read_back_mismatch_says_the_bytes_were_written() {
        let (level, line) = outcome(
            &Err(PatchError::ReadBack),
            5 * GIB,
            16 * GIB,
            11.0,
            &TextureBudget::Auto,
        );
        assert_eq!(level, Level::Warn);
        assert!(line.starts_with("texture budget: "), "{line}");
        assert!(
            line.contains("written, but the read-back did not match"),
            "{line}"
        );
        assert!(line.contains("5.0 GiB"), "{line}");
        assert!(!line.contains("not applied"), "{line}");
    }

    #[test]
    fn start_up_with_the_budget_off_skips_the_scan_and_says_so() {
        let cell = OnceLock::new();
        let (level, line) = start_up(
            &TextureBudget::Off,
            || panic!("TextureBudget = off must not scan the exe"),
            &cell,
        );
        assert_eq!(level, Level::Info);
        assert!(line.starts_with("texture budget: off"), "{line}");
        assert!(line.contains("TextureBudget = off"), "{line}");
        assert!(cell.get().is_none());
    }

    #[test]
    fn start_up_stores_the_table_and_logs_where_it_is() {
        for mode in [TextureBudget::Auto, TextureBudget::Gib(6.0)] {
            let cell = OnceLock::new();
            let prepared = Prepared {
                top_value_addr: 0x1_413b_8718,
            };
            let (level, line) = start_up(&mode, || Ok(prepared), &cell);
            assert_eq!(level, Level::Info, "{mode:?}");
            assert_eq!(
                line, "texture budget: table at 0x1413b86d0 (top 2 GiB)",
                "{mode:?}"
            );
            assert_eq!(cell.get(), Some(&prepared));
        }
    }

    #[test]
    fn start_up_warns_when_it_cannot_use_the_table() {
        for (error, phrase) in [
            (
                TbError::NotFound,
                "not found in .rdata (or .rdata could not be read)",
            ),
            (TbError::Multiple(3), "3 copies"),
            // The spec's words (design.md); Task 17 looks for them in the log.
            (TbError::ChangedByOtherTool, "changed by another tool"),
        ] {
            let cell = OnceLock::new();
            let (level, line) = start_up(&TextureBudget::Auto, || Err(error.clone()), &cell);
            assert_eq!(level, Level::Warn, "{error:?}");
            assert!(line.starts_with("texture budget: off: "), "{line}");
            assert!(line.contains(phrase), "{line} should contain {phrase:?}");
            // No parenthesis inside a parenthesis, and nothing but the reason after `off:`.
            assert!(line.matches('(').count() <= 1, "{line}");
            assert!(cell.get().is_none(), "{error:?}");
        }
    }

    #[test]
    fn the_changed_by_another_tool_text_names_the_launcher() {
        let text = TbError::ChangedByOtherTool.to_string();
        assert!(text.contains("changed by another tool"), "{text}");
        assert!(text.contains("launcher"), "{text}");
        assert!(!text.contains('('), "{text}");
    }

    // ---- the real game exe (ignored: reads ENSHROUDED_EXE, else the Steam install) -------------------

    /// Where the table is in build f68140a3 (sha256 b28b448a...; the Steam exe has the same data
    /// at the same address): the controller's own check with pefile, 2026-10-26. The top value is
    /// the last 8 bytes of the table.
    const TABLE_VA: u64 = 0x1_413b_86d0;
    const TOP_VALUE_VA: u64 = 0x1_413b_8718;

    /// The `.rdata` section of a PE32+ file: its virtual address (image base included) and the
    /// bytes of it that are in the file. Walks the headers by hand because the parser in `scan`
    /// is private.
    fn rdata_of(file: &[u8]) -> (u64, &[u8]) {
        use crate::scan::{u16_at, u32_at, u64_at};
        let nt = u32_at(file, 0x3c).unwrap() as usize;
        let n_sections = u16_at(file, nt + 6).unwrap() as usize;
        let optional_size = u16_at(file, nt + 20).unwrap() as usize;
        let optional = nt + 24;
        assert_eq!(u16_at(file, optional), Some(0x20b), "PE32+");
        let image_base = u64_at(file, optional + 24).unwrap();
        let table = optional + optional_size;
        for i in 0..n_sections {
            let at = table + i * 40;
            if file.get(at..at + 8) != Some(b".rdata\0\0".as_slice()) {
                continue;
            }
            let virtual_size = u32_at(file, at + 8).unwrap() as usize;
            let rva = u32_at(file, at + 12).unwrap() as u64;
            let raw_size = u32_at(file, at + 16).unwrap() as usize;
            let raw_ptr = u32_at(file, at + 20).unwrap() as usize;
            let len = virtual_size.min(raw_size);
            return (image_base + rva, &file[raw_ptr..raw_ptr + len]);
        }
        panic!("no .rdata section");
    }

    #[test]
    #[ignore = "reads the game exe (ENSHROUDED_EXE, else the Steam install) from disk"]
    fn texture_table_unique_in_game_exe() {
        use crate::build_id::test_support::game_exe_or_skip;
        use crate::build_id::{classify, sha256_file, Build};
        let Some(path) = game_exe_or_skip("texture_table_unique_in_game_exe") else {
            return;
        };
        let build = classify(sha256_file(&path).expect("hash the game exe"));
        assert!(
            matches!(build, Build::Known(_)),
            "this check is for the known build; got {build:?}"
        );

        let file = std::fs::read(&path).expect("read the game exe");
        // Exactly once in .rdata, at the address the RE notes give.
        let (rdata_va, rdata) = rdata_of(&file);
        let started = std::time::Instant::now();
        let offset = prepare_in(rdata).expect("the table is in .rdata exactly once");
        let table_va = rdata_va + offset as u64;
        println!(
            "table at {table_va:#x}, top value at {:#x} (.rdata is {} bytes; search took {:?})",
            table_va + 72,
            rdata.len(),
            started.elapsed()
        );
        assert_eq!(table_va, TABLE_VA);
        assert_eq!(table_va + 72, TOP_VALUE_VA);
        assert_eq!(TOP_VALUE_VA % 8, 0, "the top value is 8-byte aligned");
        // Nowhere else in the file either.
        let anywhere = prepare_in(&file);
        assert!(
            anywhere.is_ok(),
            "the table must occur once in the whole file: {anywhere:?}"
        );
    }
}
