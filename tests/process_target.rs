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
const PARENT_PID: &str = "HEADROOM_OWN_SERVER_HOST_PARENT_PID";
const TEST: &str = "server_host_skips_every_feature_even_with_unknown_build_enabled";
const DLL: &str = "enshrouded_headroom.dll";
#[test]
fn server_host_skips_every_feature_even_with_unknown_build_enabled() {
    let exact_child_args =
        std::env::args()
            .skip(1)
            .eq(["--exact", TEST, "--nocapture", "--test-threads=1"]
                .into_iter()
                .map(str::to_owned));
    let named_server = std::env::current_exe()
        .ok()
        .and_then(|p| p.file_name().map(|n| n.to_owned()))
        .is_some_and(|n| n.eq_ignore_ascii_case("enshrouded_server.exe"));
    let distinct_parent = std::env::var(PARENT_PID)
        .ok()
        .and_then(|p| p.parse::<u32>().ok())
        .is_some_and(|p| p != 0 && p != std::process::id());
    if let Some(dir) =
        std::env::var_os(CHILD).filter(|_| exact_child_args && named_server && distinct_parent)
    {
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
        .args(["--exact", TEST, "--nocapture", "--test-threads=1"])
        .env(CHILD, &dir)
        .env(PARENT_PID, std::process::id().to_string())
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

/// A stale but usable child directory must not turn an ordinary test process into a native child.
#[test]
fn stale_valid_child_marker_cannot_bypass_server_log_assertion() {
    let exe = std::env::current_exe().unwrap();
    let dir = std::env::temp_dir()
        .join("headroom-stale-server-marker-tests")
        .join(std::process::id().to_string());
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::copy(exe.parent().unwrap().join(DLL), dir.join(DLL)).unwrap();
    std::fs::write(dir.join(INI_FILE_NAME), DEFAULT_INI).unwrap();
    let sentinel = "stale marker must not load this DLL or overwrite this log";
    std::fs::write(dir.join(LOG_FILE_NAME), sentinel).unwrap();
    let output = Command::new(&exe)
        .args(["--exact", TEST, "--nocapture", "--test-threads=1"])
        .env(CHILD, &dir)
        .env(PARENT_PID, std::process::id().to_string())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed"));
    assert_eq!(
        std::fs::read_to_string(dir.join(LOG_FILE_NAME)).unwrap(),
        sentinel,
        "ordinary test process must ignore the stale directory and exercise its own server child"
    );
    let checked = std::env::temp_dir().join("headroom-stale-server-marker-tests");
    assert!(dir.starts_with(checked));
    std::fs::remove_dir_all(dir).unwrap();
}
