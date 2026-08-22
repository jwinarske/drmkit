// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! drmkit's library-wide monotonic clock abstraction.
//!
//! Port of `src/time/clock.{hpp,cpp}` from drm-cxx @ `dc2915b`.
//!
//! Subsystems that animate, pace, or schedule take a [`&dyn Clock`](Clock) so
//! callers can:
//!
//! - sync animation with an external presentation clock — a compositor that
//!   already tracks vblank/present timestamps hands the same time point to
//!   drmkit and to its own renderers;
//! - freeze or step time in tests without sleeping (see [`ManualClock`]);
//! - consolidate on a single clock as other timing-aware subsystems
//!   (page-flip pacing, input debouncing, animation cycles) land.
//!
//! Callers who do not care get [`default_clock`] and never construct a clock.
//!
//! # Why a raw nanosecond count and not `std::time::Instant`
//!
//! [`Timestamp`] is a bare `CLOCK_MONOTONIC` nanosecond count, not an
//! `Instant`. `Instant` is opaque: it cannot be constructed from a value, so
//! it can express neither of the first two bullets above. The kernel hands
//! back page-flip and vblank timestamps as `CLOCK_MONOTONIC` — the whole point
//! of the abstraction is that a compositor can pass one of *those* straight in.
//!
//! This is a deliberate deviation from the C++, which lifts
//! `std::chrono::steady_clock`'s `time_since_epoch()` onto its own tag.
//! `steady_clock` is `CLOCK_MONOTONIC` on glibc in practice, so the values
//! agree; naming `CLOCK_MONOTONIC` outright makes the contract explicit rather
//! than incidental.

use core::fmt;
use core::ops::{Add, AddAssign, Sub, SubAssign};
use core::time::Duration;

/// A point on the monotonic timeline, in nanoseconds since an unspecified
/// epoch (`CLOCK_MONOTONIC`, i.e. system boot on Linux).
///
/// Only intervals are meaningful; the absolute value carries no wall-clock
/// meaning and must never be formatted as a date. The counter is monotonic and
/// does not jump when the wall clock is stepped by NTP or by the user.
///
/// A `u64` nanosecond counter covers roughly 584 years of uptime, so overflow
/// is not a practical concern — but arithmetic here saturates rather than
/// wrapping, because a clock that silently runs backwards would turn a pacing
/// bug into a torn frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Timestamp(u64);

impl Timestamp {
    /// The zero point of the monotonic timeline. Useful as a "never" sentinel
    /// and as the starting value for a [`ManualClock`].
    pub const ZERO: Self = Self(0);

    /// Build a timestamp from a raw `CLOCK_MONOTONIC` nanosecond count — for
    /// example one the kernel reported with a page-flip event.
    #[must_use]
    pub const fn from_nanos(nanos: u64) -> Self {
        Self(nanos)
    }

    /// Build a timestamp from a `CLOCK_MONOTONIC` `timespec` pair, the shape
    /// DRM event structs and `libinput` report.
    ///
    /// Negative components clamp to zero: `CLOCK_MONOTONIC` never yields them,
    /// and clamping keeps a malformed event from producing a timestamp that
    /// sorts before [`Timestamp::ZERO`].
    #[must_use]
    pub const fn from_timespec(secs: i64, nanos: i64) -> Self {
        let secs = if secs < 0 { 0 } else { secs.cast_unsigned() };
        let nanos = if nanos < 0 { 0 } else { nanos.cast_unsigned() };
        Self(secs.saturating_mul(1_000_000_000).saturating_add(nanos))
    }

    /// Build a timestamp from a `CLOCK_MONOTONIC` `timeval` pair (microsecond
    /// resolution), the shape `drmEventVBlank` reports.
    #[must_use]
    pub const fn from_timeval(secs: i64, micros: i64) -> Self {
        let secs = if secs < 0 { 0 } else { secs.cast_unsigned() };
        let micros = if micros < 0 {
            0
        } else {
            micros.cast_unsigned()
        };
        Self(
            secs.saturating_mul(1_000_000_000)
                .saturating_add(micros.saturating_mul(1_000)),
        )
    }

    /// The raw nanosecond count.
    #[must_use]
    pub const fn as_nanos(self) -> u64 {
        self.0
    }

    /// Interval from `earlier` to `self`, saturating at zero if `earlier` is
    /// in fact later.
    ///
    /// Saturating is the right failure mode for a display pipeline: a
    /// negative interval would otherwise wrap to ~584 years and stall pacing
    /// until reboot.
    ///
    /// ```
    /// use core::time::Duration;
    /// use drmkit_time::Timestamp;
    ///
    /// let t0 = Timestamp::from_nanos(1_000);
    /// let t1 = Timestamp::from_nanos(17_667_000);
    /// assert_eq!(t1.saturating_duration_since(t0), Duration::from_nanos(17_666_000));
    /// assert_eq!(t0.saturating_duration_since(t1), Duration::ZERO);
    /// ```
    #[must_use]
    pub const fn saturating_duration_since(self, earlier: Self) -> Duration {
        Duration::from_nanos(self.0.saturating_sub(earlier.0))
    }

    /// Interval from `earlier` to `self`, or `None` if `earlier` is later.
    ///
    /// Use this where going backwards indicates a bug worth surfacing rather
    /// than absorbing.
    #[must_use]
    pub const fn checked_duration_since(self, earlier: Self) -> Option<Duration> {
        match self.0.checked_sub(earlier.0) {
            Some(delta) => Some(Duration::from_nanos(delta)),
            None => None,
        }
    }
}

impl Add<Duration> for Timestamp {
    type Output = Self;

    fn add(self, rhs: Duration) -> Self {
        Self(self.0.saturating_add(saturating_nanos(rhs)))
    }
}

impl AddAssign<Duration> for Timestamp {
    fn add_assign(&mut self, rhs: Duration) {
        *self = *self + rhs;
    }
}

impl Sub<Duration> for Timestamp {
    type Output = Self;

    fn sub(self, rhs: Duration) -> Self {
        Self(self.0.saturating_sub(saturating_nanos(rhs)))
    }
}

impl SubAssign<Duration> for Timestamp {
    fn sub_assign(&mut self, rhs: Duration) {
        *self = *self - rhs;
    }
}

impl Sub for Timestamp {
    type Output = Duration;

    /// Equivalent to [`Timestamp::saturating_duration_since`].
    fn sub(self, rhs: Self) -> Duration {
        self.saturating_duration_since(rhs)
    }
}

impl fmt::Display for Timestamp {
    /// Renders as seconds with nanosecond precision — readable in a trace
    /// without implying a wall-clock date.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}.{:09}s",
            self.0 / 1_000_000_000,
            self.0 % 1_000_000_000
        )
    }
}

/// `Duration::as_nanos` is a `u128`; clamp it into the `u64` timeline instead
/// of truncating. A duration past `u64::MAX` nanoseconds is nonsense in a
/// display pipeline, and clamping keeps the saturating contract intact.
fn saturating_nanos(d: Duration) -> u64 {
    u64::try_from(d.as_nanos()).unwrap_or(u64::MAX)
}

/// A monotonic clock.
///
/// One dynamic call per sample — negligible next to a KMS ioctl. Implementors
/// must be monotonic: successive [`now`](Clock::now) values must never
/// decrease, or pacing arithmetic downstream saturates to zero and stalls.
///
/// `Send + Sync` because a scene and its sources may sample the clock from
/// different threads; the C++ `Clock&` carries the same expectation implicitly.
pub trait Clock: Send + Sync + fmt::Debug {
    /// Sample the clock.
    fn now(&self) -> Timestamp;
}

/// Default implementation backed by `CLOCK_MONOTONIC`.
///
/// Holds no state, so it is safe to instantiate per caller or share via
/// [`default_clock`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct SteadyClock;

impl SteadyClock {
    /// Construct the clock. Equivalent to [`SteadyClock::default`].
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl Clock for SteadyClock {
    /// # Panics
    ///
    /// Never in practice. `clock_gettime(CLOCK_MONOTONIC)` cannot fail on a
    /// Linux kernel that supports it, and `rustix` models it as infallible.
    fn now(&self) -> Timestamp {
        let ts = rustix::time::clock_gettime(rustix::time::ClockId::Monotonic);
        Timestamp::from_timespec(ts.tv_sec, ts.tv_nsec as i64)
    }
}

/// A process-wide [`SteadyClock`]. The returned reference is valid for the
/// lifetime of the program.
///
/// ```
/// use drmkit_time::{Clock, default_clock};
///
/// let t0 = default_clock().now();
/// let t1 = default_clock().now();
/// assert!(t1 >= t0, "CLOCK_MONOTONIC must not run backwards");
/// ```
#[must_use]
pub fn default_clock() -> &'static SteadyClock {
    static CLOCK: SteadyClock = SteadyClock::new();
    &CLOCK
}

/// A clock whose time only moves when the test moves it.
///
/// The C++ header names "freeze or step time in tests without sleeping" as a
/// reason the abstraction exists, but leaves each test to build its own stub.
/// Providing one here keeps the scene, cursor, and pacing suites from
/// reinventing it — and keeps them from reinventing it *differently*.
///
/// Interior mutability is deliberate: [`Clock::now`] takes `&self`, and a test
/// holding a `&dyn Clock` still needs to advance it.
///
/// ```
/// use core::time::Duration;
/// use drmkit_time::{Clock, ManualClock, Timestamp};
///
/// let clock = ManualClock::new();
/// assert_eq!(clock.now(), Timestamp::ZERO);
///
/// clock.advance(Duration::from_millis(16));
/// assert_eq!(clock.now(), Timestamp::from_nanos(16_000_000));
///
/// clock.set(Timestamp::from_nanos(1_000));
/// assert_eq!(clock.now().as_nanos(), 1_000);
/// ```
#[derive(Debug, Default)]
pub struct ManualClock {
    nanos: core::sync::atomic::AtomicU64,
}

impl ManualClock {
    /// A clock frozen at [`Timestamp::ZERO`].
    #[must_use]
    pub const fn new() -> Self {
        Self {
            nanos: core::sync::atomic::AtomicU64::new(0),
        }
    }

    /// A clock frozen at `start`.
    #[must_use]
    pub const fn starting_at(start: Timestamp) -> Self {
        Self {
            nanos: core::sync::atomic::AtomicU64::new(start.as_nanos()),
        }
    }

    /// Move the clock forward by `delta`. Saturates at `u64::MAX` nanoseconds.
    ///
    /// Saturating rather than wrapping is the whole point: a wrapped counter
    /// runs *backwards*, and every interval taken against it collapses to
    /// [`Duration::ZERO`] (see [`Timestamp::saturating_duration_since`]) —
    /// a test clock that silently stalls pacing instead of failing loudly.
    pub fn advance(&self, delta: Duration) {
        let step = saturating_nanos(delta);
        // `fetch_add` wraps on overflow, so clamp inside a compare-exchange
        // loop instead. Hand-rolled rather than `fetch_update`, which nightly
        // has deprecated in favor of `try_update` -- naming the primitives
        // directly keeps this compiling on stable and nightly alike, which the
        // miri lane needs.
        let mut current = self.nanos.load(core::sync::atomic::Ordering::Relaxed);
        loop {
            let clamped = current.saturating_add(step);
            match self.nanos.compare_exchange_weak(
                current,
                clamped,
                core::sync::atomic::Ordering::Relaxed,
                core::sync::atomic::Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(observed) => current = observed,
            }
        }
    }

    /// Jump the clock to `at`.
    ///
    /// Nothing stops a test from moving it backwards — that is the point of a
    /// manual clock — but production code must never observe a clock that
    /// does, so keep such tests scoped to the code under test.
    pub fn set(&self, at: Timestamp) {
        self.nanos
            .store(at.as_nanos(), core::sync::atomic::Ordering::Relaxed);
    }
}

impl Clock for ManualClock {
    fn now(&self) -> Timestamp {
        Timestamp::from_nanos(self.nanos.load(core::sync::atomic::Ordering::Relaxed))
    }
}

#[cfg(test)]
mod tests;
