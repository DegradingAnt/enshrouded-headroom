//! enshrouded-headroom: an ASI plugin for Enshrouded.
//!
//! Task 1 scaffold: the plugin loads, writes one line to its log, and does nothing else.

use std::ffi::OsString;
use std::io::Write;
use std::os::windows::ffi::OsStringExt;
use std::path::PathBuf;

use windows_sys::core::BOOL;
use windows_sys::Win32::Foundation::{HINSTANCE, HMODULE, TRUE};
use windows_sys::Win32::System::LibraryLoader::{
    DisableThreadLibraryCalls, GetModuleFileNameW, GetModuleHandleExW,
    GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS, GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
};
use windows_sys::Win32::System::SystemServices::DLL_PROCESS_ATTACH;

const LOG_FILE_NAME: &str = "enshrouded-headroom.log";

/// Directory that holds this module (the `.asi`), or `None` if Windows cannot tell us.
fn module_dir() -> Option<PathBuf> {
    let mut module: HMODULE = std::ptr::null_mut();
    // SAFETY: the address of a function in this module is a valid FROM_ADDRESS argument, and
    // UNCHANGED_REFCOUNT means no reference is taken that we would have to release.
    let found = unsafe {
        GetModuleHandleExW(
            GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
            module_dir as *const () as *const u16,
            &mut module,
        )
    };
    if found == 0 {
        return None;
    }
    // 32768 UTF-16 units is the longest path Windows allows.
    let mut buf = vec![0u16; 32768];
    // SAFETY: `buf` is valid for `buf.len()` UTF-16 units.
    let len = unsafe { GetModuleFileNameW(module, buf.as_mut_ptr(), buf.len() as u32) } as usize;
    if len == 0 || len >= buf.len() {
        return None;
    }
    let path = PathBuf::from(OsString::from_wide(&buf[..len]));
    path.parent().map(PathBuf::from)
}

/// Create (or truncate) the log next to the module and write the "loaded" line.
/// Task 3 replaces this with the fern logger; failures are ignored on purpose.
fn write_loaded_line() {
    let Some(dir) = module_dir() else { return };
    if let Ok(mut file) = std::fs::File::create(dir.join(LOG_FILE_NAME)) {
        let _ = writeln!(
            file,
            "enshrouded-headroom {} loaded",
            env!("CARGO_PKG_VERSION")
        );
    }
}

/// Called by Ultimate ASI Loader once the game process is up. Must never unwind into the loader.
#[no_mangle]
pub extern "C" fn InitializeASI() {
    let _ = std::panic::catch_unwind(write_loaded_line);
}

/// `DllMain` only turns off thread notifications; all real work waits for `InitializeASI`.
///
/// # Safety
///
/// Called by the Windows loader only, with this DLL's own module handle.
#[no_mangle]
pub unsafe extern "system" fn DllMain(
    module: HINSTANCE,
    reason: u32,
    _reserved: *mut core::ffi::c_void,
) -> BOOL {
    if reason == DLL_PROCESS_ATTACH {
        // SAFETY: `module` is this DLL's own handle, passed in by the loader.
        unsafe { DisableThreadLibraryCalls(module) };
    }
    TRUE
}
