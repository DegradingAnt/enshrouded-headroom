//! The start-up sequence, run once from `InitializeASI`.
//!
//! It is fail-closed from end to end. Nothing changes in the game until every check has passed:
//! the settings, the process, the exact build, no other climb mod, every signature, and every byte
//! at every site.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use emberkit::build_id::{self, Build};
use emberkit::patch::{read_checked, write_checked};
use emberkit::process::{self, Process};
use emberkit::scan::{find_in_sections, main_module, Pattern};
use emberkit::{guard, hooks, logging};
use log::LevelFilter;
use windows_sys::Win32::Foundation::HMODULE;
use windows_sys::Win32::System::LibraryLoader::{
    GetModuleFileNameW, GetModuleHandleExW, GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS,
    GET_MODULE_HANDLE_EX_FLAG_PIN, GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
};

use crate::coexist;
use crate::config::{self, Settings, INI_FILE_NAME};
use crate::diag;
use crate::hooks as detours;
use crate::plan;
use crate::sigs;
use crate::Feature;

/// The log file's name, next to the `.asi`.
const LOG_FILE_NAME: &str = "ember-climb.log";
/// The folder under `%LOCALAPPDATA%` the log falls back to.
const FALLBACK_DIR_NAME: &str = "ember-climb";
/// The processes this mod runs in. The server decides where players are, so it runs there too.
const TARGETS: &[Process] = &[Process::Client, Process::DedicatedServer];
/// The builds this mod was written against, by the label in `emberkit::build_id::KNOWN`.
const SUPPORTED: [&str; 4] = [
    "f68140a3-steam",
    "d7baa768-server",
    "1283531-steam",
    "1283531-server",
];
/// The section that holds the game's code.
const CODE_SECTION: &str = ".text";
/// How long after start-up to look for another climb mod again. Every plugin has loaded by then.
const LATE_CHECK: Duration = Duration::from_secs(10);

/// Taken by the first and only start-up.
static STARTED: AtomicBool = AtomicBool::new(false);

/// May this build be patched? `Err` is the one line the log gets for a build that may not.
pub fn check_build(build: &Build) -> Result<&'static str, String> {
    match build {
        Build::Known(label) if SUPPORTED.contains(label) => Ok(label),
        Build::Known(label) => Err(format!("build {label} is not supported: nothing patched")),
        Build::Unknown(hash) => Err(format!(
            "unknown build {}: nothing patched",
            build_id::to_hex(hash)
        )),
    }
}

/// The handle of this module, or `None` if Windows cannot tell.
fn own_module(flags: u32) -> Option<HMODULE> {
    let mut module: HMODULE = std::ptr::null_mut();
    // SAFETY: FROM_ADDRESS reads the address as an address only, and `start` is in this module.
    let found = unsafe {
        GetModuleHandleExW(
            GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | flags,
            start as *const () as *const u16,
            &mut module,
        )
    };
    (found != 0).then_some(module)
}

/// The folder of this `.asi`.
fn module_dir() -> Option<PathBuf> {
    let module = own_module(GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT)?;
    let mut buffer = vec![0u16; 1024];
    // SAFETY: `buffer` is a writable block of the length passed, and `module` is this module.
    let length = unsafe { GetModuleFileNameW(module, buffer.as_mut_ptr(), buffer.len() as u32) };
    let text = buffer
        .get(..length as usize)
        .filter(|text| !text.is_empty())?;
    PathBuf::from(String::from_utf16_lossy(text))
        .parent()
        .map(Path::to_path_buf)
}

/// The settings in `dir`, the warnings about them, and where they came from. With no file the
/// default file is created there, so the player has something to edit.
fn load_settings(dir: Option<&Path>) -> (Settings, Vec<String>, String) {
    let Some(dir) = dir else {
        let warning = "no folder for the settings file, using the defaults".to_owned();
        return (Settings::default(), vec![warning], "defaults".to_owned());
    };
    let path = dir.join(INI_FILE_NAME);
    let shown = path.display().to_string();
    match config::read_ini(dir) {
        Ok(Some((settings, warnings))) => (settings, warnings, shown),
        Ok(None) => {
            let mut warnings = Vec::new();
            if let Err(error) = config::write_default(dir) {
                warnings.push(format!("cannot create {shown} ({error})"));
            }
            (Settings::default(), warnings, shown)
        }
        Err(error) => {
            let warning = format!("cannot read {shown} ({error}), using the defaults");
            (Settings::default(), vec![warning], shown)
        }
    }
}

/// What these settings turn on, for the log.
pub fn summary(settings: &Settings) -> String {
    let starts: Vec<&str> = [
        (settings.walk, "walk"),
        (settings.jump, "jump"),
        (settings.sprint, "sprint"),
        (settings.glide, "glide"),
    ]
    .into_iter()
    .filter_map(|(on, name)| on.then_some(name))
    .collect();
    let starts = if starts.is_empty() {
        "nothing".to_owned()
    } else {
        starts.join(", ")
    };
    format!(
        "climbing is on: surface mask {:#x}, plane tolerance {} m, walkable below {} degrees, \
         stamina x{}, a climb starts by {starts}, diagnostics {}",
        settings.surface_mask,
        settings.plane_tolerance,
        settings.walkable_angle,
        settings.stamina_multiplier,
        if settings.diagnostics { "on" } else { "off" },
    )
}

/// Find every signature in the running game.
fn find_signatures() -> Result<sigs::Found, String> {
    let module = main_module();
    sigs::find_all(&sigs::ALL, |sig| {
        Pattern::parse(sig.pattern)
            .and_then(|pattern| find_in_sections(&module, &[CODE_SECTION], &pattern))
            .map_err(|error| error.to_string())
    })
}

/// The checks, the writes and the hooks. `Err` says why climbing stays off.
///
/// Hooks are queued first, because a queued hook changes nothing. The writes go in next. The
/// queue is enabled last, and the detours only act after that, so a failure at any point leaves
/// them forwarding to the game.
fn install(settings: &Settings) -> Result<String, String> {
    let found = find_signatures()?;
    log::info!("all {} signatures matched once", sigs::ALL.len());
    let base = main_module().base;
    let snapshot = plan::snapshot(&found, base, |addr, len| {
        // SAFETY: start-up only, on one thread, before the game runs. Each address is inside a
        // signature match or just past it, in `.text`, and `read_checked` checks the range.
        unsafe { read_checked(addr, len) }.ok()
    })?;
    let writes = plan::plan(&snapshot, settings.glide)?;
    let raycast = found.address(&sigs::RAYCAST_FN)?;
    // SAFETY: the signature matched once in a build of known hash, so this is `FUN_1409fff60`.
    detours::prepare(unsafe { detours::Runtime::new(settings, raycast) });
    // The detours live in this module, so it must stay loaded for as long as the game runs.
    own_module(GET_MODULE_HANDLE_EX_FLAG_PIN).ok_or("cannot pin this module")?;
    for site in detours::wanted(settings) {
        let target = site.target(&found, |addr, len| {
            // SAFETY: as for the snapshot above. `read_checked` checks the range, so a bad target
            // gives an error and not a fault.
            unsafe { read_checked(addr, len) }.ok()
        })?;
        // SAFETY: start-up only. Each target is the entry of the function its signature names, in
        // a build of known hash, and each detour has that function's exact type. None of the
        // first instructions is RIP-relative or a branch (tests/real_exe.rs proves that on the
        // file), and none is a jump target: they are function prologues.
        let trampoline = unsafe { hooks::create_and_queue(target, site.detour) }
            .map_err(|why| format!("{}: {why}", site.sig.name))?;
        site.slot.set(trampoline);
    }
    plan::apply_with(&writes, |addr, old, new| {
        // SAFETY: start-up only, on one thread, before the game runs this code. `plan` proved the
        // bytes are the game's, and `write_checked` checks them again.
        unsafe { write_checked(addr, old, new) }
    })?;
    let written: Vec<String> = writes
        .iter()
        .map(|write| format!("{} at {}", write.what, plan::in_exe(base, write.addr)))
        .collect();
    log::info!("wrote {}", written.join(", "));
    let enabled = hooks::apply_queued();
    if let Some((level, line)) = hooks::hooks_line(&enabled) {
        log::log!(level, "{line}");
    }
    if let Err(why) = enabled {
        let back = plan::undo_with(&writes, |addr, old, new| {
            // SAFETY: as for the writes above. The detours are not active yet, and each write
            // checks that its own bytes are there.
            unsafe { write_checked(addr, old, new) }
        });
        return Err(format!("{why}; {back}"));
    }
    detours::activate();
    // Climbing is on from here. A thread that cannot start costs a log line, not the climbing.
    if settings.diagnostics {
        if let Err(why) = diag::spawn() {
            log::warn!("{why}");
        }
    }
    if let Err(error) = spawn_late_check() {
        log::warn!("cannot start the late check for other climb mods: {error}");
    }
    Ok(summary(settings))
}

/// The line for a climb mod that was not loaded yet when start-up looked.
fn late_refusal(modules: &[PathBuf]) -> Option<String> {
    coexist::find_climb_anywhere(modules).map(coexist::refusal)
}

/// Look once more for another climb mod. Plugins load in name order, so one can load after this
/// mod. If one did, custom detour work stops. Permanent startup patches remain until exit;
/// undoing them here could overwrite code the other mod now owns. Startup preflight prevents
/// the normal loader-order conflict; this late check only reports unsupported hot-loading.
fn spawn_late_check() -> std::io::Result<()> {
    let check = || {
        let refusal = coexist::loaded_modules()
            .map(|modules| late_refusal(&modules))
            .unwrap_or_else(Some);
        if let Some(line) = refusal {
            detours::deactivate();
            log::warn!("{line}; dynamic climbing is disabled, but startup code patches remain until exit. Restart after resolving the conflict.");
        }
    };
    std::thread::Builder::new()
        .name("climb-coexist".to_owned())
        .spawn(move || {
            std::thread::sleep(LATE_CHECK);
            guard::guard(Feature::Startup, || (), check);
        })
        .map(drop)
}

/// The steps that need the game's identity.
fn start_in_game(settings: &Settings, build: &Build) {
    let label = match check_build(build) {
        Ok(label) => label,
        Err(line) => {
            log::warn!("{line}");
            return;
        }
    };
    log::info!("build {label}");
    if settings.grace_seconds > 0.0 {
        log::warn!("GraceSeconds is unavailable in this test build; seam forgiveness stays off");
    }
    let preflight = || -> Result<(), String> {
        let exe =
            std::env::current_exe().map_err(|error| format!("cannot locate the game: {error}"))?;
        let folder = exe.parent().ok_or("cannot locate the game's folder")?;
        if let Some(path) = coexist::installed_conflict(folder)? {
            return Err(coexist::refusal(&path));
        }
        let modules = coexist::loaded_modules()?;
        if let Some(path) = coexist::find_climb_anywhere(&modules) {
            return Err(coexist::refusal(path));
        }
        Ok(())
    };
    if let Err(why) = preflight() {
        log::warn!("{why}");
        return;
    }
    match install(settings) {
        Ok(line) => log::info!("{line}"),
        Err(why) => log::warn!("climbing is off: {why}"),
    }
}

/// Run the start-up once. Called from `InitializeASI`, inside the guard.
pub fn start() {
    if STARTED.swap(true, Ordering::AcqRel) {
        return;
    }
    let module = module_dir();
    let (settings, warnings, shown) = load_settings(module.as_deref());
    let log_dir = module.as_deref().unwrap_or(Path::new(""));
    logging::init_named(LOG_FILE_NAME, FALLBACK_DIR_NAME, log_dir, LevelFilter::Info);
    guard::install_panic_hook();
    log::info!("ember-climb {} loaded", env!("CARGO_PKG_VERSION"));
    log::info!("settings: {shown}");
    for warning in &warnings {
        log::warn!("{warning}");
    }
    if !settings.enabled {
        log::warn!("Enabled = 0 in {INI_FILE_NAME}: the mod does nothing");
        return;
    }
    let exe = std::env::current_exe().ok();
    let hash = exe.as_deref().map(build_id::sha256_file);
    let build = match hash {
        Some(Ok(hash)) => build_id::classify(hash),
        Some(Err(error)) => {
            log::warn!("cannot read the game exe: {error}; nothing patched");
            return;
        }
        None => {
            log::warn!("cannot find the game exe; nothing patched");
            return;
        }
    };
    let process = process::classify(exe.as_deref(), Some(&build));
    process::start_for_target(
        process,
        TARGETS,
        "ember-climb",
        |line| log::warn!("{line}"),
        || start_in_game(&settings, &build),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Both named builds are supported, and they are the two the task names.
    #[test]
    fn the_client_and_the_server_are_supported() {
        assert_eq!(
            check_build(&Build::Known("f68140a3-steam")),
            Ok("f68140a3-steam")
        );
        assert_eq!(
            check_build(&Build::Known("d7baa768-server")),
            Ok("d7baa768-server")
        );
    }

    /// Patch 1283531 needs both prediction and authority on the same native contracts.
    #[test]
    fn patch_1283531_client_and_server_are_supported() {
        for label in ["1283531-steam", "1283531-server"] {
            assert_eq!(check_build(&Build::Known(label)), Ok(label));
        }
    }

    /// Every supported label is a build `emberkit` can recognise. A typo here would switch the
    /// mod off with no error.
    #[test]
    fn every_supported_label_is_a_known_build() {
        for label in SUPPORTED {
            assert!(
                build_id::KNOWN.iter().any(|(_, known)| *known == label),
                "{label}"
            );
        }
    }

    /// An unknown build gets exactly one line, which gives its hash and says nothing changed.
    #[test]
    fn an_unknown_build_gets_one_line() {
        let line = check_build(&Build::Unknown([0xab; 32])).unwrap_err();
        assert_eq!(
            line,
            format!("unknown build {}: nothing patched", "ab".repeat(32))
        );
        assert!(!line.contains('\n'));
    }

    /// A known build that is not on the list stays off too.
    #[test]
    fn another_known_build_stays_off() {
        let line = check_build(&Build::Known("f68140a3")).unwrap_err();
        assert!(line.contains("not supported"), "{line}");
    }

    /// A climb mod that loaded after start-up is caught by the late check, and a clean list is
    /// left alone.
    #[test]
    fn the_late_check_finds_a_mod_that_loaded_after_this_one() {
        let late = [
            PathBuf::from(r"C:\game\plugins\ember-climb.asi"),
            PathBuf::from(r"C:\game\plugins\zz-climb-anywhere.asi"),
        ];
        let line = late_refusal(&late).unwrap();
        assert!(line.contains("zz-climb-anywhere.asi"), "{line}");
        assert!(line.contains("never run together"), "{line}");
        assert_eq!(late_refusal(&late[..1]), None);
    }

    /// The client and the server are both targets, so the mod runs in both.
    #[test]
    fn both_game_processes_are_targets() {
        assert!(TARGETS.contains(&Process::Client));
        assert!(TARGETS.contains(&Process::DedicatedServer));
    }

    /// The summary tells a tester what is on, so a log can be read by itself.
    #[test]
    fn the_summary_says_what_is_on() {
        assert_eq!(
            summary(&Settings::default()),
            "climbing is on: surface mask 0x124, plane tolerance 0.45 m, walkable below 50 \
             degrees, stamina x1, a climb starts by walk, jump, sprint, diagnostics on"
        );
    }

    /// With every start off the summary says so.
    #[test]
    fn the_summary_says_when_nothing_starts_a_climb() {
        let settings = Settings {
            walk: false,
            jump: false,
            sprint: false,
            ..Settings::default()
        };
        assert!(summary(&settings).contains("a climb starts by nothing"));
    }

    /// This module can find its own folder, which is where the settings live.
    #[test]
    fn the_module_folder_is_found() {
        let dir = module_dir().unwrap();
        assert!(dir.is_absolute(), "{dir:?}");
    }

    /// A first run leaves a settings file to edit, and uses the defaults.
    #[test]
    fn a_first_run_creates_the_settings_file() {
        let dir = std::env::temp_dir().join(format!("ember-climb-run-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let _ = std::fs::remove_file(dir.join(INI_FILE_NAME));
        let (settings, warnings, shown) = load_settings(Some(&dir));
        assert_eq!(settings, Settings::default());
        assert_eq!(warnings, Vec::<String>::new());
        assert!(shown.ends_with(INI_FILE_NAME), "{shown}");
        assert!(dir.join(INI_FILE_NAME).is_file());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A player's file is read, and its warnings come back.
    #[test]
    fn a_players_file_is_read() {
        let dir = std::env::temp_dir().join(format!("ember-climb-read-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(INI_FILE_NAME), "[Climb]\nEnabled=0\nOops=1\n").unwrap();
        let (settings, warnings, _) = load_settings(Some(&dir));
        assert!(!settings.enabled);
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// With no folder the mod still starts, on the defaults, and says so.
    #[test]
    fn no_folder_gives_the_defaults() {
        let (settings, warnings, shown) = load_settings(None);
        assert_eq!(settings, Settings::default());
        assert_eq!(warnings.len(), 1);
        assert_eq!(shown, "defaults");
    }

    /// A folder that cannot hold the file gives a warning, not a failure.
    #[test]
    fn an_unusable_folder_gives_the_defaults_and_a_warning() {
        let missing = std::env::temp_dir()
            .join("ember-climb-no-such-folder")
            .join("deep");
        let (settings, warnings, _) = load_settings(Some(&missing));
        assert_eq!(settings, Settings::default());
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].contains("cannot create"), "{warnings:?}");
    }
}
