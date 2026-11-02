//! Process priority, applied after the first renderer device exists.

use crate::config::Priority;
use windows_sys::Win32::Foundation::GetLastError;
use windows_sys::Win32::System::Threading::{
    GetCurrentProcess, SetPriorityClass, ABOVE_NORMAL_PRIORITY_CLASS,
};

/// The process priority class to set, or `None` to leave it unchanged.
fn decide(setting: Priority) -> Option<u32> {
    match setting {
        Priority::AboveNormal => Some(ABOVE_NORMAL_PRIORITY_CLASS),
        Priority::Normal => None,
    }
}

/// Apply the configured priority after renderer creation. A failure leaves the game running.
pub fn apply(setting: Priority) {
    let Some(class) = decide(setting) else {
        log::info!("priority: unchanged (setting)");
        return;
    };
    // SAFETY: the pseudo-handle names this process and remains valid for this call. `class` is
    // ABOVE_NORMAL_PRIORITY_CLASS, the only class returned by `decide`.
    if unsafe { SetPriorityClass(GetCurrentProcess(), class) } == 0 {
        // SAFETY: this reads this thread's error immediately after the failed Windows call.
        let error = unsafe { GetLastError() };
        log::warn!("priority: could not set AboveNormal (GetLastError={error})");
    } else {
        log::info!("priority: AboveNormal");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decide_raises_above_normal_and_leaves_normal_unchanged() {
        assert_eq!(
            decide(Priority::AboveNormal),
            Some(ABOVE_NORMAL_PRIORITY_CLASS)
        );
        assert_eq!(decide(Priority::Normal), None);
    }
}
