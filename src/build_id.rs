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

/// Where the `#[ignore]` tests that read the real game exe find it.
///
/// Two rules, the same for every path here:
/// - An environment variable that is set (and not empty) is an explicit request. If it does not
///   name a readable file, the test **panics**. It is never skipped.
/// - Only the default, the Steam path, may be skipped, and only if there is no file there (a PC
///   without the game). Any other problem with it (a directory, no access) panics too.
///
/// A skip prints a line and the test returns, so the harness still reports it as `ok`. The line
/// is captured: it shows only with `-- --ignored --nocapture`. A green `--ignored` run without
/// `--nocapture` therefore does not show that a game exe was read.
#[cfg(test)]
pub(crate) mod test_support {
    use std::ffi::OsString;
    use std::io::ErrorKind;
    use std::path::{Path, PathBuf};

    /// The environment variable that names the exe for the real-exe tests.
    pub const EXE_ENV: &str = "ENSHROUDED_EXE";

    /// The environment variable that names the Steam exe for the Steam-against-old-build test,
    /// which must have the Steam exe whatever `ENSHROUDED_EXE` says.
    pub const STEAM_EXE_ENV: &str = "ENSHROUDED_STEAM_EXE";

    /// The exe of the Steam install of the game (Steam buildid 26082781).
    pub const STEAM_EXE: &str =
        r"C:\Program Files (x86)\Steam\steamapps\common\Enshrouded\enshrouded.exe";

    /// The exe for the real-exe tests: `ENSHROUDED_EXE` if set (panics if it is not a readable
    /// file), else [`STEAM_EXE`] (`None`, after a printed reason, if there is no file there).
    pub fn game_exe_or_skip(test: &str) -> Option<PathBuf> {
        locate(
            test,
            EXE_ENV,
            std::env::var_os(EXE_ENV),
            Path::new(STEAM_EXE),
        )
    }

    /// The Steam exe: `ENSHROUDED_STEAM_EXE` if set (panics if it is not a readable file), else
    /// [`STEAM_EXE`] (`None`, after a printed reason, if there is no file there).
    pub fn steam_exe_or_skip(test: &str) -> Option<PathBuf> {
        locate(
            test,
            STEAM_EXE_ENV,
            std::env::var_os(STEAM_EXE_ENV),
            Path::new(STEAM_EXE),
        )
    }

    /// Why a path is not a file the tests can read.
    enum Unusable {
        /// There is nothing at that path.
        Missing,
        /// There is something, but not a readable file; the text says what.
        Other(String),
    }

    fn check_file(path: &Path) -> Result<(), Unusable> {
        match std::fs::metadata(path) {
            Ok(meta) if meta.is_file() => {}
            Ok(_) => return Err(Unusable::Other("it is not a file".to_owned())),
            Err(error) if error.kind() == ErrorKind::NotFound => return Err(Unusable::Missing),
            Err(error) => return Err(Unusable::Other(format!("it cannot be read: {error}"))),
        }
        std::fs::File::open(path)
            .map(drop)
            .map_err(|error| Unusable::Other(format!("it cannot be opened: {error}")))
    }

    /// The rules in the module docs, with the environment value passed in (`explicit`), so a
    /// test can run them without touching the process environment.
    pub(super) fn locate(
        test: &str,
        env_name: &str,
        explicit: Option<OsString>,
        default: &Path,
    ) -> Option<PathBuf> {
        if let Some(value) = explicit.filter(|value| !value.is_empty()) {
            let path = PathBuf::from(value);
            return match check_file(&path) {
                Ok(()) => Some(path),
                Err(why) => {
                    let why = match why {
                        Unusable::Missing => "there is no file there".to_owned(),
                        Unusable::Other(text) => text,
                    };
                    panic!(
                        "{test}: {env_name} is set to {}, but {why}. A path that is set is never \
                         skipped: fix it, or unset {env_name}.",
                        path.display()
                    )
                }
            };
        }
        match check_file(default) {
            Ok(()) => Some(default.to_path_buf()),
            Err(Unusable::Missing) => {
                println!(
                    "SKIPPED {test}: no game exe at {} (the default Steam path; set {env_name} \
                     to the exe to test)",
                    default.display()
                );
                None
            }
            Err(Unusable::Other(why)) => panic!(
                "{test}: the default exe {} is there, but {why}. Set {env_name} to the exe to \
                 test.",
                default.display()
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;
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

    use crate::scan::parse_pe_file as parse_pe;

    /// The old exe is kept under `reference/` in the repo (not in git).
    fn reference_exe() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("reference")
            .join("old-build")
            .join("enshrouded.exe")
    }

    /// The bytes of the reference copy. It must be there: no copy is a failure, not a skip.
    fn read_reference(path: &Path) -> Vec<u8> {
        std::fs::read(path).unwrap_or_else(|error| {
            panic!(
                "the reference copy of the old exe is not readable at {} ({error}). It is not in \
                 git: put the old build's exe there.",
                path.display()
            )
        })
    }

    #[test]
    #[ignore = "reads the Steam exe (ENSHROUDED_STEAM_EXE, else the Steam path) and \
                reference/old-build/enshrouded.exe; a skip shows only with -- --ignored --nocapture"]
    fn steam_exe_matches_the_old_build_except_the_stub() {
        // Its own Steam path: `ENSHROUDED_EXE` may name any exe, and that must not fail this.
        let Some(steam_path) = test_support::steam_exe_or_skip("steam_exe_matches_the_old_build")
        else {
            return;
        };
        // With the Steam exe in hand, the reference copy is required: a missing one fails.
        let old_path = reference_exe();
        let old_file = read_reference(&old_path);
        let steam_file = std::fs::read(&steam_path).expect("read the Steam exe");
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

    // ---- finding the exe for the real-exe tests (always run) -----------------------------------

    #[test]
    #[should_panic(expected = "C:/nope.exe, but there is no file there")]
    fn a_path_that_is_set_but_missing_panics_instead_of_skipping() {
        let default = tempfile_with(b"x");
        let set = Some(OsString::from("C:/nope.exe"));
        test_support::locate("t", "ENSHROUDED_EXE", set, &default.0);
    }

    #[test]
    #[should_panic(expected = "it is not a file")]
    fn a_path_that_is_set_to_a_directory_panics() {
        let set = Some(std::env::temp_dir().into_os_string());
        test_support::locate("t", "ENSHROUDED_EXE", set, std::path::Path::new("unused"));
    }

    #[test]
    fn a_path_that_is_set_to_a_file_is_used_and_the_default_is_not_looked_at() {
        let file = tempfile_with(b"x");
        let set = Some(file.0.clone().into_os_string());
        let got = test_support::locate("t", "ENSHROUDED_EXE", set, std::path::Path::new("C:/no"));
        assert_eq!(got, Some(file.0.clone()));
    }

    #[test]
    fn an_empty_value_or_none_means_the_default() {
        let file = tempfile_with(b"x");
        for set in [None, Some(OsString::new())] {
            let got = test_support::locate("t", "ENSHROUDED_EXE", set, &file.0);
            assert_eq!(got, Some(file.0.clone()));
        }
    }

    #[test]
    fn a_missing_default_is_the_one_case_that_skips() {
        let gone = tempfile_with(b"x").0.with_extension("missing");
        for set in [None, Some(OsString::new())] {
            assert_eq!(
                test_support::locate("t", "ENSHROUDED_EXE", set, &gone),
                None
            );
        }
    }

    #[test]
    #[should_panic(expected = "the reference copy of the old exe is not readable at")]
    fn a_missing_reference_copy_fails_instead_of_skipping() {
        let gone = tempfile_with(b"x").0.with_extension("missing");
        read_reference(&gone);
    }

    #[test]
    #[should_panic(expected = "is there, but it is not a file")]
    fn a_default_that_is_not_a_file_panics_instead_of_skipping() {
        let dir = std::env::temp_dir();
        test_support::locate("t", "ENSHROUDED_EXE", None, &dir);
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
