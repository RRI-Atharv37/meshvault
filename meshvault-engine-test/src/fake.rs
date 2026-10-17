//! In-memory [`Engine`] used by tests of code that sits on top of an engine.

use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard, PoisonError};

use meshvault_engine::{
    Engine, EngineError, EngineEvent, EngineStatus, FolderId, FolderSpec, FolderState,
    FolderStatus, PeerId, PeerSpec, PeerStatus,
};
use tokio::sync::broadcast;

const EVENT_BUFFER: usize = 256;

#[derive(Debug)]
struct PeerEntry {
    id: PeerId,
    connected: bool,
}

#[derive(Debug)]
struct FolderEntry {
    id: FolderId,
    path: PathBuf,
    peers: Vec<PeerId>,
    state: FolderState,
}

#[derive(Debug, Default)]
struct State {
    peers: Vec<PeerEntry>,
    folders: Vec<FolderEntry>,
}

/// An [`Engine`] that keeps everything in memory and never touches the network
/// or the disk. It follows the full contract, so it can stand in for a real
/// engine in tests.
///
/// Peers start disconnected. Use [`FakeEngine::connect_peer`] and
/// [`FakeEngine::disconnect_peer`] to simulate the network.
#[derive(Debug)]
pub struct FakeEngine {
    state: Mutex<State>,
    events: broadcast::Sender<EngineEvent>,
}

impl FakeEngine {
    /// Create an empty engine with no peers and no folders.
    pub fn new() -> Self {
        let (events, _) = broadcast::channel(EVENT_BUFFER);
        Self {
            state: Mutex::new(State::default()),
            events,
        }
    }

    /// Simulate a known peer coming online. Emits `PeerConnected` on a change.
    ///
    /// Returns `NotFound` if the peer was never added.
    pub fn connect_peer(&self, peer: &PeerId) -> Result<(), EngineError> {
        self.set_connected(peer, true)
    }

    /// Simulate a known peer going offline. Emits `PeerDisconnected` on a change.
    ///
    /// Returns `NotFound` if the peer was never added.
    pub fn disconnect_peer(&self, peer: &PeerId) -> Result<(), EngineError> {
        self.set_connected(peer, false)
    }

    fn set_connected(&self, peer: &PeerId, connected: bool) -> Result<(), EngineError> {
        let mut state = self.lock();
        let entry = state
            .peers
            .iter_mut()
            .find(|p| &p.id == peer)
            .ok_or(EngineError::NotFound)?;
        if entry.connected != connected {
            entry.connected = connected;
            self.emit(if connected {
                EngineEvent::PeerConnected(peer.clone())
            } else {
                EngineEvent::PeerDisconnected(peer.clone())
            });
        }
        Ok(())
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn emit(&self, event: EngineEvent) {
        // No receivers is fine: events are only for whoever is listening.
        let _ = self.events.send(event);
    }

    fn transition(
        &self,
        folder: &FolderId,
        to: FolderState,
        applies: impl Fn(FolderState) -> bool,
    ) -> Result<(), EngineError> {
        let mut state = self.lock();
        let entry = state
            .folders
            .iter_mut()
            .find(|f| &f.id == folder)
            .ok_or(EngineError::NotFound)?;
        if applies(entry.state) {
            entry.state = to;
            self.emit(EngineEvent::FolderStateChanged {
                folder: folder.clone(),
                state: to,
            });
        }
        Ok(())
    }
}

impl Default for FakeEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl Engine for FakeEngine {
    async fn add_peer(&self, peer: PeerSpec) -> Result<(), EngineError> {
        let mut state = self.lock();
        if !state.peers.iter().any(|p| p.id == peer.id) {
            state.peers.push(PeerEntry {
                id: peer.id,
                connected: false,
            });
        }
        Ok(())
    }

    async fn remove_peer(&self, peer: &PeerId) -> Result<(), EngineError> {
        let mut state = self.lock();
        let Some(index) = state.peers.iter().position(|p| &p.id == peer) else {
            return Ok(());
        };
        let removed = state.peers.remove(index);
        for folder in &mut state.folders {
            folder.peers.retain(|p| p != peer);
        }
        if removed.connected {
            self.emit(EngineEvent::PeerDisconnected(removed.id));
        }
        Ok(())
    }

    async fn share_folder(&self, spec: FolderSpec) -> Result<(), EngineError> {
        if !spec.path.is_absolute() {
            return Err(EngineError::InvalidArgument(format!(
                "folder path must be absolute: {}",
                spec.path.display()
            )));
        }
        let mut state = self.lock();
        if let Some(unknown) = spec
            .peers
            .iter()
            .find(|p| !state.peers.iter().any(|known| &known.id == *p))
        {
            return Err(EngineError::InvalidArgument(format!(
                "unknown peer: {}",
                unknown.0
            )));
        }
        match state.folders.iter_mut().find(|f| f.id == spec.id) {
            Some(known) if known.path != spec.path => return Err(EngineError::Conflict),
            Some(known) => known.peers = spec.peers,
            None => {
                self.emit(EngineEvent::FolderStateChanged {
                    folder: spec.id.clone(),
                    state: FolderState::Scanning,
                });
                state.folders.push(FolderEntry {
                    id: spec.id,
                    path: spec.path,
                    peers: spec.peers,
                    state: FolderState::Scanning,
                });
            }
        }
        Ok(())
    }

    async fn status(&self) -> Result<EngineStatus, EngineError> {
        let state = self.lock();
        Ok(EngineStatus {
            peers: state
                .peers
                .iter()
                .map(|p| PeerStatus {
                    id: p.id.clone(),
                    connected: p.connected,
                })
                .collect(),
            folders: state
                .folders
                .iter()
                .map(|f| FolderStatus {
                    id: f.id.clone(),
                    state: f.state,
                    bytes_remaining: 0,
                })
                .collect(),
        })
    }

    fn events(&self) -> broadcast::Receiver<EngineEvent> {
        self.events.subscribe()
    }

    async fn pause(&self, folder: &FolderId) -> Result<(), EngineError> {
        self.transition(folder, FolderState::Paused, |s| s != FolderState::Paused)
    }

    async fn resume(&self, folder: &FolderId) -> Result<(), EngineError> {
        self.transition(folder, FolderState::Scanning, |s| s == FolderState::Paused)
    }
}
