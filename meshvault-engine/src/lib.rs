//! The `Engine` contract.
//!
//! An engine moves files between trusted devices. The app talks only to this
//! trait, so backends are interchangeable. Every implementation must pass the same contract test
//! suite in the `meshvault-engine-test` crate, including its in-memory
//! `FakeEngine`.
//!
//! # Rules every implementation follows
//!
//! - **Idempotent.** Repeating a call with the same arguments succeeds and
//!   changes nothing. `add_peer` on a known peer, `remove_peer` on an unknown
//!   one, `pause` on a paused folder and `resume` on a running one are all `Ok`.
//! - **Local state only.** A call returns once the engine has accepted the
//!   change. It does not wait for the remote side. Anything slow
//!   (connecting, scanning, transferring) is reported through [`EngineEvent`]s.
//! - **No leaks.** Backend names, ports, API keys and raw backend errors never
//!   cross this boundary. They are mapped to [`EngineError`].
//! - **Paths are confined.** A folder's files stay inside its `path`. Nothing
//!   a remote peer sends may write outside it.
//! - **One shared view of events.** [`Engine::events`] can be called many
//!   times. Each caller gets its own receiver and sees every event emitted
//!   after it subscribed, in order.

#![deny(missing_docs)]

use std::fmt;
use std::future::Future;
use std::path::PathBuf;

use tokio::sync::broadcast;

/// Identity of a device. For MeshVault's engine this is a device public key.
/// Treat it as opaque: compare it, never parse it.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PeerId(pub String);

/// Identity of a shared folder, chosen by the caller and stable across
/// restarts. The same ID on two devices means the same shared folder.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FolderId(pub String);

/// A device we are willing to sync with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerSpec {
    /// The device's identity.
    pub id: PeerId,
    /// Human-readable name for the UI. Not used for identity.
    pub name: String,
}

/// A folder to share.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FolderSpec {
    /// Which shared folder this is.
    pub id: FolderId,
    /// Absolute path on this device.
    pub path: PathBuf,
    /// Peers that receive this folder. Each must already have been added with
    /// [`Engine::add_peer`].
    pub peers: Vec<PeerId>,
}

/// Why a folder is not syncing right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FolderState {
    /// Looking for local changes.
    Scanning,
    /// Transferring files in either direction.
    Syncing,
    /// Everything we know about matches every connected peer.
    UpToDate,
    /// Paused by [`Engine::pause`].
    Paused,
    /// Cannot sync; see the matching [`EngineEvent::FolderError`].
    Error,
}

/// Snapshot of one folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FolderStatus {
    /// Which folder this describes.
    pub id: FolderId,
    /// What the folder is doing now.
    pub state: FolderState,
    /// Bytes still to transfer. Zero when `UpToDate`.
    pub bytes_remaining: u64,
}

/// Snapshot of one peer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerStatus {
    /// Which peer this describes.
    pub id: PeerId,
    /// Whether the peer is connected right now.
    pub connected: bool,
}

/// Snapshot of the whole engine, from [`Engine::status`].
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct EngineStatus {
    /// Every trusted peer.
    pub peers: Vec<PeerStatus>,
    /// Every shared folder.
    pub folders: Vec<FolderStatus>,
}

/// Something that happened. Events are facts about the past: they are never
/// retracted, and `status()` always agrees with the last event seen for the
/// same peer or folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EngineEvent {
    /// A trusted peer connected.
    PeerConnected(PeerId),
    /// A trusted peer disconnected, or was removed while connected.
    PeerDisconnected(PeerId),
    /// A folder moved from one state to another. Emitted once per change, so
    /// a folder that stays `Syncing` produces no repeats.
    FolderStateChanged {
        /// The folder that changed.
        folder: FolderId,
        /// The state it moved to.
        state: FolderState,
    },
    /// Transfer progress for a folder. Emitted at most a few times a second.
    Progress {
        /// The folder transferring.
        folder: FolderId,
        /// Bytes transferred so far.
        bytes_done: u64,
        /// Bytes to transfer in total.
        bytes_total: u64,
    },
    /// A folder cannot sync. `message` is for the log, not for end users.
    FolderError {
        /// The folder that cannot sync.
        folder: FolderId,
        /// What went wrong, for logs.
        message: String,
    },
}

/// Everything that can go wrong at the boundary. Backend-specific detail goes
/// into `Backend`'s string, for logs only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EngineError {
    /// The peer or folder named in the call is not known to the engine.
    NotFound,
    /// The call is invalid, for example a relative path or an unknown peer in
    /// `FolderSpec::peers`.
    InvalidArgument(String),
    /// A folder with this ID already exists with a different path.
    Conflict,
    /// The engine is not running (not started, crashed, shutting down).
    Unavailable,
    /// Anything else the backend reported.
    Backend(String),
}

impl fmt::Display for EngineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EngineError::NotFound => write!(f, "not found"),
            EngineError::InvalidArgument(m) => write!(f, "invalid argument: {m}"),
            EngineError::Conflict => write!(f, "conflicting folder"),
            EngineError::Unavailable => write!(f, "engine unavailable"),
            EngineError::Backend(m) => write!(f, "backend error: {m}"),
        }
    }
}

impl std::error::Error for EngineError {}

/// A sync backend.
///
/// The methods return `impl Future + Send` rather than using `async fn` so
/// callers can spawn them on a multi-threaded runtime. The trait is therefore
/// not object-safe: the app selects an engine with a generic parameter or an
/// enum, not `Box<dyn Engine>`. (Open item: confirm this choice when the app
/// wires up its first engine.)
pub trait Engine: Send + Sync {
    /// Trust a device. After this, the device may connect and receive folders
    /// it is listed in.
    ///
    /// - Adding a known peer updates its `name` and is otherwise a no-op.
    /// - Does not wait for the peer to be online. Connection is reported by
    ///   [`EngineEvent::PeerConnected`].
    fn add_peer(&self, peer: PeerSpec) -> impl Future<Output = Result<(), EngineError>> + Send;

    /// Stop trusting a device and stop syncing with it.
    ///
    /// - The peer is also removed from every folder's peer list.
    /// - Files already received stay on disk.
    /// - Removing an unknown peer is `Ok`, not `NotFound`.
    /// - The peer's connection, if any, ends and `PeerDisconnected` follows.
    fn remove_peer(&self, peer: &PeerId) -> impl Future<Output = Result<(), EngineError>> + Send;

    /// Start sharing a folder with its listed peers.
    ///
    /// - `InvalidArgument` if the path is relative or a listed peer is unknown.
    /// - Sharing the same ID and path again replaces the peer list. This is
    ///   how peers are added to or removed from a folder.
    /// - The same ID with a different path is `Conflict`.
    /// - New folders start in `Scanning`, unless paused, and emit
    ///   `FolderStateChanged` for it. Re-sharing a known folder emits nothing.
    /// - A call that fails emits no events.
    fn share_folder(
        &self,
        folder: FolderSpec,
    ) -> impl Future<Output = Result<(), EngineError>> + Send;

    /// Take a snapshot of the engine's current state.
    ///
    /// - Always answers from local knowledge. Never blocks on the network.
    /// - `Unavailable` if the engine is not running.
    fn status(&self) -> impl Future<Output = Result<EngineStatus, EngineError>> + Send;

    /// Subscribe to events.
    ///
    /// - The receiver sees every event emitted after this call, in order.
    /// - A receiver that falls too far behind gets `RecvError::Lagged`. The
    ///   caller recovers by calling [`Engine::status`] and carrying on.
    fn events(&self) -> broadcast::Receiver<EngineEvent>;

    /// Stop syncing a folder without forgetting it.
    ///
    /// - The folder moves to `Paused` and `FolderStateChanged` follows.
    /// - Transfers in flight stop. Partial data is kept so `resume` can
    ///   continue instead of starting over.
    /// - Pausing a paused folder is `Ok`. An unknown folder is `NotFound`.
    fn pause(&self, folder: &FolderId) -> impl Future<Output = Result<(), EngineError>> + Send;

    /// Undo [`Engine::pause`]. The folder moves to `Scanning`.
    ///
    /// - Resuming a folder that is not paused is `Ok`.
    /// - An unknown folder is `NotFound`.
    fn resume(&self, folder: &FolderId) -> impl Future<Output = Result<(), EngineError>> + Send;
}
