//! File logging for the plugin.
//!
//! [`init_named`] opens `enshrouded-headroom.log` next to the `.asi` (or in
//! `%LOCALAPPDATA%\enshrouded-headroom\` when that folder cannot be written) and installs it as
//! the process-wide `log` logger. After that, `log::info!` and friends write to the file from
//! any thread. Each record is flushed as it is written, so the last lines survive a crash.
//!
//! Inside this crate, `log::` in a module other than the crate root means the `log` crate; this
//! module is `crate::log`. At the crate root, write `::log::` for the crate.

use std::fs::{File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use log::LevelFilter;

/// Name of the log file, in whichever folder [`init_named`] picks.
#[cfg(test)]
const LOG_FILE_NAME: &str = "enshrouded-headroom.log";

/// Sub-folder of `%LOCALAPPDATA%` used when the preferred folder is not writable.
#[cfg(test)]
const FALLBACK_DIR_NAME: &str = "enshrouded-headroom";

/// Taken by the one `init` call that may install the logger; see [`init_with_named`].
static LOGGER_CLAIMED: AtomicBool = AtomicBool::new(false);

/// Start logging and return the folder that holds the log file.
///
/// The result is `preferred_dir` when the log file can be created there, else
/// `%LOCALAPPDATA%\enshrouded-headroom\` (created if missing), with a warning in the log that
/// says why. An empty `preferred_dir` means "no preferred folder" and goes straight to the
/// fallback. If neither folder works, logging stays off and the fallback path is still returned.
/// If there is no fallback either (`preferred_dir` is empty and `%LOCALAPPDATA%` is not set),
/// the result is `preferred_dir` itself, an EMPTY path, which means "no usable folder". It never
/// panics.
///
/// `level` is the highest level that is written. It is applied with `log::set_max_level`, so
/// later code can raise or lower it by calling that function again.
///
/// The `log` crate accepts one logger per process. A second call still picks and returns a
/// folder but leaves the first logger (and its file) alone.
pub fn init_named(
    file_name: &str,
    fallback_name: &str,
    preferred_dir: &Path,
    level: LevelFilter,
) -> PathBuf {
    let fallback = std::env::var_os("LOCALAPPDATA")
        .filter(|dir| !dir.is_empty())
        .map(|dir| PathBuf::from(dir).join(fallback_name));
    init_with_named(file_name, preferred_dir, fallback, level)
}

/// [`init_named`] with the fallback folder passed in, so tests need not touch `%LOCALAPPDATA%`.
fn init_with_named(
    file_name: &str,
    preferred: &Path,
    fallback: Option<PathBuf>,
    level: LevelFilter,
) -> PathBuf {
    // The first call claims the process-wide logger slot and alone truncates the file. It does so
    // when it opens the file, so a truncation that fails is an open that fails and the next
    // folder is tried; the log can never silently mix two runs. Every other call opens without
    // truncating, so it cannot cut the file under the logger that is already writing to it.
    let owner = !LOGGER_CLAIMED.swap(true, Ordering::AcqRel);
    let Chosen { dir, file, notice } = choose_folder_named(file_name, preferred, fallback, owner);
    let installed = match file {
        Some(file) if owner => install(file, level),
        Some(_) => false,
        None => {
            if owner {
                // Nothing was installed: leave the slot to a later call.
                LOGGER_CLAIMED.store(false, Ordering::Release);
            }
            false
        }
    };
    // The notice says where the log is. That is only true if this call's file became the log.
    if let Some(notice) = notice.filter(|_| installed) {
        log::warn!("{notice}");
    }
    dir
}

/// Where the log goes, as decided by [`choose_folder_named`].
struct Chosen {
    /// The folder to report.
    dir: PathBuf,
    /// The opened log file, or `None` when no folder could be used.
    file: Option<File>,
    /// Set when the log file is in the fallback folder: what to tell the log about that.
    notice: Option<String>,
}

/// Open the log file in `preferred`, else in `fallback` (created if missing). `truncate` empties
/// an existing file as it is opened.
fn choose_folder_named(
    file_name: &str,
    preferred: &Path,
    fallback: Option<PathBuf>,
    truncate: bool,
) -> Chosen {
    let preferred_error = if preferred.as_os_str().is_empty() {
        None
    } else {
        match open_log_file_named(file_name, preferred, truncate) {
            Ok(file) => {
                return Chosen {
                    dir: preferred.to_path_buf(),
                    file: Some(file),
                    notice: None,
                }
            }
            Err(err) => Some(err),
        }
    };
    let Some(fallback) = fallback else {
        return Chosen {
            dir: preferred.to_path_buf(),
            file: None,
            notice: None,
        };
    };
    let file = std::fs::create_dir_all(&fallback)
        .and_then(|()| open_log_file_named(file_name, &fallback, truncate))
        .ok();
    let notice = file
        .as_ref()
        .map(|_| fallback_notice(preferred, preferred_error.as_ref(), &fallback));
    Chosen {
        dir: fallback,
        file,
        notice,
    }
}

/// The warning that says why the log is in the fallback folder.
fn fallback_notice(preferred: &Path, error: Option<&io::Error>, fallback: &Path) -> String {
    match error {
        Some(err) => format!(
            "cannot write to {}: {err}; logging to {} instead",
            preferred.display(),
            fallback.display()
        ),
        None => format!(
            "no preferred log folder; logging to {} instead",
            fallback.display()
        ),
    }
}

/// Open the log file in `dir` for writing. Directories are not created here: the preferred folder
/// is the one the `.asi` sits in, so it exists or it is unusable.
fn open_log_file_named(file_name: &str, dir: &Path, truncate: bool) -> io::Result<File> {
    OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(truncate)
        .open(dir.join(file_name))
}

/// Make `file` the process logger. `false` if some other logger was already set.
fn install(file: File, level: LevelFilter) -> bool {
    if dispatch(file).apply().is_err() {
        return false;
    }
    // `apply` opened the gate fully; narrow it to the requested level.
    log::set_max_level(level);
    true
}

/// The logger: one line per record, `[seconds-since-init LEVEL] message`, flushed per record.
///
/// It accepts every level on its own; the level limit is `log::max_level`, so it can be
/// changed after `init` without rebuilding the logger.
fn dispatch(file: File) -> fern::Dispatch {
    let start = Instant::now();
    fern::Dispatch::new()
        .format(move |out, message, record| {
            // Build the whole line before fern sees it. Fern writes `format_args!` output while
            // it holds the output lock (we do not enable its `meta-logging-in-format` feature,
            // which would format first), and `message` can run any `Display` code. If that code
            // panicked, or logged, the panic hook would then wait for a lock its own thread
            // holds. Formatted here, user code runs before any lock is taken.
            let line = format!(
                "[{:>8.3}s {:<5}] {message}",
                start.elapsed().as_secs_f64(),
                record.level()
            );
            out.finish(format_args!("{line}"));
        })
        .chain(file)
}

#[cfg(test)]
fn init(dir: &Path, level: LevelFilter) -> PathBuf {
    init_named(LOG_FILE_NAME, FALLBACK_DIR_NAME, dir, level)
}
#[cfg(test)]
fn init_with(dir: &Path, fallback: Option<PathBuf>, level: LevelFilter) -> PathBuf {
    init_with_named(LOG_FILE_NAME, dir, fallback, level)
}
#[cfg(test)]
fn choose_folder(dir: &Path, fallback: Option<PathBuf>, truncate: bool) -> Chosen {
    choose_folder_named(LOG_FILE_NAME, dir, fallback, truncate)
}

#[cfg(test)]
mod tests {
    use super::*;
    use log::{Level, Log, Record};
    use std::fmt;
    use std::panic::{catch_unwind, AssertUnwindSafe};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

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
    fn fallback_notice_names_the_error_and_the_fallback() {
        let denied = io::Error::new(io::ErrorKind::PermissionDenied, "denied");
        assert_eq!(
            fallback_notice(
                Path::new(r"C:\game\plugins"),
                Some(&denied),
                Path::new(r"C:\Users\me\AppData\Local\enshrouded-headroom")
            ),
            concat!(
                r"cannot write to C:\game\plugins: denied; ",
                r"logging to C:\Users\me\AppData\Local\enshrouded-headroom instead"
            )
        );
        assert_eq!(
            fallback_notice(Path::new(""), None, Path::new(r"C:\fb")),
            r"no preferred log folder; logging to C:\fb instead"
        );
    }

    #[test]
    fn notice_only_when_the_log_ended_up_in_the_fallback() {
        with_scratch("notice", |base| {
            let fallback = base.join("fb");
            // Preferred folder works: no notice, and the fallback is not touched.
            let chosen = choose_folder(base, Some(fallback.clone()), false);
            assert!(chosen.file.is_some() && chosen.notice.is_none());
            assert!(!fallback.exists());
            // Preferred folder fails, fallback works: a notice that names both.
            let missing = base.join("missing");
            let chosen = choose_folder(&missing, Some(fallback.clone()), false);
            assert!(chosen.file.is_some());
            let notice = chosen.notice.expect("a notice for the fallback");
            assert!(notice.starts_with(&format!("cannot write to {}", missing.display())));
            assert!(notice.ends_with(&format!("logging to {} instead", fallback.display())));
            // Neither works: no file and no notice (there is no log to put it in).
            let blocker = base.join("a-file");
            std::fs::write(&blocker, b"x").unwrap();
            let chosen = choose_folder(&missing, Some(blocker.join("fb")), false);
            assert!(chosen.file.is_none() && chosen.notice.is_none());
        });
    }

    #[test]
    fn only_the_installing_call_truncates_the_log() {
        with_scratch("truncate", |dir| {
            let path = dir.join(LOG_FILE_NAME);
            std::fs::write(&path, b"old run\n").unwrap();
            // A call that does not own the logger keeps what the owner is writing.
            drop(choose_folder(dir, None, false).file);
            assert_eq!(std::fs::read(&path).unwrap(), b"old run\n");
            // The owner starts a new log as it opens the file.
            drop(choose_folder(dir, None, true).file);
            assert_eq!(std::fs::read(&path).unwrap(), b"");
        });
    }

    #[test]
    fn empty_preferred_dir_and_no_fallback_gives_an_empty_path() {
        assert!(init_with(Path::new(""), None, LevelFilter::Info)
            .as_os_str()
            .is_empty());
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
            assert!(!fallback.exists(), "no folder below a regular file");
            assert!(
                !base.join("missing").join(LOG_FILE_NAME).exists(),
                "no log file in the preferred folder"
            );
        });
    }

    /// A `Display` that logs through the same logger while it is being formatted, then panics.
    /// This is what a panic hook does on a thread whose `Display` panicked inside `log!`.
    struct LogsThenPanics<'a>(&'a dyn Log);

    impl fmt::Display for LogsThenPanics<'_> {
        fn fmt(&self, _: &mut fmt::Formatter<'_>) -> fmt::Result {
            self.0.log(
                &Record::builder()
                    .args(format_args!("logged from inside Display"))
                    .level(Level::Error)
                    .target("t")
                    .build(),
            );
            panic!("display boom");
        }
    }

    #[test]
    fn a_record_whose_display_panics_does_not_hang_the_logger() {
        with_scratch("display-panic", |dir| {
            let path = dir.join(LOG_FILE_NAME);
            let file = File::create(&path).unwrap();
            let (tx, rx) = std::sync::mpsc::channel();
            // A separate thread, so that a deadlock fails this test after the timeout instead of
            // hanging the whole test run.
            std::thread::spawn(move || {
                let (_, logger) = dispatch(file).into_log();
                let panicked = catch_unwind(AssertUnwindSafe(|| {
                    logger.log(
                        &Record::builder()
                            .args(format_args!("{}", LogsThenPanics(&*logger)))
                            .level(Level::Info)
                            .target("t")
                            .build(),
                    );
                }))
                .is_err();
                logger.log(
                    &Record::builder()
                        .args(format_args!("after the panic"))
                        .level(Level::Info)
                        .target("t")
                        .build(),
                );
                let _ = tx.send(panicked);
            });
            let panicked = rx
                .recv_timeout(Duration::from_secs(10))
                .expect("the logger hung: a Display that logs and panics deadlocked it");
            assert!(panicked, "the Display panic must reach the caller");
            let text = std::fs::read_to_string(&path).unwrap();
            assert!(text.contains("logged from inside Display"), "{text:?}");
            assert!(text.contains("after the panic"), "{text:?}");
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
