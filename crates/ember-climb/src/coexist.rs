//! Refuse to run next to ClimbAnywhere. Two mods that change the same climb code must never run
//! together.
//!
//! This reads installed binary names in the standard loader folders and loaded module names.
//! It never opens, reads or disassembles a third-party binary. ClimbAnywhere has been seen under three names: an `.asi` with
//! its own name, a chained `dinput8Hooked.dll`, and a `dinput8.dll` in `plugins\`. Plugins load in
//! name order, so this mod can start first. That is why start-up looks once more a little later.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use windows_sys::Win32::Foundation::HMODULE;
use windows_sys::Win32::System::LibraryLoader::GetModuleFileNameW;
use windows_sys::Win32::System::ProcessStatus::K32EnumProcessModules;
use windows_sys::Win32::System::Threading::GetCurrentProcess;

/// The name ClimbAnywhere's DLL is chained under.
const CHAINED_NAME: &str = "dinput8hooked.dll";
/// The loader's name. A real loader sits next to the exe, so this name in `plugins\` is not one.
/// ClimbAnywhere's server build has this name next to the exe too. It then keeps the loader out, so
/// this mod does not start and has nothing to refuse.
const LOADER_NAME: &str = "dinput8.dll";
/// The folder plugins load from.
const PLUGIN_FOLDER: &str = "plugins";
/// The most modules looked at. A game has a few hundred.
const MODULE_LIMIT: usize = 4096;

/// A name in lower case, or an empty text if there is none.
fn lower(name: Option<&OsStr>) -> String {
    name.and_then(OsStr::to_str)
        .map(str::to_ascii_lowercase)
        .unwrap_or_default()
}

/// Is this loaded module ClimbAnywhere? Any spelling of the name counts, and so do the two names
/// it is installed under.
pub fn is_climb_anywhere(path: &Path) -> bool {
    let file = lower(path.file_name());
    let folder = lower(path.parent().and_then(Path::file_name));
    let letters: String = file.chars().filter(char::is_ascii_alphanumeric).collect();
    file == CHAINED_NAME
        || letters.contains("climbanywhere")
        || (file == LOADER_NAME && folder == PLUGIN_FOLDER)
}

/// The first path in `modules` that is ClimbAnywhere.
pub fn find_climb_anywhere(modules: &[PathBuf]) -> Option<&Path> {
    modules
        .iter()
        .map(PathBuf::as_path)
        .find(|path| is_climb_anywhere(path))
}

/// A conflicting binary waiting in the game's standard loader folders.
/// Only directory entries are read; no third-party binary is opened.
pub fn installed_conflict(game_dir: &Path) -> Result<Option<PathBuf>, String> {
    let mut folders = vec![game_dir.to_path_buf()];
    for name in ["plugins", "scripts", "update"] {
        let folder = game_dir.join(name);
        // Ultimate ASI Loader can load direct subfolders as well as the folder itself.
        folders.extend(subfolders(&folder)?);
        folders.push(folder);
    }
    for folder in folders {
        for path in entries(&folder)? {
            let extension = lower(path.extension());
            if matches!(extension.as_str(), "asi" | "dll") && is_climb_anywhere(&path) {
                return Ok(Some(path));
            }
        }
    }
    Ok(None)
}

/// Directory entries, failing closed on an unreadable folder. An absent optional folder is empty.
fn entries(folder: &Path) -> Result<Vec<PathBuf>, String> {
    let entries = match std::fs::read_dir(folder) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(format!(
                "cannot check climb conflicts in {}: {error}",
                folder.display()
            ))
        }
    };
    let mut paths = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| format!("cannot list {}: {error}", folder.display()))?;
        if paths.len() >= MODULE_LIMIT {
            return Err(format!(
                "too many entries in {} to check climb conflicts",
                folder.display()
            ));
        }
        paths.push(entry.path());
    }
    Ok(paths)
}

/// The direct subfolders a recursive loader option can scan.
fn subfolders(folder: &Path) -> Result<Vec<PathBuf>, String> {
    entries(folder)?
        .into_iter()
        .try_fold(Vec::new(), |mut folders, path| {
            let metadata = std::fs::metadata(&path)
                .map_err(|error| format!("cannot inspect {}: {error}", path.display()))?;
            if metadata.is_dir() {
                folders.push(path);
            }
            Ok(folders)
        })
}

/// The log line for a refusal.
pub fn refusal(path: &Path) -> String {
    format!(
        "ember-climb is off: {} is present. ClimbAnywhere installs under that name, and two climb \
         mods must never run together. Remove ClimbAnywhere (see the README), then restart.",
        path.display()
    )
}

/// The path of `module`, or `None` if Windows cannot tell.
fn module_path(module: HMODULE) -> Option<PathBuf> {
    let mut buffer = vec![0u16; 1024];
    // SAFETY: `buffer` is a writable block of the length passed, and `module` was just listed by
    // Windows. A stale handle gives an error, not a fault.
    let length = unsafe { GetModuleFileNameW(module, buffer.as_mut_ptr(), buffer.len() as u32) };
    let text = buffer
        .get(..length as usize)
        .filter(|text| !text.is_empty() && text.len() < buffer.len())?;
    Some(PathBuf::from(String::from_utf16_lossy(text)))
}

/// The paths of every module loaded in this process.
pub fn loaded_modules() -> Result<Vec<PathBuf>, String> {
    let mut modules: Vec<HMODULE> = vec![std::ptr::null_mut(); MODULE_LIMIT];
    let size = u32::try_from(std::mem::size_of_val(modules.as_slice())).unwrap_or(0);
    let mut needed = 0u32;
    // SAFETY: `modules` is a writable block of `size` bytes, `needed` is a writable u32, and
    // `GetCurrentProcess` gives a handle that needs no closing.
    let listed = unsafe {
        K32EnumProcessModules(GetCurrentProcess(), modules.as_mut_ptr(), size, &mut needed)
    };
    if listed == 0 || needed == 0 || needed > size {
        return Err("cannot list every loaded module; nothing patched".to_owned());
    }
    let count = needed as usize / std::mem::size_of::<HMODULE>();
    modules
        .into_iter()
        .take(count)
        .map(|module| {
            module_path(module).ok_or_else(|| "cannot read a loaded module's full name".to_owned())
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Is the module with this name, next to the exe, ClimbAnywhere?
    fn named(name: &str) -> bool {
        is_climb_anywhere(&Path::new(r"C:\game").join(name))
    }

    /// A later-loading plugin must be refused before this mod writes any game bytes.
    #[test]
    fn an_installed_late_plugin_is_found_before_it_loads() {
        let dir = std::env::temp_dir().join(format!("ember-climb-late-{}", std::process::id()));
        let plugins = dir.join("plugins");
        std::fs::create_dir_all(&plugins).unwrap();
        let pending = plugins.join("zz-climb-anywhere.asi");
        std::fs::write(&pending, b"inert test file, never loaded").unwrap();
        assert_eq!(installed_conflict(&dir), Ok(Some(pending)));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A disabled backup and old settings cannot activate another climbing mod.
    #[test]
    fn disabled_files_and_settings_do_not_block_startup() {
        let dir = std::env::temp_dir().join(format!("ember-climb-disabled-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("plugins")).unwrap();
        for file in [
            "climb-anywhere.asi.off",
            "climb-anywhere.ini",
            "climb-anywhere.log",
        ] {
            std::fs::write(dir.join("plugins").join(file), b"inert").unwrap();
        }
        std::fs::write(dir.join("dinput8.dll"), b"inert loader filename").unwrap();
        assert_eq!(installed_conflict(&dir), Ok(None));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A chained proxy can load outside the ASI ordering, so its installed name also blocks.
    #[test]
    fn an_installed_chained_proxy_is_found() {
        let dir = std::env::temp_dir().join(format!("ember-climb-chained-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let pending = dir.join("dinput8Hooked.dll");
        std::fs::write(&pending, b"inert").unwrap();
        assert_eq!(installed_conflict(&dir), Ok(Some(pending)));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Every spelling a person might use counts.
    #[test]
    fn the_name_is_found_in_any_spelling() {
        for name in [
            "climb-anywhere.asi",
            "ClimbAnywhere.dll",
            "climb_anywhere.asi",
            "CLIMB ANYWHERE.DLL",
            "ClimbAnywhere-1.2.asi",
        ] {
            assert!(named(name), "{name}");
        }
    }

    /// The chained name counts in any case, and only as the whole name.
    #[test]
    fn the_chained_name_is_found() {
        assert!(named("dinput8Hooked.dll"));
        assert!(named("DINPUT8HOOKED.DLL"));
        assert!(!named("not-dinput8Hooked.dll"));
    }

    /// A `dinput8.dll` in `plugins\` is no loader: a loader sits next to the exe.
    #[test]
    fn a_loader_name_in_plugins_is_found() {
        assert!(is_climb_anywhere(Path::new(r"C:\game\plugins\dinput8.dll")));
        assert!(is_climb_anywhere(Path::new(r"C:\game\PLUGINS\DINPUT8.DLL")));
        assert!(!is_climb_anywhere(Path::new(r"C:\game\dinput8.dll")));
    }

    /// The loader, the game, this mod and the other mods are left alone.
    #[test]
    fn other_modules_are_left_alone() {
        for name in [
            "dinput8.dll",
            "enshrouded.exe",
            "ember-climb.asi",
            "enshrouded-headroom.asi",
            "climb.dll",
            "anywhere.dll",
        ] {
            assert!(!named(name), "{name}");
        }
    }

    /// The file name decides, not a folder that happens to hold the name.
    #[test]
    fn the_path_is_found_by_its_file_name() {
        let modules = [
            PathBuf::from(r"C:\game\enshrouded.exe"),
            PathBuf::from(r"C:\game\dinput8Hooked.dll"),
            PathBuf::from(r"C:\game\plugins\ember-climb.asi"),
        ];
        assert_eq!(
            find_climb_anywhere(&modules),
            Some(Path::new(r"C:\game\dinput8Hooked.dll"))
        );
        let clean = [PathBuf::from(r"C:\ClimbAnywhere\enshrouded.exe")];
        assert_eq!(find_climb_anywhere(&clean), None);
        assert_eq!(find_climb_anywhere(&[]), None);
    }

    /// The refusal names the file, says why and says what to do.
    #[test]
    fn the_refusal_says_why_and_what_to_do() {
        let line = refusal(Path::new(r"C:\game\dinput8Hooked.dll"));
        assert!(line.contains(r"C:\game\dinput8Hooked.dll"), "{line}");
        assert!(line.contains("never run together"), "{line}");
        assert!(line.contains("Remove ClimbAnywhere"), "{line}");
    }

    /// This test process has modules, and Windows lists them, this test binary among them.
    #[test]
    fn the_real_list_holds_this_process() {
        let exe = std::env::current_exe().unwrap();
        let modules = loaded_modules().unwrap();
        assert!(modules.len() > 1, "{modules:?}");
        assert!(
            modules
                .iter()
                .any(|path| path.file_name() == exe.file_name()),
            "{modules:?}"
        );
        assert_eq!(find_climb_anywhere(&modules), None);
    }
}
