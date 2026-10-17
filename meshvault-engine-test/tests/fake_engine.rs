//! `FakeEngine` against the shared contract, plus its own simulation hooks.

use meshvault_engine::{Engine, EngineError, EngineEvent, PeerId, PeerSpec};
use meshvault_engine_test::FakeEngine;

mod contract {
    use super::FakeEngine;
    meshvault_engine_test::engine_contract_tests!(FakeEngine::new());
}

fn peer(name: &str) -> PeerSpec {
    PeerSpec {
        id: PeerId(name.to_string()),
        name: name.to_string(),
    }
}

#[tokio::test]
async fn connect_peer_marks_it_connected_and_emits_once() {
    let engine = FakeEngine::new();
    engine.add_peer(peer("a")).await.unwrap();
    let mut rx = engine.events();

    engine.connect_peer(&PeerId("a".into())).unwrap();
    engine.connect_peer(&PeerId("a".into())).unwrap();

    let status = engine.status().await.unwrap();
    assert!(status.peers[0].connected);
    assert_eq!(
        rx.try_recv().unwrap(),
        EngineEvent::PeerConnected(PeerId("a".into()))
    );
    assert!(rx.try_recv().is_err());
}

#[tokio::test]
async fn disconnect_peer_marks_it_offline_and_emits_once() {
    let engine = FakeEngine::new();
    engine.add_peer(peer("a")).await.unwrap();
    engine.connect_peer(&PeerId("a".into())).unwrap();
    let mut rx = engine.events();

    engine.disconnect_peer(&PeerId("a".into())).unwrap();
    engine.disconnect_peer(&PeerId("a".into())).unwrap();

    assert!(!engine.status().await.unwrap().peers[0].connected);
    assert_eq!(
        rx.try_recv().unwrap(),
        EngineEvent::PeerDisconnected(PeerId("a".into()))
    );
    assert!(rx.try_recv().is_err());
}

#[tokio::test]
async fn connecting_an_unknown_peer_is_not_found() {
    let engine = FakeEngine::new();
    let ghost = PeerId("ghost".into());

    assert_eq!(engine.connect_peer(&ghost), Err(EngineError::NotFound));
    assert_eq!(engine.disconnect_peer(&ghost), Err(EngineError::NotFound));
}

#[tokio::test]
async fn removing_a_connected_peer_emits_disconnected() {
    let engine = FakeEngine::new();
    engine.add_peer(peer("a")).await.unwrap();
    engine.connect_peer(&PeerId("a".into())).unwrap();
    let mut rx = engine.events();

    engine.remove_peer(&PeerId("a".into())).await.unwrap();

    assert_eq!(
        rx.try_recv().unwrap(),
        EngineEvent::PeerDisconnected(PeerId("a".into()))
    );
}
