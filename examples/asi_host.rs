//! Smoke-test host: loads an `.asi`, calls its `InitializeASI` export, waits 500 ms, exits 0.
//!
//! Usage: `cargo run --release --example asi_host -- target/release/enshrouded-headroom.asi`

use std::os::windows::ffi::OsStrExt;
use std::process::ExitCode;
use std::time::Duration;

use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};

fn main() -> ExitCode {
    let Some(arg) = std::env::args_os().nth(1) else {
        eprintln!("usage: asi_host <path-to.asi>");
        return ExitCode::from(2);
    };
    let path = match std::path::absolute(&arg) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("asi_host: bad path {arg:?}: {e}");
            return ExitCode::from(2);
        }
    };
    let wide: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();

    // SAFETY: `wide` is NUL-terminated and outlives the call.
    let module = unsafe { LoadLibraryW(wide.as_ptr()) };
    if module.is_null() {
        eprintln!(
            "asi_host: LoadLibraryW failed for {}: {}",
            path.display(),
            std::io::Error::last_os_error()
        );
        return ExitCode::from(3);
    }

    // SAFETY: `module` is a live module handle and the name is NUL-terminated.
    let Some(init) = (unsafe { GetProcAddress(module, c"InitializeASI".as_ptr().cast()) }) else {
        eprintln!(
            "asi_host: export InitializeASI not found in {}",
            path.display()
        );
        return ExitCode::from(4);
    };
    // SAFETY: the export is `extern "C" fn()` by contract with Ultimate ASI Loader.
    let init: extern "C" fn() = unsafe { std::mem::transmute(init) };
    init();

    std::thread::sleep(Duration::from_millis(500));
    println!(
        "asi_host: {} loaded and InitializeASI returned",
        path.display()
    );
    ExitCode::SUCCESS
}
