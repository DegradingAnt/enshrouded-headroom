//! AMD Anti-Lag planning, capability gates and one serialized native present stream.

use std::ffi::CStr;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};

use ash::vk;

use crate::config::AntiLagMode;

/// Configuration published before Vulkan startup.
static MODE: std::sync::OnceLock<AntiLagMode> = std::sync::OnceLock::new();

/// Save the parsed Anti-Lag setting before installing Vulkan hooks.
pub fn init(mode: AntiLagMode) {
    let _ = MODE.set(mode);
    log::info!("Anti-Lag: {mode:?}; experimental, not verified on AMD hardware");
}

use crate::vk::types::{
    AntiLagDataAMD, AntiLagModeAMD, AntiLagPresentationInfoAMD, AntiLagStageAMD, AMD_ANTI_LAG_NAME,
    ANTI_LAG_DATA_AMD, ANTI_LAG_PRESENTATION_INFO_AMD,
};

/// The extension and feature-chain edits needed for an eligible device.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DevicePlan {
    /// Add `VK_AMD_anti_lag` if it is not already enabled.
    pub add_extension: bool,
    /// Chain the Anti-Lag feature struct if it is not already present.
    pub chain_feature: bool,
}

/// Plan missing Anti-Lag entries. Device groups with more than one device are skipped.
///
/// Future wiring must query extension and feature support before using this plan.
pub fn plan_device(exts: &[&CStr], feature_in_chain: bool, device_group_count: u32) -> DevicePlan {
    let eligible = device_group_count <= 1;
    DevicePlan {
        add_extension: eligible && !exts.contains(&AMD_ANTI_LAG_NAME),
        chain_feature: eligible && !feature_in_chain,
    }
}

/// Select Anti-Lag only with its extension and feature, and never alongside active Reflex.
///
/// Auto also requires AMD's vendor ID. Explicit `On` accepts any vendor with both capabilities.
pub fn decide_mode(
    cfg: &AntiLagMode,
    vendor_id: u32,
    ext_listed: bool,
    feature_supported: bool,
    reflex_active: bool,
) -> bool {
    if reflex_active || !ext_listed || !feature_supported {
        return false;
    }
    match cfg {
        AntiLagMode::Off => false,
        AntiLagMode::On => true,
        AntiLagMode::Auto => vendor_id == 0x1002,
    }
}

/// An Anti-Lag marker stage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    /// Begin the input interval for the next frame.
    Input,
    /// Mark presentation of the frame whose input interval began earlier.
    Present,
}

/// One update that future wiring must send to the driver.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Call {
    /// The marker to send.
    pub stage: Stage,
    /// The monotonically increasing frame identifier.
    pub frame_index: u64,
}

/// Atomic call planning for one serialized present stream. It starts enabled at frame 1.
///
/// The caller must serialize all methods: `before_present`, the real present, then
/// `after_present`. It must execute each returned call before the next method. Merely returning
/// a call does not send it. The first present has no INPUT marker, so it has no PRESENT marker.
/// INPUT is planned after the preceding present returns; it approximates the game's input stage.
///
/// The atomics protect only this planner's values, not driver calls or multiple present streams.
/// Relaxed ordering suffices because the caller serializes methods and no other data is published.
/// Counter exhaustion disables the planner rather than wrapping or repeating an identifier.
pub struct Pacer {
    /// The current frame, initially 1.
    frame_index: AtomicU64,
    /// Whether the current frame's INPUT was returned and its PRESENT is still due.
    input_pending: AtomicBool,
    /// False after a permanent stop.
    enabled: AtomicBool,
    /// Consecutive updates taking more than 100 ms.
    slow_streak: AtomicU8,
}

impl Pacer {
    /// Start an enabled planner for a new present stream.
    pub const fn new() -> Self {
        Self {
            frame_index: AtomicU64::new(1),
            input_pending: AtomicBool::new(false),
            enabled: AtomicBool::new(true),
            slow_streak: AtomicU8::new(0),
        }
    }

    /// Plan PRESENT only once for a frame with an earlier returned INPUT.
    pub fn before_present(&self) -> Option<Call> {
        if !self.enabled.load(Ordering::Relaxed)
            || !self.input_pending.swap(false, Ordering::Relaxed)
        {
            return None;
        }
        Some(Call {
            stage: Stage::Present,
            frame_index: self.frame_index.load(Ordering::Relaxed),
        })
    }

    /// Advance the frame and plan INPUT, unless disabled or the device was lost.
    ///
    /// Other present results still advance the sequence. Device loss stops it for the session.
    pub fn after_present(&self, result: vk::Result) -> Option<Call> {
        if result == vk::Result::ERROR_DEVICE_LOST {
            self.enabled.store(false, Ordering::Relaxed);
            self.input_pending.store(false, Ordering::Relaxed);
            return None;
        }
        if !self.enabled.load(Ordering::Relaxed) {
            return None;
        }
        let Some(next) = self.frame_index.load(Ordering::Relaxed).checked_add(1) else {
            self.enabled.store(false, Ordering::Relaxed);
            self.input_pending.store(false, Ordering::Relaxed);
            return None;
        };
        self.frame_index.store(next, Ordering::Relaxed);
        self.input_pending.store(true, Ordering::Relaxed);
        Some(Call {
            stage: Stage::Input,
            frame_index: next,
        })
    }

    /// Record a completed driver update's duration, in microseconds.
    ///
    /// Three consecutive updates strictly over 100 ms stop the planner. A faster update resets
    /// the streak. Returns true only for the call that causes this permanent disable transition.
    /// The caller measures time and logs that transition; this method performs no I/O.
    pub fn note_block(&self, micros: u64) -> bool {
        if !self.enabled.load(Ordering::Relaxed) {
            return false;
        }
        if micros <= 100_000 {
            self.slow_streak.store(0, Ordering::Relaxed);
            return false;
        }
        let streak = self.slow_streak.load(Ordering::Relaxed).saturating_add(1);
        self.slow_streak.store(streak, Ordering::Relaxed);
        if streak < 3 {
            return false;
        }
        self.input_pending.store(false, Ordering::Relaxed);
        self.enabled.swap(false, Ordering::Relaxed)
    }
}

impl Default for Pacer {
    /// Start the same stream as `Pacer::new()`.
    fn default() -> Self {
        Self::new()
    }
}

/// Fill one Anti-Lag update without allocating, dereferencing pointers or calling the driver.
///
/// `data.p_presentation_info` borrows the address of `info` as a raw pointer. This function does
/// not extend its lifetime. Future wiring must keep `info` alive at the same address and avoid
/// mutating it until the driver call returns. Moving either value does not repair that pointer.
/// Both extension chains are cleared; the mode is ON and no frame-rate cap is requested.
pub fn fill_data(call: Call, info: &mut AntiLagPresentationInfoAMD, data: &mut AntiLagDataAMD) {
    *info = AntiLagPresentationInfoAMD {
        s_type: ANTI_LAG_PRESENTATION_INFO_AMD,
        p_next: std::ptr::null_mut(),
        stage: match call.stage {
            Stage::Input => AntiLagStageAMD::INPUT,
            Stage::Present => AntiLagStageAMD::PRESENT,
        },
        frame_index: call.frame_index,
    };
    *data = AntiLagDataAMD {
        s_type: ANTI_LAG_DATA_AMD,
        p_next: std::ptr::null(),
        mode: AntiLagModeAMD::ON,
        max_fps: 0,
        p_presentation_info: std::ptr::from_ref(info),
    };
}

use crate::frame_timer::{qpc_now, qpc_to_ms};
use crate::guard::{guard, Feature};
use crate::vk::types::{
    PFN_vkAntiLagUpdateAMD, PhysicalDeviceAntiLagFeaturesAMD, PHYSICAL_DEVICE_ANTI_LAG_FEATURES_AMD,
};
use ash::vk::Handle;
use std::sync::{
    atomic::{AtomicPtr, AtomicUsize},
    OnceLock,
};

/// First eligible AMD device and its actual dispatchable queue handles.
struct Fns {
    device: vk::Device,
    update: PFN_vkAntiLagUpdateAMD,
    queues: Vec<vk::Queue>,
}
/// First admission only; raw handle reuse can never rearm this table.
static FNS: OnceLock<Fns> = OnceLock::new();
/// Set only after mandatory native teardown routing was proven and the table was published.
static DEVICE_LIVE: AtomicBool = AtomicBool::new(false);
/// Quiet stop cause, consumed once at a device/swapchain lifecycle safe point.
static PENDING_STOP: AtomicU8 = AtomicU8::new(0);

/// Whether this exact admitted device generation still has mandatory wrapped destruction.
pub(crate) fn has_device_lifetime(device: usize) -> bool {
    DEVICE_LIVE.load(Ordering::Acquire)
        && FNS
            .get()
            .is_some_and(|f| f.device.as_raw() as usize == device)
}

/// Report a quiet present-path stop once; never call this from presentation.
pub fn report_pending() {
    let reason = PENDING_STOP.swap(0, Ordering::AcqRel);
    if reason == 0 {
        return;
    }
    // A host-supplied logger can unwind. Reporting must never interrupt mandatory native
    // teardown or create a second device through a surrounding guard's fallback.
    let _ = crate::guard::catch("Anti-Lag deferred report", || match reason {
        1 => log::warn!("Anti-Lag disabled: three consecutive updates exceeded 100 ms"),
        2 => log::warn!("Anti-Lag disabled: device lost"),
        3 => log::warn!("Anti-Lag disabled: concurrent present streams"),
        _ => (),
    });
}

/// Resolve listed AMD support through the exact observed instance generation.
/// # Safety
/// The physical device and instance remain live under the caller's query lease.
unsafe fn supported(
    phys: vk::PhysicalDevice,
    raw: usize,
    instance: usize,
    api: Option<crate::vk::InstanceApi>,
) -> bool {
    // SAFETY: the caller's lease protects this real resolver and its exact physical-device parent.
    let properties =
        unsafe { crate::reflex::instance_proc(raw, instance, c"vkGetPhysicalDeviceProperties") };
    let Some(properties) = properties else {
        return false;
    };
    // SAFETY: the name identifies this exact physical-device function signature.
    let properties = unsafe {
        std::mem::transmute::<unsafe extern "system" fn(), vk::PFN_vkGetPhysicalDeviceProperties>(
            properties,
        )
    };
    let mut output = vk::PhysicalDeviceProperties::default();
    // SAFETY: live physical device, initialized matching output.
    unsafe { properties(phys, &mut output) };
    if output.vendor_id != 0x1002 {
        return false;
    }
    let Some(route) = crate::reflex::feature_query_name(output.api_version, api) else {
        return false;
    };
    // SAFETY: the selected alias is proven by API/extension facts; no alias-pointer fallback.
    let functions = unsafe {
        (
            crate::reflex::instance_proc(raw, instance, c"vkEnumerateDeviceExtensionProperties"),
            crate::reflex::instance_proc(raw, instance, route),
        )
    };
    let (Some(enumerate), Some(features)) = functions else {
        return false;
    };
    // SAFETY: static names identify these exact native signatures.
    let enumerate = unsafe {
        std::mem::transmute::<
            unsafe extern "system" fn(),
            vk::PFN_vkEnumerateDeviceExtensionProperties,
        >(enumerate)
    };
    // SAFETY: both legal Features2 aliases share this exact signature.
    let features = unsafe {
        std::mem::transmute::<unsafe extern "system" fn(), vk::PFN_vkGetPhysicalDeviceFeatures2>(
            features,
        )
    };
    let mut count = 0;
    // SAFETY: count-only query on the live leased physical device.
    if unsafe { enumerate(phys, std::ptr::null(), &mut count, std::ptr::null_mut()) }
        != vk::Result::SUCCESS
        || count > 4096
    {
        return false;
    }
    let mut listed = vec![vk::ExtensionProperties::default(); count as usize];
    // SAFETY: initialized array has the previously advertised capacity; INCOMPLETE fails closed.
    if unsafe { enumerate(phys, std::ptr::null(), &mut count, listed.as_mut_ptr()) }
        != vk::Result::SUCCESS
    {
        return false;
    }
    let Some(listed) = listed.get(..count as usize) else {
        return false;
    };
    let extension = listed.iter().any(|entry| {
        let bytes = entry.extension_name.map(|c| c as u8);
        CStr::from_bytes_until_nul(&bytes).is_ok_and(|name| name == AMD_ANTI_LAG_NAME)
    });
    if !extension {
        return false;
    }
    let mut anti = PhysicalDeviceAntiLagFeaturesAMD {
        s_type: PHYSICAL_DEVICE_ANTI_LAG_FEATURES_AMD,
        p_next: std::ptr::null_mut(),
        anti_lag: vk::FALSE,
    };
    let mut query = vk::PhysicalDeviceFeatures2 {
        p_next: std::ptr::from_mut(&mut anti).cast(),
        ..Default::default()
    };
    // SAFETY: only the driver-listed AMD node is queried; all local outputs live through the call.
    unsafe { features(phys, &mut query) };
    MODE.get().is_some_and(|mode| {
        decide_mode(
            mode,
            output.vendor_id,
            extension,
            anti.anti_lag == vk::TRUE,
            false,
        )
    })
}

/// Load actual queues rather than inferring their device from common driver function pointers.
/// # Safety
/// Newly created device and original queue-create array obey their native Vulkan contracts.
unsafe fn load(device: vk::Device, info: &vk::DeviceCreateInfo<'_>) -> Option<Fns> {
    // SAFETY: mandatory GDPA was bound to the enumerated parent before device admission.
    let (update, queue) = unsafe {
        (
            crate::vk::device_proc(device.as_raw() as usize, c"vkAntiLagUpdateAMD")?,
            crate::vk::device_proc(device.as_raw() as usize, c"vkGetDeviceQueue")?,
        )
    };
    // SAFETY: lookups name exactly these native signatures.
    let update = unsafe { std::mem::transmute::<usize, PFN_vkAntiLagUpdateAMD>(update) };
    // SAFETY: the core Vulkan 1.0 queue query has this signature.
    let get_queue = unsafe { std::mem::transmute::<usize, vk::PFN_vkGetDeviceQueue>(queue) };
    if info.queue_create_info_count == 0 || info.queue_create_info_count > 64 {
        return None;
    }
    // SAFETY: valid original VkDeviceCreateInfo contains its advertised live queue-create array.
    let creates = unsafe {
        std::slice::from_raw_parts(
            info.p_queue_create_infos,
            info.queue_create_info_count as usize,
        )
    };
    let mut queues = Vec::new();
    for create in creates {
        // vkGetDeviceQueue is legal only for a queue created with zero flags. Protected queues
        // are left untouched instead of guessing an unproven vkGetDeviceQueue2 route.
        if !create.flags.is_empty() || create.queue_count > 64 {
            return None;
        }
        for index in 0..create.queue_count {
            let mut queue = vk::Queue::null();
            // SAFETY: this family/index was created with zero flags on this exact live device.
            unsafe { get_queue(device, create.queue_family_index, index, &mut queue) };
            if queue == vk::Queue::null() {
                return None;
            }
            queues.push(queue);
        }
    }
    if queues.is_empty() {
        return None;
    }
    // Warm the Windows frequency cache off the present path.
    let _ = qpc_to_ms(1);
    Some(Fns {
        device,
        update,
        queues,
    })
}

/// Add Anti-Lag to eligible AMD devices, otherwise retain Reflex and the exact original call.
/// # Safety
/// The real function and all arguments obey vkCreateDevice's contract.
pub unsafe fn create_device(
    real: vk::PFN_vkCreateDevice,
    phys: vk::PhysicalDevice,
    info: *const vk::DeviceCreateInfo<'_>,
    allocator: *const vk::AllocationCallbacks<'_>,
    out: *mut vk::Device,
) -> vk::Result {
    report_pending();
    let completed = std::cell::Cell::new(None);
    let original = || {
        if let Some(result) = completed.get() {
            return result;
        }
        // SAFETY: unchanged original arguments; Reflex remains the existing fallback path.
        let result = unsafe { crate::reflex::create_device(real, phys, info, allocator, out) };
        completed.set(Some(result));
        result
    };
    guard(Feature::AntiLag, original, || {
        let Some(mode) = MODE.get() else {
            return original();
        };
        if *mode == AntiLagMode::Off || info.is_null() || FNS.get().is_some() {
            return original();
        }
        if !crate::vk::prepare_device_lifetime(phys, true) {
            return original();
        }
        // SAFETY: synchronous query lease proves parentage and retains native handles throughout.
        let eligible =
            crate::vk::with_physical_device_capabilities(phys, |raw, instance, api| unsafe {
                supported(phys, raw, instance, api)
            })
            .unwrap_or(false);
        if !eligible {
            log::info!(
                "Anti-Lag: not listed, off (extension, feature or legal query route unavailable)"
            );
            let result = original();
            if result == vk::Result::SUCCESS && !out.is_null() {
                // SAFETY: successful original creation initialized the caller's output.
                if crate::reflex::has_device_lifetime(unsafe { out.read() }.as_raw() as usize) {
                    log::info!("Anti-Lag: Reflex active on this device, off");
                }
            }
            return result;
        }
        // SAFETY: the input chain is live and valid for this synchronous native call.
        let Some(mut copied) = (unsafe { crate::reflex::DeviceCopy::anti_lag(&*info) }) else {
            return original();
        };
        // SAFETY: original inputs remain live while the owned typed copy is linked.
        let changed = copied.info(unsafe { &*info });
        // SAFETY: original and changed inputs satisfy the shared synchronous creation contract.
        let (result, enabled) = unsafe {
            crate::reflex::create_or_retry(
                Feature::AntiLag,
                real,
                phys,
                info,
                &changed,
                allocator,
                out,
            )
        };
        completed.set(Some(result));
        if !enabled {
            return result;
        }
        guard(
            Feature::AntiLag,
            || (),
            || {
                // SAFETY: native success initializes the device output and original queue info is live.
                let device = unsafe { out.read() };
                if crate::reflex::has_device_lifetime(device.as_raw() as usize) {
                    log::info!("Anti-Lag: Reflex active on this device, off");
                    return;
                }
                // SAFETY: this newly created device owns exactly the original queues.
                if let Some(fns) = unsafe { load(device, &*info) } {
                    if FNS.set(fns).is_ok() {
                        DEVICE_LIVE.store(true, Ordering::Release);
                        log::info!("Anti-Lag: on; experimental, not verified on AMD hardware");
                    }
                } else {
                    crate::guard::disable(
                        Feature::AntiLag,
                        "missing device entry point or unsupported queues",
                    );
                }
            },
        );
        result
    })
}

/// Immutable association and serialized planner for one observed live swapchain.
struct Runtime {
    swapchain: vk::SwapchainKHR,
    pacer: Pacer,
    presenting: AtomicBool,
    primed: AtomicBool,
}
/// Published runtime pointer, detached before waiting for existing readers.
static ACTIVE: AtomicPtr<Runtime> = AtomicPtr::new(std::ptr::null_mut());
/// SeqCst ordering with ACTIVE prevents reclamation while a reader can reach the old pointer.
static READERS: AtomicUsize = AtomicUsize::new(0);
/// Lifecycle work only; presentation never locks this mutex.
static LIFECYCLE: std::sync::Mutex<()> = std::sync::Mutex::new(());
/// Keeps the current immutable runtime alive through one present.
struct Reader;
impl Reader {
    /// Enter before reading the published pointer.
    fn enter() -> Self {
        READERS.fetch_add(1, Ordering::SeqCst);
        Self
    }
    /// Borrow at most for this reader's lifetime.
    fn runtime(&self) -> Option<&Runtime> {
        // SAFETY: writers detach first and wait for this reader count before reclaiming.
        unsafe { ACTIVE.load(Ordering::SeqCst).as_ref() }
    }
}
impl Drop for Reader {
    fn drop(&mut self) {
        READERS.fetch_sub(1, Ordering::SeqCst);
    }
}
/// Release exclusive nonblocking present admission on every return or unwind.
struct Admission<'a>(&'a AtomicBool);
impl Drop for Admission<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}
/// Replace a runtime only while LIFECYCLE is held; wait outside the game present path.
fn replace(new: *mut Runtime) {
    let old = ACTIVE.swap(new, Ordering::SeqCst);
    if old.is_null() {
        return;
    }
    while READERS.load(Ordering::SeqCst) != 0 {
        std::thread::yield_now();
    }
    // SAFETY: old was allocated once with Box and no reader can still reach it.
    drop(unsafe { Box::from_raw(old) });
}
/// Preserve one quiet disable transition for the next lifecycle safe point.
fn stop(reason: u8) {
    if crate::guard::disable_silent(Feature::AntiLag) {
        let _ = PENDING_STOP.compare_exchange(0, reason, Ordering::AcqRel, Ordering::Relaxed);
    }
}
impl Runtime {
    /// Fill and time one update. No allocation, lock or logging occurs here.
    fn update(&self, fns: &Fns, call: Call) {
        if crate::guard::is_disabled(Feature::AntiLag) {
            return;
        }
        let mut info = AntiLagPresentationInfoAMD {
            s_type: ANTI_LAG_PRESENTATION_INFO_AMD,
            p_next: std::ptr::null_mut(),
            stage: AntiLagStageAMD::INPUT,
            frame_index: 0,
        };
        let mut data = AntiLagDataAMD {
            s_type: ANTI_LAG_DATA_AMD,
            p_next: std::ptr::null(),
            mode: AntiLagModeAMD::ON,
            max_fps: 0,
            p_presentation_info: std::ptr::null(),
        };
        fill_data(call, &mut info, &mut data);
        let start = qpc_now();
        // SAFETY: this reader retains the exact live admitted device; local data lives through return.
        unsafe { (fns.update)(fns.device, &data) };
        let elapsed = qpc_to_ms(qpc_now().saturating_sub(start)) * 1000.0;
        if self.pacer.note_block(elapsed as u64) {
            stop(1);
        }
    }
    /// Serialize INPUT/PRESENT around exactly one native presentation without waiting.
    fn present(&self, fns: &Fns, original: impl Fn() -> vk::Result) -> vk::Result {
        if self
            .presenting
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            stop(3);
            return original();
        }
        let _admission = Admission(&self.presenting);
        if crate::guard::is_disabled(Feature::AntiLag) {
            return original();
        }
        if !self.primed.swap(true, Ordering::Relaxed) {
            if let Some(call) = self.pacer.after_present(vk::Result::SUCCESS) {
                self.update(fns, call);
            }
        }
        if let Some(call) = self.pacer.before_present() {
            self.update(fns, call);
        }
        let result = original();
        if result == vk::Result::ERROR_DEVICE_LOST {
            let _ = self.pacer.after_present(result);
            stop(2);
            return result;
        }
        if !crate::guard::is_disabled(Feature::AntiLag) {
            if let Some(call) = self.pacer.after_present(result) {
                self.update(fns, call);
            }
        }
        result
    }
}

/// Associate the successful original swapchain with its admitted AMD device.
/// # Safety
/// Success initialized out; the caller retains the native device and swapchain until wrapped teardown.
pub unsafe fn after_create_swapchain(
    device: vk::Device,
    result: vk::Result,
    out: *mut vk::SwapchainKHR,
) {
    report_pending();
    if result != vk::Result::SUCCESS
        || out.is_null()
        || !has_device_lifetime(device.as_raw() as usize)
        || crate::guard::is_disabled(Feature::AntiLag)
    {
        return;
    }
    guard(
        Feature::AntiLag,
        || (),
        || {
            let _writer = LIFECYCLE.lock().unwrap_or_else(|e| e.into_inner());
            if !has_device_lifetime(device.as_raw() as usize) {
                return;
            }
            // SAFETY: successful creation initialized this caller-owned output.
            let swapchain = unsafe { out.read() };
            let runtime = Runtime {
                swapchain,
                pacer: Pacer::new(),
                presenting: AtomicBool::new(false),
                primed: AtomicBool::new(false),
            };
            replace(Box::into_raw(Box::new(runtime)));
        },
    );
}
/// Retire before native teardown, even after a feature guard has disabled ordinary work.
/// # Safety
/// Called synchronously before destruction with Vulkan external synchronization obeyed.
pub unsafe fn before_destroy(device: vk::Device, swapchain: Option<vk::SwapchainKHR>) {
    report_pending();
    if !FNS.get().is_some_and(|f| f.device == device) {
        return;
    }
    let _writer = LIFECYCLE.lock().unwrap_or_else(|e| e.into_inner());
    if swapchain.is_none() {
        DEVICE_LIVE.store(false, Ordering::Release);
    }
    let matches = {
        let reader = Reader::enter();
        reader
            .runtime()
            .is_some_and(|r| swapchain.is_none_or(|swapchain| r.swapchain == swapchain))
    };
    if matches {
        replace(std::ptr::null_mut());
    }
}
/// Pace only actual queues and the live swapchain of the admitted AMD device generation.
/// # Safety
/// The original closure passes the same valid Vulkan present arguments exactly once.
pub unsafe fn on_present(
    queue: vk::Queue,
    info: *const vk::PresentInfoKHR<'_>,
    original: impl Fn() -> vk::Result,
) -> vk::Result {
    let completed = std::cell::Cell::new(None);
    let once = || {
        if let Some(result) = completed.get() {
            return result;
        }
        let result = original();
        completed.set(Some(result));
        result
    };
    guard(Feature::AntiLag, once, || {
        if info.is_null() || !DEVICE_LIVE.load(Ordering::Acquire) {
            return once();
        }
        let reader = Reader::enter();
        let Some(runtime) = reader.runtime() else {
            return once();
        };
        let Some(fns) = FNS.get() else {
            return once();
        };
        if !DEVICE_LIVE.load(Ordering::Acquire) || !fns.queues.contains(&queue) {
            return once();
        }
        // SAFETY: live native input; exactly one swapchain pointer is read only after checking count.
        let info = unsafe { &*info };
        if info.swapchain_count != 1 || info.p_swapchains.is_null() {
            return once();
        }
        // SAFETY: native contract supplies at least the one checked array entry.
        if unsafe { info.p_swapchains.read() } != runtime.swapchain {
            return once();
        }
        runtime.present(fns, once)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Existing extension and feature entries are not duplicated; device groups are skipped.
    #[test]
    fn device_plan_adds_only_missing_parts() {
        for count in [0, 1] {
            assert_eq!(
                plan_device(&[], false, count),
                DevicePlan {
                    add_extension: true,
                    chain_feature: true
                }
            );
            assert_eq!(
                plan_device(&[c"VK_AMD_anti_lag"], false, count),
                DevicePlan {
                    add_extension: false,
                    chain_feature: true
                }
            );
            assert_eq!(
                plan_device(&[], true, count),
                DevicePlan {
                    add_extension: true,
                    chain_feature: false
                }
            );
            assert_eq!(
                plan_device(&[c"VK_KHR_swapchain", c"VK_AMD_anti_lag"], true, count),
                DevicePlan {
                    add_extension: false,
                    chain_feature: false
                }
            );
        }
        for count in [2, u32::MAX] {
            for exts in [&[][..], &[c"VK_AMD_anti_lag"][..]] {
                for feature in [false, true] {
                    assert_eq!(
                        plan_device(exts, feature, count),
                        DevicePlan {
                            add_extension: false,
                            chain_feature: false
                        }
                    );
                }
            }
        }
    }

    /// Every capability, vendor and mutual-exclusion branch has an explicit expected result.
    #[test]
    fn mode_requires_capabilities_and_excludes_reflex() {
        for (mode, vendor, enabled) in [
            (AntiLagMode::Off, 0x1002, false),
            (AntiLagMode::Off, 0x10de, false),
            (AntiLagMode::Auto, 0x1002, true),
            (AntiLagMode::Auto, 0x10de, false),
            (AntiLagMode::Auto, 0, false),
            (AntiLagMode::On, 0x1002, true),
            (AntiLagMode::On, 0x10de, true),
            (AntiLagMode::On, 0, true),
        ] {
            assert_eq!(decide_mode(&mode, vendor, true, true, false), enabled);
            for (ext, feature, reflex) in [
                (false, false, false),
                (false, true, false),
                (true, false, false),
                (false, false, true),
                (false, true, true),
                (true, false, true),
                (true, true, true),
            ] {
                assert!(!decide_mode(&mode, vendor, ext, feature, reflex));
            }
        }
    }

    /// Five presents produce matched stage pairs after the initial unmarked present.
    #[test]
    fn pacer_pairs_input_and_present_over_five_frames() {
        let p = Pacer::new();
        assert_eq!(p.before_present(), None);
        for (current, next) in [(1, 2), (2, 3), (3, 4), (4, 5), (5, 6)] {
            if current > 1 {
                assert_eq!(
                    p.before_present(),
                    Some(Call {
                        stage: Stage::Present,
                        frame_index: current
                    })
                );
                assert_eq!(p.before_present(), None, "one PRESENT per INPUT");
            }
            assert_eq!(
                p.after_present(vk::Result::SUCCESS),
                Some(Call {
                    stage: Stage::Input,
                    frame_index: next
                })
            );
        }
        assert_eq!(Pacer::default().before_present(), None);
    }

    /// A near-exhausted test fixture still exercises only the public call-planning interface.
    #[test]
    fn exhausted_frame_counter_stops_without_reusing_an_id() {
        let p = Pacer {
            frame_index: AtomicU64::new(u64::MAX - 1),
            ..Pacer::new()
        };
        assert_eq!(
            p.after_present(vk::Result::SUCCESS),
            Some(Call {
                stage: Stage::Input,
                frame_index: u64::MAX
            })
        );
        assert_eq!(
            p.before_present(),
            Some(Call {
                stage: Stage::Present,
                frame_index: u64::MAX
            })
        );
        for _ in 0..3 {
            assert_eq!(p.after_present(vk::Result::SUCCESS), None);
            assert_eq!(p.before_present(), None);
        }
    }

    /// A lost device cancels a pending PRESENT and permanently stops INPUT planning.
    #[test]
    fn device_lost_disables_pacer() {
        for prime in [false, true] {
            let p = Pacer::new();
            if prime {
                assert!(p.after_present(vk::Result::SUCCESS).is_some());
            }
            assert_eq!(p.after_present(vk::Result::ERROR_DEVICE_LOST), None);
            for _ in 0..3 {
                assert_eq!(p.before_present(), None);
                assert_eq!(p.after_present(vk::Result::SUCCESS), None);
            }
        }
    }

    /// Non-device-loss present results still advance as required by the pure-layer contract.
    #[test]
    fn other_present_results_keep_sequence() {
        let p = Pacer::new();
        for (result, next) in [
            (vk::Result::SUBOPTIMAL_KHR, 2),
            (vk::Result::ERROR_OUT_OF_DATE_KHR, 3),
        ] {
            assert_eq!(
                p.after_present(result),
                Some(Call {
                    stage: Stage::Input,
                    frame_index: next
                })
            );
            assert_eq!(
                p.before_present(),
                Some(Call {
                    stage: Stage::Present,
                    frame_index: next
                })
            );
        }
    }

    /// Three strictly-over-100 ms updates disable once, including a pending PRESENT.
    #[test]
    fn three_slow_calls_disable_once() {
        let p = Pacer::new();
        assert!(p.after_present(vk::Result::SUCCESS).is_some());
        assert!(!p.note_block(100_001));
        assert!(!p.note_block(u64::MAX));
        assert!(p.note_block(100_001));
        for _ in 0..5 {
            assert!(!p.note_block(u64::MAX));
            assert!(!p.note_block(0));
            assert_eq!(p.before_present(), None);
            assert_eq!(p.after_present(vk::Result::SUCCESS), None);
        }
    }

    /// A fast update, including exactly 100 ms, resets consecutive slow updates.
    #[test]
    fn fast_call_resets_slow_streak() {
        for fast in [0, 99_999, 100_000] {
            let p = Pacer::new();
            assert!(!p.note_block(100_001));
            assert!(!p.note_block(100_001));
            assert!(!p.note_block(fast));
            assert!(!p.note_block(100_001));
            assert!(!p.note_block(100_001));
            assert!(p.after_present(vk::Result::SUCCESS).is_some());
            assert!(p.note_block(100_001));
        }
    }

    /// Another stop cause cannot later look like a new slow-call shutdown.
    #[test]
    fn slow_call_never_reports_a_second_disable_cause() {
        let p = Pacer::new();
        assert!(!p.note_block(100_001));
        assert!(!p.note_block(100_001));
        assert_eq!(p.after_present(vk::Result::ERROR_DEVICE_LOST), None);
        assert!(!p.note_block(100_001));
    }

    /// Every FFI field is initialized, including both previously non-null chains.
    #[test]
    fn fill_data_initializes_both_stages_and_clears_chains() {
        for (stage, raw_stage, frame_index) in [(Stage::Input, 0, 2), (Stage::Present, 1, u64::MAX)]
        {
            let mut info = AntiLagPresentationInfoAMD {
                s_type: vk::StructureType::from_raw(-1),
                p_next: std::ptr::dangling_mut(),
                stage: AntiLagStageAMD::INPUT,
                frame_index: 0,
            };
            let mut data = AntiLagDataAMD {
                s_type: vk::StructureType::from_raw(-1),
                p_next: std::ptr::dangling(),
                mode: AntiLagModeAMD::OFF,
                max_fps: 999,
                p_presentation_info: std::ptr::null(),
            };
            fill_data(Call { stage, frame_index }, &mut info, &mut data);
            assert_eq!(info.s_type.as_raw(), 1_000_476_002);
            assert!(info.p_next.is_null());
            assert_eq!(info.stage.as_raw(), raw_stage);
            assert_eq!(info.frame_index, frame_index);
            assert_eq!(data.s_type.as_raw(), 1_000_476_001);
            assert!(data.p_next.is_null());
            assert_eq!(data.mode, AntiLagModeAMD::ON);
            assert_eq!(data.max_fps, 0);
            assert_eq!(data.p_presentation_info, std::ptr::from_ref(&info));
        }
    }
}
