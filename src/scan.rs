//! Signature scanning: byte patterns with wildcards that count only when they match exactly once.
//!
//! [`Pattern`] is a space-separated hex string with `??` wildcards. [`find_unique`] scans a byte
//! slice; [`find_in_sections`] scans named sections of the running exe in place and returns a
//! virtual address. A signature is trusted only when it matches exactly once: no match and
//! several matches are both errors, and a section that cannot be examined counts as no match.
//!
//! # Safety of the in-place scan
//!
//! The PE headers of the exe are read from memory, so nothing in them is trusted. Header bytes
//! are copied from one readable `VirtualQuery` region (at most 64 KiB, and at most `Module.size`
//! when known); parsing then requires the section table to end inside `SizeOfImage`, and every
//! read uses checked arithmetic. A slice over a section is made only after `VirtualQuery` has
//! reported every page of its range committed, readable and part of the module's own allocation,
//! and only if the range lies inside both `SizeOfImage` and the [`Module`] the caller passed in.
//! If any check fails, the result is [`ScanError::NotFound`] and nothing is read. The check is
//! `VirtualQuery` first, scan second, so [`find_in_sections`] is for start-up only; see the
//! "Start-up only" rule on that function.

use std::ffi::c_void;
use std::fmt;
use std::mem::size_of;
use std::ptr::{self, null};

use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Memory::{
    VirtualQuery, MEMORY_BASIC_INFORMATION, MEM_COMMIT, PAGE_EXECUTE_READ, PAGE_EXECUTE_READWRITE,
    PAGE_EXECUTE_WRITECOPY, PAGE_GUARD, PAGE_READONLY, PAGE_READWRITE, PAGE_WRITECOPY,
};

/// A byte pattern: `Some(b)` must equal `b`, `None` matches any byte. At least one byte is fixed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pattern(Vec<Option<u8>>);

/// Why a scan did not give exactly one address.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScanError {
    /// The pattern text is not valid.
    BadPattern(String),
    /// No match, or the searched section could not be read (see the module docs).
    NotFound,
    /// More than one match; the count (overlapping matches included).
    Multiple(usize),
}

impl fmt::Display for ScanError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ScanError::BadPattern(why) => write!(f, "bad pattern: {why}"),
            ScanError::NotFound => f.write_str("no match (or the section could not be read)"),
            ScanError::Multiple(n) => write!(f, "{n} matches, expected exactly one"),
        }
    }
}

impl std::error::Error for ScanError {}

impl Pattern {
    /// Parse space-separated tokens: two hex digits for a byte, `??` for a wildcard.
    ///
    /// A pattern with no fixed byte (including the empty one) is refused, because it would match
    /// everywhere.
    pub fn parse(s: &str) -> Result<Pattern, ScanError> {
        let mut cells = Vec::new();
        for tok in s.split_ascii_whitespace() {
            if tok == "??" {
                cells.push(None);
                continue;
            }
            let two_hex_digits = tok.len() == 2 && tok.bytes().all(|b| b.is_ascii_hexdigit());
            match (two_hex_digits, u8::from_str_radix(tok, 16)) {
                (true, Ok(byte)) => cells.push(Some(byte)),
                _ => {
                    let shown: String = tok.chars().take(16).collect();
                    return Err(ScanError::BadPattern(format!("bad token {shown:?}")));
                }
            }
        }
        if cells.iter().all(Option::is_none) {
            return Err(ScanError::BadPattern(
                "a pattern needs at least one fixed byte".into(),
            ));
        }
        Ok(Pattern(cells))
    }

    /// Number of bytes the pattern covers, wildcards included.
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Always false for a parsed pattern; here for the `len` convention.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// The first fixed byte and its index: the scan looks for this byte, then checks the rest.
    fn anchor(&self) -> Option<(usize, u8)> {
        self.0
            .iter()
            .enumerate()
            .find_map(|(i, cell)| cell.map(|b| (i, b)))
    }
}

/// Count the matches of `pat` in `hay` (overlapping ones too) and note the first.
fn scan_slice(hay: &[u8], pat: &Pattern) -> (usize, Option<usize>) {
    let plen = pat.0.len();
    let Some((anchor_at, anchor_byte)) = pat.anchor() else {
        return (0, None);
    };
    let Some(last_start) = hay.len().checked_sub(plen) else {
        return (0, None);
    };
    // A match starting at `s` has its anchor byte at `s + anchor_at`, so the anchor can only be
    // in `hay[anchor_at..=last_start + anchor_at]` (in range: `anchor_at < plen`).
    let Some(candidates) = hay.get(anchor_at..=last_start + anchor_at) else {
        return (0, None);
    };
    let (mut count, mut first, mut from) = (0usize, None, 0usize);
    while let Some(rel) = candidates
        .get(from..)
        .and_then(|c| c.iter().position(|&b| b == anchor_byte))
    {
        let start = from + rel;
        let fits = hay.get(start..start + plen).is_some_and(|w| {
            w.iter()
                .zip(&pat.0)
                .all(|(b, cell)| cell.is_none_or(|x| x == *b))
        });
        if fits {
            count += 1;
            if first.is_none() {
                first = Some(start);
            }
        }
        from = start + 1;
    }
    (count, first)
}

fn verdict(count: usize, first: Option<usize>) -> Result<usize, ScanError> {
    match first {
        None => Err(ScanError::NotFound),
        Some(at) if count == 1 => Ok(at),
        Some(_) => Err(ScanError::Multiple(count)),
    }
}

/// The offset of the one match of `pat` in `hay`.
///
/// `NotFound` for none and `Multiple(n)` for `n > 1` (overlapping matches count).
pub fn find_unique(hay: &[u8], pat: &Pattern) -> Result<usize, ScanError> {
    let (count, first) = scan_slice(hay, pat);
    verdict(count, first)
}

/// Like [`find_unique`] over several `(virtual address, bytes)` regions together; returns the
/// virtual address of the one match.
fn find_unique_in_regions(regions: &[(usize, &[u8])], pat: &Pattern) -> Result<usize, ScanError> {
    let mut count = 0usize;
    let mut first: Option<usize> = None;
    for &(va, bytes) in regions {
        let (n, at) = scan_slice(bytes, pat);
        count = count.saturating_add(n);
        if let (None, Some(at)) = (first, at) {
            first = Some(va.checked_add(at).ok_or(ScanError::NotFound)?);
        }
    }
    verdict(count, first)
}

/// The running exe in memory: its base address and `SizeOfImage`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Module {
    pub base: usize,
    pub size: usize,
}

// ---- PE headers ---------------------------------------------------------------------------

const MZ_MAGIC: u16 = 0x5a4d;
const PE_SIGNATURE: u32 = 0x0000_4550;
const PE32_PLUS_MAGIC: u16 = 0x20b;
/// The loader refuses more than 96 sections.
const MAX_SECTIONS: usize = 96;
const SECTION_HEADER_LEN: usize = 40;
/// A PE32+ optional header is 112 bytes before its data directories.
const MIN_OPTIONAL_HEADER: usize = 112;
/// Most header bytes copied out of memory. `SizeOfHeaders` is 0x400 to 0x1000 in practice.
const MAX_HEADER_COPY: usize = 0x10000;
/// The DOS header is 64 bytes; anything shorter cannot be a PE image.
const MIN_HEADER_COPY: usize = 0x40;

struct SectionHeader {
    /// Zero-padded; a name of exactly 8 bytes has no NUL.
    name: [u8; 8],
    virtual_size: u32,
    virtual_address: u32,
    raw_size: u32,
    /// File offset of the section's data; only the on-disk cross-check test reads it.
    #[cfg_attr(not(test), allow(dead_code))]
    raw_ptr: u32,
}

impl SectionHeader {
    /// Whole-name comparison. A request longer than 8 bytes never matches (long section names
    /// that live in the COFF string table are not supported).
    fn is_named(&self, name: &str) -> bool {
        let len = self
            .name
            .iter()
            .position(|&b| b == 0)
            .unwrap_or(self.name.len());
        !name.is_empty() && self.name.get(..len) == Some(name.as_bytes())
    }

    /// `(rva, length)` of the section in memory, or `None` if it ends past `limit`. An empty
    /// section (VirtualSize and SizeOfRawData both 0) is `Some((rva, 0))`, not `None`, so the
    /// caller can tell "nothing to search" from "outside the module".
    fn mem_range(&self, limit: usize) -> Option<(usize, usize)> {
        // VirtualSize 0 means "use the raw size".
        let len = match self.virtual_size {
            0 => self.raw_size as usize,
            v => v as usize,
        };
        let rva = self.virtual_address as usize;
        let end = rva.checked_add(len)?;
        (end <= limit).then_some((rva, len))
    }
}

struct PeInfo {
    /// Preferred load address; only the on-disk cross-check test reads it (in memory the real
    /// base comes from the [`Module`]).
    #[cfg_attr(not(test), allow(dead_code))]
    image_base: u64,
    size_of_image: u32,
    sections: Vec<SectionHeader>,
}

pub(crate) fn u16_at(b: &[u8], off: usize) -> Option<u16> {
    let end = off.checked_add(2)?;
    Some(u16::from_le_bytes(b.get(off..end)?.try_into().ok()?))
}

pub(crate) fn u32_at(b: &[u8], off: usize) -> Option<u32> {
    let end = off.checked_add(4)?;
    Some(u32::from_le_bytes(b.get(off..end)?.try_into().ok()?))
}

pub(crate) fn u64_at(b: &[u8], off: usize) -> Option<u64> {
    let end = off.checked_add(8)?;
    Some(u64::from_le_bytes(b.get(off..end)?.try_into().ok()?))
}

/// Parse the PE32+ headers at the start of `b` (a file, or a copy of the header block of a
/// mapped image). `None` for anything malformed, truncated or not 64-bit; no read goes outside
/// `b`, and the section table must lie inside `SizeOfImage`.
fn parse_pe(b: &[u8]) -> Option<PeInfo> {
    if u16_at(b, 0)? != MZ_MAGIC {
        return None;
    }
    let nt = u32_at(b, 0x3c)? as usize;
    if u32_at(b, nt)? != PE_SIGNATURE {
        return None;
    }
    let file_header = nt.checked_add(4)?;
    let n_sections = usize::from(u16_at(b, file_header.checked_add(2)?)?);
    let optional_size = usize::from(u16_at(b, file_header.checked_add(16)?)?);
    let optional = file_header.checked_add(20)?;
    if u16_at(b, optional)? != PE32_PLUS_MAGIC || optional_size < MIN_OPTIONAL_HEADER {
        return None;
    }
    let image_base = u64_at(b, optional.checked_add(24)?)?;
    let size_of_image = u32_at(b, optional.checked_add(56)?)?;
    if size_of_image == 0 || n_sections == 0 || n_sections > MAX_SECTIONS {
        return None;
    }
    let table = optional.checked_add(optional_size)?;
    let table_end = table.checked_add(n_sections * SECTION_HEADER_LEN)?;
    if table_end > size_of_image as usize {
        return None;
    }
    let mut sections = Vec::with_capacity(n_sections);
    for i in 0..n_sections {
        let at = table.checked_add(i * SECTION_HEADER_LEN)?;
        sections.push(SectionHeader {
            name: b.get(at..at.checked_add(8)?)?.try_into().ok()?,
            virtual_size: u32_at(b, at.checked_add(8)?)?,
            virtual_address: u32_at(b, at.checked_add(12)?)?,
            raw_size: u32_at(b, at.checked_add(16)?)?,
            raw_ptr: u32_at(b, at.checked_add(20)?)?,
        });
    }
    Some(PeInfo {
        image_base,
        size_of_image,
        sections,
    })
}

// ---- readable memory ----------------------------------------------------------------------

/// True if a page with this protection can be read: one of the six readable protections, and
/// not a guard page (a read of a guard page raises an exception). Modifier bits such as
/// `PAGE_NOCACHE` do not matter.
pub(crate) fn protect_is_readable(protect: u32) -> bool {
    if protect & PAGE_GUARD != 0 {
        return false;
    }
    matches!(
        protect & 0xff,
        PAGE_READONLY
            | PAGE_READWRITE
            | PAGE_WRITECOPY
            | PAGE_EXECUTE_READ
            | PAGE_EXECUTE_READWRITE
            | PAGE_EXECUTE_WRITECOPY
    )
}

/// How many bytes from `addr` to the end of its `VirtualQuery` region are committed and
/// readable. Zero if `addr` is unmapped, reserved only, no-access or a guard page, or if its
/// region belongs to a different allocation than the one starting at `alloc_base`. (A loaded
/// image is one allocation, so a section slice can never run into an unrelated neighbour.)
pub(crate) fn readable_run(addr: usize, alloc_base: usize) -> usize {
    let mut info = MEMORY_BASIC_INFORMATION::default();
    // SAFETY: `info` is a valid buffer of exactly the size passed. VirtualQuery accepts any
    // address, reads no memory at it, and reports failure by returning 0.
    let written = unsafe {
        VirtualQuery(
            addr as *const c_void,
            &mut info,
            size_of::<MEMORY_BASIC_INFORMATION>(),
        )
    };
    if written < size_of::<MEMORY_BASIC_INFORMATION>()
        || info.State != MEM_COMMIT
        || info.AllocationBase as usize != alloc_base
        || !protect_is_readable(info.Protect)
    {
        return 0;
    }
    (info.BaseAddress as usize)
        .saturating_add(info.RegionSize)
        .saturating_sub(addr)
}

/// True if every byte of `[addr, addr + len)` is readable, walking region by region with `run`
/// (see [`readable_run`]). An overflowing range, or a gap anywhere, gives false.
pub(crate) fn walk_readable(addr: usize, len: usize, run: impl Fn(usize) -> usize) -> bool {
    let Some(end) = addr.checked_add(len) else {
        return false;
    };
    let mut at = addr;
    while at < end {
        match run(at) {
            0 => return false,
            // Each step moves forward by at least one byte, so this ends.
            n => at = at.saturating_add(n),
        }
    }
    true
}

/// Copy up to `limit` (and [`MAX_HEADER_COPY`]) bytes from `base`, only from the one readable
/// region that starts there, and only if `base` is the start of its allocation (an image's base
/// always is). A copy, so no reference to live memory outlives this call.
fn read_headers(base: usize, limit: usize) -> Option<Vec<u8>> {
    let len = readable_run(base, base).min(limit).min(MAX_HEADER_COPY);
    if len < MIN_HEADER_COPY {
        return None;
    }
    let mut copy = vec![0u8; len];
    // SAFETY: `readable_run` reported `[base, base + len)` as committed, readable memory in one
    // region, and `len` is at most that run. `copy` is a fresh allocation of `len` bytes, so the
    // ranges cannot overlap.
    unsafe { ptr::copy_nonoverlapping(base as *const u8, copy.as_mut_ptr(), len) };
    Some(copy)
}

fn read_pe(m: &Module) -> Option<PeInfo> {
    if m.base == 0 {
        return None;
    }
    parse_pe(&read_headers(m.base, m.size)?)
}

/// The running exe: base from `GetModuleHandleW(NULL)`, size from its PE `SizeOfImage`.
///
/// If the headers in memory cannot be read or are not a PE32+ image, `size` is 0 and every
/// search in the module comes back `NotFound`.
///
/// Start-up only, like [`find_in_sections`]: see the rule there.
pub fn main_module() -> Module {
    // SAFETY: a null module name asks for the handle of the process's own exe; it has no
    // preconditions and returns null on failure.
    let base = unsafe { GetModuleHandleW(null()) } as usize;
    let size = if base == 0 {
        0
    } else {
        read_headers(base, usize::MAX)
            .and_then(|h| parse_pe(&h))
            .map_or(0, |pe| pe.size_of_image as usize)
    };
    Module { base, size }
}

/// Search the named PE sections of `m` in place for the one match of `pat`; returns its virtual
/// address.
///
/// Uniqueness counts across all the named sections together; a name given twice counts once.
/// Every named section must exist, lie inside the module and `SizeOfImage`, and be fully
/// committed and readable inside the module's own allocation (`m.base` must be the start of
/// that allocation, as an image base is), or the result is `NotFound` (see the module docs). A
/// named section that is empty (VirtualSize and SizeOfRawData both 0) has nothing to search and
/// is skipped.
///
/// # Start-up only
///
/// Call this only from `InitializeASI` / `entry::start`, on one thread, before the game's
/// `WinMain` runs. Never call it from a hook or from the governor thread. Reasons:
/// - it forms a `&[u8]` over live image memory, which is sound only while nothing else writes
///   that memory; at start-up the game and the hooks are not running yet;
/// - the readability check is `VirtualQuery` first and the scan second, so a protection change
///   in between (another thread) would fault;
/// - it logs and allocates on failure, which hooks must never do.
pub fn find_in_sections(m: &Module, sections: &[&str], pat: &Pattern) -> Result<usize, ScanError> {
    let Some(pe) = read_pe(m) else {
        ::log::warn!("scan: no readable PE headers at {:#x}", m.base);
        return Err(ScanError::NotFound);
    };
    let limit = m.size.min(pe.size_of_image as usize);
    let mut seen: Vec<&str> = Vec::new();
    let mut regions: Vec<(usize, &[u8])> = Vec::new();
    for &name in sections {
        if seen.contains(&name) {
            continue;
        }
        seen.push(name);
        let mut found = false;
        for section in pe.sections.iter().filter(|s| s.is_named(name)) {
            found = true;
            let range = section
                .mem_range(limit)
                .and_then(|(rva, len)| Some((m.base.checked_add(rva)?, len)));
            let Some((va, len)) = range else {
                ::log::warn!("scan: section {name} lies outside the module");
                return Err(ScanError::NotFound);
            };
            if len == 0 {
                ::log::warn!("scan: section {name} is empty");
                continue;
            }
            if !walk_readable(va, len, |at| readable_run(at, m.base)) {
                ::log::warn!("scan: section {name} is not fully readable");
                return Err(ScanError::NotFound);
            }
            // SAFETY: `walk_readable` just confirmed every page of `[va, va + len)` committed
            // and readable, all inside the allocation that starts at `m.base`; `len` is at most
            // `SizeOfImage`, which is under `u32::MAX`, so it fits `isize`; `u8` has no
            // alignment or validity requirement. The bytes are only read, and the slice does not
            // leave this function. That nothing writes the memory meanwhile, and that the
            // protection does not change between the check and the read, rests on the
            // "Start-up only" rule in this function's docs: callers must follow it.
            let bytes = unsafe { std::slice::from_raw_parts(va as *const u8, len) };
            regions.push((va, bytes));
        }
        if !found {
            ::log::warn!("scan: section {name} not in the image");
            return Err(ScanError::NotFound);
        }
    }
    find_unique_in_regions(&regions, pat)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sigs;
    use std::ffi::c_void;
    use std::ptr::null;
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows_sys::Win32::System::Memory::{
        VirtualAlloc, VirtualFree, VirtualProtect, MEM_COMMIT, MEM_RELEASE, MEM_RESERVE,
        PAGE_EXECUTE_READ, PAGE_GUARD, PAGE_NOACCESS, PAGE_READONLY, PAGE_READWRITE,
    };

    fn pat(s: &str) -> Pattern {
        Pattern::parse(s).unwrap()
    }

    // ---- the five tests from the brief ----------------------------------------------------

    #[test]
    fn wildcard_match_once() {
        let hay = [0x90, 0x83, 0xf9, 0x01, 0x75, 0x12, 0xc7, 0x90];
        let p = Pattern::parse("83 f9 01 75 ?? c7").unwrap();
        assert_eq!(find_unique(&hay, &p), Ok(1));
    }

    #[test]
    fn two_matches_is_error() {
        let hay = [0xaa, 0xbb, 0xaa, 0xbb];
        assert_eq!(
            find_unique(&hay, &Pattern::parse("aa bb").unwrap()),
            Err(ScanError::Multiple(2))
        );
    }

    #[test]
    fn bad_pattern() {
        assert!(matches!(
            Pattern::parse("zz"),
            Err(ScanError::BadPattern(_))
        ));
    }

    #[test]
    fn every_sig_parses() {
        for s in sigs::ALL {
            Pattern::parse(s.pattern).unwrap();
        }
    }

    #[test]
    fn finds_own_text() {
        // The test binary's own .text: just assert the API walks the sections.
        let m = main_module();
        assert!(m.size > 0x1000);
        assert!(matches!(
            find_in_sections(
                &m,
                &[".text"],
                &Pattern::parse("de ad be ef 13 37 c0 de 42 42").unwrap()
            ),
            Err(ScanError::NotFound)
        ));
    }

    // ---- Pattern::parse -------------------------------------------------------------------

    #[test]
    fn parse_accepts_hex_wildcards_and_any_spacing() {
        let p = pat("  AA\tbb  ??\n0F ");
        assert_eq!(p.len(), 4);
        assert!(!p.is_empty());
        assert_eq!(p, Pattern(vec![Some(0xaa), Some(0xbb), None, Some(0x0f)]));
    }

    #[test]
    fn parse_rejects_malformed_tokens() {
        for bad in [
            "", "   ", "?? ??", "a", "123", "?", "???", "0x12", "+f", "-1", "zz", "aa bb c", "é",
            "aa,bb", "aa  b?",
        ] {
            assert!(
                matches!(Pattern::parse(bad), Err(ScanError::BadPattern(_))),
                "{bad:?} should be a bad pattern"
            );
        }
    }

    // ---- scanning a slice -----------------------------------------------------------------

    #[test]
    fn leading_and_trailing_wildcards_match_at_the_edges() {
        assert_eq!(find_unique(&[0x11, 0xaa], &pat("?? aa")), Ok(0));
        assert_eq!(find_unique(&[0xaa, 0x00], &pat("aa ??")), Ok(0));
        assert_eq!(
            find_unique(&[0xaa], &pat("aa ??")),
            Err(ScanError::NotFound)
        );
        assert_eq!(
            find_unique(&[0xaa], &pat("?? aa")),
            Err(ScanError::NotFound)
        );
        assert_eq!(find_unique(&[0, 0, 0xbb, 0xcc], &pat("?? ?? bb cc")), Ok(0));
        // The fixed bytes sit too early for the wildcards in front of them to fit.
        assert_eq!(
            find_unique(&[0xbb, 0xcc, 0, 0], &pat("?? ?? bb cc")),
            Err(ScanError::NotFound)
        );
    }

    #[test]
    fn overlapping_matches_all_count() {
        assert_eq!(
            find_unique(&[0xaa; 3], &pat("aa aa")),
            Err(ScanError::Multiple(2))
        );
        assert_eq!(
            find_unique(&[0xaa; 5], &pat("aa")),
            Err(ScanError::Multiple(5))
        );
    }

    #[test]
    fn short_or_empty_haystack_is_not_found() {
        assert_eq!(find_unique(&[], &pat("aa")), Err(ScanError::NotFound));
        assert_eq!(
            find_unique(&[0xaa], &pat("aa bb")),
            Err(ScanError::NotFound)
        );
        assert_eq!(
            find_unique(&[0xaa, 0xbb], &pat("aa bb cc")),
            Err(ScanError::NotFound)
        );
    }

    #[test]
    fn exact_fit_and_last_position_match() {
        assert_eq!(find_unique(&[1, 2, 3], &pat("01 02 03")), Ok(0));
        assert_eq!(find_unique(&[9, 9, 1, 2, 3], &pat("01 02 03")), Ok(2));
        assert_eq!(find_unique(&[1, 2, 3, 9], &pat("01 ?? 03")), Ok(0));
    }

    /// The anchored scan agrees with a plain windows() comparison on pseudo-random data.
    #[test]
    fn scan_agrees_with_naive_reference() {
        let mut state = 0x2545_f491_4f6c_dd1du64;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        for round in 0..400 {
            let hay_len = (next() % 300) as usize;
            // A 4-letter alphabet makes matches common.
            let hay: Vec<u8> = (0..hay_len).map(|_| (next() % 4) as u8).collect();
            let pat_len = 1 + (next() % 6) as usize;
            let mut cells: Vec<Option<u8>> = (0..pat_len)
                .map(|_| {
                    if next() % 3 == 0 {
                        None
                    } else {
                        Some((next() % 4) as u8)
                    }
                })
                .collect();
            if cells.iter().all(Option::is_none) {
                cells[0] = Some(0);
            }
            let p = Pattern(cells.clone());
            let naive: Vec<usize> = hay
                .windows(pat_len)
                .enumerate()
                .filter(|(_, w)| w.iter().zip(&cells).all(|(b, c)| c.is_none_or(|x| x == *b)))
                .map(|(i, _)| i)
                .collect();
            let (count, first) = scan_slice(&hay, &p);
            assert_eq!(count, naive.len(), "round {round}");
            assert_eq!(first, naive.first().copied(), "round {round}");
        }
    }

    // ---- searching several regions together -----------------------------------------------

    #[test]
    fn regions_count_together_and_give_a_virtual_address() {
        let a = [0u8, 0, 0xaa, 0xbb];
        let b = [0xaa, 0xbb, 0, 0];
        let p = pat("aa bb");
        assert_eq!(find_unique_in_regions(&[(0x1000, &a)], &p), Ok(0x1002));
        assert_eq!(
            find_unique_in_regions(&[(0x1000, &a), (0x2000, &[0, 0])], &p),
            Ok(0x1002)
        );
        assert_eq!(
            find_unique_in_regions(&[(0x1000, &a), (0x2000, &b)], &p),
            Err(ScanError::Multiple(2))
        );
        assert_eq!(find_unique_in_regions(&[], &p), Err(ScanError::NotFound));
        assert_eq!(
            find_unique_in_regions(&[(0x1000, &[])], &p),
            Err(ScanError::NotFound)
        );
        // A virtual address that would overflow is a failure, not a wrap.
        assert_eq!(
            find_unique_in_regions(&[(usize::MAX - 1, &a)], &p),
            Err(ScanError::NotFound)
        );
    }

    // ---- PE header parsing ----------------------------------------------------------------

    /// A minimal PE32+ header block: (name, virtual_size, virtual_address, raw_size, raw_ptr).
    fn synth_pe(secs: &[(&str, u32, u32, u32, u32)], size_of_image: u32) -> Vec<u8> {
        let mut b = vec![0u8; 0x400];
        b[0..2].copy_from_slice(b"MZ");
        b[0x3c..0x40].copy_from_slice(&0x80u32.to_le_bytes());
        b[0x80..0x84].copy_from_slice(b"PE\0\0");
        let fh = 0x84;
        b[fh..fh + 2].copy_from_slice(&0x8664u16.to_le_bytes());
        b[fh + 2..fh + 4].copy_from_slice(&(secs.len() as u16).to_le_bytes());
        b[fh + 16..fh + 18].copy_from_slice(&240u16.to_le_bytes());
        let opt = fh + 20;
        b[opt..opt + 2].copy_from_slice(&0x20bu16.to_le_bytes());
        b[opt + 24..opt + 32].copy_from_slice(&0x1_4000_0000u64.to_le_bytes());
        b[opt + 56..opt + 60].copy_from_slice(&size_of_image.to_le_bytes());
        b[opt + 60..opt + 64].copy_from_slice(&0x400u32.to_le_bytes());
        let table = opt + 240;
        for (i, (name, vsize, va, rsize, rptr)) in secs.iter().enumerate() {
            let o = table + i * 40;
            b[o..o + name.len()].copy_from_slice(name.as_bytes());
            b[o + 8..o + 12].copy_from_slice(&vsize.to_le_bytes());
            b[o + 12..o + 16].copy_from_slice(&va.to_le_bytes());
            b[o + 16..o + 20].copy_from_slice(&rsize.to_le_bytes());
            b[o + 20..o + 24].copy_from_slice(&rptr.to_le_bytes());
        }
        b
    }

    #[test]
    fn parse_pe_reads_the_fields() {
        let b = synth_pe(
            &[
                (".text", 0x2000, 0x1000, 0x1e00, 0x400),
                (".rdata", 0x800, 0x3000, 0x800, 0x2200),
            ],
            0x4000,
        );
        let info = parse_pe(&b).unwrap();
        assert_eq!(info.image_base, 0x1_4000_0000);
        assert_eq!(info.size_of_image, 0x4000);
        assert_eq!(info.sections.len(), 2);
        let t = &info.sections[0];
        assert!(t.is_named(".text") && !t.is_named(".rdata"));
        assert_eq!(
            (t.virtual_size, t.virtual_address, t.raw_size, t.raw_ptr),
            (0x2000, 0x1000, 0x1e00, 0x400)
        );
        assert!(info.sections[1].is_named(".rdata"));
    }

    #[test]
    fn section_names_match_whole_names_only() {
        let b = synth_pe(
            &[
                (".text", 0x10, 0x1000, 0, 0),
                ("12345678", 0x10, 0x2000, 0, 0),
            ],
            0x3000,
        );
        let info = parse_pe(&b).unwrap();
        assert!(info.sections[0].is_named(".text"));
        assert!(!info.sections[0].is_named(".tex"));
        assert!(!info.sections[0].is_named(".text2"));
        assert!(!info.sections[0].is_named(""));
        // A name that fills all eight bytes has no NUL; a longer request never matches it.
        assert!(info.sections[1].is_named("12345678"));
        assert!(!info.sections[1].is_named("123456789"));
        assert!(!info.sections[1].is_named("1234567"));
    }

    #[test]
    fn mem_range_uses_virtual_size_then_raw_size_and_stays_in_the_limit() {
        let h = |vsize, va, rsize| SectionHeader {
            name: [0; 8],
            virtual_size: vsize,
            virtual_address: va,
            raw_size: rsize,
            raw_ptr: 0,
        };
        assert_eq!(
            h(0x2000, 0x1000, 0x1e00).mem_range(0x4000),
            Some((0x1000, 0x2000))
        );
        // VirtualSize 0 means "use the raw size".
        assert_eq!(
            h(0, 0x1000, 0x1e00).mem_range(0x4000),
            Some((0x1000, 0x1e00))
        );
        // Ends exactly at the limit: fine. One byte past: refused.
        assert_eq!(
            h(0x3000, 0x1000, 0).mem_range(0x4000),
            Some((0x1000, 0x3000))
        );
        assert_eq!(h(0x3001, 0x1000, 0).mem_range(0x4000), None);
        // Empty (VirtualSize and SizeOfRawData both 0) is not "outside": it has length 0. It is
        // outside only if it sits past the limit.
        assert_eq!(h(0, 0x1000, 0).mem_range(0x4000), Some((0x1000, 0)));
        assert_eq!(h(0, 0x4000, 0).mem_range(0x4000), Some((0x4000, 0)));
        assert_eq!(h(0, 0x4001, 0).mem_range(0x4000), None);
        // The 32-bit extreme.
        assert_eq!(
            h(u32::MAX, u32::MAX, 0).mem_range(usize::MAX),
            Some((u32::MAX as usize, u32::MAX as usize))
        );
        assert_eq!(h(u32::MAX, u32::MAX, 0).mem_range(0x4000), None);
    }

    /// Offset of the optional header in `synth_pe`'s output.
    const OPT: usize = 0x98;

    #[test]
    fn parse_pe_rejects_bad_headers() {
        let good = synth_pe(&[(".text", 0x1000, 0x1000, 0x200, 0x400)], 0x2000);
        assert!(parse_pe(&good).is_some());
        let with = |edit: &dyn Fn(&mut Vec<u8>)| {
            let mut b = good.clone();
            edit(&mut b);
            parse_pe(&b).is_some()
        };
        assert!(!with(&|b| b[0] = b'X'), "bad MZ");
        assert!(!with(&|b| b[0x80] = b'X'), "bad PE signature");
        assert!(
            !with(&|b| b[0x98..0x9a].copy_from_slice(&0x10bu16.to_le_bytes())),
            "PE32, not PE32+"
        );
        assert!(
            !with(&|b| b[0x3c..0x40].copy_from_slice(&u32::MAX.to_le_bytes())),
            "e_lfanew huge"
        );
        assert!(
            !with(&|b| b[0x3c..0x40].copy_from_slice(&0x3feu32.to_le_bytes())),
            "e_lfanew at the end"
        );
        assert!(
            !with(&|b| b[0x86..0x88].copy_from_slice(&0u16.to_le_bytes())),
            "no sections"
        );
        assert!(
            !with(&|b| b[0x86..0x88].copy_from_slice(&97u16.to_le_bytes())),
            "too many sections"
        );
        assert!(
            !with(&|b| b[0x86..0x88].copy_from_slice(&u16::MAX.to_le_bytes())),
            "65535 sections"
        );
        assert!(
            !with(&|b| b[0x94..0x96].copy_from_slice(&111u16.to_le_bytes())),
            "optional header too small"
        );
        assert!(
            !with(&|b| b[0x94..0x96].copy_from_slice(&u16::MAX.to_le_bytes())),
            "optional header huge"
        );
        assert!(
            !with(&|b| b[OPT + 56..OPT + 60].copy_from_slice(&0u32.to_le_bytes())),
            "SizeOfImage 0"
        );
        // The section table must lie inside the image: SizeOfImage 0x100 ends before it.
        assert!(
            !with(&|b| b[OPT + 56..OPT + 60].copy_from_slice(&0x100u32.to_le_bytes())),
            "table past image"
        );
        assert!(parse_pe(&[]).is_none());
    }

    #[test]
    fn parse_pe_never_panics_on_truncated_or_corrupt_input() {
        let good = synth_pe(
            &[
                (".text", 0x1000, 0x1000, 0x200, 0x400),
                (".rdata", 0x10, 0x2000, 0, 0),
            ],
            0x3000,
        );
        // Every truncation length. The parser reads the first 24 bytes of each 40-byte section
        // header, so the last header's fields end at 0x188 + 40 + 24; anything shorter is None.
        for n in 0..=good.len() {
            let r = parse_pe(&good[..n]);
            assert_eq!(r.is_some(), n >= 0x188 + 40 + 24, "len {n}");
        }
        // Every byte of the headers set to each of a few extreme values.
        for i in 0..0x230 {
            for v in [0x00u8, 0x7f, 0x80, 0xff] {
                let mut b = good.clone();
                b[i] = v;
                let _ = parse_pe(&b);
            }
        }
    }

    // ---- readable memory ------------------------------------------------------------------

    #[test]
    fn protect_flags_that_allow_a_read() {
        for ok in [
            PAGE_READONLY,
            PAGE_READWRITE,
            0x08,
            PAGE_EXECUTE_READ,
            0x40,
            0x80,
        ] {
            assert!(protect_is_readable(ok), "{ok:#x}");
            // A modifier bit (NOCACHE, WRITECOMBINE, TARGETS_INVALID) does not change that.
            assert!(protect_is_readable(ok | 0x200), "{ok:#x}|NOCACHE");
            assert!(
                protect_is_readable(ok | 0x4000_0000),
                "{ok:#x}|TARGETS_INVALID"
            );
            assert!(!protect_is_readable(ok | PAGE_GUARD), "{ok:#x}|GUARD");
        }
        for no in [
            0,
            PAGE_NOACCESS,
            0x10, /* PAGE_EXECUTE: no read */
            PAGE_GUARD,
        ] {
            assert!(!protect_is_readable(no), "{no:#x}");
        }
    }

    #[test]
    fn walk_readable_follows_regions_and_stops_at_a_gap() {
        // Regions: [0x1000, 0x3000) readable, [0x3000, 0x4000) not, [0x4000, 0x6000) readable.
        let run = |a: usize| match a {
            0x1000..=0x2fff => 0x3000 - a,
            0x4000..=0x5fff => 0x6000 - a,
            _ => 0,
        };
        assert!(walk_readable(0x1000, 0x2000, run));
        assert!(walk_readable(0x1800, 0x100, run));
        assert!(!walk_readable(0x1000, 0x2001, run));
        assert!(!walk_readable(0x2fff, 0x2, run));
        assert!(!walk_readable(0x3000, 1, run));
        assert!(walk_readable(0x4000, 0x2000, run));
        assert!(!walk_readable(0x4000, 0x2001, run));
        assert!(!walk_readable(0x1000, 0x5000, run), "spans the gap");
        // Two adjacent readable regions are one readable range.
        let two = |a: usize| match a {
            0x1000..=0x1fff => 0x2000 - a,
            0x2000..=0x2fff => 0x3000 - a,
            _ => 0,
        };
        assert!(walk_readable(0x1000, 0x2000, two));
        // Overflow of address + length is a "no", never a wrap.
        assert!(!walk_readable(usize::MAX - 4, 100, |_| usize::MAX));
        assert!(!walk_readable(0x1000, usize::MAX, run));
        // A region that claims to run to the end of the address space terminates.
        assert!(walk_readable(0x1000, 0x5000, |_| usize::MAX));
    }

    /// A page range from VirtualAlloc, freed on drop.
    struct Pages(*mut c_void, usize);
    impl Pages {
        fn new(n_pages: usize) -> Pages {
            let len = n_pages * 0x1000;
            // SAFETY: a fresh anonymous allocation; the OS picks the address.
            let p = unsafe { VirtualAlloc(null(), len, MEM_COMMIT | MEM_RESERVE, PAGE_READWRITE) };
            assert!(!p.is_null());
            Pages(p, len)
        }
        /// Commit `len` bytes at exactly `addr` (a free, 64 KiB-aligned range), or `None`.
        fn at(addr: usize, len: usize) -> Option<Pages> {
            // SAFETY: asks for a specific free range; the OS refuses if it is taken.
            let p = unsafe {
                VirtualAlloc(
                    addr as *const c_void,
                    len,
                    MEM_COMMIT | MEM_RESERVE,
                    PAGE_READWRITE,
                )
            };
            (!p.is_null()).then_some(Pages(p, len))
        }
        fn addr(&self) -> usize {
            self.0 as usize
        }
        fn protect(&self, page: usize, prot: u32) {
            let mut old = 0;
            // SAFETY: the page lies inside this allocation.
            let ok = unsafe {
                VirtualProtect(
                    (self.addr() + page * 0x1000) as *const c_void,
                    0x1000,
                    prot,
                    &mut old,
                )
            };
            assert_ne!(ok, 0);
        }
    }
    impl Drop for Pages {
        fn drop(&mut self) {
            // SAFETY: `self.0` is the base this allocation returned.
            unsafe { VirtualFree(self.0, 0, MEM_RELEASE) };
        }
    }

    #[test]
    fn readable_run_reads_the_real_page_protections() {
        let pages = Pages::new(4);
        let a = pages.addr();
        // The walk the scanner uses: the allocation base is the allocation's own address.
        let run = |at: usize| readable_run(at, a);
        assert_eq!(run(a), 0x4000, "four committed RW pages are one region");
        assert_eq!(run(a + 0x10), 0x4000 - 0x10, "mid-page address");
        pages.protect(1, PAGE_NOACCESS);
        pages.protect(2, PAGE_GUARD | PAGE_READWRITE);
        pages.protect(3, PAGE_READONLY);
        assert_eq!(run(a), 0x1000);
        assert_eq!(run(a + 0x1000), 0, "NOACCESS");
        assert_eq!(run(a + 0x2800), 0, "GUARD");
        assert_eq!(run(a + 0x3000), 0x1000, "READONLY");
        assert!(walk_readable(a, 0x1000, run));
        assert!(!walk_readable(a, 0x2000, run));
        assert!(
            !walk_readable(a + 0xff0, 0x20, run),
            "crosses into NOACCESS"
        );
        pages.protect(1, PAGE_READONLY);
        pages.protect(2, PAGE_READWRITE);
        assert!(
            walk_readable(a, 0x4000, run),
            "RW + RO pages are different regions, all readable"
        );
        assert!(
            !walk_readable(a, 0x4001, run),
            "one byte past the allocation"
        );
    }

    #[test]
    fn readable_run_is_zero_for_unmapped_and_reserved_memory() {
        assert_eq!(readable_run(0, 0), 0);
        assert_eq!(readable_run(0x1000, 0x1000), 0);
        assert_eq!(readable_run(usize::MAX, usize::MAX), 0);
        assert_eq!(readable_run(usize::MAX - 0xfff, usize::MAX - 0xfff), 0);
        // Reserved but never committed.
        // SAFETY: a fresh anonymous reservation, freed below.
        let p = unsafe { VirtualAlloc(null(), 0x2000, MEM_RESERVE, PAGE_NOACCESS) };
        assert!(!p.is_null());
        assert_eq!(readable_run(p as usize, p as usize), 0);
        // SAFETY: `p` is the base the reservation returned.
        unsafe { VirtualFree(p, 0, MEM_RELEASE) };
    }

    #[test]
    fn readable_run_refuses_another_allocation() {
        let pages = Pages::new(2);
        let a = pages.addr();
        assert_eq!(readable_run(a, a), 0x2000);
        // Right address, wrong claimed allocation base: not the module's own memory.
        assert_eq!(readable_run(a, a + 0x1000), 0);
        assert_eq!(
            readable_run(a + 0x1000, a + 0x1000),
            0,
            "not an allocation base"
        );
        assert_eq!(readable_run(a + 0x1000, 0), 0);
    }

    /// Two committed RW allocations of 64 KiB each, one directly after the other (an allocation
    /// always starts on a 64 KiB boundary, so adjacent ones are separate allocations).
    fn adjacent_allocations() -> (Pages, Pages) {
        const SPAN: usize = 0x10000;
        for _ in 0..100 {
            // Find a free 128 KiB range by reserving it and letting it go again.
            // SAFETY: a fresh anonymous reservation, freed right away.
            let probe = unsafe { VirtualAlloc(null(), 2 * SPAN, MEM_RESERVE, PAGE_NOACCESS) };
            if probe.is_null() {
                continue;
            }
            // SAFETY: `probe` is the base the reservation returned.
            unsafe { VirtualFree(probe, 0, MEM_RELEASE) };
            let start = probe as usize;
            // Another thread may take the range in between; then try a new one.
            if let (Some(first), Some(second)) =
                (Pages::at(start, SPAN), Pages::at(start + SPAN, SPAN))
            {
                return (first, second);
            }
        }
        panic!("could not place two adjacent allocations");
    }

    #[test]
    fn a_section_cannot_run_into_the_next_allocation() {
        let (first, second) = adjacent_allocations();
        let (a, b) = (first.addr(), second.addr());
        assert_eq!(b, a + 0x10000);
        // Both are plain readable memory, and the second starts where the first ends...
        assert_eq!(readable_run(a, a), 0x10000, "stops at the allocation's end");
        assert_eq!(readable_run(b, b), 0x10000);
        // ...but a walk anchored at the first allocation does not cross into the second.
        let run = |at: usize| readable_run(at, a);
        assert!(walk_readable(a, 0x10000, run));
        assert!(
            !walk_readable(a + 0xf000, 0x2000, run),
            "straddles the boundary"
        );
        assert!(!walk_readable(b, 0x1000, run), "wholly in the neighbour");

        // The same through find_in_sections: an image in the first allocation whose sections
        // are at 0xf800 (straddles) and 0x10000 (wholly in the neighbour).
        let hdr = synth_pe(
            &[
                (".text", 0x1000, 0x1000, 0x1000, 0x400),
                (".strad", 0x1000, 0xf800, 0x1000, 0),
                (".next", 0x800, 0x10000, 0x800, 0),
            ],
            0x20000,
        );
        poke(&first, 0, &hdr);
        let needle = [0xc3, 0x9a, 0x17, 0x6e, 0xb2, 0x40];
        poke(&first, 0x1100, &needle);
        poke(&second, 0x10, &needle);
        let m = Module {
            base: a,
            size: 0x20000,
        };
        let p = pat("c3 9a 17 6e b2 40");
        assert_eq!(find_in_sections(&m, &[".text"], &p), Ok(a + 0x1100));
        assert_eq!(
            find_in_sections(&m, &[".strad"], &p),
            Err(ScanError::NotFound)
        );
        assert_eq!(
            find_in_sections(&m, &[".next"], &p),
            Err(ScanError::NotFound),
            "the needle at b + 0x10 is in another allocation"
        );
        assert_eq!(
            find_in_sections(&m, &[".text", ".next"], &p),
            Err(ScanError::NotFound)
        );
    }

    // ---- module and section search: the test binary itself ---------------------------------

    fn current_exe_pe() -> PeInfo {
        let file = std::fs::read(std::env::current_exe().unwrap()).unwrap();
        parse_pe(&file).unwrap()
    }

    #[test]
    fn main_module_is_this_exe_and_its_size_matches_the_file() {
        let m = main_module();
        // SAFETY: GetModuleHandleW(NULL) has no preconditions.
        let handle = unsafe { GetModuleHandleW(null()) } as usize;
        assert_eq!(m.base, handle);
        assert_ne!(m.base, 0);
        let on_disk = current_exe_pe();
        assert_eq!(m.size, on_disk.size_of_image as usize);
        assert!(on_disk.sections.iter().any(|s| s.is_named(".text")));
    }

    // Distinct byte strings that live once each in this exe's .rdata.
    static NEEDLE: [u8; 20] = [
        0x5e, 0x31, 0xc7, 0x91, 0x0b, 0xd2, 0x74, 0xa8, 0x13, 0xee, 0x4d, 0x69, 0xf0, 0x27, 0xb6,
        0x3c, 0x88, 0x05, 0xda, 0x71,
    ];
    static TWIN_A: [u8; 12] = [
        0x19, 0x6b, 0xe0, 0x44, 0x01, 0x9d, 0x3a, 0xf7, 0x52, 0xc8, 0x0e, 0x85,
    ];
    static TWIN_B: [u8; 12] = [
        0x19, 0x6b, 0xe0, 0x44, 0x02, 0x9d, 0x3a, 0xf7, 0x52, 0xc8, 0x0e, 0x85,
    ];

    fn hex(bytes: &[u8]) -> String {
        bytes
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<Vec<_>>()
            .join(" ")
    }

    #[test]
    fn finds_a_unique_needle_in_own_rdata_and_reports_its_address() {
        let m = main_module();
        let at = std::hint::black_box(&NEEDLE).as_ptr() as usize;
        assert_eq!(
            find_in_sections(&m, &[".rdata"], &pat(&hex(&NEEDLE))),
            Ok(at)
        );
        // A wildcard in the middle still finds it; the same pattern is absent from .text.
        let mut toks: Vec<String> = NEEDLE.iter().map(|b| format!("{b:02x}")).collect();
        toks[7] = "??".into();
        let wild = pat(&toks.join(" "));
        assert_eq!(find_in_sections(&m, &[".rdata"], &wild), Ok(at));
        assert_eq!(
            find_in_sections(&m, &[".text"], &wild),
            Err(ScanError::NotFound)
        );
        // Uniqueness counts over all the named sections together.
        assert_eq!(find_in_sections(&m, &[".text", ".rdata"], &wild), Ok(at));
        assert_eq!(
            find_in_sections(&m, &[".rdata", ".rdata"], &wild),
            Ok(at),
            "a name given twice counts once"
        );
    }

    #[test]
    fn two_copies_in_own_rdata_are_multiple() {
        let m = main_module();
        let (a, b) = (
            std::hint::black_box(&TWIN_A).as_ptr() as usize,
            std::hint::black_box(&TWIN_B).as_ptr() as usize,
        );
        assert_ne!(a, b);
        let mut toks: Vec<String> = TWIN_A.iter().map(|b| format!("{b:02x}")).collect();
        toks[4] = "??".into();
        assert_eq!(
            find_in_sections(&m, &[".rdata"], &pat(&toks.join(" "))),
            Err(ScanError::Multiple(2))
        );
        // With the differing byte fixed, each is unique.
        assert_eq!(
            find_in_sections(&m, &[".rdata"], &pat(&hex(&TWIN_A))),
            Ok(a)
        );
        assert_eq!(
            find_in_sections(&m, &[".rdata"], &pat(&hex(&TWIN_B))),
            Ok(b)
        );
    }

    #[test]
    fn unknown_section_names_and_bad_modules_are_not_found() {
        let m = main_module();
        let p = pat("5e 31 c7 91");
        assert_eq!(
            find_in_sections(&m, &[".nope"], &p),
            Err(ScanError::NotFound)
        );
        assert_eq!(
            find_in_sections(&m, &[".rdata", ".nope"], &p),
            Err(ScanError::NotFound)
        );
        assert_eq!(find_in_sections(&m, &[], &p), Err(ScanError::NotFound));
        // No headers at the claimed base, or nothing mapped there: refuse without reading.
        let zeros = vec![0u8; 0x2000];
        let on_heap = Module {
            base: zeros.as_ptr() as usize,
            size: 0x2000,
        };
        assert_eq!(
            find_in_sections(&on_heap, &[".text"], &p),
            Err(ScanError::NotFound)
        );
        for base in [0usize, 0x1000, usize::MAX - 0x100] {
            assert_eq!(
                find_in_sections(
                    &Module {
                        base,
                        size: 0x10000
                    },
                    &[".text"],
                    &p
                ),
                Err(ScanError::NotFound),
                "{base:#x}"
            );
        }
        assert_eq!(
            find_in_sections(
                &Module {
                    base: m.base,
                    size: 0
                },
                &[".text"],
                &p
            ),
            Err(ScanError::NotFound)
        );
        // A size too small to hold the section is refused, not clamped.
        assert_eq!(
            find_in_sections(
                &Module {
                    base: m.base,
                    size: 0x1000
                },
                &[".text"],
                &p
            ),
            Err(ScanError::NotFound)
        );
    }

    // ---- module and section search: a synthetic image in committed memory ------------------

    /// A 4-page image: headers, `.text` (page 1), `.rdata` (page 2, half used), page 3 spare.
    fn synth_image(rdata_vsize: u32, size_of_image: u32) -> Pages {
        let pages = Pages::new(4);
        let hdr = synth_pe(
            &[
                (".text", 0x1000, 0x1000, 0x1000, 0x400),
                (".rdata", rdata_vsize, 0x2000, 0x800, 0x1400),
            ],
            size_of_image,
        );
        // SAFETY: the allocation is four RW pages; the header block is 0x400 bytes.
        unsafe { std::ptr::copy_nonoverlapping(hdr.as_ptr(), pages.0 as *mut u8, hdr.len()) };
        pages
    }

    fn poke(pages: &Pages, offset: usize, bytes: &[u8]) {
        assert!(offset + bytes.len() <= pages.1);
        // SAFETY: in bounds of the RW allocation (asserted above).
        unsafe {
            std::ptr::copy_nonoverlapping(
                bytes.as_ptr(),
                (pages.addr() + offset) as *mut u8,
                bytes.len(),
            )
        };
    }

    #[test]
    fn synthetic_image_end_to_end() {
        let needle = [0xc3, 0x9a, 0x17, 0x6e, 0xb2, 0x40];
        let p = pat("c3 9a 17 6e b2 40");
        let pages = synth_image(0x800, 0x4000);
        let m = Module {
            base: pages.addr(),
            size: 0x4000,
        };
        assert_eq!(
            find_in_sections(&m, &[".text"], &p),
            Err(ScanError::NotFound)
        );
        poke(&pages, 0x1100, &needle);
        assert_eq!(find_in_sections(&m, &[".text"], &p), Ok(m.base + 0x1100));
        assert_eq!(
            find_in_sections(&m, &[".rdata"], &p),
            Err(ScanError::NotFound)
        );
        poke(&pages, 0x2010, &needle);
        assert_eq!(find_in_sections(&m, &[".rdata"], &p), Ok(m.base + 0x2010));
        assert_eq!(
            find_in_sections(&m, &[".text", ".rdata"], &p),
            Err(ScanError::Multiple(2))
        );
        assert_eq!(
            find_in_sections(&m, &[".rdata", ".text"], &p),
            Err(ScanError::Multiple(2))
        );
        assert_eq!(
            find_in_sections(&m, &[".text", ".text"], &p),
            Ok(m.base + 0x1100)
        );
        // The header block is not part of any section: bytes there are never searched.
        poke(&pages, 0x300, &needle);
        assert_eq!(find_in_sections(&m, &[".text"], &p), Ok(m.base + 0x1100));
        // Past a section's VirtualSize (0x2800 is the end of .rdata) is not searched either.
        poke(&pages, 0x2900, &needle);
        assert_eq!(find_in_sections(&m, &[".rdata"], &p), Ok(m.base + 0x2010));
        // A match may not run off the end of the section.
        poke(&pages, 0x27fd, &needle[..3]);
        poke(&pages, 0x2800, &needle[3..]);
        assert_eq!(find_in_sections(&m, &[".rdata"], &p), Ok(m.base + 0x2010));
    }

    #[test]
    fn synthetic_image_limits() {
        let p = pat("c3 9a 17 6e b2 40");
        let pages = synth_image(0x800, 0x4000);
        poke(&pages, 0x1100, &[0xc3, 0x9a, 0x17, 0x6e, 0xb2, 0x40]);
        let base = pages.addr();
        // The caller's size bounds the search: .rdata (0x2000..0x2800) is past 0x2000.
        let small = Module { base, size: 0x2000 };
        assert_eq!(find_in_sections(&small, &[".text"], &p), Ok(base + 0x1100));
        assert_eq!(
            find_in_sections(&small, &[".rdata"], &p),
            Err(ScanError::NotFound)
        );
        assert_eq!(
            find_in_sections(&small, &[".text", ".rdata"], &p),
            Err(ScanError::NotFound)
        );
        // The header's SizeOfImage bounds it too, whatever size the caller claims.
        let lying = Module {
            base,
            size: 0x10_0000,
        };
        assert_eq!(find_in_sections(&lying, &[".text"], &p), Ok(base + 0x1100));
        let tight = synth_image(0x800, 0x2400);
        let t = Module {
            base: tight.addr(),
            size: 0x10_0000,
        };
        assert_eq!(
            find_in_sections(&t, &[".rdata"], &p),
            Err(ScanError::NotFound),
            "section ends past SizeOfImage"
        );
        // A VirtualSize that runs past SizeOfImage is refused, not clamped.
        let big = synth_image(0x9000, 0x4000);
        let b = Module {
            base: big.addr(),
            size: 0x4000,
        };
        assert_eq!(
            find_in_sections(&b, &[".rdata"], &p),
            Err(ScanError::NotFound)
        );
    }

    #[test]
    fn unreadable_section_pages_are_not_found_and_not_read() {
        let p = pat("c3 9a 17 6e b2 40");
        let pages = synth_image(0x800, 0x4000);
        poke(&pages, 0x1100, &[0xc3, 0x9a, 0x17, 0x6e, 0xb2, 0x40]);
        let m = Module {
            base: pages.addr(),
            size: 0x4000,
        };
        pages.protect(2, PAGE_NOACCESS);
        assert_eq!(
            find_in_sections(&m, &[".rdata"], &p),
            Err(ScanError::NotFound)
        );
        assert_eq!(
            find_in_sections(&m, &[".text", ".rdata"], &p),
            Err(ScanError::NotFound)
        );
        assert_eq!(
            find_in_sections(&m, &[".text"], &p),
            Ok(m.base + 0x1100),
            "other sections are unaffected"
        );
        pages.protect(2, PAGE_GUARD | PAGE_READWRITE);
        assert_eq!(
            find_in_sections(&m, &[".rdata"], &p),
            Err(ScanError::NotFound)
        );
        // The header page itself unreadable: nothing found, nothing read.
        pages.protect(2, PAGE_READWRITE);
        pages.protect(0, PAGE_NOACCESS);
        assert_eq!(
            find_in_sections(&m, &[".text"], &p),
            Err(ScanError::NotFound)
        );
    }

    #[test]
    fn an_empty_section_is_skipped_but_an_outside_one_fails() {
        let needle = [0xc3, 0x9a, 0x17, 0x6e, 0xb2, 0x40];
        let p = pat("c3 9a 17 6e b2 40");
        let pages = Pages::new(4);
        let hdr = synth_pe(
            &[
                (".text", 0x1000, 0x1000, 0x1000, 0x400),
                // VirtualSize 0 and SizeOfRawData 0: nothing to search, not an error.
                (".empty", 0, 0x2000, 0, 0),
                // Past SizeOfImage: outside the module.
                (".gone", 0x800, 0x9000, 0x800, 0),
            ],
            0x4000,
        );
        poke(&pages, 0, &hdr);
        poke(&pages, 0x1100, &needle);
        let m = Module {
            base: pages.addr(),
            size: 0x4000,
        };
        assert_eq!(
            find_in_sections(&m, &[".text", ".empty"], &p),
            Ok(m.base + 0x1100)
        );
        assert_eq!(
            find_in_sections(&m, &[".empty", ".text"], &p),
            Ok(m.base + 0x1100)
        );
        // Only the empty section named: nothing to find.
        assert_eq!(
            find_in_sections(&m, &[".empty"], &p),
            Err(ScanError::NotFound)
        );
        // A section outside the module still fails the whole search, even next to a good one.
        assert_eq!(
            find_in_sections(&m, &[".gone"], &p),
            Err(ScanError::NotFound)
        );
        assert_eq!(
            find_in_sections(&m, &[".text", ".gone"], &p),
            Err(ScanError::NotFound)
        );
    }

    // ---- the real game exe (ignored: reads ENSHROUDED_EXE, else the Steam install) ----------

    /// The anchors from the RE notes: the virtual address each pattern matches at in build
    /// f68140a3 (sha256 b28b448a...; the Steam exe has the same code at the same addresses). The
    /// game-exe test asserts on them, so a pattern that drifts to a different place fails the
    /// test; it is a build-specific check.
    const ANCHORS: [(&str, u64); 10] = [
        ("SCENE_SETTINGS_FN", 0x1_400f_72b0),
        ("SCATTER_BRICK_LOAD", 0x1_4007_9502),
        ("SCATTER_INSTANCE_LOADS", 0x1_4007_691c),
        ("SCATTER_CAP_CONST", 0x1_4007_4d4c),
        ("SCATTER_CAP_ALLOC", 0x1_4007_45d8),
        ("DECO_HIGH_IMM", 0x1_400d_77c3),
        ("DISTANT_HIGH_IMM", 0x1_400d_7816),
        ("VOXEL_HIGH_IMMS", 0x1_400f_7f1f),
        ("ASYNC_OVERRIDE", 0x1_4084_90d2),
        ("TERRAIN_UPDATE_FN", 0x1_401e_2e80),
    ];

    #[test]
    #[ignore = "reads the game exe (ENSHROUDED_EXE, else the Steam path); a skip shows only with -- --ignored --nocapture"]
    fn sigs_unique_in_game_exe() {
        use crate::build_id::test_support::game_exe_or_skip;
        let Some(path) = game_exe_or_skip("sigs_unique_in_game_exe") else {
            return;
        };
        let file = std::fs::read(&path).expect("read the game exe");
        let info = parse_pe(&file).expect("PE headers of the game exe");
        let mut failures = Vec::new();
        let mut drift = Vec::new();
        for sig in sigs::ALL {
            let p = Pattern::parse(sig.pattern).unwrap();
            // Map each named section by file offset: the part of it that is both in the file and
            // in memory (the lesser of raw size and virtual size).
            let mut regions: Vec<(usize, &[u8])> = Vec::new();
            for name in sig.sections {
                for s in info.sections.iter().filter(|s| s.is_named(name)) {
                    let (rva, len) = s
                        .mem_range(info.size_of_image as usize)
                        .expect("section in image");
                    let raw = len.min(s.raw_size as usize);
                    let start = s.raw_ptr as usize;
                    let bytes = file.get(start..start + raw).expect("section bytes in file");
                    regions.push((info.image_base as usize + rva, bytes));
                }
            }
            assert!(
                !regions.is_empty(),
                "{}: sections {:?} not in the exe",
                sig.name,
                sig.sections
            );
            let anchor = ANCHORS
                .iter()
                .find(|(n, _)| *n == sig.name)
                .map(|(_, a)| *a)
                .expect("anchor");
            match find_unique_in_regions(&regions, &p) {
                Ok(va) => {
                    let sites: Vec<String> = sig
                        .patch_offsets
                        .iter()
                        .map(|o| format!("{:#x}", va + o))
                        .collect();
                    let delta = va as i64 - anchor as i64;
                    let note = if delta == 0 {
                        "= anchor".to_string()
                    } else {
                        format!("anchor {anchor:#x}, delta {delta:+} bytes")
                    };
                    println!(
                        "{:<24} match {va:#x}  patch sites [{}]  ({note})",
                        sig.name,
                        sites.join(", ")
                    );
                    if delta != 0 {
                        drift.push(format!(
                            "{}: matched {va:#x}, anchor {anchor:#x}, delta {delta:+} bytes",
                            sig.name
                        ));
                    }
                }
                Err(e) => failures.push(format!("{}: {e:?}", sig.name)),
            }
        }
        assert!(failures.is_empty(), "not exactly once: {failures:?}");
        assert!(drift.is_empty(), "match differs from its anchor: {drift:?}");
    }
}
