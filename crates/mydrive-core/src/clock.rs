//! The injected clock every engine timer uses, with a manual clock for tests.
//!
//! Every timer in the engine (quiet periods, polling, back-off, `Retry-After` holds, expiry)
//! reads time through [`Clock`], so tests can move time explicitly with [`ManualClock`]
//! instead of sleeping (definition of done: "no dependence on wall-clock time").
//!
//! The clock has two independent readings:
//!
//! - [`Clock::now`], a monotonic [`MonotonicInstant`] for measuring intervals and setting
//!   deadlines. It never goes backwards.
//! - [`Clock::wall_now`], wall-clock time in whole seconds since the Unix epoch, for values
//!   stored on disk or compared with server times (for example an HTTP-date `Retry-After`).
//!   It may jump in either direction, as a real system clock can.
//!
//! This module does no I/O, reads no system time and uses no async runtime: waiting is a
//! plain [`Future`]. The real clock, on the engine's async runtime, lives in the engine.

use std::collections::BTreeMap;
use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::task::{Context, Poll, Waker};
use std::time::Duration;

/// A point on a monotonic clock, held as the time elapsed since that clock's origin.
///
/// The origin is arbitrary and differs between clocks: instants from different clocks must
/// not be compared. [`ManualClock`] starts at [`MonotonicInstant::ZERO`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MonotonicInstant(Duration);

impl MonotonicInstant {
    /// The clock's origin.
    pub const ZERO: Self = Self(Duration::ZERO);

    /// The instant `elapsed` after the clock's origin.
    pub const fn from_origin(elapsed: Duration) -> Self {
        Self(elapsed)
    }

    /// The time elapsed between the clock's origin and this instant.
    pub const fn since_origin(self) -> Duration {
        self.0
    }

    /// The instant `duration` later, or `None` if that cannot be represented.
    pub fn checked_add(self, duration: Duration) -> Option<Self> {
        self.0.checked_add(duration).map(Self)
    }

    /// The instant `duration` later, or the latest representable instant on overflow.
    ///
    /// A deadline that saturates is in effect "never", which is the safe reading of a
    /// timeout too large to represent.
    pub fn saturating_add(self, duration: Duration) -> Self {
        Self(self.0.saturating_add(duration))
    }

    /// The time from `earlier` to this instant, or zero if `earlier` is later.
    pub fn saturating_duration_since(self, earlier: Self) -> Duration {
        self.0.saturating_sub(earlier.0)
    }
}

/// The future returned by [`Clock::sleep_until`]. It completes once the clock's monotonic
/// time has reached the deadline.
pub type Sleep = Pin<Box<dyn Future<Output = ()> + Send + 'static>>;

/// A source of time, injected into everything that sets a timer.
///
/// Implementations are shared between tasks and threads (`Arc<dyn Clock>`), hence
/// `Send + Sync`.
pub trait Clock: Send + Sync {
    /// The current monotonic time.
    fn now(&self) -> MonotonicInstant;

    /// The current wall-clock time, in whole seconds since the Unix epoch (UTC).
    fn wall_now(&self) -> i64;

    /// A future that completes when [`Clock::now`] has reached or passed `deadline`.
    ///
    /// A deadline that has already passed gives a future that completes on its first poll.
    /// Dropping the future before it completes cancels the wait.
    fn sleep_until(&self, deadline: MonotonicInstant) -> Sleep;
}

/// A clock that moves only when told to, for deterministic tests.
///
/// Cloning gives another handle to the same clock, so a test can keep one handle and give
/// the code under test another (or an `Arc<dyn Clock>`). It is `Send + Sync` and needs no
/// async runtime: [`ManualClock::advance`] wakes the waiters it releases through their
/// [`Waker`]s, whatever executor polls them.
///
/// Monotonic time starts at [`MonotonicInstant::ZERO`]; wall-clock time starts at the value
/// given to [`ManualClock::new`]. The two move independently.
#[derive(Debug, Clone)]
pub struct ManualClock {
    shared: Arc<Mutex<State>>,
}

/// A waiter's key: its deadline, then its registration order, so that waiters with the same
/// deadline are woken in the order they were created.
type WaiterKey = (MonotonicInstant, u64);

#[derive(Debug)]
struct State {
    now: MonotonicInstant,
    wall: i64,
    next_id: u64,
    /// Waiters that have been polled, are still before their deadline, and are still alive.
    /// An entry is removed when it is woken or when its future is dropped, so this never
    /// holds a waiter that can no longer be woken usefully.
    waiters: BTreeMap<WaiterKey, Waker>,
}

impl ManualClock {
    /// A clock at monotonic time zero and wall-clock time `wall_now` (seconds since the Unix
    /// epoch).
    pub fn new(wall_now: i64) -> Self {
        Self {
            shared: Arc::new(Mutex::new(State {
                now: MonotonicInstant::ZERO,
                wall: wall_now,
                next_id: 0,
                waiters: BTreeMap::new(),
            })),
        }
    }

    /// Moves monotonic time forward by `by` and wakes, in deadline order, every waiter whose
    /// deadline is now reached or passed. Wall-clock time does not change.
    ///
    /// Time saturates at the latest representable instant rather than overflowing.
    pub fn advance(&self, by: Duration) {
        let released = {
            let mut state = lock(&self.shared);
            state.now = state.now.saturating_add(by);
            let now = state.now;
            let mut released = Vec::new();
            // The map is ordered by deadline, so the released waiters are a prefix of it.
            while let Some(entry) = state.waiters.first_entry() {
                if entry.key().0 > now {
                    break;
                }
                released.push(entry.remove());
            }
            released
        };
        // Woken outside the lock: a waker may poll its future at once, and that poll takes
        // the lock again.
        for waker in released {
            waker.wake();
        }
    }

    /// Sets wall-clock time to `wall_now` seconds since the Unix epoch, forwards or
    /// backwards. Monotonic time and waiters are unaffected.
    pub fn set_wall_now(&self, wall_now: i64) {
        lock(&self.shared).wall = wall_now;
    }

    /// Moves wall-clock time by `seconds` (negative moves it back), saturating at the `i64`
    /// range. Monotonic time and waiters are unaffected.
    pub fn advance_wall(&self, seconds: i64) {
        let mut state = lock(&self.shared);
        state.wall = state.wall.saturating_add(seconds);
    }

    /// How many waiters are registered and not yet released.
    ///
    /// A [`Sleep`] registers on its first poll that finds its deadline in the future, so a
    /// test can wait until the code under test is actually sleeping before it advances the
    /// clock. A waiter leaves the count when it is released or dropped.
    pub fn pending_waiters(&self) -> usize {
        lock(&self.shared).waiters.len()
    }
}

impl Clock for ManualClock {
    fn now(&self) -> MonotonicInstant {
        lock(&self.shared).now
    }

    fn wall_now(&self) -> i64 {
        lock(&self.shared).wall
    }

    fn sleep_until(&self, deadline: MonotonicInstant) -> Sleep {
        let id = {
            let mut state = lock(&self.shared);
            let id = state.next_id;
            state.next_id = state.next_id.wrapping_add(1);
            id
        };
        Box::pin(ManualSleep {
            shared: Arc::clone(&self.shared),
            key: (deadline, id),
        })
    }
}

/// The waiter behind [`ManualClock::sleep_until`].
struct ManualSleep {
    shared: Arc<Mutex<State>>,
    key: WaiterKey,
}

impl fmt::Debug for ManualSleep {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ManualSleep")
            .field("deadline", &self.key.0)
            .finish_non_exhaustive()
    }
}

impl Future for ManualSleep {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        let mut state = lock(&self.shared);
        if state.now >= self.key.0 {
            state.waiters.remove(&self.key);
            return Poll::Ready(());
        }
        // Re-polled with a different waker (the task moved): keep only the latest one.
        match state.waiters.get_mut(&self.key) {
            Some(waker) => waker.clone_from(cx.waker()),
            None => {
                state.waiters.insert(self.key, cx.waker().clone());
            }
        }
        Poll::Pending
    }
}

impl Drop for ManualSleep {
    fn drop(&mut self) {
        // A cancelled wait leaves nothing behind and wakes no one.
        lock(&self.shared).waiters.remove(&self.key);
    }
}

/// Locks the clock state. A panic while the lock was held cannot leave the state half
/// updated (each update is a single assignment or map operation), so a poisoned lock is
/// used as it is rather than turning one panicking test thread into many.
fn lock(shared: &Mutex<State>) -> MutexGuard<'_, State> {
    shared.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::task::Wake;

    /// Counts wake-ups, and records them in a shared log under a label.
    struct Recorder {
        label: usize,
        wakes: AtomicUsize,
        log: Arc<Mutex<Vec<usize>>>,
    }

    impl Wake for Recorder {
        fn wake(self: Arc<Self>) {
            self.wake_by_ref();
        }

        fn wake_by_ref(self: &Arc<Self>) {
            self.wakes.fetch_add(1, Ordering::SeqCst);
            self.log.lock().unwrap().push(self.label);
        }
    }

    fn recorder(label: usize, log: &Arc<Mutex<Vec<usize>>>) -> Arc<Recorder> {
        Arc::new(Recorder {
            label,
            wakes: AtomicUsize::new(0),
            log: Arc::clone(log),
        })
    }

    fn poll_with(sleep: &mut Sleep, waker: &Arc<Recorder>) -> Poll<()> {
        let waker = Waker::from(Arc::clone(waker));
        sleep.as_mut().poll(&mut Context::from_waker(&waker))
    }

    fn at(secs: u64) -> MonotonicInstant {
        MonotonicInstant::from_origin(Duration::from_secs(secs))
    }

    const WALL: i64 = 1_767_225_600;

    /// AC5: a waiter for T completes exactly when `advance` reaches T, and not before.
    #[test]
    fn waiter_completes_exactly_at_its_deadline() {
        let clock = ManualClock::new(WALL);
        let log = Arc::new(Mutex::new(Vec::new()));
        let waker = recorder(1, &log);
        let mut sleep = clock.sleep_until(at(10));

        assert_eq!(poll_with(&mut sleep, &waker), Poll::Pending);
        clock.advance(Duration::from_secs(9));
        clock.advance(Duration::from_millis(999));
        assert_eq!(waker.wakes.load(Ordering::SeqCst), 0, "woken before T");
        assert_eq!(poll_with(&mut sleep, &waker), Poll::Pending);

        clock.advance(Duration::from_millis(1));
        assert_eq!(clock.now(), at(10));
        assert_eq!(waker.wakes.load(Ordering::SeqCst), 1, "not woken at T");
        assert_eq!(poll_with(&mut sleep, &waker), Poll::Ready(()));
        assert_eq!(clock.pending_waiters(), 0);
    }

    /// AC5: an advance that passes T, not just reaches it, also completes the waiter.
    #[test]
    fn waiter_completes_when_advance_passes_its_deadline() {
        let clock = ManualClock::new(WALL);
        let log = Arc::new(Mutex::new(Vec::new()));
        let waker = recorder(1, &log);
        let mut sleep = clock.sleep_until(at(10));
        assert_eq!(poll_with(&mut sleep, &waker), Poll::Pending);
        clock.advance(Duration::from_secs(3600));
        assert_eq!(waker.wakes.load(Ordering::SeqCst), 1);
        assert_eq!(poll_with(&mut sleep, &waker), Poll::Ready(()));
    }

    /// A deadline already reached completes on the first poll and registers nothing.
    #[test]
    fn past_deadline_completes_immediately() {
        let clock = ManualClock::new(WALL);
        clock.advance(Duration::from_secs(5));
        let log = Arc::new(Mutex::new(Vec::new()));
        let waker = recorder(1, &log);
        for deadline in [at(0), at(5)] {
            let mut sleep = clock.sleep_until(deadline);
            assert_eq!(poll_with(&mut sleep, &waker), Poll::Ready(()));
        }
        assert_eq!(clock.pending_waiters(), 0);
    }

    /// AC5: two waiters complete in deadline order, whatever order they were created in, and
    /// a single advance past both wakes them earlier deadline first.
    #[test]
    fn waiters_complete_in_deadline_order() {
        let clock = ManualClock::new(WALL);
        let log = Arc::new(Mutex::new(Vec::new()));
        let late_waker = recorder(20, &log);
        let early_waker = recorder(10, &log);
        let mut late = clock.sleep_until(at(20));
        let mut early = clock.sleep_until(at(10));
        assert_eq!(poll_with(&mut late, &late_waker), Poll::Pending);
        assert_eq!(poll_with(&mut early, &early_waker), Poll::Pending);

        clock.advance(Duration::from_secs(10));
        assert_eq!(poll_with(&mut early, &early_waker), Poll::Ready(()));
        assert_eq!(poll_with(&mut late, &late_waker), Poll::Pending);
        clock.advance(Duration::from_secs(10));
        assert_eq!(poll_with(&mut late, &late_waker), Poll::Ready(()));
        assert_eq!(*log.lock().unwrap(), vec![10, 20]);

        // Both released by one advance: still woken in deadline order.
        log.lock().unwrap().clear();
        let mut late = clock.sleep_until(at(40));
        let mut early = clock.sleep_until(at(30));
        assert_eq!(poll_with(&mut late, &late_waker), Poll::Pending);
        assert_eq!(poll_with(&mut early, &early_waker), Poll::Pending);
        clock.advance(Duration::from_secs(100));
        assert_eq!(*log.lock().unwrap(), vec![10, 20]);
    }

    /// Waiters with the same deadline are woken in the order they were created.
    #[test]
    fn equal_deadlines_wake_in_creation_order() {
        let clock = ManualClock::new(WALL);
        let log = Arc::new(Mutex::new(Vec::new()));
        let wakers: Vec<_> = (0..3).map(|i| recorder(i, &log)).collect();
        let mut sleeps: Vec<_> = (0..3).map(|_| clock.sleep_until(at(1))).collect();
        for i in [2, 0, 1] {
            assert_eq!(poll_with(&mut sleeps[i], &wakers[i]), Poll::Pending);
        }
        clock.advance(Duration::from_secs(1));
        assert_eq!(*log.lock().unwrap(), vec![0, 1, 2]);
    }

    /// AC5: advancing wall-clock time does not move monotonic time or wake waiters, and
    /// advancing monotonic time does not move wall-clock time.
    #[test]
    fn wall_and_monotonic_time_are_independent() {
        let clock = ManualClock::new(WALL);
        let log = Arc::new(Mutex::new(Vec::new()));
        let waker = recorder(1, &log);
        let mut sleep = clock.sleep_until(at(10));
        assert_eq!(poll_with(&mut sleep, &waker), Poll::Pending);

        clock.advance_wall(86_400);
        clock.set_wall_now(WALL + 1_000_000);
        assert_eq!(clock.wall_now(), WALL + 1_000_000);
        assert_eq!(clock.now(), MonotonicInstant::ZERO);
        assert_eq!(waker.wakes.load(Ordering::SeqCst), 0);
        assert_eq!(poll_with(&mut sleep, &waker), Poll::Pending);

        // Wall time may go backwards; monotonic time is still untouched.
        clock.advance_wall(-2_000_000);
        assert_eq!(clock.wall_now(), WALL - 1_000_000);
        assert_eq!(clock.now(), MonotonicInstant::ZERO);

        clock.advance(Duration::from_secs(10));
        assert_eq!(clock.wall_now(), WALL - 1_000_000);
        assert_eq!(poll_with(&mut sleep, &waker), Poll::Ready(()));
    }

    /// A waiter dropped before its deadline is removed, and a later advance neither wakes it
    /// nor disturbs the other waiters.
    #[test]
    fn dropped_waiter_does_not_leak_or_wake() {
        let clock = ManualClock::new(WALL);
        let log = Arc::new(Mutex::new(Vec::new()));
        let dropped_waker = recorder(1, &log);
        let kept_waker = recorder(2, &log);
        let mut dropped = clock.sleep_until(at(5));
        let mut kept = clock.sleep_until(at(10));
        assert_eq!(poll_with(&mut dropped, &dropped_waker), Poll::Pending);
        assert_eq!(poll_with(&mut kept, &kept_waker), Poll::Pending);
        assert_eq!(clock.pending_waiters(), 2);

        drop(dropped);
        assert_eq!(clock.pending_waiters(), 1);
        clock.advance(Duration::from_secs(5));
        assert_eq!(dropped_waker.wakes.load(Ordering::SeqCst), 0);
        assert_eq!(kept_waker.wakes.load(Ordering::SeqCst), 0);
        assert_eq!(poll_with(&mut kept, &kept_waker), Poll::Pending);

        clock.advance(Duration::from_secs(5));
        assert_eq!(*log.lock().unwrap(), vec![2]);
        assert_eq!(poll_with(&mut kept, &kept_waker), Poll::Ready(()));
        assert_eq!(clock.pending_waiters(), 0);
        // Dropping a completed waiter is harmless.
        drop(kept);
        assert_eq!(clock.pending_waiters(), 0);
    }

    /// A waiter re-polled with a new waker is woken through the new one only, once.
    #[test]
    fn repoll_replaces_the_waker() {
        let clock = ManualClock::new(WALL);
        let log = Arc::new(Mutex::new(Vec::new()));
        let first = recorder(1, &log);
        let second = recorder(2, &log);
        let mut sleep = clock.sleep_until(at(1));
        assert_eq!(poll_with(&mut sleep, &first), Poll::Pending);
        assert_eq!(poll_with(&mut sleep, &second), Poll::Pending);
        assert_eq!(clock.pending_waiters(), 1);
        clock.advance(Duration::from_secs(1));
        assert_eq!(*log.lock().unwrap(), vec![2]);
    }

    /// The clock is shared across threads: a thread blocked on a sleep is released by an
    /// advance from another thread, with no async runtime involved.
    #[test]
    fn usable_across_threads() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<ManualClock>();
        fn assert_send<T: Send>() {}
        assert_send::<Sleep>();

        struct ThreadWaker(std::thread::Thread);
        impl Wake for ThreadWaker {
            fn wake(self: Arc<Self>) {
                self.0.unpark();
            }
        }

        let clock = ManualClock::new(WALL);
        let shared: Arc<dyn Clock> = Arc::new(clock.clone());
        let (done, finished) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let waker = Waker::from(Arc::new(ThreadWaker(std::thread::current())));
            let mut cx = Context::from_waker(&waker);
            let mut sleep = shared.sleep_until(at(60));
            while sleep.as_mut().poll(&mut cx).is_pending() {
                std::thread::park();
            }
            done.send(shared.now()).unwrap();
        });
        // Advance only once the other thread is actually waiting, so the test does not
        // depend on timing.
        while clock.pending_waiters() == 0 {
            std::thread::yield_now();
        }
        clock.advance(Duration::from_secs(60));
        // The timeout only turns a lost wake-up into a failure instead of a hang; a correct
        // clock releases the thread at once.
        let woke_at = finished.recv_timeout(Duration::from_secs(30));
        assert_eq!(woke_at, Ok(at(60)), "the waiting thread was not woken");
    }

    /// Instant arithmetic saturates instead of overflowing.
    #[test]
    fn instant_arithmetic() {
        let max = MonotonicInstant::from_origin(Duration::MAX);
        assert_eq!(max.checked_add(Duration::from_nanos(1)), None);
        assert_eq!(max.saturating_add(Duration::from_secs(1)), max);
        assert_eq!(at(5).saturating_duration_since(at(7)), Duration::ZERO);
        assert_eq!(
            at(7).saturating_duration_since(at(5)),
            Duration::from_secs(2)
        );
        assert_eq!(at(3).checked_add(Duration::from_secs(4)), Some(at(7)));
        assert_eq!(at(7).since_origin(), Duration::from_secs(7));

        let clock = ManualClock::new(WALL);
        clock.advance(Duration::MAX);
        clock.advance(Duration::from_secs(1));
        assert_eq!(clock.now(), max);
        clock.set_wall_now(i64::MAX);
        clock.advance_wall(1);
        assert_eq!(clock.wall_now(), i64::MAX);
    }

    /// AC6: the clock does no I/O and never reads the system time. The forbidden names are
    /// split so that this test does not match itself.
    #[test]
    fn source_reads_no_system_time_and_does_no_io() {
        let source = include_str!("clock.rs");
        for forbidden in [
            concat!("System", "Time"),
            concat!("Instant", "::now"),
            concat!("std::", "fs"),
            concat!("std::", "io"),
            concat!("std::", "net"),
            concat!("std::", "env"),
            concat!("std::", "process"),
            concat!("tok", "io"),
        ] {
            assert!(!source.contains(forbidden), "clock.rs uses {forbidden}");
        }
    }
}
