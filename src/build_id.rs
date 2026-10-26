//! Which game build is this?
//!
//! The patches in this plugin are written against one build of Enshrouded. [`sha256_file`] hashes
//! the game's exe and [`classify`] says whether that hash is a build we know.

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
pub const KNOWN: &[(&str, &str)] = &[(
    "b28b448a8dbf48c932b717f035031f5a89712a7cccfcc2d6cac0d47e6421c12a",
    "f68140a3",
)];

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
