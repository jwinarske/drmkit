// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! Parity port of `tests/unit/test_page_flip.cpp` and `test_mode.cpp` from
//! drm-cxx @ `dc2915b`.
//!
//! The two `PageFlipEintr` cases pin **invariant 3** and are the reason this
//! crate carries a `libc` dev-dependency: forcing a real `EINTR` out of
//! `epoll_wait` needs `sigaction` without `SA_RESTART` plus `setitimer`, and
//! rustix exposes neither in a stable safe API. Nothing in the library depends
//! on `libc`.

use super::*;
use std::os::fd::{AsRawFd as _, OwnedFd};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use rustix::event::{EventfdFlags, eventfd};

// --- SIGALRM storm harness ---------------------------------------------------

/// Counts delivered SIGALRMs, so a test can assert the storm actually fired.
/// A storm that never fires would leave the `EINTR` path untested while the
/// test still passed — the failure mode the C++ guards against with the same
/// assertion.
static ALRM_COUNT: AtomicU32 = AtomicU32::new(0);

/// Only one storm at a time: signal disposition and the interval timer are
/// process-wide, so two concurrent storms would restore each other's state.
static STORM_LOCK: Mutex<()> = Mutex::new(());

extern "C" fn on_sigalrm(_sig: libc::c_int) {
    ALRM_COUNT.fetch_add(1, Ordering::Relaxed);
}

/// Delivers a SIGALRM storm **to one specific thread**, so a wait on that
/// thread is genuinely interrupted.
///
/// The C++ uses `setitimer(ITIMER_REAL)`, which posts the signal to the
/// process and lets the kernel pick any thread not blocking it. Under Rust's
/// test harness that is usually the wrong thread: the other cases' threads
/// absorb the signal and the wait under test is never interrupted. An earlier
/// version of this harness did exactly that, and both invariant cases passed
/// against a deliberately broken `dispatch` that propagated `EINTR` instead of
/// retrying — they were asserting on a storm that never reached them.
///
/// `pthread_kill` against the captured thread removes the ambiguity. The
/// handler is installed with `SA_RESTART` cleared so interrupted syscalls
/// return `EINTR` rather than being restarted.
struct SigalrmStorm {
    _guard: std::sync::MutexGuard<'static, ()>,
    old_action: libc::sigaction,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    sender: Option<std::thread::JoinHandle<()>>,
}

impl SigalrmStorm {
    /// Start storming the calling thread. Must be called from the thread that
    /// will perform the interruptible wait.
    fn targeting_current_thread() -> Self {
        let guard = STORM_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        ALRM_COUNT.store(0, Ordering::Relaxed);

        // SAFETY: `sigaction` is called with a correctly initialized structure
        // and the previous disposition is captured for restoration in `Drop`.
        // The handler only touches an atomic, which is async-signal-safe.
        let old_action = unsafe {
            let mut action: libc::sigaction = std::mem::zeroed();
            action.sa_sigaction = on_sigalrm as *const () as usize;
            libc::sigemptyset(&raw mut action.sa_mask);
            action.sa_flags = 0; // no SA_RESTART: syscalls return EINTR

            let mut old: libc::sigaction = std::mem::zeroed();
            libc::sigaction(libc::SIGALRM, &raw const action, &raw mut old);
            old
        };

        // `pthread_t` is an integer on Linux, so it crosses the thread boundary
        // by value.
        // SAFETY: reading this thread's own identifier.
        let target = unsafe { libc::pthread_self() };
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));

        let sender = {
            let stop = std::sync::Arc::clone(&stop);
            std::thread::spawn(move || {
                // Block SIGALRM here so this thread never consumes its own
                // deliveries.
                // SAFETY: operating on this thread's own signal mask.
                unsafe {
                    let mut set: libc::sigset_t = std::mem::zeroed();
                    libc::sigemptyset(&raw mut set);
                    libc::sigaddset(&raw mut set, libc::SIGALRM);
                    libc::pthread_sigmask(libc::SIG_BLOCK, &raw const set, std::ptr::null_mut());
                }
                while !stop.load(Ordering::Relaxed) {
                    // SAFETY: `target` is a live thread for the whole storm --
                    // the storm is dropped on that thread, which joins this one
                    // before returning.
                    unsafe {
                        libc::pthread_kill(target, libc::SIGALRM);
                    }
                    std::thread::sleep(Duration::from_micros(500));
                }
            })
        };

        Self {
            _guard: guard,
            old_action,
            stop,
            sender: Some(sender),
        }
    }

    /// Deliveries counted since this storm started.
    ///
    /// Reads a process-global counter, but stays a method: it is only
    /// meaningful while a storm is alive, and `&self` is what expresses that.
    #[expect(
        clippy::unused_self,
        reason = "the borrow ties the reading to a live storm; the counter is \
                  process-global because a signal handler cannot capture state"
    )]
    fn fired(&self) -> u32 {
        ALRM_COUNT.load(Ordering::Relaxed)
    }
}

impl Drop for SigalrmStorm {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(sender) = self.sender.take() {
            let _ = sender.join();
        }
        // SAFETY: restoring the disposition captured in the constructor.
        unsafe {
            libc::sigaction(
                libc::SIGALRM,
                &raw const self.old_action,
                std::ptr::null_mut(),
            );
        }
    }
}

fn make_eventfd() -> OwnedFd {
    eventfd(0, EventfdFlags::CLOEXEC | EventfdFlags::NONBLOCK).expect("eventfd")
}

fn signal_eventfd(fd: &OwnedFd) {
    let one = 1u64.to_ne_bytes();
    rustix::io::write(fd, &one).expect("write eventfd");
}

// --- test_page_flip.cpp: foreign sources -------------------------------------

/// `PageFlipForeignSource.AddSourceRejectsNegativeFd`
#[test]
fn add_source_rejects_negative_fd() {
    let mut pf = PageFlip::without_device().expect("epoll");
    assert!(matches!(
        pf.add_source(-1, Box::new(|| {})),
        Err(ModesetError::InvalidSource)
    ));
}

/// `PageFlipForeignSource.AddSourceRejectsDuplicateFd`
#[test]
fn add_source_rejects_duplicate_fd() {
    let efd = make_eventfd();
    let raw = efd.as_raw_fd();

    let mut pf = PageFlip::without_device().expect("epoll");
    pf.add_source(raw, Box::new(|| {})).expect("first add");
    assert!(matches!(
        pf.add_source(raw, Box::new(|| {})),
        Err(ModesetError::InvalidSource)
    ));

    pf.remove_source(raw);
}

/// `PageFlipForeignSource.RemoveSourceIsIdempotent`
#[test]
fn remove_source_is_idempotent() {
    let efd = make_eventfd();
    let raw = efd.as_raw_fd();

    let mut pf = PageFlip::without_device().expect("epoll");
    pf.remove_source(raw); // never added
    pf.add_source(raw, Box::new(|| {})).expect("add");
    pf.remove_source(raw);
    pf.remove_source(raw); // twice
}

/// `PageFlipForeignSource.EventfdReadinessFiresCallback`
#[test]
fn eventfd_readiness_fires_callback() {
    let efd = make_eventfd();
    let raw = efd.as_raw_fd();
    let calls = std::sync::Arc::new(AtomicU32::new(0));

    let mut pf = PageFlip::without_device().expect("epoll");
    {
        let counter = std::sync::Arc::clone(&calls);
        let drain = efd.as_raw_fd();
        pf.add_source(
            raw,
            Box::new(move || {
                // The dispatcher never reads the descriptor; the callback must
                // drain it or the next dispatch fires again.
                let mut buf = [0u8; 8];
                // SAFETY: `drain` is the eventfd registered above, kept alive
                // by `efd` for the whole test.
                let borrowed = unsafe { std::os::fd::BorrowedFd::borrow_raw(drain) };
                let _ = rustix::io::read(borrowed, &mut buf);
                counter.fetch_add(1, Ordering::Relaxed);
            }),
        )
        .expect("add");
    }

    signal_eventfd(&efd);
    pf.dispatch(Timeout::Bounded(Duration::from_secs(1)))
        .expect("dispatch");
    assert_eq!(calls.load(Ordering::Relaxed), 1);

    // Drained, so a second dispatch finds nothing.
    assert!(matches!(
        pf.dispatch(Timeout::Poll),
        Err(ModesetError::TimedOut)
    ));

    pf.remove_source(raw);
}

/// `PageFlipForeignSource.DispatchTimesOutWithNoReadyFds`
#[test]
fn dispatch_times_out_with_no_ready_fds() {
    let efd = make_eventfd();
    let raw = efd.as_raw_fd();

    let mut pf = PageFlip::without_device().expect("epoll");
    pf.add_source(raw, Box::new(|| {})).expect("add");

    assert!(matches!(
        pf.dispatch(Timeout::Bounded(Duration::from_millis(10))),
        Err(ModesetError::TimedOut)
    ));

    pf.remove_source(raw);
}

/// A dispatcher with no device and no sources must say so rather than block
/// forever on an empty epoll.
#[test]
fn dispatch_with_nothing_registered_is_an_error() {
    let mut pf = PageFlip::without_device().expect("epoll");
    assert!(matches!(
        pf.dispatch(Timeout::Infinite),
        Err(ModesetError::NothingToWaitOn)
    ));
}

/// `PageFlipForeignSource.MoveTransfersDispatcher`
#[test]
fn move_transfers_the_dispatcher() {
    let efd = make_eventfd();
    let raw = efd.as_raw_fd();
    let calls = std::sync::Arc::new(AtomicU32::new(0));

    let mut pf = PageFlip::without_device().expect("epoll");
    let counter = std::sync::Arc::clone(&calls);
    let drain = efd.as_raw_fd();
    pf.add_source(
        raw,
        Box::new(move || {
            let mut buf = [0u8; 8];
            // SAFETY: as above.
            let borrowed = unsafe { std::os::fd::BorrowedFd::borrow_raw(drain) };
            let _ = rustix::io::read(borrowed, &mut buf);
            counter.fetch_add(1, Ordering::Relaxed);
        }),
    )
    .expect("add");

    let mut moved = pf; // move
    signal_eventfd(&efd);
    moved
        .dispatch(Timeout::Bounded(Duration::from_secs(1)))
        .expect("dispatch after move");
    assert_eq!(calls.load(Ordering::Relaxed), 1);

    moved.remove_source(raw);
}

// --- invariant 3 -------------------------------------------------------------

/// `PageFlipEintr.DispatchInfiniteRetriesUntilReady`
///
/// **Invariant 3.** An infinite wait under a signal storm must swallow every
/// `EINTR` and return success once the descriptor is readable — never an
/// interrupted error.
#[test]
fn dispatch_infinite_retries_until_ready() {
    let efd = make_eventfd();
    let raw = efd.as_raw_fd();
    let calls = std::sync::Arc::new(AtomicU32::new(0));

    let mut pf = PageFlip::without_device().expect("epoll");
    let counter = std::sync::Arc::clone(&calls);
    let drain = efd.as_raw_fd();
    pf.add_source(
        raw,
        Box::new(move || {
            let mut buf = [0u8; 8];
            // SAFETY: as above.
            let borrowed = unsafe { std::os::fd::BorrowedFd::borrow_raw(drain) };
            let _ = rustix::io::read(borrowed, &mut buf);
            counter.fetch_add(1, Ordering::Relaxed);
        }),
    )
    .expect("add");

    // The storm must be running *before* the waker starts watching its
    // counter. Spawned first, the waker could observe the other storm test's
    // count, arm the descriptor immediately, and leave this test's storm at
    // zero -- which is exactly how this test failed under parallel execution.
    let storm = SigalrmStorm::targeting_current_thread();

    // Make the descriptor readable only after the storm has interrupted the
    // wait several times. The C++ arms this from inside its signal handler;
    // doing it from a helper thread keeps the handler async-signal-safe.
    let signal_fd = efd.as_raw_fd();
    let waker = std::thread::spawn(move || {
        // Block SIGALRM here so the kernel delivers it to the thread parked in
        // epoll_wait. Without this the signal can land on this spinning thread
        // instead, leaving the wait uninterrupted -- the assertion below would
        // then pass while the EINTR path went unexercised.
        // SAFETY: operating on this thread's own signal mask with a correctly
        // initialized sigset.
        unsafe {
            let mut set: libc::sigset_t = std::mem::zeroed();
            libc::sigemptyset(&raw mut set);
            libc::sigaddset(&raw mut set, libc::SIGALRM);
            libc::pthread_sigmask(libc::SIG_BLOCK, &raw const set, std::ptr::null_mut());
        }

        // Bounded, not an open-ended spin. If `dispatch` returns early --
        // which is exactly what a broken implementation does -- the storm is
        // torn down and the counter stops advancing. An unbounded wait here
        // would then deadlock, turning a test failure into a CI hang.
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while ALRM_COUNT.load(Ordering::Relaxed) < 8 && std::time::Instant::now() < deadline {
            std::thread::yield_now();
        }
        // SAFETY: the main thread owns `efd` and joins this thread before
        // dropping it, so the descriptor is alive for this write.
        let borrowed = unsafe { std::os::fd::BorrowedFd::borrow_raw(signal_fd) };
        let one = 1u64.to_ne_bytes();
        let _ = rustix::io::write(borrowed, &one);
    });

    let result = pf.dispatch(Timeout::Infinite);
    let fired = storm.fired();
    drop(storm);
    waker.join().expect("waker thread");

    result.expect("an infinite wait must swallow EINTR, never report it");
    assert_eq!(calls.load(Ordering::Relaxed), 1);
    assert!(
        fired >= 8,
        "storm fired only {fired} times -- the EINTR path went untested"
    );

    pf.remove_source(raw);
}

/// `PageFlipEintr.DispatchBoundedTimeoutHonorsBudgetUnderStorm`
///
/// **Invariant 3.** A bounded wait must recompute its remaining budget across
/// `EINTR` retries. Without that, a 1 ms signal cadence restarts a 50 ms wait
/// forever and the call never returns.
#[test]
fn dispatch_bounded_timeout_honors_budget_under_storm() {
    const BUDGET: Duration = Duration::from_millis(50);
    const WATCHDOG: Duration = Duration::from_secs(5);

    let (tx, rx) = std::sync::mpsc::channel();

    // The storm targets whichever thread calls `targeting_current_thread`, so
    // it has to run on the thread performing the wait. Putting that on its own
    // thread also lets the watchdog below bound how long a regression takes to
    // report.
    std::thread::spawn(move || {
        let efd = make_eventfd(); // never made readable
        let raw = efd.as_raw_fd();
        let mut pf = PageFlip::without_device().expect("epoll");
        pf.add_source(raw, Box::new(|| {})).expect("add");

        let clock = drmkit_time::default_clock();
        let storm = SigalrmStorm::targeting_current_thread();
        let start = drmkit_time::Clock::now(clock);
        let result = pf.dispatch(Timeout::Bounded(BUDGET));
        let elapsed = drmkit_time::Clock::now(clock).saturating_duration_since(start);
        let fired = storm.fired();
        drop(storm);
        pf.remove_source(raw);

        let timed_out = matches!(result, Err(ModesetError::TimedOut));
        let _ = tx.send((timed_out, format!("{result:?}"), elapsed, fired));
    });

    let Ok((timed_out, rendered, elapsed, fired)) = rx.recv_timeout(WATCHDOG) else {
        panic!(
            "dispatch did not return within {WATCHDOG:?} for a {BUDGET:?} budget: \
             the remaining budget was not recomputed across EINTR retries"
        );
    };

    assert!(timed_out, "expected a timeout, got {rendered}");
    assert!(
        elapsed >= Duration::from_millis(45),
        "returned after {elapsed:?}, before the budget elapsed"
    );
    assert!(
        elapsed < Duration::from_millis(500),
        "took {elapsed:?}: the budget was not recomputed across EINTR retries"
    );
    assert!(
        fired > 0,
        "storm never fired -- the EINTR path went untested"
    );
}

// --- test_mode.cpp -----------------------------------------------------------

/// Build a mode with the fields the selectors read.
fn mode(
    width: u16,
    height: u16,
    refresh: u32,
    preferred: bool,
    interlaced: bool,
) -> drm::control::Mode {
    // SAFETY: `drm_mode_modeinfo` is a plain C struct of integers and a
    // fixed-size name buffer; all-zero is a valid, if empty, mode.
    let mut raw: drm_ffi::drm_mode_modeinfo = unsafe { std::mem::zeroed() };
    raw.hdisplay = width;
    raw.vdisplay = height;
    raw.vrefresh = refresh;
    raw.clock = refresh * u32::from(width) * u32::from(height) / 1000;
    if preferred {
        raw.type_ |= drm_ffi::DRM_MODE_TYPE_PREFERRED;
    }
    if interlaced {
        raw.flags |= drm_ffi::DRM_MODE_FLAG_INTERLACE;
    }
    drm::control::Mode::from(raw)
}

#[test]
fn mode_accessors_read_the_right_fields() {
    let m = mode(1920, 1080, 60, true, false);
    assert_eq!(m.width(), 1920);
    assert_eq!(m.height(), 1080);
    assert_eq!(m.refresh(), 60);
    assert!(m.preferred());
    assert!(!m.interlaced());
    assert_eq!(m.area(), 1920 * 1080);
    assert!(m.clock_khz() > 0);

    let i = mode(1920, 1080, 30, false, true);
    assert!(i.interlaced());
    assert!(!i.preferred());
}

#[test]
fn select_preferred_mode_prefers_the_flagged_one() {
    let modes = [
        mode(3840, 2160, 30, false, false), // larger, but not preferred
        mode(1920, 1080, 60, true, false),
        mode(1280, 720, 60, false, false),
    ];
    let chosen = select_preferred_mode(&modes).expect("a mode");
    assert_eq!((chosen.width(), chosen.height()), (1920, 1080));
}

#[test]
fn select_preferred_mode_falls_back_to_largest_then_refresh() {
    let modes = [
        mode(1280, 720, 60, false, false),
        mode(1920, 1080, 30, false, false),
        mode(1920, 1080, 60, false, false), // same area, higher refresh
    ];
    let chosen = select_preferred_mode(&modes).expect("a mode");
    assert_eq!((chosen.width(), chosen.height()), (1920, 1080));
    assert_eq!(chosen.refresh(), 60);
}

#[test]
fn select_preferred_mode_rejects_an_empty_list() {
    assert!(matches!(
        select_preferred_mode(&[]),
        Err(ModesetError::NoModes)
    ));
}

#[test]
fn select_mode_finds_an_exact_match() {
    let modes = [
        mode(1280, 720, 60, false, false),
        mode(1920, 1080, 60, false, false),
        mode(3840, 2160, 60, false, false),
    ];
    let chosen = select_mode(&modes, 1920, 1080, 0).expect("a mode");
    assert_eq!((chosen.width(), chosen.height()), (1920, 1080));
}

#[test]
fn select_mode_falls_back_to_the_closest_resolution() {
    let modes = [
        mode(1280, 720, 60, false, false),
        mode(3840, 2160, 60, false, false),
    ];
    let chosen = select_mode(&modes, 1920, 1080, 0).expect("a mode");
    assert_eq!(
        (chosen.width(), chosen.height()),
        (1280, 720),
        "1280x720 is closer to 1920x1080 than 3840x2160 is"
    );
}

#[test]
fn select_mode_weights_the_refresh_target() {
    let modes = [
        mode(1920, 1080, 30, false, false),
        mode(1920, 1080, 60, false, false),
    ];
    assert_eq!(select_mode(&modes, 1920, 1080, 30).unwrap().refresh(), 30);
    assert_eq!(select_mode(&modes, 1920, 1080, 60).unwrap().refresh(), 60);
    // With no refresh target, an exact-resolution tie breaks on higher refresh.
    assert_eq!(select_mode(&modes, 1920, 1080, 0).unwrap().refresh(), 60);
}

#[test]
fn select_mode_skips_interlaced_modes() {
    let modes = [
        mode(1920, 1080, 60, false, true), // exact match, but interlaced
        mode(1280, 720, 60, false, false),
    ];
    let chosen = select_mode(&modes, 1920, 1080, 0).expect("a mode");
    assert_eq!(
        (chosen.width(), chosen.height()),
        (1280, 720),
        "an interlaced exact match must lose to a progressive approximation"
    );

    // Only interlaced modes available: nothing usable.
    let only_interlaced = [mode(1920, 1080, 60, false, true)];
    assert!(matches!(
        select_mode(&only_interlaced, 1920, 1080, 0),
        Err(ModesetError::NoModes)
    ));
}

#[test]
fn select_mode_rejects_an_empty_list() {
    assert!(matches!(
        select_mode(&[], 1920, 1080, 0),
        Err(ModesetError::NoModes)
    ));
}

// --- invariant 3, deterministically -----------------------------------------

/// The budget-recomputation half of invariant 3, without signals.
///
/// A bad budget makes `dispatch` **hang** rather than answer wrongly, and no
/// signal-driven test reports a hang reliably: the wedged thread keeps the
/// process alive, and even `process::exit` blocks behind locks held by a thread
/// under a signal storm. Both were tried. Pinning the pure function instead
/// turns that regression into a fast, deterministic failure, and leaves the
/// storm case above to supply end-to-end evidence rather than to be the guard.
mod remaining_budget_tests {
    use super::{Duration, Timeout};
    use crate::page_flip::remaining_budget;
    use drmkit_time::Timestamp;

    const T0: Timestamp = Timestamp::from_nanos(1_000_000_000);

    #[test]
    fn infinite_stays_infinite() {
        assert_eq!(remaining_budget(Timeout::Infinite, None, T0), None);
        assert_eq!(remaining_budget(Timeout::Infinite, Some(T0), T0), None);
    }

    #[test]
    fn poll_stays_zero() {
        assert_eq!(
            remaining_budget(Timeout::Poll, None, T0),
            Some(Duration::ZERO)
        );
    }

    #[test]
    fn bounded_shrinks_as_the_deadline_approaches() {
        let budget = Duration::from_millis(50);
        let deadline = T0 + budget;

        assert_eq!(
            remaining_budget(Timeout::Bounded(budget), Some(deadline), T0),
            Some(budget),
            "at the start the whole budget is left"
        );
        assert_eq!(
            remaining_budget(
                Timeout::Bounded(budget),
                Some(deadline),
                T0 + Duration::from_millis(20)
            ),
            Some(Duration::from_millis(30)),
            "20ms in, 30ms remains -- it must shrink, not reset"
        );
    }

    /// The clause that prevents the hang: past the deadline a retry becomes a
    /// poll. Returning the full budget here is what lets a signal storm restart
    /// the wait forever.
    #[test]
    fn bounded_saturates_at_zero_past_the_deadline() {
        let budget = Duration::from_millis(50);
        let deadline = T0 + budget;

        assert_eq!(
            remaining_budget(Timeout::Bounded(budget), Some(deadline), deadline),
            Some(Duration::ZERO),
            "exactly at the deadline"
        );
        assert_eq!(
            remaining_budget(
                Timeout::Bounded(budget),
                Some(deadline),
                deadline + Duration::from_secs(10)
            ),
            Some(Duration::ZERO),
            "well past it -- must not wrap to a huge duration"
        );
    }

    /// A simulated storm: every retry must see a budget no larger than the
    /// last, and the sequence must reach zero. A resetting budget loops here
    /// forever in production.
    #[test]
    fn repeated_retries_converge_to_zero() {
        let budget = Duration::from_millis(50);
        let deadline = T0 + budget;

        let mut previous = budget;
        let mut now = T0;
        for step in 1..=200 {
            now += Duration::from_micros(500); // the storm's cadence
            let remaining = remaining_budget(Timeout::Bounded(budget), Some(deadline), now)
                .expect("bounded waits always yield a duration");
            assert!(
                remaining <= previous,
                "retry {step}: budget grew from {previous:?} to {remaining:?}"
            );
            previous = remaining;
        }
        assert_eq!(previous, Duration::ZERO, "the budget must reach zero");
    }

    /// A bounded wait with no deadline ends rather than blocking forever.
    #[test]
    fn bounded_without_a_deadline_is_treated_as_expired() {
        assert_eq!(
            remaining_budget(Timeout::Bounded(Duration::from_secs(1)), None, T0),
            Some(Duration::ZERO)
        );
    }
}
