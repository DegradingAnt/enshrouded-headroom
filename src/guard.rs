//! Headroom feature identities and shared guard API.
pub use emberkit::guard::*;
#[cfg(test)]
use std::panic::catch_unwind;
#[cfg(test)]
use std::sync::atomic::{AtomicU32, Ordering};
/// A part of the mod that can be switched off on its own.
///
/// Each variant owns one bit of the disabled mask, given by its `index`.
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
    AntiLag,
}

impl Feature {
    /// Every variant, in `index` order. Rust cannot list an enum's variants by itself, so this is
    /// kept by hand; the check below and the tests catch a stale entry, a wrong order and a list
    /// that outgrows the mask.
    const ALL: [Feature; 10] = [
        Feature::TextureBudget,
        Feature::AsyncCompute,
        Feature::StartupScales,
        Feature::ObjectDistance,
        Feature::ScatterDistance,
        Feature::TerrainDistance,
        Feature::VkWrap,
        Feature::Reflex,
        Feature::Governor,
        Feature::AntiLag,
    ];

    /// This feature's position in the disabled mask.
    ///
    /// One arm per variant and no wildcard: a new variant does not compile until it is given a
    /// position here.
    const fn index(self) -> u32 {
        match self {
            Feature::TextureBudget => 0,
            Feature::AsyncCompute => 1,
            Feature::StartupScales => 2,
            Feature::ObjectDistance => 3,
            Feature::ScatterDistance => 4,
            Feature::TerrainDistance => 5,
            Feature::VkWrap => 6,
            Feature::Reflex => 7,
            Feature::Governor => 8,
            Feature::AntiLag => 9,
        }
    }

    /// This feature's bit in the disabled mask.
    #[cfg(test)]
    const fn bit(self) -> u32 {
        1 << self.index()
    }
}

// Compile-time check: `ALL` is in index order, and every index fits the `u32` mask.
const _: () = {
    let mut i = 0;
    while i < Feature::ALL.len() {
        assert!(Feature::ALL[i].index() == i as u32);
        i += 1;
    }
    assert!(Feature::ALL.len() <= u32::BITS as usize);
};

impl emberkit::guard::FeatureId for Feature {
    fn index(self) -> u32 {
        self.index()
    }
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
    //   StartupScales   disabled by guard_survives_a_payload_whose_drop_panics
    // The bit-mask logic is tested on a local mask, not on DISABLED.

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

    /// A panic payload whose destructor panics.
    struct DropBomb;

    impl Drop for DropBomb {
        fn drop(&mut self) {
            panic!("drop boom");
        }
    }

    #[test]
    fn guard_survives_a_payload_whose_drop_panics() {
        let value = guard(
            Feature::StartupScales,
            || 1,
            || std::panic::panic_any(DropBomb),
        );
        assert_eq!(value, 1);
        assert!(is_disabled(Feature::StartupScales));
    }

    #[test]
    fn catch_returns_the_value_or_the_panic_text() {
        assert_eq!(catch("x", || 7), Ok(7));
        let why = catch("create the hook", || -> u32 {
            panic!("MH_Initialize failed")
        })
        .unwrap_err();
        assert_eq!(why, "create the hook: panic: MH_Initialize failed");
    }

    #[test]
    fn catch_survives_a_payload_whose_drop_panics() {
        let why = catch("apply", || -> u32 { std::panic::panic_any(DropBomb) }).unwrap_err();
        assert_eq!(why, "apply: panic: <non-string panic payload>");
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
        for f in Feature::ALL {
            assert_ne!(f.bit(), 0, "{f:?}");
            assert_eq!(seen & f.bit(), 0, "{f:?} shares a bit");
            seen |= f.bit();
        }
        assert_eq!(seen.count_ones() as usize, Feature::ALL.len());
    }

    #[test]
    fn all_lists_every_variant_in_index_order() {
        for (position, f) in Feature::ALL.iter().enumerate() {
            assert_eq!(f.index() as usize, position, "{f:?}");
        }
        // `Feature::index` is an exhaustive match, so a new variant cannot be missed there. The
        // hand-kept `ALL` can be, so count the variants in this file's own source.
        let source = include_str!("guard.rs");
        let variants = source
            .split("pub enum Feature {")
            .nth(1)
            .and_then(|rest| rest.split('}').next())
            .unwrap_or_default()
            .lines()
            .map(str::trim)
            .filter(|line| !line.starts_with("//") && line.ends_with(','))
            .count();
        assert_eq!(variants, Feature::ALL.len(), "Feature::ALL is out of date");
    }

    #[test]
    fn hook_scope_is_not_reentrant_resets_on_drop_and_is_per_thread() {
        let first = HookScope::enter();
        assert!(first.is_some());
        assert!(HookScope::enter().is_none(), "nested entry, same thread");
        assert!(
            std::thread::spawn(|| HookScope::enter().is_some())
                .join()
                .unwrap(),
            "another thread is not affected"
        );
        drop(first);
        assert!(HookScope::enter().is_some(), "entry works after the scope");
    }

    #[test]
    fn marking_reports_first_time_only_and_leaves_others_alone() {
        let mask = AtomicU32::new(0);
        assert!(mark(&mask, Feature::VkWrap), "first mark is new");
        assert!(!mark(&mask, Feature::VkWrap), "second mark is not new");
        assert!(has(&mask, Feature::VkWrap));
        for f in Feature::ALL.into_iter().filter(|f| *f != Feature::VkWrap) {
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

    /// Public kill-switch behavior runs in a child because the mask and logger are global.
    #[test]
    fn silent_disable_sets_the_bit_without_logging_and_preserves_logged_disable() {
        const CHILD_ENV: &str = "HEADROOM_GUARD_DISABLE_CHILD";
        if std::env::var_os(CHILD_ENV).is_none() {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "guard::tests::silent_disable_sets_the_bit_without_logging_and_preserves_logged_disable",
                    "--nocapture",
                ])
                .env(CHILD_ENV, "1")
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "isolated guard test failed:\nstdout: {}\nstderr: {}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }

        /// Record the log facade's public events only in this isolated child.
        struct Recorder(std::sync::Mutex<Vec<(log::Level, String)>>);
        impl log::Log for Recorder {
            fn enabled(&self, _: &log::Metadata<'_>) -> bool {
                true
            }
            fn log(&self, record: &log::Record<'_>) {
                self.0
                    .lock()
                    .unwrap()
                    .push((record.level(), record.args().to_string()));
            }
            fn flush(&self) {}
        }
        static LOGGER: Recorder = Recorder(std::sync::Mutex::new(Vec::new()));
        log::set_logger(&LOGGER).unwrap();
        log::set_max_level(log::LevelFilter::Trace);

        assert!(
            !is_disabled(Feature::Reflex),
            "the child starts with a fresh mask"
        );
        assert!(
            disable_silent(Feature::Reflex),
            "first call changes the bit"
        );
        assert!(is_disabled(Feature::Reflex));
        assert!(
            !disable_silent(Feature::Reflex),
            "second call is not a change"
        );
        assert!(
            LOGGER.0.lock().unwrap().is_empty(),
            "silent disable must not log"
        );
        disable(Feature::Reflex, "already silently disabled");
        assert!(
            LOGGER.0.lock().unwrap().is_empty(),
            "an already disabled feature stays silent"
        );
        assert!(!is_disabled(Feature::TextureBudget));
        disable(Feature::TextureBudget, "legacy first reason");
        disable(Feature::TextureBudget, "legacy second reason");
        assert!(is_disabled(Feature::TextureBudget));
        assert_eq!(
            *LOGGER.0.lock().unwrap(),
            [(
                log::Level::Warn,
                "TextureBudget disabled: legacy first reason".to_owned()
            )]
        );
    }
}
