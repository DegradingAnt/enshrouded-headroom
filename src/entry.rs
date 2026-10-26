//! The start-up sequence, run once from `InitializeASI`.
//!
//! Every feature adds its own step to [`start`] and asks [`code_patches_allowed`] whether it may
//! touch the game's code.
//!
//! Inside this module `log::` is the `log` crate; the plugin's own logging module is
//! `crate::log`.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;

use crate::build_id::{self, Build};
use crate::config::{self, Config, INI_FILE_NAME};
use crate::features;
use crate::guard;

/// Sub-folder of `%LOCALAPPDATA%` for the settings file when there is no module folder. It is the
/// same folder the log falls back to.
const FALLBACK_DIR_NAME: &str = "enshrouded-headroom";

/// Set once by [`start`]. Unset (before `start`, or if it stopped early) reads as "not allowed".
static CODE_PATCHES: OnceLock<bool> = OnceLock::new();

/// May code patches and code hooks go into the game? Only on a known build, or when the player
/// set `AllowUnknownBuild = 1`.
pub fn decide_code_patches(build: &Build, allow_unknown: bool) -> bool {
    matches!(build, Build::Known(_)) || allow_unknown
}

/// The answer [`start`] reached. `false` until `start` has got that far.
pub fn code_patches_allowed() -> bool {
    read_flag(&CODE_PATCHES)
}

/// `cell`'s value, or `false` while it is unset.
fn read_flag(cell: &OnceLock<bool>) -> bool {
    cell.get().copied().unwrap_or(false)
}

/// Has `flag` been taken before? The first call on a flag says `true`, every later call `false`.
fn first_call(flag: &AtomicBool) -> bool {
    !flag.swap(true, Ordering::AcqRel)
}

/// The folder for the settings file: the module folder, else `%LOCALAPPDATA%\enshrouded-headroom`,
/// else `None`. Both must be absolute paths: a relative path would land in whatever folder the
/// game happens to be running from.
fn settings_dir(module_dir: Option<&Path>, local_app_data: Option<&OsStr>) -> Option<PathBuf> {
    if let Some(dir) = module_dir.filter(|dir| dir.is_absolute()) {
        return Some(dir.to_path_buf());
    }
    let base = Path::new(local_app_data?);
    base.is_absolute().then(|| base.join(FALLBACK_DIR_NAME))
}

/// Read (or create) the settings file in `dir`. With no folder, the defaults and a warning.
fn load_settings(dir: Option<&Path>) -> (Config, Vec<String>) {
    match dir {
        Some(dir) => config::load_or_create(dir),
        None => (
            Config::default(),
            vec![
                "no folder for the settings file (no module folder, no %LOCALAPPDATA%); \
                 using the defaults"
                    .to_owned(),
            ],
        ),
    }
}

/// Hash the exe at `exe` and classify the hash. `Err` says why that was not possible.
fn identify(exe: Option<&Path>) -> Result<Build, String> {
    let path = exe.ok_or_else(|| "cannot find the path of the game exe".to_owned())?;
    build_id::sha256_file(path)
        .map(build_id::classify)
        .map_err(|error| format!("cannot read {}: {error}", path.display()))
}

/// The log line for the build: `game build: <label|unknown sha256 …>; code patches <on|off>`.
/// (An exe that could not be hashed has no hash to show; the reason stands in its place.)
fn build_line(build: &Result<Build, String>, allowed: bool) -> String {
    let patches = if allowed { "on" } else { "off" };
    match build {
        Ok(Build::Known(label)) => format!("game build: {label}; code patches {patches}"),
        Ok(Build::Unknown(hash)) => format!(
            "game build: unknown sha256 {}; code patches {patches}",
            build_id::to_hex(hash)
        ),
        Err(why) => format!("game build: unknown, not hashed ({why}); code patches {patches}"),
    }
}

/// A second line for a build that is not in the known table, or `None` for a known build.
fn unknown_build_advice(build: &Result<Build, String>, allowed: bool) -> Option<String> {
    if matches!(build, Ok(Build::Known(_))) {
        None
    } else if allowed {
        Some(
            "AllowUnknownBuild = 1: code patches are on for a build this plugin has not been \
             checked against; each patch still checks the game's bytes before it writes"
                .to_owned(),
        )
    } else {
        Some(format!(
            "code patches stay off on this build; to try them anyway, set AllowUnknownBuild = 1 \
             under [General] in {INI_FILE_NAME}"
        ))
    }
}

/// What the build step concluded.
#[derive(Debug, PartialEq, Eq)]
enum Verdict {
    /// `Enabled = 0`: the mod does nothing, and the exe is not even read.
    Disabled,
    /// The exe was identified (or could not be) and the code-patch decision made.
    Checked {
        build: Result<Build, String>,
        allowed: bool,
    },
}

/// Steps 6 and 7: with `Enabled = 0` stop at once; otherwise call `identify` (which reads the exe)
/// and decide whether code patches are allowed. An exe that cannot be identified counts as unknown.
fn judge(cfg: &Config, identify: impl FnOnce() -> Result<Build, String>) -> Verdict {
    if !cfg.general.enabled {
        return Verdict::Disabled;
    }
    let allow_unknown = cfg.general.allow_unknown_build;
    let build = identify();
    let allowed = build.as_ref().map_or(allow_unknown, |build| {
        decide_code_patches(build, allow_unknown)
    });
    Verdict::Checked { build, allowed }
}

/// Store the verdict's code-patch answer in `cell`. A `Disabled` verdict stores nothing, so the
/// cell stays unset and reads as "not allowed".
fn record(verdict: &Verdict, cell: &OnceLock<bool>) {
    if let Verdict::Checked { allowed, .. } = verdict {
        // `start` runs once, so the cell is empty here; an `Err` could only mean "already set".
        let _ = cell.set(*allowed);
    }
}

/// The log level of the build line: `Info` for a known build, `Warn` for any other, so that an
/// unknown build's hash is in the log at the default level and at `LogLevel = warn`.
fn build_level(build: &Result<Build, String>) -> log::Level {
    match build {
        Ok(Build::Known(_)) => log::Level::Info,
        Ok(Build::Unknown(_)) | Err(_) => log::Level::Warn,
    }
}

/// Set once the first call to [`start`] has begun.
static STARTED: AtomicBool = AtomicBool::new(false);

/// Run the start-up steps in order. Called once, from `InitializeASI`, which catches any panic.
///
/// A panic part-way leaves the later steps undone and [`code_patches_allowed`] unset, so `false`.
pub fn start() {
    if !first_call(&STARTED) {
        log::warn!("start-up ran twice; the second run is ignored");
        return;
    }

    // 1. Where the module is, and 2. the settings. The settings come first because they hold the
    // log level.
    let module = crate::module_dir();
    let local_app_data = std::env::var_os("LOCALAPPDATA");
    let settings = settings_dir(module.as_deref(), local_app_data.as_deref());
    let (cfg, warnings) = load_settings(settings.as_deref());

    // 3. The log. No usable module folder is an empty path, which sends it to its own fallback.
    let log_dir = module
        .as_deref()
        .filter(|dir| dir.is_absolute())
        .unwrap_or(Path::new(""));
    crate::log::init(log_dir, cfg.general.log_level.into());

    // 4. From here on a panic goes to the log, not to a stderr that nobody reads.
    guard::install_panic_hook();

    // 5. What has been learned so far.
    log::info!("enshrouded-headroom {} loaded", env!("CARGO_PKG_VERSION"));
    for warning in &warnings {
        log::warn!("{warning}");
    }

    // 6. Which build is this, and 7. may code be patched in it. (`Enabled = 0` stops here, before
    // the exe is read.) Nothing reads the answer between `judge` and `record`.
    let verdict = judge(&cfg, || identify(crate::exe_path().as_deref()));
    record(&verdict, &CODE_PATCHES);
    let Verdict::Checked { build, allowed } = &verdict else {
        log::warn!("Enabled = 0 in {INI_FILE_NAME}: the mod does nothing");
        return;
    };
    log::log!(build_level(build), "{}", build_line(build, *allowed));
    if let Some(advice) = unknown_build_advice(build, *allowed) {
        log::warn!("{advice}");
    }

    // Later tasks add their start-up steps below this line, in the order the plan gives.

    // 8. The texture budget. It patches data (a table in `.rdata`), not code, so it does not wait
    // for `code_patches_allowed`: on any build it needs the whole 80-byte table to match. It logs
    // its own line, including why it is off.
    features::texture_budget::init(&cfg.general.texture_budget);

    // 9. Async compute. It patches code, so it asks `code_patches_allowed` (set above) before it
    // touches the game, and it logs its own line, including why it is off.
    features::async_compute::init(&cfg.general.async_compute);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::build_id::KNOWN;

    #[test]
    fn unknown_build_disables_code_features() {
        assert!(!decide_code_patches(&Build::Unknown([0; 32]), false));
        assert!(decide_code_patches(&Build::Unknown([0; 32]), true));
        assert!(decide_code_patches(&Build::Known("f68140a3"), false));
    }

    #[test]
    fn the_flag_reads_false_until_start_sets_it() {
        let cell = OnceLock::new();
        assert!(!read_flag(&cell));
        cell.set(true).unwrap();
        assert!(read_flag(&cell));
        let off = OnceLock::new();
        off.set(false).unwrap();
        assert!(!read_flag(&off));
    }

    const GAME_DIR: &str = r"C:\Games\Enshrouded\plugins";
    const LOCAL: &str = r"C:\Users\someone\AppData\Local";

    #[test]
    fn settings_live_next_to_the_module() {
        let got = settings_dir(Some(Path::new(GAME_DIR)), Some(OsStr::new(LOCAL)));
        assert_eq!(got, Some(PathBuf::from(GAME_DIR)));
    }

    #[test]
    fn without_a_module_folder_settings_go_to_local_app_data() {
        let want = Some(Path::new(LOCAL).join("enshrouded-headroom"));
        assert_eq!(settings_dir(None, Some(OsStr::new(LOCAL))), want);
        // An empty path is the same as none (T3-7).
        assert_eq!(
            settings_dir(Some(Path::new("")), Some(OsStr::new(LOCAL))),
            want
        );
    }

    #[test]
    fn settings_dir_is_never_a_relative_path() {
        let local = Some(OsStr::new(LOCAL));
        // A relative module folder is not used; the fallback is.
        assert_eq!(
            settings_dir(Some(Path::new(r"plugins")), local),
            Some(Path::new(LOCAL).join("enshrouded-headroom"))
        );
        // No usable fallback either: no folder at all, not "the current folder".
        assert_eq!(settings_dir(None, None), None);
        assert_eq!(settings_dir(None, Some(OsStr::new(""))), None);
        assert_eq!(settings_dir(None, Some(OsStr::new(r"AppData\Local"))), None);
        assert_eq!(
            settings_dir(Some(Path::new(r"plugins")), Some(OsStr::new(r"Local"))),
            None
        );
    }

    #[test]
    fn no_settings_folder_gives_the_defaults_and_a_warning() {
        // `load_settings(None)` has no path to read or write, so it cannot touch any file.
        let (cfg, warnings) = load_settings(None);
        assert_eq!(cfg, Config::default());
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].contains("settings"), "{}", warnings[0]);
    }

    fn hash_of_abc() -> [u8; 32] {
        let mut out = [0u8; 32];
        let text = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
        for (i, byte) in out.iter_mut().enumerate() {
            *byte = u8::from_str_radix(&text[2 * i..2 * i + 2], 16).unwrap();
        }
        out
    }

    fn scratch_file(label: &str, bytes: &[u8]) -> PathBuf {
        let dir = std::env::temp_dir().join("enshrouded-headroom-tests");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("entry-{}-{label}.bin", std::process::id()));
        std::fs::write(&path, bytes).unwrap();
        path
    }

    #[test]
    fn identify_hashes_the_exe() {
        let path = scratch_file("abc", b"abc");
        let got = identify(Some(&path));
        let _ = std::fs::remove_file(&path);
        // "abc" is not a game build.
        assert_eq!(got, Ok(Build::Unknown(hash_of_abc())));
    }

    #[test]
    fn identify_says_why_it_could_not() {
        let missing = std::env::temp_dir()
            .join("enshrouded-headroom-tests")
            .join("no-such-exe.exe");
        let why = identify(Some(&missing)).unwrap_err();
        assert!(why.contains("no-such-exe.exe"), "{why}");
        assert!(identify(None).unwrap_err().contains("path"));
    }

    #[test]
    fn the_build_line_for_a_known_build() {
        assert_eq!(
            build_line(&Ok(Build::Known("f68140a3")), true),
            "game build: f68140a3; code patches on"
        );
    }

    #[test]
    fn the_build_line_for_an_unknown_build_names_the_hash() {
        let hash = hash_of_abc();
        let sha = build_id::to_hex(&hash);
        assert_eq!(
            build_line(&Ok(Build::Unknown(hash)), false),
            format!("game build: unknown sha256 {sha}; code patches off")
        );
        assert_eq!(
            build_line(&Ok(Build::Unknown(hash)), true),
            format!("game build: unknown sha256 {sha}; code patches on")
        );
    }

    #[test]
    fn the_build_line_for_an_exe_that_could_not_be_read() {
        assert_eq!(
            build_line(&Err("cannot read x".to_owned()), false),
            "game build: unknown, not hashed (cannot read x); code patches off"
        );
    }

    #[test]
    fn advice_only_for_builds_that_are_not_known() {
        let known = Ok(Build::Known(KNOWN[0].1));
        assert_eq!(unknown_build_advice(&known, true), None);
        let unknown = Ok(Build::Unknown([7; 32]));
        let off = unknown_build_advice(&unknown, false).unwrap();
        assert!(off.contains("AllowUnknownBuild = 1"), "{off}");
        assert!(off.contains(INI_FILE_NAME), "{off}");
        let on = unknown_build_advice(&unknown, true).unwrap();
        assert!(on.contains("AllowUnknownBuild"), "{on}");
        assert_ne!(on, off);
        assert!(unknown_build_advice(&Err(String::new()), false).is_some());
    }

    /// A `Config` with the two switches the build step looks at.
    fn cfg_with(enabled: bool, allow_unknown_build: bool) -> Config {
        let mut cfg = Config::default();
        cfg.general.enabled = enabled;
        cfg.general.allow_unknown_build = allow_unknown_build;
        cfg
    }

    #[test]
    fn enabled_0_stops_before_the_exe_is_read_and_leaves_the_flag_unset() {
        // Even with AllowUnknownBuild = 1, so the stop is not just "patches are off anyway".
        let cfg = cfg_with(false, true);
        let exe_was_read = std::cell::Cell::new(false);
        let verdict = judge(&cfg, || {
            exe_was_read.set(true);
            Ok(Build::Known("f68140a3"))
        });
        assert_eq!(verdict, Verdict::Disabled);
        assert!(
            !exe_was_read.get(),
            "the exe must not be read when Enabled = 0"
        );
        let cell = OnceLock::new();
        record(&verdict, &cell);
        assert_eq!(cell.get(), None, "a stopped start-up must not set the flag");
        assert!(!read_flag(&cell));
    }

    #[test]
    fn enabled_1_identifies_the_build_and_decides_the_flag() {
        let known = Ok(Build::Known("f68140a3"));
        let unknown = Ok(Build::Unknown([7; 32]));
        let unreadable: Result<Build, String> = Err("cannot read x".to_owned());
        // (build, AllowUnknownBuild, expected answer)
        let cases = [
            (known, false, true),
            (unknown.clone(), false, false),
            (unknown, true, true),
            (unreadable.clone(), false, false),
            (unreadable, true, true),
        ];
        for (build, allow_unknown, want) in cases {
            let verdict = judge(&cfg_with(true, allow_unknown), || build.clone());
            assert_eq!(
                verdict,
                Verdict::Checked {
                    build: build.clone(),
                    allowed: want
                }
            );
            let cell = OnceLock::new();
            record(&verdict, &cell);
            assert_eq!(
                cell.get(),
                Some(&want),
                "{build:?}, AllowUnknownBuild={allow_unknown}"
            );
        }
    }

    #[test]
    fn only_a_known_build_is_logged_below_warn() {
        use log::Level;
        assert_eq!(build_level(&Ok(Build::Known("f68140a3"))), Level::Info);
        assert_eq!(build_level(&Ok(Build::Unknown([7; 32]))), Level::Warn);
        assert_eq!(build_level(&Err("cannot read x".to_owned())), Level::Warn);
    }

    #[test]
    fn only_the_first_start_runs() {
        let flag = AtomicBool::new(false);
        assert!(first_call(&flag));
        assert!(!first_call(&flag));
        assert!(!first_call(&flag));
    }
}
