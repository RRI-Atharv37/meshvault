use meshvault_core::path::RelPath;
use meshvault_core::time::{FakeClock, Timestamp};
use meshvault_core::watch::{NotifyWatchSource, ReconcileTimer, Watch, WatchEvent, WatchSource};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const HALF_HOUR: Duration = Duration::from_secs(30 * 60);

#[derive(Clone, Default)]
struct Fake(Arc<Mutex<Vec<WatchEvent>>>);

impl Fake {
    fn push(&self, e: WatchEvent) {
        self.0.lock().unwrap().push(e);
    }
}

impl WatchSource for Fake {
    fn drain(&mut self) -> Vec<WatchEvent> {
        std::mem::take(&mut *self.0.lock().unwrap())
    }
}

fn clock() -> FakeClock {
    FakeClock::new(Timestamp(1_800_000_000_000_000_000))
}

#[test]
fn watch_changed_paths_collected() {
    let clock = clock();
    let fake = Fake::default();
    let mut w = Watch::new(fake.clone(), HALF_HOUR, &clock);
    let a = RelPath::parse("a.txt").unwrap();
    let b = RelPath::parse("d/b.txt").unwrap();
    fake.push(WatchEvent::Changed(a.clone()));
    fake.push(WatchEvent::Changed(b.clone()));
    fake.push(WatchEvent::Changed(a.clone()));
    let poll = w.poll(&clock);
    assert!(!poll.reconcile);
    assert_eq!(poll.changed.into_iter().collect::<Vec<_>>(), [a, b]);
    assert!(w.poll(&clock).changed.is_empty());
}

#[test]
fn watch_overflow_requests_reconcile() {
    let clock = clock();
    let fake = Fake::default();
    let mut w = Watch::new(fake.clone(), HALF_HOUR, &clock);
    fake.push(WatchEvent::Overflow);
    assert!(w.poll(&clock).reconcile);
    assert!(!w.poll(&clock).reconcile);
}

#[test]
fn watch_error_requests_reconcile() {
    let clock = clock();
    let fake = Fake::default();
    let mut w = Watch::new(fake.clone(), HALF_HOUR, &clock);
    fake.push(WatchEvent::Error("handle closed".into()));
    assert!(w.poll(&clock).reconcile);
}

#[test]
fn watch_reconcile_resets_the_timer() {
    let clock = clock();
    let fake = Fake::default();
    let mut w = Watch::new(fake.clone(), HALF_HOUR, &clock);
    clock.advance(Duration::from_secs(20 * 60));
    fake.push(WatchEvent::Overflow);
    assert!(w.poll(&clock).reconcile);
    clock.advance(Duration::from_secs(20 * 60));
    assert!(!w.poll(&clock).reconcile);
    clock.advance(Duration::from_secs(10 * 60));
    assert!(w.poll(&clock).reconcile);
}

#[test]
fn reconcile_timer_fires_every_30_min() {
    let clock = clock();
    let mut t = ReconcileTimer::new(HALF_HOUR, &clock);
    assert!(!t.due(&clock));
    clock.advance(Duration::from_secs(29 * 60 + 59));
    assert!(!t.due(&clock));
    clock.advance(Duration::from_secs(1));
    assert!(t.due(&clock));
    t.mark_done(&clock);
    assert!(!t.due(&clock));
    clock.advance(HALF_HOUR);
    assert!(t.due(&clock));
}

#[test]
fn reconcile_timer_survives_clock_jump_back() {
    let clock = clock();
    let mut t = ReconcileTimer::new(HALF_HOUR, &clock);
    clock.jump_back(Duration::from_secs(3600));
    assert!(t.due(&clock));
    t.mark_done(&clock);
    assert!(!t.due(&clock));
    clock.advance(HALF_HOUR);
    assert!(t.due(&clock));
}

#[test]
fn watch_timer_triggers_reconcile_without_events() {
    let clock = clock();
    let mut w = Watch::new(Fake::default(), HALF_HOUR, &clock);
    assert!(!w.poll(&clock).reconcile);
    clock.advance(HALF_HOUR);
    assert!(w.poll(&clock).reconcile);
}

#[test]
fn notify_source_sees_new_file() {
    let dir = tempfile::tempdir().unwrap();
    let mut src = NotifyWatchSource::start(dir.path()).unwrap();
    std::fs::create_dir(dir.path().join("sub")).unwrap();
    std::fs::write(dir.path().join("sub").join("a.txt"), b"x").unwrap();
    std::fs::create_dir(dir.path().join(".meshvault")).unwrap();
    std::fs::write(dir.path().join(".meshvault").join("index"), b"x").unwrap();

    let want = RelPath::parse("sub/a.txt").unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut seen = Vec::new();
    while Instant::now() < deadline {
        seen.extend(src.drain());
        if seen.contains(&WatchEvent::Changed(want.clone())) || seen.contains(&WatchEvent::Overflow)
        {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(
        seen.contains(&WatchEvent::Changed(want)) || seen.contains(&WatchEvent::Overflow),
        "{seen:?}"
    );
    assert!(
        !seen
            .iter()
            .any(|e| matches!(e, WatchEvent::Changed(p) if p.as_str().contains("meshvault"))),
        "metadata directory must not be reported: {seen:?}"
    );
}
