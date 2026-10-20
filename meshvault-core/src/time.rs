//! Time as a dependency: core code never reads the clock directly.

use std::sync::atomic::{AtomicI64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Nanoseconds since the Unix epoch, UTC.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Timestamp(pub i64);

/// A source of the current time.
pub trait Clock: Send + Sync {
    /// The current time.
    fn now(&self) -> Timestamp;
}

/// The wall clock.
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Timestamp {
        let nanos = match SystemTime::now().duration_since(UNIX_EPOCH) {
            Ok(d) => i64::try_from(d.as_nanos()).unwrap_or(i64::MAX),
            Err(e) => i64::try_from(e.duration().as_nanos()).map_or(i64::MIN, |n| -n),
        };
        Timestamp(nanos)
    }
}

/// A clock tests control by hand.
pub struct FakeClock(AtomicI64);

impl FakeClock {
    /// Starts at `start`.
    pub fn new(start: Timestamp) -> Self {
        FakeClock(AtomicI64::new(start.0))
    }

    /// Sets the time.
    pub fn set(&self, t: Timestamp) {
        self.0.store(t.0, Ordering::SeqCst);
    }

    /// Moves the time forward.
    pub fn advance(&self, d: Duration) {
        self.0.fetch_add(nanos(d), Ordering::SeqCst);
    }

    /// Moves the time backward, as a system clock change would.
    pub fn jump_back(&self, d: Duration) {
        self.0.fetch_sub(nanos(d), Ordering::SeqCst);
    }
}

fn nanos(d: Duration) -> i64 {
    i64::try_from(d.as_nanos()).unwrap_or(i64::MAX)
}

impl Clock for FakeClock {
    fn now(&self) -> Timestamp {
        Timestamp(self.0.load(Ordering::SeqCst))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fake_clock_advance() {
        let c = FakeClock::new(Timestamp(1_000));
        c.advance(Duration::from_nanos(500));
        assert_eq!(c.now(), Timestamp(1_500));
        c.set(Timestamp(7));
        assert_eq!(c.now(), Timestamp(7));
    }

    #[test]
    fn fake_clock_jump_back() {
        let c = FakeClock::new(Timestamp(1_000));
        c.jump_back(Duration::from_nanos(400));
        assert_eq!(c.now(), Timestamp(600));
    }

    #[test]
    fn system_clock_monotonic_enough() {
        let a = SystemClock.now();
        let b = SystemClock.now();
        assert!(a.0 > 1_600_000_000_000_000_000);
        assert!(b.0 >= a.0 - 1_000_000_000);
    }
}
