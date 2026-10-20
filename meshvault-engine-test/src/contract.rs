//! The contract tests. Each function takes a fresh, running, empty engine and
//! panics if the engine breaks a rule from the [`meshvault_engine`] docs.
//!
//! Run them all with [`engine_contract_tests!`](crate::engine_contract_tests).
//! Rules that depend on how a backend fails, such as `Unavailable`, are tested
//! in the backend's own crate.
//!
//! A real backend keeps working after a call returns: a new folder scans and
//! then reports `UpToDate` by itself. The tests allow for that. Where a test
//! needs "nothing more happens" it first waits for the engine to go quiet (see
//! [`Fixtures::settle`]), and where it compares a sequence of states it ignores
//! `UpToDate`, which a backend may report between the states the caller asked
//! for. An engine that never reports `UpToDate`, like `FakeEngine`, passes too.

use std::path::PathBuf;
use std::time::Duration;

use meshvault_engine::{
    Engine, EngineError, EngineEvent, FolderId, FolderSpec, FolderState, FolderStatus, PeerId,
    PeerSpec,
};
use tokio::sync::broadcast;
use tokio::time::timeout;

use crate::Fixtures;

const EVENT_WAIT: Duration = Duration::from_secs(5);
const QUIET_WAIT: Duration = Duration::from_millis(100);
const SETTLE_LIMIT: Duration = Duration::from_secs(30);

fn peer_id(fx: &Fixtures, name: &str) -> PeerId {
    fx.make_peer_id(name)
}

fn peer(fx: &Fixtures, name: &str) -> PeerSpec {
    PeerSpec {
        id: peer_id(fx, name),
        name: name.to_string(),
    }
}

fn folder_id(name: &str) -> FolderId {
    FolderId(format!("folder-{name}"))
}

fn folder_path(fx: &Fixtures, name: &str) -> PathBuf {
    fx.root().join(name)
}

fn folder(fx: &Fixtures, name: &str, peers: &[&str]) -> FolderSpec {
    FolderSpec {
        id: folder_id(name),
        path: folder_path(fx, name),
        peers: peers.iter().map(|p| peer_id(fx, p)).collect(),
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

/// Collect events until the engine has been quiet for the fixtures' settle time.
async fn settle(rx: &mut broadcast::Receiver<EngineEvent>, fx: &Fixtures) -> Vec<EngineEvent> {
    let mut seen = Vec::new();
    let started = tokio::time::Instant::now();
    while let Ok(got) = timeout(fx.settle_time(), rx.recv()).await {
        seen.push(got.expect("event stream closed or lagged"));
        assert!(
            started.elapsed() < SETTLE_LIMIT,
            "engine never went quiet; last events: {seen:?}"
        );
    }
    seen
}

/// Wait until the engine is quiet and drop whatever it said meanwhile.
async fn let_settle<E: Engine>(engine: &E, fx: &Fixtures) {
    let mut rx = engine.events();
    settle(&mut rx, fx).await;
}

/// The states `name` moved through, without `UpToDate` and with repeats
/// collapsed. This is the sequence the caller asked for.
fn requested_states(events: &[EngineEvent], name: &str) -> Vec<FolderState> {
    let mut states: Vec<FolderState> = Vec::new();
    for event in events {
        if let EngineEvent::FolderStateChanged { folder, state } = event {
            if *folder == folder_id(name)
                && *state != FolderState::UpToDate
                && states.last() != Some(state)
            {
                states.push(*state);
            }
        }
    }
    states
}

/// Every event is the folder moving between `Scanning` and `UpToDate`.
fn assert_only_settling(events: &[EngineEvent], name: &str) {
    for event in events {
        let settling = matches!(
            event,
            EngineEvent::FolderStateChanged { folder, state }
                if *folder == folder_id(name)
                    && matches!(state, FolderState::Scanning | FolderState::UpToDate)
        );
        assert!(settling, "unexpected event while settling: {event:?}");
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
pub async fn new_engine_is_empty<E: Engine>(engine: &E, _fx: &Fixtures) {
    let status = engine.status().await.expect("status");
    assert!(status.peers.is_empty());
    assert!(status.folders.is_empty());
}

/// An added peer shows up in `status` and starts disconnected.
pub async fn add_peer_appears_in_status<E: Engine>(engine: &E, fx: &Fixtures) {
    engine.add_peer(peer(fx, "a")).await.expect("add_peer");

    let status = engine.status().await.expect("status");
    assert_eq!(status.peers.len(), 1);
    assert_eq!(status.peers[0].id, peer_id(fx, "a"));
    assert!(!status.peers[0].connected);
}

/// Adding the same peer again, even under a new name, succeeds and adds nothing.
pub async fn add_peer_is_idempotent<E: Engine>(engine: &E, fx: &Fixtures) {
    engine
        .add_peer(peer(fx, "a"))
        .await
        .expect("first add_peer");
    engine
        .add_peer(peer(fx, "a"))
        .await
        .expect("second add_peer");
    engine
        .add_peer(PeerSpec {
            id: peer_id(fx, "a"),
            name: "renamed".to_string(),
        })
        .await
        .expect("add_peer with new name");

    assert_eq!(peer_ids(engine).await, vec![peer_id(fx, "a")]);
}

/// A removed peer is gone from `status`; other peers stay.
pub async fn remove_peer_removes_only_that_peer<E: Engine>(engine: &E, fx: &Fixtures) {
    engine.add_peer(peer(fx, "a")).await.expect("add a");
    engine.add_peer(peer(fx, "b")).await.expect("add b");

    engine
        .remove_peer(&peer_id(fx, "a"))
        .await
        .expect("remove_peer");

    assert_eq!(peer_ids(engine).await, vec![peer_id(fx, "b")]);
}

/// Removing an unknown peer, or the same peer twice, is `Ok`.
pub async fn remove_peer_is_idempotent<E: Engine>(engine: &E, fx: &Fixtures) {
    engine
        .remove_peer(&peer_id(fx, "never-added"))
        .await
        .expect("remove unknown peer");

    engine.add_peer(peer(fx, "a")).await.expect("add_peer");
    engine
        .remove_peer(&peer_id(fx, "a"))
        .await
        .expect("first remove");
    engine
        .remove_peer(&peer_id(fx, "a"))
        .await
        .expect("second remove");

    assert!(peer_ids(engine).await.is_empty());
}

/// A removed peer is no longer trusted, so new folders cannot list it.
pub async fn removed_peer_cannot_be_shared_with<E: Engine>(engine: &E, fx: &Fixtures) {
    engine.add_peer(peer(fx, "a")).await.expect("add_peer");
    engine
        .remove_peer(&peer_id(fx, "a"))
        .await
        .expect("remove_peer");

    let result = engine.share_folder(folder(fx, "docs", &["a"])).await;

    assert!(matches!(result, Err(EngineError::InvalidArgument(_))));
}

/// A new folder is reported as `Scanning` first. A backend that finishes
/// scanning on its own may follow with `UpToDate`, and nothing else.
pub async fn share_folder_starts_scanning<E: Engine>(engine: &E, fx: &Fixtures) {
    engine.add_peer(peer(fx, "a")).await.expect("add_peer");
    let mut rx = engine.events();

    engine
        .share_folder(folder(fx, "docs", &["a"]))
        .await
        .expect("share_folder");

    assert_eq!(
        next_event(&mut rx).await,
        state_changed("docs", FolderState::Scanning)
    );
    let status = folder_status(engine, "docs")
        .await
        .expect("folder in status");
    assert!(matches!(
        status.state,
        FolderState::Scanning | FolderState::UpToDate
    ));
    assert_only_settling(&settle(&mut rx, fx).await, "docs");
}

/// A folder may be shared with no peers at all.
pub async fn share_folder_without_peers_is_ok<E: Engine>(engine: &E, fx: &Fixtures) {
    engine
        .share_folder(folder(fx, "solo", &[]))
        .await
        .expect("share_folder");

    assert!(folder_status(engine, "solo").await.is_some());
}

/// A relative path is `InvalidArgument`, and nothing is created or emitted.
pub async fn share_folder_rejects_relative_path<E: Engine>(engine: &E, _fx: &Fixtures) {
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
pub async fn share_folder_rejects_unknown_peer<E: Engine>(engine: &E, fx: &Fixtures) {
    let mut rx = engine.events();

    let result = engine.share_folder(folder(fx, "docs", &["ghost"])).await;

    assert!(matches!(result, Err(EngineError::InvalidArgument(_))));
    assert!(folder_status(engine, "docs").await.is_none());
    assert_no_event(&mut rx).await;
}

/// The same ID with a different path is `Conflict` and leaves the folder alone.
pub async fn share_folder_same_id_other_path_conflicts<E: Engine>(engine: &E, fx: &Fixtures) {
    engine
        .share_folder(folder(fx, "docs", &[]))
        .await
        .expect("share_folder");
    let_settle(engine, fx).await;
    let mut rx = engine.events();

    let result = engine
        .share_folder(FolderSpec {
            id: folder_id("docs"),
            path: folder_path(fx, "elsewhere"),
            peers: vec![],
        })
        .await;

    assert_eq!(result, Err(EngineError::Conflict));
    let status = engine.status().await.expect("status");
    assert_eq!(status.folders.len(), 1);
    assert_no_event(&mut rx).await;
}

/// Sharing the same folder again succeeds, adds nothing and emits nothing.
pub async fn share_folder_is_idempotent<E: Engine>(engine: &E, fx: &Fixtures) {
    engine.add_peer(peer(fx, "a")).await.expect("add_peer");
    engine
        .share_folder(folder(fx, "docs", &["a"]))
        .await
        .expect("first share");
    let_settle(engine, fx).await;
    let mut rx = engine.events();

    engine
        .share_folder(folder(fx, "docs", &["a"]))
        .await
        .expect("second share");

    assert_eq!(engine.status().await.expect("status").folders.len(), 1);
    assert_no_event(&mut rx).await;
}

/// Re-sharing replaces the peer list; the new list is checked like a first share.
pub async fn share_folder_again_replaces_peers<E: Engine>(engine: &E, fx: &Fixtures) {
    engine.add_peer(peer(fx, "a")).await.expect("add a");
    engine.add_peer(peer(fx, "b")).await.expect("add b");
    engine
        .share_folder(folder(fx, "docs", &["a"]))
        .await
        .expect("first share");

    engine
        .share_folder(folder(fx, "docs", &["a", "b"]))
        .await
        .expect("share with more peers");
    engine
        .share_folder(folder(fx, "docs", &["b"]))
        .await
        .expect("share with fewer peers");

    let result = engine
        .share_folder(folder(fx, "docs", &["a", "ghost"]))
        .await;
    assert!(matches!(result, Err(EngineError::InvalidArgument(_))));
    assert_eq!(engine.status().await.expect("status").folders.len(), 1);
}

/// Pausing moves the folder to `Paused` in `status` and emits the change.
pub async fn pause_moves_folder_to_paused<E: Engine>(engine: &E, fx: &Fixtures) {
    engine
        .share_folder(folder(fx, "docs", &[]))
        .await
        .expect("share_folder");
    let_settle(engine, fx).await;
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
pub async fn pause_is_idempotent<E: Engine>(engine: &E, fx: &Fixtures) {
    engine
        .share_folder(folder(fx, "docs", &[]))
        .await
        .expect("share_folder");
    engine.pause(&folder_id("docs")).await.expect("first pause");
    let_settle(engine, fx).await;
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
pub async fn resume_moves_folder_to_scanning<E: Engine>(engine: &E, fx: &Fixtures) {
    engine
        .share_folder(folder(fx, "docs", &[]))
        .await
        .expect("share_folder");
    engine.pause(&folder_id("docs")).await.expect("pause");
    let_settle(engine, fx).await;
    let mut rx = engine.events();

    engine.resume(&folder_id("docs")).await.expect("resume");

    assert_eq!(
        next_event(&mut rx).await,
        state_changed("docs", FolderState::Scanning)
    );
    let status = folder_status(engine, "docs")
        .await
        .expect("folder in status");
    assert!(matches!(
        status.state,
        FolderState::Scanning | FolderState::UpToDate
    ));
}

/// Resuming a folder that is not paused is `Ok`, changes nothing and emits nothing.
pub async fn resume_running_folder_is_a_no_op<E: Engine>(engine: &E, fx: &Fixtures) {
    engine
        .share_folder(folder(fx, "docs", &[]))
        .await
        .expect("share_folder");
    let_settle(engine, fx).await;
    let before = folder_status(engine, "docs")
        .await
        .expect("folder in status");
    let mut rx = engine.events();

    engine.resume(&folder_id("docs")).await.expect("resume");

    let after = folder_status(engine, "docs")
        .await
        .expect("folder in status");
    assert_eq!(after, before);
    assert_no_event(&mut rx).await;
}

/// Pause and resume on an unknown folder are `NotFound` and emit nothing.
pub async fn pause_and_resume_unknown_folder_are_not_found<E: Engine>(engine: &E, _fx: &Fixtures) {
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
pub async fn events_arrive_in_order<E: Engine>(engine: &E, fx: &Fixtures) {
    let mut rx = engine.events();

    engine
        .share_folder(folder(fx, "docs", &[]))
        .await
        .expect("share_folder");
    engine.pause(&folder_id("docs")).await.expect("pause");
    engine.resume(&folder_id("docs")).await.expect("resume");
    engine.pause(&folder_id("docs")).await.expect("pause again");

    let events = settle(&mut rx, fx).await;
    assert_eq!(
        requested_states(&events, "docs"),
        [
            FolderState::Scanning,
            FolderState::Paused,
            FolderState::Scanning,
            FolderState::Paused,
        ]
    );
    assert_eq!(
        events.last(),
        Some(&state_changed("docs", FolderState::Paused)),
        "nothing may follow the final pause"
    );
}

/// `status` agrees with the last event seen for a folder.
pub async fn status_agrees_with_last_event<E: Engine>(engine: &E, fx: &Fixtures) {
    let mut rx = engine.events();
    engine
        .share_folder(folder(fx, "docs", &[]))
        .await
        .expect("share_folder");
    engine.pause(&folder_id("docs")).await.expect("pause");
    engine.resume(&folder_id("docs")).await.expect("resume");

    let events = settle(&mut rx, fx).await;
    let last = events.iter().rev().find_map(|event| match event {
        EngineEvent::FolderStateChanged { folder, state } if *folder == folder_id("docs") => {
            Some(*state)
        }
        _ => None,
    });

    let status = folder_status(engine, "docs")
        .await
        .expect("folder in status");
    assert_eq!(Some(status.state), last);
}

/// Every subscriber sees every event, in the same order.
pub async fn every_subscriber_sees_every_event<E: Engine>(engine: &E, fx: &Fixtures) {
    let mut first = engine.events();
    let mut second = engine.events();

    engine
        .share_folder(folder(fx, "docs", &[]))
        .await
        .expect("share_folder");
    engine.pause(&folder_id("docs")).await.expect("pause");

    let seen_first = settle(&mut first, fx).await;
    let seen_second = settle(&mut second, fx).await;
    assert_eq!(seen_first, seen_second);
    assert_eq!(
        requested_states(&seen_first, "docs"),
        [FolderState::Scanning, FolderState::Paused]
    );
}

/// A subscriber sees only events emitted after it subscribed.
pub async fn subscriber_sees_only_later_events<E: Engine>(engine: &E, fx: &Fixtures) {
    engine
        .share_folder(folder(fx, "docs", &[]))
        .await
        .expect("share_folder");
    let_settle(engine, fx).await;
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
///
/// A backend whose peer IDs have a fixed shape, or that needs its own
/// directories, builds the engine together with its [`Fixtures`]:
///
/// ```ignore
/// meshvault_engine_test::engine_contract_tests!(fixtures: {
///     let engine = MyEngine::start().await;
///     (engine, Fixtures::new().peer_ids(my_peer_id))
/// });
/// ```
#[macro_export]
macro_rules! engine_contract_tests {
    (fixtures: $make:expr) => {
        $crate::engine_contract_tests!(@all $make);
    };
    ($make:expr) => {
        $crate::engine_contract_tests!(@all ($make, $crate::Fixtures::new()));
    };
    (@all $make:expr) => {
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
                let (engine, fixtures) = $make;
                $crate::contract::$name(&engine, &fixtures).await;
                drop(engine);
                drop(fixtures);
            }
        )+
    };
}
