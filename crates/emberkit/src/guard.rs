//! Panic guard and per-feature kill switch.
//!
//! Every extern function and hook body runs inside [`guard`]. If the body panics, the panic
//! stops there: the feature is switched off for the rest of the session and the caller gets
//! the fallback value (normally "call the game's original function"). A feature that is off
//! stays off; its body never runs again.
//!
//! The panic hook ([`install_panic_hook`]) is separate: it only decides where panic text goes.

use std::any::Any;
use std::cell::Cell;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicU32, Ordering};

/// A mod-owned fixed-size feature identity.
pub trait FeatureId: Copy + std::fmt::Debug {
    /// Stable bit position within this mod's disabled mask.
    fn index(self) -> u32;
}
/// A mask bit derived only from the bounded index, never from caller-supplied shifts.
fn feature_bit(f: impl FeatureId) -> u32 {
    1u32.checked_shl(f.index()).unwrap_or(0)
}
/// Features that have been switched off, one bit per [`FeatureId`]. Process-global: a switched-off
/// feature never comes back until the game restarts.
static DISABLED: AtomicU32 = AtomicU32::new(0);

/// Set `f`'s bit in `mask`. Returns `true` only for the call that actually changed it.
pub fn mark(mask: &AtomicU32, f: impl FeatureId) -> bool {
    // Relaxed is enough: the mask is a flag with no data hanging off it, and `fetch_or` is
    // atomic, so exactly one caller sees the bit as newly set.
    let bit = feature_bit(f);
    bit != 0 && mask.fetch_or(bit, Ordering::Relaxed) & bit == 0
}

/// Is `f`'s bit set in `mask`?
pub fn has(mask: &AtomicU32, f: impl FeatureId) -> bool {
    feature_bit(f) == 0 || mask.load(Ordering::Relaxed) & feature_bit(f) != 0
}

/// Has `f` been switched off?
pub fn is_disabled(f: impl FeatureId) -> bool {
    has(&DISABLED, f)
}

/// Switch `f` off without formatting, allocation or logging.
///
/// Returns `true` only when this call first disabled the feature. A caller may report that
/// change later from a safe point.
pub fn disable_silent(f: impl FeatureId) -> bool {
    mark(&DISABLED, f)
}

/// Switch `f` off for the rest of the session and log `why`. The reason is logged once; a
/// second call for the same feature is silent.
pub fn disable(f: impl FeatureId, why: &str) {
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
pub fn guard<T>(f: impl FeatureId, fallback: impl FnOnce() -> T, body: impl FnOnce() -> T) -> T {
    if is_disabled(f) {
        return fallback();
    }
    // Unwind safety: once `disable` has run, no call that starts later runs `body`, so those
    // calls never see state this panic left half-written. A call already in flight on another
    // thread still can. Hook bodies must therefore keep their atomics consistent at every point
    // where they can panic.
    match catch_panic(body) {
        Ok(value) => value,
        Err(message) => {
            disable(f, &format!("panic: {message}"));
            fallback()
        }
    }
}

/// Run `f` and turn a panic into `Err("<what>: panic: <message>")`. For code that has no
/// [`FeatureId`] to switch off, such as a start-up call into a crate that can panic (the `minhook`
/// crate panics when `MH_Initialize` fails). Nothing is disabled and nothing is logged here; the
/// panic hook logs the panic itself.
///
/// Unwind safety is the caller's: after an `Err`, the caller must not rely on state that `f` was
/// changing.
pub fn catch<T>(what: &str, f: impl FnOnce() -> T) -> Result<T, String> {
    catch_panic(f).map_err(|message| format!("{what}: panic: {message}"))
}

/// `f()`, or the text of its panic. The one place this module handles a panic payload; the
/// callers ([`guard`], [`catch`]) give the unwind-safety argument for their own `f`.
fn catch_panic<T>(f: impl FnOnce() -> T) -> Result<T, String> {
    catch_unwind(AssertUnwindSafe(f)).map_err(|payload| {
        let message = payload_message(payload.as_ref()).to_owned();
        // Never drop the payload: `panic_any` can carry a value whose destructor panics, and that
        // panic would unwind out of this function. Forgetting leaks a few bytes, once per caught
        // panic.
        std::mem::forget(payload);
        message
    })
}

/// The text of a panic payload: `panic!("literal")` and `panic!("{x}")` give a `&str` or a
/// `String`; `panic_any` can carry anything else.
pub fn payload_message(payload: &(dyn Any + Send)) -> &str {
    if let Some(text) = payload.downcast_ref::<&'static str>() {
        text
    } else if let Some(text) = payload.downcast_ref::<String>() {
        text
    } else {
        "<non-string panic payload>"
    }
}

/// One log line for a panic: thread, `file:line:column`, message.
pub fn format_panic(
    thread: Option<&str>,
    location: Option<(&str, u32, u32)>,
    message: &str,
) -> String {
    let thread = thread.unwrap_or("<unnamed>");
    match location {
        Some((file, line, column)) => {
            format!("panic in thread '{thread}' at {file}:{line}:{column}: {message}")
        }
        None => format!("panic in thread '{thread}' at <unknown location>: {message}"),
    }
}

thread_local! {
    /// Set while this thread's panic hook is running.
    static IN_HOOK: Cell<bool> = const { Cell::new(false) };
}

/// Marks the current thread as "inside the panic hook" until dropped.
pub struct HookScope;

impl HookScope {
    /// `None` if this thread is already inside the hook (or its thread-local storage is gone).
    pub fn enter() -> Option<HookScope> {
        match IN_HOOK.try_with(|flag| flag.replace(true)) {
            Ok(false) => Some(HookScope),
            _ => None,
        }
    }
}

impl Drop for HookScope {
    fn drop(&mut self) {
        let _ = IN_HOOK.try_with(|flag| flag.set(false));
    }
}

/// Send panic text to the log instead of stderr.
///
/// This replaces the process-wide panic hook and does not call the previous one. The game has
/// no console, so stderr text would be lost anyway; the log is where a player can find it.
///
/// A hook that is entered while this thread is already inside it logs nothing. Today std aborts
/// the process on a panic inside a hook, so that cannot happen; the flag is the cheap guard
/// against logging into a lock this thread already holds, should that ever change.
pub fn install_panic_hook() {
    std::panic::set_hook(Box::new(|info| {
        let Some(_scope) = HookScope::enter() else {
            return;
        };
        let thread = std::thread::current();
        let location = info.location().map(|l| (l.file(), l.line(), l.column()));
        let text = format_panic(thread.name(), location, payload_message(info.payload()));
        log::error!("{text}");
    }));
}

/// Identity used only by native scanner regression callbacks.
#[cfg(test)]
#[derive(Clone, Copy, Debug)]
pub enum TestFeature {
    /// Query stub panic boundary.
    VkWrap,
}
#[cfg(test)]
impl FeatureId for TestFeature {
    fn index(self) -> u32 {
        0
    }
}
#[cfg(test)]
mod generic_tests {
    use super::*;
    #[derive(Clone, Copy, Debug)]
    struct Id(u32);
    impl FeatureId for Id {
        fn index(self) -> u32 {
            self.0
        }
    }
    #[test]
    fn invalid_feature_indices_are_disabled_without_shifting() {
        assert!(is_disabled(Id(32)));
        assert!(is_disabled(Id(u32::MAX)));
        assert_eq!(guard(Id(32), || 7, || panic!("invalid identity ran")), 7);
    }
}
