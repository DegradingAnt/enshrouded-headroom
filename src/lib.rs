//! enshrouded-headroom: an ASI plugin for Enshrouded.
//!
//! `InitializeASI` runs [`entry::start`], which reads the settings, starts the log, identifies the
//! game build and decides whether code patches are allowed. The panic guard in [`guard`] is ready
//! for the hooks that later tasks add to `entry::start`.

pub mod antilag;
pub mod build_id;
mod compatibility;
pub mod config;
pub mod display;
pub mod entry;
pub mod features;
/// Pure bounded frame-pacing statistics.
pub mod frame_stats;
pub mod frame_timer;
pub mod governor;
pub mod governor_thread;
pub mod guard;
pub mod iat;
pub mod knobs;
pub mod log;
pub mod patch;
pub mod reflex;
pub mod scan;
pub mod sigs;
pub mod vk;
pub mod vram_budget;

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

/// Full path of the file `module` was loaded from, or `None` if Windows cannot tell us. A null
/// `module` means the exe of this process.
fn module_path(module: HMODULE) -> Option<PathBuf> {
    // 32768 UTF-16 units is the longest path Windows allows.
    let mut buf = vec![0u16; 32768];
    // SAFETY: `buf` is valid for `buf.len()` UTF-16 units. A null `module` is allowed and selects
    // the process exe; any other `module` is a handle that the caller got from Windows.
    let len = unsafe { GetModuleFileNameW(module, buf.as_mut_ptr(), buf.len() as u32) } as usize;
    if len == 0 || len >= buf.len() {
        return None;
    }
    Some(PathBuf::from(OsString::from_wide(buf.get(..len)?)))
}

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
    module_path(module)?.parent().map(PathBuf::from)
}

/// Path of the game's exe (the image of this process), or `None` if Windows cannot tell us.
fn exe_path() -> Option<PathBuf> {
    module_path(std::ptr::null_mut())
}

/// Called by Ultimate ASI Loader once the game process is up. Must never unwind into the loader.
///
/// `entry::start` has no `guard::Feature` of its own (the enum is fixed at the nine features), so
/// [`guard::catch`] is its guard: a panic in the start-up sequence stops it, and the game goes on
/// without the rest of the mod. `guard::catch` never drops the panic payload (see there), and the
/// panic hook has already logged the panic, so the `Err` text is not needed here.
#[no_mangle]
pub extern "C" fn InitializeASI() {
    // Unwind safety: after a panic nothing here uses the state `entry::start` was changing; the
    // later start-up steps simply do not run.
    let _ = guard::catch("start-up", entry::start);
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
