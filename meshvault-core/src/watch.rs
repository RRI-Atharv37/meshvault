//! Change notifications as an optimisation: a missed or failed notification only means a full scan.

use crate::path::{fold, RelPath, META_DIR};
use crate::time::{Clock, Timestamp};
use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use std::collections::BTreeSet;
use std::path::{Component, Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, TryRecvError};
use std::time::Duration;

/// What a watcher backend reports.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WatchEvent {
    /// Something at this path changed.
    Changed(RelPath),
    /// Events were lost; only a full scan is trustworthy.
    Overflow,
    /// The backend failed; only a full scan is trustworthy.
    Error(String),
}

/// A source of [`WatchEvent`]s.
pub trait WatchSource: Send {
    /// Returns every event since the last call, without blocking.
    fn drain(&mut self) -> Vec<WatchEvent>;
}

/// Why a watcher could not start.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum WatchError {
    /// The OS watcher failed.
    #[error("watcher: {0}")]
    Backend(String),
}

/// Tells when a periodic full scan is due; a clock that moves backwards counts as due.
#[derive(Debug)]
pub struct ReconcileTimer {
    every: Duration,
    last: Timestamp,
}

impl ReconcileTimer {
    /// Starts counting from now.
    pub fn new(every: Duration, clock: &dyn Clock) -> ReconcileTimer {
        ReconcileTimer {
            every,
            last: clock.now(),
        }
    }

    /// Whether a full scan is due.
    pub fn due(&self, clock: &dyn Clock) -> bool {
        let now = clock.now();
        let every = i64::try_from(self.every.as_nanos()).unwrap_or(i64::MAX);
        now < self.last || now.0.saturating_sub(self.last.0) >= every
    }

    /// Records that a full scan just started.
    pub fn mark_done(&mut self, clock: &dyn Clock) {
        self.last = clock.now();
    }
}

/// What to do after polling.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WatchPoll {
    /// Paths reported changed.
    pub changed: BTreeSet<RelPath>,
    /// A full scan is needed (overflow, error, or the timer).
    pub reconcile: bool,
}

/// A watcher plus the reconcile timer.
pub struct Watch<S: WatchSource> {
    source: S,
    timer: ReconcileTimer,
}

impl<S: WatchSource> Watch<S> {
    /// Wraps a source; a full scan is due every `reconcile_every`.
    pub fn new(source: S, reconcile_every: Duration, clock: &dyn Clock) -> Watch<S> {
        Watch {
            source,
            timer: ReconcileTimer::new(reconcile_every, clock),
        }
    }

    /// Collects events and decides whether a full scan is needed. A `reconcile` result restarts the
    /// timer.
    pub fn poll(&mut self, clock: &dyn Clock) -> WatchPoll {
        let mut poll = WatchPoll::default();
        for event in self.source.drain() {
            match event {
                WatchEvent::Changed(p) => {
                    poll.changed.insert(p);
                }
                WatchEvent::Overflow | WatchEvent::Error(_) => poll.reconcile = true,
            }
        }
        if self.timer.due(clock) {
            poll.reconcile = true;
        }
        if poll.reconcile {
            self.timer.mark_done(clock);
        }
        poll
    }
}

/// The OS-backed source.
pub struct NotifyWatchSource {
    _watcher: RecommendedWatcher,
    events: Receiver<notify::Result<notify::Event>>,
    root: PathBuf,
    disconnected: bool,
}

impl NotifyWatchSource {
    /// Watches `root` recursively.
    pub fn start(root: &Path) -> Result<NotifyWatchSource, WatchError> {
        let (tx, events) = channel();
        let mut watcher = notify::recommended_watcher(move |res| {
            let _ = tx.send(res);
        })
        .map_err(|e| WatchError::Backend(e.to_string()))?;
        watcher
            .watch(root, RecursiveMode::Recursive)
            .map_err(|e| WatchError::Backend(e.to_string()))?;
        Ok(NotifyWatchSource {
            _watcher: watcher,
            events,
            root: root.to_path_buf(),
            disconnected: false,
        })
    }

    fn relative(&self, path: &Path) -> Option<Option<RelPath>> {
        let rel = path.strip_prefix(&self.root).ok()?;
        let mut parts = Vec::new();
        for c in rel.components() {
            match c {
                Component::Normal(name) => parts.push(name.to_str()?.to_string()),
                _ => return None,
            }
        }
        if parts.is_empty() || fold(&parts[0]) == fold(META_DIR) {
            return Some(None);
        }
        Some(RelPath::parse(&parts.join("/")).ok())
    }
}

impl WatchSource for NotifyWatchSource {
    fn drain(&mut self) -> Vec<WatchEvent> {
        let mut out = Vec::new();
        loop {
            match self.events.try_recv() {
                Ok(Ok(event)) => {
                    if event.need_rescan() {
                        out.push(WatchEvent::Overflow);
                        continue;
                    }
                    if matches!(event.kind, EventKind::Access(_)) {
                        continue;
                    }
                    for path in &event.paths {
                        match self.relative(path) {
                            Some(Some(rel)) => out.push(WatchEvent::Changed(rel)),
                            Some(None) => {}
                            None => out.push(WatchEvent::Overflow),
                        }
                    }
                }
                Ok(Err(e)) => out.push(WatchEvent::Error(e.to_string())),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    if !self.disconnected {
                        self.disconnected = true;
                        out.push(WatchEvent::Error("watcher stopped".into()));
                    }
                    break;
                }
            }
        }
        out
    }
}
