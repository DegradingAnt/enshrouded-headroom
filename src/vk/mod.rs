//! The Vulkan wrapper chain: the mod's way into the game's renderer.
//!
//! The game loads `vulkan-1.dll` with `LoadLibraryW` and asks `GetProcAddress`, through its one
//! KERNEL32 import slot (0x1413b2908 in build f68140a3), for `vkGetInstanceProcAddr`. It gets
//! `vkGetDeviceProcAddr` and `vkCreateDevice` through that, and `vkQueuePresentKHR` through
//! `vkGetDeviceProcAddr`; there is one present call site (RE notes, "Present-hook design", verified
//! from the bytes). So one import-slot swap at start-up puts the mod in front of every step:
//!
//! ```text
//! GetProcAddress slot -> wrap_gpa
//!   "vkGetInstanceProcAddr" on vulkan-1  -> wrap_gipa
//!     "vkGetDeviceProcAddr"               -> wrap_gdpa
//!     "vkCreateDevice"                    -> wrap_create_device
//!     "vkQueuePresentKHR", "vkCreateSwapchainKHR" (defensive, see classify_gipa)
//!   wrap_gdpa
//!     "vkQueuePresentKHR"                 -> wrap_present
//!     "vkCreateSwapchainKHR"              -> wrap_create_swapchain
//! ```
//!
//! No game address is used, so the chain works on any build and is not a code patch.
//!
//! # Rules every wrapper keeps
//!
//! - It is `extern "system"` with the exact signature of the function it stands in for; the
//!   `const` checks below compare each one with ash's `PFN_*` type (and `wrap_gpa` with windows-sys's
//!   `GetProcAddress`) at compile time.
//! - Its body runs inside `guard(Feature::VkWrap, fallback, body)`. The fallback preserves required
//!   lookup/lifetime routing after disable. Native creation/present/destruction run at most once;
//!   nothing after a completed native call can make the
//!   guard call it again: work after the call runs in an inner guard whose fallback does nothing.
//! - It never calls a null real pointer: `wrap_gpa` returns null and the others
//!   `VK_ERROR_INITIALIZATION_FAILED`, and the body logs that once. (The fallback does not log, so
//!   it cannot panic.)
//! - Real pointers are kept first-value-wins (`compare_exchange` from 0): game threads may call the
//!   wrappers at the same time. A wrapper is handed out only when the pointer it will call is right
//!   for every call that can reach it; otherwise the game gets the real pointer back, unwrapped.
//!   See `hand_out` and `device_function`.
//! - `wrap_present` runs every frame, so it touches only atomics: no lock, no allocation, no log.
//!   The one-time wrappers (device creation) may log.

pub mod types;

use std::ffi::{c_char, CStr};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use ash::vk::{self, Handle};
use log::Level;
use windows_sys::core::PCSTR;
use windows_sys::Win32::Foundation::{FARPROC, HMODULE};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;

use crate::config::{Priority, TextureBudget};
use crate::features::priority;
use crate::features::texture_budget::{self, budget_bytes, outcome};
use crate::frame_timer::{qpc_now, FRAMES};
use crate::guard::{guard, Feature};
use crate::iat;
use crate::patch::PatchError;
use crate::scan::Module;

// ---- the statics -------------------------------------------------------------------------------
//
// All are written first-value-wins and only by this module; other modules (and tests) read them.

/// What the exe's `GetProcAddress` import slot held before [`install`] put [`wrap_gpa`] there:
/// kernel32's `GetProcAddress`, or another tool's hook of it, which is then chained to.
pub static REAL_GPA: AtomicUsize = AtomicUsize::new(0);
/// The real `vkGetInstanceProcAddr`, recorded when [`wrap_gpa`] hands out [`wrap_gipa`].
pub static REAL_GIPA: AtomicUsize = AtomicUsize::new(0);
/// The real `vkGetDeviceProcAddr`, recorded when [`wrap_gipa`] hands out [`wrap_gdpa`]. Task 16
/// loads its entry points through it, with [`device_proc`].
pub static REAL_GDPA: AtomicUsize = AtomicUsize::new(0);
/// The real `vkCreateDevice`.
pub static REAL_CREATE_DEVICE: AtomicUsize = AtomicUsize::new(0);
/// The real `vkCreateSwapchainKHR`.
pub static REAL_CREATE_SWAPCHAIN: AtomicUsize = AtomicUsize::new(0);
/// The real `vkQueuePresentKHR`.
pub static REAL_PRESENT: AtomicUsize = AtomicUsize::new(0);
/// Real lifetime endpoints; teardown retires Reflex resources even after Reflex is disabled.
pub static REAL_DESTROY_DEVICE: AtomicUsize = AtomicUsize::new(0);
/// Real swapchain destruction endpoint.
pub static REAL_DESTROY_SWAPCHAIN: AtomicUsize = AtomicUsize::new(0);
/// `vkGetPhysicalDeviceMemoryProperties`, looked up when `vkCreateDevice` is handed out.
pub static REAL_GET_MEMORY_PROPERTIES: AtomicUsize = AtomicUsize::new(0);
/// `vkGetPhysicalDeviceProperties`, looked up when `vkCreateDevice` is handed out.
pub static REAL_GET_PROPERTIES: AtomicUsize = AtomicUsize::new(0);
/// The first non-null `VkInstance` the game passes to `vkGetInstanceProcAddr`.
pub static GAME_INSTANCE: AtomicUsize = AtomicUsize::new(0);
/// Set when [`wrap_gpa`] hands out [`wrap_gipa`]: the game is now inside the chain.
pub static WRAPPER_ENGAGED: AtomicBool = AtomicBool::new(false);

/// The first `VkDevice` the game creates and the `VkPhysicalDevice` it is on, published together:
/// a reader sees both or neither. Read it with [`game_device`].
static GAME_DEVICE: OnceLock<GameDevice> = OnceLock::new();
/// The handle of the first device, taken by `compare_exchange` from 0. Only the thread that takes
/// it sets [`GAME_DEVICE`], so that `set` never waits for another thread and never fails.
static FIRST_DEVICE_TAKEN: AtomicUsize = AtomicUsize::new(0);

/// The game's device: the first one it created, and its physical device.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GameDevice {
    /// The `VkDevice` handle.
    pub device: usize,
    /// The `VkPhysicalDevice` handle it was created on.
    pub phys: usize,
}

/// The game's first device and its physical device, or `None` until it has created one. One
/// atomic load; the two handles always belong together.
pub fn game_device() -> Option<GameDevice> {
    GAME_DEVICE.get().copied()
}

/// Publish `device` on `phys` in `cell` if it is the first device: `taken` is still 0. Returns
/// `true` for that first device only (the caller then does the one-time work); `false` for a null
/// handle and for every later device, which leave `cell` as it is.
fn record_first_device(
    taken: &AtomicUsize,
    cell: &OnceLock<GameDevice>,
    device: usize,
    phys: usize,
) -> bool {
    if device == 0
        || taken
            .compare_exchange(0, device, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
    {
        return false;
    }
    // Only the thread that took `taken` gets here, so the cell is empty and `set` succeeds.
    let _ = cell.set(GameDevice { device, phys });
    true
}

/// What `wrap_create_device` needs from the settings file. Set once, by [`init`].
static TEXTURE_SETTINGS: OnceLock<TextureSettings> = OnceLock::new();
/// The process priority setting, applied once after the first device is created.
static PRIORITY_SETTING: OnceLock<Priority> = OnceLock::new();

/// One flag per wrapper: "a null real pointer has been logged".
static NULL_GPA: AtomicBool = AtomicBool::new(false);
static NULL_GIPA: AtomicBool = AtomicBool::new(false);
static NULL_GDPA: AtomicBool = AtomicBool::new(false);
static NULL_CREATE_DEVICE: AtomicBool = AtomicBool::new(false);
static NULL_CREATE_SWAPCHAIN: AtomicBool = AtomicBool::new(false);
static NULL_PRESENT: AtomicBool = AtomicBool::new(false);

/// The settings `wrap_create_device` uses for the texture budget.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TextureSettings {
    pub mode: TextureBudget,
    pub reserve_gib: f64,
}

// ---- classifying names -------------------------------------------------------------------------

/// Which wrapper a looked-up name gets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wrap {
    Gipa,
    Gdpa,
    CreateDevice,
    CreateSwapchain,
    DestroyDevice,
    DestroySwapchain,
    Present,
}

/// The wrapper for a `GetProcAddress` name (on `vulkan-1.dll`). Pure.
pub fn classify_gpa(name: &[u8]) -> Option<Wrap> {
    match name {
        b"vkGetInstanceProcAddr" => Some(Wrap::Gipa),
        _ => None,
    }
}

/// The wrapper for a `vkGetInstanceProcAddr` name. Pure.
///
/// Besides the chain's own steps (`vkGetDeviceProcAddr`, `vkCreateDevice`, and
/// `vkGetInstanceProcAddr` itself, so a game that re-fetches it stays in the chain), it also maps
/// `vkQueuePresentKHR` and `vkCreateSwapchainKHR`, in case some path fetches them at instance
/// level. The game gets them through `vkGetDeviceProcAddr`.
pub fn classify_gipa(name: &[u8]) -> Option<Wrap> {
    match name {
        b"vkGetInstanceProcAddr" => Some(Wrap::Gipa),
        b"vkGetDeviceProcAddr" => Some(Wrap::Gdpa),
        b"vkCreateDevice" => Some(Wrap::CreateDevice),
        b"vkCreateSwapchainKHR" => Some(Wrap::CreateSwapchain),
        b"vkDestroyDevice" => Some(Wrap::DestroyDevice),
        b"vkDestroySwapchainKHR" => Some(Wrap::DestroySwapchain),
        b"vkQueuePresentKHR" => Some(Wrap::Present),
        _ => None,
    }
}

/// The wrapper for a `vkGetDeviceProcAddr` name. Pure. No submit wrapper (ruling G10).
///
/// `vkGetDeviceProcAddr` itself is mapped so a game that re-fetches it at device level stays in
/// the chain; the loader then gives the top of the device's chain, so it goes through
/// `device_function` like the other two (see `gdpa_choose`).
pub fn classify_gdpa(name: &[u8]) -> Option<Wrap> {
    match name {
        b"vkGetDeviceProcAddr" => Some(Wrap::Gdpa),
        b"vkCreateSwapchainKHR" => Some(Wrap::CreateSwapchain),
        b"vkDestroyDevice" => Some(Wrap::DestroyDevice),
        b"vkDestroySwapchainKHR" => Some(Wrap::DestroySwapchain),
        b"vkQueuePresentKHR" => Some(Wrap::Present),
        _ => None,
    }
}

/// Longest name read for the classifiers, NUL included. The longest name they know is 21 bytes.
const NAME_CAP: usize = 256;

/// A `GetProcAddress` name at or below 0xFFFF is an ordinal, not a pointer (and 0 is null).
fn is_ordinal(name: usize) -> bool {
    name <= 0xFFFF
}

/// Copy the NUL-terminated name at `name` into `buf` and return it without the NUL. `None` for an
/// ordinal or null (`name` <= 0xFFFF; nothing is read) and for a name with no NUL in the first
/// [`NAME_CAP`] bytes (no byte past those is read).
///
/// # Safety
///
/// Unless it is <= 0xFFFF, `name` must point to a NUL-terminated string, or to at least
/// [`NAME_CAP`] readable bytes. A `GetProcAddress` / `vkGet*ProcAddr` caller passes the former.
unsafe fn read_name(name: *const u8, buf: &mut [u8; NAME_CAP]) -> Option<&[u8]> {
    if is_ordinal(name as usize) {
        return None;
    }
    for len in 0..NAME_CAP {
        // SAFETY: bytes 0..len were not NUL, so by the contract byte `len` is still part of the
        // string (or of the NAME_CAP readable bytes); `len` < NAME_CAP.
        let byte = unsafe { name.add(len).read() };
        if byte == 0 {
            return buf.get(..len);
        }
        *buf.get_mut(len)? = byte;
    }
    None
}

// ---- handing out wrappers ----------------------------------------------------------------------

/// Store `value` in `slot` unless it already holds something; return what the slot holds now. 0 is
/// never stored.
fn first_wins(slot: &AtomicUsize, value: usize) -> usize {
    if value == 0 {
        return slot.load(Ordering::Acquire);
    }
    match slot.compare_exchange(0, value, Ordering::AcqRel, Ordering::Acquire) {
        Ok(_) => value,
        Err(held) => held,
    }
}

/// What the game gets for a lookup whose real answer is `real`: `wrapper`, or `real` itself.
///
/// - `real` 0 (the function does not exist) or `real` equal to `wrapper` (already ours): `real`,
///   and nothing is stored, so a wrapper can never be made to call itself.
/// - Otherwise `real` goes into `slot` first-value-wins, and the wrapper, which calls what `slot`
///   holds, is handed out when the slot holds `real` itself, or when `any_held_is_valid` says that
///   whatever it holds is valid for this caller too. Else `real` comes back unwrapped: a later,
///   different pointer (a reloaded `vulkan-1.dll`, another device's driver) must not be routed to
///   the first one.
fn hand_out(slot: &AtomicUsize, real: usize, wrapper: usize, any_held_is_valid: bool) -> usize {
    if real == 0 || real == wrapper {
        return real;
    }
    if first_wins(slot, real) == real || any_held_is_valid {
        wrapper
    } else {
        real
    }
}

/// [`hand_out`] for a device-level function (`vkQueuePresentKHR`, `vkCreateSwapchainKHR`,
/// `vkGetDeviceProcAddr`) looked up with `vkGetDeviceProcAddr` on `device`. `game_device` is the
/// device of [`game_device()`], 0 while there is none.
///
/// - The game's device: wrapped whatever the slot holds. It holds either the loader's trampoline
///   (from a `vkGetInstanceProcAddr` lookup), which serves every device, or this device's own
///   pointer. (For `vkGetDeviceProcAddr` it is always the trampoline: see [`gdpa_choose`].)
/// - Another device, once the game's is known (a second GPU, or a device made again after a device
///   loss): nothing is stored, and it is wrapped only if the slot already holds this very pointer,
///   so the wrapper calls exactly what the game would have. Otherwise the real pointer comes back
///   unwrapped: the slot's pointer may belong to another driver.
/// - No device known yet: wrapped only if the slot ends up holding this very pointer.
fn device_function(
    slot: &AtomicUsize,
    real: usize,
    wrapper: usize,
    device: usize,
    game_device: usize,
) -> usize {
    match game_device {
        0 => hand_out(slot, real, wrapper, false),
        game if game == device => hand_out(slot, real, wrapper, true),
        _ if real != 0 && real != wrapper && slot.load(Ordering::Acquire) == real => wrapper,
        _ => real,
    }
}

// ---- calling the real functions ----------------------------------------------------------------
//
// None of these logs or can panic: they are the guard fallbacks.

/// `GetProcAddress` as the import slot holds it.
type GpaFn = unsafe extern "system" fn(HMODULE, PCSTR) -> FARPROC;

/// The address in a `FARPROC` / `PFN_vkVoidFunction`, 0 for none.
fn addr_of(f: Option<unsafe extern "system" fn() -> isize>) -> usize {
    f.map_or(0, |f| f as usize)
}

/// The address in a `PFN_vkVoidFunction`, 0 for none.
fn void_addr(f: vk::PFN_vkVoidFunction) -> usize {
    f.map_or(0, |f| f as usize)
}

/// `addr` as a `FARPROC`.
fn to_farproc(addr: usize) -> FARPROC {
    // SAFETY: `FARPROC` is `Option` of a function pointer: the size of a `usize`, with 0 as `None`
    // (the guaranteed null-pointer niche). Every `addr` passed here is 0 or a function address
    // that `GetProcAddress` returned or one of this module's wrappers.
    unsafe { std::mem::transmute::<usize, FARPROC>(addr) }
}

/// `addr` as a `PFN_vkVoidFunction`.
fn to_void(addr: usize) -> vk::PFN_vkVoidFunction {
    // SAFETY: as in `to_farproc`: an `Option` of a function pointer, 0 is `None`, and `addr` is 0
    // or a function address from the loader or one of this module's wrappers.
    unsafe { std::mem::transmute::<usize, vk::PFN_vkVoidFunction>(addr) }
}

/// Call the `GetProcAddress` at `real`, or return null if `real` is 0.
///
/// # Safety
///
/// `real` is 0 or a function with `GetProcAddress`'s signature; the arguments are the caller's.
unsafe fn call_gpa(real: usize, module: HMODULE, name: PCSTR) -> FARPROC {
    if real == 0 {
        return None;
    }
    // SAFETY: non-zero, and by the contract a function of this type.
    let f = unsafe { std::mem::transmute::<usize, GpaFn>(real) };
    // SAFETY: the caller's arguments, for the function they were meant for.
    unsafe { f(module, name) }
}

/// Call the `vkGetInstanceProcAddr` at `real`, or return null if `real` is 0.
///
/// # Safety
///
/// `real` is 0 or a `PFN_vkGetInstanceProcAddr`; the arguments are the caller's.
unsafe fn call_gipa(
    real: usize,
    instance: vk::Instance,
    name: *const c_char,
) -> vk::PFN_vkVoidFunction {
    if real == 0 {
        return None;
    }
    // SAFETY: non-zero, and by the contract a function of this type.
    let f = unsafe { std::mem::transmute::<usize, vk::PFN_vkGetInstanceProcAddr>(real) };
    // SAFETY: the caller's arguments, for the function they were meant for.
    unsafe { f(instance, name) }
}

/// Call the `vkGetDeviceProcAddr` at `real`, or return null if `real` is 0.
///
/// # Safety
///
/// `real` is 0 or a `PFN_vkGetDeviceProcAddr`; the arguments are the caller's.
unsafe fn call_gdpa(
    real: usize,
    device: vk::Device,
    name: *const c_char,
) -> vk::PFN_vkVoidFunction {
    if real == 0 {
        return None;
    }
    // SAFETY: non-zero, and by the contract a function of this type.
    let f = unsafe { std::mem::transmute::<usize, vk::PFN_vkGetDeviceProcAddr>(real) };
    // SAFETY: the caller's arguments, for the function they were meant for.
    unsafe { f(device, name) }
}

/// Call the `vkCreateDevice` at `real`, or fail with `VK_ERROR_INITIALIZATION_FAILED` if 0.
///
/// # Safety
///
/// `real` is 0 or a `PFN_vkCreateDevice`; the arguments are the caller's.
unsafe fn call_create_device(
    real: usize,
    phys: vk::PhysicalDevice,
    info: *const vk::DeviceCreateInfo<'_>,
    allocator: *const vk::AllocationCallbacks<'_>,
    device: *mut vk::Device,
) -> vk::Result {
    if real == 0 {
        return vk::Result::ERROR_INITIALIZATION_FAILED;
    }
    // SAFETY: non-zero, and by the contract a function of this type.
    let f = unsafe { std::mem::transmute::<usize, vk::PFN_vkCreateDevice>(real) };
    // SAFETY: the caller's arguments, for the function they were meant for.
    unsafe { f(phys, info, allocator, device) }
}

/// Call the `vkCreateSwapchainKHR` at `real`, or fail with `VK_ERROR_INITIALIZATION_FAILED` if 0.
///
/// # Safety
///
/// `real` is 0 or a `PFN_vkCreateSwapchainKHR`; the arguments are the caller's.
unsafe fn call_create_swapchain(
    real: usize,
    device: vk::Device,
    info: *const vk::SwapchainCreateInfoKHR<'_>,
    allocator: *const vk::AllocationCallbacks<'_>,
    swapchain: *mut vk::SwapchainKHR,
) -> vk::Result {
    if real == 0 {
        return vk::Result::ERROR_INITIALIZATION_FAILED;
    }
    // SAFETY: non-zero, and by the contract a function of this type.
    let f = unsafe { std::mem::transmute::<usize, vk::PFN_vkCreateSwapchainKHR>(real) };
    // SAFETY: the caller's arguments, for the function they were meant for.
    unsafe { f(device, info, allocator, swapchain) }
}

/// Call the `vkQueuePresentKHR` at `real`, or fail with `VK_ERROR_INITIALIZATION_FAILED` if 0.
///
/// # Safety
///
/// `real` is 0 or a `PFN_vkQueuePresentKHR`; the arguments are the caller's.
unsafe fn call_present(
    real: usize,
    queue: vk::Queue,
    info: *const vk::PresentInfoKHR<'_>,
) -> vk::Result {
    if real == 0 {
        return vk::Result::ERROR_INITIALIZATION_FAILED;
    }
    // SAFETY: non-zero, and by the contract a function of this type.
    let f = unsafe { std::mem::transmute::<usize, vk::PFN_vkQueuePresentKHR>(real) };
    // SAFETY: the caller's arguments, for the function they were meant for. The present info is
    // passed on as the game made it, never read or changed here (ruling G12).
    unsafe { f(queue, info) }
}

/// Log, once per `flag`, that a wrapper had no real function to call.
fn warn_null_once(flag: &AtomicBool, what: &str) {
    if !flag.swap(true, Ordering::Relaxed) {
        log::warn!("vk: the real {what} is not known; the call fails instead of calling null");
    }
}

// ---- the wrappers ------------------------------------------------------------------------------

/// Stands in for `GetProcAddress` in the exe's import slot.
///
/// Every call goes to [`REAL_GPA`] with the original arguments. Only a lookup of
/// `vkGetInstanceProcAddr` on `vulkan-1.dll` is changed: it gets [`wrap_gipa`]. The name is read
/// only when `module` is not null and is the handle `GetModuleHandleW("vulkan-1.dll")` gives (which
/// is null while that DLL is not loaded), and never for an ordinal (`name` <= 0xFFFF).
///
/// `GetModuleHandleW` is asked only for a non-null module, a name that is not an ordinal and a
/// lookup that found something. It takes the loader's module-list lock, which the real
/// `GetProcAddress` takes too; `GetProcAddress` is a lookup, never on the per-frame path.
///
/// # Safety
///
/// The contract of `GetProcAddress`: `module` is a module handle (or null) and `name` an ordinal
/// or a NUL-terminated string.
pub unsafe extern "system" fn wrap_gpa(module: HMODULE, name: PCSTR) -> FARPROC {
    let real = REAL_GPA.load(Ordering::Acquire);
    // SAFETY: `real` is 0 or the pointer the import slot held, which has GetProcAddress's
    // signature; the arguments are the game's own, passed on unchanged.
    let original = move || unsafe { call_gpa(real, module, name) };
    let fallback = || {
        let found = original();
        let known = REAL_GIPA.load(Ordering::Acquire);
        if known != 0 && addr_of(found) == known && !is_ordinal(name as usize) {
            let mut buf = [0u8; NAME_CAP];
            // SAFETY: non-ordinal original GetProcAddress name remains readable and terminated.
            if matches!(
                unsafe { read_name(name, &mut buf) },
                Some(b"vkGetInstanceProcAddr")
            ) {
                return to_farproc(wrap_gipa as *const () as usize);
            }
        }
        found
    };
    guard(Feature::VkWrap, fallback, || {
        if real == 0 {
            warn_null_once(&NULL_GPA, "GetProcAddress");
            return None;
        }
        let found = original();
        // SAFETY: a module name literal, NUL-terminated; GetModuleHandleW takes no reference and
        // returns null when the module is not loaded.
        let vulkan = || unsafe { GetModuleHandleW(windows_sys::w!("vulkan-1.dll")) };
        // SAFETY: `name` is the caller's GetProcAddress argument (see this function's contract).
        unsafe { gpa_choose(found, module, name, vulkan) }
    })
}

/// The part of [`wrap_gpa`] after the real call: `found` is what `GetProcAddress` returned, and
/// `vulkan` gives the handle of `vulkan-1.dll` (asked only when it matters).
///
/// # Safety
///
/// `name` is an ordinal or a NUL-terminated string.
unsafe fn gpa_choose(
    found: FARPROC,
    module: HMODULE,
    name: PCSTR,
    vulkan: impl FnOnce() -> HMODULE,
) -> FARPROC {
    let found_addr = addr_of(found);
    if found_addr == 0 || module.is_null() || is_ordinal(name as usize) {
        return found;
    }
    let vulkan = vulkan();
    if vulkan.is_null() || vulkan != module {
        return found;
    }
    let mut buf = [0u8; NAME_CAP];
    // SAFETY: not an ordinal, so by the contract a NUL-terminated string.
    let Some(name) = (unsafe { read_name(name, &mut buf) }) else {
        return found;
    };
    match classify_gpa(name) {
        Some(Wrap::Gipa) => {
            let wrapper = wrap_gipa as *const () as usize;
            let out = hand_out(&REAL_GIPA, found_addr, wrapper, false);
            if out == wrapper {
                WRAPPER_ENGAGED.store(true, Ordering::Release);
            }
            to_farproc(out)
        }
        _ => found,
    }
}

/// Stands in for `vkGetInstanceProcAddr`.
///
/// Every call goes to [`REAL_GIPA`] with the original arguments; the names [`classify_gipa`] knows
/// get their wrapper (see `hand_out`). The first non-null instance is kept in
/// [`GAME_INSTANCE`].
///
/// # Safety
///
/// The contract of `vkGetInstanceProcAddr`.
pub unsafe extern "system" fn wrap_gipa(
    instance: vk::Instance,
    name: *const c_char,
) -> vk::PFN_vkVoidFunction {
    let real = REAL_GIPA.load(Ordering::Acquire);
    // SAFETY: `real` is 0 or the vkGetInstanceProcAddr that GetProcAddress returned; the
    // arguments are the game's own, passed on unchanged.
    let original = move || unsafe { call_gipa(real, instance, name) };
    let fallback = || {
        let found = original();
        // SAFETY: original live GIPA arguments and result; essential lifetime routing persists.
        unsafe { gipa_lifetime_fallback(real, instance, name, found) }
    };
    guard(Feature::VkWrap, fallback, || {
        if real == 0 {
            warn_null_once(&NULL_GIPA, "vkGetInstanceProcAddr");
            return None;
        }
        first_wins(&GAME_INSTANCE, instance.as_raw() as usize);
        let found = original();
        // SAFETY: `real` is the real vkGetInstanceProcAddr, and `instance` and `name` are the
        // game's arguments to it.
        unsafe { gipa_choose(real, instance, name, found) }
    })
}

/// The part of [`wrap_gipa`] after the real call returned `found`.
///
/// # Safety
///
/// `real_gipa` is the real `vkGetInstanceProcAddr`; `instance` and `name` are valid arguments to
/// it (a null `name` is passed through without being read).
unsafe fn gipa_choose(
    real_gipa: usize,
    instance: vk::Instance,
    name: *const c_char,
    found: vk::PFN_vkVoidFunction,
) -> vk::PFN_vkVoidFunction {
    let found_addr = void_addr(found);
    if found_addr == 0 || is_ordinal(name as usize) {
        return found;
    }
    let mut buf = [0u8; NAME_CAP];
    // SAFETY: not null, so by the contract a NUL-terminated string.
    let Some(name) = (unsafe { read_name(name.cast(), &mut buf) }) else {
        return found;
    };
    // SAFETY: original live lookup and result. This only recognizes instance lifetime names.
    let lifetime =
        unsafe { instance_lifetime_choose(real_gipa, instance, name.as_ptr().cast(), found) };
    if void_addr(lifetime) != found_addr {
        return lifetime;
    }
    let out = match classify_gipa(name) {
        None => return found,
        Some(Wrap::Gipa) => hand_out(
            &REAL_GIPA,
            found_addr,
            wrap_gipa as *const () as usize,
            false,
        ),
        Some(Wrap::Gdpa) => hand_out(
            &REAL_GDPA,
            found_addr,
            wrap_gdpa as *const () as usize,
            false,
        ),
        Some(Wrap::CreateDevice) => {
            let wrapper = wrap_create_device as *const () as usize;
            let out = hand_out(&REAL_CREATE_DEVICE, found_addr, wrapper, false);
            if out == wrapper {
                // The instance is live: the game is calling with it right now. These are the
                // instance-compatible functions. These legacy diagnostic slots do not establish
                // parentage: native GPU queries resolve again through the enumerated parent lease.
                // SAFETY: `real_gipa` is the real vkGetInstanceProcAddr and `instance` the
                // game's live instance; the names are NUL-terminated literals.
                let (memory, properties) = unsafe {
                    (
                        call_gipa(
                            real_gipa,
                            instance,
                            c"vkGetPhysicalDeviceMemoryProperties".as_ptr(),
                        ),
                        call_gipa(
                            real_gipa,
                            instance,
                            c"vkGetPhysicalDeviceProperties".as_ptr(),
                        ),
                    )
                };
                first_wins(&REAL_GET_MEMORY_PROPERTIES, void_addr(memory));
                first_wins(&REAL_GET_PROPERTIES, void_addr(properties));
            }
            out
        }
        Some(Wrap::CreateSwapchain) => hand_out(
            &REAL_CREATE_SWAPCHAIN,
            found_addr,
            wrap_create_swapchain as *const () as usize,
            false,
        ),
        Some(Wrap::DestroyDevice) => hand_out(
            &REAL_DESTROY_DEVICE,
            found_addr,
            wrap_destroy_device as *const () as usize,
            false,
        ),
        Some(Wrap::DestroySwapchain) => hand_out(
            &REAL_DESTROY_SWAPCHAIN,
            found_addr,
            wrap_destroy_swapchain as *const () as usize,
            false,
        ),
        Some(Wrap::Present) => hand_out(
            &REAL_PRESENT,
            found_addr,
            wrap_present as *const () as usize,
            false,
        ),
    };
    to_void(out)
}

/// Stands in for `vkGetDeviceProcAddr`.
///
/// Every call goes to [`REAL_GDPA`] with the original arguments; `vkQueuePresentKHR`,
/// `vkCreateSwapchainKHR` and `vkGetDeviceProcAddr` itself get their wrappers (see
/// `gdpa_choose` and `device_function`).
///
/// # Safety
///
/// The contract of `vkGetDeviceProcAddr`.
pub unsafe extern "system" fn wrap_gdpa(
    device: vk::Device,
    name: *const c_char,
) -> vk::PFN_vkVoidFunction {
    let real = REAL_GDPA.load(Ordering::Acquire);
    // SAFETY: `real` is 0 or the real vkGetDeviceProcAddr; the arguments are the game's own,
    // passed on unchanged.
    let original = move || unsafe { call_gdpa(real, device, name) };
    let fallback = || {
        let found = original();
        // SAFETY: original GDPA arguments; only lookup-chain and destruction names are routed.
        if unsafe { lifetime_device_name(name) } {
            // SAFETY: original lookup result and name remain valid here.
            unsafe { gdpa_choose(device, name, found) }
        } else {
            found
        }
    };
    guard(Feature::VkWrap, fallback, || {
        if real == 0 {
            warn_null_once(&NULL_GDPA, "vkGetDeviceProcAddr");
            return None;
        }
        let found = original();
        // SAFETY: `name` is the game's argument to vkGetDeviceProcAddr.
        unsafe { gdpa_choose(device, name, found) }
    })
}

/// Mandatory device lifetime routes, including GDPA itself so a subsequent lookup cannot escape.
/// # Safety
/// The native lookup name contract; null/ordinal inputs are forwarded without reading.
unsafe fn lifetime_device_name(name: *const c_char) -> bool {
    if is_ordinal(name as usize) {
        return false;
    }
    let mut buf = [0u8; NAME_CAP];
    // SAFETY: native lookup name is readable and terminated.
    matches!(
        unsafe { read_name(name.cast(), &mut buf) },
        Some(
            b"vkGetInstanceProcAddr"
                | b"vkGetDeviceProcAddr"
                | b"vkDestroyDevice"
                | b"vkDestroySwapchainKHR"
        )
    )
}
/// Feature disable skips optional work but cannot hand out an escape from established lifetimes.
/// # Safety
/// Original live GIPA arguments and its result.
unsafe fn gipa_lifetime_fallback(
    real: usize,
    instance: vk::Instance,
    name: *const c_char,
    found: vk::PFN_vkVoidFunction,
) -> vk::PFN_vkVoidFunction {
    // SAFETY: original lookup inputs satisfy both bounded classifiers.
    unsafe {
        let lifetime = instance_lifetime_choose(real, instance, name, found);
        if void_addr(lifetime) != void_addr(found) {
            return lifetime;
        }
        if lifetime_device_name(name) {
            gipa_choose(real, instance, name, found)
        } else {
            found
        }
    }
}

/// One flag per device-level name: "an unwrapped hand-out has been logged".
static UNWRAPPED_GDPA: AtomicBool = AtomicBool::new(false);
static UNWRAPPED_PRESENT: AtomicBool = AtomicBool::new(false);
static UNWRAPPED_CREATE_SWAPCHAIN: AtomicBool = AtomicBool::new(false);
/// One-time diagnostics for lifetime wrappers.
static UNWRAPPED_DESTROY_DEVICE: AtomicBool = AtomicBool::new(false);
/// One-time diagnostics for swapchain teardown.
static UNWRAPPED_DESTROY_SWAPCHAIN: AtomicBool = AtomicBool::new(false);

/// `true` for the first call on `flag`, `false` for every later one.
fn first_time(flag: &AtomicBool) -> bool {
    !flag.swap(true, Ordering::Relaxed)
}

/// The warning for a device-level function handed back unwrapped. Without it, a probe device
/// created first would leave the frame timer dead with no clue in the log.
fn unwrapped_line(name: &str, device: usize, game: usize) -> String {
    format!("vk: {name} for device {device:#x} handed back unwrapped: the game device is {game:#x}")
}

/// The part of [`wrap_gdpa`] after the real call returned `found`.
///
/// Ordinary lookup/present/create names retain [`device_function`]. Destruction requires explicit
/// lifetime admission before a different saved endpoint may be used. GDPA may return a distinct
/// top-of-chain address for the same device: an admitted record still needs its retirement wrapper
/// and may safely use the previously validated instance-compatible native endpoint. An unadmitted
/// device has no such proof and receives its own original when the addresses differ.
///
/// A name handed back unwrapped is logged once (one-time lookups, so the log is allowed; neither
/// `hand_out` nor `wrap_present` logs).
///
/// # Safety
///
/// `name` is null or a NUL-terminated string.
unsafe fn gdpa_choose(
    device: vk::Device,
    name: *const c_char,
    found: vk::PFN_vkVoidFunction,
) -> vk::PFN_vkVoidFunction {
    let found_addr = void_addr(found);
    if found_addr == 0 || is_ordinal(name as usize) {
        return found;
    }
    let mut buf = [0u8; NAME_CAP];
    // SAFETY: not null, so by the contract a NUL-terminated string.
    let Some(name) = (unsafe { read_name(name.cast(), &mut buf) }) else {
        return found;
    };
    let (slot, wrapper, logged, what) = match classify_gdpa(name) {
        Some(Wrap::Gdpa) => (
            &REAL_GDPA,
            wrap_gdpa as *const () as usize,
            &UNWRAPPED_GDPA,
            "vkGetDeviceProcAddr",
        ),
        Some(Wrap::Present) => (
            &REAL_PRESENT,
            wrap_present as *const () as usize,
            &UNWRAPPED_PRESENT,
            "vkQueuePresentKHR",
        ),
        Some(Wrap::CreateSwapchain) => (
            &REAL_CREATE_SWAPCHAIN,
            wrap_create_swapchain as *const () as usize,
            &UNWRAPPED_CREATE_SWAPCHAIN,
            "vkCreateSwapchainKHR",
        ),
        Some(Wrap::DestroyDevice) => (
            &REAL_DESTROY_DEVICE,
            wrap_destroy_device as *const () as usize,
            &UNWRAPPED_DESTROY_DEVICE,
            "vkDestroyDevice",
        ),
        Some(Wrap::DestroySwapchain) => (
            &REAL_DESTROY_SWAPCHAIN,
            wrap_destroy_swapchain as *const () as usize,
            &UNWRAPPED_DESTROY_SWAPCHAIN,
            "vkDestroySwapchainKHR",
        ),
        // `classify_gdpa` gives no other variant.
        None | Some(Wrap::Gipa | Wrap::CreateDevice) => return found,
    };
    let device = device.as_raw() as usize;
    let game = game_device().map_or(0, |game| game.device);
    let out = match classify_gdpa(name) {
        // Only admission proves that the saved instance endpoint is compatible with this device.
        // Being the first GAME_DEVICE alone is not proof: optional admission may have failed.
        Some(Wrap::DestroyDevice) => hand_out(
            slot,
            found_addr,
            wrapper,
            DEVICE_QUERIES.protects(device) || crate::reflex::has_device_lifetime(device),
        ),
        Some(Wrap::DestroySwapchain) => hand_out(
            slot,
            found_addr,
            wrapper,
            crate::reflex::has_device_lifetime(device),
        ),
        _ => device_function(slot, found_addr, wrapper, device, game),
    };
    if out != wrapper && first_time(logged) {
        log::warn!("{}", unwrapped_line(what, device, game));
    }
    to_void(out)
}

/// Stands in for `vkCreateDevice`.
///
/// Calls the real one with the game's arguments, unchanged (Task 9 adds no extension, so the
/// game's extension list goes through as it is). On the first device that is created it publishes
/// the device and its physical device together (see [`game_device`]), logs `wrapper chain engaged
/// (device created)`, sizes the texture budget for the card (see `texture_step`), and applies
/// the `Priority` setting (see [`priority::apply`]).
///
/// # Safety
///
/// The contract of `vkCreateDevice`.
pub unsafe extern "system" fn wrap_create_device(
    phys: vk::PhysicalDevice,
    info: *const vk::DeviceCreateInfo<'_>,
    allocator: *const vk::AllocationCallbacks<'_>,
    device: *mut vk::Device,
) -> vk::Result {
    let real = REAL_CREATE_DEVICE.load(Ordering::Acquire);
    // SAFETY: `real` is 0 or the real vkCreateDevice; the arguments are the game's own, passed on
    // unchanged.
    let original = move || unsafe { call_create_device(real, phys, info, allocator, device) };
    guard(Feature::VkWrap, original, || {
        if real == 0 {
            warn_null_once(&NULL_CREATE_DEVICE, "vkCreateDevice");
            return vk::Result::ERROR_INITIALIZATION_FAILED;
        }
        // SAFETY: nonzero real is the saved vkCreateDevice; arguments retain the native contract.
        let result = unsafe {
            crate::reflex::create_device(
                std::mem::transmute::<usize, vk::PFN_vkCreateDevice>(real),
                phys,
                info,
                allocator,
                device,
            )
        };
        // The device exists now. A panic from here on must not make the outer guard create a
        // second one, so the rest runs in its own guard, whose fallback does nothing.
        guard(
            Feature::VkWrap,
            || (),
            // SAFETY: `result` and `device` are what the real vkCreateDevice just returned and
            // wrote; `phys` is the game's physical device.
            || unsafe { after_create_device(result, phys, device) },
        );
        result
    })
}

/// Record the first device and do the one-time work for it.
///
/// # Safety
///
/// `result` is what the real `vkCreateDevice` returned for `phys`, and `device` the output pointer
/// it was given (on `SUCCESS` it holds the new device).
unsafe fn after_create_device(
    result: vk::Result,
    phys: vk::PhysicalDevice,
    device: *mut vk::Device,
) {
    if result != vk::Result::SUCCESS || device.is_null() {
        return;
    }
    // SAFETY: on SUCCESS the driver has written the new handle to `device`, a valid, aligned
    // pointer the game passed in.
    let device = unsafe { device.read() }.as_raw() as usize;
    if !record_first_device(
        &FIRST_DEVICE_TAKEN,
        &GAME_DEVICE,
        device,
        phys.as_raw() as usize,
    ) {
        return;
    }
    if let Some(parent) =
        physical_parent(phys.as_raw() as usize).filter(|_| prepare_device_lifetime(phys, false))
    {
        DEVICE_QUERIES.publish(
            GameDevice {
                device,
                phys: phys.as_raw() as usize,
            },
            parent,
        );
    }
    // SAFETY: `phys` is the physical device the first device was just created on.
    unsafe { on_first_device(phys) };
}

/// The first device's one-time work: log, set the texture budget and apply process priority.
///
/// # Safety
///
/// `phys` is a live physical device.
unsafe fn on_first_device(phys: vk::PhysicalDevice) {
    let gpu = with_physical_device(phys, |gipa, instance| {
        // SAFETY: enumeration proved this parent; its lease protects both dispatch and physical
        // device for the complete synchronous query. Unknown parentage never enters this closure.
        unsafe {
            let instance = vk::Instance::from_raw(instance as u64);
            let memory = void_addr(call_gipa(
                gipa,
                instance,
                c"vkGetPhysicalDeviceMemoryProperties".as_ptr(),
            ));
            let properties = void_addr(call_gipa(
                gipa,
                instance,
                c"vkGetPhysicalDeviceProperties".as_ptr(),
            ));
            query_gpu(memory, properties, phys)
        }
    })
    .flatten();
    log::info!("{}", engaged_line(gpu.as_ref()));
    let step = texture_step(
        texture_budget::prepared(),
        gpu.as_ref(),
        TEXTURE_SETTINGS.get(),
        |prepared, bytes| {
            // A DATA write after start-up, while the game's threads run. The rule in patch.rs
            // allows that for data no thread is using: the game reads this value about 4.6 s after
            // the device exists (texture_budget module docs).
            // SAFETY: `prepared` comes from `texture_budget::prepared()`, which hands out only what
            // `prepare` checked, and this runs right after the first device is created, before the
            // game reads the table.
            unsafe { texture_budget::apply(prepared, bytes) }
        },
    );
    if let Some((level, line)) = step {
        log::log!(level, "{line}");
    }
    if let Some(setting) = PRIORITY_SETTING.get() {
        priority::apply(*setting);
    }
}

/// Stands in for `vkCreateSwapchainKHR`. Task 9 passes every call through unchanged.
///
/// # Safety
///
/// The contract of `vkCreateSwapchainKHR`.
pub unsafe extern "system" fn wrap_create_swapchain(
    device: vk::Device,
    info: *const vk::SwapchainCreateInfoKHR<'_>,
    allocator: *const vk::AllocationCallbacks<'_>,
    swapchain: *mut vk::SwapchainKHR,
) -> vk::Result {
    let real = REAL_CREATE_SWAPCHAIN.load(Ordering::Acquire);
    // SAFETY: `real` is 0 or the real vkCreateSwapchainKHR; the arguments are the game's own,
    // passed on unchanged.
    let original =
        move || unsafe { call_create_swapchain(real, device, info, allocator, swapchain) };
    guard(Feature::VkWrap, original, || {
        if real == 0 {
            warn_null_once(&NULL_CREATE_SWAPCHAIN, "vkCreateSwapchainKHR");
            return vk::Result::ERROR_INITIALIZATION_FAILED;
        }
        // SAFETY: saved native function and unchanged Vulkan caller arguments.
        unsafe {
            crate::reflex::create_swapchain(
                std::mem::transmute::<usize, vk::PFN_vkCreateSwapchainKHR>(real),
                device,
                info,
                allocator,
                swapchain,
            )
        }
    })
}

/// Stands in for `vkQueuePresentKHR`: stamps [`FRAMES`] with [`qpc_now`], then calls the real
/// present with the game's arguments and returns its result.
///
/// It never modifies the `VkPresentInfoKHR` or its `pNext` chain (ruling G12): the game chains its
/// own `VkPresentId2KHR` (sType 1000479001) on every present, and the pointer is passed on as it
/// came. It runs every frame, so it only touches atomics: no lock, no allocation, no log (the
/// null-pointer warning cannot happen once a wrapper was handed out, and is logged once).
///
/// # Safety
///
/// The contract of `vkQueuePresentKHR`.
pub unsafe extern "system" fn wrap_present(
    queue: vk::Queue,
    info: *const vk::PresentInfoKHR<'_>,
) -> vk::Result {
    let real = REAL_PRESENT.load(Ordering::Acquire);
    // SAFETY: `real` is 0 or the real vkQueuePresentKHR; the arguments are the game's own, passed
    // on unchanged.
    let original = move || unsafe { call_present(real, queue, info) };
    guard(Feature::VkWrap, original, || {
        if real == 0 {
            warn_null_once(&NULL_PRESENT, "vkQueuePresentKHR");
            return vk::Result::ERROR_INITIALIZATION_FAILED;
        }
        FRAMES.push(qpc_now());
        original()
    })
}

// Each wrapper has the exact type of the function it stands in for.
const _: GpaFn = wrap_gpa;
const _: GpaFn = windows_sys::Win32::System::LibraryLoader::GetProcAddress;
const _: vk::PFN_vkGetInstanceProcAddr = wrap_gipa;
const _: vk::PFN_vkGetDeviceProcAddr = wrap_gdpa;
const _: vk::PFN_vkCreateDevice = wrap_create_device;
const _: vk::PFN_vkCreateSwapchainKHR = wrap_create_swapchain;
const _: vk::PFN_vkQueuePresentKHR = wrap_present;

/// The real `vkGetDeviceProcAddr`'s answer for `name` on `device`, past every wrapper, or `None` if
/// the device has no such function or the real `vkGetDeviceProcAddr` is not known yet. Task 16
/// loads its Reflex entry points through it.
///
/// # Safety
///
/// `device` is 0 (gives `None`) or a live `VkDevice` of the game (normally the one
/// [`game_device`] gives).
pub unsafe fn device_proc(device: usize, name: &CStr) -> Option<usize> {
    let real = REAL_GDPA.load(Ordering::Acquire);
    if real == 0 || device == 0 {
        return None;
    }
    // SAFETY: `real` is the real vkGetDeviceProcAddr, `device` a live device (contract) and
    // `name` a NUL-terminated string.
    let found = unsafe { call_gdpa(real, vk::Device::from_raw(device as u64), name.as_ptr()) };
    Some(void_addr(found)).filter(|&addr| addr != 0)
}

// ---- the first device: the GPU and the texture budget ------------------------------------------

/// What the mod learns about the GPU the game's device is on. (No `Debug`: ash is built without
/// its `debug` feature, so `PhysicalDeviceType` has none.)
#[derive(Clone)]
struct GpuInfo {
    name: String,
    device_type: vk::PhysicalDeviceType,
    /// The largest heap with `DEVICE_LOCAL`, in bytes.
    device_local: u64,
}

impl GpuInfo {
    fn integrated(&self) -> bool {
        self.device_type == vk::PhysicalDeviceType::INTEGRATED_GPU
    }
}

/// The largest `DEVICE_LOCAL` heap, 0 if none. A heap count above the array size is cut to it.
fn largest_device_local(memory: &vk::PhysicalDeviceMemoryProperties) -> u64 {
    let count = usize::try_from(memory.memory_heap_count).unwrap_or(usize::MAX);
    memory
        .memory_heaps
        .iter()
        .take(count)
        .filter(|heap| heap.flags.contains(vk::MemoryHeapFlags::DEVICE_LOCAL))
        .map(|heap| heap.size)
        .max()
        .unwrap_or(0)
}

/// The device name: the bytes before the first NUL, or all of the array if it has none.
fn device_name(properties: &vk::PhysicalDeviceProperties) -> String {
    let bytes: Vec<u8> = properties
        .device_name
        .iter()
        .take_while(|&&c| c != 0)
        .map(|&c| c as u8)
        .collect();
    String::from_utf8_lossy(&bytes).into_owned()
}

fn gpu_info(
    memory: &vk::PhysicalDeviceMemoryProperties,
    properties: &vk::PhysicalDeviceProperties,
) -> GpuInfo {
    GpuInfo {
        name: device_name(properties),
        device_type: properties.device_type,
        device_local: largest_device_local(memory),
    }
}

/// Ask the driver about `phys`. `None` if either function is not known.
///
/// # Safety
///
/// `get_memory` and `get_properties` are 0 or the loader's `vkGetPhysicalDeviceMemoryProperties` /
/// `vkGetPhysicalDeviceProperties`, and `phys` is a live physical device.
unsafe fn query_gpu(
    get_memory: usize,
    get_properties: usize,
    phys: vk::PhysicalDevice,
) -> Option<GpuInfo> {
    if get_memory == 0 || get_properties == 0 {
        return None;
    }
    // SAFETY: both are the loader's functions with these signatures (contract), non-zero.
    let (get_memory, get_properties) = unsafe {
        (
            std::mem::transmute::<usize, vk::PFN_vkGetPhysicalDeviceMemoryProperties>(get_memory),
            std::mem::transmute::<usize, vk::PFN_vkGetPhysicalDeviceProperties>(get_properties),
        )
    };
    let mut memory = vk::PhysicalDeviceMemoryProperties::default();
    let mut properties = vk::PhysicalDeviceProperties::default();
    // SAFETY: `phys` is live (contract) and both outputs are valid, writable structs of the types
    // the functions fill.
    unsafe {
        get_memory(phys, &mut memory);
        get_properties(phys, &mut properties);
    }
    Some(gpu_info(&memory, &properties))
}

/// `bytes` as GiB with one decimal.
fn gib(bytes: u64) -> String {
    format!("{:.1} GiB", bytes as f64 / (1u64 << 30) as f64)
}

fn type_name(device_type: vk::PhysicalDeviceType) -> &'static str {
    match device_type {
        vk::PhysicalDeviceType::DISCRETE_GPU => "discrete",
        vk::PhysicalDeviceType::INTEGRATED_GPU => "integrated",
        vk::PhysicalDeviceType::VIRTUAL_GPU => "virtual",
        vk::PhysicalDeviceType::CPU => "CPU",
        _ => "other",
    }
}

/// The one log line for the first device.
fn engaged_line(gpu: Option<&GpuInfo>) -> String {
    match gpu {
        Some(gpu) => format!(
            "wrapper chain engaged (device created): GPU \"{}\", {}, largest device-local heap {}",
            gpu.name,
            type_name(gpu.device_type),
            gib(gpu.device_local)
        ),
        None => "wrapper chain engaged (device created); the GPU could not be queried".to_owned(),
    }
}

/// The texture-budget step of the first device: `None` when there is nothing to say (the table
/// was not prepared, and `texture_budget::init` has already logged why), else the one line to log.
/// `apply` is called at most once, and only with a budget from [`budget_bytes`].
fn texture_step<P>(
    prepared: Option<P>,
    gpu: Option<&GpuInfo>,
    settings: Option<&TextureSettings>,
    apply: impl FnOnce(P, u64) -> Result<(), PatchError>,
) -> Option<(Level, String)> {
    let prepared = prepared?;
    let Some(settings) = settings else {
        return Some((
            Level::Warn,
            "texture budget: not applied: the settings were not handed to the Vulkan wrapper"
                .to_owned(),
        ));
    };
    let Some(gpu) = gpu else {
        return Some((
            Level::Warn,
            "texture budget: not applied: the GPU's memory heaps could not be read".to_owned(),
        ));
    };
    let TextureSettings { mode, reserve_gib } = *settings;
    match budget_bytes(gpu.device_local, reserve_gib, gpu.integrated(), &mode) {
        Some(bytes) => {
            let result = apply(prepared, bytes);
            Some(outcome(
                &result,
                bytes,
                gpu.device_local,
                reserve_gib,
                &mode,
            ))
        }
        // No budget: say why, in this order. The setting first (it decides on any GPU), then an
        // integrated GPU, then the reserve.
        None if matches!(mode, TextureBudget::Off) => Some((
            Level::Info,
            "texture budget: off: TextureBudget = off in the settings file".to_owned(),
        )),
        None if gpu.integrated() => Some((
            Level::Info,
            format!(
                "texture budget: unchanged: integrated GPU (device-local {})",
                gib(gpu.device_local)
            ),
        )),
        None => Some((
            Level::Info,
            format!(
                "texture budget: unchanged: device-local {} less the {reserve_gib} GiB reserve is \
                 not above 2 GiB",
                gib(gpu.device_local)
            ),
        )),
    }
}

// ---- installing --------------------------------------------------------------------------------

/// Put [`wrap_gpa`] in the exe's KERNEL32 `GetProcAddress` import slot, keeping the old value in
/// [`REAL_GPA`]. `Err` says why the slot was not changed.
///
/// Start-up only (from `entry::start`): it walks the import table and swaps the slot, see the
/// rules on [`iat::find_slot`] and [`iat::swap`].
pub fn install(m: &Module) -> Result<(), String> {
    // SAFETY: start-up only (above), and `m` is the running exe from `scan::main_module`.
    let slot = unsafe { iat::find_slot(m, "kernel32.dll", "GetProcAddress") }
        .ok_or_else(|| "the exe has no KERNEL32 GetProcAddress import".to_owned())?;
    let swap = |slot: *mut usize, new: usize| {
        // SAFETY: `install_at` passes only the `slot` that `find_slot` returned, and `new` is
        // `wrap_gpa` (GetProcAddress's exact type, checked above) or the value the slot held
        // before; start-up only, so the swap's rules hold.
        unsafe { iat::swap(slot, new) }
    };
    // SAFETY: `slot` comes from `find_slot`, so it is an aligned, readable import slot, and `swap`
    // is `iat::swap`.
    unsafe { install_at(slot, &REAL_GPA, wrap_gpa as *const () as usize, swap) }.map(|_| ())
}

/// The body of [`install`], with the slot, the static and the swap passed in. Returns the old
/// value of the slot.
///
/// The old value goes into `real` BEFORE the swap, so a game thread that calls through the slot
/// the moment it changes finds its real pointer. If the swap does not happen (`swap` returns 0,
/// Task 5's "not hooked"), `real` stays set but nothing calls it.
///
/// # Safety
///
/// `slot` is the address of an import slot, readable as a `usize`. `swap(slot, new)` behaves like
/// [`iat::swap`]: it writes `new` and returns the old value, or returns 0 and changes nothing.
unsafe fn install_at(
    slot: *mut usize,
    real: &AtomicUsize,
    wrapper: usize,
    mut swap: impl FnMut(*mut usize, usize) -> usize,
) -> Result<usize, String> {
    if slot.is_null() || !slot.is_aligned() {
        return Err(format!("the import slot {slot:p} is null or not aligned"));
    }
    // SAFETY: an aligned, readable usize (contract and the check above).
    let old = unsafe { slot.read_volatile() };
    if old == 0 {
        return Err(format!("the import slot {slot:p} holds 0"));
    }
    if old == wrapper {
        return Err("already installed: the slot holds the wrapper".to_owned());
    }
    let held = first_wins(real, old);
    if held != old {
        return Err(format!(
            "already installed once: the real GetProcAddress is kept as {held:#x}"
        ));
    }
    let was = swap(slot, wrapper);
    if was == 0 {
        return Err(format!(
            "not hooked: the import slot {slot:p} was not changed (see the iat warning above)"
        ));
    }
    if was != old {
        // Something changed the slot between the read above and the swap, so `real` holds a
        // value that was not the one replaced. Put back what the swap found and give up.
        let back = swap(slot, was);
        if back != wrapper {
            log::error!(
                "vk: putting {was:#x} back into the import slot {slot:p} failed; it may hold \
                 {wrapper:#x}"
            );
        }
        return Err(format!(
            "the import slot {slot:p} changed during install ({old:#x}, then {was:#x}); not \
             hooked"
        ));
    }
    Ok(old)
}

/// The start-up step from `entry::start`: keep the texture and priority settings for the first
/// device and install the chain. It swaps an import slot, not code, so it runs on unknown builds
/// too. Logs
/// `vk wrapper installed`, or why it is off.
pub fn init(texture: TextureSettings, priority: Priority) {
    // `entry::start` runs once, so the cell is empty; an `Err` could only mean it was set already.
    let _ = TEXTURE_SETTINGS.set(texture);
    let _ = PRIORITY_SETTING.set(priority);
    match install(&crate::scan::main_module()) {
        Ok(()) => log::info!("vk wrapper installed"),
        Err(why) => log::warn!("vk wrapper: off: {why}"),
    }
}

/// Observed parentage and dispatch for one instance generation. Enumeration is the only source
/// of physical-device membership. Retired generations remain invalid even if handles are reused.
struct InstanceContext {
    instance: usize,
    gipa: usize,
    live: AtomicBool,
    readers: AtomicUsize,
    physical: Mutex<Vec<usize>>,
    enumerate: AtomicUsize,
    groups: AtomicUsize,
    groups_khr: AtomicUsize,
    destroy: AtomicUsize,
}
impl InstanceContext {
    /// An observed live lookup does not yet establish any physical-device parentage.
    fn new(instance: usize, gipa: usize) -> Self {
        Self {
            instance,
            gipa,
            live: AtomicBool::new(true),
            readers: AtomicUsize::new(0),
            physical: Mutex::new(Vec::new()),
            enumerate: AtomicUsize::new(0),
            groups: AtomicUsize::new(0),
            groups_khr: AtomicUsize::new(0),
            destroy: AtomicUsize::new(0),
        }
    }
    /// Count before checking liveness, paired with retirement's SeqCst invalidation and drain.
    fn with<T>(&self, query: impl FnOnce(usize, usize) -> T) -> Option<T> {
        self.readers.fetch_add(1, Ordering::SeqCst);
        let _reader = DeviceQueryReader(&self.readers);
        self.live
            .load(Ordering::SeqCst)
            .then(|| query(self.gipa, self.instance))
    }
    /// Record bounded, non-null actual enumeration output only.
    fn observe(&self, physical: impl Iterator<Item = vk::PhysicalDevice>) {
        let mut known = self.physical.lock().unwrap_or_else(|e| e.into_inner());
        for physical in physical {
            let raw = physical.as_raw() as usize;
            if raw != 0 && !known.contains(&raw) && known.len() < 256 {
                known.push(raw);
            }
        }
    }
    /// Reject new queries before draining every admitted query, including unwinding callbacks.
    fn retire(&self) {
        self.live.store(false, Ordering::SeqCst);
        while self.readers.load(Ordering::SeqCst) != 0 {
            std::thread::yield_now();
        }
    }
}
/// Bounded instance generations. This lock is never used on the present path.
static INSTANCES: Mutex<Vec<Arc<InstanceContext>>> = Mutex::new(Vec::new());
/// Find an observed live generation. Null or unknown instances have no inferred parentage.
fn instance_context(instance: usize) -> Option<Arc<InstanceContext>> {
    INSTANCES
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .find(|context| context.instance == instance)
        .cloned()
}
/// Find the unique observed parent. Ambiguous or unobserved physical handles fail closed.
fn physical_parent(phys: usize) -> Option<Arc<InstanceContext>> {
    let instances = INSTANCES.lock().unwrap_or_else(|e| e.into_inner());
    let mut matches = instances.iter().filter(|context| {
        context.live.load(Ordering::SeqCst)
            && context
                .physical
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .contains(&phys)
    });
    let first = matches.next()?.clone();
    if matches.next().is_some() {
        None
    } else {
        Some(first)
    }
}
/// Query only through a parent proven by enumeration. The closure must obey the same synchronous
/// handle-use rules as with_game_device; no unobserved instance is ever dispatched here.
pub(crate) fn with_physical_device<T>(
    phys: vk::PhysicalDevice,
    query: impl FnOnce(usize, usize) -> T,
) -> Option<T> {
    physical_parent(phys.as_raw() as usize)?.with(query)
}
/// Bind required lifetime endpoints to the observed parent before exposing a query/runtime.
/// Existing first-wins pointers may already have been handed to another instance. A conflicting
/// pointer cannot be overwritten safely, so this instance remains ineligible instead.
pub(crate) fn prepare_device_lifetime(phys: vk::PhysicalDevice, reflex: bool) -> bool {
    with_physical_device(phys, |gipa, instance| {
        let bind = |slot: &AtomicUsize, name: &CStr| {
            // SAFETY: the enumeration-derived parent is protected by the surrounding lease.
            let raw = void_addr(unsafe {
                call_gipa(gipa, vk::Instance::from_raw(instance as u64), name.as_ptr())
            });
            raw != 0 && first_wins(slot, raw) == raw
        };
        bind(&REAL_DESTROY_DEVICE, c"vkDestroyDevice")
            && (!reflex
                || (bind(&REAL_DESTROY_SWAPCHAIN, c"vkDestroySwapchainKHR")
                    && bind(&REAL_GDPA, c"vkGetDeviceProcAddr")))
    })
    .unwrap_or(false)
}

/// Keep instance enumeration and retirement in the installed chain, even when ordinary wrapping
/// is disabled. Unknown names pass through. A full registry refuses new observations safely.
/// # Safety
/// The original live GIPA instance/name and its returned function are supplied unchanged.
unsafe fn instance_lifetime_choose(
    real: usize,
    instance: vk::Instance,
    name: *const c_char,
    found: vk::PFN_vkVoidFunction,
) -> vk::PFN_vkVoidFunction {
    if instance == vk::Instance::null() || found.is_none() || is_ordinal(name as usize) {
        return found;
    }
    let mut buf = [0u8; NAME_CAP];
    // SAFETY: the original GIPA name is NUL-terminated by its native contract.
    let Some(name) = (unsafe { read_name(name.cast(), &mut buf) }) else {
        return found;
    };
    let wrapper = match name {
        b"vkEnumeratePhysicalDevices" => wrap_enumerate_physical as *const () as usize,
        b"vkEnumeratePhysicalDeviceGroups" => wrap_enumerate_groups as *const () as usize,
        b"vkEnumeratePhysicalDeviceGroupsKHR" => wrap_enumerate_groups_khr as *const () as usize,
        b"vkDestroyInstance" => wrap_destroy_instance as *const () as usize,
        _ => return found,
    };
    let mut instances = INSTANCES.lock().unwrap_or_else(|e| e.into_inner());
    let context = if let Some(context) = instances
        .iter()
        .find(|c| c.instance == instance.as_raw() as usize)
    {
        context.clone()
    } else {
        if instances.len() >= 64 {
            return found;
        }
        let context = Arc::new(InstanceContext::new(instance.as_raw() as usize, real));
        instances.push(context.clone());
        context
    };
    let slot = match name {
        b"vkEnumeratePhysicalDevices" => &context.enumerate,
        b"vkEnumeratePhysicalDeviceGroups" => &context.groups,
        b"vkEnumeratePhysicalDeviceGroupsKHR" => &context.groups_khr,
        _ => &context.destroy,
    };
    slot.store(void_addr(found), Ordering::Release);
    to_void(wrapper)
}
/// Observe actual outputs without altering the application's enumeration.
/// # Safety
/// The contract of vkEnumeratePhysicalDevices, obtained through the installed GIPA chain.
unsafe extern "system" fn wrap_enumerate_physical(
    instance: vk::Instance,
    count: *mut u32,
    out: *mut vk::PhysicalDevice,
) -> vk::Result {
    let completed = std::cell::Cell::new(None);
    let original = || {
        if let Some(result) = completed.get() {
            return result;
        }
        let Some(context) = instance_context(instance.as_raw() as usize) else {
            return vk::Result::ERROR_INITIALIZATION_FAILED;
        };
        let raw = context.enumerate.load(Ordering::Acquire);
        if raw == 0 {
            return vk::Result::ERROR_INITIALIZATION_FAILED;
        }
        // SAFETY: this entry was resolved for this same live instance; original arguments.
        let result = unsafe {
            std::mem::transmute::<usize, vk::PFN_vkEnumeratePhysicalDevices>(raw)(
                instance, count, out,
            )
        };
        completed.set(Some(result));
        if matches!(result, vk::Result::SUCCESS | vk::Result::INCOMPLETE)
            && !out.is_null()
            && !count.is_null()
        {
            // SAFETY: successful enumeration initializes exactly count output handles.
            let physical = unsafe { std::slice::from_raw_parts(out, count.read() as usize) };
            context.observe(physical.iter().copied());
        }
        result
    };
    guard(Feature::VkWrap, original, original)
}
/// Shared core/KHR group observation, preserving the exact entry point selected by the caller.
/// # Safety
/// The matching vkEnumeratePhysicalDeviceGroups[KHR] contract.
unsafe fn enumerate_groups(
    instance: vk::Instance,
    count: *mut u32,
    out: *mut vk::PhysicalDeviceGroupProperties<'_>,
    khr: bool,
    completed: &std::cell::Cell<Option<vk::Result>>,
) -> vk::Result {
    if let Some(result) = completed.get() {
        return result;
    }
    let Some(context) = instance_context(instance.as_raw() as usize) else {
        return vk::Result::ERROR_INITIALIZATION_FAILED;
    };
    let raw = if khr {
        &context.groups_khr
    } else {
        &context.groups
    }
    .load(Ordering::Acquire);
    if raw == 0 {
        return vk::Result::ERROR_INITIALIZATION_FAILED;
    }
    // SAFETY: matched instance and exact Vulkan core/KHR alias signature.
    let result = unsafe {
        std::mem::transmute::<usize, vk::PFN_vkEnumeratePhysicalDeviceGroups>(raw)(
            instance, count, out,
        )
    };
    completed.set(Some(result));
    if matches!(result, vk::Result::SUCCESS | vk::Result::INCOMPLETE)
        && !out.is_null()
        && !count.is_null()
    {
        // SAFETY: SUCCESS/INCOMPLETE initialize count group records in the caller's storage.
        for group in unsafe { std::slice::from_raw_parts(out, count.read() as usize) } {
            if let Some(physical) = group
                .physical_devices
                .get(..group.physical_device_count as usize)
            {
                context.observe(physical.iter().copied());
            }
        }
    }
    result
}
/// Core group enumeration wrapper.
/// # Safety
/// The contract of vkEnumeratePhysicalDeviceGroups.
unsafe extern "system" fn wrap_enumerate_groups(
    instance: vk::Instance,
    count: *mut u32,
    out: *mut vk::PhysicalDeviceGroupProperties<'_>,
) -> vk::Result {
    let completed = std::cell::Cell::new(None);
    // SAFETY: original group enumeration arguments; completed native calls are never repeated.
    let original = || unsafe { enumerate_groups(instance, count, out, false, &completed) };
    guard(Feature::VkWrap, original, original)
}
/// KHR group enumeration wrapper.
/// # Safety
/// The contract of vkEnumeratePhysicalDeviceGroupsKHR.
unsafe extern "system" fn wrap_enumerate_groups_khr(
    instance: vk::Instance,
    count: *mut u32,
    out: *mut vk::PhysicalDeviceGroupProperties<'_>,
) -> vk::Result {
    let completed = std::cell::Cell::new(None);
    // SAFETY: original group enumeration arguments; completed native calls are never repeated.
    let original = || unsafe { enumerate_groups(instance, count, out, true, &completed) };
    guard(Feature::VkWrap, original, original)
}
/// Retire this generation before native instance destruction. Device teardown normally drained
/// child queries already; the instance drain also protects capability queries before creation.
/// # Safety
/// The contract of vkDestroyInstance, with destruction obtained through the installed chain.
unsafe extern "system" fn wrap_destroy_instance(
    instance: vk::Instance,
    allocator: *const vk::AllocationCallbacks<'_>,
) {
    let finished = std::cell::Cell::new(false);
    let original = || {
        if finished.replace(true) {
            return;
        }
        let context = {
            let mut instances = INSTANCES.lock().unwrap_or_else(|e| e.into_inner());
            let Some(index) = instances
                .iter()
                .position(|c| c.instance == instance.as_raw() as usize)
            else {
                return;
            };
            instances.remove(index)
        };
        context.retire();
        let raw = context.destroy.load(Ordering::Acquire);
        if raw != 0 {
            // SAFETY: saved for this instance generation, still live until this original call.
            unsafe {
                std::mem::transmute::<usize, vk::PFN_vkDestroyInstance>(raw)(instance, allocator)
            };
        }
    };
    guard(Feature::VkWrap, original, original)
}
const _: vk::PFN_vkEnumeratePhysicalDevices = wrap_enumerate_physical;
const _: vk::PFN_vkEnumeratePhysicalDeviceGroups = wrap_enumerate_groups;
const _: vk::PFN_vkEnumeratePhysicalDeviceGroups = wrap_enumerate_groups_khr;
const _: vk::PFN_vkDestroyInstance = wrap_destroy_instance;

/// Frozen lookup context for the first created device, independent of GPU vendor or Reflex mode.
#[derive(Clone)]
struct QueryContext {
    game: GameDevice,
    parent: Arc<InstanceContext>,
}

/// A read-only native-device lease. All cross-atomic lifetime operations use SeqCst ordering.
struct DeviceQueries {
    context: OnceLock<QueryContext>,
    live: AtomicBool,
    readers: AtomicUsize,
}
impl DeviceQueries {
    /// Empty until the first device is published.
    const fn new() -> Self {
        Self {
            context: OnceLock::new(),
            live: AtomicBool::new(false),
            readers: AtomicUsize::new(0),
        }
    }
    /// Publish only an enumerated parent, with its generation retained for future leases.
    fn publish(&self, game: GameDevice, parent: Arc<InstanceContext>) {
        if self.context.set(QueryContext { game, parent }).is_ok() {
            self.live.store(true, Ordering::SeqCst);
        }
    }
    /// Run only while the first device remains live; callback panics still release the lease.
    fn with<T>(&self, query: impl FnOnce(GameDevice, usize, usize) -> T) -> Option<T> {
        self.readers.fetch_add(1, Ordering::SeqCst);
        let _reader = DeviceQueryReader(&self.readers);
        if !self.live.load(Ordering::SeqCst) {
            return None;
        }
        let context = self.context.get()?;
        context
            .parent
            .with(|gipa, instance| query(context.game, gipa, instance))
    }
    /// Only an admitted live context proves the saved device destructor matches this device.
    fn protects(&self, device: usize) -> bool {
        self.live.load(Ordering::SeqCst)
            && self
                .context
                .get()
                .is_some_and(|context| context.game.device == device)
    }
    /// Invalidate before native destruction, then wait outside the present path for existing readers.
    fn retire(&self, device: usize) {
        if !self
            .context
            .get()
            .is_some_and(|context| context.game.device == device)
        {
            return;
        }
        self.live.store(false, Ordering::SeqCst);
        while self.readers.load(Ordering::SeqCst) != 0 {
            std::thread::yield_now();
        }
    }
}
/// A read lease releases even if the Rust query panics.
struct DeviceQueryReader<'a>(&'a AtomicUsize);
impl Drop for DeviceQueryReader<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}
/// First-device query state, valid for every GPU and for Reflex off/unavailable.
static DEVICE_QUERIES: DeviceQueries = DeviceQueries::new();

/// Run a read-only physical-device query under a lifetime lease. The arguments are the first
/// device/physical-device pair, its frozen real GIPA address, and its instance handle.
/// Parentage comes only from intercepted physical-device or group enumeration outputs. Unknown
/// parentage returns None. Returns None before publication and permanently after that first
/// device or its instance generation is retired.
///
/// The callback must not destroy this device (that would wait for its own lease), keep native
/// references past its return, or launch asynchronous queries using these handles. Vulkan requires
/// child devices destroyed before their instance, so draining the device lease also protects its
/// physical device and instance. The game must obey that Vulkan lifetime contract and fetch its
/// destroy entry points through the installed wrapper chain.
pub fn with_game_device<T>(query: impl FnOnce(GameDevice, usize, usize) -> T) -> Option<T> {
    DEVICE_QUERIES.with(query)
}
/// Destroy a device only after retiring its Reflex resources.
/// # Safety
/// The contract of vkDestroyDevice. The game must externally synchronize teardown.
pub unsafe extern "system" fn wrap_destroy_device(
    device: vk::Device,
    allocator: *const vk::AllocationCallbacks<'_>,
) {
    let finished = std::cell::Cell::new(false);
    let original = || {
        if finished.replace(true) {
            return;
        }
        // SAFETY: called before native destruction, including the disabled-feature fallback.
        DEVICE_QUERIES.retire(device.as_raw() as usize);
        unsafe { crate::reflex::before_destroy(device, None) };
        let real = REAL_DESTROY_DEVICE.load(Ordering::Acquire);
        if real != 0 {
            // SAFETY: the saved function matches this signature; original caller arguments.
            unsafe {
                std::mem::transmute::<usize, vk::PFN_vkDestroyDevice>(real)(device, allocator)
            };
        }
    };
    guard(Feature::Reflex, original, original);
}

/// Destroy a swapchain only after its active Reflex record has no readers.
/// # Safety
/// The contract of vkDestroySwapchainKHR. The game must externally synchronize teardown.
pub unsafe extern "system" fn wrap_destroy_swapchain(
    device: vk::Device,
    swapchain: vk::SwapchainKHR,
    allocator: *const vk::AllocationCallbacks<'_>,
) {
    let finished = std::cell::Cell::new(false);
    let original = || {
        if finished.replace(true) {
            return;
        }
        // SAFETY: native swapchain remains live until the call below.
        unsafe { crate::reflex::before_destroy(device, Some(swapchain)) };
        let real = REAL_DESTROY_SWAPCHAIN.load(Ordering::Acquire);
        if real != 0 {
            // SAFETY: the saved function has this signature and receives original arguments.
            unsafe {
                std::mem::transmute::<usize, vk::PFN_vkDestroySwapchainKHR>(real)(
                    device, swapchain, allocator,
                )
            };
        }
    };
    guard(Feature::Reflex, original, original);
}

const _: vk::PFN_vkDestroyDevice = wrap_destroy_device;
const _: vk::PFN_vkDestroySwapchainKHR = wrap_destroy_swapchain;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::patch::test_support::Mem;
    use std::cell::Cell;
    use windows_sys::Win32::System::Memory::PAGE_NOACCESS;

    // ---- the brief's tests (ruling G10: no submit wrappers) -----------------------------------

    #[test]
    fn classify_names() {
        assert!(matches!(
            classify_gpa(b"vkGetInstanceProcAddr"),
            Some(Wrap::Gipa)
        ));
        assert!(classify_gpa(b"vkGetInstanceProcAddrX").is_none());
        assert!(matches!(
            classify_gipa(b"vkCreateDevice"),
            Some(Wrap::CreateDevice)
        ));
        assert!(matches!(
            classify_gdpa(b"vkQueuePresentKHR"),
            Some(Wrap::Present)
        ));
        assert!(classify_gdpa(b"vkQueueSubmit").is_none());
        assert!(classify_gdpa(b"vkQueueSubmit2").is_none());
    }

    #[test]
    fn every_classifier_knows_exactly_its_names() {
        assert_eq!(classify_gpa(b"vkGetDeviceProcAddr"), None);
        assert_eq!(classify_gpa(b"vkQueuePresentKHR"), None);
        assert_eq!(classify_gpa(b""), None);
        assert_eq!(classify_gpa(b"vkgetinstanceprocaddr"), None, "case matters");

        // Defensive: present and swapchain also at instance level.
        assert_eq!(classify_gipa(b"vkGetInstanceProcAddr"), Some(Wrap::Gipa));
        assert_eq!(classify_gipa(b"vkGetDeviceProcAddr"), Some(Wrap::Gdpa));
        assert_eq!(classify_gipa(b"vkQueuePresentKHR"), Some(Wrap::Present));
        assert_eq!(
            classify_gipa(b"vkCreateSwapchainKHR"),
            Some(Wrap::CreateSwapchain)
        );
        assert_eq!(classify_gipa(b"vkCreateInstance"), None);
        assert_eq!(classify_gipa(b"vkQueueSubmit2"), None);

        assert_eq!(classify_gdpa(b"vkGetDeviceProcAddr"), Some(Wrap::Gdpa));
        assert_eq!(
            classify_gdpa(b"vkCreateSwapchainKHR"),
            Some(Wrap::CreateSwapchain)
        );
        assert_eq!(
            classify_gdpa(b"vkCreateDevice"),
            None,
            "not a device-level command"
        );
        assert_eq!(classify_gdpa(b"vkGetInstanceProcAddr"), None);
        assert_eq!(
            classify_gdpa(b"vkQueuePresentKHR\0"),
            None,
            "the NUL is not part of it"
        );
    }

    thread_local! {
        /// Calls of `fake_gpa` on this thread. Each test runs on its own thread and measures a
        /// difference, so the two tests below do not depend on their order (and both set
        /// REAL_GPA to the same function).
        static FAKE_CALLS: Cell<u32> = const { Cell::new(0) };
        static FAKE_LAST: Cell<(usize, usize)> = const { Cell::new((0, 0)) };
    }

    /// What `fake_gpa` "finds": a real, non-null function, so the code after the real call (the
    /// module and ordinal checks in `gpa_choose`) runs on it. Never called.
    ///
    /// # Safety
    ///
    /// None needed: it does nothing.
    unsafe extern "system" fn fake_found() -> isize {
        0
    }

    fn fake_found_addr() -> usize {
        fake_found as *const () as usize
    }

    /// A `GetProcAddress` that only counts and records its arguments and answers `fake_found`.
    ///
    /// # Safety
    ///
    /// None needed: it reads neither argument. It is `unsafe extern "system"` only to have
    /// `GetProcAddress`'s type.
    unsafe extern "system" fn fake_gpa(module: HMODULE, name: PCSTR) -> FARPROC {
        FAKE_CALLS.with(|c| c.set(c.get() + 1));
        FAKE_LAST.with(|c| c.set((module as usize, name as usize)));
        to_farproc(fake_found_addr())
    }

    /// Only the two `wrap_gpa` tests set REAL_GPA, both to `fake_gpa`, and no unit test calls
    /// `install`, so this plain store cannot pull the rug from under another test.
    fn set_real_gpa_for_test(f: usize) {
        REAL_GPA.store(f, Ordering::Release);
    }

    fn kernel32() -> HMODULE {
        // SAFETY: a module name literal; kernel32 is always loaded.
        unsafe { GetModuleHandleW(windows_sys::w!("kernel32.dll")) }
    }

    /// Review focus 2: whatever was in the slot (kernel32, or another tool's hook) is what gets
    /// called, with the caller's arguments.
    #[test]
    fn wrap_gpa_calls_saved_pointer() {
        set_real_gpa_for_test(fake_gpa as *const () as usize);
        let before = FAKE_CALLS.with(Cell::get);
        let name = b"Sleep\0";
        // SAFETY: a live module handle and a NUL-terminated name.
        let got = unsafe { wrap_gpa(kernel32(), name.as_ptr()) };
        assert_eq!(
            addr_of(got),
            fake_found_addr(),
            "the fake's answer comes back"
        );
        assert_eq!(FAKE_CALLS.with(Cell::get) - before, 1);
        assert_eq!(
            FAKE_LAST.with(Cell::get),
            (kernel32() as usize, name.as_ptr() as usize)
        );
    }

    #[test]
    fn wrap_gpa_ordinal_passthrough() {
        set_real_gpa_for_test(fake_gpa as *const () as usize);
        let before = FAKE_CALLS.with(Cell::get);
        // Ordinal 5. Address 5 is in the first 64 KiB, which Windows never maps, so a read of the
        // "name" would fault and end the test.
        let ordinal = 5usize as PCSTR;
        // SAFETY: an ordinal is a valid GetProcAddress name argument.
        let got = unsafe { wrap_gpa(kernel32(), ordinal) };
        // The fake found something, so the checks after the real call ran on it, and it came
        // back as it was.
        assert_eq!(addr_of(got), fake_found_addr());
        assert_eq!(FAKE_CALLS.with(Cell::get) - before, 1);
        assert_eq!(FAKE_LAST.with(Cell::get), (kernel32() as usize, 5));

        // The same answer with the module equal to "vulkan-1": now only the ordinal check stands
        // between the lookup and a name read. It returns before the module is even compared.
        let compared = Cell::new(0);
        let vulkan_is_this_module = || {
            compared.set(compared.get() + 1);
            kernel32()
        };
        // SAFETY: an ordinal, which must not be read (that is what is tested).
        let got = unsafe { gpa_choose(got, kernel32(), ordinal, vulkan_is_this_module) };
        assert_eq!(
            addr_of(got),
            fake_found_addr(),
            "passed through, not wrapped"
        );
        assert_eq!(compared.get(), 0, "no module compare, so no name read");
    }

    // ---- which names are read, and how far -----------------------------------------------------

    /// A stand-in for an address `GetProcAddress` returned.
    fn some_function() -> FARPROC {
        to_farproc(fake_found_addr())
    }

    #[test]
    fn a_null_module_or_an_ordinal_never_gets_its_name_compared() {
        let asked = Cell::new(0);
        let vulkan_null = || {
            asked.set(asked.get() + 1);
            std::ptr::null_mut()
        };
        let name: PCSTR = c"vkGetInstanceProcAddr".as_ptr().cast();
        // A null module while vulkan-1 is not loaded (also null): not equal, not even asked.
        // SAFETY: `name` is NUL-terminated.
        let got = unsafe { gpa_choose(some_function(), std::ptr::null_mut(), name, vulkan_null) };
        assert_eq!(addr_of(got), addr_of(some_function()));
        assert_eq!(asked.get(), 0, "a null module must not be compared");
        // An ordinal: passed through, the module is not asked for and address 7 is not read.
        // SAFETY: an ordinal, which gpa_choose never reads.
        let got = unsafe { gpa_choose(some_function(), kernel32(), 7usize as PCSTR, kernel32) };
        assert_eq!(addr_of(got), addr_of(some_function()));
        // A module that is not vulkan-1: passed through.
        // SAFETY: `name` is NUL-terminated.
        let got = unsafe { gpa_choose(some_function(), kernel32(), name, vulkan_null) };
        assert_eq!(addr_of(got), addr_of(some_function()));
        assert_eq!(asked.get(), 1);
        // A null answer stays null.
        // SAFETY: `name` is NUL-terminated.
        let got = unsafe { gpa_choose(None, kernel32(), name, kernel32) };
        assert!(got.is_none());
    }

    #[test]
    fn names_are_read_up_to_the_nul_and_never_past_the_cap() {
        // Two pages; the second has no access. Names are put at the very end of the first, so a
        // read one byte too far faults and fails the test.
        let mem = Mem::rw(0x2000);
        mem.protect(0x1000, 0x1000, PAGE_NOACCESS);
        let end = 0x1000;
        let mut buf = [0u8; NAME_CAP];

        // A name of NAME_CAP - 1 bytes and its NUL, ending at the page end: read in full.
        let mut long = vec![b'a'; NAME_CAP - 1];
        long.push(0);
        mem.put(end - NAME_CAP, &long);
        // SAFETY: NAME_CAP readable bytes, NUL-terminated.
        let got = unsafe { read_name((mem.base + end - NAME_CAP) as *const u8, &mut buf) };
        assert_eq!(got, Some(&long[..NAME_CAP - 1]));

        // NAME_CAP bytes with no NUL, ending at the page end: not a name, and nothing past them
        // is read.
        mem.put(end - NAME_CAP, &[b'b'; NAME_CAP]);
        // SAFETY: NAME_CAP readable bytes.
        let got = unsafe { read_name((mem.base + end - NAME_CAP) as *const u8, &mut buf) };
        assert_eq!(got, None);

        // A short name.
        mem.put(end - 4, b"abc\0");
        // SAFETY: NUL-terminated.
        let got = unsafe { read_name((mem.base + end - 4) as *const u8, &mut buf) };
        assert_eq!(got, Some(&b"abc"[..]));

        // Null and ordinals: nothing is read.
        // SAFETY: values <= 0xFFFF are never read.
        unsafe {
            assert_eq!(read_name(std::ptr::null(), &mut buf), None);
            assert_eq!(read_name(0xFFFF as *const u8, &mut buf), None);
        }
    }

    // ---- first value wins, and when a wrapper is handed out ------------------------------------

    const WRAPPER: usize = 0x5000_0000;

    #[test]
    fn the_first_real_pointer_wins_and_a_different_one_is_not_wrapped() {
        let slot = AtomicUsize::new(0);
        assert_eq!(hand_out(&slot, 0x1000, WRAPPER, false), WRAPPER);
        assert_eq!(slot.load(Ordering::Relaxed), 0x1000);
        // The same pointer again (a second lookup): still wrapped.
        assert_eq!(hand_out(&slot, 0x1000, WRAPPER, false), WRAPPER);
        // A different pointer (say vulkan-1 reloaded elsewhere): handed back as it is, and the
        // slot keeps the first.
        assert_eq!(hand_out(&slot, 0x2000, WRAPPER, false), 0x2000);
        assert_eq!(slot.load(Ordering::Relaxed), 0x1000);
        // ... unless whatever the slot holds is valid for this caller too.
        assert_eq!(hand_out(&slot, 0x2000, WRAPPER, true), WRAPPER);
        assert_eq!(slot.load(Ordering::Relaxed), 0x1000);
    }

    #[test]
    fn null_and_our_own_wrapper_are_never_stored() {
        let slot = AtomicUsize::new(0);
        assert_eq!(
            hand_out(&slot, 0, WRAPPER, true),
            0,
            "no function: null back"
        );
        assert_eq!(hand_out(&slot, WRAPPER, WRAPPER, true), WRAPPER);
        assert_eq!(
            slot.load(Ordering::Relaxed),
            0,
            "storing the wrapper would make it call itself"
        );
    }

    #[test]
    fn first_value_wins_across_threads() {
        let slot = AtomicUsize::new(0);
        let winners = std::thread::scope(|s| {
            let handles: Vec<_> = (1..=8usize)
                .map(|i| {
                    let slot = &slot;
                    s.spawn(move || hand_out(slot, i * 0x1000, WRAPPER, false) == WRAPPER)
                })
                .collect();
            handles
                .into_iter()
                .map(|h| h.join().unwrap())
                .filter(|&won| won)
                .count()
        });
        assert_eq!(
            winners, 1,
            "exactly one thread's pointer is kept and wrapped"
        );
        assert_ne!(slot.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn device_functions_are_wrapped_for_the_game_device_only() {
        const GAME: usize = 0xD1;
        const OTHER: usize = 0xD2;
        let trampoline = 0x1000;
        let driver = 0x2000;
        let other_driver = 0x3000;

        // The trampoline came first (an instance-level lookup); the game device's own pointer is
        // different but the trampoline serves it, so it is wrapped.
        let slot = AtomicUsize::new(trampoline);
        assert_eq!(device_function(&slot, driver, WRAPPER, GAME, GAME), WRAPPER);
        assert_eq!(slot.load(Ordering::Relaxed), trampoline);

        // Another device, once the game's is known: unwrapped, and nothing stored.
        let slot = AtomicUsize::new(0);
        assert_eq!(
            device_function(&slot, other_driver, WRAPPER, OTHER, GAME),
            other_driver
        );
        assert_eq!(slot.load(Ordering::Relaxed), 0);
        // The game's device afterwards still gets its own pointer stored and wrapped.
        assert_eq!(device_function(&slot, driver, WRAPPER, GAME, GAME), WRAPPER);
        assert_eq!(slot.load(Ordering::Relaxed), driver);
        // Another device (made again after a device loss, same driver) whose pointer is the one
        // the slot holds: the wrapper calls exactly that, so it is wrapped. Nothing changes.
        assert_eq!(
            device_function(&slot, driver, WRAPPER, OTHER, GAME),
            WRAPPER
        );
        assert_eq!(slot.load(Ordering::Relaxed), driver);
        // ... but never our own wrapper, and never a missing function.
        assert_eq!(
            device_function(&slot, WRAPPER, WRAPPER, OTHER, GAME),
            WRAPPER
        );
        assert_eq!(device_function(&slot, 0, WRAPPER, OTHER, GAME), 0);
        assert_eq!(slot.load(Ordering::Relaxed), driver);

        // No device known yet: the first pointer is stored; a different one is not wrapped.
        let slot = AtomicUsize::new(0);
        assert_eq!(device_function(&slot, driver, WRAPPER, GAME, 0), WRAPPER);
        assert_eq!(
            device_function(&slot, other_driver, WRAPPER, OTHER, 0),
            other_driver
        );
        assert_eq!(slot.load(Ordering::Relaxed), driver);

        // A missing function stays missing.
        assert_eq!(
            device_function(&AtomicUsize::new(0), 0, WRAPPER, GAME, GAME),
            0
        );
    }

    #[test]
    fn an_unwrapped_lookup_is_logged_once_per_name() {
        assert_eq!(
            unwrapped_line("vkQueuePresentKHR", 0x3333_1000, 0x3333_0000),
            "vk: vkQueuePresentKHR for device 0x33331000 handed back unwrapped: the game device \
             is 0x33330000"
        );
        let flag = AtomicBool::new(false);
        assert!(
            first_time(&flag),
            "the first unwrapped lookup of a name is logged"
        );
        assert!(!first_time(&flag), "later ones are not");
        assert!(!first_time(&flag));
    }

    // ---- the game's device: both handles or neither ---------------------------------------------

    #[test]
    fn the_first_device_is_published_with_its_physical_device() {
        let taken = AtomicUsize::new(0);
        let cell = OnceLock::new();
        assert!(
            !record_first_device(&taken, &cell, 0, 0x22),
            "a null device is not one"
        );
        assert_eq!(cell.get(), None);
        assert!(record_first_device(&taken, &cell, 0xD1, 0x22));
        let first = GameDevice {
            device: 0xD1,
            phys: 0x22,
        };
        assert_eq!(cell.get(), Some(&first));
        // A second device (same or another GPU) leaves the first in place.
        assert!(!record_first_device(&taken, &cell, 0xD2, 0x23));
        assert_eq!(cell.get(), Some(&first));
    }

    #[test]
    fn one_thread_publishes_the_first_device_and_its_pair_stays_together() {
        let taken = AtomicUsize::new(0);
        let cell = OnceLock::new();
        let winners = std::thread::scope(|s| {
            let handles: Vec<_> = (1..=8usize)
                .map(|i| {
                    let (taken, cell) = (&taken, &cell);
                    s.spawn(move || record_first_device(taken, cell, i * 0x100, i * 0x100 + 1))
                })
                .collect();
            handles
                .into_iter()
                .map(|h| h.join().unwrap())
                .filter(|&won| won)
                .count()
        });
        assert_eq!(winners, 1);
        let game = cell.get().copied().unwrap();
        assert_eq!(
            game.phys,
            game.device + 1,
            "device and phys of the same call"
        );
        assert_eq!(taken.load(Ordering::Relaxed), game.device);
    }

    // ---- a null real pointer is never called ---------------------------------------------------

    #[test]
    fn a_null_real_pointer_is_never_called() {
        let name = c"vkQueuePresentKHR".as_ptr();
        // SAFETY: with `real` 0 nothing is called and no argument is used.
        unsafe {
            assert!(call_gpa(0, kernel32(), name.cast()).is_none());
            assert!(call_gipa(0, vk::Instance::null(), name).is_none());
            assert!(call_gdpa(0, vk::Device::null(), name).is_none());
            let failed = vk::Result::ERROR_INITIALIZATION_FAILED;
            let null_out = std::ptr::null_mut();
            assert_eq!(
                call_create_device(
                    0,
                    vk::PhysicalDevice::null(),
                    std::ptr::null(),
                    std::ptr::null(),
                    null_out
                ),
                failed
            );
            assert_eq!(
                call_create_swapchain(
                    0,
                    vk::Device::null(),
                    std::ptr::null(),
                    std::ptr::null(),
                    std::ptr::null_mut()
                ),
                failed
            );
            assert_eq!(call_present(0, vk::Queue::null(), std::ptr::null()), failed);
            assert_eq!(device_proc(0, c"vkQueuePresentKHR"), None);
        }
    }

    // ---- install ------------------------------------------------------------------------------

    const OLD: usize = 0x7FF0_1234_5670;

    #[test]
    fn install_stores_the_real_pointer_before_the_swap() {
        let mut cell: usize = OLD;
        let slot: *mut usize = &mut cell;
        let real = AtomicUsize::new(0);
        let mut swaps = 0;
        let swap = |s: *mut usize, new: usize| {
            swaps += 1;
            assert_eq!(
                real.load(Ordering::Acquire),
                OLD,
                "the slot must not change before the real pointer is stored"
            );
            // SAFETY: `s` is `slot`, a live usize.
            unsafe { s.replace(new) }
        };
        // SAFETY: `slot` is a live, aligned usize; the fake swap writes it like iat::swap.
        let got = unsafe { install_at(slot, &real, WRAPPER, swap) };
        assert_eq!(got, Ok(OLD));
        assert_eq!(swaps, 1);
        assert_eq!(cell, WRAPPER);
        assert_eq!(real.load(Ordering::Relaxed), OLD);
    }

    #[test]
    fn a_swap_that_does_not_happen_is_an_error() {
        let mut cell: usize = OLD;
        let real = AtomicUsize::new(0);
        // SAFETY: a live usize; the fake swap changes nothing and says so with 0.
        let got = unsafe { install_at(&mut cell, &real, WRAPPER, |_, _| 0) };
        assert!(got.unwrap_err().contains("not hooked"));
        assert_eq!(cell, OLD);
        // REAL_GPA stays set, unused: nothing is in the slot to call it.
        assert_eq!(real.load(Ordering::Relaxed), OLD);
    }

    #[test]
    fn install_refuses_a_zero_slot_a_second_install_and_our_own_wrapper() {
        let real = AtomicUsize::new(0);
        let never = |_: *mut usize, _: usize| -> usize { panic!("must not swap") };

        let mut zero: usize = 0;
        // SAFETY: a live usize; the swap is never called.
        let got = unsafe { install_at(&mut zero, &real, WRAPPER, never) };
        assert!(got.is_err());
        assert_eq!(real.load(Ordering::Relaxed), 0);

        let mut ours: usize = WRAPPER;
        // SAFETY: as above.
        let got = unsafe { install_at(&mut ours, &real, WRAPPER, never) };
        assert!(got.unwrap_err().contains("already"));
        assert_eq!(real.load(Ordering::Relaxed), 0);

        // `real` already holds something else: a second install does not chain twice.
        real.store(0x1111, Ordering::Relaxed);
        let mut cell: usize = OLD;
        // SAFETY: as above.
        let got = unsafe { install_at(&mut cell, &real, WRAPPER, never) };
        assert!(got.unwrap_err().contains("already"));
        assert_eq!(cell, OLD);
    }

    #[test]
    fn a_slot_that_changed_during_install_is_put_back() {
        let mut cell: usize = OLD;
        let slot: *mut usize = &mut cell;
        let real = AtomicUsize::new(0);
        let mut calls = Vec::new();
        // The fake swap reports that the slot held 0x9999 by the time it swapped (someone changed
        // it after install read it), and otherwise writes like iat::swap.
        let swap = |s: *mut usize, new: usize| {
            calls.push(new);
            // SAFETY: `s` is `slot`, a live usize.
            let old = unsafe { s.replace(new) };
            if calls.len() == 1 {
                0x9999
            } else {
                old
            }
        };
        // SAFETY: `slot` is a live, aligned usize.
        let got = unsafe { install_at(slot, &real, WRAPPER, swap) };
        assert!(got.unwrap_err().contains("changed"));
        assert_eq!(
            calls,
            [WRAPPER, 0x9999],
            "the value found by the swap is put back"
        );
        assert_eq!(cell, 0x9999);
    }

    #[test]
    fn a_misaligned_or_null_slot_is_refused() {
        let real = AtomicUsize::new(0);
        let never = |_: *mut usize, _: usize| -> usize { panic!("must not swap") };
        // SAFETY: neither pointer is read: null and misaligned are refused first.
        unsafe {
            assert!(install_at(std::ptr::null_mut(), &real, WRAPPER, never).is_err());
            let misaligned = std::ptr::without_provenance_mut::<usize>(3);
            assert!(install_at(misaligned, &real, WRAPPER, never).is_err());
        }
    }

    // ---- the first device -----------------------------------------------------------------------

    fn heap(size: u64, flags: vk::MemoryHeapFlags) -> vk::MemoryHeap {
        vk::MemoryHeap { size, flags }
    }

    const GIB: u64 = 1 << 30;

    #[test]
    fn the_largest_device_local_heap_is_found_and_the_count_is_bounded() {
        let mut memory = vk::PhysicalDeviceMemoryProperties {
            memory_heap_count: 3,
            ..Default::default()
        };
        memory.memory_heaps[0] = heap(32 * GIB, vk::MemoryHeapFlags::empty());
        memory.memory_heaps[1] = heap(16 * GIB, vk::MemoryHeapFlags::DEVICE_LOCAL);
        memory.memory_heaps[2] = heap(GIB / 4, vk::MemoryHeapFlags::DEVICE_LOCAL);
        assert_eq!(largest_device_local(&memory), 16 * GIB);
        // Heaps past the count do not count.
        memory.memory_heap_count = 1;
        assert_eq!(largest_device_local(&memory), 0);
        // A count past the array (a broken driver) is cut to it instead of panicking.
        memory.memory_heap_count = 99;
        memory.memory_heaps[vk::MAX_MEMORY_HEAPS - 1] =
            heap(24 * GIB, vk::MemoryHeapFlags::DEVICE_LOCAL);
        assert_eq!(largest_device_local(&memory), 24 * GIB);
    }

    #[test]
    fn the_device_name_stops_at_the_nul_or_the_array_end() {
        let mut properties = vk::PhysicalDeviceProperties::default();
        for (dst, &src) in properties.device_name.iter_mut().zip(b"GeForce Test\0junk") {
            *dst = src as c_char;
        }
        assert_eq!(device_name(&properties), "GeForce Test");
        properties.device_name = [b'x' as c_char; vk::MAX_PHYSICAL_DEVICE_NAME_SIZE];
        assert_eq!(
            device_name(&properties).len(),
            vk::MAX_PHYSICAL_DEVICE_NAME_SIZE
        );
    }

    fn gpu(device_local: u64, device_type: vk::PhysicalDeviceType) -> GpuInfo {
        GpuInfo {
            name: "Test GPU".to_owned(),
            device_type,
            device_local,
        }
    }

    const AUTO: TextureSettings = TextureSettings {
        mode: TextureBudget::Auto,
        reserve_gib: 11.0,
    };

    #[test]
    fn the_texture_budget_is_applied_once_for_a_card_with_room() {
        let card = gpu(16 * GIB, vk::PhysicalDeviceType::DISCRETE_GPU);
        let mut applied = Vec::new();
        let step = texture_step(Some(()), Some(&card), Some(&AUTO), |(), bytes| {
            applied.push(bytes);
            Ok(())
        });
        let want = budget_bytes(16 * GIB, 11.0, false, &TextureBudget::Auto).unwrap();
        assert_eq!(applied, [want]);
        assert_eq!(
            step,
            Some(outcome(&Ok(()), want, 16 * GIB, 11.0, &TextureBudget::Auto))
        );
    }

    #[test]
    fn the_texture_budget_reports_what_apply_said() {
        let card = gpu(16 * GIB, vk::PhysicalDeviceType::DISCRETE_GPU);
        let fixed = TextureSettings {
            mode: TextureBudget::Gib(6.0),
            reserve_gib: 11.0,
        };
        let step = texture_step(Some(()), Some(&card), Some(&fixed), |(), _| {
            Err(PatchError::ReadBack)
        });
        assert_eq!(
            step,
            Some(outcome(
                &Err(PatchError::ReadBack),
                6 * GIB,
                16 * GIB,
                11.0,
                &fixed.mode
            ))
        );
    }

    #[test]
    fn no_texture_write_without_a_table_room_or_a_discrete_card() {
        let never = |(): (), _: u64| -> Result<(), PatchError> { panic!("must not apply") };
        let big = gpu(16 * GIB, vk::PhysicalDeviceType::DISCRETE_GPU);
        // No table: `texture_budget::init` has said why; nothing more to say.
        assert_eq!(texture_step(None, Some(&big), Some(&AUTO), never), None);
        // Integrated GPU (review focus 5).
        let laptop = gpu(16 * GIB, vk::PhysicalDeviceType::INTEGRATED_GPU);
        let (level, line) = texture_step(Some(()), Some(&laptop), Some(&AUTO), never).unwrap();
        assert_eq!(level, Level::Info);
        assert!(
            line.contains("unchanged") && line.contains("integrated"),
            "{line}"
        );
        // No room above 2 GiB once the reserve is taken.
        let small = gpu(12 * GIB, vk::PhysicalDeviceType::DISCRETE_GPU);
        let (level, line) = texture_step(Some(()), Some(&small), Some(&AUTO), never).unwrap();
        assert_eq!(level, Level::Info);
        assert!(
            line.contains("unchanged") && line.contains("12.0 GiB"),
            "{line}"
        );
        // The GPU could not be asked, or there are no settings: a warning, no write.
        let (level, line) = texture_step(Some(()), None, Some(&AUTO), never).unwrap();
        assert_eq!(level, Level::Warn);
        assert!(line.contains("not applied"), "{line}");
        let (level, line) = texture_step(Some(()), Some(&big), None, never).unwrap();
        assert_eq!(level, Level::Warn);
        assert!(line.contains("not applied"), "{line}");
    }

    /// The reasons are checked in order: TextureBudget = off first, then an integrated GPU, then
    /// the reserve. An integrated GPU with the feature off is "off", not "integrated".
    #[test]
    fn texture_budget_off_is_named_before_the_gpu_or_the_reserve() {
        let never = |(): (), _: u64| -> Result<(), PatchError> { panic!("must not apply") };
        let off = TextureSettings {
            mode: TextureBudget::Off,
            reserve_gib: 11.0,
        };
        for card in [
            gpu(16 * GIB, vk::PhysicalDeviceType::INTEGRATED_GPU),
            gpu(12 * GIB, vk::PhysicalDeviceType::DISCRETE_GPU),
            gpu(16 * GIB, vk::PhysicalDeviceType::DISCRETE_GPU),
        ] {
            let (level, line) = texture_step(Some(()), Some(&card), Some(&off), never).unwrap();
            assert_eq!(level, Level::Info);
            assert!(
                line.contains("off") && !line.contains("integrated") && !line.contains("reserve"),
                "{line}"
            );
        }
        // An integrated GPU with the feature on names the GPU, not the reserve.
        let laptop = gpu(12 * GIB, vk::PhysicalDeviceType::INTEGRATED_GPU);
        let (_, line) = texture_step(Some(()), Some(&laptop), Some(&AUTO), never).unwrap();
        assert!(
            line.contains("integrated") && !line.contains("reserve"),
            "{line}"
        );
    }

    #[test]
    fn the_engaged_line_names_the_gpu() {
        let card = gpu(16 * GIB, vk::PhysicalDeviceType::DISCRETE_GPU);
        let line = engaged_line(Some(&card));
        assert!(
            line.starts_with("wrapper chain engaged (device created)"),
            "{line}"
        );
        assert!(
            line.contains("\"Test GPU\", discrete") && line.contains("16.0 GiB"),
            "{line}"
        );
        assert!(engaged_line(None).starts_with("wrapper chain engaged (device created)"));
    }

    #[test]
    fn the_gpu_query_needs_both_functions() {
        // SAFETY: with a 0 pointer nothing is called.
        unsafe {
            assert!(query_gpu(0, 1, vk::PhysicalDevice::null()).is_none());
            assert!(query_gpu(1, 0, vk::PhysicalDevice::null()).is_none());
        }
    }
}

#[cfg(test)]
mod device_lease_tests {
    use super::*;
    /// The lease protects AMD/off devices as well, refuses new readers at teardown, and drains the last reader.
    #[test]
    fn device_queries_are_paired_and_teardown_drains_readers() {
        let state = DeviceQueries::new();
        state.publish(
            GameDevice { device: 1, phys: 2 },
            Arc::new(InstanceContext::new(4, 3)),
        );
        assert_eq!(
            state.with(|d, g, i| (d.device, d.phys, g, i)),
            Some((1, 2, 3, 4))
        );
        std::thread::scope(|scope| {
            let (entered_tx, entered_rx) = std::sync::mpsc::channel();
            let (release_tx, release_rx) = std::sync::mpsc::channel();
            let state_ref = &state;
            let worker = scope.spawn(move || {
                state_ref.with(|_, _, _| {
                    entered_tx.send(()).unwrap();
                    release_rx.recv().unwrap();
                })
            });
            entered_rx.recv().unwrap();
            let (done_tx, done_rx) = std::sync::mpsc::channel();
            let state_ref = &state;
            let destroy = scope.spawn(move || {
                state_ref.retire(1);
                done_tx.send(()).unwrap();
            });
            while state.live.load(Ordering::SeqCst) {
                std::thread::yield_now();
            }
            assert!(state
                .with(|_, _, _| panic!("must not query after detach"))
                .is_none());
            assert!(
                done_rx.try_recv().is_err(),
                "teardown cannot finish while the query is live"
            );
            release_tx.send(()).unwrap();
            worker.join().unwrap();
            destroy.join().unwrap();
            done_rx.recv().unwrap();
        });
        state.publish(
            GameDevice { device: 5, phys: 6 },
            Arc::new(InstanceContext::new(8, 7)),
        );
        assert!(
            state.with(|_, _, _| ()).is_none(),
            "the first-device-only context never reactivates"
        );
    }
    /// A Rust query panic releases its lease so teardown cannot hang on a leaked reader count.
    #[test]
    fn device_query_panic_releases_reader() {
        let state = DeviceQueries::new();
        state.publish(
            GameDevice { device: 1, phys: 2 },
            Arc::new(InstanceContext::new(4, 3)),
        );
        assert!(std::panic::catch_unwind(|| state.with(|_, _, _| panic!("query"))).is_err());
        assert_eq!(state.readers.load(Ordering::SeqCst), 0);
        state.retire(1);
    }
}

#[cfg(test)]
mod disabled_lifetime_tests {
    use super::*;
    static TOP_DESTROYS: AtomicUsize = AtomicUsize::new(0);
    static DESTROYS: AtomicUsize = AtomicUsize::new(0);
    /// Fake device destruction records the one native invocation.
    unsafe extern "system" fn destroy_device(_: vk::Device, _: *const vk::AllocationCallbacks<'_>) {
        let body = || {
            DESTROYS.fetch_add(1, Ordering::SeqCst);
        };
        guard(Feature::VkWrap, body, body);
    }
    /// A distinct same-device top-of-chain pointer must not bypass an admitted lease's cleanup.
    unsafe extern "system" fn destroy_device_top(
        _: vk::Device,
        _: *const vk::AllocationCallbacks<'_>,
    ) {
        let body = || {
            TOP_DESTROYS.fetch_add(1, Ordering::SeqCst);
        };
        guard(Feature::VkWrap, body, body);
    }
    /// Fake swapchain destruction records the one native invocation.
    unsafe extern "system" fn destroy_swapchain(
        _: vk::Device,
        _: vk::SwapchainKHR,
        _: *const vk::AllocationCallbacks<'_>,
    ) {
        let body = || {
            DESTROYS.fetch_add(1, Ordering::SeqCst);
        };
        guard(Feature::VkWrap, body, body);
    }
    /// Fake instance destruction records the one native invocation.
    unsafe extern "system" fn destroy_instance(
        _: vk::Instance,
        _: *const vk::AllocationCallbacks<'_>,
    ) {
        let body = || {
            DESTROYS.fetch_add(1, Ordering::SeqCst);
        };
        guard(Feature::VkWrap, body, body);
    }
    /// Resolve fake lifetime functions regardless of the mod's feature mask.
    unsafe fn lookup(name: *const c_char) -> vk::PFN_vkVoidFunction {
        // SAFETY: the tests pass static terminated function names.
        to_void(match unsafe { CStr::from_ptr(name) }.to_bytes() {
            b"vkDestroyInstance" => destroy_instance as *const () as usize,
            b"vkDestroyDevice" => destroy_device as *const () as usize,
            b"vkDestroySwapchainKHR" => destroy_swapchain as *const () as usize,
            b"vkGetInstanceProcAddr" => gipa as *const () as usize,
            b"vkGetDeviceProcAddr" => gdpa as *const () as usize,
            _ => 0,
        })
    }
    /// Fake import resolver returns the already-established Vulkan GIPA function.
    unsafe extern "system" fn gpa(_: HMODULE, _: PCSTR) -> FARPROC {
        let body = || to_farproc(gipa as *const () as usize);
        guard(Feature::VkWrap, body, body)
    }
    /// Fake GIPA has the native lookup contract.
    unsafe extern "system" fn gipa(_: vk::Instance, name: *const c_char) -> vk::PFN_vkVoidFunction {
        // SAFETY: native lookup caller provides a static terminated name.
        let body = || unsafe { lookup(name) };
        guard(Feature::VkWrap, body, body)
    }
    /// Fake GDPA has the native lookup contract.
    unsafe extern "system" fn gdpa(_: vk::Device, name: *const c_char) -> vk::PFN_vkVoidFunction {
        let body = || {
            // SAFETY: native lookup caller provides a static terminated name.
            if unsafe { CStr::from_ptr(name) } == c"vkDestroyDevice" {
                to_void(destroy_device_top as *const () as usize)
            } else {
                // SAFETY: same terminated name; this is the fake device's lookup table.
                unsafe { lookup(name) }
            }
        };
        guard(Feature::VkWrap, body, body)
    }
    /// Isolated real-wrapper regression: disabling ordinary wrapping cannot sever retirement.
    #[test]
    fn disabled_lookup_keeps_teardown_and_drains_device_queries() {
        const TEST: &str =
            "vk::disabled_lifetime_tests::disabled_lookup_keeps_teardown_and_drains_device_queries";
        let Ok(case) = std::env::var("W23_DISABLED_LOOKUP") else {
            for case in [
                "gipa-instance",
                "gipa-device",
                "gdpa-device",
                "gipa-swapchain",
                "gdpa-swapchain",
            ] {
                let output = std::process::Command::new(std::env::current_exe().unwrap())
                    .args(["--exact", TEST, "--nocapture"])
                    .env("W23_DISABLED_LOOKUP", case)
                    .output()
                    .unwrap();
                assert!(
                    output.status.success(),
                    "{case}: {}\n{}",
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                );
            }
            return;
        };
        let game = GameDevice {
            device: 11,
            phys: 22,
        };
        assert!(GAME_DEVICE.set(game).is_ok());
        let parent = Arc::new(InstanceContext::new(33, gipa as *const () as usize));
        parent.observe(std::iter::once(vk::PhysicalDevice::from_raw(22)));
        INSTANCES.lock().unwrap().push(parent.clone());
        DEVICE_QUERIES.publish(game, parent);
        // Admission has validated this instance-compatible endpoint, while GDPA returns a distinct top.
        REAL_DESTROY_DEVICE.store(destroy_device as *const () as usize, Ordering::Release);
        REAL_GPA.store(gpa as *const () as usize, Ordering::Release);
        REAL_GIPA.store(gipa as *const () as usize, Ordering::Release);
        REAL_GDPA.store(gdpa as *const () as usize, Ordering::Release);
        crate::guard::disable(Feature::VkWrap, "test");
        let device = vk::Device::from_raw(11);
        let instance = vk::Instance::from_raw(33);
        // SAFETY: fake live handles, functions and static names obey the native contracts.
        let resolved = unsafe {
            assert_eq!(
                addr_of(wrap_gpa(
                    1usize as HMODULE,
                    c"vkGetInstanceProcAddr".as_ptr().cast()
                )),
                wrap_gipa as *const () as usize
            );
            assert_eq!(
                void_addr(wrap_gipa(instance, c"vkGetInstanceProcAddr".as_ptr())),
                wrap_gipa as *const () as usize
            );
            assert_eq!(
                void_addr(wrap_gipa(instance, c"vkGetDeviceProcAddr".as_ptr())),
                wrap_gdpa as *const () as usize
            );
            assert_eq!(
                void_addr(wrap_gdpa(device, c"vkGetDeviceProcAddr".as_ptr())),
                wrap_gdpa as *const () as usize
            );
            let name = if case.ends_with("device") {
                c"vkDestroyDevice"
            } else {
                c"vkDestroySwapchainKHR"
            };
            if case.starts_with("gipa") {
                wrap_gipa(instance, name.as_ptr())
            } else {
                wrap_gdpa(device, name.as_ptr())
            }
        };
        if case == "gipa-instance" {
            // SAFETY: known live fake instance and exact native destruction signature.
            let raw = unsafe { wrap_gipa(instance, c"vkDestroyInstance".as_ptr()) };
            assert_eq!(void_addr(raw), wrap_destroy_instance as *const () as usize);
            std::thread::scope(|scope| {
                let (entered_tx, entered_rx) = std::sync::mpsc::channel();
                let (release_tx, release_rx) = std::sync::mpsc::channel();
                let reader = scope.spawn(move || {
                    with_physical_device(vk::PhysicalDevice::from_raw(22), |_, i| {
                        assert_eq!(i, 33);
                        entered_tx.send(()).unwrap();
                        release_rx.recv().unwrap();
                    })
                });
                entered_rx.recv().unwrap();
                let destroy =
                    scope.spawn(move || {
                        // SAFETY: no native device was created in this isolated capability-lease case.
                        unsafe {
                            std::mem::transmute::<
                                unsafe extern "system" fn(),
                                vk::PFN_vkDestroyInstance,
                            >(raw.unwrap())(instance, std::ptr::null())
                        };
                    });
                while instance_context(33).is_some() {
                    std::thread::yield_now();
                }
                assert!(
                    with_physical_device(vk::PhysicalDevice::from_raw(22), |_, _| panic!(
                        "retired"
                    ))
                    .is_none()
                );
                assert_eq!(DESTROYS.load(Ordering::SeqCst), 0);
                release_tx.send(()).unwrap();
                reader.join().unwrap();
                destroy.join().unwrap();
            });
            // Same handle, new generation: an old device lease must never borrow its resolver.
            let replacement = Arc::new(InstanceContext::new(33, 999));
            replacement.observe(std::iter::once(vk::PhysicalDevice::from_raw(22)));
            INSTANCES.lock().unwrap().push(replacement);
            assert!(with_game_device(|_, _, _| panic!("old generation")).is_none());
            assert_eq!(DESTROYS.load(Ordering::SeqCst), 1);
            return;
        }
        if case.ends_with("swapchain") {
            assert_eq!(
                void_addr(resolved),
                wrap_destroy_swapchain as *const () as usize
            );
            // SAFETY: lookup resolved the exact fake swapchain destruction signature.
            unsafe {
                std::mem::transmute::<unsafe extern "system" fn(), vk::PFN_vkDestroySwapchainKHR>(
                    resolved.unwrap(),
                )(device, vk::SwapchainKHR::from_raw(44), std::ptr::null())
            };
        } else {
            assert_eq!(
                void_addr(resolved),
                wrap_destroy_device as *const () as usize
            );
            std::thread::scope(|scope| {
                let (entered_tx, entered_rx) = std::sync::mpsc::channel();
                let (release_tx, release_rx) = std::sync::mpsc::channel();
                let reader = scope.spawn(move || {
                    with_game_device(|_, _, _| {
                        entered_tx.send(()).unwrap();
                        release_rx.recv().unwrap();
                    })
                });
                entered_rx.recv().unwrap();
                let destroy = scope.spawn(move || {
                    // SAFETY: this fake device stays live until its one native destructor call.
                    unsafe {
                        std::mem::transmute::<unsafe extern "system" fn(), vk::PFN_vkDestroyDevice>(
                            resolved.unwrap(),
                        )(device, std::ptr::null())
                    };
                });
                while DEVICE_QUERIES.live.load(Ordering::SeqCst) {
                    std::thread::yield_now();
                }
                assert!(with_game_device(|_, _, _| panic!("retired")).is_none());
                assert_eq!(DESTROYS.load(Ordering::SeqCst), 0);
                release_tx.send(()).unwrap();
                reader.join().unwrap();
                destroy.join().unwrap();
            });
        }
        assert_eq!(DESTROYS.load(Ordering::SeqCst), 1);
        assert_eq!(TOP_DESTROYS.load(Ordering::SeqCst), 0);
    }
}
