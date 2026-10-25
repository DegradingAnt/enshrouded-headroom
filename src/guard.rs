//! Panic guard and per-feature kill switch.
//!
//! Every extern function and hook body runs inside [`guard`]. If the body panics, the panic
//! stops there: the feature is switched off for the rest of the session and the caller gets
//! the fallback value (normally "call the game's original function"). A feature that is off
//! stays off; its body never runs again.
//!
//! The panic hook ([`install_panic_hook`]) is separate: it only decides where panic text goes.

use std::any::Any;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicU32, Ordering};

/// A part of the mod that can be switched off on its own.
///
/// Each variant owns one bit of the disabled mask. Keep `Governor` the last variant, or move
/// the `LAST` constant below with it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Feature {
    TextureBudget,
    AsyncCompute,
    StartupScales,
    ObjectDistance,
    ScatterDistance,
    TerrainDistance,
    VkWrap,
    Reflex,
    Governor,
}

/// The highest-numbered variant. The mask is a `u32`, so its index must stay below 32.
const LAST: Feature = Feature::Governor;
const _: () = assert!((LAST as u32) < u32::BITS);

impl Feature {
    /// This feature's bit in the disabled mask.
    fn bit(self) -> u32 {
        1 << (self as u32)
    }
}

/// Features that have been switched off, one bit per [`Feature`]. Process-global: a switched-off
/// feature never comes back until the game restarts.
static DISABLED: AtomicU32 = AtomicU32::new(0);

/// Set `f`'s bit in `mask`. Returns `true` only for the call that actually changed it.
fn mark(mask: &AtomicU32, f: Feature) -> bool {
    // Relaxed is enough: the mask is a flag with no data hanging off it, and `fetch_or` is
    // atomic, so exactly one caller sees the bit as newly set.
    mask.fetch_or(f.bit(), Ordering::Relaxed) & f.bit() == 0
}

/// Is `f`'s bit set in `mask`?
fn has(mask: &AtomicU32, f: Feature) -> bool {
    mask.load(Ordering::Relaxed) & f.bit() != 0
}

/// Has `f` been switched off?
pub fn is_disabled(f: Feature) -> bool {
    has(&DISABLED, f)
}

/// Switch `f` off for the rest of the session and log `why`. The reason is logged once; a
/// second call for the same feature is silent.
pub fn disable(f: Feature, why: &str) {
    if mark(&DISABLED, f) {
        log::warn!("{f:?} disabled: {why}");
    }
}

/// Run `body` unless `f` is off, and never let a panic out of it.
///
/// - `f` already disabled: returns `fallback()`; `body` does not run.
/// - `body` panics: the panic is caught, `f` is disabled (the reason is logged once), and the
///   result is `fallback()`.
/// - Otherwise: returns `body()`.
///
/// `fallback` itself is not guarded. It is expected to be the plain "call the original" path.
pub fn guard<T>(f: Feature, fallback: impl FnOnce() -> T, body: impl FnOnce() -> T) -> T {
    if is_disabled(f) {
        return fallback();
    }
    // AssertUnwindSafe: after a panic the feature is off and `body` is never run again, so no
    // caller can observe state that the panic left half-updated.
    match catch_unwind(AssertUnwindSafe(body)) {
        Ok(value) => value,
        Err(payload) => {
            disable(f, &format!("panic: {}", payload_message(payload.as_ref())));
            fallback()
        }
    }
}

/// The text of a panic payload: `panic!("literal")` and `panic!("{x}")` give a `&str` or a
/// `String`; `panic_any` can carry anything else.
fn payload_message(payload: &(dyn Any + Send)) -> &str {
    if let Some(text) = payload.downcast_ref::<&'static str>() {
        text
    } else if let Some(text) = payload.downcast_ref::<String>() {
        text
    } else {
        "<non-string panic payload>"
    }
}

/// One log line for a panic: thread, `file:line:column`, message.
fn format_panic(thread: Option<&str>, location: Option<(&str, u32, u32)>, message: &str) -> String {
    let thread = thread.unwrap_or("<unnamed>");
    match location {
        Some((file, line, column)) => {
            format!("panic in thread '{thread}' at {file}:{line}:{column}: {message}")
        }
        None => format!("panic in thread '{thread}' at <unknown location>: {message}"),
    }
}

/// Send panic text to the log instead of stderr.
///
/// This replaces the process-wide panic hook and does not call the previous one. The game has
/// no console, so stderr text would be lost anyway; the log is where a player can find it.
pub fn install_panic_hook() {
    std::panic::set_hook(Box::new(|info| {
        let thread = std::thread::current();
        let location = info.location().map(|l| (l.file(), l.line(), l.column()));
        let text = format_panic(thread.name(), location, payload_message(info.payload()));
        log::error!("{text}");
    }));
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;

    // DISABLED is process-global, tests run in parallel, and other modules' tests share this
    // process. Features these tests own:
    //   Governor        never disabled (guard_returns_body_value)
    //   Reflex          disabled by guard_catches_panic_and_disables
    //   ObjectDistance  disabled by disabled_feature_runs_fallback_and_skips_body
    //   AsyncCompute    disabled by guard_catches_panic_with_string_payload
    // The bit-mask logic is tested on a local mask, not on DISABLED.

    const ALL: [Feature; 9] = [
        Feature::TextureBudget,
        Feature::AsyncCompute,
        Feature::StartupScales,
        Feature::ObjectDistance,
        Feature::ScatterDistance,
        Feature::TerrainDistance,
        Feature::VkWrap,
        Feature::Reflex,
        Feature::Governor,
    ];

    #[test]
    fn guard_returns_body_value() {
        assert_eq!(guard(Feature::Governor, || 0, || 7), 7);
    }

    #[test]
    fn guard_catches_panic_and_disables() {
        assert_eq!(guard(Feature::Reflex, || 1, || panic!("boom")), 1);
        assert!(is_disabled(Feature::Reflex));
        assert_eq!(guard(Feature::Reflex, || 2, || 3), 2); // stays disabled
    }

    #[test]
    fn disabled_feature_runs_fallback_and_skips_body() {
        disable(Feature::ObjectDistance, "test");
        assert!(is_disabled(Feature::ObjectDistance));
        let body_ran = AtomicBool::new(false);
        let value = guard(
            Feature::ObjectDistance,
            || 5,
            || {
                body_ran.store(true, Ordering::Relaxed);
                9
            },
        );
        assert_eq!(value, 5);
        assert!(!body_ran.load(Ordering::Relaxed));
    }

    #[test]
    fn guard_catches_panic_with_string_payload() {
        let n = 3;
        assert_eq!(
            guard(Feature::AsyncCompute, || -1, || panic!("bad value {n}")),
            -1
        );
        assert!(is_disabled(Feature::AsyncCompute));
    }

    #[test]
    fn every_feature_has_its_own_bit() {
        let mut seen = 0u32;
        for f in ALL {
            assert_ne!(f.bit(), 0, "{f:?}");
            assert_eq!(seen & f.bit(), 0, "{f:?} shares a bit");
            seen |= f.bit();
        }
        assert_eq!(seen.count_ones() as usize, ALL.len());
    }

    #[test]
    fn marking_reports_first_time_only_and_leaves_others_alone() {
        let mask = AtomicU32::new(0);
        assert!(mark(&mask, Feature::VkWrap), "first mark is new");
        assert!(!mark(&mask, Feature::VkWrap), "second mark is not new");
        assert!(has(&mask, Feature::VkWrap));
        for f in ALL.into_iter().filter(|f| *f != Feature::VkWrap) {
            assert!(!has(&mask, f), "{f:?} must stay enabled");
        }
    }

    #[test]
    fn payload_message_reads_str_string_and_other_payloads() {
        let a = catch_unwind(|| panic!("static text")).unwrap_err();
        assert_eq!(payload_message(a.as_ref()), "static text");
        let b = catch_unwind(|| {
            let n = 4;
            panic!("formatted {n}")
        })
        .unwrap_err();
        assert_eq!(payload_message(b.as_ref()), "formatted 4");
        let c = catch_unwind(|| std::panic::panic_any(42u32)).unwrap_err();
        assert_eq!(payload_message(c.as_ref()), "<non-string panic payload>");
    }

    #[test]
    fn format_panic_names_thread_location_and_message() {
        assert_eq!(
            format_panic(Some("worker"), Some(("src/x.rs", 10, 5)), "boom"),
            "panic in thread 'worker' at src/x.rs:10:5: boom"
        );
    }

    #[test]
    fn format_panic_copes_with_missing_thread_and_location() {
        assert_eq!(
            format_panic(None, None, "boom"),
            "panic in thread '<unnamed>' at <unknown location>: boom"
        );
    }
}
