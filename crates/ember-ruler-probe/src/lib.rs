//! Opt-in raw position calibration. No live sample is admitted to the measuring session.
mod native;
mod reader;
mod runtime;
use std::ffi::c_void;
/// Startup-only opt-in defaults for this diagnostic, separate from Headroom.
pub const DEFAULT_INI: &str = include_str!("../enshrouded-ruler-probe.default.ini");
/// Separate kill switches for our code; none can suppress the native original.
#[derive(Clone, Copy, Debug)]
enum Feature {
    /// Bootstrap failures do not affect any original game call.
    Startup,
    /// Raw sampling is independent of native forwarding.
    Observation,
    /// Formatting failures affect only our background logger.
    Worker,
}
impl emberkit::guard::FeatureId for Feature {
    fn index(self) -> u32 {
        match self {
            Self::Startup => 0,
            Self::Observation => 1,
            Self::Worker => 2,
        }
    }
}
/// UAL calls this outside loader lock. Disabled by default; unknown binaries never hook.
#[no_mangle]
#[allow(non_snake_case)]
pub extern "system" fn InitializeASI() {
    emberkit::guard::guard(
        Feature::Startup,
        || (),
        || {
            if let Err(why) = runtime::start() {
                log::warn!("probe inactive: {why}");
            }
        },
    );
}
/// Native observation boundary, called after the original simulation step.
/// # Safety
/// storage must be the admitted callback's live storage on its owner thread.
#[no_mangle]
pub unsafe extern "C" fn ember_ruler_observe(storage: *mut c_void) {
    emberkit::guard::guard(
        Feature::Observation,
        || (),
        || {
            // SAFETY: the C++ detour invokes this in its validated callback after the original returns.
            unsafe { runtime::observe(storage) };
        },
    );
}
