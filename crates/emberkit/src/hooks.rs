//! Startup-only MinHook queue.
use crate::guard::catch;
use ::log::Level;
use minhook::{MinHook, MH_STATUS};
use std::ffi::c_void;
use std::sync::atomic::{AtomicU32, Ordering};
/// Hooks that `create_and_queue` queued and [`apply_queued`] has not applied yet.
static QUEUED: AtomicU32 = AtomicU32::new(0);

/// Create a MinHook hook from `target` to `detour` and queue it to be enabled. Returns the
/// trampoline, the address that runs the original `target`.
///
/// Nothing in `target` changes until [`apply_queued`] runs, so the caller stores the
/// trampoline where its detour reads it before that.
///
/// # Safety
///
/// - `target` is the entry of a function in executable memory. MinHook copies its first
///   instructions (at least 5 bytes) into the trampoline, relocating RIP-relative ones, and writes
///   a jump over them; no code may jump into those bytes.
/// - `detour` is a function with exactly the type and calling convention of `target`.
/// - Start-up only (`entry::start`), on one thread.
pub unsafe fn create_and_queue(target: usize, detour: usize) -> Result<usize, String> {
    let (target_ptr, detour_ptr) = (target as *mut c_void, detour as *mut c_void);
    // SAFETY: the caller's contract. MinHook reads `target` (executable, checked by MinHook
    // itself) and builds the trampoline in its own memory; it writes nothing into `target` yet.
    let created = catch("create the hook", || unsafe {
        MinHook::create_hook(target_ptr, detour_ptr)
    })?;
    let trampoline =
        created.map_err(|status| format!("MinHook cannot hook {target:#x}: {status:?}"))?;
    // SAFETY: `target` has a hook now (created above); queueing only marks it.
    let queued = catch("queue the hook", || unsafe {
        MinHook::queue_enable_hook(target_ptr)
    })?;
    if let Err(status) = queued {
        // Take the unused hook out again. If that fails too, it stays created but disabled, which
        // changes nothing in `target`.
        // SAFETY: `target` has a hook that is not enabled.
        let _ = catch("remove the hook", || unsafe {
            MinHook::remove_hook(target_ptr)
        });
        return Err(format!(
            "MinHook cannot queue the hook at {target:#x}: {status:?}"
        ));
    }
    QUEUED.fetch_add(1, Ordering::AcqRel);
    Ok(trampoline as usize)
}

/// Enable every queued hook at once. Returns how many were queued (0: nothing to do, and MinHook
/// is not even started).
///
/// MinHook suspends the process's other threads while it writes the jumps, and moves a thread
/// whose instruction pointer is inside a patched area. If one hook fails, MinHook stops there.
pub fn apply_queued() -> Result<u32, String> {
    let queued = QUEUED.swap(0, Ordering::AcqRel);
    if queued == 0 {
        return Ok(0);
    }
    // SAFETY: every queued hook was made by `create_and_queue`, whose contract makes its target
    // safe to patch and its detour the right type; each detour's trampoline was stored before
    // this call (the rule in `create_and_queue`).
    match catch("enable the queued hooks", || unsafe {
        MinHook::apply_queued()
    })? {
        Ok(()) => Ok(queued),
        Err(status) => Err(not_all_enabled_text(queued, status)),
    }
}

/// The text for a failed `MH_ApplyQueued`. It enables the hooks in the order they were created and
/// stops at the first failure (MinHook `hook.c`), so the hooks before the failing one are live and
/// the rest are not.
pub fn not_all_enabled_text(queued: u32, status: MH_STATUS) -> String {
    format!(
        "MinHook could not enable all {queued} queued hook(s) ({status:?}); hooks queued before \
         the failing one are live"
    )
}

/// The log line for [`apply_queued`]'s result; `None` when nothing was queued.
pub fn hooks_line(result: &Result<u32, String>) -> Option<(Level, String)> {
    match result {
        Ok(0) => None,
        Ok(n) => Some((Level::Info, format!("hooks: {n} enabled"))),
        Err(why) => Some((Level::Warn, format!("hooks: {why}"))),
    }
}
