//! Which game build is this?
//!
//! The patches in this plugin are written against one build of Enshrouded (it ships as two exes,
//! see [`KNOWN`]). [`sha256_file`] hashes the game's exe and [`classify`] says whether that hash
//! is a build we know.

use std::fs::File;
use std::io::{self, Read};
use std::path::Path;

use windows_sys::Win32::Foundation::NTSTATUS;
use windows_sys::Win32::Security::Cryptography::{
    BCryptCloseAlgorithmProvider, BCryptCreateHash, BCryptDestroyHash, BCryptFinishHash,
    BCryptHashData, BCryptOpenAlgorithmProvider, BCRYPT_ALG_HANDLE, BCRYPT_HASH_HANDLE,
    BCRYPT_SHA256_ALGORITHM,
};

/// Game builds this plugin was written against: `(sha256 of the exe in lower-case hex, label)`.
///
/// `f68140a3-steam` (Steam buildid 26082781) is the same code as `f68140a3`: all of `.text`,
/// `.rdata`, `.data`, `.pdata`, `.rsrc` and `.reloc` are byte-identical at the same addresses.
/// Only the PE headers and the Steam DRM stub (`.bind`) differ. The label lets the log say which
/// of the two files is running.
pub const KNOWN: &[(&str, &str)] = &[
    (
        "b28b448a8dbf48c932b717f035031f5a89712a7cccfcc2d6cac0d47e6421c12a",
        "f68140a3",
    ),
    (
        "d64966e23b1a458ee6f147bcbceac0b577fa7e39a212f371287d8cc119817882",
        "f68140a3-steam",
    ),
];

/// How many bytes of the file are read, and handed to the hash, at a time.
const CHUNK: usize = 256 * 1024;

/// What [`classify`] makes of a hash.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Build {
    /// A build in [`KNOWN`], by its label.
    Known(&'static str),
    /// Any other build, by its hash.
    Unknown([u8; 32]),
}

/// Look `hash` up in [`KNOWN`].
pub fn classify(hash: [u8; 32]) -> Build {
    let hex = to_hex(&hash);
    match KNOWN.iter().find(|(known, _)| *known == hex) {
        Some(&(_, label)) => Build::Known(label),
        None => Build::Unknown(hash),
    }
}

/// `hash` as 64 lower-case hex digits.
pub fn to_hex(hash: &[u8; 32]) -> String {
    hash.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// SHA-256 of the file at `path`, read in one pass in chunks of [`CHUNK`] bytes, so the file
/// (the game exe is 35 MB) is never held in memory. The hash is BCrypt's (`bcrypt.dll`, part of
/// Windows).
pub fn sha256_file(path: &Path) -> io::Result<[u8; 32]> {
    let mut file = File::open(path)?;
    // `hash` borrows `provider`, so the hash is always destroyed first, on every path out.
    let provider = Provider::open_sha256()?;
    let mut hash = Hash::create(&provider)?;
    let mut chunk = vec![0u8; CHUNK];
    loop {
        let read = match file.read(&mut chunk) {
            Ok(0) => break,
            Ok(read) => read,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        };
        let data = chunk
            .get(..read)
            .ok_or_else(|| io::Error::other("read returned more bytes than the buffer holds"))?;
        hash.update(data)?;
    }
    hash.finish()
}

/// The length of a SHA-256 digest in bytes.
const DIGEST_LEN: u32 = 32;

/// `Ok` for a BCrypt status that means success (zero or positive), else an error that names the
/// call and the status.
fn check(call: &str, status: NTSTATUS) -> io::Result<()> {
    if status >= 0 {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "{call} failed: NTSTATUS {:#010x}",
            status as u32
        )))
    }
}

/// An open BCrypt SHA-256 provider. It is closed when this value is dropped.
struct Provider(BCRYPT_ALG_HANDLE);

impl Provider {
    fn open_sha256() -> io::Result<Provider> {
        let mut handle: BCRYPT_ALG_HANDLE = std::ptr::null_mut();
        // SAFETY: `handle` is a valid place for the result; `BCRYPT_SHA256_ALGORITHM` is a
        // NUL-terminated UTF-16 string; a null implementation name selects the default provider
        // for the algorithm; 0 is "no flags".
        let status = unsafe {
            BCryptOpenAlgorithmProvider(&mut handle, BCRYPT_SHA256_ALGORITHM, std::ptr::null(), 0)
        };
        check("BCryptOpenAlgorithmProvider", status)?;
        // From here on `Drop` closes it. (On failure nothing was opened, so there is nothing to
        // close above.)
        Ok(Provider(handle))
    }
}

impl Drop for Provider {
    fn drop(&mut self) {
        // SAFETY: `self.0` came from a successful `BCryptOpenAlgorithmProvider` and is closed
        // here, once. `Hash` borrows the provider, so no hash created from it is still alive.
        unsafe { BCryptCloseAlgorithmProvider(self.0, 0) };
    }
}

/// A BCrypt hash in progress. It is destroyed when this value is dropped.
struct Hash<'provider> {
    handle: BCRYPT_HASH_HANDLE,
    /// Keeps the provider open for as long as the hash lives.
    _provider: &'provider Provider,
}

impl<'provider> Hash<'provider> {
    fn create(provider: &'provider Provider) -> io::Result<Hash<'provider>> {
        let mut handle: BCRYPT_HASH_HANDLE = std::ptr::null_mut();
        // SAFETY: `provider.0` is an open provider; `handle` is a valid place for the result. A
        // null object buffer of size 0 makes BCrypt allocate the hash object itself and
        // `BCryptDestroyHash` free it (Windows 7 and later); a null secret and 0 flags make it a
        // plain hash, not a MAC.
        let status = unsafe {
            BCryptCreateHash(
                provider.0,
                &mut handle,
                std::ptr::null_mut(),
                0,
                std::ptr::null(),
                0,
                0,
            )
        };
        check("BCryptCreateHash", status)?;
        Ok(Hash {
            handle,
            _provider: provider,
        })
    }

    /// Feed `data` to the hash.
    fn update(&mut self, data: &[u8]) -> io::Result<()> {
        let len = u32::try_from(data.len())
            .map_err(|_| io::Error::other("a chunk is longer than BCrypt accepts"))?;
        // SAFETY: `self.handle` is a live hash; `data` is valid for `len` bytes.
        let status = unsafe { BCryptHashData(self.handle, data.as_ptr(), len, 0) };
        check("BCryptHashData", status)
    }

    /// The digest of everything fed so far. It takes the hash by value: BCrypt accepts no more
    /// data after `BCryptFinishHash`, and this way the handle cannot be used again. `Drop` still
    /// destroys it, on the error path as well.
    fn finish(self) -> io::Result<[u8; 32]> {
        let mut digest = [0u8; 32];
        // SAFETY: `self.handle` is a live hash; `digest` is valid for `DIGEST_LEN` (32) bytes, the
        // size of a SHA-256 digest.
        let status = unsafe { BCryptFinishHash(self.handle, digest.as_mut_ptr(), DIGEST_LEN, 0) };
        check("BCryptFinishHash", status)?;
        Ok(digest)
    }
}

impl Drop for Hash<'_> {
    fn drop(&mut self) {
        // SAFETY: `self.handle` came from a successful `BCryptCreateHash` and is destroyed here,
        // once.
        unsafe { BCryptDestroyHash(self.handle) };
    }
}

/// Where the `#[ignore]` tests that read the real game exe find it. One path for all of them.
#[cfg(test)]
pub(crate) mod test_support {
    use std::path::PathBuf;

    /// The environment variable that overrides [`STEAM_EXE`].
    pub const EXE_ENV: &str = "ENSHROUDED_EXE";

    /// The exe of the Steam install of the game (Steam buildid 26082781).
    pub const STEAM_EXE: &str =
        r"C:\Program Files (x86)\Steam\steamapps\common\Enshrouded\enshrouded.exe";

    /// The exe the real-exe tests read: `ENSHROUDED_EXE` if it is set and not empty, else
    /// [`STEAM_EXE`].
    pub fn game_exe_path() -> PathBuf {
        match std::env::var_os(EXE_ENV) {
            Some(path) if !path.is_empty() => PathBuf::from(path),
            _ => PathBuf::from(STEAM_EXE),
        }
    }

    /// [`game_exe_path`] if there is a file there. If not, print why `test` is skipped and give
    /// `None`; the test then returns, so a PC without the game does not fail it.
    pub fn game_exe_or_skip(test: &str) -> Option<PathBuf> {
        let path = game_exe_path();
        if path.is_file() {
            Some(path)
        } else {
            println!(
                "SKIPPED {test}: no game exe at {} (set {EXE_ENV} to the exe to test)",
                path.display()
            );
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A file under the system temp dir that is removed again when this value is dropped.
    struct TempFile(PathBuf);

    impl TempFile {
        fn with(bytes: &[u8]) -> TempFile {
            static COUNTER: AtomicUsize = AtomicUsize::new(0);
            let dir = std::env::temp_dir().join("enshrouded-headroom-tests");
            std::fs::create_dir_all(&dir).unwrap();
            let path = dir.join(format!(
                "build_id-{}-{}.bin",
                std::process::id(),
                COUNTER.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::write(&path, bytes).unwrap();
            TempFile(path)
        }
    }

    impl Drop for TempFile {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    fn tempfile_with(bytes: &[u8]) -> TempFile {
        TempFile::with(bytes)
    }

    fn hex(hash: &[u8; 32]) -> String {
        hash.iter().map(|b| format!("{b:02x}")).collect()
    }

    fn unhex(text: &str) -> [u8; 32] {
        let mut out = [0u8; 32];
        assert_eq!(text.len(), 64);
        for (i, byte) in out.iter_mut().enumerate() {
            *byte = u8::from_str_radix(&text[2 * i..2 * i + 2], 16).unwrap();
        }
        out
    }

    #[test]
    fn sha256_of_abc() {
        let p = tempfile_with(b"abc");
        assert_eq!(
            hex(&sha256_file(&p.0).unwrap()),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn sha256_of_the_nist_million_a_vector() {
        // 1,000,000 bytes: several chunks and a short last one.
        let p = tempfile_with(&vec![b'a'; 1_000_000]);
        assert_eq!(
            hex(&sha256_file(&p.0).unwrap()),
            "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0"
        );
    }

    #[test]
    fn sha256_is_right_on_and_around_the_chunk_boundaries() {
        // Expected values from `head -c N /dev/zero | sha256sum`. They sit one byte below, on and
        // one byte above one chunk, on two chunks, and at length 0.
        assert_eq!(
            CHUNK, 262_144,
            "the lengths below were picked for this chunk size"
        );
        let cases: [(usize, &str); 5] = [
            (
                0,
                "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
            ),
            (
                262_143,
                "5e44317a2003e8c8ce0726c637bf61c8f4c861c01c276b3d70d0d866e72fa288",
            ),
            (
                262_144,
                "8a39d2abd3999ab73c34db2476849cddf303ce389b35826850f9a700589b4a90",
            ),
            (
                262_145,
                "b27a032984ea8a6bec700c3d6f63f8fcfbf8ff8ef87e972891feda4eea4aad0c",
            ),
            (
                524_288,
                "07854d2fef297a06ba81685e660c332de36d5d18d546927d30daad6d7fda1541",
            ),
        ];
        for (len, expected) in cases {
            let p = tempfile_with(&vec![0u8; len]);
            assert_eq!(hex(&sha256_file(&p.0).unwrap()), expected, "length {len}");
        }
    }

    #[test]
    fn sha256_of_a_missing_file_is_not_found() {
        let p = tempfile_with(b"x");
        let gone = p.0.with_extension("missing");
        let error = sha256_file(&gone).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
    }

    #[test]
    fn known_build_classified() {
        assert!(matches!(
            classify(unhex(KNOWN[0].0)),
            Build::Known("f68140a3")
        ));
    }

    /// sha256 of the Steam exe (Steam buildid 26082781; 35,338,312 bytes).
    const STEAM_SHA256: &str = "d64966e23b1a458ee6f147bcbceac0b577fa7e39a212f371287d8cc119817882";
    /// sha256 of the exe of the old install (35,338,752 bytes).
    const OLD_SHA256: &str = "b28b448a8dbf48c932b717f035031f5a89712a7cccfcc2d6cac0d47e6421c12a";

    #[test]
    fn steam_exe_is_build_f68140a3() {
        assert_eq!(
            classify(unhex(STEAM_SHA256)),
            Build::Known("f68140a3-steam")
        );
        // The old exe is still the same known build as before.
        assert_eq!(classify(unhex(OLD_SHA256)), Build::Known("f68140a3"));
    }

    // ---- the Steam exe against the reference copy of the old exe (ignored: reads both) -------

    /// A section of a PE file, as the section table gives it.
    struct Section {
        name: String,
        virtual_size: u32,
        rva: u32,
        raw_size: u32,
        raw_ptr: u32,
    }

    /// The parts of a PE32+ file that the comparison needs.
    struct Pe {
        entry_rva: u32,
        section_alignment: u32,
        sections: Vec<Section>,
    }

    impl Pe {
        /// The size `section` takes in memory: its virtual size rounded up to a whole number of
        /// section-aligned pages. (The Steam headers give each virtual size exactly, the old
        /// headers give it rounded up; the loader maps the same pages for both.)
        fn pages_of(&self, section: &Section) -> u64 {
            let align = u64::from(self.section_alignment.max(1));
            u64::from(section.virtual_size).div_ceil(align) * align
        }

        fn section(&self, name: &str) -> Result<&Section, String> {
            self.sections
                .iter()
                .find(|s| s.name == name)
                .ok_or_else(|| format!("no {name} section"))
        }

        /// The bytes of `section` that are in the file.
        fn raw<'file>(&self, file: &'file [u8], section: &Section) -> Result<&'file [u8], String> {
            let start = section.raw_ptr as usize;
            let end = start + section.raw_size as usize;
            file.get(start..end).ok_or_else(|| {
                format!(
                    "{}: raw data {start:#x}..{end:#x} is past the end of the file ({} bytes)",
                    section.name,
                    file.len()
                )
            })
        }

        /// The name of the section whose memory range holds `rva`.
        fn section_of(&self, rva: u32) -> Option<&str> {
            self.sections
                .iter()
                .find(|s| (s.rva..s.rva.saturating_add(s.virtual_size)).contains(&rva))
                .map(|s| s.name.as_str())
        }
    }

    /// Read the headers of a PE32+ file. A file that is not one gives an error that says what is
    /// wrong, not a panic.
    fn parse_pe(file: &[u8]) -> Result<Pe, String> {
        use crate::scan::{u16_at, u32_at};
        let nt = u32_at(file, 0x3c).ok_or("no e_lfanew at 0x3c")? as usize;
        if file.get(nt..nt + 4) != Some(b"PE\0\0".as_slice()) {
            return Err(format!("no PE signature at e_lfanew {nt:#x}"));
        }
        let section_count = u16_at(file, nt + 6).ok_or("no NumberOfSections")? as usize;
        let optional_size = u16_at(file, nt + 20).ok_or("no SizeOfOptionalHeader")? as usize;
        let optional = nt + 24;
        if u16_at(file, optional) != Some(0x20b) {
            return Err("the optional header is not PE32+ (magic 0x20b)".to_owned());
        }
        let entry_rva = u32_at(file, optional + 16).ok_or("no AddressOfEntryPoint")?;
        let section_alignment = u32_at(file, optional + 32).ok_or("no SectionAlignment")?;
        let table = optional + optional_size;
        let mut sections = Vec::new();
        for i in 0..section_count {
            let at = table + i * 40;
            let header = file
                .get(at..at + 40)
                .ok_or_else(|| format!("section header {i} is past the end of the file"))?;
            let end = header.iter().position(|&b| b == 0).unwrap_or(8).min(8);
            let name = String::from_utf8_lossy(header.get(..end).ok_or("name")?).into_owned();
            sections.push(Section {
                name,
                virtual_size: u32_at(header, 8).ok_or("VirtualSize")?,
                rva: u32_at(header, 12).ok_or("VirtualAddress")?,
                raw_size: u32_at(header, 16).ok_or("SizeOfRawData")?,
                raw_ptr: u32_at(header, 20).ok_or("PointerToRawData")?,
            });
        }
        Ok(Pe {
            entry_rva,
            section_alignment,
            sections,
        })
    }

    /// The old exe is kept under `reference/` in the repo (not in git).
    fn reference_exe() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("reference")
            .join("old-build")
            .join("enshrouded.exe")
    }

    #[test]
    #[ignore = "reads the Steam exe and reference/old-build/enshrouded.exe from disk"]
    fn steam_exe_matches_the_old_build_except_the_stub() {
        let Some(steam_path) = test_support::game_exe_or_skip("steam_exe_matches_the_old_build")
        else {
            return;
        };
        let old_path = reference_exe();
        if !old_path.is_file() {
            println!(
                "SKIPPED steam_exe_matches_the_old_build: no reference copy of the old exe at {}",
                old_path.display()
            );
            return;
        }
        let steam_file = std::fs::read(&steam_path).expect("read the Steam exe");
        let old_file = std::fs::read(&old_path).expect("read the reference exe");
        let steam = parse_pe(&steam_file).expect("PE headers of the Steam exe");
        let old = parse_pe(&old_file).expect("PE headers of the reference exe");

        // The hash of each real file is the one KNOWN names, so the table entry is not a typo.
        let hash_of = |p: &Path| classify(sha256_file(p).expect("hash the exe"));
        assert_eq!(hash_of(&steam_path), Build::Known("f68140a3-steam"));
        assert_eq!(hash_of(&old_path), Build::Known("f68140a3"));

        // The game's code and data: the same place in memory and the same bytes.
        for name in [".text", ".rdata", ".data", ".pdata", ".rsrc", ".reloc"] {
            let (s, o) = (
                steam.section(name).expect("Steam exe"),
                old.section(name).expect("reference exe"),
            );
            assert_eq!(s.rva, o.rva, "{name}: virtual address");
            // The two headers state the virtual size differently (exact, or rounded up to the
            // page); the pages the section takes in memory are the same.
            assert_eq!(steam.pages_of(s), old.pages_of(o), "{name}: size in memory");
            let same = steam.raw(&steam_file, s).expect("Steam exe")
                == old.raw(&old_file, o).expect("reference exe");
            assert!(same, "{name}: the bytes differ");
            println!(
                "{name:<7} rva {:#x} virtual size {:#x} (old header {:#x}) raw size {:#x}: \
                 byte-identical",
                s.rva, s.virtual_size, o.virtual_size, s.raw_size
            );
        }

        // The only code that is new is Steam's stub, and the exe now starts there; the old exe
        // started in the game's own `.text`.
        assert_eq!(steam.section_of(steam.entry_rva), Some(".bind"));
        assert_eq!(old.section_of(old.entry_rva), Some(".text"));
        println!(
            "entry point: Steam {:#x} in .bind, old {:#x} in .text",
            steam.entry_rva, old.entry_rva
        );
    }

    #[test]
    fn parse_pe_reports_a_bad_file_instead_of_panicking() {
        assert!(parse_pe(&[]).is_err());
        assert!(parse_pe(&[0u8; 0x40]).is_err());
        // A valid e_lfanew that points past the end of the file.
        let mut file = vec![0u8; 0x40];
        file[0x3c] = 0xff;
        assert!(parse_pe(&file).is_err());
        // A PE signature, then nothing.
        let mut file = vec![0u8; 0x80];
        file[0x3c] = 0x40;
        file[0x40..0x44].copy_from_slice(b"PE\0\0");
        assert!(parse_pe(&file).is_err());
    }

    #[test]
    fn any_other_hash_is_unknown_and_keeps_its_hash() {
        let mut hash = unhex(KNOWN[0].0);
        hash[31] ^= 1;
        assert_eq!(classify(hash), Build::Unknown(hash));
        assert_eq!(classify([0; 32]), Build::Unknown([0; 32]));
    }

    #[test]
    fn to_hex_is_64_lower_case_digits() {
        let mut hash = [0u8; 32];
        hash[0] = 0xab;
        hash[1] = 0x01;
        hash[31] = 0xff;
        let text = to_hex(&hash);
        assert_eq!(text.len(), 64);
        assert!(text.starts_with("ab01") && text.ends_with("ff"), "{text}");
        assert_eq!(text, text.to_ascii_lowercase());
        assert_eq!(to_hex(&unhex(KNOWN[0].0)), KNOWN[0].0);
    }

    #[test]
    fn the_known_table_is_well_formed() {
        assert!(!KNOWN.is_empty());
        for (i, (hash, label)) in KNOWN.iter().enumerate() {
            assert_eq!(hash.len(), 64, "{label}");
            assert!(
                hash.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')),
                "{label}: the hash must be lower-case hex"
            );
            assert!(!label.is_empty());
            assert!(
                KNOWN[i + 1..].iter().all(|(other, _)| other != hash),
                "{label}: duplicate hash"
            );
        }
    }
}
