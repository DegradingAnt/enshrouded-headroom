//! Checked writes into live memory, allocation near an address, and `rel32` displacements.
//!
//! [`write_checked`] is the only way this mod changes code or data in the game: it compares the
//! bytes that are there with the bytes the caller expects, and writes only on an exact match.
//! [`alloc_near`] finds memory within reach of a 32-bit displacement, for trampolines and code
//! caves. [`rel32`] turns two addresses into that displacement, or says they are too far apart.
//!
//! # Threads
//!
//! Nothing here is atomic with respect to other threads and nothing takes a lock. A thread that
//! runs or reads the patched bytes while a write is in progress can see a mix of old and new
//! bytes, and two threads that patch the same bytes can both pass the compare. Callers therefore
//! patch code from `InitializeASI`, before the game's threads run, or patch data that no thread
//! is using at that moment. See the "Start-up only" rule on [`write_checked`].
//!
//! # Faults
//!
//! An access violation is a structured exception, not a Rust panic: `catch_unwind` (and so the
//! panic guard) cannot stop it. So nothing here reads or writes an address before `VirtualQuery`
//! has reported every byte of the range committed and readable.

use std::ffi::c_void;
use std::fmt;
use std::mem::size_of;
use std::ptr;

use windows_sys::Win32::Foundation::GetLastError;
use windows_sys::Win32::System::Diagnostics::Debug::FlushInstructionCache;
use windows_sys::Win32::System::Memory::{
    VirtualAlloc, VirtualFree, VirtualProtect, VirtualQuery, MEMORY_BASIC_INFORMATION, MEM_COMMIT,
    MEM_FREE, MEM_RELEASE, MEM_RESERVE, PAGE_EXECUTE_READWRITE, PAGE_READWRITE,
};
use windows_sys::Win32::System::SystemInformation::{GetSystemInfo, SYSTEM_INFO};
use windows_sys::Win32::System::Threading::GetCurrentProcess;

use crate::scan::protect_is_readable;

/// A completed patch: where it went and the bytes it replaced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Patched {
    pub addr: usize,
    pub original: Vec<u8>,
}

/// Why a patch was not applied (or, for [`PatchError::ReadBack`], not confirmed).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PatchError {
    /// The bytes in memory are not the expected ones. Nothing was written and no protection was
    /// changed. `at` is the address of the first byte that differs; `found` is everything read
    /// from the checked range, starting at the `addr` that was passed in.
    Mismatch { at: usize, found: Vec<u8> },
    /// `VirtualProtect` failed. The value is `GetLastError()`. Nothing was written, and every
    /// page that had already been made writable has been put back.
    Protect(u32),
    /// The write was done, but the bytes read back afterwards are not the ones written.
    ReadBack,
    /// Part of the range is not committed, readable memory (unmapped, reserved only, no access,
    /// a guard page, or execute-only), or the range wraps past the end of the address space. The
    /// target was not read and nothing was changed.
    NotMapped,
    /// `expected` and `new` are empty or have different lengths. Nothing was touched.
    BadLength,
}

impl fmt::Display for PatchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PatchError::Mismatch { at, found } => write!(
                f,
                "bytes at {at:#x} are not the expected ones (read {found:02x?})"
            ),
            PatchError::Protect(code) => write!(f, "VirtualProtect failed (error {code})"),
            PatchError::ReadBack => f.write_str("read-back after the write did not match"),
            PatchError::NotMapped => f.write_str("range is not committed, readable memory"),
            PatchError::BadLength => {
                f.write_str("expected and new must be non-empty and equal in length")
            }
        }
    }
}

impl std::error::Error for PatchError {}

// ---- querying memory ----------------------------------------------------------------------

/// One piece of a range that lies in a single `VirtualQuery` region.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Piece {
    addr: usize,
    len: usize,
}

/// What `VirtualQuery` said about the region that holds an address.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Region {
    base: usize,
    size: usize,
    state: u32,
    protect: u32,
}

/// The region that holds `addr`, or `None` if `VirtualQuery` fails (an address outside the
/// user-mode range, for one).
fn query(addr: usize) -> Option<Region> {
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
    (written >= size_of::<MEMORY_BASIC_INFORMATION>()).then_some(Region {
        base: info.BaseAddress as usize,
        size: info.RegionSize,
        state: info.State,
        protect: info.Protect,
    })
}

/// Split `[addr, addr + len)` into one [`Piece`] per `VirtualQuery` region, or `None` unless every
/// byte is committed and readable (the rule of `scan::protect_is_readable`: not a guard page, and
/// execute-only counts as unreadable). Unlike the scanner's walk, the pieces may belong to any
/// allocation: this is used for stack buffers and for pages from [`alloc_near`], not only for the
/// exe image. A range that wraps past the end of the address space is `None`.
fn readable_pieces(addr: usize, len: usize) -> Option<Vec<Piece>> {
    let end = addr.checked_add(len)?;
    let mut pieces = Vec::new();
    let mut at = addr;
    while at < end {
        let r = query(at)?;
        if r.state != MEM_COMMIT || !protect_is_readable(r.protect) {
            return None;
        }
        let region_end = r.base.checked_add(r.size)?;
        // A region that does not contain `at` would make this loop stand still.
        if r.base > at || region_end <= at {
            return None;
        }
        let piece_end = region_end.min(end);
        pieces.push(Piece {
            addr: at,
            len: piece_end.saturating_sub(at),
        });
        at = piece_end;
    }
    Some(pieces)
}

/// Copy `len` bytes from `addr`.
///
/// # Safety
///
/// `[addr, addr + len)` must be readable memory.
unsafe fn copy_out(addr: usize, len: usize) -> Vec<u8> {
    let mut out = vec![0u8; len];
    // SAFETY: the caller guarantees the source is readable for `len` bytes; `out` is a fresh
    // allocation of `len` bytes, so the ranges cannot overlap.
    unsafe { ptr::copy_nonoverlapping(addr as *const u8, out.as_mut_ptr(), len) };
    out
}

/// Copy `len` bytes from `addr`, but only if every byte is committed and readable.
///
/// # Safety
///
/// Same start-up rule as [`write_checked`]: the check is `VirtualQuery` first and the read second,
/// so another thread that frees or re-protects the range in between would make the read fault.
pub(crate) unsafe fn read_checked(addr: usize, len: usize) -> Result<Vec<u8>, PatchError> {
    if len == 0 {
        return Err(PatchError::BadLength);
    }
    readable_pieces(addr, len).ok_or(PatchError::NotMapped)?;
    // SAFETY: `readable_pieces` just reported every byte of the range committed and readable.
    Ok(unsafe { copy_out(addr, len) })
}

// ---- changing protection ------------------------------------------------------------------

/// `VirtualProtect` one piece. Returns the old protection, or `GetLastError()`.
fn set_protect(p: &Piece, protect: u32) -> Result<u32, u32> {
    let mut old = 0u32;
    // SAFETY: `old` is a valid `u32` for the old-protection output. VirtualProtect accepts any
    // address range, touches no memory itself and reports failure by returning 0; the error
    // code is read straight away, before anything else can overwrite it.
    unsafe {
        if VirtualProtect(p.addr as *const c_void, p.len, protect, &mut old) == 0 {
            Err(GetLastError())
        } else {
            Ok(old)
        }
    }
}

/// Put one piece back to the protection `old`; logs a warning if that fails.
fn put_back(p: &Piece, old: u32) -> bool {
    let restored = set_protect(p, old);
    if let Err(code) = restored {
        ::log::warn!(
            "patch: could not restore protection {old:#x} at {:#x} (error {code})",
            p.addr
        );
    }
    restored.is_ok()
}

/// Make each piece writable, in order. If one fails, put back the ones already changed and return
/// that failure's error code. On success returns each piece with the protection it had.
///
/// The two operations are parameters so the rollback can be tested without a page that fails.
fn unprotect_all(
    pieces: &[Piece],
    mut set_writable: impl FnMut(&Piece) -> Result<u32, u32>,
    mut restore: impl FnMut(&Piece, u32) -> bool,
) -> Result<Vec<(Piece, u32)>, u32> {
    let mut changed: Vec<(Piece, u32)> = Vec::with_capacity(pieces.len());
    for piece in pieces {
        match set_writable(piece) {
            Ok(old) => changed.push((*piece, old)),
            Err(code) => {
                restore_all(&changed, &mut restore);
                return Err(code);
            }
        }
    }
    Ok(changed)
}

/// Put every piece back to its old protection. Tries all of them even if one fails; true only if
/// all succeeded.
fn restore_all(changed: &[(Piece, u32)], restore: &mut impl FnMut(&Piece, u32) -> bool) -> bool {
    let mut all_ok = true;
    for (piece, old) in changed {
        all_ok &= restore(piece, *old);
    }
    all_ok
}

// ---- patching -----------------------------------------------------------------------------

/// Replace `expected` with `new` at `addr`, only if memory holds exactly `expected`.
///
/// In order: reject bad lengths; `VirtualQuery` the whole range and refuse it unless every byte
/// is committed and readable (so the compare cannot fault); compare; make the pages
/// `PAGE_EXECUTE_READWRITE`; write; put each page's own old protection back; read the bytes back;
/// `FlushInstructionCache`. Nothing is written on any error before the write, and the old
/// protection is restored whether or not the read-back matches. If restoring a protection fails
/// (it should not), the patch stays in place, the pages stay writable, and a warning is logged.
///
/// # Start-up only
///
/// Call this from `InitializeASI` / `entry::start`, on one thread, before the game's threads run,
/// or on data nothing is using. It is not atomic with respect to other threads (see the module
/// docs), the readability check is `VirtualQuery` first and the read second (another thread that
/// frees or re-protects the range in between would make the read fault), and it logs on failure,
/// which hooks must never do.
///
/// # Safety
///
/// `[addr, addr + expected.len())` must be memory this process may change without breaking
/// anything that is running: not memory that `expected` or `new` borrow, and not code that
/// another thread is executing.
pub unsafe fn write_checked(
    addr: usize,
    expected: &[u8],
    new: &[u8],
) -> Result<Patched, PatchError> {
    let len = expected.len();
    if len == 0 || len != new.len() {
        return Err(PatchError::BadLength);
    }
    let pieces = readable_pieces(addr, len).ok_or(PatchError::NotMapped)?;
    // SAFETY: `readable_pieces` just reported every byte of `[addr, addr + len)` committed and
    // readable.
    let found = unsafe { copy_out(addr, len) };
    if let Some(i) = found.iter().zip(expected).position(|(a, b)| a != b) {
        return Err(PatchError::Mismatch {
            at: addr.saturating_add(i),
            found,
        });
    }
    let changed = unprotect_all(
        &pieces,
        |p| set_protect(p, PAGE_EXECUTE_READWRITE),
        put_back,
    )
    .map_err(PatchError::Protect)?;
    // SAFETY: `unprotect_all` succeeded, so every page of `[addr, addr + len)` is now
    // PAGE_EXECUTE_READWRITE, and the range was committed a moment ago. `ptr::copy` is a memmove,
    // so it is sound even if the caller broke the "do not borrow the target" rule above.
    unsafe { ptr::copy(new.as_ptr(), addr as *mut u8, len) };
    // Restore before anything else can fail, so no path below leaves the pages writable. A page
    // that cannot be restored is logged by `put_back` and stays writable; the patch stays too.
    restore_all(&changed, &mut put_back);
    // SAFETY: the range was readable before the write, and a page is now either back to its old
    // (readable) protection or, if the restore failed, still read-write; so it is readable.
    let back = unsafe { copy_out(addr, len) };
    // SAFETY: GetCurrentProcess returns a pseudo-handle and has no preconditions; the range is
    // committed memory of this process. A failed flush is harmless on x86-64 (the instruction
    // cache is coherent there), so the result is only logged.
    let flushed = unsafe { FlushInstructionCache(GetCurrentProcess(), addr as *const c_void, len) };
    if flushed == 0 {
        ::log::warn!("patch: FlushInstructionCache failed for {addr:#x}");
    }
    if back != new {
        return Err(PatchError::ReadBack);
    }
    Ok(Patched {
        addr,
        original: found,
    })
}

/// [`write_checked`] for one little-endian `u32`.
///
/// # Safety
///
/// Same contract as [`write_checked`] for the four bytes at `addr`.
pub unsafe fn write_u32_checked(
    addr: usize,
    expected: u32,
    new: u32,
) -> Result<Patched, PatchError> {
    // SAFETY: the caller's contract for `write_checked` is this function's contract.
    unsafe { write_checked(addr, &expected.to_le_bytes(), &new.to_le_bytes()) }
}

// ---- rel32 and near allocation ------------------------------------------------------------

/// A 32-bit displacement from the end of an instruction (`next_insn`) to `dest`, or `None` if
/// `dest` is further than 2 GiB behind or 2 GiB - 1 ahead.
pub fn rel32(next_insn: usize, dest: usize) -> Option<i32> {
    // i128 holds any difference of two usize values, so this cannot overflow.
    let delta = (dest as i128).checked_sub(next_insn as i128)?;
    i32::try_from(delta).ok()
}

/// Windows hands out address space in blocks of this size.
const GRANULE: i128 = 0x1_0000;
/// A `rel32` reaches this far behind, and one byte less ahead.
const REACH: i128 = 1 << 31;
/// More region steps than the whole 4 GiB window can have (65536 granules per direction at most,
/// and every step moves at least one granule). A bound for a `query` that misbehaves.
const MAX_STEPS: usize = 1 << 18;

/// The address-space facts the near search needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Limits {
    /// `lpMinimumApplicationAddress`.
    min_addr: usize,
    /// `lpMaximumApplicationAddress`: the last usable address.
    max_addr: usize,
    page: usize,
}

fn limits() -> Limits {
    let mut si = SYSTEM_INFO::default();
    // SAFETY: GetSystemInfo fills the structure it is given and cannot fail.
    unsafe { GetSystemInfo(&mut si) };
    Limits {
        min_addr: si.lpMinimumApplicationAddress as usize,
        max_addr: si.lpMaximumApplicationAddress as usize,
        page: si.dwPageSize as usize,
    }
}

fn round_up(n: i128, to: i128) -> i128 {
    (n + to - 1).div_euclid(to) * to
}

fn round_down(n: i128, to: i128) -> i128 {
    n.div_euclid(to) * to
}

/// Reserve and commit `size` bytes of read-write memory at exactly `addr`.
fn reserve_at(addr: usize, size: usize) -> bool {
    // SAFETY: with an explicit address VirtualAlloc either gets fresh pages there or fails; it
    // never replaces mapped memory.
    let got = unsafe {
        VirtualAlloc(
            addr as *const c_void,
            size,
            MEM_RESERVE | MEM_COMMIT,
            PAGE_READWRITE,
        )
    };
    if got.is_null() {
        return false;
    }
    if got as usize == addr {
        return true;
    }
    // Not the address that was asked for (VirtualAlloc should not do that): give it back.
    // SAFETY: `got` is the base of the allocation that was just made; MEM_RELEASE with size 0
    // frees all of it.
    unsafe { VirtualFree(got, 0, MEM_RELEASE) };
    false
}

/// Reserve and commit `size` bytes of `PAGE_READWRITE` memory such that the whole block (rounded
/// up to whole pages) can be reached by a `rel32` from `target`: every byte of it is within
/// 2 GiB behind or 2 GiB - 1 ahead of `target`, which is the way [`rel32`] measures. Returns its
/// address (64 KiB aligned, zero-filled), or `None` if no such block exists.
///
/// Pass the address the displacement is measured from, that is the address of the instruction
/// after the `jmp`/`call` (`next_insn` in [`rel32`]), not the start of the instruction. The block
/// nearest to `target` that can be had is chosen, looking both below and above it. The block is
/// never freed: it lives as long as the process, as a trampoline or code cave must.
///
/// Walks `VirtualQuery` regions rather than every 64 KiB step, which visits the same candidate
/// addresses (the 64 KiB-aligned starts inside free regions) without querying each one. If the
/// system call that reserves a candidate fails (another thread took it), the search moves on to
/// the next candidate.
pub fn alloc_near(target: usize, size: usize) -> Option<usize> {
    find_near(target, size, limits(), &query, &mut reserve_at)
}

/// The search behind [`alloc_near`], with the address space (`query`) and the reservation
/// (`alloc`) as parameters so it can be tested on a made-up address space.
///
/// All arithmetic is in `i128`, so `target - 2 GiB` can go negative without wrapping.
fn find_near(
    target: usize,
    size: usize,
    lim: Limits,
    query: &dyn Fn(usize) -> Option<Region>,
    alloc: &mut dyn FnMut(usize, usize) -> bool,
) -> Option<usize> {
    if size == 0 || lim.page == 0 {
        return None;
    }
    let span = round_up(size as i128, lim.page as i128);
    let t = target as i128;
    // Block starts that are on the 64 KiB grid, inside the usable address range, and keep the
    // first and the last byte of the block within a rel32 of `target`.
    let lo = round_up((t - REACH).max(lim.min_addr as i128), GRANULE);
    let hi = round_down(
        (t + REACH - span).min(lim.max_addr as i128 + 1 - span),
        GRANULE,
    );
    if hi < lo {
        return None;
    }
    let fits = |a: i128| {
        rel32(target, a as usize).is_some() && rel32(target, (a + span - 1) as usize).is_some()
    };
    // Two cursors walk away from the target, one up and one down, taking turns. Each is always a
    // multiple of GRANULE; a cursor outside [lo, hi] is finished.
    let start = t.clamp(lo, hi);
    let mut up = round_up(start, GRANULE);
    let mut down = if start % GRANULE == 0 {
        start - GRANULE
    } else {
        round_down(start, GRANULE)
    };
    let mut steps = 0usize;
    while steps < MAX_STEPS && (up <= hi || down >= lo) {
        if up <= hi {
            steps += 1;
            up = match query(up as usize) {
                None => hi + 1,
                Some(r) => {
                    let (base, end) = (r.base as i128, r.base as i128 + r.size as i128);
                    if base > up || end <= up {
                        // A region that does not hold the address it was asked about.
                        up + GRANULE
                    } else if r.state == MEM_FREE && up + span <= end {
                        if fits(up) && alloc(up as usize, size) {
                            return Some(up as usize);
                        }
                        up + GRANULE
                    } else {
                        round_up(end, GRANULE).max(up + GRANULE)
                    }
                }
            };
        }
        if down >= lo {
            steps += 1;
            down = match query(down as usize) {
                None => lo - 1,
                Some(r) => {
                    let (base, end) = (r.base as i128, r.base as i128 + r.size as i128);
                    if base > down || end <= down {
                        down - GRANULE
                    } else if r.state != MEM_FREE {
                        // Jump below the occupied region.
                        round_down(base - 1, GRANULE).min(down - GRANULE)
                    } else if down + span > end {
                        // The block would run out of the free region: the highest start that
                        // ends inside it.
                        round_down(end - span, GRANULE).min(down - GRANULE)
                    } else {
                        if fits(down) && alloc(down as usize, size) {
                            return Some(down as usize);
                        }
                        down - GRANULE
                    }
                }
            };
        }
    }
    None
}

/// Memory helpers shared by the tests of this module and of `iat`.
#[cfg(test)]
pub(crate) mod test_support {
    use std::ptr::null;
    use windows_sys::Win32::System::Memory::{
        VirtualAlloc, VirtualFree, VirtualProtect, VirtualQuery, MEMORY_BASIC_INFORMATION,
        MEM_COMMIT, MEM_RELEASE, MEM_RESERVE, PAGE_READWRITE,
    };

    /// Some `VirtualAlloc` memory that is released on drop.
    pub(crate) struct Mem {
        pub(crate) base: usize,
    }

    impl Mem {
        pub(crate) fn new(size: usize, kind: u32, protect: u32) -> Mem {
            let p = unsafe { VirtualAlloc(null(), size, kind, protect) };
            assert!(!p.is_null(), "VirtualAlloc failed");
            Mem { base: p as usize }
        }

        /// Committed read-write memory.
        pub(crate) fn rw(size: usize) -> Mem {
            Mem::new(size, MEM_RESERVE | MEM_COMMIT, PAGE_READWRITE)
        }

        pub(crate) fn put(&self, off: usize, bytes: &[u8]) {
            unsafe {
                std::ptr::copy_nonoverlapping(
                    bytes.as_ptr(),
                    (self.base + off) as *mut u8,
                    bytes.len(),
                )
            };
        }

        pub(crate) fn get(&self, off: usize, len: usize) -> Vec<u8> {
            let mut out = vec![0u8; len];
            unsafe {
                std::ptr::copy_nonoverlapping((self.base + off) as *const u8, out.as_mut_ptr(), len)
            };
            out
        }

        pub(crate) fn protect(&self, off: usize, len: usize, protect: u32) {
            let mut old = 0;
            let ok =
                unsafe { VirtualProtect((self.base + off) as *const _, len, protect, &mut old) };
            assert!(ok != 0, "VirtualProtect failed");
        }
    }

    impl Drop for Mem {
        fn drop(&mut self) {
            unsafe { VirtualFree(self.base as *mut _, 0, MEM_RELEASE) };
        }
    }

    pub(crate) fn release(addr: usize) {
        unsafe { VirtualFree(addr as *mut _, 0, MEM_RELEASE) };
    }

    pub(crate) fn info(addr: usize) -> MEMORY_BASIC_INFORMATION {
        let mut info = MEMORY_BASIC_INFORMATION::default();
        let n = unsafe {
            VirtualQuery(
                addr as *const _,
                &mut info,
                std::mem::size_of::<MEMORY_BASIC_INFORMATION>(),
            )
        };
        assert!(n > 0, "VirtualQuery failed");
        info
    }

    pub(crate) fn protect_of(addr: usize) -> u32 {
        info(addr).Protect
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows_sys::Win32::System::Memory::{
        VirtualAlloc, MEM_COMMIT, MEM_FREE, MEM_RESERVE, PAGE_EXECUTE, PAGE_EXECUTE_READ,
        PAGE_GUARD, PAGE_NOACCESS, PAGE_READONLY, PAGE_READWRITE,
    };

    const PAGE: usize = 0x1000;

    // ---- the tests from the brief ---------------------------------------------------------

    #[test]
    fn refuses_unexpected_bytes() {
        let mut buf = [1u8, 2, 3, 4];
        let r = unsafe { write_checked(buf.as_mut_ptr() as usize, &[9, 9], &[5, 6]) };
        assert!(matches!(r, Err(PatchError::Mismatch { .. })));
        assert_eq!(buf, [1, 2, 3, 4]);
    }

    #[test]
    fn writes_and_keeps_original() {
        let mut buf = [1u8, 2, 3, 4];
        let p = unsafe { write_checked(buf.as_mut_ptr() as usize + 1, &[2, 3], &[7, 8]) }.unwrap();
        assert_eq!(buf, [1, 7, 8, 4]);
        assert_eq!(p.original, vec![2, 3]);
    }

    #[test]
    fn alloc_near_is_in_rel32_range() {
        let here = alloc_near_is_in_rel32_range as *const () as usize;
        let a = alloc_near(here, 4096).unwrap();
        assert!(rel32(here, a).is_some() && rel32(here, a + 4095).is_some());
        release(a);
    }

    use super::test_support::{info, protect_of, release, Mem};

    // ---- write_checked: lengths -----------------------------------------------------------

    #[test]
    fn empty_or_unequal_lengths_are_refused_and_touch_nothing() {
        let mut buf = [1u8, 2, 3, 4];
        let at = buf.as_mut_ptr() as usize;
        for (e, n) in [
            (&[][..], &[][..]),
            (&[1, 2][..], &[7][..]),
            (&[1][..], &[7, 8][..]),
        ] {
            let r = unsafe { write_checked(at, e, n) };
            assert_eq!(r, Err(PatchError::BadLength));
        }
        assert_eq!(buf, [1, 2, 3, 4]);
    }

    #[test]
    fn mismatch_reports_first_differing_address_and_all_bytes_found() {
        let mut buf = [1u8, 2, 3, 4];
        let at = buf.as_mut_ptr() as usize;
        let r = unsafe { write_checked(at, &[1, 2, 9, 4], &[0, 0, 0, 0]) };
        assert_eq!(
            r,
            Err(PatchError::Mismatch {
                at: at + 2,
                found: vec![1, 2, 3, 4]
            })
        );
        assert_eq!(buf, [1, 2, 3, 4]);
    }

    #[test]
    fn write_u32_roundtrip_and_mismatch() {
        let mut word = 0x1122_3344u32;
        let at = &mut word as *mut u32 as usize;
        let p = unsafe { write_u32_checked(at, 0x1122_3344, 0xdead_beef) }.unwrap();
        assert_eq!(p.original, 0x1122_3344u32.to_le_bytes().to_vec());
        assert_eq!(word, 0xdead_beef);
        let r = unsafe { write_u32_checked(at, 0x1122_3344, 1) };
        assert!(matches!(r, Err(PatchError::Mismatch { .. })));
        assert_eq!(word, 0xdead_beef);
    }

    // ---- write_checked: memory that must not be read (hazard 3) ----------------------------

    #[test]
    fn null_and_wrapping_ranges_are_not_mapped() {
        assert_eq!(
            unsafe { write_checked(0, &[0], &[1]) },
            Err(PatchError::NotMapped)
        );
        assert_eq!(
            unsafe { write_checked(usize::MAX, &[0, 0], &[1, 1]) },
            Err(PatchError::NotMapped)
        );
        assert_eq!(
            unsafe { write_checked(usize::MAX - 1, &[0, 0, 0], &[1, 1, 1]) },
            Err(PatchError::NotMapped)
        );
    }

    #[test]
    fn no_access_reserved_guard_and_execute_only_pages_are_not_read() {
        let none = Mem::new(PAGE, MEM_RESERVE | MEM_COMMIT, PAGE_NOACCESS);
        let reserved = Mem::new(PAGE, MEM_RESERVE, PAGE_NOACCESS);
        let guard = Mem::new(PAGE, MEM_RESERVE | MEM_COMMIT, PAGE_READWRITE | PAGE_GUARD);
        let exec_only = Mem::new(PAGE, MEM_RESERVE | MEM_COMMIT, PAGE_EXECUTE);
        for (name, m, protect) in [
            ("no access", &none, PAGE_NOACCESS),
            ("reserved", &reserved, PAGE_NOACCESS),
            ("guard", &guard, PAGE_READWRITE | PAGE_GUARD),
            ("execute only", &exec_only, PAGE_EXECUTE),
        ] {
            let r = unsafe { write_checked(m.base, &[0, 0], &[1, 1]) };
            assert_eq!(r, Err(PatchError::NotMapped), "{name}");
            if name != "reserved" {
                // Nothing was touched, and the guard page was not triggered (which would have
                // cleared PAGE_GUARD).
                assert_eq!(protect_of(m.base), protect, "{name}: protection changed");
            }
        }
    }

    #[test]
    fn a_range_that_runs_into_uncommitted_memory_is_refused_whole() {
        // Two pages reserved, only the first committed.
        let m = Mem::new(2 * PAGE, MEM_RESERVE, PAGE_NOACCESS);
        let committed =
            unsafe { VirtualAlloc(m.base as *const _, PAGE, MEM_COMMIT, PAGE_READWRITE) };
        assert!(!committed.is_null());
        m.put(PAGE - 2, &[5, 6]);
        let r = unsafe { write_checked(m.base + PAGE - 2, &[5, 6, 0, 0], &[9, 9, 9, 9]) };
        assert_eq!(r, Err(PatchError::NotMapped));
        assert_eq!(
            m.get(PAGE - 2, 2),
            vec![5, 6],
            "the committed part must be untouched"
        );
        assert_eq!(protect_of(m.base), PAGE_READWRITE);
    }

    // ---- write_checked: protection (hazards 4) ---------------------------------------------

    #[test]
    fn read_only_and_execute_read_pages_are_patched_and_put_back() {
        for protect in [PAGE_READONLY, PAGE_EXECUTE_READ] {
            let m = Mem::rw(PAGE);
            m.put(8, &[1, 2, 3, 4]);
            m.protect(0, PAGE, protect);
            let p = unsafe { write_checked(m.base + 8, &[1, 2, 3, 4], &[9, 8, 7, 6]) }.unwrap();
            assert_eq!(p.original, vec![1, 2, 3, 4]);
            assert_eq!(protect_of(m.base), protect);
            m.protect(0, PAGE, PAGE_READWRITE);
            assert_eq!(m.get(8, 4), vec![9, 8, 7, 6]);
        }
    }

    #[test]
    fn a_range_over_two_regions_puts_each_regions_own_protection_back() {
        let m = Mem::rw(2 * PAGE);
        m.put(PAGE - 2, &[1, 2, 3, 4]);
        m.protect(PAGE, PAGE, PAGE_READONLY);
        assert_eq!(protect_of(m.base), PAGE_READWRITE);
        assert_eq!(protect_of(m.base + PAGE), PAGE_READONLY);
        unsafe { write_checked(m.base + PAGE - 2, &[1, 2, 3, 4], &[5, 6, 7, 8]) }.unwrap();
        assert_eq!(protect_of(m.base), PAGE_READWRITE, "first page");
        assert_eq!(protect_of(m.base + PAGE), PAGE_READONLY, "second page");
        m.protect(PAGE, PAGE, PAGE_READWRITE);
        assert_eq!(m.get(PAGE - 2, 4), vec![5, 6, 7, 8]);
    }

    #[test]
    fn a_page_that_cannot_be_made_writable_is_a_protect_error_and_nothing_changes() {
        // KUSER_SHARED_DATA is a read-only view that every 64-bit Windows process has at this
        // address; `NtMajorVersion` sits at +0x26c and never changes. VirtualProtect refuses to
        // make it writable. (`new` equals `expected`, so even a surprise success writes nothing.)
        const KUSER: usize = 0x7ffe_0000;
        let at = KUSER + 0x26c;
        let major = unsafe { *(at as *const u32) };
        let before = protect_of(KUSER);
        let r = unsafe { write_u32_checked(at, major, major) };
        eprintln!("VirtualProtect(RWX) on KUSER_SHARED_DATA: {r:?}");
        assert!(
            matches!(r, Err(PatchError::Protect(code)) if code != 0),
            "{r:?}"
        );
        assert_eq!(protect_of(KUSER), before);
    }

    // ---- the page-protection rollback logic, with a fake VirtualProtect --------------------

    #[test]
    fn a_failure_part_way_restores_the_pages_already_changed() {
        let pieces = [
            Piece {
                addr: 0x1000,
                len: 16,
            },
            Piece {
                addr: 0x2000,
                len: 16,
            },
            Piece {
                addr: 0x3000,
                len: 16,
            },
        ];
        let mut log: Vec<String> = Vec::new();
        let mut restored: Vec<(usize, u32)> = Vec::new();
        let r = unprotect_all(
            &pieces,
            |p| {
                log.push(format!("set {:#x}", p.addr));
                match p.addr {
                    0x1000 => Ok(PAGE_READONLY),
                    0x2000 => Ok(PAGE_EXECUTE_READ),
                    _ => Err(87),
                }
            },
            |p, old| {
                restored.push((p.addr, old));
                true
            },
        );
        assert_eq!(r.map(|_| ()), Err(87));
        restored.sort_unstable();
        assert_eq!(
            restored,
            vec![(0x1000, PAGE_READONLY), (0x2000, PAGE_EXECUTE_READ)]
        );
        assert_eq!(log, ["set 0x1000", "set 0x2000", "set 0x3000"]);
    }

    #[test]
    fn restore_all_tries_every_page_and_reports_any_failure() {
        let changed = [
            (
                Piece {
                    addr: 0x1000,
                    len: 1,
                },
                PAGE_READONLY,
            ),
            (
                Piece {
                    addr: 0x2000,
                    len: 1,
                },
                PAGE_READONLY,
            ),
            (
                Piece {
                    addr: 0x3000,
                    len: 1,
                },
                PAGE_READONLY,
            ),
        ];
        let mut tried = Vec::new();
        let all_ok = restore_all(&changed, &mut |p, _| {
            tried.push(p.addr);
            p.addr != 0x2000
        });
        assert!(!all_ok);
        assert_eq!(tried, vec![0x1000, 0x2000, 0x3000]);
        assert!(restore_all(&changed, &mut |_, _| true));
    }

    // ---- rel32 (hazard 6) ------------------------------------------------------------------

    #[test]
    fn rel32_edges() {
        let next = 0x1_4000_0000usize;
        assert_eq!(rel32(next, next), Some(0));
        assert_eq!(rel32(next, next + 0x7fff_ffff), Some(i32::MAX));
        assert_eq!(rel32(next, next + 0x8000_0000), None);
        assert_eq!(rel32(next, next - 0x8000_0000), Some(i32::MIN));
        assert_eq!(rel32(next, next - 0x8000_0001), None);
        assert_eq!(rel32(next, next + 5), Some(5));
        assert_eq!(rel32(next, next - 5), Some(-5));
    }

    #[test]
    fn rel32_cannot_overflow_at_the_ends_of_the_address_space() {
        assert_eq!(rel32(usize::MAX, 0), None);
        assert_eq!(rel32(0, usize::MAX), None);
        assert_eq!(rel32(usize::MAX, usize::MAX), Some(0));
        assert_eq!(rel32(usize::MAX, usize::MAX - 7), Some(-7));
        assert_eq!(rel32(0, 0x7fff_ffff), Some(i32::MAX));
        assert_eq!(rel32(0, 0x8000_0000), None);
        assert_eq!(rel32(usize::MAX - 0x7fff_ffff, usize::MAX), Some(i32::MAX));
        assert_eq!(rel32(usize::MAX - 0x8000_0000, usize::MAX), None);
    }

    // ---- alloc_near: the search, on a fake address space (hazard 5) ------------------------

    const LIMITS: Limits = Limits {
        min_addr: 0x1_0000,
        max_addr: 0x7fff_fffe_ffff,
        page: PAGE,
    };
    const TOP: usize = 0x8000_0000_0000;
    const G: usize = 0x1_0000;
    const GIB2: usize = 0x8000_0000;

    /// An address space that is occupied except for the listed free ranges `(start, end)`.
    struct Map(Vec<(usize, usize, bool)>);

    impl Map {
        fn with_free(free: &[(usize, usize)]) -> Map {
            let mut regions = Vec::new();
            let mut at = 0;
            for &(s, e) in free {
                if s > at {
                    regions.push((at, s, false));
                }
                regions.push((s, e, true));
                at = e;
            }
            if at < TOP {
                regions.push((at, TOP, false));
            }
            Map(regions)
        }

        fn query(&self, a: usize) -> Option<Region> {
            let &(s, e, free) = self.0.iter().find(|&&(s, e, _)| a >= s && a < e)?;
            Some(Region {
                base: s,
                size: e - s,
                state: if free { MEM_FREE } else { MEM_RESERVE },
                protect: 0,
            })
        }
    }

    /// Run `find_near` on `map`; `accept(addr, size)` decides whether each allocation attempt
    /// succeeds. Returns the result and every address that was tried.
    fn search(
        map: &Map,
        target: usize,
        size: usize,
        mut accept: impl FnMut(usize, usize) -> bool,
    ) -> (Option<usize>, Vec<usize>) {
        let mut tries = Vec::new();
        let got = find_near(target, size, LIMITS, &|a| map.query(a), &mut |a, n| {
            tries.push(a);
            accept(a, n)
        });
        (got, tries)
    }

    fn reach_ok(target: usize, a: usize, size: usize) -> bool {
        rel32(target, a).is_some() && rel32(target, a + size - 1).is_some()
    }

    #[test]
    fn a_low_target_does_not_underflow_and_stays_above_the_minimum_address() {
        let map = Map::with_free(&[(0x1_0000, TOP)]);
        for target in [0usize, 1, 0x1_0000, 0x2_0000, 0x7fff_ffff] {
            let (got, _) = search(&map, target, 4096, |_, _| true);
            let a = got.unwrap_or_else(|| panic!("no block for target {target:#x}"));
            assert!(a >= LIMITS.min_addr, "{a:#x}");
            assert_eq!(a % G, 0);
            assert!(reach_ok(target, a, 4096), "target {target:#x} -> {a:#x}");
        }
        // Only the bottom of the address space is free, and it is below the target.
        let map = Map::with_free(&[(0x1_0000, 0x1_8000)]);
        let (got, _) = search(&map, 0x2_0000, 4096, |_, _| true);
        assert_eq!(got, Some(0x1_0000));
        // Nothing is free: a clean None, with no wrap to the top of the address space.
        let map = Map::with_free(&[]);
        assert_eq!(search(&map, 0x2_0000, 4096, |_, _| true).0, None);
    }

    #[test]
    fn it_finds_free_memory_below_the_target_and_above_it() {
        let target = 0x1_4000_0000;
        let below = (target - 0x1000_0000, target - 0x0f00_0000);
        let above = (target + 0x1000_0000, target + 0x1100_0000);
        let (got, _) = search(&Map::with_free(&[below]), target, 4096, |_, _| true);
        let a = got.expect("free memory below the target");
        assert!(a >= below.0 && a + 4096 <= below.1 && a < target, "{a:#x}");
        let (got, _) = search(&Map::with_free(&[above]), target, 4096, |_, _| true);
        let a = got.expect("free memory above the target");
        assert!(a >= above.0 && a + 4096 <= above.1 && a > target, "{a:#x}");
        let (got, _) = search(&Map::with_free(&[below, above]), target, 4096, |_, _| true);
        let a = got.expect("free memory on both sides");
        assert!(reach_ok(target, a, 4096));
    }

    #[test]
    fn the_block_starts_on_a_granule_and_ends_inside_its_free_region() {
        let target = 0x1_4000_0000;
        // The free region starts and ends off the 64 KiB grid.
        let free = (target - 0x40_0000 + 0x1000, target - 0x40_0000 + 0x2_0800);
        let (got, tries) = search(&Map::with_free(&[free]), target, 0x1000, |_, _| true);
        let a = got.expect("block");
        assert_eq!(a % G, 0);
        assert!(a >= free.0 && a + 0x1000 <= free.1, "{a:#x}");
        assert_eq!(tries, vec![a]);
        // A region that cannot hold the block is never tried, even one byte short.
        let free = (target - 0x40_0000, target - 0x40_0000 + 0x1fff);
        let (got, tries) = search(&Map::with_free(&[free]), target, 0x2000, |_, _| true);
        assert_eq!((got, tries), (None, vec![]));
    }

    #[test]
    fn the_whole_block_must_be_in_reach_not_just_its_start() {
        let target = 0x1_0000_0000;
        let last_ok = target + GIB2 - G;
        let fits = |free: [(usize, usize); 1], size| {
            search(&Map::with_free(&free), target, size, |_, _| true).0
        };
        // Highest start whose last byte is still reachable: found.
        assert_eq!(fits([(last_ok, last_ok + G)], 0x1000), Some(last_ok));
        // One granule further: out of reach.
        assert_eq!(fits([(last_ok + G, last_ok + 2 * G)], 0x1000), None);
        // A block longer than a granule does not fit at that last start, whether or not the free
        // region goes on past it: its last byte would be out of reach.
        assert_eq!(fits([(last_ok, last_ok + G)], G + 1), None);
        assert_eq!(fits([(last_ok, last_ok + 2 * G)], G + 1), None);
        assert_eq!(fits([(last_ok, last_ok + 2 * G)], G), Some(last_ok));
        // The lowest start is exactly 2 GiB behind the target (displacement i32::MIN).
        let first_ok = target - GIB2;
        assert_eq!(fits([(first_ok, first_ok + G)], 0x1000), Some(first_ok));
        assert_eq!(fits([(first_ok - G, first_ok)], 0x1000), None);
    }

    #[test]
    fn a_size_is_rounded_up_to_whole_pages_before_the_reach_check() {
        // The last reachable byte is target + 2 GiB - 1 = 0x1_8000_07ff. A block of 0x800 bytes at
        // 0x1_8000_0000 would end exactly there, but it commits a whole 0x1000-byte page, which
        // runs 0x800 bytes past it.
        let target = 0x1_0000_0800;
        let at = |start: usize| Map::with_free(&[(start, start + G)]);
        assert_eq!(
            search(&at(0x1_8000_0000), target, 0x800, |_, _| true).0,
            None
        );
        assert_eq!(
            search(&at(0x1_7fff_0000), target, 0x800, |_, _| true).0,
            Some(0x1_7fff_0000)
        );
    }

    #[test]
    fn a_failed_allocation_moves_on_to_the_next_candidate() {
        let map = Map::with_free(&[(0x1_0000, TOP)]);
        let target = 0x1_4000_0000;
        let (got, tries) = search(&map, target, 4096, |_, _| false);
        assert_eq!(got, None);
        // Bounded, distinct, aligned and in reach.
        assert!(
            tries.len() <= 2 * (2 * GIB2 / G) + 2,
            "{} attempts",
            tries.len()
        );
        let mut sorted = tries.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), tries.len(), "an address was tried twice");
        assert!(tries
            .iter()
            .all(|&a| a % G == 0 && reach_ok(target, a, 4096)));
        // Both sides were tried.
        assert!(tries.iter().any(|&a| a < target) && tries.iter().any(|&a| a > target));
        // Rejecting the first three attempts gives the fourth.
        let mut count = 0;
        let (got, tries) = search(&map, target, 4096, |_, _| {
            count += 1;
            count > 3
        });
        assert_eq!(tries.len(), 4);
        assert_eq!(got, tries.last().copied());
    }

    #[test]
    fn nonsense_sizes_and_targets_give_none() {
        let map = Map::with_free(&[(0x1_0000, TOP)]);
        let target = 0x1_4000_0000;
        for size in [0, usize::MAX, usize::MAX - 0xfff, 2 * GIB2 + 1, 4 * GIB2] {
            assert_eq!(
                search(&map, target, size, |_, _| true).0,
                None,
                "size {size:#x}"
            );
        }
        for target in [usize::MAX, TOP + GIB2 + 0x1_0000, 0x7fff_ffff_0000 + GIB2] {
            assert_eq!(
                search(&map, target, 4096, |_, _| true).0,
                None,
                "target {target:#x}"
            );
        }
    }

    #[test]
    fn the_top_of_user_space_is_in_reach_of_a_target_just_above_it() {
        // 2^47 is above the last usable address, but the last blocks below it are in reach.
        let map = Map::with_free(&[(0x1_0000, TOP)]);
        let (got, _) = search(&map, TOP, 4096, |_, _| true);
        let a = got.expect("block near the top of user space");
        assert!(
            a + 4096 <= LIMITS.max_addr + 1 && reach_ok(TOP, a, 4096),
            "{a:#x}"
        );
    }

    #[test]
    fn a_lying_or_failing_query_cannot_loop_forever() {
        // A region of size 0 never moves the cursor by itself.
        let queries = std::cell::Cell::new(0usize);
        let got = find_near(
            0x1_4000_0000,
            4096,
            LIMITS,
            &|a| {
                queries.set(queries.get() + 1);
                Some(Region {
                    base: a,
                    size: 0,
                    state: MEM_FREE,
                    protect: 0,
                })
            },
            &mut |_, _| false,
        );
        assert_eq!(got, None);
        assert!(queries.get() <= 4 * GIB2 / G, "{} queries", queries.get());
        // A query that fails means that direction is done.
        let got = find_near(0x1_4000_0000, 4096, LIMITS, &|_| None, &mut |_, _| true);
        assert_eq!(got, None);
    }

    // ---- alloc_near: the real thing --------------------------------------------------------

    #[test]
    fn alloc_near_gives_zeroed_read_write_memory_in_reach() {
        let here = alloc_near_gives_zeroed_read_write_memory_in_reach as *const () as usize;
        let a = alloc_near(here, 5000).expect("block");
        assert_eq!(a % G, 0);
        assert!(reach_ok(here, a, 2 * PAGE), "{a:#x}");
        let i = info(a);
        assert_eq!((i.State, i.Protect), (MEM_COMMIT, PAGE_READWRITE));
        assert!(i.RegionSize >= 2 * PAGE);
        let block = unsafe { std::slice::from_raw_parts_mut(a as *mut u8, 5000) };
        assert!(block.iter().all(|&b| b == 0));
        block.fill(0xab);
        let b = alloc_near(here, 5000).expect("second block");
        assert_ne!(a, b, "a second request must not get the same block");
        release(a);
        release(b);
    }

    #[test]
    fn alloc_near_a_target_in_the_low_2_gib_does_not_underflow() {
        // `target - 2 GiB` would wrap here.
        let a = alloc_near(0x2_0000, 4096).expect("block in the low 2 GiB");
        assert!(a >= 0x1_0000 && reach_ok(0x2_0000, a, 4096), "{a:#x}");
        release(a);
    }

    #[test]
    fn alloc_near_edges_of_the_address_space() {
        assert_eq!(alloc_near(usize::MAX, 4096), None);
        assert_eq!(alloc_near(1 << 48, 4096), None);
        assert_eq!(alloc_near(0x1_4000_0000, 0), None);
        assert_eq!(alloc_near(0x1_4000_0000, usize::MAX), None);
    }
}
