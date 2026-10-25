//! enshrouded-headroom: an ASI plugin for Enshrouded.
//!
//! For now the plugin loads, starts its log, writes one line to it, and does nothing else. The
//! panic guard in [`guard`] is ready for the hooks that later tasks add.

pub mod config;
pub mod guard;
pub mod log;
pub mod scan;
pub mod sigs;

use std::ffi::OsString;
use std::os::windows::ffi::OsStringExt;
use std::path::PathBuf;

use windows_sys::core::BOOL;
use windows_sys::Win32::Foundation::{HINSTANCE, HMODULE, TRUE};
use windows_sys::Win32::System::LibraryLoader::{
    DisableThreadLibraryCalls, GetModuleFileNameW, GetModuleHandleExW,
    GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS, GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
};
use windows_sys::Win32::System::SystemServices::DLL_PROCESS_ATTACH;

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

/// Start the log (next to the module, or in `%LOCALAPPDATA%` as a fallback) and write the
/// "loaded" line through it.
///
/// `self::log` is this crate's logging module; the `log` crate is `::log` at the crate root.
fn start() {
    // No module folder (Windows could not tell us): an empty path sends `init` to the fallback.
    let dir = module_dir().unwrap_or_default();
    self::log::init(&dir, ::log::LevelFilter::Info);
    ::log::info!("enshrouded-headroom {} loaded", env!("CARGO_PKG_VERSION"));
}

/// Called by Ultimate ASI Loader once the game process is up. Must never unwind into the loader.
#[no_mangle]
pub extern "C" fn InitializeASI() {
    if let Err(payload) = std::panic::catch_unwind(start) {
        // Never drop the payload: `panic_any` can carry a value whose destructor panics, and a
        // panic here would unwind into the loader. Forgetting leaks a few bytes, once.
        std::mem::forget(payload);
    }
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
