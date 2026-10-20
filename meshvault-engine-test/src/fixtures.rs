//! Per-backend knobs for the contract tests.

use std::any::Any;
use std::path::PathBuf;
use std::time::Duration;

use meshvault_engine::PeerId;

/// What the contract tests need to know about the backend under test.
///
/// The defaults suit [`FakeEngine`](crate::FakeEngine). A real backend
/// overrides what differs: the shape of a valid peer ID, where folders may be
/// created, and how long it takes to go quiet.
pub struct Fixtures {
    peer_id: fn(&str) -> PeerId,
    folder_root: PathBuf,
    settle: Duration,
    _guard: Option<Box<dyn Any + Send + Sync>>,
}

impl Fixtures {
    /// Defaults: any string is a valid peer ID, folders live under the system
    /// temp directory, and an engine is quiet after 100 ms without events.
    pub fn new() -> Self {
        Self {
            peer_id: |name| PeerId(format!("peer-{name}")),
            folder_root: std::env::temp_dir().join("meshvault-contract"),
            settle: Duration::from_millis(100),
            _guard: None,
        }
    }

    /// Turn a short test name such as `"a"` into a valid, distinct peer ID.
    /// The same name must always give the same ID.
    pub fn peer_ids(mut self, make: fn(&str) -> PeerId) -> Self {
        self.peer_id = make;
        self
    }

    /// The directory under which the tests create their folders.
    pub fn folder_root(mut self, root: impl Into<PathBuf>) -> Self {
        self.folder_root = root.into();
        self
    }

    /// How long without any event counts as quiet. A backend that scans and
    /// settles on its own needs this longer than its slowest settling gap.
    pub fn settle(mut self, quiet: Duration) -> Self {
        self.settle = quiet;
        self
    }

    /// Keep `guard` alive until the test ends, for example a temporary
    /// directory the engine runs in.
    pub fn keep_alive(mut self, guard: impl Any + Send + Sync) -> Self {
        self._guard = Some(Box::new(guard));
        self
    }

    pub(crate) fn make_peer_id(&self, name: &str) -> PeerId {
        (self.peer_id)(name)
    }

    pub(crate) fn root(&self) -> &PathBuf {
        &self.folder_root
    }

    pub(crate) fn settle_time(&self) -> Duration {
        self.settle
    }
}

impl Default for Fixtures {
    fn default() -> Self {
        Self::new()
    }
}
