//! Dedicated-server rejection through the actual ASI export in an owned synthetic host.
use enshrouded_headroom::{
    config::{DEFAULT_INI, INI_FILE_NAME},
    log::LOG_FILE_NAME,
};
use std::{
    os::windows::ffi::OsStrExt,
    path::{Path, PathBuf},
    process::Command,
};
use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
const CHILD: &str = "HEADROOM_OWN_SERVER_HOST_DIR";
const TEST: &str = "server_host_skips_every_feature_even_with_unknown_build_enabled";
const DLL: &str = "enshrouded_headroom.dll";
#[test]
fn server_host_skips_every_feature_even_with_unknown_build_enabled() {
    if let Some(dir) = std::env::var_os(CHILD) {
        let path = Path::new(&dir).join(DLL);
        let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
        // SAFETY: NUL-terminated owned path lives through LoadLibraryW.
        let module = unsafe { LoadLibraryW(wide.as_ptr()) };
        assert!(!module.is_null());
        // SAFETY: module is loaded and export name is NUL-terminated.
        let init = unsafe { GetProcAddress(module, c"InitializeASI".as_ptr().cast()) }.unwrap();
        // SAFETY: InitializeASI exports this ABI and signature in src/lib.rs.
        let init: extern "C" fn() = unsafe { std::mem::transmute(init) };
        init();
        return;
    }
    let exe = std::env::current_exe().unwrap();
    let dir = std::env::temp_dir()
        .join("headroom-owned-process-tests")
        .join(std::process::id().to_string());
    std::fs::create_dir_all(&dir).unwrap();
    let dll = exe.parent().unwrap().join(DLL);
    assert!(dll.is_file());
    std::fs::copy(dll, dir.join(DLL)).unwrap();
    std::fs::copy(&exe, dir.join("enshrouded_server.exe")).unwrap();
    std::fs::write(
        dir.join(INI_FILE_NAME),
        DEFAULT_INI.replace("AllowUnknownBuild=0", "AllowUnknownBuild=1"),
    )
    .unwrap();
    let output = Command::new(dir.join("enshrouded_server.exe"))
        .args(["--exact", TEST, "--nocapture"])
        .env(CHILD, &dir)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let log = std::fs::read_to_string(dir.join(LOG_FILE_NAME)).unwrap();
    let messages: Vec<_> = log
        .lines()
        .map(|l| l.split_once("] ").map_or(l, |(_, m)| m))
        .collect();
    assert_eq!(
        messages,
        ["enshrouded-headroom: not used on a dedicated server"],
        "{log}"
    );
    // Both paths are owned copies inside this test's unique scratch directory.
    let checked: PathBuf = std::env::temp_dir().join("headroom-owned-process-tests");
    assert!(dir.starts_with(checked));
    std::fs::remove_dir_all(dir).unwrap();
}
