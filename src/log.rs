//! File logging for the plugin.
//!
//! [`init`] opens `enshrouded-headroom.log` next to the `.asi` (or in
//! `%LOCALAPPDATA%\enshrouded-headroom\` when that folder cannot be written) and installs it as
//! the process-wide `log` logger. After that, `log::info!` and friends write to the file from
//! any thread. Each record is flushed as it is written, so the last lines survive a crash.
//!
//! Inside this crate, `log::` in a module other than the crate root means the `log` crate; this
//! module is `crate::log`. At the crate root, write `::log::` for the crate.

use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use log::LevelFilter;

/// Name of the log file, in whichever folder [`init`] picks.
pub const LOG_FILE_NAME: &str = "enshrouded-headroom.log";

/// Sub-folder of `%LOCALAPPDATA%` used when the preferred folder is not writable.
const FALLBACK_DIR_NAME: &str = "enshrouded-headroom";

/// Set once a logger has been installed (or is being installed) by this module.
static LOGGER_CLAIMED: AtomicBool = AtomicBool::new(false);

/// Start logging and return the folder that holds the log file.
///
/// The result is `preferred_dir` when the log file can be created there, else
/// `%LOCALAPPDATA%\enshrouded-headroom\` (created if missing), with a warning in the log that
/// says why. An empty `preferred_dir` means "no preferred folder" and goes straight to the
/// fallback. If neither folder works, logging stays off and the fallback path is still returned
/// (or `preferred_dir` when `%LOCALAPPDATA%` is not set). It never panics.
///
/// `level` is the highest level that is written. It is applied with `log::set_max_level`, so
/// later code can raise or lower it by calling that function again.
///
/// The `log` crate accepts one logger per process. A second call still picks and returns a
/// folder but leaves the first logger (and its file) alone.
pub fn init(preferred_dir: &Path, level: LevelFilter) -> PathBuf {
    let fallback = std::env::var_os("LOCALAPPDATA")
        .filter(|dir| !dir.is_empty())
        .map(|dir| PathBuf::from(dir).join(FALLBACK_DIR_NAME));
    init_with(preferred_dir, fallback, level)
}

/// [`init`] with the fallback folder passed in, so tests need not touch `%LOCALAPPDATA%`.
fn init_with(preferred: &Path, fallback: Option<PathBuf>, level: LevelFilter) -> PathBuf {
    let why_not_preferred = if preferred.as_os_str().is_empty() {
        "the module folder is unknown".to_owned()
    } else {
        match open_log_file(preferred) {
            Ok(file) => {
                install(file, level);
                return preferred.to_path_buf();
            }
            Err(err) => format!("cannot write to {}: {err}", preferred.display()),
        }
    };
    let Some(fallback) = fallback else {
        return preferred.to_path_buf();
    };
    if let Ok(file) = std::fs::create_dir_all(&fallback).and_then(|()| open_log_file(&fallback)) {
        install(file, level);
        log::warn!(
            "{why_not_preferred}; logging to {} instead",
            fallback.display()
        );
    }
    fallback
}

/// Open the log file in `dir` for writing without truncating it. Directories are not created
/// here: the preferred folder is the one the `.asi` sits in, so it exists or it is unusable.
fn open_log_file(dir: &Path) -> std::io::Result<File> {
    OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(dir.join(LOG_FILE_NAME))
}

/// Make `file` the process logger, unless this module already did that.
fn install(file: File, level: LevelFilter) {
    if LOGGER_CLAIMED.swap(true, Ordering::AcqRel) {
        return;
    }
    // A new log for every run. Only the call that installs the logger truncates, so a repeated
    // `init` cannot cut the file under the logger that is already writing to it.
    let _ = file.set_len(0);
    if dispatch(file).apply().is_ok() {
        // `apply` opened the gate fully; narrow it to the requested level.
        log::set_max_level(level);
    }
}

/// The logger: one line per record, `[seconds-since-init LEVEL] message`, flushed per record.
///
/// It accepts every level on its own; the level limit is `log::max_level`, so it can be
/// changed after `init` without rebuilding the logger.
fn dispatch(file: File) -> fern::Dispatch {
    let start = Instant::now();
    fern::Dispatch::new()
        .format(move |out, message, record| {
            out.finish(format_args!(
                "[{:>8.3}s {:<5}] {message}",
                start.elapsed().as_secs_f64(),
                record.level()
            ))
        })
        .chain(file)
}

#[cfg(test)]
mod tests {
    use super::*;
    use log::Log;
    use std::sync::atomic::{AtomicUsize, Ordering};

    // The `log` crate takes one logger per process and these tests run in parallel, so whichever
    // test installs the logger first wins; the others must still get a folder back. Tests that
    // read what was written therefore build their own logger with `dispatch` instead of `init`.

    /// Run `test` with a fresh, empty directory under the system temp dir, then remove it.
    /// (If `test` panics the directory stays, which helps to see what went wrong.)
    fn with_scratch(label: &str, test: impl FnOnce(&Path)) {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir()
            .join("enshrouded-headroom-tests")
            .join(format!(
                "{}-{}-{label}",
                std::process::id(),
                COUNTER.fetch_add(1, Ordering::Relaxed)
            ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        test(&dir);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn log_falls_back_when_dir_unwritable() {
        let p = init(
            Path::new(r"Z:\definitely\not\writable"),
            log::LevelFilter::Info,
        );
        assert!(p.starts_with(std::env::var("LOCALAPPDATA").unwrap()));
    }

    #[test]
    fn uses_preferred_dir_when_writable() {
        with_scratch("preferred", |base| {
            let preferred = base.join("preferred");
            std::fs::create_dir(&preferred).unwrap();
            let fallback = base.join("never-created");
            let used = init_with(&preferred, Some(fallback.clone()), LevelFilter::Info);
            assert_eq!(used, preferred);
            assert!(preferred.join(LOG_FILE_NAME).is_file());
            assert!(!fallback.exists(), "fallback must not be touched");
        });
    }

    #[test]
    fn creates_missing_fallback_dir() {
        with_scratch("fallback-base", |base| {
            let preferred = base.join("no-such-dir");
            let fallback = base.join("nested").join("enshrouded-headroom");
            let used = init_with(&preferred, Some(fallback.clone()), LevelFilter::Info);
            assert_eq!(used, fallback);
            assert!(fallback.join(LOG_FILE_NAME).is_file());
            assert!(!preferred.exists(), "the preferred dir is never created");
        });
    }

    #[test]
    fn empty_preferred_dir_goes_straight_to_fallback() {
        with_scratch("empty-preferred", |base| {
            let fallback = base.join("fb");
            let used = init_with(Path::new(""), Some(fallback.clone()), LevelFilter::Info);
            assert_eq!(used, fallback);
            assert!(fallback.join(LOG_FILE_NAME).is_file());
        });
    }

    #[test]
    fn returns_fallback_without_panicking_when_nothing_is_writable() {
        with_scratch("nothing-writable", |base| {
            let blocker = base.join("a-file");
            std::fs::write(&blocker, b"x").unwrap();
            // A directory cannot be created below a regular file.
            let fallback = blocker.join("enshrouded-headroom");
            let used = init_with(
                &base.join("missing"),
                Some(fallback.clone()),
                LevelFilter::Info,
            );
            assert_eq!(used, fallback);
        });
    }

    #[test]
    fn returns_preferred_when_there_is_no_fallback_at_all() {
        with_scratch("no-fallback", |base| {
            let preferred = base.join("missing");
            assert_eq!(init_with(&preferred, None, LevelFilter::Info), preferred);
        });
    }

    #[test]
    fn repeated_init_is_tolerated() {
        with_scratch("repeat", |dir| {
            assert_eq!(init_with(dir, None, LevelFilter::Info), dir);
            assert_eq!(init_with(dir, None, LevelFilter::Debug), dir);
        });
    }

    #[test]
    fn logger_writes_one_flushed_line_per_record() {
        with_scratch("format", |dir| {
            let path = dir.join(LOG_FILE_NAME);
            let (_, logger) = dispatch(File::create(&path).unwrap()).into_log();
            logger.log(
                &log::Record::builder()
                    .args(format_args!("hello {}", 7))
                    .level(log::Level::Warn)
                    .target("t")
                    .build(),
            );
            // No flush call and the logger is still alive: the line must already be in the file.
            let text = std::fs::read_to_string(&path).unwrap();
            let line = text.strip_suffix('\n').expect("line ends with a newline");
            assert!(!line.contains('\n'), "exactly one line: {text:?}");
            assert!(line.starts_with('['), "{line:?}");
            assert!(line.ends_with("s WARN ] hello 7"), "{line:?}");
        });
    }
}
