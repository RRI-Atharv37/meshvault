//! The contract tests. Each function takes a fresh, running, empty engine and
//! panics if the engine breaks a rule from the [`meshvault_engine`] docs.
//!
//! Run them all with [`engine_contract_tests!`](crate::engine_contract_tests).
//! Rules that depend on how a backend fails, such as `Unavailable`, are tested
//! in the backend's own crate.

use std::path::PathBuf;
use std::time::Duration;

use meshvault_engine::{
    Engine, EngineError, EngineEvent, FolderId, FolderSpec, FolderState, FolderStatus, PeerId,
    PeerSpec,
};
use tokio::sync::broadcast;
use tokio::time::timeout;

const EVENT_WAIT: Duration = Duration::from_secs(5);
const QUIET_WAIT: Duration = Duration::from_millis(100);

fn peer_id(name: &str) -> PeerId {
    PeerId(format!("peer-{name}"))
}

fn peer(name: &str) -> PeerSpec {
    PeerSpec {
        id: peer_id(name),
        name: name.to_string(),
    }
}

fn folder_id(name: &str) -> FolderId {
    FolderId(format!("folder-{name}"))
}

fn folder_path(name: &str) -> PathBuf {
    std::env::temp_dir().join("meshvault-contract").join(name)
}

fn folder(name: &str, peers: &[&str]) -> FolderSpec {
    FolderSpec {
        id: folder_id(name),
        path: folder_path(name),
        peers: peers.iter().map(|p| peer_id(p)).collect(),
    }
}

fn state_changed(name: &str, state: FolderState) -> EngineEvent {
    EngineEvent::FolderStateChanged {
        folder: folder_id(name),
        state,
    }
}

async fn next_event(rx: &mut broadcast::Receiver<EngineEvent>) -> EngineEvent {
    timeout(EVENT_WAIT, rx.recv())
        .await
        .expect("timed out waiting for an event")
        .expect("event stream closed or lagged")
}

async fn assert_no_event(rx: &mut broadcast::Receiver<EngineEvent>) {
    if let Ok(got) = timeout(QUIET_WAIT, rx.recv()).await {
        panic!("expected no event, got {got:?}");
    }
}

async fn folder_status<E: Engine>(engine: &E, name: &str) -> Option<FolderStatus> {
    let id = folder_id(name);
    let status = engine.status().await.expect("status");
    status.folders.into_iter().find(|f| f.id == id)
}

async fn peer_ids<E: Engine>(engine: &E) -> Vec<PeerId> {
    let status = engine.status().await.expect("status");
    status.peers.into_iter().map(|p| p.id).collect()
}

/// A new engine has no peers and no folders.
pub async fn new_engine_is_empty<E: Engine>(engine: &E) {
    let status = engine.status().await.expect("status");
    assert!(status.peers.is_empty());
    assert!(status.folders.is_empty());
}

/// An added peer shows up in `status` and starts disconnected.
pub async fn add_peer_appears_in_status<E: Engine>(engine: &E) {
    engine.add_peer(peer("a")).await.expect("add_peer");

    let status = engine.status().await.expect("status");
    assert_eq!(status.peers.len(), 1);
    assert_eq!(status.peers[0].id, peer_id("a"));
    assert!(!status.peers[0].connected);
}

/// Adding the same peer again, even under a new name, succeeds and adds nothing.
pub async fn add_peer_is_idempotent<E: Engine>(engine: &E) {
    engine.add_peer(peer("a")).await.expect("first add_peer");
    engine.add_peer(peer("a")).await.expect("second add_peer");
    engine
        .add_peer(PeerSpec {
            id: peer_id("a"),
            name: "renamed".to_string(),
        })
        .await
        .expect("add_peer with new name");

    assert_eq!(peer_ids(engine).await, vec![peer_id("a")]);
}

/// A removed peer is gone from `status`; other peers stay.
pub async fn remove_peer_removes_only_that_peer<E: Engine>(engine: &E) {
    engine.add_peer(peer("a")).await.expect("add a");
    engine.add_peer(peer("b")).await.expect("add b");

    engine
        .remove_peer(&peer_id("a"))
        .await
        .expect("remove_peer");

    assert_eq!(peer_ids(engine).await, vec![peer_id("b")]);
}

/// Removing an unknown peer, or the same peer twice, is `Ok`.
pub async fn remove_peer_is_idempotent<E: Engine>(engine: &E) {
    engine
        .remove_peer(&peer_id("never-added"))
        .await
        .expect("remove unknown peer");

    engine.add_peer(peer("a")).await.expect("add_peer");
    engine
        .remove_peer(&peer_id("a"))
        .await
        .expect("first remove");
    engine
        .remove_peer(&peer_id("a"))
        .await
        .expect("second remove");

    assert!(peer_ids(engine).await.is_empty());
}

/// A removed peer is no longer trusted, so new folders cannot list it.
pub async fn removed_peer_cannot_be_shared_with<E: Engine>(engine: &E) {
    engine.add_peer(peer("a")).await.expect("add_peer");
    engine
        .remove_peer(&peer_id("a"))
        .await
        .expect("remove_peer");

    let result = engine.share_folder(folder("docs", &["a"])).await;

    assert!(matches!(result, Err(EngineError::InvalidArgument(_))));
}

/// A new folder appears in `status` as `Scanning` and emits one state event.
pub async fn share_folder_starts_scanning<E: Engine>(engine: &E) {
    engine.add_peer(peer("a")).await.expect("add_peer");
    let mut rx = engine.events();

    engine
        .share_folder(folder("docs", &["a"]))
        .await
        .expect("share_folder");

    let status = folder_status(engine, "docs")
        .await
        .expect("folder in status");
    assert_eq!(status.state, FolderState::Scanning);
    assert_eq!(
        next_event(&mut rx).await,
        state_changed("docs", FolderState::Scanning)
    );
    assert_no_event(&mut rx).await;
}

/// A folder may be shared with no peers at all.
pub async fn share_folder_without_peers_is_ok<E: Engine>(engine: &E) {
    engine
        .share_folder(folder("solo", &[]))
        .await
        .expect("share_folder");

    assert!(folder_status(engine, "solo").await.is_some());
}

/// A relative path is `InvalidArgument`, and nothing is created or emitted.
pub async fn share_folder_rejects_relative_path<E: Engine>(engine: &E) {
    let mut rx = engine.events();
    let spec = FolderSpec {
        id: folder_id("rel"),
        path: PathBuf::from("relative/path"),
        peers: vec![],
    };

    let result = engine.share_folder(spec).await;

    assert!(matches!(result, Err(EngineError::InvalidArgument(_))));
    assert!(folder_status(engine, "rel").await.is_none());
    assert_no_event(&mut rx).await;
}

/// An unknown peer in the peer list is `InvalidArgument`, and nothing is created or emitted.
pub async fn share_folder_rejects_unknown_peer<E: Engine>(engine: &E) {
    let mut rx = engine.events();

    let result = engine.share_folder(folder("docs", &["ghost"])).await;

    assert!(matches!(result, Err(EngineError::InvalidArgument(_))));
    assert!(folder_status(engine, "docs").await.is_none());
    assert_no_event(&mut rx).await;
}

/// The same ID with a different path is `Conflict` and leaves the folder alone.
pub async fn share_folder_same_id_other_path_conflicts<E: Engine>(engine: &E) {
    engine
        .share_folder(folder("docs", &[]))
        .await
        .expect("share_folder");
    let mut rx = engine.events();

    let result = engine
        .share_folder(FolderSpec {
            id: folder_id("docs"),
            path: folder_path("elsewhere"),
            peers: vec![],
        })
        .await;

    assert_eq!(result, Err(EngineError::Conflict));
    let status = engine.status().await.expect("status");
    assert_eq!(status.folders.len(), 1);
    assert_no_event(&mut rx).await;
}

/// Sharing the same folder again succeeds, adds nothing and emits nothing.
pub async fn share_folder_is_idempotent<E: Engine>(engine: &E) {
    engine.add_peer(peer("a")).await.expect("add_peer");
    engine
        .share_folder(folder("docs", &["a"]))
        .await
        .expect("first share");
    let mut rx = engine.events();

    engine
        .share_folder(folder("docs", &["a"]))
        .await
        .expect("second share");

    assert_eq!(engine.status().await.expect("status").folders.len(), 1);
    assert_no_event(&mut rx).await;
}

/// Re-sharing replaces the peer list; the new list is checked like a first share.
pub async fn share_folder_again_replaces_peers<E: Engine>(engine: &E) {
    engine.add_peer(peer("a")).await.expect("add a");
    engine.add_peer(peer("b")).await.expect("add b");
    engine
        .share_folder(folder("docs", &["a"]))
        .await
        .expect("first share");

    engine
        .share_folder(folder("docs", &["a", "b"]))
        .await
        .expect("share with more peers");
    engine
        .share_folder(folder("docs", &["b"]))
        .await
        .expect("share with fewer peers");

    let result = engine.share_folder(folder("docs", &["a", "ghost"])).await;
    assert!(matches!(result, Err(EngineError::InvalidArgument(_))));
    assert_eq!(engine.status().await.expect("status").folders.len(), 1);
}

/// Pausing moves the folder to `Paused` in `status` and emits the change.
pub async fn pause_moves_folder_to_paused<E: Engine>(engine: &E) {
    engine
        .share_folder(folder("docs", &[]))
        .await
        .expect("share_folder");
    let mut rx = engine.events();

    engine.pause(&folder_id("docs")).await.expect("pause");

    let status = folder_status(engine, "docs")
        .await
        .expect("folder in status");
    assert_eq!(status.state, FolderState::Paused);
    assert_eq!(
        next_event(&mut rx).await,
        state_changed("docs", FolderState::Paused)
    );
}

/// Pausing a paused folder is `Ok` and emits nothing.
pub async fn pause_is_idempotent<E: Engine>(engine: &E) {
    engine
        .share_folder(folder("docs", &[]))
        .await
        .expect("share_folder");
    engine.pause(&folder_id("docs")).await.expect("first pause");
    let mut rx = engine.events();

    engine
        .pause(&folder_id("docs"))
        .await
        .expect("second pause");

    let status = folder_status(engine, "docs")
        .await
        .expect("folder in status");
    assert_eq!(status.state, FolderState::Paused);
    assert_no_event(&mut rx).await;
}

/// Resuming a paused folder moves it to `Scanning` and emits the change.
pub async fn resume_moves_folder_to_scanning<E: Engine>(engine: &E) {
    engine
        .share_folder(folder("docs", &[]))
        .await
        .expect("share_folder");
    engine.pause(&folder_id("docs")).await.expect("pause");
    let mut rx = engine.events();

    engine.resume(&folder_id("docs")).await.expect("resume");

    let status = folder_status(engine, "docs")
        .await
        .expect("folder in status");
    assert_eq!(status.state, FolderState::Scanning);
    assert_eq!(
        next_event(&mut rx).await,
        state_changed("docs", FolderState::Scanning)
    );
}

/// Resuming a folder that is not paused is `Ok`, changes nothing and emits nothing.
pub async fn resume_running_folder_is_a_no_op<E: Engine>(engine: &E) {
    engine
        .share_folder(folder("docs", &[]))
        .await
        .expect("share_folder");
    let mut rx = engine.events();

    engine.resume(&folder_id("docs")).await.expect("resume");

    let status = folder_status(engine, "docs")
        .await
        .expect("folder in status");
    assert_eq!(status.state, FolderState::Scanning);
    assert_no_event(&mut rx).await;
}

/// Pause and resume on an unknown folder are `NotFound` and emit nothing.
pub async fn pause_and_resume_unknown_folder_are_not_found<E: Engine>(engine: &E) {
    let mut rx = engine.events();

    assert_eq!(
        engine.pause(&folder_id("ghost")).await,
        Err(EngineError::NotFound)
    );
    assert_eq!(
        engine.resume(&folder_id("ghost")).await,
        Err(EngineError::NotFound)
    );
    assert_no_event(&mut rx).await;
}

/// Events arrive in the order the changes were made.
pub async fn events_arrive_in_order<E: Engine>(engine: &E) {
    let mut rx = engine.events();

    engine
        .share_folder(folder("docs", &[]))
        .await
        .expect("share_folder");
    engine.pause(&folder_id("docs")).await.expect("pause");
    engine.resume(&folder_id("docs")).await.expect("resume");
    engine.pause(&folder_id("docs")).await.expect("pause again");

    let expected = [
        FolderState::Scanning,
        FolderState::Paused,
        FolderState::Scanning,
        FolderState::Paused,
    ];
    for state in expected {
        assert_eq!(next_event(&mut rx).await, state_changed("docs", state));
    }
    assert_no_event(&mut rx).await;
}

/// `status` agrees with the last event seen for a folder.
pub async fn status_agrees_with_last_event<E: Engine>(engine: &E) {
    let mut rx = engine.events();
    engine
        .share_folder(folder("docs", &[]))
        .await
        .expect("share_folder");
    engine.pause(&folder_id("docs")).await.expect("pause");
    engine.resume(&folder_id("docs")).await.expect("resume");

    let mut last = None;
    for _ in 0..3 {
        if let EngineEvent::FolderStateChanged { state, .. } = next_event(&mut rx).await {
            last = Some(state);
        }
    }

    let status = folder_status(engine, "docs")
        .await
        .expect("folder in status");
    assert_eq!(Some(status.state), last);
}

/// Every subscriber sees every event, in the same order.
pub async fn every_subscriber_sees_every_event<E: Engine>(engine: &E) {
    let mut first = engine.events();
    let mut second = engine.events();

    engine
        .share_folder(folder("docs", &[]))
        .await
        .expect("share_folder");
    engine.pause(&folder_id("docs")).await.expect("pause");

    for rx in [&mut first, &mut second] {
        assert_eq!(
            next_event(rx).await,
            state_changed("docs", FolderState::Scanning)
        );
        assert_eq!(
            next_event(rx).await,
            state_changed("docs", FolderState::Paused)
        );
    }
}

/// A subscriber sees only events emitted after it subscribed.
pub async fn subscriber_sees_only_later_events<E: Engine>(engine: &E) {
    engine
        .share_folder(folder("docs", &[]))
        .await
        .expect("share_folder");
    let mut rx = engine.events();

    engine.pause(&folder_id("docs")).await.expect("pause");

    assert_eq!(
        next_event(&mut rx).await,
        state_changed("docs", FolderState::Paused)
    );
    assert_no_event(&mut rx).await;
}

/// Expands to one `#[tokio::test]` per contract test, each on a fresh engine.
///
/// The argument is an expression that builds the engine. It runs inside an
/// async test, so it may use `.await`. The calling crate needs `tokio` with the
/// `rt` and `macros` features as a dev-dependency.
///
/// ```ignore
/// meshvault_engine_test::engine_contract_tests!(MyEngine::start().await);
/// ```
#[macro_export]
macro_rules! engine_contract_tests {
    ($make:expr) => {
        $crate::engine_contract_tests!(@each $make;
            new_engine_is_empty,
            add_peer_appears_in_status,
            add_peer_is_idempotent,
            remove_peer_removes_only_that_peer,
            remove_peer_is_idempotent,
            removed_peer_cannot_be_shared_with,
            share_folder_starts_scanning,
            share_folder_without_peers_is_ok,
            share_folder_rejects_relative_path,
            share_folder_rejects_unknown_peer,
            share_folder_same_id_other_path_conflicts,
            share_folder_is_idempotent,
            share_folder_again_replaces_peers,
            pause_moves_folder_to_paused,
            pause_is_idempotent,
            resume_moves_folder_to_scanning,
            resume_running_folder_is_a_no_op,
            pause_and_resume_unknown_folder_are_not_found,
            events_arrive_in_order,
            status_agrees_with_last_event,
            every_subscriber_sees_every_event,
            subscriber_sees_only_later_events,
        );
    };
    (@each $make:expr; $($name:ident),+ $(,)?) => {
        $(
            #[tokio::test]
            async fn $name() {
                let engine = $make;
                $crate::contract::$name(&engine).await;
            }
        )+
    };
}
