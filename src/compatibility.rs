//! Read-only startup notices for optional third-party tools.

use std::path::Path;

/// Provisional detection never establishes installed patches or live process state.
const TWEAKER_NOTICE: &str = "Tweaker files detected: possible installation, activity not verified; GI and fog remain owned by Tweaker; combined runtime compatibility is unverified";

/// Report optional tool markers once during enabled startup, never from a hook.
pub(crate) fn report_tweaker(exe: Option<&Path>) {
    if let Some(notice) = tweaker_notice(exe) {
        log::info!("{notice}");
    }
}

/// Return a notice only for both file markers beside the absolute executable path.
fn tweaker_notice(exe: Option<&Path>) -> Option<&'static str> {
    let directory = exe.filter(|path| path.is_absolute())?.parent()?;
    let root = directory.join("_debris_mod");
    (root.join("state.json").is_file() && root.join("tools/live.py").is_file())
        .then_some(TWEAKER_NOTICE)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Prevent parallel tests in this process from sharing a fixture directory.
    static NEXT: AtomicUsize = AtomicUsize::new(0);

    /// A unique, test-owned directory directly inside the system temporary directory.
    struct Fixture(PathBuf);

    impl Fixture {
        /// Create a new directory without reusing or clearing a prior fixture.
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "headroom-tweaker-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }

        /// Synthetic executable location; no executable needs to be opened or created.
        fn exe(&self) -> PathBuf {
            self.0.join("enshrouded.exe")
        }

        /// Marker root in this fixture's executable directory.
        fn markers(&self) -> PathBuf {
            self.0.join("_debris_mod")
        }

        /// Create only the parent directories used by the markers.
        fn parents(&self) {
            std::fs::create_dir_all(self.markers().join("tools")).unwrap();
        }
    }

    impl Drop for Fixture {
        /// Remove only this known fixture directly under the system temporary directory.
        fn drop(&mut self) {
            let temporary = std::env::temp_dir();
            if self.0.parent() == Some(temporary.as_path())
                && self
                    .0
                    .file_name()
                    .is_some_and(|name| name.to_string_lossy().starts_with("headroom-tweaker-"))
            {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
    }

    /// Neither marker alone establishes even a possible installation.
    #[test]
    fn absent_and_partial_markers_emit_no_notice() {
        let fixture = Fixture::new();
        assert_eq!(tweaker_notice(Some(&fixture.exe())), None);
        fixture.parents();
        let state = fixture.markers().join("state.json");
        std::fs::write(&state, b"not parsed").unwrap();
        assert_eq!(tweaker_notice(Some(&fixture.exe())), None);
        std::fs::remove_file(&state).unwrap();
        std::fs::write(fixture.markers().join("tools/live.py"), b"not executed").unwrap();
        assert_eq!(tweaker_notice(Some(&fixture.exe())), None);
    }

    /// The production notice helper identifies files without parsing, executing or changing them.
    #[test]
    fn complete_markers_report_only_possible_installation_and_stay_unchanged() {
        let fixture = Fixture::new();
        fixture.parents();
        let state = fixture.markers().join("state.json");
        let live = fixture.markers().join("tools/live.py");
        std::fs::write(&state, b"invalid JSON is deliberately not parsed").unwrap();
        std::fs::write(&live, b"raise Exception('must never execute')").unwrap();
        let notice = tweaker_notice(Some(&fixture.exe()));
        assert_eq!(notice, Some(TWEAKER_NOTICE));
        assert!(notice.unwrap().contains("activity not verified"));
        assert_eq!(
            std::fs::read(&state).unwrap(),
            b"invalid JSON is deliberately not parsed"
        );
        assert_eq!(
            std::fs::read(&live).unwrap(),
            b"raise Exception('must never execute')"
        );
    }

    /// Directory names alone must not count as installation files.
    #[test]
    fn directories_do_not_count_as_markers() {
        let state_directory = Fixture::new();
        state_directory.parents();
        std::fs::create_dir(state_directory.markers().join("state.json")).unwrap();
        std::fs::write(state_directory.markers().join("tools/live.py"), b"marker").unwrap();
        assert_eq!(tweaker_notice(Some(&state_directory.exe())), None);
        let live_directory = Fixture::new();
        live_directory.parents();
        std::fs::write(live_directory.markers().join("state.json"), b"marker").unwrap();
        std::fs::create_dir(live_directory.markers().join("tools/live.py")).unwrap();
        assert_eq!(tweaker_notice(Some(&live_directory.exe())), None);
    }

    /// Unknown and relative executable paths must not make detection depend on the working directory.
    #[test]
    fn missing_or_relative_executable_path_is_unknown() {
        assert_eq!(tweaker_notice(None), None);
        assert_eq!(tweaker_notice(Some(Path::new("enshrouded.exe"))), None);
    }

    /// Markers elsewhere must not be mistaken for files beside the executable.
    #[test]
    fn markers_must_be_beside_the_executable() {
        let fixture = Fixture::new();
        fixture.parents();
        std::fs::write(fixture.markers().join("state.json"), b"marker").unwrap();
        std::fs::write(fixture.markers().join("tools/live.py"), b"marker").unwrap();
        assert_eq!(
            tweaker_notice(Some(&fixture.0.join("other/enshrouded.exe"))),
            None
        );
    }
}
