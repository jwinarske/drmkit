// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! Page-flip event dispatch.
//!
//! Port of `src/modeset/page_flip.{hpp,cpp}`. Home of **invariant 3**.

use std::collections::HashMap;
use std::os::fd::{AsFd as _, BorrowedFd, OwnedFd, RawFd};
use std::time::Duration;

use drm::control::Device as _;
use drmkit_core::Device;
use drmkit_time::{Clock, SteadyClock, Timestamp};
use rustix::event::epoll;

use crate::error::{ModesetError, Result};

/// Maximum events drained from one `epoll_wait`.
const MAX_DISPATCH_EVENTS: usize = 16;

/// Called when a page-flip event completes.
pub type Handler = Box<dyn FnMut(PageFlipEvent) + Send>;

/// Called when a foreign source descriptor becomes readable.
///
/// The dispatcher never reads from the descriptor itself: the callback must
/// drain it (`eventfd_read`, `signalfd_siginfo`, `recvmsg`, …) before
/// returning, or the next dispatch fires the same callback again.
pub type SourceCallback = Box<dyn FnMut() + Send>;

/// A completed page flip.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct PageFlipEvent {
    /// The CRTC whose flip completed.
    pub crtc_id: u32,
    /// The vblank sequence number.
    pub sequence: u64,
    /// When the flip landed, on the monotonic timeline.
    pub timestamp: Timestamp,
}

/// How a dispatch wait is bounded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Timeout {
    /// Block until an event arrives.
    #[default]
    Infinite,
    /// Return immediately if nothing is ready.
    Poll,
    /// Wait at most this long.
    Bounded(Duration),
}

/// Dispatches page-flip events, and optionally foreign event sources, from one
/// epoll instance.
///
/// # Single-threaded contract
///
/// Do not call [`dispatch`](Self::dispatch), [`add_source`](Self::add_source),
/// or [`remove_source`](Self::remove_source) concurrently on the same
/// instance. `&mut self` enforces this in Rust; the C++ can only document it.
pub struct PageFlip {
    epoll: OwnedFd,
    drm_fd: Option<RawFd>,
    handler: Option<Handler>,
    sources: HashMap<RawFd, SourceCallback>,
    clock: &'static SteadyClock,
}

impl core::fmt::Debug for PageFlip {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("PageFlip")
            .field("drm_fd", &self.drm_fd)
            .field("has_handler", &self.handler.is_some())
            .field("sources", &self.sources.len())
            .finish_non_exhaustive()
    }
}

impl PageFlip {
    /// Build a dispatcher watching `device`'s descriptor.
    ///
    /// # Errors
    ///
    /// [`ModesetError::Io`] if the epoll instance cannot be created or the DRM
    /// descriptor cannot be registered.
    pub fn new(device: &Device) -> Result<Self> {
        let mut flip = Self::without_device()?;
        flip.register_drm_fd(device.raw_fd())?;
        Ok(flip)
    }

    /// Build a dispatcher with no DRM descriptor, for folding foreign sources
    /// into one loop before a device exists — or in tests.
    ///
    /// The C++ has no such constructor; its tests fabricate a `Device` holding
    /// fd `-1` to reach the same state. Naming the state is clearer than
    /// smuggling an invalid descriptor through a type that promises a valid
    /// one.
    ///
    /// # Errors
    ///
    /// [`ModesetError::Io`] if the epoll instance cannot be created.
    pub fn without_device() -> Result<Self> {
        Ok(Self {
            epoll: epoll::create(epoll::CreateFlags::CLOEXEC)?,
            drm_fd: None,
            handler: None,
            sources: HashMap::new(),
            clock: drmkit_time::default_clock(),
        })
    }

    /// Install the page-flip handler, replacing any previous one.
    pub fn set_handler(&mut self, handler: Handler) {
        self.handler = Some(handler);
    }

    /// The epoll descriptor this dispatcher waits on.
    ///
    /// Exposed so a host event loop can watch it for readability and drive
    /// `dispatch(Timeout::Poll)` itself instead of blocking. Level-triggered:
    /// readable whenever the DRM descriptor or any source has a pending event,
    /// quiescent once dispatch has drained them.
    ///
    /// The dispatcher retains ownership — do not read, close, or `epoll_ctl`
    /// it directly.
    #[must_use]
    pub fn epoll_fd(&self) -> BorrowedFd<'_> {
        self.epoll.as_fd()
    }

    fn register_drm_fd(&mut self, fd: RawFd) -> Result<()> {
        // SAFETY: `fd` comes from a live `Device`, whose descriptor the caller
        // keeps open for at least as long as this dispatcher; the borrow ends
        // when `epoll::add` returns.
        let borrowed = unsafe { BorrowedFd::borrow_raw(fd) };
        epoll::add(
            &self.epoll,
            borrowed,
            epoll::EventData::new_u64(fd_key(fd)),
            epoll::EventFlags::IN,
        )?;
        self.drm_fd = Some(fd);
        Ok(())
    }

    /// Register a foreign descriptor on this dispatcher.
    ///
    /// Folds external sources — a libcamera completion eventfd, a signalfd for
    /// SIGINT, a udev monitor — into the same loop the page-flip events ride
    /// on, so a caller need not run a parallel epoll alongside dispatch.
    ///
    /// The dispatcher takes **no** ownership: the caller closes `fd`.
    /// [`remove_source`](Self::remove_source) must run, or this dispatcher be
    /// dropped, before the descriptor is closed — otherwise the kernel keeps
    /// firing `EPOLLERR` on the stale registration.
    ///
    /// # Errors
    ///
    /// [`ModesetError::InvalidSource`] for a negative or already-registered
    /// descriptor.
    pub fn add_source(&mut self, fd: RawFd, callback: SourceCallback) -> Result<()> {
        if fd < 0 || self.sources.contains_key(&fd) || self.drm_fd == Some(fd) {
            return Err(ModesetError::InvalidSource);
        }

        // SAFETY: the caller owns `fd` and contracts to keep it open until
        // `remove_source` or drop; the borrow ends when `epoll::add` returns.
        let borrowed = unsafe { BorrowedFd::borrow_raw(fd) };
        epoll::add(
            &self.epoll,
            borrowed,
            epoll::EventData::new_u64(fd_key(fd)),
            epoll::EventFlags::IN,
        )?;

        self.sources.insert(fd, callback);
        Ok(())
    }

    /// Unregister a descriptor. Idempotent: unregistering one that was never
    /// added, or doing it twice, is a no-op.
    pub fn remove_source(&mut self, fd: RawFd) {
        if self.sources.remove(&fd).is_some() {
            // SAFETY: the caller contracts to keep `fd` open until this call
            // returns; the borrow ends here.
            let borrowed = unsafe { BorrowedFd::borrow_raw(fd) };
            // Best-effort: the kernel drops the registration when the
            // descriptor closes anyway, so ENOENT here is not an error.
            let _ = epoll::delete(&self.epoll, borrowed);
        }
    }

    /// Re-bind to a new device after a libseat or VT pause-and-resume.
    ///
    /// The dispatcher caches the DRM descriptor and registers it with its
    /// epoll instance. libseat closes that descriptor on pause and hands back a
    /// fresh one on resume, leaving the registration pointing at a dead — or
    /// kernel-recycled — descriptor. Without a rebind the dispatcher would
    /// either silently miss flip events or fire against a stranger's
    /// descriptor.
    ///
    /// Removing the old registration is best-effort. Adding the new one is
    /// required: on failure the dispatcher is left with no DRM descriptor, so
    /// later dispatches report [`ModesetError::NothingToWaitOn`] rather than
    /// blocking on a half-registered epoll.
    ///
    /// Foreign sources are **not** re-bound: their descriptors are caller-owned
    /// and may or may not survive the resume. A caller whose sources did not
    /// survive must `remove_source` the old descriptor — already a no-op once
    /// the kernel reclaimed it — and `add_source` the new one.
    ///
    /// # Errors
    ///
    /// [`ModesetError::Io`] if the new descriptor cannot be registered.
    pub fn rebind(&mut self, device: &Device) -> Result<()> {
        if let Some(old) = self.drm_fd.take() {
            // SAFETY: borrowed only for this call. The descriptor may already
            // be closed, which is why the result is discarded.
            let borrowed = unsafe { BorrowedFd::borrow_raw(old) };
            let _ = epoll::delete(&self.epoll, borrowed);
        }
        // `drm_fd` is already None; it is restored only on success.
        self.register_drm_fd(device.raw_fd())
    }

    /// Wait for and dispatch events.
    ///
    /// # Invariant 3: never returns `Interrupted`
    ///
    /// `EINTR` is retried internally, so a signal landing mid-wait — SIGCHLD,
    /// an interval timer, a profiler — never abandons a flip still in flight.
    /// For [`Timeout::Bounded`] the remaining budget is recomputed from a
    /// monotonic clock across retries, so a signal storm cannot stretch the
    /// wait past the requested duration.
    ///
    /// The Rust-specific hazard the C++ does not have: `rustix`'s epoll wait
    /// returns `Errno::INTR`, and a bare `?` on it would leak `Interrupted`
    /// straight through this contract. The retry loop below is the only thing
    /// standing between that and a broken invariant.
    ///
    /// A foreign source's own callback may still observe `EINTR` on its own
    /// reads — that remains the caller's.
    ///
    /// # Errors
    ///
    /// - [`ModesetError::NothingToWaitOn`] if there is no DRM descriptor and no
    ///   sources.
    /// - [`ModesetError::TimedOut`] if the budget elapsed with no events.
    /// - [`ModesetError::Io`] for any other failure. Never `Errno::INTR`.
    pub fn dispatch(&mut self, timeout: Timeout) -> Result<()> {
        if self.drm_fd.is_none() && self.sources.is_empty() {
            return Err(ModesetError::NothingToWaitOn);
        }

        let ready = self.wait_retrying_eintr(timeout)?;
        if ready.is_empty() {
            return Err(ModesetError::TimedOut);
        }

        self.route(&ready);
        Ok(())
    }

    /// The `epoll_wait` half of [`Self::dispatch`], with the invariant-3 retry
    /// loop. Separated so the loop is readable on its own.
    fn wait_retrying_eintr(&self, timeout: Timeout) -> Result<Vec<(RawFd, epoll::EventFlags)>> {
        let mut events: Vec<epoll::Event> = Vec::with_capacity(MAX_DISPATCH_EVENTS);

        // A bounded wait needs a deadline so the budget can be recomputed
        // across retries; an infinite or polling wait is retried verbatim.
        let deadline = match timeout {
            Timeout::Bounded(budget) => Some(self.clock.now() + budget),
            Timeout::Infinite | Timeout::Poll => None,
        };

        loop {
            let remaining = remaining_budget(timeout, deadline, self.clock.now());

            let spec = remaining.map(|d| rustix::event::Timespec {
                tv_sec: d.as_secs().try_into().unwrap_or(i64::MAX),
                tv_nsec: d.subsec_nanos().into(),
            });

            events.clear();
            // `spare_capacity`, not `&mut events`: the `Buffer` impl for
            // `&mut Vec<T>` uses the vector's *length* as `maxevents`, so a
            // freshly cleared vector asks the kernel for zero events and
            // `epoll_wait` rejects that with EINVAL.
            match epoll::wait(
                &self.epoll,
                rustix::buffer::spare_capacity(&mut events),
                spec.as_ref(),
            ) {
                Ok(_ready) => {
                    return Ok(events
                        .iter()
                        .map(|event| (key_to_fd(event.data.u64()), event.flags))
                        .collect());
                }
                // The invariant. Retry rather than propagate.
                Err(rustix::io::Errno::INTR) => {}
                Err(e) => return Err(ModesetError::Io(e)),
            }
        }
    }

    /// Route ready descriptors to the page-flip handler or a source callback.
    fn route(&mut self, ready: &[(RawFd, epoll::EventFlags)]) {
        for (fd, _flags) in ready {
            if Some(*fd) == self.drm_fd {
                self.handle_drm_events(*fd);
                continue;
            }
            // A callback may have removed its own registration mid-dispatch;
            // skipping is correct, and the next dispatch will not see it.
            if let Some(callback) = self.sources.get_mut(fd) {
                callback();
            }
        }
    }

    /// Read and dispatch pending DRM events on the device descriptor.
    fn handle_drm_events(&mut self, fd: RawFd) {
        let Some(handler) = self.handler.as_mut() else {
            // No handler installed. The events still have to be drained, or
            // epoll reports the descriptor readable forever.
            drain_drm_events(fd);
            return;
        };

        // SAFETY: `fd` is the registered DRM descriptor, alive for as long as
        // the `Device` the caller holds; the borrow ends with this call.
        let borrowed = unsafe { BorrowedFd::borrow_raw(fd) };
        let reader = DrmEventReader { fd: borrowed };

        let Ok(events) = reader.receive_events() else {
            // A read error here means the descriptor is gone or the kernel gave
            // us a short read; either way the next dispatch re-observes it.
            return;
        };

        for event in events {
            if let drm::control::Event::PageFlip(flip) = event {
                handler(PageFlipEvent {
                    crtc_id: flip.crtc.into(),
                    sequence: u64::from(flip.frame),
                    timestamp: Timestamp::from_nanos(
                        u64::try_from(flip.duration.as_nanos()).unwrap_or(u64::MAX),
                    ),
                });
            }
        }
    }
}

/// The timeout to pass to the next `epoll_wait` attempt.
///
/// This is the budget-recomputation half of **invariant 3**, factored out so it
/// can be tested without signals. An infinite wait stays infinite and a poll
/// stays a poll — both are retried verbatim. A bounded wait returns what is
/// left of its deadline, saturating at zero once the deadline has passed, so a
/// retry after the budget expired becomes a poll rather than a fresh
/// full-length wait.
///
/// The saturation is the whole point. Returning the full budget on every retry
/// would mean a sub-millisecond signal cadence restarts a 50 ms wait forever
/// and `dispatch` never returns at all — a hang, not a wrong answer, which is
/// why this needs a test that cannot itself hang.
#[expect(
    clippy::match_same_arms,
    reason = "a poll and an unmeasurable bounded wait both yield zero for \
              different reasons; merging them would erase why"
)]
pub(crate) fn remaining_budget(
    timeout: Timeout,
    deadline: Option<Timestamp>,
    now: Timestamp,
) -> Option<Duration> {
    match (timeout, deadline) {
        (Timeout::Infinite, _) => None,
        (Timeout::Poll, _) => Some(Duration::ZERO),
        (Timeout::Bounded(_), Some(deadline)) => Some(deadline.saturating_duration_since(now)),
        // A bounded wait with no deadline cannot happen -- `dispatch` always
        // computes one -- but treating it as expired is the safe reading: it
        // ends the wait rather than blocking indefinitely on a budget nobody
        // can measure.
        (Timeout::Bounded(_), None) => Some(Duration::ZERO),
    }
}

/// Drain DRM events without dispatching them, so a descriptor with no
/// installed handler does not stay permanently readable.
fn drain_drm_events(fd: RawFd) {
    // SAFETY: as in `handle_drm_events`.
    let borrowed = unsafe { BorrowedFd::borrow_raw(fd) };
    let reader = DrmEventReader { fd: borrowed };
    let _ = reader.receive_events();
}

/// Minimal adapter giving the `drm` crate's event reader something to borrow.
struct DrmEventReader<'a> {
    fd: BorrowedFd<'a>,
}

impl std::os::fd::AsFd for DrmEventReader<'_> {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.fd
    }
}

impl drm::Device for DrmEventReader<'_> {}
impl drm::control::Device for DrmEventReader<'_> {}

/// epoll carries a `u64` cookie; descriptors are `i32`. Round-tripping through
/// the unsigned 32-bit representation is exact for every descriptor the kernel
/// can hand out, and keeps a hypothetical negative value from aliasing another
/// key.
const fn fd_key(fd: RawFd) -> u64 {
    fd.cast_unsigned() as u64
}

/// Inverse of [`fd_key`]. Only ever called on a key this module wrote.
const fn key_to_fd(key: u64) -> RawFd {
    #[expect(
        clippy::cast_possible_truncation,
        reason = "the key was produced by fd_key from a 32-bit descriptor, so \
                  the high half is always zero"
    )]
    let narrowed = key as u32;
    narrowed.cast_signed()
}

impl Drop for PageFlip {
    fn drop(&mut self) {
        // Registrations die with the epoll descriptor; nothing to unwind. The
        // caller still owns every source descriptor and closes them itself.
    }
}
