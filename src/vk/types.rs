//! Vulkan types that ash 0.38.0 lacks.
//!
//! ash 0.38.0 is generated from Vulkan 1.3.281. It already has every `VK_NV_low_latency2` struct
//! (`LatencySleepModeInfoNV`, `LatencySleepInfoNV`, `SetLatencyMarkerInfoNV`,
//! `GetLatencyMarkerInfoNV`, `LatencyTimingsFrameReportNV`, `LatencySubmissionPresentIdNV`,
//! `SwapchainLatencyCreateInfoNV`, `OutOfBandQueueTypeInfoNV`, `LatencySurfaceCapabilitiesNV`), and
//! `VK_KHR_present_id` (`PresentIdKHR`, `PhysicalDevicePresentIdFeaturesKHR`); their layouts match
//! the current header. It does not have `VK_KHR_present_id2`, which is newer, so that is defined
//! here, together with `VK_AMD_anti_lag` (first in header 291).
//!
//! Source: Vulkan-Headers `include/vulkan/vulkan_core.h`, `VK_HEADER_VERSION 366` (tag v1.4.366,
//! commit 73814d006638aa00bd553215b054f0f02e457e22),
//! <https://raw.githubusercontent.com/KhronosGroup/Vulkan-Headers/v1.4.366/include/vulkan/vulkan_core.h>.
//!
//! The header gives the fields, not the sizes. The sizes in the tests follow from the fields under
//! the C layout rules of x86-64 Windows (pointers 8 bytes and 8-aligned, `VkStructureType`,
//! `uint32_t` and `VkBool32` 4 bytes).

//! AMD declarations follow extension 477, spec version 1, in the pinned Khronos registry:
//! <https://github.com/KhronosGroup/Vulkan-Headers/blob/73814d006638aa00bd553215b054f0f02e457e22/registry/vk.xml>.

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

/// `VK_AMD_ANTI_LAG_EXTENSION_NAME` from the pinned Khronos registry.
pub const AMD_ANTI_LAG_NAME: &CStr = c"VK_AMD_anti_lag";
/// `VK_AMD_ANTI_LAG_SPEC_VERSION`.
pub const AMD_ANTI_LAG_SPEC_VERSION: u32 = 1;
/// `VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_ANTI_LAG_FEATURES_AMD`.
pub const PHYSICAL_DEVICE_ANTI_LAG_FEATURES_AMD: vk::StructureType =
    vk::StructureType::from_raw(1_000_476_000);
/// `VK_STRUCTURE_TYPE_ANTI_LAG_DATA_AMD`.
pub const ANTI_LAG_DATA_AMD: vk::StructureType = vk::StructureType::from_raw(1_000_476_001);
/// `VK_STRUCTURE_TYPE_ANTI_LAG_PRESENTATION_INFO_AMD`.
pub const ANTI_LAG_PRESENTATION_INFO_AMD: vk::StructureType =
    vk::StructureType::from_raw(1_000_476_002);

/// `VkAntiLagModeAMD`, represented as the registry's 32-bit C enum.
#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AntiLagModeAMD(i32);

impl AntiLagModeAMD {
    /// Let the driver select the mode.
    pub const DRIVER_CONTROL: Self = Self(0);
    /// Enable Anti-Lag.
    pub const ON: Self = Self(1);
    /// Disable Anti-Lag.
    pub const OFF: Self = Self(2);

    /// The numeric Vulkan value.
    pub const fn as_raw(self) -> i32 {
        self.0
    }
}

/// `VkAntiLagStageAMD`, represented as the registry's 32-bit C enum.
#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AntiLagStageAMD(i32);

impl AntiLagStageAMD {
    /// The input stage.
    pub const INPUT: Self = Self(0);
    /// The presentation stage.
    pub const PRESENT: Self = Self(1);

    /// The numeric Vulkan value.
    pub const fn as_raw(self) -> i32 {
        self.0
    }
}

/// `VkPhysicalDeviceAntiLagFeaturesAMD`: query or enable the `antiLag` feature.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct PhysicalDeviceAntiLagFeaturesAMD {
    /// `PHYSICAL_DEVICE_ANTI_LAG_FEATURES_AMD`.
    pub s_type: vk::StructureType,
    /// The next feature struct, or null.
    pub p_next: *mut c_void,
    /// Whether Anti-Lag is supported or enabled.
    pub anti_lag: vk::Bool32,
}

/// `VkAntiLagPresentationInfoAMD`: a stage and its frame identifier.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct AntiLagPresentationInfoAMD {
    /// `ANTI_LAG_PRESENTATION_INFO_AMD`.
    pub s_type: vk::StructureType,
    /// The next struct, or null.
    pub p_next: *mut c_void,
    /// The input or presentation stage.
    pub stage: AntiLagStageAMD,
    /// A monotonically increasing frame identifier.
    pub frame_index: u64,
}

/// `VkAntiLagDataAMD`: the arguments for one Anti-Lag update.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct AntiLagDataAMD {
    /// `ANTI_LAG_DATA_AMD`.
    pub s_type: vk::StructureType,
    /// The next struct, or null.
    pub p_next: *const c_void,
    /// The requested Anti-Lag mode.
    pub mode: AntiLagModeAMD,
    /// A frame-rate limit, or zero for no limit.
    pub max_fps: u32,
    /// The stage and frame identifier, or null.
    pub p_presentation_info: *const AntiLagPresentationInfoAMD,
}

/// `vkAntiLagUpdateAMD` from the pinned registry; this alias has no function body.
///
/// # Safety
///
/// The caller must use a live device with the extension and feature enabled. The data and all
/// chained pointers must stay valid for the call and satisfy the Vulkan command's requirements.
#[allow(non_camel_case_types)]
pub type PFN_vkAntiLagUpdateAMD = unsafe extern "system" fn(vk::Device, *const AntiLagDataAMD);

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

    /// The pinned registry's AMD structs under the Windows x64 C ABI.
    #[test]
    fn anti_lag_layout_matches_registry() {
        assert_eq!(size_of::<PhysicalDeviceAntiLagFeaturesAMD>(), 24);
        assert_eq!(align_of::<PhysicalDeviceAntiLagFeaturesAMD>(), 8);
        assert_eq!(offset_of!(PhysicalDeviceAntiLagFeaturesAMD, s_type), 0);
        assert_eq!(offset_of!(PhysicalDeviceAntiLagFeaturesAMD, p_next), 8);
        assert_eq!(offset_of!(PhysicalDeviceAntiLagFeaturesAMD, anti_lag), 16);
        assert_eq!(size_of::<AntiLagPresentationInfoAMD>(), 32);
        assert_eq!(align_of::<AntiLagPresentationInfoAMD>(), 8);
        assert_eq!(offset_of!(AntiLagPresentationInfoAMD, s_type), 0);
        assert_eq!(offset_of!(AntiLagPresentationInfoAMD, p_next), 8);
        assert_eq!(offset_of!(AntiLagPresentationInfoAMD, stage), 16);
        assert_eq!(offset_of!(AntiLagPresentationInfoAMD, frame_index), 24);
        assert_eq!(size_of::<AntiLagDataAMD>(), 32);
        assert_eq!(align_of::<AntiLagDataAMD>(), 8);
        assert_eq!(offset_of!(AntiLagDataAMD, s_type), 0);
        assert_eq!(offset_of!(AntiLagDataAMD, p_next), 8);
        assert_eq!(offset_of!(AntiLagDataAMD, mode), 16);
        assert_eq!(offset_of!(AntiLagDataAMD, max_fps), 20);
        assert_eq!(offset_of!(AntiLagDataAMD, p_presentation_info), 24);
    }

    /// Extension 477 and its offsets define these exact numbers.
    #[test]
    fn anti_lag_values_match_registry() {
        assert_eq!(AMD_ANTI_LAG_NAME, c"VK_AMD_anti_lag");
        assert_eq!(AMD_ANTI_LAG_SPEC_VERSION, 1);
        assert_eq!(
            PHYSICAL_DEVICE_ANTI_LAG_FEATURES_AMD.as_raw(),
            1_000_476_000
        );
        assert_eq!(ANTI_LAG_DATA_AMD.as_raw(), 1_000_476_001);
        assert_eq!(ANTI_LAG_PRESENTATION_INFO_AMD.as_raw(), 1_000_476_002);
        assert_eq!(size_of::<AntiLagModeAMD>(), 4);
        assert_eq!(align_of::<AntiLagModeAMD>(), 4);
        assert_eq!(size_of::<AntiLagStageAMD>(), 4);
        assert_eq!(align_of::<AntiLagStageAMD>(), 4);
        assert_eq!(AntiLagModeAMD::DRIVER_CONTROL.as_raw(), 0);
        assert_eq!(AntiLagModeAMD::ON.as_raw(), 1);
        assert_eq!(AntiLagModeAMD::OFF.as_raw(), 2);
        assert_eq!(AntiLagStageAMD::INPUT.as_raw(), 0);
        assert_eq!(AntiLagStageAMD::PRESENT.as_raw(), 1);
    }
}
