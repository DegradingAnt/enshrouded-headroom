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
    MEM_FREE, MEM_RELEASE, MEM_RESERVE, PAGE_EXECUTE, PAGE_EXECUTE_READ, PAGE_EXECUTE_READWRITE,
    PAGE_EXECUTE_WRITECOPY, PAGE_READWRITE,
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
    /// `VirtualProtect` failed. The value is `GetLastError()`. Nothing was written. Every page
    /// that had already been made writable was put back, or a warning was logged if that failed.
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
    /// The protection of the `VirtualQuery` region the piece lies in, as it was reported. It
    /// decides what [`writable_protection`] asks for.
    protect: u32,
}

/// What `VirtualQuery` said about the region that holds an address.
///
/// `base` is the queried address rounded down to a page, not the start of the allocation: the
/// region reported is the run of like pages that starts at that page. `alloc_base` is where the
/// allocation containing the page starts (0 for free memory).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Region {
    base: usize,
    alloc_base: usize,
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
        alloc_base: info.AllocationBase as usize,
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
            protect: r.protect,
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

/// The protection bits that mean "code may run from this page".
const EXECUTABLE: u32 =
    PAGE_EXECUTE | PAGE_EXECUTE_READ | PAGE_EXECUTE_READWRITE | PAGE_EXECUTE_WRITECOPY;

/// The protection to ask for to make a page whose current protection is `current` writable:
/// `PAGE_EXECUTE_READWRITE` if code may run from it already, otherwise `PAGE_READWRITE`.
///
/// A data patch (the `.rdata` texture table, say) must not turn a page that cannot execute into
/// one that can, even for a moment: a policy that forbids dynamic code (ACG) refuses a page that is
/// both writable and executable.
fn writable_protection(current: u32) -> u32 {
    if current & EXECUTABLE != 0 {
        PAGE_EXECUTE_READWRITE
    } else {
        PAGE_READWRITE
    }
}

/// The operating-system calls [`write_checked_with`] makes besides `VirtualQuery`, as a trait so
/// the tests can make each of them fail or record when it is called.
trait Os {
    /// Make the pages of `p` writable, with the protection [`writable_protection`] gives for
    /// `p.protect`. Returns their old protection, or `GetLastError()`.
    fn set_writable(&mut self, p: &Piece) -> Result<u32, u32>;

    /// Put the pages of `p` back to the protection `old`. The error is `GetLastError()`.
    fn restore(&mut self, p: &Piece, old: u32) -> Result<(), u32>;

    /// Copy `len` bytes from `addr`.
    ///
    /// # Safety
    ///
    /// `[addr, addr + len)` must be readable memory.
    unsafe fn read(&mut self, addr: usize, len: usize) -> Vec<u8>;

    /// `FlushInstructionCache` over the range. False on failure.
    fn flush(&mut self, addr: usize, len: usize) -> bool;

    /// Record a warning.
    fn warn(&mut self, message: &str);
}

/// The real calls.
struct WindowsOs;

impl Os for WindowsOs {
    fn set_writable(&mut self, p: &Piece) -> Result<u32, u32> {
        set_protect(p, writable_protection(p.protect))
    }

    fn restore(&mut self, p: &Piece, old: u32) -> Result<(), u32> {
        set_protect(p, old).map(|_| ())
    }

    unsafe fn read(&mut self, addr: usize, len: usize) -> Vec<u8> {
        // SAFETY: the caller guarantees `[addr, addr + len)` is readable.
        unsafe { copy_out(addr, len) }
    }

    fn flush(&mut self, addr: usize, len: usize) -> bool {
        // SAFETY: GetCurrentProcess returns a pseudo-handle and has no preconditions.
        // FlushInstructionCache only reads its arguments (it never dereferences the address) and
        // reports failure by returning 0.
        unsafe { FlushInstructionCache(GetCurrentProcess(), addr as *const c_void, len) != 0 }
    }

    fn warn(&mut self, message: &str) {
        ::log::warn!("{message}");
    }
}

/// Make each piece writable, in order. If one fails, put back the ones already changed (a warning
/// for each that cannot be) and return that failure's error code. On success returns each piece
/// with the protection it had.
fn unprotect_all(pieces: &[Piece], os: &mut impl Os) -> Result<Vec<(Piece, u32)>, u32> {
    let mut changed: Vec<(Piece, u32)> = Vec::with_capacity(pieces.len());
    for piece in pieces {
        match os.set_writable(piece) {
            Ok(old) => changed.push((*piece, old)),
            Err(code) => {
                restore_all(&changed, os);
                return Err(code);
            }
        }
    }
    Ok(changed)
}

/// Put every piece back to its old protection, with a warning for each one that cannot be. Tries
/// all of them even if one fails; true only if all succeeded.
fn restore_all(changed: &[(Piece, u32)], os: &mut impl Os) -> bool {
    let mut all_ok = true;
    for (piece, old) in changed {
        if let Err(code) = os.restore(piece, *old) {
            all_ok = false;
            os.warn(&format!(
                "patch: could not restore protection {old:#x} at {:#x} (error {code})",
                piece.addr
            ));
        }
    }
    all_ok
}

// ---- patching -----------------------------------------------------------------------------

/// Replace `expected` with `new` at `addr`, only if memory holds exactly `expected`.
///
/// In order: reject bad lengths; `VirtualQuery` the whole range and refuse it unless every byte
/// is committed and readable (so the compare cannot fault); compare; make each region writable
/// (`PAGE_READWRITE` for a region that cannot execute, `PAGE_EXECUTE_READWRITE` for one that can,
/// see `writable_protection`); write; put each region's own old protection back; read the bytes
/// back; `FlushInstructionCache`. Nothing is written on any error before the write, and the old
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
    // SAFETY: the caller's contract for `write_checked` is the contract of `write_checked_with`,
    // and `WindowsOs` makes the real protection and read calls it needs.
    unsafe { write_checked_with(&mut WindowsOs, addr, expected, new) }
}

/// The body of [`write_checked`], with the operating-system calls in `os`.
///
/// # Safety
///
/// The contract of [`write_checked`]. In addition `os.set_writable` must make the pages writable
/// when it returns `Ok`, since the write below relies on that.
unsafe fn write_checked_with(
    os: &mut impl Os,
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
    let found = unsafe { os.read(addr, len) };
    if let Some(i) = found.iter().zip(expected).position(|(a, b)| a != b) {
        return Err(PatchError::Mismatch {
            at: addr.saturating_add(i),
            found,
        });
    }
    let changed = unprotect_all(&pieces, os).map_err(PatchError::Protect)?;
    // SAFETY: `unprotect_all` succeeded, so every page of `[addr, addr + len)` is now writable
    // (`os.set_writable` returned Ok for each piece), and the range was committed a moment ago.
    // `new` is a slice of `len` readable bytes. That `new` and `expected` do not overlap the
    // target is the caller's contract in `# Safety`.
    unsafe { ptr::copy_nonoverlapping(new.as_ptr(), addr as *mut u8, len) };
    // Restore before anything else can fail, so no path below leaves the pages writable. A page
    // that cannot be restored is logged by `restore_all` and stays writable; the patch stays too.
    restore_all(&changed, os);
    // SAFETY: the range was readable before the write, and a page is now either back to its old
    // (readable) protection or, if the restore failed, still read-write; so it is readable.
    let back = unsafe { os.read(addr, len) };
    // A failed flush is harmless on x86-64 (the instruction cache is coherent there).
    if !os.flush(addr, len) {
        os.warn(&format!(
            "patch: FlushInstructionCache failed for {addr:#x}"
        ));
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

/// The address a 32-bit displacement `disp` reaches from the end of an instruction (`next_insn`):
/// the inverse of [`rel32`]. `None` if it would be below 0 or past the end of the address space.
pub fn rel32_target(next_insn: usize, disp: i32) -> Option<usize> {
    next_insn.checked_add_signed(isize::try_from(disp).ok()?)
}

/// Windows hands out address space in blocks of this size.
const GRANULE: i128 = 0x1_0000;
/// A `rel32` reaches this far behind, and one byte less ahead.
const REACH: i128 = 1 << 31;
/// The most steps the search takes before it gives up. Each of the two cursors moves at least one
/// granule per step and the window is 4 GiB (65536 granules), so a `query` that behaves needs at
/// most about 131072 steps in all; this is twice that. Only a misbehaving `query` can reach it.
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
    // frees all of it. The error code is read straight away, before anything else can overwrite
    // it.
    unsafe {
        if VirtualFree(got, 0, MEM_RELEASE) == 0 {
            ::log::warn!(
                "patch: VirtualAlloc gave {:#x} instead of {addr:#x}, and VirtualFree of it failed (error {})",
                got as usize,
                GetLastError()
            );
        }
    }
    false
}

/// Reserve and commit `size` bytes of `PAGE_READWRITE` memory such that the whole block (rounded
/// up to whole pages) can be reached by a `rel32` from `target`: every byte of it is within
/// 2 GiB behind or 2 GiB - 1 ahead of `target`, which is the way [`rel32`] measures. Returns its
/// address (64 KiB aligned, zero-filled), or `None` if no such block exists.
///
/// Pass the address the displacement is measured from, that is the address of the instruction
/// after the `jmp`/`call` (`next_insn` in [`rel32`]), not the start of the instruction. The block
/// is found near `target`: the search runs outward from it, below and above in turn, and takes the
/// first block it can reserve (so it is not guaranteed to be the very nearest one). The block is
/// never freed: it lives as long as the process, as a trampoline or code cave must.
///
/// Walks `VirtualQuery` regions rather than every 64 KiB step, which visits the same candidate
/// addresses (the 64 KiB-aligned starts inside free regions) without querying each one. If the
/// system call that reserves a candidate fails (another thread took it), the search moves on to
/// the next candidate.
pub fn alloc_near(target: usize, size: usize) -> Option<usize> {
    find_near(target, size, limits(), &query, &mut reserve_at)
}

/// [`alloc_near`] for several displacement sources at once: every byte of the block is within a
/// `rel32` of EVERY address in `targets` (each the `next_insn` of one instruction). The search
/// starts at the first target. `None` if `targets` is empty or no such block exists.
///
/// Use it when several instructions are pointed at the same block: a block that only the first
/// of them reaches is no use.
pub fn alloc_near_all(targets: &[usize], size: usize) -> Option<usize> {
    find_near_all(targets, size, limits(), &query, &mut reserve_at)
}

/// The search behind [`alloc_near`]: [`find_near_all`] with one target.
fn find_near(
    target: usize,
    size: usize,
    lim: Limits,
    query: &dyn Fn(usize) -> Option<Region>,
    alloc: &mut dyn FnMut(usize, usize) -> bool,
) -> Option<usize> {
    find_near_all(&[target], size, lim, query, alloc)
}

/// The search behind [`alloc_near_all`], with the address space (`query`) and the reservation
/// (`alloc`) as parameters so it can be tested on a made-up address space.
///
/// All arithmetic is in `i128`, so `target - 2 GiB` can go negative without wrapping.
fn find_near_all(
    targets: &[usize],
    size: usize,
    lim: Limits,
    query: &dyn Fn(usize) -> Option<Region>,
    alloc: &mut dyn FnMut(usize, usize) -> bool,
) -> Option<usize> {
    let &first = targets.first()?;
    if size == 0 || lim.page == 0 {
        return None;
    }
    let span = round_up(size as i128, lim.page as i128);
    // Block starts that are on the 64 KiB grid, inside the usable address range, and keep the
    // first and the last byte of the block within a rel32 of every target: the highest of the
    // low ends and the lowest of the high ends.
    let mut low = lim.min_addr as i128;
    let mut high = lim.max_addr as i128 + 1 - span;
    for &target in targets {
        let t = target as i128;
        low = low.max(t - REACH);
        high = high.min(t + REACH - span);
    }
    let lo = round_up(low, GRANULE);
    let hi = round_down(high, GRANULE);
    if hi < lo {
        return None;
    }
    let fits = |a: i128| {
        targets.iter().all(|&target| {
            rel32(target, a as usize).is_some() && rel32(target, (a + span - 1) as usize).is_some()
        })
    };
    // Two cursors walk away from the first target, one up and one down, taking turns. Each is
    // always a multiple of GRANULE; a cursor outside [lo, hi] is finished.
    let start = (first as i128).clamp(lo, hi);
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
                        // Jump below the whole allocation. `base` is only the queried page, so
                        // jumping below it would crawl one granule per query through a big
                        // allocation. Every page from `alloc_base` up to here is part of the
                        // allocation, so none of it is free. (A nonsense `alloc_base` falls back
                        // to `base`.)
                        let alloc_base = r.alloc_base as i128;
                        let floor = if alloc_base != 0 && alloc_base <= base {
                            alloc_base
                        } else {
                            base
                        };
                        round_down(floor - 1, GRANULE).min(down - GRANULE)
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

    /// Run this one live-image test in a child; the parent never reaches its mutation body.
    /// The child checks a different process id and runs with one test worker.
    pub(crate) fn isolated_process(test: &str, parent_env: &str) -> bool {
        if let Some(parent) = std::env::var_os(parent_env) {
            assert_ne!(
                parent,
                std::ffi::OsString::from(std::process::id().to_string())
            );
            return false;
        }
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", test, "--nocapture", "--test-threads=1"])
            .env(parent_env, std::process::id().to_string())
            .output()
            .unwrap();
        assert!(
            output.status.success() && String::from_utf8_lossy(&output.stdout).contains("1 passed"),
            "isolated test failed: {}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        true
    }

    /// Some `VirtualAlloc` memory that is released on drop.
    ///
    /// `put`, `get` and `protect` are safe functions, so each one checks that its range lies
    /// inside the `size` bytes this value owns and panics if not (this is test code). Whether the
    /// pages are readable or writable at that moment is up to the test, which set the protection.
    pub(crate) struct Mem {
        pub(crate) base: usize,
        size: usize,
    }

    impl Mem {
        pub(crate) fn new(size: usize, kind: u32, protect: u32) -> Mem {
            // SAFETY: a null address lets VirtualAlloc choose; it returns fresh pages or null.
            let p = unsafe { VirtualAlloc(null(), size, kind, protect) };
            assert!(!p.is_null(), "VirtualAlloc failed");
            Mem {
                base: p as usize,
                size,
            }
        }

        /// Committed read-write memory.
        pub(crate) fn rw(size: usize) -> Mem {
            Mem::new(size, MEM_RESERVE | MEM_COMMIT, PAGE_READWRITE)
        }

        /// Take over a `VirtualAlloc` block (for example one from `alloc_near`) so it is freed
        /// when this value is dropped.
        ///
        /// # Safety
        ///
        /// `base` must be the base address of a live `VirtualAlloc` allocation of at least `size`
        /// bytes, and nothing else may free it or use it after this value is dropped.
        pub(crate) unsafe fn adopt(base: usize, size: usize) -> Mem {
            Mem { base, size }
        }

        /// Panics unless `[off, off + len)` is inside the allocation.
        fn check(&self, off: usize, len: usize) {
            let end = off.checked_add(len).expect("range overflows");
            assert!(
                end <= self.size,
                "range {off:#x}+{len:#x} is outside the {:#x}-byte allocation",
                self.size
            );
        }

        pub(crate) fn put(&self, off: usize, bytes: &[u8]) {
            self.check(off, bytes.len());
            // SAFETY: the range is inside the allocation this value owns (checked above), so it
            // is memory of this process; `bytes` is a separate slice, so the ranges cannot
            // overlap. The caller (a test) has left those pages writable.
            unsafe {
                std::ptr::copy_nonoverlapping(
                    bytes.as_ptr(),
                    (self.base + off) as *mut u8,
                    bytes.len(),
                )
            };
        }

        pub(crate) fn get(&self, off: usize, len: usize) -> Vec<u8> {
            self.check(off, len);
            let mut out = vec![0u8; len];
            // SAFETY: the range is inside the allocation this value owns (checked above); `out`
            // is a fresh allocation, so the ranges cannot overlap. The caller (a test) has left
            // those pages readable.
            unsafe {
                std::ptr::copy_nonoverlapping((self.base + off) as *const u8, out.as_mut_ptr(), len)
            };
            out
        }

        pub(crate) fn protect(&self, off: usize, len: usize, protect: u32) {
            self.check(off, len);
            let mut old = 0;
            // SAFETY: the range is inside the allocation this value owns (checked above), `old`
            // is a valid output location, and VirtualProtect touches no memory itself.
            let ok =
                unsafe { VirtualProtect((self.base + off) as *const _, len, protect, &mut old) };
            assert!(ok != 0, "VirtualProtect failed");
        }
    }

    impl Drop for Mem {
        fn drop(&mut self) {
            // SAFETY: `base` is the base of a live allocation that `new` made (or the caller of
            // `adopt` vouched for) and nothing else frees it; this is the one release.
            unsafe { VirtualFree(self.base as *mut _, 0, MEM_RELEASE) };
        }
    }

    pub(crate) fn info(addr: usize) -> MEMORY_BASIC_INFORMATION {
        let mut info = MEMORY_BASIC_INFORMATION::default();
        // SAFETY: `info` is a valid buffer of exactly the size passed. VirtualQuery accepts any
        // address, reads no memory at it, and reports failure by returning 0.
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
    use super::test_support::{info, protect_of, Mem};
    use super::*;
    use windows_sys::Win32::System::Memory::{
        VirtualAlloc, MEM_COMMIT, MEM_FREE, MEM_RESERVE, PAGE_GUARD, PAGE_NOACCESS, PAGE_NOCACHE,
        PAGE_READONLY, PAGE_WRITECOPY,
    };

    const PAGE: usize = 0x1000;

    // ---- the tests from the brief ---------------------------------------------------------

    #[test]
    fn refuses_unexpected_bytes() {
        let mut buf = [1u8, 2, 3, 4];
        // SAFETY: the range is inside `buf`, a local array only this thread uses; `expected` and
        // `new` are separate literals, not borrows of it.
        let r = unsafe { write_checked(buf.as_mut_ptr() as usize, &[9, 9], &[5, 6]) };
        assert!(matches!(r, Err(PatchError::Mismatch { .. })));
        assert_eq!(buf, [1, 2, 3, 4]);
    }

    #[test]
    fn writes_and_keeps_original() {
        let mut buf = [1u8, 2, 3, 4];
        // SAFETY: the range is inside `buf`, a local array only this thread uses; `expected` and
        // `new` are separate literals, not borrows of it.
        let p = unsafe { write_checked(buf.as_mut_ptr() as usize + 1, &[2, 3], &[7, 8]) }.unwrap();
        assert_eq!(buf, [1, 7, 8, 4]);
        assert_eq!(p.original, vec![2, 3]);
    }

    #[test]
    fn alloc_near_is_in_rel32_range() {
        let here = alloc_near_is_in_rel32_range as *const () as usize;
        let a = alloc_near(here, 4096).unwrap();
        // SAFETY: `alloc_near` returns the base of a fresh VirtualAlloc block of at least the
        // size asked for, and nothing else frees it.
        let _block = unsafe { Mem::adopt(a, 4096) };
        assert!(rel32(here, a).is_some() && rel32(here, a + 4095).is_some());
    }

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
            // SAFETY: `at` is the start of `buf`, a local array only this thread uses; the
            // lengths are rejected before anything is read or written.
            let r = unsafe { write_checked(at, e, n) };
            assert_eq!(r, Err(PatchError::BadLength));
        }
        assert_eq!(buf, [1, 2, 3, 4]);
    }

    #[test]
    fn mismatch_reports_first_differing_address_and_all_bytes_found() {
        let mut buf = [1u8, 2, 3, 4];
        let at = buf.as_mut_ptr() as usize;
        // SAFETY: the range is `buf`, a local array only this thread uses; `expected` and `new`
        // are separate literals.
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
        // SAFETY: the four bytes are `word`, a local only this thread uses.
        let p = unsafe { write_u32_checked(at, 0x1122_3344, 0xdead_beef) }.unwrap();
        assert_eq!(p.original, 0x1122_3344u32.to_le_bytes().to_vec());
        assert_eq!(word, 0xdead_beef);
        // SAFETY: as above; the value no longer matches, so nothing is written.
        let r = unsafe { write_u32_checked(at, 0x1122_3344, 1) };
        assert!(matches!(r, Err(PatchError::Mismatch { .. })));
        assert_eq!(word, 0xdead_beef);
    }

    // ---- write_checked: memory that must not be read (hazard 3) ----------------------------

    #[test]
    fn null_and_wrapping_ranges_are_not_mapped() {
        let cases: [(usize, &[u8], &[u8]); 3] = [
            (0, &[0], &[1]),
            (usize::MAX, &[0, 0], &[1, 1]),
            (usize::MAX - 1, &[0, 0, 0], &[1, 1, 1]),
        ];
        for (addr, expected, new) in cases {
            // SAFETY: none of these ranges is mapped (the null page, and ranges that run off the
            // end of the address space), so there is no memory to break; `write_checked` refuses
            // them before reading or writing anything, which is what this test checks.
            let r = unsafe { write_checked(addr, expected, new) };
            assert_eq!(r, Err(PatchError::NotMapped), "{addr:#x}");
        }
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
            // SAFETY: `m` is a page this test owns; it is unreadable, so `write_checked` refuses
            // it before reading or writing it, which is what this test checks.
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
        // SAFETY: committing the first page of the region this test just reserved.
        let committed =
            unsafe { VirtualAlloc(m.base as *const _, PAGE, MEM_COMMIT, PAGE_READWRITE) };
        assert!(!committed.is_null());
        m.put(PAGE - 2, &[5, 6]);
        // SAFETY: the range is in pages this test owns; its second half is uncommitted, so
        // `write_checked` refuses the whole range before reading or writing it.
        let r = unsafe { write_checked(m.base + PAGE - 2, &[5, 6, 0, 0], &[9, 9, 9, 9]) };
        assert_eq!(r, Err(PatchError::NotMapped));
        assert_eq!(
            m.get(PAGE - 2, 2),
            vec![5, 6],
            "the committed part must be untouched"
        );
        assert_eq!(protect_of(m.base), PAGE_READWRITE);
    }

    // ---- write_checked: protection (hazard 4) ----------------------------------------------

    #[test]
    fn read_only_and_execute_read_pages_are_patched_and_put_back() {
        for protect in [PAGE_READONLY, PAGE_EXECUTE_READ] {
            let m = Mem::rw(PAGE);
            m.put(8, &[1, 2, 3, 4]);
            m.protect(0, PAGE, protect);
            // SAFETY: the range is inside `m`, a page this test owns and no other thread uses;
            // `expected` and `new` are separate literals.
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
        // SAFETY: the range is inside `m`, two pages this test owns and no other thread uses;
        // `expected` and `new` are separate literals.
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
        // SAFETY: KUSER_SHARED_DATA is mapped readable in every Windows process at this address,
        // and `NtMajorVersion` is a 4-byte aligned `u32` inside it.
        let major = unsafe { *(at as *const u32) };
        let before = protect_of(KUSER);
        // SAFETY: the value written equals the value read, so even if the page were writable
        // nothing would change; VirtualProtect is expected to refuse, which is what is tested.
        let r = unsafe { write_u32_checked(at, major, major) };
        eprintln!("VirtualProtect(RWX) on KUSER_SHARED_DATA: {r:?}");
        assert!(
            matches!(r, Err(PatchError::Protect(code)) if code != 0),
            "{r:?}"
        );
        assert_eq!(protect_of(KUSER), before);
    }

    // ---- the operating-system calls, replaced by a fake -----------------------------------

    /// An [`Os`] that changes no protection, records every call, and fails on request.
    #[derive(Default)]
    struct FakeOs {
        events: Vec<String>,
        warnings: Vec<String>,
        flushes: Vec<(usize, usize)>,
        reads: usize,
        /// `set_writable` fails with this code for the piece at this address.
        fail_set_at: Option<(usize, u32)>,
        /// `restore` fails (error 5) for the piece at this address.
        fail_restore_at: Option<usize>,
        /// The second `read` returns its bytes with the first one flipped.
        corrupt_second_read: bool,
        fail_flush: bool,
    }

    impl Os for FakeOs {
        fn set_writable(&mut self, p: &Piece) -> Result<u32, u32> {
            self.events
                .push(format!("set_writable {:#x}+{}", p.addr, p.len));
            match self.fail_set_at {
                Some((at, code)) if at == p.addr => Err(code),
                // A distinct "old protection" per page, so a restore can be matched to its page.
                _ => Ok(0x20 + (p.addr >> 12) as u32),
            }
        }

        fn restore(&mut self, p: &Piece, old: u32) -> Result<(), u32> {
            self.events.push(format!("restore {:#x} {old:#x}", p.addr));
            if self.fail_restore_at == Some(p.addr) {
                Err(5)
            } else {
                Ok(())
            }
        }

        unsafe fn read(&mut self, addr: usize, len: usize) -> Vec<u8> {
            self.reads += 1;
            self.events.push("read".to_string());
            // SAFETY: the caller guarantees `[addr, addr + len)` is readable.
            let mut bytes = unsafe { copy_out(addr, len) };
            if self.corrupt_second_read && self.reads == 2 {
                if let Some(first) = bytes.first_mut() {
                    *first ^= 0xff;
                }
            }
            bytes
        }

        fn flush(&mut self, addr: usize, len: usize) -> bool {
            self.events.push("flush".to_string());
            self.flushes.push((addr, len));
            !self.fail_flush
        }

        fn warn(&mut self, message: &str) {
            self.warnings.push(message.to_string());
        }
    }

    /// Run `write_checked_with` over `len` bytes at `off` in a read-write page that holds 1, 2, 3
    /// ...; the replacement is 0x41, 0x42 ...
    fn fake_write(
        fake: &mut FakeOs,
        m: &Mem,
        off: usize,
        len: usize,
    ) -> Result<Patched, PatchError> {
        let expected: Vec<u8> = (1..=len as u8).collect();
        let new: Vec<u8> = (0x41..0x41 + len as u8).collect();
        m.put(off, &expected);
        // SAFETY: the range is inside `m`, memory this test owns and no other thread uses, and it
        // is already read-write, which is all `FakeOs::set_writable` (it changes nothing) needs;
        // `expected` and `new` are separate vectors.
        unsafe { write_checked_with(fake, m.base + off, &expected, &new) }
    }

    #[test]
    fn the_calls_come_in_order_and_flush_gets_the_address_and_length() {
        let m = Mem::rw(PAGE);
        let mut fake = FakeOs::default();
        let at = m.base + 8;
        let p = fake_write(&mut fake, &m, 8, 4).unwrap();
        assert_eq!(p.addr, at);
        assert_eq!(p.original, vec![1, 2, 3, 4]);
        assert_eq!(m.get(8, 4), vec![0x41, 0x42, 0x43, 0x44]);
        // Compare, make writable, write, restore before the read-back, then flush; once each.
        let page = (at >> 12) as u32;
        assert_eq!(
            fake.events,
            vec![
                "read".to_string(),
                format!("set_writable {at:#x}+4"),
                format!("restore {at:#x} {:#x}", 0x20 + page),
                "read".to_string(),
                "flush".to_string(),
            ]
        );
        assert_eq!(fake.flushes, vec![(at, 4)]);
        assert!(fake.warnings.is_empty());
    }

    #[test]
    fn a_failed_restore_after_the_write_keeps_the_patch_and_warns() {
        let m = Mem::rw(PAGE);
        let mut fake = FakeOs {
            fail_restore_at: Some(m.base + 8),
            ..FakeOs::default()
        };
        let p = fake_write(&mut fake, &m, 8, 4).expect("the patch stays in place: Ok");
        assert_eq!(p.original, vec![1, 2, 3, 4]);
        assert_eq!(m.get(8, 4), vec![0x41, 0x42, 0x43, 0x44]);
        assert_eq!(fake.warnings.len(), 1, "{:?}", fake.warnings);
        assert!(fake.warnings[0].contains("could not restore"));
        assert!(fake.warnings[0].contains(&format!("{:#x}", m.base + 8)));
        // The read-back and the flush still ran.
        assert_eq!(fake.reads, 2);
        assert_eq!(fake.flushes.len(), 1);
    }

    #[test]
    fn a_read_back_mismatch_is_reported_and_the_pages_are_still_restored() {
        let m = Mem::rw(PAGE);
        let mut fake = FakeOs {
            corrupt_second_read: true,
            ..FakeOs::default()
        };
        let r = fake_write(&mut fake, &m, 8, 4);
        assert_eq!(r, Err(PatchError::ReadBack));
        // The bytes were written (only the read-back was made to disagree)...
        assert_eq!(m.get(8, 4), vec![0x41, 0x42, 0x43, 0x44]);
        // ...the protection was restored before the read-back, and the flush still ran.
        let restore = fake.events.iter().position(|e| e.starts_with("restore"));
        let second_read = fake.events.iter().rposition(|e| e == "read");
        assert!(restore < second_read, "{:?}", fake.events);
        assert_eq!(fake.flushes.len(), 1);
    }

    #[test]
    fn a_failed_flush_is_only_a_warning() {
        let m = Mem::rw(PAGE);
        let mut fake = FakeOs {
            fail_flush: true,
            ..FakeOs::default()
        };
        assert!(fake_write(&mut fake, &m, 8, 4).is_ok());
        assert_eq!(fake.warnings.len(), 1, "{:?}", fake.warnings);
        assert!(fake.warnings[0].contains("FlushInstructionCache"));
        assert_eq!(fake.flushes.len(), 1);
    }

    #[test]
    fn a_protect_failure_on_the_second_page_writes_nothing_and_restores_the_first() {
        // Two writable pages with different protection values (read-write, then execute-read-
        // write), so VirtualQuery reports two regions and the range has two pieces.
        let m = Mem::rw(2 * PAGE);
        m.protect(PAGE, PAGE, PAGE_EXECUTE_READWRITE);
        m.put(PAGE - 2, &[1, 2, 3, 4]);
        let second = m.base + PAGE;
        let mut fake = FakeOs {
            fail_set_at: Some((second, 87)),
            ..FakeOs::default()
        };
        // SAFETY: the range is inside `m`, two writable pages this test owns and no other thread
        // uses; `FakeOs::set_writable` changes nothing; `expected` and `new` are literals.
        let r = unsafe {
            write_checked_with(&mut fake, m.base + PAGE - 2, &[1, 2, 3, 4], &[5, 6, 7, 8])
        };
        assert_eq!(r, Err(PatchError::Protect(87)));
        assert_eq!(m.get(PAGE - 2, 4), vec![1, 2, 3, 4], "nothing was written");
        let first = m.base + PAGE - 2;
        let page = (first >> 12) as u32;
        assert_eq!(
            fake.events,
            vec![
                "read".to_string(),
                format!("set_writable {first:#x}+2"),
                format!("set_writable {second:#x}+2"),
                format!("restore {first:#x} {:#x}", 0x20 + page),
            ]
        );
        assert_eq!(fake.reads, 1);
        assert!(fake.flushes.is_empty());
    }

    // ---- the page-protection rollback logic, without any memory ---------------------------

    fn piece(addr: usize) -> Piece {
        Piece {
            addr,
            len: 16,
            protect: PAGE_READWRITE,
        }
    }

    #[test]
    fn a_failure_part_way_restores_the_pages_already_changed() {
        let pieces = [piece(0x1000), piece(0x2000), piece(0x3000)];
        let mut fake = FakeOs {
            fail_set_at: Some((0x3000, 87)),
            ..FakeOs::default()
        };
        let r = unprotect_all(&pieces, &mut fake);
        assert_eq!(r.map(|_| ()), Err(87));
        assert_eq!(
            fake.events,
            vec![
                "set_writable 0x1000+16".to_string(),
                "set_writable 0x2000+16".to_string(),
                "set_writable 0x3000+16".to_string(),
                format!("restore 0x1000 {:#x}", 0x21),
                format!("restore 0x2000 {:#x}", 0x22),
            ]
        );
        assert!(fake.warnings.is_empty());
    }

    #[test]
    fn a_rollback_that_cannot_restore_a_page_warns_and_still_reports_the_original_error() {
        let pieces = [piece(0x1000), piece(0x2000)];
        let mut fake = FakeOs {
            fail_set_at: Some((0x2000, 87)),
            fail_restore_at: Some(0x1000),
            ..FakeOs::default()
        };
        assert_eq!(unprotect_all(&pieces, &mut fake).map(|_| ()), Err(87));
        assert_eq!(fake.warnings.len(), 1, "{:?}", fake.warnings);
        assert!(fake.warnings[0].contains("0x1000"));
    }

    #[test]
    fn restore_all_tries_every_page_and_reports_any_failure() {
        let changed = [
            (piece(0x1000), PAGE_READONLY),
            (piece(0x2000), PAGE_READONLY),
            (piece(0x3000), PAGE_READONLY),
        ];
        let mut fake = FakeOs {
            fail_restore_at: Some(0x2000),
            ..FakeOs::default()
        };
        assert!(!restore_all(&changed, &mut fake));
        assert_eq!(
            fake.events,
            vec![
                format!("restore 0x1000 {PAGE_READONLY:#x}"),
                format!("restore 0x2000 {PAGE_READONLY:#x}"),
                format!("restore 0x3000 {PAGE_READONLY:#x}"),
            ]
        );
        assert_eq!(fake.warnings.len(), 1);
        assert!(restore_all(&changed, &mut FakeOs::default()));
    }

    // ---- which protection a page is made writable with (T7-6) -----------------------------

    #[test]
    fn only_a_page_that_can_execute_is_made_executable_and_writable() {
        for current in [PAGE_READONLY, PAGE_READWRITE, PAGE_WRITECOPY] {
            assert_eq!(writable_protection(current), PAGE_READWRITE, "{current:#x}");
        }
        for current in [
            PAGE_EXECUTE,
            PAGE_EXECUTE_READ,
            PAGE_EXECUTE_READWRITE,
            PAGE_EXECUTE_WRITECOPY,
        ] {
            assert_eq!(
                writable_protection(current),
                PAGE_EXECUTE_READWRITE,
                "{current:#x}"
            );
        }
        // Modifier bits do not change the answer.
        assert_eq!(
            writable_protection(PAGE_READONLY | PAGE_NOCACHE),
            PAGE_READWRITE
        );
        assert_eq!(
            writable_protection(PAGE_EXECUTE_READ | PAGE_NOCACHE),
            PAGE_EXECUTE_READWRITE
        );
    }

    #[test]
    fn a_piece_carries_the_protection_of_its_region() {
        let m = Mem::rw(2 * PAGE);
        m.protect(PAGE, PAGE, PAGE_READONLY);
        let pieces = readable_pieces(m.base + PAGE - 2, 4).unwrap();
        assert_eq!(pieces.len(), 2);
        assert_eq!(pieces[0].protect, PAGE_READWRITE);
        assert_eq!(pieces[1].protect, PAGE_READONLY);
    }

    /// The real calls, except that right after `set_writable` it asks `VirtualQuery` what
    /// protection the first page really has, and notes it in `made`.
    #[derive(Default)]
    struct SpyOs {
        made: Vec<u32>,
    }

    impl Os for SpyOs {
        fn set_writable(&mut self, p: &Piece) -> Result<u32, u32> {
            let old = WindowsOs.set_writable(p)?;
            self.made.push(protect_of(p.addr));
            Ok(old)
        }

        fn restore(&mut self, p: &Piece, old: u32) -> Result<(), u32> {
            WindowsOs.restore(p, old)
        }

        unsafe fn read(&mut self, addr: usize, len: usize) -> Vec<u8> {
            // SAFETY: the caller guarantees `[addr, addr + len)` is readable.
            unsafe { WindowsOs.read(addr, len) }
        }

        fn flush(&mut self, addr: usize, len: usize) -> bool {
            WindowsOs.flush(addr, len)
        }

        fn warn(&mut self, message: &str) {
            WindowsOs.warn(message);
        }
    }

    #[test]
    fn a_read_only_page_is_made_writable_without_the_execute_bit() {
        // What the page is made, per what it was: (before, while the write happens).
        for (before, during) in [
            (PAGE_READONLY, PAGE_READWRITE),
            (PAGE_READWRITE, PAGE_READWRITE),
            (PAGE_EXECUTE_READ, PAGE_EXECUTE_READWRITE),
            (PAGE_EXECUTE_READWRITE, PAGE_EXECUTE_READWRITE),
        ] {
            let m = Mem::rw(PAGE);
            m.put(0, &[1, 2, 3, 4]);
            m.protect(0, PAGE, before);
            let mut spy = SpyOs::default();
            // SAFETY: the range is inside `m`, a page this test owns and no other thread uses;
            // `expected` and `new` are literals, not borrows of it.
            let r = unsafe { write_checked_with(&mut spy, m.base, &[1, 2, 3, 4], &[5, 6, 7, 8]) };
            assert!(r.is_ok(), "{before:#x}: {r:?}");
            assert_eq!(spy.made, vec![during], "page was {before:#x}");
            assert_eq!(protect_of(m.base), before, "restored to {before:#x}");
            assert_eq!(m.get(0, 4), vec![5, 6, 7, 8], "page was {before:#x}");
        }
    }

    #[test]
    fn a_read_only_image_page_can_be_patched_without_the_execute_bit() {
        if super::test_support::isolated_process(
            "patch::tests::a_read_only_image_page_can_be_patched_without_the_execute_bit",
            "HEADROOM_HEADER_MUTATION_PARENT",
        ) {
            return;
        }
        assert!(
            std::env::var_os("HEADROOM_HEADER_MUTATION_PARENT").is_some(),
            "image mutation must run in an isolated child"
        );
        // The first page of this test's own exe: PE headers in an image section, read-only, as
        // the game's `.rdata` is. The isolated child changes and restores four DOS-stub bytes;
        // no parallel scanner test reads the image headers.
        let base = crate::scan::main_module().base;
        assert_ne!(base, 0);
        let target = base + 0x44;
        assert_eq!(protect_of(base), PAGE_READONLY);
        // SAFETY: the header page is committed and readable, and stays so; start-up rule: this
        // test is the only code that changes those four bytes.
        let original = unsafe { read_checked(target, 4) }.unwrap();
        let mut changed = original.clone();
        changed[0] ^= 0xff;

        let mut spy = SpyOs::default();
        // SAFETY: only this child test reads or writes these DOS-stub bytes; `original` and
        // `changed` are separate vectors.
        let r = unsafe { write_checked_with(&mut spy, target, &original, &changed) };
        assert!(r.is_ok(), "{r:?}");
        // SAFETY: as above; this puts the bytes back.
        let back = unsafe { write_checked_with(&mut WindowsOs, target, &changed, &original) };
        assert!(back.is_ok(), "{back:?}");

        assert_eq!(spy.made.len(), 1);
        // An image page that is made writable reads as read-write or copy-on-write.
        assert_ne!(
            spy.made[0] & (PAGE_READWRITE | PAGE_WRITECOPY),
            0,
            "{:#x}",
            spy.made[0]
        );
        assert_eq!(spy.made[0] & EXECUTABLE, 0, "{:#x}", spy.made[0]);
        assert_eq!(protect_of(base), PAGE_READONLY, "restored");
        // SAFETY: as above.
        assert_eq!(unsafe { read_checked(target, 4) }.unwrap(), original);
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

    #[test]
    fn rel32_target_is_the_inverse_of_rel32() {
        let next = 0x1_4007_693e;
        for dest in [
            next,
            next + 5,
            next - 5,
            next + 0x7fff_ffff,
            next - 0x8000_0000,
            0x1_413b_65fc,
        ] {
            let disp = rel32(next, dest).unwrap();
            assert_eq!(rel32_target(next, disp), Some(dest), "{dest:#x}");
        }
        // The grass instance load of build f68140a3 reads 90.0 at 0x1413b65fc.
        assert_eq!(rel32_target(next, 0x0133_fcbe), Some(0x1_413b_65fc));
    }

    #[test]
    fn rel32_target_cannot_overflow_at_the_ends_of_the_address_space() {
        assert_eq!(rel32_target(0, -1), None);
        assert_eq!(rel32_target(usize::MAX, 1), None);
        assert_eq!(rel32_target(usize::MAX, 0), Some(usize::MAX));
        assert_eq!(rel32_target(0, i32::MAX), Some(0x7fff_ffff));
        assert_eq!(rel32_target(0x8000_0000, i32::MIN), Some(0));
        assert_eq!(rel32_target(0x7fff_ffff, i32::MIN), None);
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

        /// Answers like `VirtualQuery`: the region starts at the page that holds `a`, not at the
        /// start of the allocation, and `alloc_base` is where the allocation starts (0 if free).
        fn query(&self, a: usize) -> Option<Region> {
            let &(s, e, free) = self.0.iter().find(|&&(s, e, _)| a >= s && a < e)?;
            let base = a / PAGE * PAGE;
            Some(Region {
                base,
                alloc_base: if free { 0 } else { s },
                size: e - base,
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
    fn a_big_occupied_allocation_is_jumped_not_crawled_through() {
        // The target sits inside one 4 GiB allocation; the nearest free memory is below its start.
        // VirtualQuery reports a region that starts at the queried page, so only `alloc_base` lets
        // the downward cursor jump over the allocation in one query instead of one per granule.
        let target = 0x1_4000_0000;
        let map = Map::with_free(&[(0x1_0000_0000, 0x1_1000_0000)]);
        let queries = std::cell::Cell::new(0usize);
        let got = find_near(
            target,
            4096,
            LIMITS,
            &|a| {
                queries.set(queries.get() + 1);
                map.query(a)
            },
            &mut |_, _| true,
        );
        let a = got.expect("free memory below the allocation");
        assert!((0x1_0000_0000..0x1_1000_0000).contains(&a), "{a:#x}");
        assert!(queries.get() <= 6, "{} queries", queries.get());
    }

    #[test]
    fn a_nonsense_allocation_base_falls_back_to_the_region_base() {
        // A non-free region that reports alloc_base 0 (or one above its own base) is still walked
        // past, a granule at a time. The occupied allocation is [0x1_8000, TOP); the free memory
        // is below it.
        let map = Map::with_free(&[(0x1_0000, 0x1_8000)]);
        for bad in [0usize, usize::MAX] {
            let got = find_near(
                0x4_0000,
                4096,
                LIMITS,
                &|a| {
                    map.query(a).map(|r| Region {
                        alloc_base: bad,
                        ..r
                    })
                },
                &mut |_, _| true,
            );
            assert_eq!(got, Some(0x1_0000), "alloc_base {bad:#x}");
        }
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
                    alloc_base: 0,
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

    // ---- alloc_near_all: every target in reach -----------------------------------------------

    /// Run `find_near_all` on `map`, accepting every allocation attempt. Returns the result and
    /// every address that was tried.
    fn search_all(map: &Map, targets: &[usize], size: usize) -> (Option<usize>, Vec<usize>) {
        let mut tries = Vec::new();
        let got = find_near_all(targets, size, LIMITS, &|a| map.query(a), &mut |a, _| {
            tries.push(a);
            true
        });
        (got, tries)
    }

    #[test]
    fn alloc_near_all_needs_every_target_in_reach() {
        // Two targets 0x2bd1 bytes apart, like the ends of the two grass distance loads. Their
        // windows overlap, except for 0x2bd1 bytes at each end.
        let a = 0x1_4000_0000;
        let b = a + 0x2bd1;
        // The lowest granule that `a` reaches, which `b` does not ...
        let low = (a - GIB2, a - GIB2 + G);
        // ... and the highest granule that `b` reaches, which `a` does not.
        let high = (a + GIB2, a + GIB2 + G);
        assert!(reach_ok(a, low.0, 4096) && !reach_ok(b, low.0, 4096));
        assert!(reach_ok(b, high.0, 4096) && !reach_ok(a, high.0, 4096));

        let map = Map::with_free(&[low]);
        assert_eq!(search(&map, a, 4096, |_, _| true).0, Some(low.0), "a alone");
        assert_eq!(
            search_all(&map, &[a, b], 4096),
            (None, vec![]),
            "b is out of reach"
        );
        let map = Map::with_free(&[high]);
        assert_eq!(
            search(&map, b, 4096, |_, _| true).0,
            Some(high.0),
            "b alone"
        );
        assert_eq!(
            search_all(&map, &[a, b], 4096),
            (None, vec![]),
            "a is out of reach"
        );

        // With a granule that both reach, that one is taken and no other is tried.
        let both = (a - 0x1000_0000, a - 0x1000_0000 + G);
        let map = Map::with_free(&[low, both, high]);
        for targets in [[a, b], [b, a]] {
            let (got, tries) = search_all(&map, &targets, 4096);
            assert_eq!(got, Some(both.0), "{targets:x?}");
            assert_eq!(tries, [both.0], "{targets:x?}");
        }
        assert!(reach_ok(a, both.0, 4096) && reach_ok(b, both.0, 4096));
        assert_eq!(search_all(&map, &[], 4096), (None, vec![]), "no target");
    }

    // ---- alloc_near: the real thing --------------------------------------------------------

    #[test]
    fn alloc_near_gives_zeroed_read_write_memory_in_reach() {
        let here = alloc_near_gives_zeroed_read_write_memory_in_reach as *const () as usize;
        let a = alloc_near(here, 5000).expect("block");
        // SAFETY: `alloc_near` returns the base of a fresh VirtualAlloc block of at least 5000
        // bytes, and nothing else frees it.
        let block = unsafe { Mem::adopt(a, 5000) };
        assert_eq!(a % G, 0);
        assert!(reach_ok(here, a, 2 * PAGE), "{a:#x}");
        let i = info(a);
        assert_eq!((i.State, i.Protect), (MEM_COMMIT, PAGE_READWRITE));
        assert!(i.RegionSize >= 2 * PAGE);
        assert_eq!(block.get(0, 5000), vec![0u8; 5000]);
        block.put(0, &[0xab; 5000]);
        let b = alloc_near(here, 5000).expect("second block");
        // SAFETY: as above for the second block.
        let _second = unsafe { Mem::adopt(b, 5000) };
        assert_ne!(a, b, "a second request must not get the same block");
    }

    #[test]
    fn alloc_near_a_target_in_the_low_2_gib_does_not_underflow() {
        // `target - 2 GiB` would wrap here.
        let a = alloc_near(0x2_0000, 4096).expect("block in the low 2 GiB");
        // SAFETY: `alloc_near` returns the base of a fresh VirtualAlloc block of at least 4096
        // bytes, and nothing else frees it.
        let _block = unsafe { Mem::adopt(a, 4096) };
        assert!(a >= 0x1_0000 && reach_ok(0x2_0000, a, 4096), "{a:#x}");
    }

    #[test]
    fn alloc_near_all_gives_a_block_that_every_target_reaches() {
        let here = alloc_near_all_gives_a_block_that_every_target_reaches as *const () as usize;
        let targets = [here, here + 0x2bd1];
        let a = alloc_near_all(&targets, 4096).expect("block");
        // SAFETY: `alloc_near_all` returns the base of a fresh VirtualAlloc block of at least 4096
        // bytes, and nothing else frees it.
        let block = unsafe { Mem::adopt(a, 4096) };
        assert_eq!(a % G, 0);
        for t in targets {
            assert!(reach_ok(t, a, 4096), "{t:#x} -> {a:#x}");
        }
        assert_eq!(block.get(0, 4096), vec![0u8; 4096]);
        assert_eq!(alloc_near_all(&[], 4096), None);
        assert_eq!(alloc_near_all(&targets, 0), None);
    }

    #[test]
    fn alloc_near_edges_of_the_address_space() {
        assert_eq!(alloc_near(usize::MAX, 4096), None);
        assert_eq!(alloc_near(1 << 48, 4096), None);
        assert_eq!(alloc_near(0x1_4000_0000, 0), None);
        assert_eq!(alloc_near(0x1_4000_0000, usize::MAX), None);
    }
}
