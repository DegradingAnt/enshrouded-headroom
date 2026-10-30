//! Driver memory-budget queries for the governor.
//!
//! Primary Vulkan contracts:
//! - <https://docs.vulkan.org/refpages/latest/refpages/source/VkPhysicalDeviceMemoryBudgetPropertiesEXT.html>
//! - <https://docs.vulkan.org/refpages/latest/refpages/source/vkGetPhysicalDeviceMemoryProperties2.html>
//! - <https://docs.vulkan.org/refpages/latest/refpages/source/vkEnumerateDeviceExtensionProperties.html>
//! - <https://docs.vulkan.org/spec/latest/chapters/initialization.html>

use ash::vk;

/// Query capability proven by the caller from instance creation and physical-device metadata.
/// A non-null loader address is never capability proof.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueryRoute {
    /// No permitted route was established; the query returns unknown without calling the driver.
    Unknown,
    /// Both the instance's effective API version and the physical device support Vulkan1.1 or later.
    Core11,
    /// VK_KHR_get_physical_device_properties2 was enabled when this instance was created.
    EnabledKhr,
}

/// Budget minus usage in MiB for the largest device-local heap; malformed counts are unknown.
fn select_free_mib(
    memory: &vk::PhysicalDeviceMemoryProperties,
    budget: &vk::PhysicalDeviceMemoryBudgetPropertiesEXT<'_>,
) -> Option<u64> {
    let heaps = memory
        .memory_heaps
        .get(..usize::try_from(memory.memory_heap_count).ok()?)?;
    let (index, _) = heaps
        .iter()
        .enumerate()
        .filter(|(_, heap)| heap.flags.contains(vk::MemoryHeapFlags::DEVICE_LOCAL))
        .max_by_key(|(_, heap)| heap.size)?;
    Some(
        budget
            .heap_budget
            .get(index)?
            .saturating_sub(*budget.heap_usage.get(index)?)
            / (1024 * 1024),
    )
}

/// A typed command prepared for one protected instance/physical-device lifetime.
/// No native handle or budget value is retained here.
pub(crate) struct BudgetQuery {
    /// The exact core or enabled-KHR function selected by proven capabilities.
    query: vk::PFN_vkGetPhysicalDeviceMemoryProperties2,
}

impl BudgetQuery {
    /// Read a new dynamic budget under the same lifetime used for preparation.
    /// # Safety
    /// The original instance and matching physical device remain live under their synchronous
    /// lease. This command is never reused for another generation or queried after retirement.
    pub(crate) unsafe fn sample(&self, phys: vk::PhysicalDevice) -> Option<u64> {
        let mut budget = vk::PhysicalDeviceMemoryBudgetPropertiesEXT::default();
        let mut memory = vk::PhysicalDeviceMemoryProperties2::default().push_next(&mut budget);
        // SAFETY: the caller's live matching lease protects the command, device and stack outputs.
        unsafe { (self.query)(phys, &mut memory) };
        select_free_mib(&memory.memory_properties, &budget)
    }
}

/// Resolve immutable budget capability once, without retaining handles or reading a budget.
/// # Safety
/// The same loader, matching-live-pair, proven-route and synchronization requirements as
/// free_mib apply. Every later sample must hold that exact original lifetime's lease.
pub(crate) unsafe fn prepare(
    gipa: vk::PFN_vkGetInstanceProcAddr,
    instance: vk::Instance,
    phys: vk::PhysicalDevice,
    route: QueryRoute,
) -> Option<BudgetQuery> {
    let query_name = match route {
        QueryRoute::Unknown => return None,
        QueryRoute::Core11 => c"vkGetPhysicalDeviceMemoryProperties2",
        QueryRoute::EnabledKhr => c"vkGetPhysicalDeviceMemoryProperties2KHR",
    };
    // SAFETY: the caller supplies a live matching instance and loader; static names are C strings.
    let enumerate = unsafe { gipa(instance, c"vkEnumerateDeviceExtensionProperties".as_ptr()) }?;
    // SAFETY: the queried name determines this exact Vulkan function type.
    let enumerate: vk::PFN_vkEnumerateDeviceExtensionProperties =
        unsafe { std::mem::transmute(enumerate) };
    // A fixed upper bound avoids allocating in the governor loop. An unusually larger or
    // concurrently growing list returns INCOMPLETE and is conservatively unknown.
    let mut properties = [vk::ExtensionProperties::default(); 512];
    let mut count = 512u32;
    // SAFETY: live physical device; output has count entries and the count pointer is writable.
    let result = unsafe { enumerate(phys, std::ptr::null(), &mut count, properties.as_mut_ptr()) };
    if result != vk::Result::SUCCESS {
        return None;
    }
    let listed = properties.get(..usize::try_from(count).ok()?)?;
    let supported = listed.iter().any(|property| {
        property
            .extension_name_as_c_str()
            .is_ok_and(|name| name == c"VK_EXT_memory_budget")
    });
    if !supported {
        return None;
    }
    // SAFETY: the caller proved this exact route for the live instance/physical-device pair.
    // Resolving an address does not supply that proof and cannot authorize another route.
    let query = unsafe { gipa(instance, query_name.as_ptr()) }?;
    // SAFETY: the exact entry-point name specifies this type, including the KHR alias.
    let query: vk::PFN_vkGetPhysicalDeviceMemoryProperties2 = unsafe { std::mem::transmute(query) };
    Some(BudgetQuery { query })
}

/// Query the driver's advertised memory budget without allocating.
///
/// The largest DEVICE_LOCAL heap supplies budget minus usage, saturated at zero, in MiB.
/// An unproven route, unsupported extension, missing selected entry point or refused/incomplete
/// enumeration is unknown. Only the caller-proven core or KHR command is resolved; a non-null
/// address never establishes support and no fallback to another route is attempted.
/// Sources: Khronos vkEnumerateDeviceExtensionProperties, vkGetPhysicalDeviceMemoryProperties2
/// and VkPhysicalDeviceMemoryBudgetPropertiesEXT reference pages.
///
/// # Safety
///
/// `gipa` is the real Vulkan loader entry point. `instance` and `phys` are a matching live
/// instance/physical-device pair and stay alive throughout this call; their destruction must
/// not race this query. The driver honors the Vulkan ABI and output-buffer contracts.
///
/// `route` must reflect capabilities observed for these exact handles:
/// - `Core11`: the API version used to create this instance AND this physical device's reported
///   API version are both at least Vulkan1.1, permitting the core physical-device query.
/// - `EnabledKhr`: VK_KHR_get_physical_device_properties2 was enabled in this instance's
///   creation info, permitting the KHR query, even when the core route is not available.
/// - `Unknown`: no query route has been proven; no Vulkan call is made.
///
/// A loader version, a live device lease, an advertised memory-budget extension, or a non-null
/// function pointer alone proves neither route. Do not retain handles beyond their valid lease.
pub unsafe fn free_mib(
    gipa: vk::PFN_vkGetInstanceProcAddr,
    instance: vk::Instance,
    phys: vk::PhysicalDevice,
    route: QueryRoute,
) -> Option<u64> {
    // SAFETY: this function's contract supplies the matching live pair and proven route.
    let query = unsafe { prepare(gipa, instance, phys, route) }?;
    // SAFETY: the same synchronous lease remains held through this fresh dynamic sample.
    unsafe { query.sample(phys) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::guard::{guard, Feature};
    use ash::vk::Handle;
    use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

    static SUPPORTED: AtomicBool = AtomicBool::new(false);
    static QUERIES: AtomicU32 = AtomicU32::new(0);

    /// Return the original fixture extension advertisement.
    /// # Safety
    /// The caller supplies a writable count and at least one writable extension slot.
    unsafe extern "system" fn fake_enumerate(
        _: vk::PhysicalDevice,
        _: *const std::ffi::c_char,
        count: *mut u32,
        properties: *mut vk::ExtensionProperties,
    ) -> vk::Result {
        guard(
            Feature::Governor,
            || vk::Result::ERROR_UNKNOWN,
            || {
                // SAFETY: free_mib supplies valid count and a writable extension buffer.
                unsafe {
                    *count = u32::from(SUPPORTED.load(Ordering::Relaxed));
                    if *count == 1 {
                        let mut extension = vk::ExtensionProperties::default();
                        for (to, from) in extension
                            .extension_name
                            .iter_mut()
                            .zip(c"VK_EXT_memory_budget".to_bytes_with_nul())
                        {
                            *to = *from as std::ffi::c_char;
                        }
                        properties.write(extension);
                    }
                }
                vk::Result::SUCCESS
            },
        )
    }

    /// Fill the original fixture budget and record its query.
    /// # Safety
    /// The output is writable properties2 chained to a writable budget struct.
    unsafe extern "system" fn fake_memory(
        _: vk::PhysicalDevice,
        memory: *mut vk::PhysicalDeviceMemoryProperties2<'_>,
    ) {
        guard(
            Feature::Governor,
            || (),
            || {
                QUERIES.fetch_add(1, Ordering::Relaxed);
                // SAFETY: free_mib provides writable properties chained to a budget struct of this type.
                unsafe {
                    (*memory).memory_properties.memory_heap_count = 1;
                    (*memory).memory_properties.memory_heaps[0] = vk::MemoryHeap {
                        size: 8 << 30,
                        flags: vk::MemoryHeapFlags::DEVICE_LOCAL,
                    };
                    let budget = &mut *((*memory)
                        .p_next
                        .cast::<vk::PhysicalDeviceMemoryBudgetPropertiesEXT<'_>>());
                    budget.heap_budget[0] = 7 << 30;
                    budget.heap_usage[0] = 5 << 30;
                }
            },
        )
    }

    /// Resolve the original fixture native commands.
    /// # Safety
    /// The name points to a live NUL-terminated command string.
    unsafe extern "system" fn fake_gipa(
        _: vk::Instance,
        name: *const std::ffi::c_char,
    ) -> vk::PFN_vkVoidFunction {
        guard(
            Feature::Governor,
            || None,
            || {
                // SAFETY: free_mib passes a static nul-terminated Vulkan entry-point name.
                match unsafe { std::ffi::CStr::from_ptr(name) }.to_bytes() {
                    b"vkEnumerateDeviceExtensionProperties" => {
                        // SAFETY: Vulkan represents all entry points as vkVoidFunction until typed by name.
                        Some(unsafe {
                            std::mem::transmute::<
                                vk::PFN_vkEnumerateDeviceExtensionProperties,
                                unsafe extern "system" fn(),
                            >(fake_enumerate)
                        })
                    }
                    b"vkGetPhysicalDeviceMemoryProperties2" => {
                        // SAFETY: the caller casts this function back according to the exact queried name.
                        Some(unsafe {
                            std::mem::transmute::<
                                vk::PFN_vkGetPhysicalDeviceMemoryProperties2,
                                unsafe extern "system" fn(),
                            >(fake_memory)
                        })
                    }
                    _ => None,
                }
            },
        )
    }

    /// Every callback honors the disabled feature before touching caller outputs or counters.
    #[test]
    fn native_callbacks_return_fallbacks_when_disabled() {
        const TEST: &str = "vram_budget::tests::native_callbacks_return_fallbacks_when_disabled";
        if crate::patch::test_support::isolated_process(TEST, "HEADROOM_VRAM_GUARD_PARENT") {
            return;
        }
        crate::guard::disable_silent(crate::guard::Feature::Governor);
        SUPPORTED.store(true, Ordering::Relaxed);
        let physical = vk::PhysicalDevice::from_raw(2);
        let instance = vk::Instance::from_raw(1);
        let mut count = 7;
        let mut extension = vk::ExtensionProperties {
            spec_version: 99,
            ..Default::default()
        };
        // SAFETY: the isolated child owns all native output slots and static lookup names.
        unsafe {
            assert_eq!(
                fake_enumerate(physical, std::ptr::null(), &mut count, &mut extension),
                vk::Result::ERROR_UNKNOWN
            );
            assert_eq!(count, 7);
            assert_eq!(extension.spec_version, 99);
            assert_eq!(
                route_enumerate(physical, std::ptr::null(), &mut count, &mut extension),
                vk::Result::ERROR_UNKNOWN
            );
            assert_eq!(count, 7);
            assert_eq!(extension.spec_version, 99);
            let mut budget = vk::PhysicalDeviceMemoryBudgetPropertiesEXT::default();
            let mut memory = vk::PhysicalDeviceMemoryProperties2::default().push_next(&mut budget);
            memory.memory_properties.memory_heap_count = 7;
            fake_memory(physical, &mut memory);
            unsupported_core(physical, &mut memory);
            supported_khr(physical, &mut memory);
            assert_eq!(memory.memory_properties.memory_heap_count, 7);
            assert_eq!(budget.heap_budget[0], 0);
            assert_eq!(QUERIES.load(Ordering::Relaxed), 0);
            assert_eq!(CORE_CALLS.with(std::cell::Cell::get), 0);
            assert_eq!(KHR_CALLS.with(std::cell::Cell::get), 0);
            assert!(
                fake_gipa(instance, c"vkEnumerateDeviceExtensionProperties".as_ptr()).is_none()
            );
            assert!(
                route_gipa(instance, c"vkEnumerateDeviceExtensionProperties".as_ptr()).is_none()
            );
            assert!(
                missing_khr_gipa(instance, c"vkEnumerateDeviceExtensionProperties".as_ptr())
                    .is_none()
            );
            assert_eq!(ROUTE_LOOKUPS.with(std::cell::Cell::get), 0);
        }
    }

    #[test]
    fn driver_query_requires_advertised_extension_and_fills_the_budget_chain() {
        let instance = vk::Instance::from_raw(1);
        let physical = vk::PhysicalDevice::from_raw(2);
        SUPPORTED.store(false, Ordering::Relaxed);
        QUERIES.store(0, Ordering::Relaxed);
        // SAFETY: these synthetic handles are only passed to our matching fake driver functions.
        assert_eq!(
            unsafe { free_mib(fake_gipa, instance, physical, QueryRoute::Core11) },
            None
        );
        assert_eq!(QUERIES.load(Ordering::Relaxed), 0);
        SUPPORTED.store(true, Ordering::Relaxed);
        // SAFETY: the same fake driver owns every pointer the query uses.
        assert_eq!(
            unsafe { free_mib(fake_gipa, instance, physical, QueryRoute::Core11) },
            Some(2048)
        );
        assert_eq!(QUERIES.load(Ordering::Relaxed), 1);
    }

    thread_local! {
        /// These synchronous fake calls stay on the querying test thread.
        static CORE_CALLS: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
        static KHR_CALLS: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
        static ROUTE_LOOKUPS: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
    }

    /// Advertise the budget extension independently of the original test's toggled fake.
    /// # Safety
    /// The caller supplies a writable count and at least one writable extension slot.
    unsafe extern "system" fn route_enumerate(
        _: vk::PhysicalDevice,
        _: *const std::ffi::c_char,
        count: *mut u32,
        properties: *mut vk::ExtensionProperties,
    ) -> vk::Result {
        guard(
            Feature::Governor,
            || vk::Result::ERROR_UNKNOWN,
            || {
                let mut extension = vk::ExtensionProperties::default();
                for (to, from) in extension
                    .extension_name
                    .iter_mut()
                    .zip(c"VK_EXT_memory_budget".to_bytes_with_nul())
                {
                    *to = *from as std::ffi::c_char;
                }
                // SAFETY: free_mib supplies a writable count and at least one extension slot.
                unsafe {
                    *count = 1;
                    properties.write(extension);
                }
                vk::Result::SUCCESS
            },
        )
    }

    /// Fill the same synthetic budget without touching another test's observation counters.
    /// # Safety
    /// The output is writable properties2 chained to a writable budget struct.
    unsafe fn route_memory(memory: *mut vk::PhysicalDeviceMemoryProperties2<'_>) {
        // SAFETY: the query owns the writable properties and chained budget for this call.
        unsafe {
            (*memory).memory_properties.memory_heap_count = 1;
            (*memory).memory_properties.memory_heaps[0] = vk::MemoryHeap {
                size: 8 << 30,
                flags: vk::MemoryHeapFlags::DEVICE_LOCAL,
            };
            let budget = &mut *((*memory)
                .p_next
                .cast::<vk::PhysicalDeviceMemoryBudgetPropertiesEXT<'_>>());
            budget.heap_budget[0] = 7 << 30;
            budget.heap_usage[0] = 5 << 30;
        }
    }

    /// Deliberately non-null even when the caller has proven only the KHR route.
    /// # Safety
    /// The output is writable properties2 chained to a writable budget struct.
    unsafe extern "system" fn unsupported_core(
        _: vk::PhysicalDevice,
        memory: *mut vk::PhysicalDeviceMemoryProperties2<'_>,
    ) {
        guard(
            Feature::Governor,
            || (),
            || {
                CORE_CALLS.with(|n| n.set(n.get() + 1));
                // SAFETY: free_mib owns these output buffers; counting makes a forbidden call observable.
                unsafe { route_memory(memory) };
            },
        )
    }

    /// The proven enabled-KHR implementation has a distinct observable entry point.
    /// # Safety
    /// The output is writable properties2 chained to a writable budget struct.
    unsafe extern "system" fn supported_khr(
        _: vk::PhysicalDevice,
        memory: *mut vk::PhysicalDeviceMemoryProperties2<'_>,
    ) {
        guard(
            Feature::Governor,
            || (),
            || {
                KHR_CALLS.with(|n| n.set(n.get() + 1));
                // SAFETY: free_mib owns these output buffers.
                unsafe { route_memory(memory) };
            },
        )
    }

    /// Return both addresses, so address presence cannot stand in for capability proof.
    /// # Safety
    /// The name points to a live NUL-terminated command string.
    unsafe extern "system" fn route_gipa(
        _: vk::Instance,
        name: *const std::ffi::c_char,
    ) -> vk::PFN_vkVoidFunction {
        guard(
            Feature::Governor,
            || None,
            || {
                ROUTE_LOOKUPS.with(|n| n.set(n.get() + 1));
                // SAFETY: free_mib passes a static NUL-terminated entry-point name.
                let name = unsafe { std::ffi::CStr::from_ptr(name) }.to_bytes();
                match name {
                    b"vkEnumerateDeviceExtensionProperties" => {
                        // SAFETY: Vulkan erases function types until the exact name is resolved.
                        Some(unsafe {
                            std::mem::transmute::<
                                vk::PFN_vkEnumerateDeviceExtensionProperties,
                                unsafe extern "system" fn(),
                            >(route_enumerate)
                        })
                    }
                    b"vkGetPhysicalDeviceMemoryProperties2" => {
                        // SAFETY: the caller recovers this exact signature from the name.
                        Some(unsafe {
                            std::mem::transmute::<
                                vk::PFN_vkGetPhysicalDeviceMemoryProperties2,
                                unsafe extern "system" fn(),
                            >(unsupported_core)
                        })
                    }
                    b"vkGetPhysicalDeviceMemoryProperties2KHR" => {
                        // SAFETY: the KHR alias has this identical signature.
                        Some(unsafe {
                            std::mem::transmute::<
                                vk::PFN_vkGetPhysicalDeviceMemoryProperties2,
                                unsafe extern "system" fn(),
                            >(supported_khr)
                        })
                    }
                    _ => None,
                }
            },
        )
    }

    /// Missing KHR must not fall back to an unproven non-null core address.
    /// # Safety
    /// The name points to a live NUL-terminated command string.
    unsafe extern "system" fn missing_khr_gipa(
        instance: vk::Instance,
        name: *const std::ffi::c_char,
    ) -> vk::PFN_vkVoidFunction {
        guard(
            Feature::Governor,
            || None,
            || {
                // SAFETY: free_mib supplies a static NUL-terminated name.
                if unsafe { std::ffi::CStr::from_ptr(name) }
                    == c"vkGetPhysicalDeviceMemoryProperties2KHR"
                {
                    return None;
                }
                // SAFETY: forwards the same synthetic instance/name to the matching fake.
                unsafe { route_gipa(instance, name) }
            },
        )
    }

    #[test]
    fn khr_only_does_not_call_a_non_null_unsupported_core_pointer() {
        // SAFETY: these handles/route are capabilities of this synchronous fake driver only.
        assert_eq!(
            unsafe {
                free_mib(
                    route_gipa,
                    vk::Instance::from_raw(1),
                    vk::PhysicalDevice::from_raw(2),
                    QueryRoute::EnabledKhr,
                )
            },
            Some(2048)
        );
        assert_eq!(CORE_CALLS.with(std::cell::Cell::get), 0);
        assert_eq!(KHR_CALLS.with(std::cell::Cell::get), 1);
    }

    #[test]
    fn unproven_route_is_unknown_without_resolving_or_calling_driver_addresses() {
        // SAFETY: the synthetic matching driver is valid, but no query capability is claimed.
        assert_eq!(
            unsafe {
                free_mib(
                    route_gipa,
                    vk::Instance::from_raw(1),
                    vk::PhysicalDevice::from_raw(2),
                    QueryRoute::Unknown,
                )
            },
            None
        );
        assert_eq!(ROUTE_LOOKUPS.with(std::cell::Cell::get), 0);
        assert_eq!(CORE_CALLS.with(std::cell::Cell::get), 0);
        assert_eq!(KHR_CALLS.with(std::cell::Cell::get), 0);
    }

    #[test]
    fn missing_proven_khr_entry_point_is_unknown_without_core_fallback() {
        // SAFETY: this fake claims enabled KHR but deliberately omits its entry point.
        assert_eq!(
            unsafe {
                free_mib(
                    missing_khr_gipa,
                    vk::Instance::from_raw(1),
                    vk::PhysicalDevice::from_raw(2),
                    QueryRoute::EnabledKhr,
                )
            },
            None
        );
        assert_eq!(CORE_CALLS.with(std::cell::Cell::get), 0);
        assert_eq!(KHR_CALLS.with(std::cell::Cell::get), 0);
    }

    #[test]
    fn proven_core_route_uses_only_the_core_entry_point() {
        // SAFETY: this fake route models both instance and physical-device core1.1 support.
        assert_eq!(
            unsafe {
                free_mib(
                    route_gipa,
                    vk::Instance::from_raw(1),
                    vk::PhysicalDevice::from_raw(2),
                    QueryRoute::Core11,
                )
            },
            Some(2048)
        );
        assert_eq!(CORE_CALLS.with(std::cell::Cell::get), 1);
        assert_eq!(KHR_CALLS.with(std::cell::Cell::get), 0);
    }

    #[test]
    fn free_space_uses_the_largest_device_local_heap_and_saturates() {
        let mut memory = vk::PhysicalDeviceMemoryProperties {
            memory_heap_count: 3,
            ..Default::default()
        };
        memory.memory_heaps[0] = vk::MemoryHeap {
            size: 32 << 30,
            flags: vk::MemoryHeapFlags::empty(),
        };
        memory.memory_heaps[1] = vk::MemoryHeap {
            size: 8 << 30,
            flags: vk::MemoryHeapFlags::DEVICE_LOCAL,
        };
        memory.memory_heaps[2] = vk::MemoryHeap {
            size: 16 << 30,
            flags: vk::MemoryHeapFlags::DEVICE_LOCAL,
        };
        let mut budget = vk::PhysicalDeviceMemoryBudgetPropertiesEXT::default();
        budget.heap_budget[1] = 7 << 30;
        budget.heap_usage[1] = 1 << 30;
        budget.heap_budget[2] = 12 << 30;
        budget.heap_usage[2] = 10 << 30;
        assert_eq!(select_free_mib(&memory, &budget), Some(2048));
        budget.heap_usage[2] = 13 << 30;
        assert_eq!(select_free_mib(&memory, &budget), Some(0));
        memory.memory_heap_count = 0;
        assert_eq!(select_free_mib(&memory, &budget), None);
        memory.memory_heap_count = 17;
        assert_eq!(select_free_mib(&memory, &budget), None);
    }
}
