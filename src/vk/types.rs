//! Vulkan types that ash 0.38.0 lacks.
//!
//! ash 0.38.0 is generated from Vulkan 1.3.281. It already has every `VK_NV_low_latency2` struct
//! (`LatencySleepModeInfoNV`, `LatencySleepInfoNV`, `SetLatencyMarkerInfoNV`,
//! `GetLatencyMarkerInfoNV`, `LatencyTimingsFrameReportNV`, `LatencySubmissionPresentIdNV`,
//! `SwapchainLatencyCreateInfoNV`, `OutOfBandQueueTypeInfoNV`, `LatencySurfaceCapabilitiesNV`), and
//! `VK_KHR_present_id` (`PresentIdKHR`, `PhysicalDevicePresentIdFeaturesKHR`); their layouts match
//! the current header. It does not have `VK_KHR_present_id2`, which is newer, so that is defined
//! here.
//!
//! Source: Vulkan-Headers `include/vulkan/vulkan_core.h`, `VK_HEADER_VERSION 366` (tag v1.4.366,
//! commit 73814d006638aa00bd553215b054f0f02e457e22),
//! <https://raw.githubusercontent.com/KhronosGroup/Vulkan-Headers/v1.4.366/include/vulkan/vulkan_core.h>.
//!
//! The header gives the fields, not the sizes. The sizes in the tests follow from the fields under
//! the C layout rules of x86-64 Windows (pointers 8 bytes and 8-aligned, `VkStructureType`,
//! `uint32_t` and `VkBool32` 4 bytes).

use std::ffi::{c_void, CStr};

use ash::vk;

/// `VK_KHR_PRESENT_ID_2_EXTENSION_NAME`.
pub const KHR_PRESENT_ID_2_NAME: &CStr = c"VK_KHR_present_id2";
/// `VK_KHR_PRESENT_ID_2_SPEC_VERSION`.
pub const KHR_PRESENT_ID_2_SPEC_VERSION: u32 = 1;

/// `VK_STRUCTURE_TYPE_PRESENT_ID_2_KHR`. The game chains a struct of this type on every present.
pub const STRUCTURE_TYPE_PRESENT_ID_2_KHR: vk::StructureType =
    vk::StructureType::from_raw(1_000_479_001);
/// `VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_PRESENT_ID_2_FEATURES_KHR`.
pub const STRUCTURE_TYPE_PHYSICAL_DEVICE_PRESENT_ID_2_FEATURES_KHR: vk::StructureType =
    vk::StructureType::from_raw(1_000_479_002);

/// `VkPresentId2KHR`: chained on `VkPresentInfoKHR`.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct PresentId2KHR {
    pub s_type: vk::StructureType,
    pub p_next: *const c_void,
    pub swapchain_count: u32,
    pub p_present_ids: *const u64,
}

/// `VkPhysicalDevicePresentId2FeaturesKHR`: chained on `VkPhysicalDeviceFeatures2` or
/// `VkDeviceCreateInfo`.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct PhysicalDevicePresentId2FeaturesKHR {
    pub s_type: vk::StructureType,
    pub p_next: *mut c_void,
    pub present_id2: vk::Bool32,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::mem::{align_of, offset_of, size_of};

    /// `typedef struct VkPresentId2KHR { VkStructureType sType; const void* pNext;
    /// uint32_t swapchainCount; const uint64_t* pPresentIds; }`: 4 + 4 padding + 8 + 4 + 4
    /// padding + 8.
    #[test]
    fn present_id2_matches_the_header() {
        assert_eq!(size_of::<PresentId2KHR>(), 32);
        assert_eq!(align_of::<PresentId2KHR>(), 8);
        assert_eq!(offset_of!(PresentId2KHR, s_type), 0);
        assert_eq!(offset_of!(PresentId2KHR, p_next), 8);
        assert_eq!(offset_of!(PresentId2KHR, swapchain_count), 16);
        assert_eq!(offset_of!(PresentId2KHR, p_present_ids), 24);
        assert_eq!(STRUCTURE_TYPE_PRESENT_ID_2_KHR.as_raw(), 1_000_479_001);
    }

    /// `typedef struct VkPhysicalDevicePresentId2FeaturesKHR { VkStructureType sType; void* pNext;
    /// VkBool32 presentId2; }`: 4 + 4 padding + 8 + 4 + 4 tail padding.
    #[test]
    fn present_id2_features_match_the_header() {
        assert_eq!(size_of::<PhysicalDevicePresentId2FeaturesKHR>(), 24);
        assert_eq!(align_of::<PhysicalDevicePresentId2FeaturesKHR>(), 8);
        assert_eq!(offset_of!(PhysicalDevicePresentId2FeaturesKHR, s_type), 0);
        assert_eq!(offset_of!(PhysicalDevicePresentId2FeaturesKHR, p_next), 8);
        assert_eq!(
            offset_of!(PhysicalDevicePresentId2FeaturesKHR, present_id2),
            16
        );
        assert_eq!(
            STRUCTURE_TYPE_PHYSICAL_DEVICE_PRESENT_ID_2_FEATURES_KHR.as_raw(),
            1_000_479_002
        );
    }

    #[test]
    fn present_id2_extension_name_and_version() {
        assert_eq!(KHR_PRESENT_ID_2_NAME, c"VK_KHR_present_id2");
        assert_eq!(KHR_PRESENT_ID_2_SPEC_VERSION, 1);
    }
}
