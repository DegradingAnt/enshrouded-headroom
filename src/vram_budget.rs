//! Driver memory-budget queries for the governor.
//!
//! Primary Vulkan contracts:
//! - <https://docs.vulkan.org/refpages/latest/refpages/source/VkPhysicalDeviceMemoryBudgetPropertiesEXT.html>
//! - <https://docs.vulkan.org/refpages/latest/refpages/source/vkGetPhysicalDeviceMemoryProperties2.html>
//! - <https://docs.vulkan.org/refpages/latest/refpages/source/vkEnumerateDeviceExtensionProperties.html>

use ash::vk;

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

/// Query the driver's advertised memory budget without allocating.
///
/// The largest DEVICE_LOCAL heap supplies budget minus usage, saturated at zero, in MiB.
/// Unsupported extensions, missing entry points and refused/incomplete enumeration are unknown.
/// Sources: Khronos vkEnumerateDeviceExtensionProperties, vkGetPhysicalDeviceMemoryProperties2
/// and VkPhysicalDeviceMemoryBudgetPropertiesEXT reference pages.
///
/// # Safety
///
/// `gipa` is the real Vulkan loader entry point. `instance` and `phys` are a matching live
/// instance/physical-device pair and stay alive throughout this call; their destruction must
/// not race this query. The driver honors the Vulkan ABI and output-buffer contracts.
pub unsafe fn free_mib(
    gipa: vk::PFN_vkGetInstanceProcAddr,
    instance: vk::Instance,
    phys: vk::PhysicalDevice,
) -> Option<u64> {
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
    // SAFETY: same live loader/instance contract; core 1.1 and KHR forms have the same signature.
    let query = unsafe { gipa(instance, c"vkGetPhysicalDeviceMemoryProperties2".as_ptr()) }
        .or_else(|| unsafe {
            gipa(
                instance,
                c"vkGetPhysicalDeviceMemoryProperties2KHR".as_ptr(),
            )
        })?;
    // SAFETY: the exact entry-point name specifies this type, including the KHR alias.
    let query: vk::PFN_vkGetPhysicalDeviceMemoryProperties2 = unsafe { std::mem::transmute(query) };
    let mut budget = vk::PhysicalDeviceMemoryBudgetPropertiesEXT::default();
    let mut memory = vk::PhysicalDeviceMemoryProperties2::default().push_next(&mut budget);
    // SAFETY: both stack structs have the required sType and pNext chain and remain live.
    unsafe { query(phys, &mut memory) };
    select_free_mib(&memory.memory_properties, &budget)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ash::vk::Handle;
    use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

    static SUPPORTED: AtomicBool = AtomicBool::new(false);
    static QUERIES: AtomicU32 = AtomicU32::new(0);

    unsafe extern "system" fn fake_enumerate(
        _: vk::PhysicalDevice,
        _: *const std::ffi::c_char,
        count: *mut u32,
        properties: *mut vk::ExtensionProperties,
    ) -> vk::Result {
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
    }

    unsafe extern "system" fn fake_memory(
        _: vk::PhysicalDevice,
        memory: *mut vk::PhysicalDeviceMemoryProperties2<'_>,
    ) {
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
    }

    unsafe extern "system" fn fake_gipa(
        _: vk::Instance,
        name: *const std::ffi::c_char,
    ) -> vk::PFN_vkVoidFunction {
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
    }

    #[test]
    fn driver_query_requires_advertised_extension_and_fills_the_budget_chain() {
        let instance = vk::Instance::from_raw(1);
        let physical = vk::PhysicalDevice::from_raw(2);
        SUPPORTED.store(false, Ordering::Relaxed);
        QUERIES.store(0, Ordering::Relaxed);
        // SAFETY: these synthetic handles are only passed to our matching fake driver functions.
        assert_eq!(unsafe { free_mib(fake_gipa, instance, physical) }, None);
        assert_eq!(QUERIES.load(Ordering::Relaxed), 0);
        SUPPORTED.store(true, Ordering::Relaxed);
        // SAFETY: the same fake driver owns every pointer the query uses.
        assert_eq!(
            unsafe { free_mib(fake_gipa, instance, physical) },
            Some(2048)
        );
        assert_eq!(QUERIES.load(Ordering::Relaxed), 1);
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
