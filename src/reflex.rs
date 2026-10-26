//! Pure NVIDIA Reflex decisions and device planning. No Vulkan calls are made here.

#![forbid(unsafe_code)]

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
