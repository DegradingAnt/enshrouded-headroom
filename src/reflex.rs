//! NVIDIA Reflex decisions and Vulkan wiring.
//!
//! W23 checkpoint: parsed-setting startup initialization and the quiet-disable present callback
//! are not connected until the maintainer grants the required entry.rs/guard.rs scope. An unset
//! mode leaves device creation unchanged. The complete native paths are exercised by fakes.

use std::ffi::CStr;

use ash::vk;

use crate::config::ReflexMode;

/// Where future wiring must enable the timeline semaphore feature.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimelineFix {
    /// Set the bit in the existing Vulkan 1.2 feature struct.
    SetInVulkan12,
    /// Set the bit in the existing timeline semaphore feature struct.
    SetInTimelineStruct,
    /// Chain a new timeline semaphore feature struct.
    ChainNewStruct,
}

/// Plan only missing Reflex and timeline extensions, and the timeline feature edit.
///
/// This is not a complete device-create plan. Future wiring must check driver capabilities,
/// enable present-ID support and satisfy the remaining prerequisites in design section 6.5.
/// Existing Vulkan 1.2 features take precedence over a standalone timeline feature struct.
pub fn plan_device(
    exts: &[&CStr],
    has_v12_features: bool,
    has_timeline_struct: bool,
    api_version: u32,
) -> (Vec<&'static CStr>, TimelineFix) {
    let mut extra = Vec::new();
    if !exts.contains(&ash::nv::low_latency2::NAME) {
        extra.push(ash::nv::low_latency2::NAME);
    }
    if api_version < vk::API_VERSION_1_2 && !exts.contains(&ash::khr::timeline_semaphore::NAME) {
        extra.push(ash::khr::timeline_semaphore::NAME);
    }
    let fix = if has_v12_features {
        TimelineFix::SetInVulkan12
    } else if has_timeline_struct {
        TimelineFix::SetInTimelineStruct
    } else {
        TimelineFix::ChainNewStruct
    };
    (extra, fix)
}

/// Select Reflex boost (`Some`) or off (`None`) without querying the game or driver.
///
/// Saved menu values are 0 = off, 1 = on and 2 = boost. A missing or unknown value uses auto.
/// Every enabled mode requires NVIDIA and a driver-listed `VK_NV_low_latency2` extension.
pub fn decide_mode(
    cfg: &ReflexMode,
    game_menu: Option<u32>,
    vendor_nvidia: bool,
    ext_listed: bool,
) -> Option<bool> {
    if !vendor_nvidia || !ext_listed {
        return None;
    }
    match cfg {
        ReflexMode::Off => None,
        ReflexMode::On | ReflexMode::Auto => Some(false),
        ReflexMode::Boost => Some(true),
        ReflexMode::Game => match game_menu {
            Some(0) => None,
            Some(2) => Some(true),
            _ => Some(false),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Add the extension once, preserving the existing Vulkan 1.2 feature struct.
    #[test]
    fn adds_low_latency2_once() {
        let (extra, fix) = plan_device(
            &[c"VK_KHR_swapchain", c"VK_KHR_present_id2"],
            true,
            false,
            vk::API_VERSION_1_2,
        );
        assert_eq!(extra, vec![c"VK_NV_low_latency2"]);
        assert_eq!(fix, TimelineFix::SetInVulkan12);
        assert!(
            plan_device(&[c"VK_NV_low_latency2"], true, true, vk::API_VERSION_1_2)
                .0
                .is_empty()
        );
    }

    /// The old API needs the timeline extension and a feature-chain edit.
    #[test]
    fn old_api_adds_timeline_ext_and_struct() {
        assert_eq!(
            plan_device(&[], false, false, vk::API_VERSION_1_1),
            (
                vec![c"VK_NV_low_latency2", c"VK_KHR_timeline_semaphore"],
                TimelineFix::ChainNewStruct
            )
        );
        assert_eq!(
            plan_device(
                &[c"VK_NV_low_latency2", c"VK_KHR_timeline_semaphore"],
                false,
                true,
                vk::API_VERSION_1_1
            ),
            (vec![], TimelineFix::SetInTimelineStruct)
        );
        assert_eq!(
            plan_device(&[], true, true, vk::API_VERSION_1_1).1,
            TimelineFix::SetInVulkan12
        );
        assert_eq!(
            plan_device(&[], false, true, vk::API_VERSION_1_2).0,
            vec![c"VK_NV_low_latency2"]
        );
    }

    /// Saved 0/1/2 values select off/on/boost; unknown values fall back to auto.
    #[test]
    fn mode_follows_game_menu() {
        for (menu, expected) in [
            (Some(0), None),
            (Some(1), Some(false)),
            (Some(2), Some(true)),
            (Some(3), Some(false)),
            (Some(u32::MAX), Some(false)),
            (None, Some(false)),
        ] {
            assert_eq!(decide_mode(&ReflexMode::Game, menu, true, true), expected);
        }
        for (mode, expected) in [
            (ReflexMode::Off, None),
            (ReflexMode::On, Some(false)),
            (ReflexMode::Boost, Some(true)),
            (ReflexMode::Auto, Some(false)),
        ] {
            for menu in [Some(0), Some(1), Some(2), None] {
                assert_eq!(decide_mode(&mode, menu, true, true), expected);
            }
        }
    }

    /// Every mode is gated by both NVIDIA vendor and extension support.
    #[test]
    fn unsupported_device_never_enables_reflex() {
        for mode in [
            ReflexMode::Game,
            ReflexMode::Auto,
            ReflexMode::Off,
            ReflexMode::On,
            ReflexMode::Boost,
        ] {
            for (nvidia, listed) in [(false, false), (false, true), (true, false)] {
                for menu in [None, Some(0), Some(1), Some(2), Some(3)] {
                    assert_eq!(decide_mode(&mode, menu, nvidia, listed), None);
                }
            }
        }
    }
}

#[cfg(test)]
mod wiring_tests {
    use super::*;
    /// Only Game requests the native saved-value reader; all other modes keep startup unchanged.
    #[test]
    fn menu_hook_request_follows_initialized_mode() {
        if let Ok(value) = std::env::var("W23_MODE_CHILD") {
            let (mode, expected) = match value.as_str() {
                "game" => (ReflexMode::Game, true),
                "off" => (ReflexMode::Off, false),
                "on" => (ReflexMode::On, false),
                "boost" => (ReflexMode::Boost, false),
                _ => (ReflexMode::Auto, false),
            };
            assert!(
                !wants_game_menu(),
                "unset configuration leaves existing startup unchanged"
            );
            init(mode);
            assert_eq!(wants_game_menu(), expected);
            return;
        }
        for value in ["game", "off", "on", "boost", "auto"] {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "reflex::wiring_tests::menu_hook_request_follows_initialized_mode",
                    "--nocapture",
                ])
                .env("W23_MODE_CHILD", value)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{value}: {}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }
    /// The real reader accepts only the verified options relationship and both enum ranges.
    #[test]
    fn menu_guard_and_ranges() {
        let mut buffer = [0u64; 32];
        let base = buffer.as_mut_ptr().cast::<u8>();
        // SAFETY: all offsets below lie inside this aligned buffer.
        unsafe {
            let render = base.add(16);
            render.sub(8).cast::<u32>().write(2);
            render.sub(11).write(1);
            assert_eq!(read_menu(render, render.add(0x54)), Some((2, true)));
            assert_eq!(read_menu(render, render.add(0x50)), None);
            render.sub(8).cast::<u32>().write(3);
            assert_eq!(read_menu(render, render.add(0x54)), None);
            render.sub(8).cast::<u32>().write(1);
            render.sub(11).write(2);
            assert_eq!(read_menu(render, render.add(0x54)), None);
            assert_eq!(read_menu(std::ptr::null(), std::ptr::null()), None);
            assert_eq!(
                read_menu(
                    std::ptr::without_provenance(usize::MAX - 8),
                    std::ptr::without_provenance(0x4b)
                ),
                None
            );
        }
    }
}
/// Saved Reflex and vsync values, published together; all ones means unknown.
static MENU: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(u32::MAX);

/// Read the verified settings relationship without logging or allocation.
///
/// # Safety
/// When the checked relationship holds, `render` must belong to the live options allocation
/// described in re-notes. Its preceding eleven bytes must be readable. The game's render
/// thread owns this access; volatile reads preserve its native aligned loads.
pub unsafe fn read_menu(render: *const u8, prefs: *const u8) -> Option<(u32, bool)> {
    let address = render as usize;
    if address == 0
        || address.checked_add(0x54) != Some(prefs as usize)
        || address.checked_sub(11).is_none()
        || !address.is_multiple_of(4)
    {
        return None;
    }
    // SAFETY: the caller supplies the verified live allocation when the relationship holds.
    let reflex = unsafe { render.sub(8).cast::<u32>().read_volatile() };
    // SAFETY: the same allocation contains the byte eleven bytes before render.
    let vsync = unsafe { render.sub(11).read_volatile() };
    (reflex <= 2 && vsync <= 1).then_some((reflex, vsync != 0))
}

/// Publish a coherent optional menu pair. Relaxed ordering is sufficient for this value alone.
pub fn publish_menu(value: Option<(u32, bool)>) {
    let packed = value
        .filter(|(mode, _)| *mode <= 2)
        .map_or(u32::MAX, |(mode, vsync)| mode | (u32::from(vsync) << 8));
    MENU.store(packed, std::sync::atomic::Ordering::Relaxed);
}

/// Latest saved menu value. The value is unknown until a valid post-hook read.
pub fn game_menu() -> Option<u32> {
    let value = MENU.load(std::sync::atomic::Ordering::Relaxed);
    (value != u32::MAX).then_some(value & 0xff)
}
#[cfg(test)]
mod device_wiring_tests {
    use super::*;
    /// Unknown headers have no invented size and are left for the unmodified renderer.
    #[test]
    fn opaque_device_feature_chain_is_refused_without_mutation() {
        let unknown = vk::BaseInStructure {
            s_type: vk::StructureType::APPLICATION_INFO,
            ..Default::default()
        };
        let names = [ash::khr::swapchain::NAME.as_ptr()];
        let info = vk::DeviceCreateInfo {
            enabled_extension_count: 1,
            pp_enabled_extension_names: names.as_ptr(),
            p_next: (&unknown as *const vk::BaseInStructure<'_>).cast(),
            ..Default::default()
        };
        let caps = Capabilities {
            vendor: 0x10de,
            timeline: true,
            present_id: true,
            extensions: vec![
                c"VK_NV_low_latency2".to_owned(),
                c"VK_KHR_timeline_semaphore".to_owned(),
                c"VK_KHR_present_id".to_owned(),
            ],
        };
        // SAFETY: a live unknown header; the function must read only that common header then refuse.
        assert!(unsafe { DeviceCopy::new(&info, &caps) }.is_none());
        assert!(unknown.p_next.is_null());
        assert_eq!(
            info.p_next,
            (&unknown as *const vk::BaseInStructure<'_>).cast()
        );
    }
    /// Enabling timeline support changes an owned copy and leaves every original byte/link intact.
    #[test]
    fn device_copy_preserves_original_chain_and_features() {
        let mut tail = vk::PhysicalDeviceVulkan13Features::default().dynamic_rendering(true);
        let mut v12 = vk::PhysicalDeviceVulkan12Features::default().buffer_device_address(true);
        v12.p_next = (&mut tail as *mut vk::PhysicalDeviceVulkan13Features<'_>).cast();
        let original_next = v12.p_next;
        let names = [c"VK_KHR_swapchain".as_ptr()];
        let mut info = vk::DeviceCreateInfo::default().enabled_extension_names(&names);
        info.p_next = (&v12 as *const vk::PhysicalDeviceVulkan12Features).cast();
        let caps = Capabilities {
            vendor: 0x10de,
            timeline: true,
            present_id: true,
            extensions: vec![
                c"VK_NV_low_latency2".to_owned(),
                c"VK_KHR_timeline_semaphore".to_owned(),
                c"VK_KHR_present_id".to_owned(),
            ],
        };
        // SAFETY: info and its complete chain are live for this test.
        let mut owned = unsafe { DeviceCopy::new(&info, &caps) }.unwrap();
        let changed = owned.info(&info);
        assert_ne!(changed.p_next, info.p_next);
        assert_eq!(changed.enabled_extension_count, 4);
        assert_eq!(v12.timeline_semaphore, vk::FALSE);
        assert_eq!(v12.buffer_device_address, vk::TRUE);
        assert_eq!(v12.p_next, original_next);
        // SAFETY: DeviceCopy retains all node allocations for changed's use.
        unsafe {
            let mut node = changed.p_next.cast::<vk::BaseInStructure<'_>>();
            let mut seen = false;
            while !node.is_null() {
                if (*node).s_type == vk::StructureType::PHYSICAL_DEVICE_VULKAN_1_2_FEATURES {
                    let features = &*node.cast::<vk::PhysicalDeviceVulkan12Features<'_>>();
                    assert_eq!(features.timeline_semaphore, vk::TRUE);
                    assert_eq!(features.buffer_device_address, vk::TRUE);
                    seen = true;
                }
                node = (*node).p_next;
            }
            assert!(seen);
        }
    }
}

use crate::guard::{guard, Feature};
use ash::vk::Handle;
use std::ffi::CString;
use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicU64, AtomicUsize, Ordering};
use std::sync::OnceLock;

/// Configuration published before the wrapper is installed. An unset setting fails closed.
static MODE: OnceLock<ReflexMode> = OnceLock::new();

/// Save the parsed setting before installing Vulkan hooks.
pub fn init(mode: ReflexMode) {
    let _ = MODE.set(mode);
}

/// Whether the saved menu needs the scene-settings reader, even with every distance knob off.
pub fn wants_game_menu() -> bool {
    MODE.get() == Some(&ReflexMode::Game)
}

/// Driver capabilities queried before modifying the device create info.
struct Capabilities {
    /// PCI vendor ID.
    vendor: u32,
    /// Timeline semaphore feature bit.
    timeline: bool,
    /// Present-ID v1 feature bit; v2 already enabled by the game is preserved separately.
    present_id: bool,
    /// Driver-listed extension names.
    extensions: Vec<CString>,
}

impl Capabilities {
    /// Whether the driver lists a named extension.
    fn lists(&self, name: &CStr) -> bool {
        self.extensions.iter().any(|n| n.as_c_str() == name)
    }
}

/// Typed ownership of every copied device-chain node. Unknown layouts are never guessed.
macro_rules! chain_nodes {
    ($($variant:ident: $ty:ty => $tag:expr),+ $(,)?) => {
        /// Heap allocations stay fixed while the vector moves.
        enum Node { $($variant(Box<$ty>)),+ }
        impl Node {
            /// Address of the Vulkan common prefix of an owned, correctly typed node.
            fn ptr(&mut self) -> *mut vk::BaseOutStructure<'static> {
                match self { $(Self::$variant(value) => (&mut **value as *mut $ty).cast()),+ }
            }
            /// Copy exactly the type identified by the Vulkan sType, never opaque bytes.
            /// # Safety
            /// `node` points to a live valid Vulkan input-chain structure of its declared sType.
            unsafe fn copy(node: *const vk::BaseInStructure<'_>) -> Option<Self> {
                // SAFETY: valid Vulkan input chain, as required above.
                match unsafe { (*node).s_type } {
                    $(tag if tag == $tag => {
                        // SAFETY: the matching sType identifies this exact Vulkan layout.
                        Some(Self::$variant(Box::new(unsafe { node.cast::<$ty>().read() })))
                    }),+,
                    _ => None,
                }
            }
        }
    }
}
chain_nodes! {
    Features: vk::PhysicalDeviceFeatures2<'static> => vk::StructureType::PHYSICAL_DEVICE_FEATURES_2,
    V11: vk::PhysicalDeviceVulkan11Features<'static> => vk::StructureType::PHYSICAL_DEVICE_VULKAN_1_1_FEATURES,
    V12: vk::PhysicalDeviceVulkan12Features<'static> => vk::StructureType::PHYSICAL_DEVICE_VULKAN_1_2_FEATURES,
    V13: vk::PhysicalDeviceVulkan13Features<'static> => vk::StructureType::PHYSICAL_DEVICE_VULKAN_1_3_FEATURES,
    Timeline: vk::PhysicalDeviceTimelineSemaphoreFeatures<'static> => vk::StructureType::PHYSICAL_DEVICE_TIMELINE_SEMAPHORE_FEATURES,
    Present: vk::PhysicalDevicePresentIdFeaturesKHR<'static> => vk::StructureType::PHYSICAL_DEVICE_PRESENT_ID_FEATURES_KHR,
    Present2: crate::vk::types::PhysicalDevicePresentId2FeaturesKHR => crate::vk::types::STRUCTURE_TYPE_PHYSICAL_DEVICE_PRESENT_ID_2_FEATURES_KHR,
    Group: vk::DeviceGroupDeviceCreateInfo<'static> => vk::StructureType::DEVICE_GROUP_DEVICE_CREATE_INFO,
    Sync2: vk::PhysicalDeviceSynchronization2Features<'static> => vk::StructureType::PHYSICAL_DEVICE_SYNCHRONIZATION_2_FEATURES,
    Dynamic: vk::PhysicalDeviceDynamicRenderingFeatures<'static> => vk::StructureType::PHYSICAL_DEVICE_DYNAMIC_RENDERING_FEATURES,
}

/// Owns extensions and typed copies. Borrowed child arrays remain owned by the original call.
struct DeviceCopy {
    nodes: Vec<Node>,
    extensions: Vec<*const std::ffi::c_char>,
}

impl DeviceCopy {
    /// Build an immutable-input device plan. Unsupported or cyclic chains fail closed.
    /// # Safety
    /// `original` and its extension names and chain satisfy vkCreateDevice's input contract.
    unsafe fn new(original: &vk::DeviceCreateInfo<'_>, caps: &Capabilities) -> Option<Self> {
        if caps.vendor != 0x10de
            || !caps.timeline
            || !caps.lists(ash::nv::low_latency2::NAME)
            || !caps.lists(ash::khr::timeline_semaphore::NAME)
        {
            return None;
        }
        let names = if original.enabled_extension_count == 0 {
            &[][..]
        } else {
            // SAFETY: the Vulkan input contract supplies this many valid name pointers.
            unsafe {
                std::slice::from_raw_parts(
                    original.pp_enabled_extension_names,
                    original.enabled_extension_count as usize,
                )
            }
        };
        let mut exts = Vec::new();
        for name in names {
            // SAFETY: each device extension name is a NUL-terminated Vulkan input string.
            exts.push(unsafe { CStr::from_ptr(*name) });
        }
        if !exts.contains(&ash::khr::swapchain::NAME) {
            return None;
        }
        let mut nodes = Vec::new();
        let mut cursor = original.p_next.cast::<vk::BaseInStructure<'_>>();
        let mut seen = Vec::new();
        while !cursor.is_null() {
            if seen.len() >= 64 || seen.contains(&cursor) {
                return None;
            }
            seen.push(cursor);
            // SAFETY: the original Vulkan chain contains live typed nodes for this call.
            nodes.push(unsafe { Node::copy(cursor) }?);
            // SAFETY: cursor is the current live Vulkan common header.
            cursor = unsafe { (*cursor).p_next };
        }
        let has_v12 = nodes.iter().any(|n| matches!(n, Node::V12(_)));
        let has_timeline = nodes.iter().any(|n| matches!(n, Node::Timeline(_)));
        // Vulkan forbids duplicating promoted features alongside their aggregate structure.
        if has_v12 && has_timeline {
            return None;
        }
        let uses_id2 = exts.contains(&crate::vk::types::KHR_PRESENT_ID_2_NAME)
            && nodes
                .iter()
                .any(|n| matches!(n, Node::Present2(p) if p.present_id2 == vk::TRUE));
        if uses_id2 && !caps.lists(crate::vk::types::KHR_PRESENT_ID_2_NAME) {
            return None;
        }
        if !uses_id2 && (!caps.present_id || !caps.lists(ash::khr::present_id::NAME)) {
            return None;
        }
        for node in &mut nodes {
            match node {
                Node::Group(group) if group.physical_device_count > 1 => return None,
                Node::V12(features) => features.timeline_semaphore = vk::TRUE,
                Node::Timeline(features) => features.timeline_semaphore = vk::TRUE,
                Node::Present(features) if !uses_id2 => features.present_id = vk::TRUE,
                _ => (),
            }
        }
        // The instance version is not intercepted. Requiring and enabling the KHR timeline
        // extension is conservative even on 1.2+, and makes the KHR entry point always legal.
        let (extra, fix) = plan_device(&exts, has_v12, has_timeline, vk::API_VERSION_1_1);
        let mut extensions = names.to_vec();
        extensions.extend(extra.iter().map(|e| e.as_ptr()));
        if fix == TimelineFix::ChainNewStruct {
            nodes.push(Node::Timeline(Box::new(
                vk::PhysicalDeviceTimelineSemaphoreFeatures::default().timeline_semaphore(true),
            )));
        }
        if !uses_id2 {
            if !exts.contains(&ash::khr::present_id::NAME) {
                extensions.push(ash::khr::present_id::NAME.as_ptr());
            }
            if !nodes.iter().any(|n| matches!(n, Node::Present(_))) {
                nodes.push(Node::Present(Box::new(
                    vk::PhysicalDevicePresentIdFeaturesKHR::default().present_id(true),
                )));
            }
        }
        Some(Self { nodes, extensions })
    }

    /// Link only owned node headers, retaining their allocations through the driver call.
    fn info<'a>(&mut self, original: &vk::DeviceCreateInfo<'a>) -> vk::DeviceCreateInfo<'a> {
        let mut next = std::ptr::null_mut();
        for node in self.nodes.iter_mut().rev() {
            let pointer = node.ptr();
            // SAFETY: pointer is this node's owned Vulkan header and has no active references.
            unsafe {
                (*pointer).p_next = next;
            }
            next = pointer;
        }
        let mut info = *original;
        info.p_next = next.cast();
        info.enabled_extension_count = self.extensions.len().try_into().unwrap_or(0);
        info.pp_enabled_extension_names = self.extensions.as_ptr();
        info
    }
}

/// Resolve a physical-device function through the saved instance trampoline.
/// # Safety
/// `raw` is the real GIPA for the live, leased `instance`; use the result only for that
/// instance or its children, at the named Vulkan type, before releasing the lease.
unsafe fn instance_proc(
    raw: usize,
    instance: usize,
    name: &CStr,
) -> Option<unsafe extern "system" fn()> {
    if raw == 0 || instance == 0 {
        return None;
    }
    // SAFETY: the caller supplies the real GIPA retained with its observed parent generation.
    let get = unsafe { std::mem::transmute::<usize, vk::PFN_vkGetInstanceProcAddr>(raw) };
    // SAFETY: the recorded live instance and a static NUL-terminated function name.
    unsafe { get(vk::Instance::from_raw(instance as u64), name.as_ptr()) }
}

/// Query only supported feature structures. No device is modified here.
/// # Safety
/// `phys` belongs to the recorded live instance.
unsafe fn capabilities(
    phys: vk::PhysicalDevice,
    gipa: usize,
    instance: usize,
) -> Option<Capabilities> {
    // SAFETY: each entry is resolved by its precise Vulkan function name.
    let (properties, enumerate, features) = unsafe {
        (
            std::mem::transmute::<unsafe extern "system" fn(), vk::PFN_vkGetPhysicalDeviceProperties>(
                instance_proc(gipa, instance, c"vkGetPhysicalDeviceProperties")?,
            ),
            std::mem::transmute::<
                unsafe extern "system" fn(),
                vk::PFN_vkEnumerateDeviceExtensionProperties,
            >(instance_proc(
                gipa,
                instance,
                c"vkEnumerateDeviceExtensionProperties",
            )?),
            std::mem::transmute::<unsafe extern "system" fn(), vk::PFN_vkGetPhysicalDeviceFeatures2>(
                instance_proc(gipa, instance, c"vkGetPhysicalDeviceFeatures2")?,
            ),
        )
    };
    let mut properties_out = vk::PhysicalDeviceProperties::default();
    // SAFETY: phys is live and output has the exact Vulkan layout.
    unsafe { properties(phys, &mut properties_out) };
    if properties_out.vendor_id != 0x10de {
        return None;
    }
    let mut count = 0;
    // SAFETY: null layer selects device extensions and null output queries the count.
    if unsafe { enumerate(phys, std::ptr::null(), &mut count, std::ptr::null_mut()) }
        != vk::Result::SUCCESS
        || count > 4096
    {
        return None;
    }
    let mut listed = vec![vk::ExtensionProperties::default(); count as usize];
    // SAFETY: listed has the advertised capacity. INCOMPLETE is a safe refusal.
    if unsafe { enumerate(phys, std::ptr::null(), &mut count, listed.as_mut_ptr()) }
        != vk::Result::SUCCESS
    {
        return None;
    }
    let mut extensions = Vec::new();
    for entry in listed.get(..count as usize)? {
        // The driver promises a terminated name; checking the array bounds also handles a broken fake.
        let bytes: Vec<u8> = entry.extension_name.iter().map(|c| *c as u8).collect();
        let name = CStr::from_bytes_until_nul(&bytes).ok()?;
        if name == ash::nv::low_latency2::NAME && entry.spec_version < 2 {
            return None;
        }
        extensions.push(name.to_owned());
    }
    let mut caps = Capabilities {
        vendor: properties_out.vendor_id,
        timeline: false,
        present_id: false,
        extensions,
    };
    if !caps.lists(ash::nv::low_latency2::NAME) || !caps.lists(ash::khr::timeline_semaphore::NAME) {
        return None;
    }
    let mut timeline = vk::PhysicalDeviceTimelineSemaphoreFeatures::default();
    let mut present = vk::PhysicalDevicePresentIdFeaturesKHR::default();
    let mut query = vk::PhysicalDeviceFeatures2::default().push_next(&mut timeline);
    if caps.lists(ash::khr::present_id::NAME) {
        query = query.push_next(&mut present);
    }
    // SAFETY: only driver-listed feature nodes are queried, in live writable local structures.
    unsafe { features(phys, &mut query) };
    caps.timeline = timeline.timeline_semaphore == vk::TRUE;
    caps.present_id = present.present_id == vk::TRUE;
    Some(caps)
}

/// Device entry points retained by value; they remain callable only while `device` is live.
#[derive(Clone, Copy)]
struct Fns {
    device: vk::Device,
    set_mode: vk::PFN_vkSetLatencySleepModeNV,
    marker: vk::PFN_vkSetLatencyMarkerNV,
    sleep: vk::PFN_vkLatencySleepNV,
    wait: vk::PFN_vkWaitSemaphores,
    timings: vk::PFN_vkGetLatencyTimingsNV,
    create_semaphore: vk::PFN_vkCreateSemaphore,
    destroy_semaphore: vk::PFN_vkDestroySemaphore,
}
/// First Reflex-enabled device only. A destroyed device never becomes live again.
static FNS: OnceLock<Fns> = OnceLock::new();
/// Invalidated before device destruction.
static DEVICE_LIVE: AtomicBool = AtomicBool::new(false);

impl Fns {
    /// Load every required function before publishing the table.
    /// # Safety
    /// `device` is a newly created device with all planned extensions/features enabled.
    unsafe fn load(device: vk::Device) -> Option<Self> {
        /// Resolve and cast one exact device entry point; never use ash's panic stubs.
        macro_rules! load {
            ($name:expr, $ty:ty) => {{
                // SAFETY: device is live and the literal names exactly the requested PFN type.
                unsafe {
                    std::mem::transmute::<usize, $ty>(crate::vk::device_proc(
                        device.as_raw() as usize,
                        $name,
                    )?)
                }
            }};
        }
        Some(Self {
            device,
            set_mode: load!(c"vkSetLatencySleepModeNV", vk::PFN_vkSetLatencySleepModeNV),
            marker: load!(c"vkSetLatencyMarkerNV", vk::PFN_vkSetLatencyMarkerNV),
            sleep: load!(c"vkLatencySleepNV", vk::PFN_vkLatencySleepNV),
            wait: load!(c"vkWaitSemaphoresKHR", vk::PFN_vkWaitSemaphores),
            timings: load!(c"vkGetLatencyTimingsNV", vk::PFN_vkGetLatencyTimingsNV),
            create_semaphore: load!(c"vkCreateSemaphore", vk::PFN_vkCreateSemaphore),
            destroy_semaphore: load!(c"vkDestroySemaphore", vk::PFN_vkDestroySemaphore),
        })
    }
}

/// Try Reflex device creation, retrying exactly the original pointer after driver refusal.
/// # Safety
/// The arguments obey vkCreateDevice's contract, and `real` is its real entry point.
pub unsafe fn create_device(
    real: vk::PFN_vkCreateDevice,
    phys: vk::PhysicalDevice,
    info: *const vk::DeviceCreateInfo<'_>,
    allocator: *const vk::AllocationCallbacks<'_>,
    out: *mut vk::Device,
) -> vk::Result {
    let completed = std::cell::Cell::new(None);
    let original = || {
        if let Some(result) = completed.get() {
            return result;
        }
        // SAFETY: the original arguments pass through unchanged.
        let result = unsafe { real(phys, info, allocator, out) };
        completed.set(Some(result));
        result
    };
    guard(Feature::Reflex, original, || {
        let Some(mode) = MODE.get() else {
            return original();
        };
        if *mode == ReflexMode::Off
            || info.is_null()
            || FNS.get().is_some()
            || !crate::vk::prepare_device_lifetime(phys, true)
        {
            return original();
        }
        // SAFETY: the original Vulkan arguments and live physical device.
        let Some(caps) = crate::vk::with_physical_device(phys, |gipa, instance| unsafe {
            capabilities(phys, gipa, instance)
        })
        .flatten() else {
            return original();
        };
        // SAFETY: info and its chain live until this call returns.
        let Some(mut copied) = (unsafe { DeviceCopy::new(&*info, &caps) }) else {
            log::info!("Reflex: unsupported device prerequisites or feature-chain layout; unchanged device");
            return original();
        };
        // SAFETY: the original info is live and the owned nodes live through the next driver call.
        let changed = copied.info(unsafe { &*info });
        // SAFETY: changed keeps all original inputs alive and owns each changed node/list.
        let result = unsafe { real(phys, &changed, allocator, out) };
        if result != vk::Result::SUCCESS {
            crate::guard::disable(Feature::Reflex, "driver refused");
            return original();
        }
        completed.set(Some(result));
        guard(
            Feature::Reflex,
            || (),
            || {
                // SAFETY: success initialized the caller's device output.
                if let Some(fns) = unsafe { Fns::load(out.read()) } {
                    if FNS.set(fns).is_ok() {
                        DEVICE_LIVE.store(true, Ordering::Release);
                    }
                } else {
                    crate::guard::disable(Feature::Reflex, "missing device entry point");
                }
            },
        );
        result
    })
}

#[cfg(test)]
mod runtime_tests {
    use super::*;
    use std::cell::RefCell;
    thread_local! { static CALLS: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) }; }
    fn note(value: String) {
        CALLS.with(|c| c.borrow_mut().push(value));
    }
    fn calls() -> Vec<String> {
        CALLS.with(|c| std::mem::take(&mut *c.borrow_mut()))
    }
    /// Fake driver output storage is valid for the duration of each test call.
    unsafe extern "system" fn create(
        _: vk::Device,
        _: *const vk::SemaphoreCreateInfo<'_>,
        _: *const vk::AllocationCallbacks<'_>,
        out: *mut vk::Semaphore,
    ) -> vk::Result {
        guard(
            Feature::VkWrap,
            || vk::Result::ERROR_UNKNOWN,
            || {
                // SAFETY: Runtime passes a valid local output pointer.
                unsafe { out.write(vk::Semaphore::from_raw(33)) };
                vk::Result::SUCCESS
            },
        )
    }
    unsafe extern "system" fn destroy(
        _: vk::Device,
        _: vk::Semaphore,
        _: *const vk::AllocationCallbacks<'_>,
    ) {
        let body = || note("destroy".into());
        guard(Feature::VkWrap, body, body)
    }
    unsafe extern "system" fn mode(
        _: vk::Device,
        swap: vk::SwapchainKHR,
        info: *const vk::LatencySleepModeInfoNV<'_>,
    ) -> vk::Result {
        guard(
            Feature::VkWrap,
            || vk::Result::ERROR_UNKNOWN,
            || {
                // SAFETY: Runtime passes a live initialized Vulkan input struct.
                let info = unsafe { &*info };
                note(format!(
                    "mode:{}:{}:{}",
                    swap.as_raw(),
                    info.low_latency_mode,
                    info.low_latency_boost
                ));
                vk::Result::SUCCESS
            },
        )
    }
    unsafe extern "system" fn marker(
        _: vk::Device,
        _: vk::SwapchainKHR,
        info: *const vk::SetLatencyMarkerInfoNV<'_>,
    ) {
        guard(
            Feature::VkWrap,
            || (),
            || {
                // SAFETY: Runtime passes a live initialized marker.
                let info = unsafe { &*info };
                note(format!(
                    "marker:{}:{}",
                    info.present_id,
                    info.marker.as_raw()
                ));
            },
        )
    }
    unsafe extern "system" fn sleep(
        _: vk::Device,
        _: vk::SwapchainKHR,
        info: *const vk::LatencySleepInfoNV<'_>,
    ) -> vk::Result {
        guard(
            Feature::VkWrap,
            || vk::Result::ERROR_UNKNOWN,
            || {
                // SAFETY: Runtime passes a live initialized input struct.
                note(format!("sleep:{}", unsafe { (*info).value }));
                vk::Result::SUCCESS
            },
        )
    }
    unsafe extern "system" fn wait(
        _: vk::Device,
        _: *const vk::SemaphoreWaitInfo<'_>,
        timeout: u64,
    ) -> vk::Result {
        guard(
            Feature::VkWrap,
            || vk::Result::ERROR_UNKNOWN,
            || {
                note(format!("wait:{timeout}"));
                if timeout == u64::MAX {
                    vk::Result::SUCCESS
                } else {
                    vk::Result::TIMEOUT
                }
            },
        )
    }
    unsafe extern "system" fn timings(
        _: vk::Device,
        _: vk::SwapchainKHR,
        _: *mut vk::GetLatencyMarkerInfoNV<'_>,
    ) {
        guard(Feature::VkWrap, || (), || {})
    }
    fn fns() -> Fns {
        Fns {
            device: vk::Device::from_raw(1),
            create_semaphore: create,
            destroy_semaphore: destroy,
            set_mode: mode,
            marker,
            sleep,
            wait,
            timings,
        }
    }
    /// Fake successful wait records both the required timeout and frame progression.
    unsafe extern "system" fn wait_ok(
        _: vk::Device,
        _: *const vk::SemaphoreWaitInfo<'_>,
        timeout: u64,
    ) -> vk::Result {
        guard(
            Feature::VkWrap,
            || vk::Result::ERROR_UNKNOWN,
            || {
                note(format!("wait:{timeout}"));
                vk::Result::SUCCESS
            },
        )
    }
    /// A successful frame follows the exact issue order, and a newly known menu is applied once.
    #[test]
    fn successful_order_and_menu_transition() {
        let mut table = fns();
        table.wait = wait_ok;
        // SAFETY: all fake handles and functions belong to this test.
        let runtime =
            unsafe { Runtime::new(table, vk::SwapchainKHR::from_raw(2), ReflexMode::Game, None) }
                .unwrap();
        calls();
        let run = || {
            note("present".into());
            vk::Result::SUCCESS
        };
        let _ = runtime.present(Some(2), run, || panic!("must stay active"));
        assert_eq!(
            calls(),
            [
                "mode:2:1:1",
                "marker:1:4",
                "present",
                "marker:1:5",
                "sleep:1",
                "wait:10000000",
                "marker:2:0",
                "marker:2:2"
            ]
        );
        let _ = runtime.present(Some(2), run, || panic!("must stay active"));
        assert_eq!(
            calls(),
            [
                "marker:2:4",
                "present",
                "marker:2:5",
                "sleep:2",
                "wait:10000000",
                "marker:3:0",
                "marker:3:2"
            ]
        );
    }
    /// Lost/outdated presents preserve the result and stop before any sleep or wait.
    #[test]
    fn failed_present_disables_without_sleep() {
        for result in [
            vk::Result::ERROR_DEVICE_LOST,
            vk::Result::ERROR_OUT_OF_DATE_KHR,
        ] {
            // SAFETY: all fake handles belong to this fake table.
            let runtime = unsafe {
                Runtime::new(fns(), vk::SwapchainKHR::from_raw(2), ReflexMode::Auto, None)
            }
            .unwrap();
            calls();
            let stopped = std::cell::Cell::new(false);
            assert_eq!(
                runtime.present(None, || result, || stopped.set(true)),
                result
            );
            assert!(stopped.get());
            assert_eq!(calls(), ["marker:1:4", "marker:1:5"]);
            let _ = runtime.present(None, || vk::Result::SUCCESS, || panic!("already disabled"));
            assert!(calls().is_empty());
        }
    }
    /// Test-only panic injection is in Rust post-work, never across a native ABI boundary.
    pub(super) static PANIC_AFTER_PRESENT: AtomicBool = AtomicBool::new(false);
    /// A panic after the driver returned preserves its result and never invokes it twice.
    #[test]
    fn post_panic_preserves_original_once() {
        let name = "reflex::runtime_tests::post_panic_preserves_original_once";
        if std::env::var_os("W23_PANIC_CHILD").is_none() {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", name, "--nocapture"])
                .env("W23_PANIC_CHILD", "1")
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }
        let mut table = fns();
        table.wait = wait_ok;
        // SAFETY: fake handles are kept live until after on_present and retirement.
        let runtime =
            unsafe { Runtime::new(table, vk::SwapchainKHR::from_raw(2), ReflexMode::Auto, None) }
                .unwrap();
        replace_runtime(Box::into_raw(Box::new(runtime)));
        calls();
        PANIC_AFTER_PRESENT.store(true, Ordering::Relaxed);
        let swaps = [vk::SwapchainKHR::from_raw(2)];
        let info = vk::PresentInfoKHR::default().swapchains(&swaps);
        let count = std::cell::Cell::new(0);
        // SAFETY: info references one live fake swapchain and original returns a Vulkan result.
        let result = unsafe {
            on_present(
                &info,
                || {
                    count.set(count.get() + 1);
                    vk::Result::SUBOPTIMAL_KHR
                },
                || (),
            )
        };
        assert_eq!(result, vk::Result::SUBOPTIMAL_KHR);
        assert_eq!(count.get(), 1);
        assert!(crate::guard::is_disabled(Feature::Reflex));
        assert!(!ACTIVE.load(Ordering::SeqCst).is_null());
        assert_eq!(READERS.load(Ordering::SeqCst), 0);
        replace_runtime(std::ptr::null_mut());
    }
    /// Exact original pointer and borrowed tail observed by the swapchain driver boundary.
    static ORIGINAL_SWAP: AtomicUsize = AtomicUsize::new(0);
    static ORIGINAL_TAIL: AtomicUsize = AtomicUsize::new(0);
    static SWAP_ATTEMPTS: AtomicUsize = AtomicUsize::new(0);
    static REFUSE_SWAP: AtomicBool = AtomicBool::new(false);
    /// # Safety
    /// This fake receives live wrapper inputs and writable swapchain output.
    unsafe extern "system" fn create_swap(
        _: vk::Device,
        info: *const vk::SwapchainCreateInfoKHR<'_>,
        _: *const vk::AllocationCallbacks<'_>,
        out: *mut vk::SwapchainKHR,
    ) -> vk::Result {
        guard(
            Feature::VkWrap,
            || vk::Result::ERROR_UNKNOWN,
            || {
                let attempt = SWAP_ATTEMPTS.fetch_add(1, Ordering::Relaxed);
                if info as usize == ORIGINAL_SWAP.load(Ordering::Relaxed) {
                    note("swap:original".into());
                } else {
                    // SAFETY: the wrapper retains both the owned added node and its borrowed original tail.
                    let latency = unsafe {
                        &*(*info)
                            .p_next
                            .cast::<vk::SwapchainLatencyCreateInfoNV<'_>>()
                    };
                    assert!(latency.s_type == vk::StructureType::SWAPCHAIN_LATENCY_CREATE_INFO_NV);
                    assert_eq!(latency.latency_mode_enable, vk::TRUE);
                    assert_eq!(
                        latency.p_next as usize,
                        ORIGINAL_TAIL.load(Ordering::Relaxed)
                    );
                    note("swap:changed".into());
                }
                if attempt == 0 && REFUSE_SWAP.load(Ordering::Relaxed) {
                    return vk::Result::ERROR_INITIALIZATION_FAILED;
                }
                // SAFETY: the caller supplies this writable output handle.
                unsafe {
                    out.write(vk::SwapchainKHR::from_raw(2 + attempt as u64));
                }
                vk::Result::SUCCESS
            },
        )
    }
    /// The real swapchain wrapper preserves its original tail and retries the original pointer once.
    #[test]
    fn swapchain_wrapper_chains_rearms_and_retries_exact_original() {
        let name =
            "reflex::runtime_tests::swapchain_wrapper_chains_rearms_and_retries_exact_original";
        let Ok(case) = std::env::var("W23_SWAP_CHILD") else {
            for case in ["success", "refusal"] {
                let output = std::process::Command::new(std::env::current_exe().unwrap())
                    .args(["--exact", name, "--nocapture"])
                    .env("W23_SWAP_CHILD", case)
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
        init(ReflexMode::Auto);
        assert!(FNS.set(fns()).is_ok());
        DEVICE_LIVE.store(true, Ordering::Release);
        crate::vk::REAL_CREATE_SWAPCHAIN
            .store(create_swap as *const () as usize, Ordering::Release);
        let tail = vk::SwapchainPresentModesCreateInfoEXT::default();
        let info = vk::SwapchainCreateInfoKHR {
            p_next: (&tail as *const vk::SwapchainPresentModesCreateInfoEXT<'_>).cast(),
            ..Default::default()
        };
        ORIGINAL_SWAP.store(&info as *const _ as usize, Ordering::Relaxed);
        ORIGINAL_TAIL.store(info.p_next as usize, Ordering::Relaxed);
        REFUSE_SWAP.store(case == "refusal", Ordering::Relaxed);
        let mut out = vk::SwapchainKHR::null();
        // SAFETY: live fake create input, tail and output; the saved real function is the fake driver.
        assert_eq!(
            unsafe {
                crate::vk::wrap_create_swapchain(
                    vk::Device::from_raw(1),
                    &info,
                    std::ptr::null(),
                    &mut out,
                )
            },
            vk::Result::SUCCESS
        );
        assert_eq!(info.p_next as usize, ORIGINAL_TAIL.load(Ordering::Relaxed));
        assert!(tail.p_next.is_null());
        if case == "refusal" {
            assert_eq!(calls(), ["swap:changed", "swap:original"]);
            assert_eq!(SWAP_ATTEMPTS.load(Ordering::Relaxed), 2);
            assert!(crate::guard::is_disabled(Feature::Reflex));
            assert!(ACTIVE.load(Ordering::SeqCst).is_null());
        } else {
            assert_eq!(calls(), ["swap:changed", "mode:2:1:0"]);
            // SAFETY: same fake lifetime contract for the second swapchain creation.
            assert_eq!(
                unsafe {
                    crate::vk::wrap_create_swapchain(
                        vk::Device::from_raw(1),
                        &info,
                        std::ptr::null(),
                        &mut out,
                    )
                },
                vk::Result::SUCCESS
            );
            assert_eq!(calls(), ["swap:changed", "mode:3:1:0", "destroy"]);
            let reader = Reader::enter();
            assert_eq!(reader.runtime().unwrap().swapchain.as_raw(), 3);
            drop(reader);
            // SAFETY: retire before ending the fake device's lifetime.
            unsafe { before_destroy(vk::Device::from_raw(1), None) };
            assert_eq!(calls(), ["destroy"]);
            assert!(ACTIVE.load(Ordering::SeqCst).is_null());
        }
    }
    /// A native swapchain destructor observes semaphore cleanup before its one original call.
    unsafe extern "system" fn destroy_swap_after_cleanup(
        _: vk::Device,
        _: vk::SwapchainKHR,
        _: *const vk::AllocationCallbacks<'_>,
    ) {
        let body = || note("native-swap-destroy".into());
        guard(Feature::VkWrap, body, body);
    }
    /// The fake native resolver remains usable after the mod's optional wrapper feature is off.
    unsafe extern "system" fn teardown_gipa(
        _: vk::Instance,
        _: *const std::ffi::c_char,
    ) -> vk::PFN_vkVoidFunction {
        let body = || {
            // SAFETY: erasing only this actual Vulkan function pointer for the fake lookup.
            Some(unsafe {
                std::mem::transmute::<vk::PFN_vkDestroySwapchainKHR, unsafe extern "system" fn()>(
                    destroy_swap_after_cleanup,
                )
            })
        };
        guard(Feature::VkWrap, body, body)
    }
    /// Real disabled GIPA routing must retire the active record before destroying its swapchain.
    #[test]
    fn disabled_lookup_retires_reflex_semaphore_before_original_destroy() {
        const NAME: &str = "reflex::runtime_tests::disabled_lookup_retires_reflex_semaphore_before_original_destroy";
        if std::env::var_os("W23_RUNTIME_DESTROY").is_none() {
            let out = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", NAME, "--nocapture"])
                .env("W23_RUNTIME_DESTROY", "1")
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
            return;
        }
        // SAFETY: the owned record uses only this module's fake native table and live fake handles.
        let runtime =
            unsafe { Runtime::new(fns(), vk::SwapchainKHR::from_raw(2), ReflexMode::Auto, None) }
                .unwrap();
        replace_runtime(Box::into_raw(Box::new(runtime)));
        calls();
        crate::vk::REAL_GIPA.store(teardown_gipa as *const () as usize, Ordering::Release);
        crate::guard::disable(Feature::VkWrap, "test");
        // SAFETY: the fake lookup returns exactly the tested native swapchain destructor type.
        unsafe {
            let raw =
                crate::vk::wrap_gipa(vk::Instance::from_raw(3), c"vkDestroySwapchainKHR".as_ptr())
                    .unwrap();
            let destroy = std::mem::transmute::<
                unsafe extern "system" fn(),
                vk::PFN_vkDestroySwapchainKHR,
            >(raw);
            destroy(
                vk::Device::from_raw(1),
                vk::SwapchainKHR::from_raw(2),
                std::ptr::null(),
            );
        }
        assert!(ACTIVE.load(Ordering::SeqCst).is_null());
        assert_eq!(calls(), ["destroy", "native-swap-destroy"]);
    }
    /// Each new swapchain gets a fresh sleep-mode command from the current decision.
    #[test]
    fn rearm_on_new_swapchain() {
        // SAFETY: all fake handles belong to the fake table and remain live for each Runtime.
        unsafe {
            let first =
                Runtime::new(fns(), vk::SwapchainKHR::from_raw(2), ReflexMode::Auto, None).unwrap();
            let second = Runtime::new(
                fns(),
                vk::SwapchainKHR::from_raw(3),
                ReflexMode::Boost,
                None,
            )
            .unwrap();
            assert_eq!(calls(), ["mode:2:1:0", "mode:3:1:1"]);
            drop((first, second));
        }
        assert_eq!(calls(), ["destroy", "destroy"]);
    }
    /// Reentrant admission fails closed without a second sleep sequence or blocking the real call.
    #[test]
    fn reentrant_present_is_plain_and_disables_stream() {
        let mut table = fns();
        table.wait = wait_ok;
        // SAFETY: the fake functions own these fake handles for the entire test.
        let runtime =
            unsafe { Runtime::new(table, vk::SwapchainKHR::from_raw(2), ReflexMode::Auto, None) }
                .unwrap();
        calls();
        let originals = std::cell::Cell::new(0);
        let disables = std::cell::Cell::new(0);
        let disable = || disables.set(disables.get() + 1);
        let result = runtime.present(
            None,
            || {
                originals.set(originals.get() + 1);
                runtime.present(
                    None,
                    || {
                        originals.set(originals.get() + 1);
                        vk::Result::SUCCESS
                    },
                    disable,
                )
            },
            disable,
        );
        assert_eq!(result, vk::Result::SUCCESS);
        assert_eq!(originals.get(), 2);
        assert_eq!(disables.get(), 1);
        assert_eq!(calls(), ["marker:1:4", "marker:1:5"]);
        assert!(!runtime.presenting.load(Ordering::Acquire));
    }
    /// The first wait timeout stops this stream permanently, while every real present runs once.
    #[test]
    fn timeout_disables_reflex() {
        // SAFETY: the fake table accepts these fake handles.
        let runtime =
            unsafe { Runtime::new(fns(), vk::SwapchainKHR::from_raw(2), ReflexMode::Auto, None) }
                .unwrap();
        calls();
        let run = || {
            note("present".into());
            vk::Result::SUCCESS
        };
        let stopped = std::cell::Cell::new(0);
        assert_eq!(
            runtime.present(None, run, || stopped.set(stopped.get() + 1)),
            vk::Result::SUCCESS
        );
        assert_eq!(
            calls(),
            [
                "marker:1:4",
                "present",
                "marker:1:5",
                "sleep:1",
                "wait:10000000"
            ]
        );
        assert_eq!(stopped.get(), 1);
        assert_eq!(
            runtime.present(None, run, || stopped.set(stopped.get() + 1)),
            vk::Result::SUCCESS
        );
        assert_eq!(calls(), ["present"]);
        assert_eq!(stopped.get(), 1);
    }
}

/// One owned semaphore and immutable device/swapchain pair. Retirement waits for every reader.
struct Runtime {
    fns: Fns,
    swapchain: vk::SwapchainKHR,
    semaphore: vk::Semaphore,
    mode: ReflexMode,
    decision: AtomicUsize,
    menu_known: AtomicBool,
    stopped: AtomicBool,
    presenting: AtomicBool,
    frame: AtomicU64,
    pending: AtomicBool,
}

/// Release non-waiting present admission even when a caller unwinds.
struct Admission<'a>(&'a AtomicBool);
impl Drop for Admission<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

/// Encode off/on/boost in a single atomic value.
fn mode_value(decision: Option<bool>) -> usize {
    match decision {
        None => 0,
        Some(false) => 1,
        Some(true) => 2,
    }
}

impl Runtime {
    /// Own a timeline semaphore and arm the newly created swapchain.
    /// # Safety
    /// The device/swapchain remain live until this value is retired and dropped, and fns match it.
    unsafe fn new(
        fns: Fns,
        swapchain: vk::SwapchainKHR,
        mode: ReflexMode,
        menu: Option<u32>,
    ) -> Option<Self> {
        let mut kind = vk::SemaphoreTypeCreateInfo::default()
            .semaphore_type(vk::SemaphoreType::TIMELINE)
            .initial_value(0);
        let create = vk::SemaphoreCreateInfo::default().push_next(&mut kind);
        let mut semaphore = vk::Semaphore::null();
        // SAFETY: valid device and live initialized create/output structures. Null allocator is paired in Drop.
        if unsafe { (fns.create_semaphore)(fns.device, &create, std::ptr::null(), &mut semaphore) }
            != vk::Result::SUCCESS
        {
            return None;
        }
        let decision = mode_value(decide_mode(&mode, menu, true, true));
        let runtime = Self {
            fns,
            swapchain,
            semaphore,
            mode,
            decision: AtomicUsize::new(decision),
            menu_known: AtomicBool::new(menu.is_some()),
            stopped: AtomicBool::new(false),
            presenting: AtomicBool::new(false),
            frame: AtomicU64::new(1),
            pending: AtomicBool::new(false),
        };
        if runtime.set_mode(decision) != vk::Result::SUCCESS {
            return None;
        }
        Some(runtime)
    }

    /// Send the current mode; changing mode is allowed concurrently with non-destroy Vulkan calls.
    fn set_mode(&self, decision: usize) -> vk::Result {
        let info = vk::LatencySleepModeInfoNV::default()
            .low_latency_mode(decision != 0)
            .low_latency_boost(decision == 2)
            .minimum_interval_us(0);
        // SAFETY: this Runtime owns a live swapchain/device pair under its lifetime contract.
        unsafe { (self.fns.set_mode)(self.fns.device, self.swapchain, &info) }
    }

    /// Emit a marker with the mod's own frame ID, leaving the game's present-ID chain intact.
    fn marker(&self, frame: u64, marker: vk::LatencyMarkerNV) {
        let info = vk::SetLatencyMarkerInfoNV::default()
            .present_id(frame)
            .marker(marker);
        // SAFETY: this Runtime's handles and table stay live through the caller's reader lease.
        unsafe { (self.fns.marker)(self.fns.device, self.swapchain, &info) };
    }

    /// Stop once, invoking the caller's quiet global feature switch.
    fn stop(&self, disable: impl FnOnce()) {
        if !self.stopped.swap(true, Ordering::AcqRel) {
            disable();
        }
    }

    /// Present once with ordered markers and one bounded wait. No allocation, blocking lock or log.
    /// The outer wrapper catches panics and caches the original result before post-work.
    fn present(
        &self,
        menu: Option<u32>,
        original: impl Fn() -> vk::Result,
        disable: impl Fn(),
    ) -> vk::Result {
        if self.stopped.load(Ordering::Acquire) {
            return original();
        }
        if self
            .presenting
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            self.stop(&disable);
            return original();
        }
        let _admission = Admission(&self.presenting);
        if self.stopped.load(Ordering::Acquire) {
            return original();
        }
        if menu.is_some() && !self.menu_known.swap(true, Ordering::Relaxed) {
            let decision = mode_value(decide_mode(&self.mode, menu, true, true));
            if decision != self.decision.load(Ordering::Relaxed) {
                if self.set_mode(decision) != vk::Result::SUCCESS {
                    self.stop(&disable);
                    return original();
                }
                self.decision.store(decision, Ordering::Relaxed);
            }
        }
        if self.decision.load(Ordering::Relaxed) == 0 {
            return original();
        }
        let frame = self.frame.load(Ordering::Relaxed);
        self.marker(frame, vk::LatencyMarkerNV::PRESENT_START);
        let result = original();
        #[cfg(test)]
        if runtime_tests::PANIC_AFTER_PRESENT.load(Ordering::Relaxed) {
            panic!("test post-present panic");
        }
        self.marker(frame, vk::LatencyMarkerNV::PRESENT_END);
        if result == vk::Result::ERROR_DEVICE_LOST
            || result == vk::Result::ERROR_OUT_OF_DATE_KHR
            || self.stopped.load(Ordering::Acquire)
        {
            self.stop(&disable);
            return result;
        }
        let sleep = vk::LatencySleepInfoNV::default()
            .signal_semaphore(self.semaphore)
            .value(frame);
        // SAFETY: the owned semaphore is timeline-enabled, and frame strictly increases.
        if unsafe { (self.fns.sleep)(self.fns.device, self.swapchain, &sleep) }
            != vk::Result::SUCCESS
        {
            self.stop(&disable);
            return result;
        }
        self.pending.store(true, Ordering::Release);
        let semaphores = [self.semaphore];
        let values = [frame];
        let wait = vk::SemaphoreWaitInfo::default()
            .semaphores(&semaphores)
            .values(&values);
        // SAFETY: live timeline semaphore and stack arrays, bounded timeout in nanoseconds.
        let waited = unsafe { (self.fns.wait)(self.fns.device, &wait, 10_000_000) };
        if waited != vk::Result::SUCCESS {
            if waited == vk::Result::ERROR_DEVICE_LOST {
                self.pending.store(false, Ordering::Release);
            }
            self.stop(&disable);
            return result;
        }
        self.pending.store(false, Ordering::Release);
        let Some(next) = frame.checked_add(1) else {
            self.stop(&disable);
            return result;
        };
        self.frame.store(next, Ordering::Relaxed);
        if !self.stopped.load(Ordering::Acquire) {
            self.marker(next, vk::LatencyMarkerNV::SIMULATION_START);
            self.marker(next, vk::LatencyMarkerNV::RENDERSUBMIT_START);
        }
        result
    }
}

impl Drop for Runtime {
    fn drop(&mut self) {
        if self.pending.load(Ordering::Acquire) {
            let semaphores = [self.semaphore];
            let values = [self.frame.load(Ordering::Relaxed)];
            let wait = vk::SemaphoreWaitInfo::default()
                .semaphores(&semaphores)
                .values(&values);
            // SAFETY: retirement has drained every reader; the semaphore and device remain live.
            // A pending driver signal must finish before destruction. This is never the present path.
            loop {
                // SAFETY: the retirement contract above keeps the semaphore live until signaled.
                match unsafe { (self.fns.wait)(self.fns.device, &wait, u64::MAX) } {
                    vk::Result::SUCCESS | vk::Result::ERROR_DEVICE_LOST => break,
                    _ => std::thread::yield_now(),
                }
            }
        }
        // SAFETY: no reader remains and pending signaling has completed (or the device is lost).
        unsafe { (self.fns.destroy_semaphore)(self.fns.device, self.semaphore, std::ptr::null()) };
    }
}

/// Published record and reader count use a single SeqCst order across both atomics.
static ACTIVE: AtomicPtr<Runtime> = AtomicPtr::new(std::ptr::null_mut());
/// A reader increments before loading ACTIVE; retirement detaches before observing zero readers.
static READERS: AtomicUsize = AtomicUsize::new(0);
/// Lifecycle writers serialize outside the frame path, to keep replacements from racing destruction.
static LIFECYCLE: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Keeps a native record alive while present or latency_report uses it.
struct Reader;
impl Reader {
    /// Enter before loading ACTIVE. SeqCst prevents a stale pointer/new-zero cross-atomic race.
    fn enter() -> Self {
        READERS.fetch_add(1, Ordering::SeqCst);
        Self
    }
    /// Borrow a record for at most this lease's lifetime.
    fn runtime(&self) -> Option<&Runtime> {
        let pointer = ACTIVE.load(Ordering::SeqCst);
        // SAFETY: this reader increment precedes the pointer load. A retiring writer first detaches
        // the pointer, then waits for zero readers in the same total order before freeing it.
        unsafe { pointer.as_ref() }
    }
}
impl Drop for Reader {
    fn drop(&mut self) {
        READERS.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Detach and reclaim a record. Caller holds LIFECYCLE; no new reader can reach the old record.
fn replace_runtime(new: *mut Runtime) {
    let old = ACTIVE.swap(new, Ordering::SeqCst);
    if old.is_null() {
        return;
    }
    while READERS.load(Ordering::SeqCst) != 0 {
        std::thread::yield_now();
    }
    // SAFETY: old came from Box::into_raw exactly once and no reader can now access it.
    drop(unsafe { Box::from_raw(old) });
}

/// Modify a swapchain create call without modifying its borrowed chain.
/// # Safety
/// Arguments and real entry point obey vkCreateSwapchainKHR's contract.
pub unsafe fn create_swapchain(
    real: vk::PFN_vkCreateSwapchainKHR,
    device: vk::Device,
    info: *const vk::SwapchainCreateInfoKHR<'_>,
    allocator: *const vk::AllocationCallbacks<'_>,
    out: *mut vk::SwapchainKHR,
) -> vk::Result {
    let completed = std::cell::Cell::new(None);
    let original = || {
        if let Some(result) = completed.get() {
            return result;
        }
        // SAFETY: original caller arguments, unchanged.
        let result = unsafe { real(device, info, allocator, out) };
        completed.set(Some(result));
        result
    };
    guard(Feature::Reflex, original, || {
        let Some(fns) = FNS.get().filter(|f| f.device == device) else {
            return original();
        };
        if !DEVICE_LIVE.load(Ordering::Acquire) || info.is_null() {
            return original();
        }
        // SAFETY: info is the caller's valid immutable input.
        let mut changed = unsafe { info.read() };
        let mut cursor = changed.p_next.cast::<vk::BaseInStructure<'_>>();
        let mut visited = Vec::new();
        while !cursor.is_null() {
            if visited.len() >= 64 || visited.contains(&cursor) {
                return original();
            }
            visited.push(cursor);
            // SAFETY: read only the Vulkan common header, whose layout is known even for opaque nodes.
            let header = unsafe { &*cursor };
            // Do not duplicate or alter an existing latency node; preserve the application's choice.
            if header.s_type == vk::StructureType::SWAPCHAIN_LATENCY_CREATE_INFO_NV {
                return original();
            }
            cursor = header.p_next;
        }
        let mut latency = vk::SwapchainLatencyCreateInfoNV::default().latency_mode_enable(true);
        latency.p_next = changed.p_next;
        changed.p_next = (&latency as *const vk::SwapchainLatencyCreateInfoNV<'_>).cast();
        // SAFETY: a local added node borrows the original unmodified tail; all live through the call.
        let result = unsafe { real(device, &changed, allocator, out) };
        if result != vk::Result::SUCCESS {
            crate::guard::disable(Feature::Reflex, "driver refused");
            return original();
        }
        completed.set(Some(result));
        guard(
            Feature::Reflex,
            || (),
            || {
                let Ok(_writer) = LIFECYCLE.lock() else {
                    return;
                };
                let Some(mode) = MODE.get().copied() else {
                    return;
                };
                // SAFETY: successful native creation initialized out; writer retains the live handles.
                match unsafe { Runtime::new(*fns, out.read(), mode, game_menu()) } {
                    Some(runtime) => replace_runtime(Box::into_raw(Box::new(runtime))),
                    None => crate::guard::disable(Feature::Reflex, "could not arm swapchain"),
                }
            },
        );
        result
    })
}

/// Retire resources before destroying their device or swapchain, including after feature disable.
/// # Safety
/// This is called synchronously before the real destroy while its handles remain live. The game
/// obeys Vulkan's external synchronization rules for create/present/destroy on these objects.
pub unsafe fn before_destroy(device: vk::Device, swapchain: Option<vk::SwapchainKHR>) {
    let _writer = LIFECYCLE
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    if swapchain.is_none() && FNS.get().is_some_and(|f| f.device == device) {
        DEVICE_LIVE.store(false, Ordering::Release);
    }
    let matches = {
        let reader = Reader::enter();
        reader
            .runtime()
            .is_some_and(|r| r.fns.device == device && swapchain.is_none_or(|s| s == r.swapchain))
    };
    if matches {
        replace_runtime(std::ptr::null_mut());
    }
}

/// Run Reflex around exactly one real present. The original result is cached before post-work.
/// # Safety
/// `info` obeys vkQueuePresentKHR's contract and `original` forwards that exact call. `disable`
/// must quietly switch off Feature::Reflex without blocking or allocating.
pub unsafe fn on_present(
    info: *const vk::PresentInfoKHR<'_>,
    original: impl Fn() -> vk::Result,
    disable: impl Fn(),
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
    guard(Feature::Reflex, once, || {
        if info.is_null() {
            return once();
        }
        let reader = Reader::enter();
        let Some(runtime) = reader.runtime() else {
            return once();
        };
        // SAFETY: the input is live throughout this wrapper, and the array has swapchain_count entries.
        let info = unsafe { &*info };
        if info.swapchain_count != 1 || info.p_swapchains.is_null() {
            return once();
        }
        // SAFETY: count is exactly one and Vulkan requires a valid array.
        if unsafe { info.p_swapchains.read() } != runtime.swapchain {
            return once();
        }
        runtime.present(game_menu(), once, disable)
    })
}

/// Read current latency timings and return averages. This function is never scheduled here.
/// It may allocate; callers must use a background thread. The reader lease protects handles
/// against wrapped teardown. Missing/zero timestamps are excluded instead of inventing samples.
pub fn latency_report() -> Option<String> {
    guard(
        Feature::Reflex,
        || None,
        || {
            let reader = Reader::enter();
            let runtime = reader.runtime()?;
            if runtime.stopped.load(Ordering::Acquire) {
                return None;
            }
            let mut reports = [vk::LatencyTimingsFrameReportNV::default(); 64];
            let mut query = vk::GetLatencyMarkerInfoNV {
                timing_count: reports.len() as u32,
                p_timings: reports.as_mut_ptr(),
                ..Default::default()
            };
            // SAFETY: the lease keeps the device and swapchain live, and all 64 output structs are initialized.
            unsafe { (runtime.fns.timings)(runtime.fns.device, runtime.swapchain, &mut query) };
            let mut total = 0u128;
            let mut count = 0u32;
            let mut present_total = 0u128;
            let mut present_count = 0u32;
            for report in reports.get(..usize::min(query.timing_count as usize, reports.len()))? {
                if report.sim_start_time_us != 0 {
                    if let Some(delta) = report
                        .gpu_render_end_time_us
                        .checked_sub(report.sim_start_time_us)
                    {
                        total = total.saturating_add(u128::from(delta));
                        count = count.saturating_add(1);
                    }
                }
                if report.present_start_time_us != 0 {
                    if let Some(delta) = report
                        .present_end_time_us
                        .checked_sub(report.present_start_time_us)
                    {
                        present_total = present_total.saturating_add(u128::from(delta));
                        present_count = present_count.saturating_add(1);
                    }
                }
            }
            if count == 0 && present_count == 0 {
                return None;
            }
            Some(format!("Reflex: simulation to GPU end {:.3} ms ({} samples), present call {:.3} ms ({} samples); display timestamp unavailable",
            total as f64 / f64::from(count.max(1)) / 1000.0, count,
            present_total as f64 / f64::from(present_count.max(1)) / 1000.0, present_count))
        },
    )
}
