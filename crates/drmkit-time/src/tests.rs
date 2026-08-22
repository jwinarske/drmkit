// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! drm-cxx ships no `test_clock.cpp` — the C++ clock is exercised only
//! indirectly through `cursor::Renderer`. These tests pin the contract the
//! header states in prose, so the Rust port has a named home for it before
//! any consumer lands.

use super::*;

#[test]
fn steady_clock_is_monotonic() {
    let clock = default_clock();
    let mut previous = clock.now();
    for _ in 0..1_000 {
        let current = clock.now();
        assert!(
            current >= previous,
            "CLOCK_MONOTONIC went backwards: {previous} -> {current}"
        );
        previous = current;
    }
}

#[test]
fn steady_clock_advances() {
    let clock = default_clock();
    let start = clock.now();
    // Busy-spin rather than sleep: this asserts the clock moves, and a sleep
    // would make the test a scheduler observation instead.
    let mut end = clock.now();
    while end == start {
        end = clock.now();
    }
    assert!(end > start);
}

#[test]
fn steady_clock_is_shareable_as_trait_object() {
    let clock: &dyn Clock = default_clock();
    assert!(clock.now() > Timestamp::ZERO, "boot was some time ago");
}

#[test]
fn manual_clock_starts_frozen_at_zero() {
    let clock = ManualClock::new();
    assert_eq!(clock.now(), Timestamp::ZERO);
    assert_eq!(clock.now(), Timestamp::ZERO, "does not move on its own");
}

#[test]
fn manual_clock_advances_and_sets() {
    let clock = ManualClock::starting_at(Timestamp::from_nanos(500));
    assert_eq!(clock.now().as_nanos(), 500);

    clock.advance(Duration::from_millis(16));
    assert_eq!(clock.now().as_nanos(), 500 + 16_000_000);

    clock.set(Timestamp::ZERO);
    assert_eq!(clock.now(), Timestamp::ZERO);
}

/// A wrapped counter runs backwards, and every interval taken against it
/// collapses to zero — a test clock that stalls pacing silently instead of
/// failing loudly. Saturation must be exact, not approximate.
#[test]
fn manual_clock_advance_saturates_instead_of_wrapping() {
    let clock = ManualClock::starting_at(Timestamp::from_nanos(u64::MAX - 5));
    let before = clock.now();

    clock.advance(Duration::from_secs(1));

    assert_eq!(
        clock.now().as_nanos(),
        u64::MAX,
        "must clamp at the ceiling"
    );
    assert!(clock.now() >= before, "must never move backwards");

    // Still saturated after a second overflowing step.
    clock.advance(Duration::from_secs(1));
    assert_eq!(clock.now().as_nanos(), u64::MAX);
}

/// A `Duration` larger than the `u64` nanosecond timeline must clamp too —
/// `Duration::as_nanos` is a `u128`, so a naive cast would truncate.
#[test]
fn manual_clock_advance_clamps_oversized_duration() {
    let clock = ManualClock::new();
    clock.advance(Duration::from_secs(u64::MAX));
    assert_eq!(clock.now().as_nanos(), u64::MAX);
}

#[test]
fn duration_since_saturates_instead_of_wrapping() {
    let early = Timestamp::from_nanos(1_000);
    let late = Timestamp::from_nanos(17_667_000);

    assert_eq!(
        late.saturating_duration_since(early),
        Duration::from_nanos(17_666_000)
    );
    assert_eq!(early.saturating_duration_since(late), Duration::ZERO);
    assert_eq!(late - early, Duration::from_nanos(17_666_000));
    assert_eq!(early - late, Duration::ZERO);
}

#[test]
fn checked_duration_since_reports_going_backwards() {
    let early = Timestamp::from_nanos(1_000);
    let late = Timestamp::from_nanos(2_000);

    assert_eq!(
        late.checked_duration_since(early),
        Some(Duration::from_nanos(1_000))
    );
    assert_eq!(early.checked_duration_since(late), None);
    assert_eq!(early.checked_duration_since(early), Some(Duration::ZERO));
}

#[test]
fn timestamp_arithmetic_saturates_at_both_ends() {
    let t = Timestamp::from_nanos(1_000);
    assert_eq!((t - Duration::from_secs(1)).as_nanos(), 0, "clamps at zero");
    assert_eq!((t + Duration::from_nanos(500)).as_nanos(), 1_500);

    let high = Timestamp::from_nanos(u64::MAX - 1);
    assert_eq!((high + Duration::from_secs(10)).as_nanos(), u64::MAX);

    let mut acc = Timestamp::ZERO;
    acc += Duration::from_millis(16);
    assert_eq!(acc.as_nanos(), 16_000_000);
    acc -= Duration::from_millis(32);
    assert_eq!(acc, Timestamp::ZERO, "clamps rather than wrapping");
}

#[test]
fn from_timespec_and_timeval_agree() {
    // 3s + 250ms expressed both ways — the two shapes the kernel uses for
    // page-flip (timespec) and vblank (timeval) events.
    let via_timespec = Timestamp::from_timespec(3, 250_000_000);
    let via_timeval = Timestamp::from_timeval(3, 250_000);
    assert_eq!(via_timespec, via_timeval);
    assert_eq!(via_timespec.as_nanos(), 3_250_000_000);
}

#[test]
fn negative_kernel_components_clamp_to_zero() {
    // CLOCK_MONOTONIC never reports these; clamping keeps a malformed event
    // from sorting before Timestamp::ZERO.
    assert_eq!(Timestamp::from_timespec(-1, -1), Timestamp::ZERO);
    assert_eq!(Timestamp::from_timeval(-1, -1), Timestamp::ZERO);
    assert_eq!(Timestamp::from_timespec(-1, 500).as_nanos(), 500);
}

#[test]
fn display_reads_as_an_interval_not_a_date() {
    assert_eq!(
        Timestamp::from_nanos(3_250_000_000).to_string(),
        "3.250000000s"
    );
    assert_eq!(Timestamp::ZERO.to_string(), "0.000000000s");
}
