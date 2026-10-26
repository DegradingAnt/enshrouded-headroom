//! Pure AMD Anti-Lag planning and call sequencing. No driver or operating-system calls.

#![forbid(unsafe_code)]

use std::ffi::CStr;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};

use ash::vk;

use crate::config::AntiLagMode;
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
