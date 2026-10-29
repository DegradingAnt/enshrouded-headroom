//! Bounded background refresh for the existing blueprint session.
//!
//! Start and stop this service on an ordinary application worker. File reads,
//! hashing and conversion never run in snapshot readers or notify callbacks.
//! A publication is immutable and current only as of its last completed check.
//!
//! # Lifetime
//!
//! `stop` and `Drop` join this service's worker and can block on filesystem I/O.
//! Never call them from `DllMain`, under a native loader lock, or in game,
//! render, present or input callbacks. Stop before dropping the service.
//!
//! Notify 8's Windows backend stops asynchronously. Joining this service does
//! not prove that upstream backend threads have terminated. A native host must
//! keep this code/module loaded through backend quiescence; this crate provides
//! no safe dynamic-DLL-unload boundary. The initial host must remain resident.
//!
//! # Example
//!
//! ```no_run
//! use ember_guide_feed::{Config, Format, GuideFeed, Source};
//! let source = Source {
//!     path: "guide.blueprint.json".into(),
//!     format: Format::Interchange,
//! };
//! let mut feed = GuideFeed::start(source, Config::default(), None)?;
//! let publication = feed.snapshot();
//! if let Some(view) = publication.current() {
//!     let _revision = view.model().revision();
//! }
//! feed.stop()?;
//! # Ok::<(), ember_guide_feed::Error>(())
//! ```
use ember_blueprint::{convert_highcrown, parse, read_bounded, Limits, Session, Snapshot};
use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use std::{
    fmt,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, RecvTimeoutError, SyncSender},
        Arc, RwLock,
    },
    thread::{self, JoinHandle},
    time::Duration,
};

/// An existing source interpretation; this crate defines no guide schema.
#[derive(Clone, Debug)]
pub enum Format {
    /// Raw Highcrown plan interpreted by ember-blueprint.
    Highcrown {
        /// Stable guide identity used by the existing converter.
        source_id: String,
        /// Initial local sequence when no previous session is supplied.
        first_sequence: u64,
    },
    /// Existing blueprint interchange; its producer owns revision sequencing.
    Interchange,
}

/// One caller-selected, local regular source file. The source is never written.
#[derive(Clone, Debug)]
pub struct Source {
    /// Local regular file. Its parent is watched nonrecursively.
    pub path: PathBuf,
    /// Existing parser/converter to reuse for this file.
    pub format: Format,
}

/// Resource and scheduling limits for a single source worker.
#[derive(Clone, Copy, Debug)]
pub struct Config {
    /// Existing blueprint limits. Each limit may only be tightened from default.
    pub limits: Limits,
    /// Coalesce event bursts for 25 milliseconds through one second.
    pub debounce: Duration,
    /// Recovery read interval, from debounce through 60 seconds.
    pub rescan: Duration,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            limits: Limits::default(),
            debounce: Duration::from_millis(100),
            rescan: Duration::from_secs(2),
        }
    }
}

impl Config {
    /// Reject busy loops and attempts to exceed the existing parser's policy.
    fn validate(self) -> Result<(), Error> {
        let maximum = Limits::default();
        if self.limits.max_input_bytes > maximum.max_input_bytes
            || self.limits.max_objects > maximum.max_objects
            || self.limits.max_primitives > maximum.max_primitives
            || self.limits.max_points > maximum.max_points
        {
            return Err(Error::InvalidConfig(
                "blueprint limits may only be tightened",
            ));
        }
        if !(Duration::from_millis(25)..=Duration::from_secs(1)).contains(&self.debounce)
            || self.rescan < self.debounce
            || self.rescan > Duration::from_secs(60)
        {
            return Err(Error::InvalidConfig(
                "invalid debounce or recovery interval",
            ));
        }
        Ok(())
    }
}

/// Why source data cannot be presented as current.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Failure {
    /// The initial worker check has not completed.
    Starting,
    /// The existing source boundary rejected a complete read or update.
    Source(ember_blueprint::Error),
    /// Parent-directory watching failed and will be retried by the worker.
    Watch,
    /// A source event arrived while preparing this model; another check is due.
    Changing,
}

/// One immutable publication of source status and existing blueprint state.
///
/// Retained geometry in `Stale` or `Stopped` is diagnostic data. Consumers must
/// use `current()` to decide whether geometry is usable without a stale label.
/// Older Arc publications remain historical observations, not live status.
#[derive(Clone, Debug, PartialEq)]
pub enum SourceSnapshot {
    /// The last complete source attempt succeeded with an active parent watch.
    Current(Arc<Snapshot>),
    /// No accepted model is available.
    Unavailable(Failure),
    /// Retain the previous model, while explicitly denying current availability.
    Stale {
        /// Existing geometry, selections, placement and fidelity metadata.
        retained: Arc<Snapshot>,
        /// Why this publication is stale.
        reason: Failure,
    },
    /// The service worker has stopped and cannot publish another update.
    Stopped {
        /// Optional retained model, never current after stop.
        retained: Option<Arc<Snapshot>>,
    },
}

impl SourceSnapshot {
    /// Obtain existing blueprint state only from a current publication.
    pub fn current(&self) -> Option<&Snapshot> {
        match self {
            Self::Current(value) => Some(value),
            _ => None,
        }
    }

    /// Retain geometry during stop without relabelling it current.
    fn geometry(&self) -> Option<Arc<Snapshot>> {
        match self {
            Self::Current(value)
            | Self::Stale {
                retained: value, ..
            } => Some(Arc::clone(value)),
            Self::Stopped { retained } => retained.clone(),
            Self::Unavailable(_) => None,
        }
    }
}

/// A service lifecycle error. Source failures are published separately.
#[derive(Debug)]
pub enum Error {
    /// Invalid resource or scheduling policy.
    InvalidConfig(&'static str),
    /// A source path must name a file with a parent directory.
    InvalidPath,
    /// A resumed Highcrown session has another source identity or adapter.
    ResumeMismatch,
    /// A resumed model exceeds the configured resource policy.
    Resume(ember_blueprint::Error),
    /// Resolving the local path or spawning the worker failed.
    Io(std::io::Error),
    /// Joining the service worker observed an unexpected panic.
    WorkerPanicked,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidConfig(message) => write!(f, "guide feed configuration: {message}"),
            Self::InvalidPath => f.write_str("guide feed needs a local file path"),
            Self::ResumeMismatch => f.write_str("guide feed resume source does not match"),
            Self::Resume(error) => write!(f, "guide feed resume model: {error}"),
            Self::Io(error) => write!(f, "guide feed lifecycle: {error}"),
            Self::WorkerPanicked => f.write_str("guide feed worker panicked"),
        }
    }
}

impl std::error::Error for Error {}

/// Only flags cross the notify callback boundary; no event queue is retained.
#[derive(Default)]
struct Signals {
    /// Reject callback work and terminate the service worker.
    stop: AtomicBool,
    /// Retry the upstream parent watch after a backend error.
    watch_failed: AtomicBool,
    /// Prevent a read raced by an observed event from being called current.
    dirty: AtomicBool,
}

/// A short-held pointer publication; conversion never holds this lock.
type Publication = Arc<RwLock<Arc<SourceSnapshot>>>;

/// A data-only reader that can outlive the service and observe its final stop.
///
/// Clones own no worker or callback. Their snapshot method performs no source I/O.
#[derive(Clone)]
pub struct Reader {
    /// Shared immutable pointer publication.
    published: Publication,
}

impl Reader {
    /// Clone the latest publication under a short pointer-only read lock.
    pub fn snapshot(&self) -> Arc<SourceSnapshot> {
        Arc::clone(
            &self
                .published
                .read()
                .unwrap_or_else(|error| error.into_inner()),
        )
    }
}
/// Own one background guide session and a single-slot coalesced wake queue.
///
/// A supplied Session preserves its revision, stable selections and placement.
/// This crate adds no input-command API; native selection/controller integration
/// remains a separate adapter. All existing model and fidelity data is preserved.
/// The default 16 MiB input limit accommodates a 6.26 MB raw plan.
///
/// Stop and drop on an ordinary worker, never under a native loader lock. Keep
/// the hosting module resident through upstream notify backend quiescence.
pub struct GuideFeed {
    /// Latest complete immutable publication.
    published: Publication,
    /// Shared callback/lifecycle flags.
    signals: Arc<Signals>,
    /// Bounded wake channel; redundant events are intentionally coalesced.
    wake: SyncSender<()>,
    /// The one worker that performs file reads and model updates.
    worker: Option<JoinHandle<()>>,
}

impl GuideFeed {
    /// Start a source worker without reading/converting on the calling thread.
    ///
    /// Supply the previous Session for sequence and selection continuity. With no
    /// session, the existing parser/converter establishes the first valid model.
    /// The source may be missing at start and can recover later.
    pub fn start(
        mut source: Source,
        config: Config,
        resume: Option<Session>,
    ) -> Result<Self, Error> {
        config.validate()?;
        if let Some(session) = &resume {
            session
                .snapshot()
                .model()
                .document()
                .clone()
                .validate(config.limits)
                .map_err(Error::Resume)?;
        }
        source.path = std::path::absolute(&source.path).map_err(Error::Io)?;
        if source.path.file_name().is_none() || source.path.parent().is_none() {
            return Err(Error::InvalidPath);
        }
        if let (Format::Highcrown { source_id, .. }, Some(session)) = (&source.format, &resume) {
            let snapshot = session.snapshot();
            let previous = &snapshot.model().document().source;
            if previous.adapter != "highcrown-v1" || &previous.id != source_id {
                return Err(Error::ResumeMismatch);
            }
        }
        let initial = rejected(&resume, Failure::Starting);
        let published = Arc::new(RwLock::new(Arc::new(initial)));
        let signals = Arc::new(Signals::default());
        let (wake, receiver) = mpsc::sync_channel(1);
        let worker = {
            let published = Arc::clone(&published);
            let signals = Arc::clone(&signals);
            let wake = wake.clone();
            thread::Builder::new()
                .name("ember-guide-feed".to_owned())
                .spawn(move || run(source, config, resume, published, signals, wake, receiver))
                .map_err(Error::Io)?
        };
        Ok(Self {
            published,
            signals,
            wake,
            worker: Some(worker),
        })
    }

    /// Obtain a data-only reader for another application thread.
    ///
    /// A reader observes the final Stopped publication even after this owner drops.
    pub fn reader(&self) -> Reader {
        Reader {
            published: Arc::clone(&self.published),
        }
    }
    /// Clone the latest immutable publication. This performs no source I/O.
    ///
    /// A short read lock protects only the Arc pointer, not parsing or hashing.
    pub fn snapshot(&self) -> Arc<SourceSnapshot> {
        Arc::clone(
            &self
                .published
                .read()
                .unwrap_or_else(|error| error.into_inner()),
        )
    }

    /// Stop, wake and join the service worker, then freeze its publication.
    ///
    /// Idempotent. May wait for local I/O and upstream watch setup. Never call
    /// under a loader lock or in game callbacks. It does not certify termination
    /// of notify's asynchronous Windows backend or authorize unloading its code.
    pub fn stop(&mut self) -> Result<(), Error> {
        self.signals.stop.store(true, Ordering::Release);
        // A full queue already guarantees a wake; a disconnected queue is stopped.
        let _ = self.wake.try_send(());
        if let Some(worker) = self.worker.take() {
            worker.thread().unpark();
            if let Err(payload) = worker.join() {
                // An opaque panic payload may itself panic on drop. As in the
                // native guard, retain this one exceptional payload to prevent
                // unwinding through an embedding host's shutdown boundary.
                std::mem::forget(payload);
                publish_stopped(&self.published);
                return Err(Error::WorkerPanicked);
            }
        }
        Ok(())
    }
}

impl Drop for GuideFeed {
    fn drop(&mut self) {
        // Explicit stop reports failure; Drop must not unwind during shutdown.
        let _ = self.stop();
    }
}

/// Swap only the pointer under the write lock, dropping old data afterwards.
fn publish(published: &Publication, value: SourceSnapshot) {
    let next = Arc::new(value);
    let previous = {
        let mut slot = published.write().unwrap_or_else(|error| error.into_inner());
        std::mem::replace(&mut *slot, next)
    };
    drop(previous);
}

/// Freeze the last published geometry with an explicit non-current state.
fn publish_stopped(published: &Publication) {
    let retained = published
        .read()
        .unwrap_or_else(|error| error.into_inner())
        .geometry();
    publish(published, SourceSnapshot::Stopped { retained });
}

/// Freeze publications on normal exit or an unexpected service-worker unwind.
struct PublicationOnExit(Publication);

impl Drop for PublicationOnExit {
    fn drop(&mut self) {
        publish_stopped(&self.0);
    }
}
/// Use existing session state for failure retention, never a second model store.
fn rejected(session: &Option<Session>, reason: Failure) -> SourceSnapshot {
    match session {
        Some(session) => SourceSnapshot::Stale {
            retained: session.snapshot(),
            reason,
        },
        None => SourceSnapshot::Unavailable(reason),
    }
}

/// Reuse the existing complete-read/parser and all subsequent Session polling.
fn refresh(
    session: &mut Option<Session>,
    source: &Source,
    limits: Limits,
) -> Result<(), ember_blueprint::Error> {
    if let Some(session) = session {
        return match source.format {
            Format::Highcrown { .. } => session.poll_highcrown(&source.path, limits),
            Format::Interchange => session.poll_interchange(&source.path, limits),
        }
        .map(|_| ());
    }
    let bytes = read_bounded(&source.path, limits)?;
    let model = match &source.format {
        Format::Highcrown {
            source_id,
            first_sequence,
        } => convert_highcrown(&bytes, source_id, *first_sequence, limits)?,
        Format::Interchange => parse(&bytes, limits)?,
    };
    *session = Some(Session::new(model));
    Ok(())
}

/// Attach upstream notify to the parent, so file replacement keeps working.
fn watch_parent(
    path: &Path,
    signals: &Arc<Signals>,
    wake: &SyncSender<()>,
) -> Option<RecommendedWatcher> {
    let parent = path.parent()?.to_owned();
    let target = path.to_owned();
    let callback_parent = parent.clone();
    let signals = Arc::clone(signals);
    let wake = wake.clone();
    let mut watcher = notify::recommended_watcher(move |result: notify::Result<notify::Event>| {
        if signals.stop.load(Ordering::Acquire) {
            return;
        }
        match result {
            Ok(event) => {
                // Our own source reads must not cause an endless event/read loop.
                if matches!(event.kind, EventKind::Access(_)) {
                    return;
                }
                if !event.need_rescan()
                    && !event
                        .paths
                        .iter()
                        .any(|path| path == &target || path == &callback_parent)
                {
                    return;
                }
                if event.need_rescan()
                    || (matches!(event.kind, EventKind::Remove(_))
                        && event.paths.contains(&callback_parent))
                {
                    signals.watch_failed.store(true, Ordering::Release);
                }
            }
            Err(_) => signals.watch_failed.store(true, Ordering::Release),
        }
        signals.dirty.store(true, Ordering::Release);
        // One token represents any number of edits. Never block a notify thread.
        let _ = wake.try_send(());
    })
    .ok()?;
    watcher.watch(&parent, RecursiveMode::NonRecursive).ok()?;
    Some(watcher)
}

/// Run bounded checks and retry failed watches on the same ordinary worker.
fn run(
    source: Source,
    config: Config,
    mut session: Option<Session>,
    published: Publication,
    signals: Arc<Signals>,
    wake: SyncSender<()>,
    receiver: Receiver<()>,
) {
    let _on_exit = PublicationOnExit(Arc::clone(&published));
    let mut watcher = None;
    while !signals.stop.load(Ordering::Acquire) {
        if signals.watch_failed.swap(false, Ordering::AcqRel) {
            watcher = None;
        }
        if watcher.is_none() {
            watcher = watch_parent(&source.path, &signals, &wake);
        }
        signals.dirty.store(false, Ordering::Release);
        let result = refresh(&mut session, &source, config.limits);
        if signals.stop.load(Ordering::Acquire) {
            break;
        }
        let next = match result {
            Err(error) => rejected(&session, Failure::Source(error)),
            Ok(()) if watcher.is_none() || signals.watch_failed.load(Ordering::Acquire) => {
                rejected(&session, Failure::Watch)
            }
            Ok(()) if signals.dirty.load(Ordering::Acquire) => {
                rejected(&session, Failure::Changing)
            }
            Ok(()) => match &session {
                Some(session) => SourceSnapshot::Current(session.snapshot()),
                None => SourceSnapshot::Unavailable(Failure::Starting),
            },
        };
        publish(&published, next);
        match receiver.recv_timeout(config.rescan) {
            Ok(()) => {
                // Fixed debounce never extends under a continuous event stream.
                // Stop unparks this wait. There is at most one queued token.
                let started = std::time::Instant::now();
                while !signals.stop.load(Ordering::Acquire) && started.elapsed() < config.debounce {
                    thread::park_timeout(config.debounce.saturating_sub(started.elapsed()));
                }
                let _ = receiver.try_recv();
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
    drop(watcher);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    /// Opaque panic payloads can execute arbitrary destructor code.
    struct PanicOnDrop;

    impl Drop for PanicOnDrop {
        fn drop(&mut self) {
            panic!("opaque payload destructor must not escape stop");
        }
    }

    #[test]
    fn unexpected_worker_exit_freezes_readers_and_stop_cannot_drop_a_panicking_payload() {
        let model = parse(
            include_bytes!("../../ember-blueprint/examples/watchtower.blueprint.json"),
            Limits::default(),
        )
        .unwrap();
        let session = Session::new(model);
        let published = Arc::new(RwLock::new(Arc::new(SourceSnapshot::Current(
            session.snapshot(),
        ))));
        let signals = Arc::new(Signals::default());
        let (wake, _receiver) = mpsc::sync_channel(1);
        let worker_publication = Arc::clone(&published);
        let worker = thread::spawn(move || {
            let _on_exit = PublicationOnExit(worker_publication);
            std::panic::panic_any(PanicOnDrop);
        });
        let deadline = Instant::now() + Duration::from_secs(2);
        while !worker.is_finished() {
            assert!(Instant::now() < deadline);
            thread::yield_now();
        }
        let mut feed = GuideFeed {
            published,
            signals,
            wake,
            worker: Some(worker),
        };
        assert!(matches!(
            feed.snapshot().as_ref(),
            SourceSnapshot::Stopped { .. }
        ));
        assert!(matches!(feed.stop(), Err(Error::WorkerPanicked)));
        assert!(feed.snapshot().current().is_none());
        assert!(feed.stop().is_ok());
    }
}
