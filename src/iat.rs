//! Import-address-table lookup and swap.
//!
//! [`find_slot`] walks the import table of the running exe and returns the address of the IAT
//! entry that holds a named function from a named DLL. [`swap`] replaces that entry with
//! [`crate::patch::write_checked`] and returns the old value. Together they let the mod put a hook
//! on the game's `GetProcAddress` import.
//!
//! # What is trusted
//!
//! Nothing in the image. Every read goes through a bounds check against the [`Module`] size
//! (`SizeOfImage`) and a `VirtualQuery` readability check inside the module's own allocation, and
//! copies the bytes out; no reference into the image is kept. A name that is unreadable, too long
//! or unterminated, a thunk that is out of range or malformed, or a table that runs off the end of
//! the module, makes that entry (or that descriptor) not match; it never panics and never reads
//! outside the module. The `VirtualQuery`-then-read order makes the lookup start-up only: see the
//! rule on [`find_slot`].

use std::mem::align_of;
use std::ptr;

use crate::patch::{read_checked, write_checked, PatchError, Patched};
use crate::scan::{readable_run, u16_at, u32_at, u64_at, walk_readable, Module};

const MZ_MAGIC: u16 = 0x5a4d;
const PE_SIGNATURE: u32 = 0x0000_4550;
const PE32_PLUS_MAGIC: u16 = 0x20b;
/// The optional header starts 24 bytes after the PE signature (4 + the 20-byte file header).
const OPTIONAL_FROM_NT: usize = 24;
/// A PE32+ optional header is 112 bytes before its data directories.
const OPTIONAL_FIXED: usize = 112;
/// A data directory is a 4-byte RVA and a 4-byte size.
const DIRECTORY_LEN: usize = 8;
/// Bytes read at the NT headers: signature, file header, fixed optional header, and the first
/// two data directories (the second is the import directory).
const NT_READ: usize = OPTIONAL_FROM_NT + OPTIONAL_FIXED + 2 * DIRECTORY_LEN;
const DESCRIPTOR_LEN: usize = 20;
/// The most descriptors walked. A real exe has a few dozen.
const MAX_DESCRIPTORS: usize = 4096;
/// The most thunks walked in one descriptor. `kernel32.dll` exports well under 2000 names.
const MAX_THUNKS: usize = 1 << 16;
/// The longest DLL or function name accepted (the loader's own limit for a DLL name is 260).
const MAX_NAME: usize = 256;
const SLOT_LEN: usize = 8;

/// The running image, as far as this module has checked it: where it is and how many bytes of it
/// may be read.
struct Image {
    base: usize,
    /// `min(Module.size, SizeOfImage)`; every RVA read must end at or before this.
    limit: usize,
}

impl Image {
    /// Validate the headers of `m` and return the image with the RVA of its import directory.
    fn open(m: &Module) -> Option<(Image, usize)> {
        if m.base == 0 || m.size == 0 {
            return None;
        }
        let mut img = Image {
            base: m.base,
            limit: m.size,
        };
        let dos = img.bytes(0, 0x40)?;
        if u16_at(&dos, 0)? != MZ_MAGIC {
            return None;
        }
        let nt = u32_at(&dos, 0x3c)? as usize;
        let h = img.bytes(nt, NT_READ)?;
        if u32_at(&h, 0)? != PE_SIGNATURE {
            return None;
        }
        let optional_size = usize::from(u16_at(&h, 4 + 16)?);
        let opt = OPTIONAL_FROM_NT;
        if u16_at(&h, opt)? != PE32_PLUS_MAGIC
            || optional_size < OPTIONAL_FIXED + 2 * DIRECTORY_LEN
            || u32_at(&h, opt + 108)? < 2
        {
            return None;
        }
        let size_of_image = u32_at(&h, opt + 56)? as usize;
        let import_rva = u32_at(&h, opt + OPTIONAL_FIXED + DIRECTORY_LEN)? as usize;
        if size_of_image == 0 || import_rva == 0 {
            return None;
        }
        img.limit = m.size.min(size_of_image);
        Some((img, import_rva))
    }

    /// Copy `len` bytes at `rva`, only if they lie inside the image and are committed, readable
    /// memory of the image's own allocation.
    fn bytes(&self, rva: usize, len: usize) -> Option<Vec<u8>> {
        let end = rva.checked_add(len)?;
        if len == 0 || end > self.limit {
            return None;
        }
        let va = self.base.checked_add(rva)?;
        let alloc = self.base;
        if !walk_readable(va, len, |at| readable_run(at, alloc)) {
            return None;
        }
        let mut out = vec![0u8; len];
        // SAFETY: `walk_readable` just confirmed every page of `[va, va + len)` committed and
        // readable inside the allocation that starts at `self.base`. `out` is a fresh allocation
        // of `len` bytes, so the ranges cannot overlap. That the protection does not change
        // between the check and the copy rests on the "Start-up only" rule of `find_slot`.
        unsafe { ptr::copy_nonoverlapping(va as *const u8, out.as_mut_ptr(), len) };
        Some(out)
    }

    fn u64(&self, rva: usize) -> Option<u64> {
        u64_at(&self.bytes(rva, 8)?, 0)
    }

    /// A NUL-terminated string at `rva`, without the NUL. `None` if it is longer than `max`, if
    /// the image or readable memory ends before the NUL, or if it starts outside the image.
    fn cstr(&self, rva: usize, max: usize) -> Option<Vec<u8>> {
        let mut out: Vec<u8> = Vec::new();
        let mut at = rva;
        // `max` bytes of name plus the NUL.
        while out.len() <= max {
            let room = self.limit.checked_sub(at)?;
            let va = self.base.checked_add(at)?;
            let take = readable_run(va, self.base)
                .min(room)
                .min(max + 1 - out.len());
            if take == 0 {
                return None;
            }
            let mut chunk = vec![0u8; take];
            // SAFETY: `readable_run` reported at least `take` bytes from `va` committed and
            // readable inside the allocation that starts at `self.base`; `chunk` is a fresh
            // allocation of `take` bytes. The "Start-up only" rule of `find_slot` covers the gap
            // between the check and the copy.
            unsafe { ptr::copy_nonoverlapping(va as *const u8, chunk.as_mut_ptr(), take) };
            if let Some(nul) = chunk.iter().position(|&b| b == 0) {
                out.extend_from_slice(chunk.get(..nul)?);
                return Some(out);
            }
            out.extend_from_slice(&chunk);
            at = at.checked_add(take)?;
        }
        None
    }
}

/// One hit: which descriptor, which thunk in it, and the address of the IAT slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Found {
    descriptor: usize,
    index: usize,
    slot: usize,
}

/// Every IAT slot of `func` imported by name from `dll`, in import-table order, up to `limit`
/// of them.
fn collect(m: &Module, dll: &str, func: &str, limit: usize) -> Vec<Found> {
    let mut found = Vec::new();
    if dll.is_empty() || func.is_empty() || limit == 0 {
        return found;
    }
    let Some((img, import_rva)) = Image::open(m) else {
        return found;
    };
    for descriptor in 0..MAX_DESCRIPTORS {
        let Some(at) = descriptor
            .checked_mul(DESCRIPTOR_LEN)
            .and_then(|offset| import_rva.checked_add(offset))
        else {
            break;
        };
        let Some(d) = img.bytes(at, DESCRIPTOR_LEN) else {
            break;
        };
        let (Some(int_rva), Some(name_rva), Some(iat_rva)) =
            (u32_at(&d, 0), u32_at(&d, 12), u32_at(&d, 16))
        else {
            break;
        };
        // The Windows loader stops at the first descriptor with no name or no IAT.
        if name_rva == 0 || iat_rva == 0 {
            break;
        }
        let Some(dll_name) = img.cstr(name_rva as usize, MAX_NAME) else {
            continue;
        };
        // Without an import name table there are no names to compare (the IAT itself holds
        // addresses once the loader has run).
        if int_rva == 0 || !dll_name.eq_ignore_ascii_case(dll.as_bytes()) {
            continue;
        }
        scan_thunks(
            &img,
            descriptor,
            int_rva as usize,
            iat_rva as usize,
            func.as_bytes(),
            limit,
            &mut found,
        );
        if found.len() >= limit {
            break;
        }
    }
    found
}

/// Walk the import name table of one descriptor and push the slot of each by-name entry whose
/// name is exactly `func`.
fn scan_thunks(
    img: &Image,
    descriptor: usize,
    int_rva: usize,
    iat_rva: usize,
    func: &[u8],
    limit: usize,
    found: &mut Vec<Found>,
) {
    for index in 0..MAX_THUNKS {
        if found.len() >= limit {
            return;
        }
        let Some(entry) = index.checked_mul(SLOT_LEN) else {
            return;
        };
        let Some(thunk) = int_rva.checked_add(entry).and_then(|at| img.u64(at)) else {
            return;
        };
        if thunk == 0 {
            return;
        }
        // Bit 63 marks an import by ordinal: the rest is not an RVA and must not be read as one.
        // For an import by name, bits 62..31 must be zero as well (a 31-bit RVA).
        if thunk >> 31 != 0 {
            continue;
        }
        // The hint (2 bytes) comes first, then the NUL-terminated name.
        let Some(name) = (thunk as usize)
            .checked_add(2)
            .and_then(|rva| img.cstr(rva, MAX_NAME))
        else {
            continue;
        };
        if name != func {
            continue;
        }
        // The slot must be inside the image, aligned for a pointer, and readable.
        let Some(slot_rva) = iat_rva.checked_add(entry) else {
            continue;
        };
        if !slot_rva.is_multiple_of(align_of::<usize>()) || img.bytes(slot_rva, SLOT_LEN).is_none()
        {
            continue;
        }
        let Some(slot) = img.base.checked_add(slot_rva) else {
            continue;
        };
        found.push(Found {
            descriptor,
            index,
            slot,
        });
    }
}

/// Every IAT slot of `func` imported by name from `dll` (see [`find_slot`]).
#[cfg(test)]
fn find_all(m: &Module, dll: &str, func: &str) -> Vec<Found> {
    collect(m, dll, func, usize::MAX)
}

/// The IAT slot that holds `func`, imported by name from `dll`; the DLL name is compared without
/// regard to case, the function name exactly. `None` if the import table has no such entry or
/// cannot be read safely.
///
/// Every descriptor whose DLL name matches is searched, not just the first: one program can list
/// `kernel32.dll` under several descriptors (the Rust test binary does), and the function can be
/// under any of them. If it is imported more than once, the first slot in table order is
/// returned. Imports by ordinal are skipped, and a descriptor without an import name table
/// (`OriginalFirstThunk` of 0) cannot be searched by name and is skipped.
///
/// # Start-up only
///
/// Call this only from `InitializeASI` / `entry::start`, on one thread, before the game's
/// `WinMain` runs. The readability check is `VirtualQuery` first and the copy second, so a
/// protection change in between (another thread) would fault; and it is not meant for hooks or the
/// governor thread, which must not do this much work.
///
/// # Safety
///
/// `m` must describe an image mapped into this process (normally [`crate::scan::main_module`]).
/// The pointer returned is the slot's address inside that image: valid to read while the image
/// stays loaded, and to be written only through [`swap`] / [`crate::patch::write_checked`].
pub unsafe fn find_slot(m: &Module, dll: &str, func: &str) -> Option<*mut usize> {
    collect(m, dll, func, 1)
        .first()
        .map(|f| f.slot as *mut usize)
}

/// Put `new` in the IAT slot and return what was there.
///
/// Returns 0 when the slot was not changed: `slot` is null, not 8-byte aligned or not readable
/// memory; the slot holds 0 (a bound import is never 0, so this is not an import slot and nothing
/// is written); or [`crate::patch::write_checked`] refused (the value changed between the read and
/// the write, or the page could not be made writable). A warning is logged in each case.
///
/// If the write was done but did not read back as written, the old value is put back (and the
/// outcome logged) and the result is 0. Only if that put-back fails too can 0 follow a changed
/// slot; an error is logged then, and the slot may hold a wrong value. That needs another thread
/// or the hardware to be changing the slot, so it should not happen.
///
/// Uses `write_checked` on the 8 bytes, so it carries the same rules; see its "Start-up only"
/// section.
///
/// # Safety
///
/// - `slot` must come from [`find_slot`], or otherwise be the address of a pointer-sized import
///   slot.
/// - `[slot, slot + 8)` must be memory the process may change without breaking code that is
///   running, and no Rust reference may borrow it.
/// - It is start-up only: see "Start-up only" on [`crate::patch::write_checked`].
/// - `new` must be the address of a function with the same signature and calling convention as
///   the import, or the game will call through a wrong pointer.
pub unsafe fn swap(slot: *mut usize, new: usize) -> usize {
    let mut write = |addr: usize, expected: &[u8], new_bytes: &[u8]| {
        // SAFETY: `swap_with` calls this only with the address of `slot`, whose 8 bytes `swap`'s
        // `# Safety` makes memory the process may change and no reference borrows; `expected`
        // and `new_bytes` are owned arrays inside `swap_with`, not part of the target.
        unsafe { write_checked(addr, expected, new_bytes) }
    };
    // SAFETY: this function's contract is the contract of `swap_with`, and `write` behaves like
    // `write_checked`, as it is `write_checked`.
    unsafe { swap_with(slot, new, &mut write) }
}

/// A write of `new` over `expected` at an address, like [`crate::patch::write_checked`].
type Writer<'a> = dyn FnMut(usize, &[u8], &[u8]) -> Result<Patched, PatchError> + 'a;

/// The body of [`swap`], with the write as a parameter so a failed read-back can be simulated.
///
/// # Safety
///
/// The contract of [`swap`]. `write(addr, expected, new)` must behave like
/// [`crate::patch::write_checked`] for the 8 bytes at `addr`.
unsafe fn swap_with(slot: *mut usize, new: usize, write: &mut Writer<'_>) -> usize {
    let addr = slot as usize;
    if !addr.is_multiple_of(align_of::<usize>()) {
        ::log::warn!("iat: slot {addr:#x} is not aligned");
        return 0;
    }
    // SAFETY: `read_checked` checks with VirtualQuery that the 8 bytes are readable before it
    // reads them; the start-up rule of `swap` covers the gap between the check and the read.
    let old = match unsafe { read_checked(addr, SLOT_LEN) } {
        Ok(bytes) => bytes,
        Err(e) => {
            ::log::warn!("iat: cannot read slot {addr:#x}: {e}");
            return 0;
        }
    };
    let Ok(old) = <[u8; SLOT_LEN]>::try_from(old.as_slice()) else {
        return 0;
    };
    if old == [0u8; SLOT_LEN] {
        ::log::warn!("iat: slot {addr:#x} holds 0, so it is not a bound import: not written");
        return 0;
    }
    let new_bytes = new.to_le_bytes();
    match write(addr, &old, &new_bytes) {
        Ok(_) => usize::from_le_bytes(old),
        Err(PatchError::ReadBack) => {
            // The 8 bytes were written but did not read back as written. Put the old value back;
            // the compare in the write leaves the slot alone if something else has changed it.
            match write(addr, &new_bytes, &old) {
                Ok(_) => ::log::warn!(
                    "iat: slot {addr:#x} did not read back as written; the old value was put back"
                ),
                Err(e) => ::log::error!(
                    "iat: slot {addr:#x} did not read back as written, and putting the old value back failed ({e}); the slot may hold a wrong value"
                ),
            }
            0
        }
        Err(e) => {
            ::log::warn!("iat: cannot swap slot {addr:#x}: {e}");
            0
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::patch::test_support::{protect_of, Mem};
    use windows_sys::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
    use windows_sys::Win32::System::Memory::{
        PAGE_EXECUTE_READ, PAGE_NOACCESS, PAGE_READONLY, PAGE_READWRITE,
    };

    // ---- the test from the brief, and the real image ---------------------------------------

    /// Every name the kernel32 `GetProcAddress` import of the test binary could be listed under.
    const LOADER_DLLS: [&str; 4] = [
        "kernel32.dll",
        "api-ms-win-core-libraryloader-l1-1-0.dll",
        "api-ms-win-core-libraryloader-l1-2-0.dll",
        "api-ms-win-core-libraryloader-l1-2-1.dll",
    ];

    #[test]
    fn finds_own_getprocaddress_slot() {
        let m = crate::scan::main_module();
        let slot = find_in(&m, "kernel32.dll", "GetProcAddress");
        assert!(slot.is_some());
    }

    #[test]
    fn own_slot_is_found_under_at_least_one_loader_dll_name() {
        let m = crate::scan::main_module();
        let mut hits = Vec::new();
        for dll in LOADER_DLLS {
            for f in find_all(&m, dll, "GetProcAddress") {
                eprintln!(
                    "GetProcAddress: {dll}: descriptor {} thunk {} slot {:#x}",
                    f.descriptor, f.index, f.slot
                );
                hits.push(f);
            }
        }
        assert!(
            !hits.is_empty(),
            "GetProcAddress is not imported by name from any known DLL name"
        );
        let first = find_in(&m, "kernel32.dll", "GetProcAddress");
        assert_eq!(first, hits.first().map(|f| f.slot));
    }

    /// The DLL name of every descriptor of the main module, in table order.
    fn own_descriptor_names(m: &Module) -> Vec<String> {
        let (img, import_rva) = Image::open(m).expect("headers of the test binary");
        let mut names = Vec::new();
        for i in 0..MAX_DESCRIPTORS {
            let d = img
                .bytes(import_rva + i * DESCRIPTOR_LEN, DESCRIPTOR_LEN)
                .unwrap();
            let (name_rva, iat_rva) = (u32_at(&d, 12).unwrap(), u32_at(&d, 16).unwrap());
            if name_rva == 0 || iat_rva == 0 {
                break;
            }
            let name = img.cstr(name_rva as usize, MAX_NAME).unwrap();
            names.push(String::from_utf8_lossy(&name).into_owned());
        }
        names
    }

    #[test]
    fn the_descriptor_that_holds_getprocaddress_in_the_test_binary_is_reported() {
        let m = crate::scan::main_module();
        let names = own_descriptor_names(&m);
        eprintln!("import descriptors of the test binary: {names:?}");
        let kernel32 = names
            .iter()
            .filter(|n| n.eq_ignore_ascii_case("kernel32.dll"))
            .count();
        eprintln!("descriptors named kernel32.dll (any case): {kernel32}");
        let hits = find_all(&m, "kernel32.dll", "GetProcAddress");
        assert!(
            !hits.is_empty(),
            "no kernel32.dll descriptor holds GetProcAddress"
        );
        for h in &hits {
            eprintln!(
                "GetProcAddress is in descriptor {} ({:?}), thunk {}",
                h.descriptor, names[h.descriptor], h.index
            );
        }
    }

    #[test]
    fn every_own_slot_holds_the_real_function_address() {
        let m = crate::scan::main_module();
        let k32: Vec<u16> = "kernel32.dll\0".encode_utf16().collect();
        // SAFETY: `k32` is a NUL-terminated UTF-16 string that lives through the call.
        let module = unsafe { GetModuleHandleW(k32.as_ptr()) };
        assert!(!module.is_null());
        // SAFETY: `module` is the handle of kernel32, which stays loaded; the name is a
        // NUL-terminated C string literal.
        let real = unsafe { GetProcAddress(module, c"GetProcAddress".as_ptr().cast()) }
            .map(|f| f as usize)
            .expect("GetProcAddress resolves");
        let found = find_all(&m, "kernel32.dll", "GetProcAddress");
        assert!(!found.is_empty());
        for f in found {
            // SAFETY: `find_all` only reports slots it has just checked 8-aligned and readable
            // inside the test binary's image, which stays mapped.
            assert_eq!(unsafe { *(f.slot as *const usize) }, real, "{f:?}");
        }
    }

    #[test]
    fn dll_name_match_ignores_case_and_function_name_does_not() {
        let m = crate::scan::main_module();
        let up = find_in(&m, "KERNEL32.DLL", "GetProcAddress");
        let low = find_in(&m, "kernel32.dll", "GetProcAddress");
        assert!(up.is_some());
        assert_eq!(up, low);
        assert!(find_in(&m, "kernel32.dll", "getprocaddress").is_none());
        assert!(find_in(&m, "kernel32", "GetProcAddress").is_none());
        assert!(find_in(&m, "user32.dll", "GetProcAddress").is_none());
    }

    #[test]
    fn swapping_a_slot_for_its_own_value_changes_nothing_and_keeps_the_protection() {
        if crate::patch::test_support::isolated_process(
            "iat::tests::swapping_a_slot_for_its_own_value_changes_nothing_and_keeps_the_protection",
            "HEADROOM_IAT_MUTATION_PARENT",
        ) {
            return;
        }
        assert!(
            std::env::var_os("HEADROOM_IAT_MUTATION_PARENT").is_some(),
            "image mutation must run in an isolated child"
        );
        let m = crate::scan::main_module();
        let slot = find_in(&m, "kernel32.dll", "GetProcAddress").unwrap() as *mut usize;
        let before = protect_of(slot as usize);
        // SAFETY: `slot` is a checked, aligned, readable IAT entry of the test binary's image.
        let current = unsafe { *slot };
        // SAFETY: this isolated child runs only this test. No parallel test reads this slot;
        // the same native function remains installed for calls made by the child.
        assert_eq!(unsafe { swap(slot, current) }, current);
        // SAFETY: as for the first read.
        assert_eq!(unsafe { *slot }, current);
        assert_eq!(protect_of(slot as usize), before);
    }

    // ---- a fake image, to build tables the real binary does not have -----------------------

    const IMG: usize = 0x4000;
    const NT: usize = 0x80;
    const OPT: usize = NT + 24;
    const DESCRIPTORS: usize = 0x400;
    const DATA: usize = 0x800;

    enum T<'a> {
        Name(&'a str),
        Ordinal(u16),
        Raw(u64),
    }

    struct Imp<'a> {
        dll: &'a str,
        thunks: Vec<T<'a>>,
        has_int: bool,
    }

    impl<'a> Imp<'a> {
        fn new(dll: &'a str) -> Imp<'a> {
            Imp {
                dll,
                thunks: Vec::new(),
                has_int: true,
            }
        }
        fn name(mut self, f: &'a str) -> Self {
            self.thunks.push(T::Name(f));
            self
        }
        fn ordinal(mut self, o: u16) -> Self {
            self.thunks.push(T::Ordinal(o));
            self
        }
        fn raw(mut self, v: u64) -> Self {
            self.thunks.push(T::Raw(v));
            self
        }
        fn no_int(mut self) -> Self {
            self.has_int = false;
            self
        }
    }

    /// A PE-looking image in committed read-write memory: DOS and NT headers, then import
    /// descriptors, name tables and IATs laid out by [`Fake::build`].
    struct Fake {
        mem: Mem,
        int_rva: Vec<usize>,
        iat_rva: Vec<usize>,
        /// RVA of each `IMAGE_IMPORT_BY_NAME`, per descriptor and thunk (`None` if not by name).
        hint_rva: Vec<Vec<Option<usize>>>,
    }

    fn align(n: usize, to: usize) -> usize {
        (n + to - 1) & !(to - 1)
    }

    impl Fake {
        /// The by-name entries come before the INT, so the INT is the last thing in the image.
        fn build(imps: &[Imp]) -> Fake {
            Fake::build_with(imps, false)
        }

        /// With `names_last`, the by-name entries are the last thing in the image.
        fn build_with(imps: &[Imp], names_last: bool) -> Fake {
            let mem = Mem::rw(IMG);
            let put16 = |off: usize, v: u16| mem.put(off, &v.to_le_bytes());
            let put32 = |off: usize, v: u32| mem.put(off, &v.to_le_bytes());
            let put64 = |off: usize, v: u64| mem.put(off, &v.to_le_bytes());
            // Headers.
            put16(0, 0x5a4d);
            put32(0x3c, NT as u32);
            put32(NT, 0x0000_4550);
            put16(NT + 4, 0x8664);
            put16(NT + 4 + 16, 240);
            put16(OPT, 0x20b);
            put32(OPT + 56, IMG as u32);
            put32(OPT + 108, 16);
            put32(OPT + 112 + 8, DESCRIPTORS as u32);
            put32(OPT + 112 + 12, ((imps.len() + 1) * 20) as u32);
            // Tables. For each descriptor: DLL name, IAT, then either (INT, by-name entries) or
            // (by-name entries, INT). The last thing in the image is what a test can cut the
            // module inside of while everything else stays readable.
            let mut at = DATA;
            let (mut int_rva, mut iat_rva, mut hint_rva) = (Vec::new(), Vec::new(), Vec::new());
            for (d, imp) in imps.iter().enumerate() {
                let n = imp.thunks.len();
                let dll_rva = at;
                mem.put(at, imp.dll.as_bytes());
                at = align(at + imp.dll.len() + 1, 8);
                let iat = at;
                for i in 0..n {
                    put64(iat + 8 * i, 0x1111_0000 + (d as u64) * 0x100 + i as u64);
                }
                at += 8 * (n + 1);
                let mut int = 0;
                if imp.has_int && names_last {
                    int = at;
                    at += 8 * (n + 1);
                }
                let mut values = Vec::new();
                let mut hints = Vec::new();
                for t in &imp.thunks {
                    let (value, hint) = match t {
                        T::Name(f) => {
                            let h = at;
                            mem.put(h + 2, f.as_bytes());
                            at = align(h + 2 + f.len() + 1, 2);
                            (h as u64, Some(h))
                        }
                        T::Ordinal(o) => ((1u64 << 63) | u64::from(*o), None),
                        T::Raw(v) => (*v, None),
                    };
                    values.push(value);
                    hints.push(hint);
                }
                if imp.has_int && !names_last {
                    at = align(at, 8);
                    int = at;
                    at += 8 * (n + 1);
                }
                if imp.has_int {
                    for (i, v) in values.iter().enumerate() {
                        put64(int + 8 * i, *v);
                    }
                }
                assert!(at < IMG, "fake image is full");
                let desc = DESCRIPTORS + d * 20;
                put32(desc, int as u32);
                put32(desc + 12, dll_rva as u32);
                put32(desc + 16, iat as u32);
                int_rva.push(int);
                iat_rva.push(iat);
                hint_rva.push(hints);
            }
            Fake {
                mem,
                int_rva,
                iat_rva,
                hint_rva,
            }
        }

        fn module(&self) -> Module {
            Module {
                base: self.mem.base,
                size: IMG,
            }
        }

        fn slot(&self, descriptor: usize, index: usize) -> usize {
            self.mem.base + self.iat_rva[descriptor] + 8 * index
        }

        fn put64(&self, rva: usize, v: u64) {
            self.mem.put(rva, &v.to_le_bytes());
        }

        fn put32(&self, rva: usize, v: u32) {
            self.mem.put(rva, &v.to_le_bytes());
        }

        fn find(&self, dll: &str, func: &str) -> Option<usize> {
            find_in(&self.module(), dll, func)
        }
    }

    fn find_in(m: &Module, dll: &str, func: &str) -> Option<usize> {
        // SAFETY: `m` is the test binary's own image, or a fake image in memory this test owns;
        // nothing changes either while the test runs, so the start-up rule is met.
        unsafe { find_slot(m, dll, func) }.map(|p| p as usize)
    }

    // ---- hazard 1: every matching descriptor -----------------------------------------------

    #[test]
    fn the_function_is_found_under_the_second_kernel32_descriptor() {
        let f = Fake::build(&[
            Imp::new("KERNEL32.dll")
                .name("CreateFileW")
                .name("CloseHandle"),
            Imp::new("user32.dll").name("GetProcAddress"),
            Imp::new("kernel32.dll")
                .name("Sleep")
                .name("GetProcAddress"),
        ]);
        assert_eq!(f.find("kernel32.dll", "GetProcAddress"), Some(f.slot(2, 1)));
        assert_eq!(f.find("kernel32.dll", "CreateFileW"), Some(f.slot(0, 0)));
        assert_eq!(f.find("KERNEL32.DLL", "Sleep"), Some(f.slot(2, 0)));
        assert_eq!(f.find("user32.dll", "GetProcAddress"), Some(f.slot(1, 0)));
        let all = find_all(&f.module(), "kernel32.dll", "GetProcAddress");
        assert_eq!(
            all,
            vec![Found {
                descriptor: 2,
                index: 1,
                slot: f.slot(2, 1)
            }]
        );
    }

    #[test]
    fn imported_twice_the_first_in_table_order_is_returned_and_both_are_listed() {
        let f = Fake::build(&[
            Imp::new("KERNEL32.dll").name("GetProcAddress"),
            Imp::new("kernel32.dll").name("GetProcAddress"),
        ]);
        assert_eq!(f.find("kernel32.dll", "GetProcAddress"), Some(f.slot(0, 0)));
        let slots: Vec<usize> = find_all(&f.module(), "kernel32.dll", "GetProcAddress")
            .iter()
            .map(|x| x.slot)
            .collect();
        assert_eq!(slots, vec![f.slot(0, 0), f.slot(1, 0)]);
    }

    #[test]
    fn a_matching_descriptor_that_cannot_be_searched_does_not_hide_a_later_one() {
        let f = Fake::build(&[
            Imp::new("kernel32.dll").no_int().name("GetProcAddress"),
            Imp::new("kernel32.dll").name("GetProcAddress"),
        ]);
        assert_eq!(f.find("kernel32.dll", "GetProcAddress"), Some(f.slot(1, 0)));
    }

    // ---- hazard 2: ordinals and bounds -----------------------------------------------------

    #[test]
    fn ordinal_thunks_are_skipped_but_still_count_toward_the_slot_index() {
        let f = Fake::build(&[Imp::new("kernel32.dll")
            .ordinal(1)
            .raw((1 << 63) | 0x7fff_ffff)
            .name("Other")
            .name("Target")]);
        assert_eq!(f.find("kernel32.dll", "Target"), Some(f.slot(0, 3)));
    }

    #[test]
    fn an_ordinal_thunk_is_never_read_as_a_name_rva() {
        // Descriptor 0 imports "Target" by name. Descriptor 1 has an ordinal thunk whose low bits
        // are the RVA of that very name: reading the thunk as a name would match.
        let f = Fake::build(&[
            Imp::new("a.dll").name("Target"),
            Imp::new("b.dll").ordinal(0),
        ]);
        let name_rva = f.hint_rva[0][0].unwrap() as u64;
        f.put64(f.int_rva[1], (1 << 63) | name_rva);
        assert_eq!(f.find("b.dll", "Target"), None);
        assert_eq!(f.find("a.dll", "Target"), Some(f.slot(0, 0)));
    }

    #[test]
    fn out_of_range_and_malformed_name_rvas_are_skipped_without_reading() {
        let f = Fake::build(&[Imp::new("kernel32.dll")
            .raw(IMG as u64) // one past the end
            .raw(IMG as u64 - 1) // last byte: the name would start 2 bytes later
            .raw(0x7fff_ffff) // far outside
            .raw(0x1_0000_0000 | DATA as u64) // bit 32 set: not a 31-bit RVA
            .raw(0x4000_0000_0000_0000 | DATA as u64) // bit 62 set
            .raw(0xffff_ffff) // 32-bit all ones
            .name("Target")]);
        assert_eq!(f.find("kernel32.dll", "Target"), Some(f.slot(0, 6)));
        assert_eq!(f.find("kernel32.dll", "Missing"), None);
    }

    #[test]
    fn a_name_that_runs_past_the_end_of_the_module_is_not_read_beyond_it() {
        let f = Fake::build_with(&[Imp::new("kernel32.dll").name("Target")], true);
        let name = f.hint_rva[0][0].unwrap() + 2;
        let full = f.module();
        assert!(find_in(&full, "kernel32.dll", "Target").is_some());
        // The module ends inside the name. The bytes after it are still readable memory (the
        // allocation is bigger), so only the module size stops the read.
        for cut in [name + 1, name + 3, name + 5] {
            let m = Module { size: cut, ..full };
            assert_eq!(find_in(&m, "kernel32.dll", "Target"), None, "size {cut:#x}");
        }
        // The terminating NUL is the last byte of the module: fine.
        let m = Module {
            size: name + 6 + 1,
            ..full
        };
        assert!(find_in(&m, "kernel32.dll", "Target").is_some());
    }

    #[test]
    fn a_thunk_array_that_runs_off_the_module_ends_the_search_of_that_descriptor() {
        let f = Fake::build(&[Imp::new("kernel32.dll")
            .name("A")
            .name("B")
            .name("C")
            .name("D")]);
        // Cut the module inside the 3rd thunk. Every name is still before the cut.
        let m = Module {
            size: f.int_rva[0] + 8 * 2 + 3,
            ..f.module()
        };
        assert_eq!(find_in(&m, "kernel32.dll", "A"), Some(f.slot(0, 0)));
        assert_eq!(find_in(&m, "kernel32.dll", "B"), Some(f.slot(0, 1)));
        assert_eq!(find_in(&m, "kernel32.dll", "C"), None);
        assert_eq!(find_in(&m, "kernel32.dll", "D"), None);
        // A cut inside the descriptor array itself.
        let m = Module {
            size: DESCRIPTORS + 10,
            ..f.module()
        };
        assert_eq!(find_in(&m, "kernel32.dll", "A"), None);
    }

    #[test]
    fn a_slot_outside_the_module_or_misaligned_or_unreadable_is_not_returned() {
        let f = Fake::build(&[
            Imp::new("kernel32.dll").name("Target"),
            Imp::new("kernel32.dll").name("Target"),
            Imp::new("kernel32.dll").name("Target"),
            Imp::new("kernel32.dll").name("Target"),
        ]);
        // 0: FirstThunk points past the end; 1: misaligned; 2: in a no-access page; 3: good.
        let desc = |d: usize| DESCRIPTORS + d * 20 + 16;
        f.put32(desc(0), IMG as u32 - 4);
        f.put32(desc(1), f.iat_rva[1] as u32 + 4);
        f.put32(desc(2), 0x3000);
        f.mem.protect(0x3000, 0x1000, PAGE_NOACCESS);
        assert_eq!(f.find("kernel32.dll", "Target"), Some(f.slot(3, 0)));
        let all: Vec<usize> = find_all(&f.module(), "kernel32.dll", "Target")
            .iter()
            .map(|x| x.descriptor)
            .collect();
        assert_eq!(all, vec![3]);
        f.mem.protect(0x3000, 0x1000, PAGE_READWRITE);
    }

    #[test]
    fn the_descriptor_list_ends_at_the_first_null_name_or_null_iat() {
        let f = Fake::build(&[
            Imp::new("a.dll").name("X"),
            Imp::new("kernel32.dll").name("Target"),
        ]);
        // Make descriptor 1 look like the terminator: name 0. The loader would stop before it.
        let desc1 = DESCRIPTORS + 20;
        let saved = f.mem.get(desc1 + 12, 4);
        f.put32(desc1 + 12, 0);
        assert_eq!(f.find("kernel32.dll", "Target"), None);
        f.mem.put(desc1 + 12, &saved);
        assert!(f.find("kernel32.dll", "Target").is_some());
        // And FirstThunk 0.
        f.put32(desc1 + 16, 0);
        assert_eq!(f.find("kernel32.dll", "Target"), None);
    }

    #[test]
    fn a_descriptor_after_the_terminator_is_not_read() {
        let f = Fake::build(&[
            Imp::new("a.dll").name("X"),
            Imp::new("kernel32.dll").name("Target"),
        ]);
        assert!(f.find("kernel32.dll", "Target").is_some());
        // Move descriptor 1 behind a terminator: [a.dll, null, kernel32.dll].
        let moved = f.mem.get(DESCRIPTORS + 20, 20);
        f.mem.put(DESCRIPTORS + 20, &[0; 20]);
        f.mem.put(DESCRIPTORS + 40, &moved);
        assert_eq!(f.find("kernel32.dll", "Target"), None);
    }

    #[test]
    fn long_unterminated_and_unreadable_names_just_do_not_match() {
        let long = "k".repeat(300);
        let f = Fake::build(&[
            Imp::new(&long).name("Target"),
            Imp::new("kernel32.dll").name(&long).name("Target"),
        ]);
        assert_eq!(f.find("kernel32.dll", "Target"), Some(f.slot(1, 1)));
        assert_eq!(
            f.find(&long, "Target"),
            None,
            "a 300 byte DLL name is not accepted"
        );
        assert_eq!(
            f.find("kernel32.dll", &long),
            None,
            "nor a 300 byte function name"
        );
    }

    #[test]
    fn prefixes_and_empty_names_do_not_match() {
        let f = Fake::build(&[Imp::new("kernel32.dll")
            .name("Target2")
            .name("Targe")
            .name("")]);
        assert_eq!(f.find("kernel32.dll", "Target"), None);
        assert_eq!(f.find("kernel32.dll", "Target2"), Some(f.slot(0, 0)));
        assert_eq!(f.find("kernel32.dll", ""), None);
        assert_eq!(f.find("", "Target2"), None);
        assert_eq!(f.find("kernel32.dl", "Target2"), None);
        assert_eq!(f.find("kernel32.dll\0", "Target2"), None);
    }

    // ---- headers that are not a usable PE ---------------------------------------------------

    #[test]
    fn bad_headers_and_modules_give_none() {
        let f = Fake::build(&[Imp::new("kernel32.dll").name("Target")]);
        let good = f.module();
        assert!(find_in(&good, "kernel32.dll", "Target").is_some());
        assert_eq!(
            find_in(&Module { size: 0, ..good }, "kernel32.dll", "Target"),
            None
        );
        assert_eq!(
            find_in(&Module { base: 0, size: IMG }, "kernel32.dll", "Target"),
            None
        );
        // Not the start of the allocation.
        assert_eq!(
            find_in(
                &Module {
                    base: good.base + 0x1000,
                    size: IMG - 0x1000
                },
                "kernel32.dll",
                "Target"
            ),
            None
        );
        let break_and_check = |what: &str, off: usize, bytes: &[u8]| {
            let saved = f.mem.get(off, bytes.len());
            f.mem.put(off, bytes);
            assert_eq!(f.find("kernel32.dll", "Target"), None, "{what}");
            f.mem.put(off, &saved);
            assert!(
                f.find("kernel32.dll", "Target").is_some(),
                "{what}: restore"
            );
        };
        break_and_check("MZ", 0, &[0, 0]);
        break_and_check("e_lfanew past the image", 0x3c, &(IMG as u32).to_le_bytes());
        break_and_check(
            "e_lfanew near the end",
            0x3c,
            &(IMG as u32 - 8).to_le_bytes(),
        );
        break_and_check("e_lfanew huge", 0x3c, &u32::MAX.to_le_bytes());
        break_and_check("PE signature", NT, &[0, 0, 0, 0]);
        break_and_check("PE32 magic", OPT, &0x10bu16.to_le_bytes());
        break_and_check(
            "optional header too small",
            NT + 4 + 16,
            &100u16.to_le_bytes(),
        );
        break_and_check("no import directory", OPT + 108, &1u32.to_le_bytes());
        break_and_check("import RVA 0", OPT + 120, &0u32.to_le_bytes());
        break_and_check(
            "import RVA past the image",
            OPT + 120,
            &(IMG as u32).to_le_bytes(),
        );
        break_and_check("import RVA huge", OPT + 120, &u32::MAX.to_le_bytes());
        break_and_check("SizeOfImage 0", OPT + 56, &0u32.to_le_bytes());
        break_and_check(
            "SizeOfImage cuts the tables",
            OPT + 56,
            &(DESCRIPTORS as u32).to_le_bytes(),
        );
    }

    #[test]
    fn an_unreadable_header_page_gives_none() {
        let f = Fake::build(&[Imp::new("kernel32.dll").name("Target")]);
        f.mem.protect(0, 0x1000, PAGE_NOACCESS);
        assert_eq!(f.find("kernel32.dll", "Target"), None);
        f.mem.protect(0, 0x1000, PAGE_READWRITE);
        assert!(f.find("kernel32.dll", "Target").is_some());
    }

    // ---- swap ------------------------------------------------------------------------------

    /// The 8 bytes of the slot of descriptor `d`, thunk `i` of a fake image, as a number.
    fn fake_slot_value(f: &Fake, d: usize, i: usize) -> usize {
        let bytes: [u8; 8] = f
            .mem
            .get(f.iat_rva[d] + 8 * i, 8)
            .try_into()
            .expect("8 bytes");
        usize::from_le_bytes(bytes)
    }

    #[test]
    fn swap_returns_the_old_value_and_writes_the_new_one() {
        let f = Fake::build(&[Imp::new("kernel32.dll").name("Target")]);
        let slot = f.find("kernel32.dll", "Target").unwrap() as *mut usize;
        let old = fake_slot_value(&f, 0, 0);
        assert_eq!(old, 0x1111_0000);
        // SAFETY: `slot` is an 8-aligned entry in the fake image `f`, memory this test owns and
        // no other thread uses, and no Rust reference borrows it; the value is never called.
        assert_eq!(unsafe { swap(slot, 0xdead_beef_cafe) }, old);
        assert_eq!(fake_slot_value(&f, 0, 0), 0xdead_beef_cafe);
        // SAFETY: as above.
        assert_eq!(unsafe { swap(slot, old) }, 0xdead_beef_cafe);
        assert_eq!(fake_slot_value(&f, 0, 0), old);
    }

    #[test]
    fn swap_works_on_a_read_only_page_like_a_real_iat_and_puts_the_protection_back() {
        for protect in [PAGE_READONLY, PAGE_EXECUTE_READ] {
            let m = Mem::rw(0x1000);
            m.put(0x40, &0x1234_5678_9abc_def0u64.to_le_bytes());
            m.protect(0, 0x1000, protect);
            let slot = (m.base + 0x40) as *mut usize;
            // SAFETY: `slot` is 8 aligned and inside `m`, a page this test owns and no other
            // thread uses; no Rust reference borrows it.
            assert_eq!(unsafe { swap(slot, 7) }, 0x1234_5678_9abc_def0);
            assert_eq!(protect_of(m.base), protect);
            m.protect(0, 0x1000, PAGE_READWRITE);
            assert_eq!(m.get(0x40, 8), 7u64.to_le_bytes());
        }
    }

    #[test]
    fn swap_returns_zero_and_changes_nothing_for_a_bad_slot() {
        let m = Mem::rw(0x1000);
        m.put(0x40, &[0xaa; 16]);
        // SAFETY: the slot is inside `m`, a page this test owns; it is misaligned, so `swap`
        // refuses it before reading or writing anything.
        assert_eq!(unsafe { swap((m.base + 0x41) as *mut usize, 7) }, 0);
        assert_eq!(m.get(0x40, 16), vec![0xaa; 16]);
        // SAFETY: null is not mapped; `swap` refuses it (the read check fails) before touching it.
        assert_eq!(unsafe { swap(std::ptr::null_mut(), 7) }, 0);
        // SAFETY: address 8 is not mapped; `swap` refuses it (the read check fails) before
        // touching it.
        assert_eq!(
            unsafe { swap(std::ptr::without_provenance_mut::<usize>(8), 7) },
            0
        );
        // No access.
        m.protect(0, 0x1000, PAGE_NOACCESS);
        // SAFETY: the slot is inside `m`, a page this test owns; the page is unreadable, so `swap`
        // refuses it (the read check fails) before touching it.
        assert_eq!(unsafe { swap((m.base + 0x40) as *mut usize, 7) }, 0);
        assert_eq!(protect_of(m.base), PAGE_NOACCESS);
    }

    #[test]
    fn a_slot_holding_zero_is_not_written() {
        let m = Mem::rw(0x1000);
        m.put(0x40, &[0xaa; 16]);
        m.put(0x48, &[0; 8]);
        // SAFETY: the slot is 8 aligned and inside `m`, a page this test owns and no other thread
        // uses; `swap` reads 0 and refuses to write.
        assert_eq!(unsafe { swap((m.base + 0x48) as *mut usize, 7) }, 0);
        assert_eq!(m.get(0x40, 16), [vec![0xaa; 8], vec![0; 8]].concat());
    }

    // ---- swap: a write that does not read back as written ----------------------------------

    const OLD: u64 = 0x1234_5678_9abc_def0;
    const NEW: usize = 0x0fed_cba9_8765_4321;

    /// A page with `OLD` at offset 0x40.
    fn slot_page() -> (Mem, *mut usize) {
        let m = Mem::rw(0x1000);
        m.put(0x40, &OLD.to_le_bytes());
        let slot = (m.base + 0x40) as *mut usize;
        (m, slot)
    }

    /// The `(expected, new)` bytes one call of the writer was given.
    type Call = (Vec<u8>, Vec<u8>);

    /// Run `swap_with` with a writer that does the real write, then reports `ReadBack` on its
    /// first call. `second` decides what the second call (the put-back) does: `None` runs the real
    /// `write_checked`, `Some(e)` fails with `e` without writing. `interfere` is run between the
    /// first call's write and its `ReadBack` report. Returns the swap result and every
    /// `(expected, new)` pair the writer was given.
    fn swap_with_failing_readback(
        slot: *mut usize,
        second: Option<PatchError>,
        interfere: impl FnOnce(),
    ) -> (usize, Vec<Call>) {
        let mut calls: Vec<Call> = Vec::new();
        let mut interfere = Some(interfere);
        let mut write = |addr: usize, expected: &[u8], new: &[u8]| {
            calls.push((expected.to_vec(), new.to_vec()));
            if calls.len() == 2 {
                if let Some(e) = second.clone() {
                    return Err(e);
                }
            }
            // SAFETY: `swap_with` calls this with the address of `slot`, 8 bytes inside a page
            // the test owns, and with owned arrays that are not part of the target.
            let r = unsafe { write_checked(addr, expected, new) };
            if calls.len() == 1 {
                if let Some(f) = interfere.take() {
                    f();
                }
                return r.and(Err(PatchError::ReadBack));
            }
            r
        };
        // SAFETY: `slot` is 8 aligned and inside a page the test owns and no other thread uses,
        // and nothing borrows it; `write` forwards to `write_checked`.
        let result = unsafe { swap_with(slot, NEW, &mut write) };
        (result, calls)
    }

    #[test]
    fn a_read_back_failure_puts_the_old_value_back_and_returns_zero() {
        let (m, slot) = slot_page();
        let (result, calls) = swap_with_failing_readback(slot, None, || {});
        assert_eq!(result, 0);
        assert_eq!(m.get(0x40, 8), OLD.to_le_bytes(), "the old value is back");
        assert_eq!(
            calls,
            vec![
                (OLD.to_le_bytes().to_vec(), NEW.to_le_bytes().to_vec()),
                (NEW.to_le_bytes().to_vec(), OLD.to_le_bytes().to_vec()),
            ]
        );
    }

    #[test]
    fn if_the_put_back_fails_too_the_slot_keeps_the_new_value_and_the_result_is_zero() {
        let (m, slot) = slot_page();
        let (result, calls) = swap_with_failing_readback(slot, Some(PatchError::Protect(5)), || {});
        assert_eq!(result, 0);
        assert_eq!(calls.len(), 2);
        // The documented case where 0 follows a changed slot.
        assert_eq!(m.get(0x40, 8), NEW.to_le_bytes());
    }

    #[test]
    fn the_put_back_does_not_overwrite_a_value_somebody_else_wrote() {
        let (m, slot) = slot_page();
        let other = 0x7777_6666_5555_4444u64.to_le_bytes();
        let (result, calls) = swap_with_failing_readback(slot, None, || m.put(0x40, &other));
        assert_eq!(result, 0);
        // The put-back compared first (it expected NEW), found `other`, and left it alone.
        assert_eq!(calls.len(), 2);
        assert_eq!(m.get(0x40, 8), other);
    }

    #[test]
    fn another_write_error_returns_zero_without_a_put_back() {
        let (m, slot) = slot_page();
        let mut calls = 0;
        let mut write = |_: usize, _: &[u8], _: &[u8]| -> Result<Patched, PatchError> {
            calls += 1;
            Err(PatchError::Mismatch {
                at: 0,
                found: vec![],
            })
        };
        // SAFETY: `slot` is 8 aligned and inside a page the test owns and no other thread uses;
        // this writer never touches memory.
        assert_eq!(unsafe { swap_with(slot, NEW, &mut write) }, 0);
        assert_eq!(calls, 1);
        assert_eq!(m.get(0x40, 8), OLD.to_le_bytes());
    }
}
