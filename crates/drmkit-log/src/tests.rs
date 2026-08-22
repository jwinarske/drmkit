// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! Parity port of `tests/unit/test_log.cpp` from drm-cxx @ `dc2915b`, plus
//! pins for behavior the C++ suite documents but does not test.
//!
//! The C++ fixture snapshots and restores the global level and sink around
//! each test. gtest runs cases serially, so a snapshot suffices there. Rust's
//! harness runs cases on parallel threads against the same process-wide
//! state, so a snapshot alone would let a level-filter test bleed into an
//! unrelated one. [`Fixture`] therefore serializes on a mutex *and* restores,
//! which is the same contract the C++ fixture provides.

use super::*;
use std::sync::{Mutex, MutexGuard};

static TEST_LOCK: Mutex<()> = Mutex::new(());

type Captured = Arc<Mutex<Vec<(LogLevel, String)>>>;

/// Serializes access to the process-wide level and sink, installs a capturing
/// sink, and restores both on drop.
struct Fixture {
    _guard: MutexGuard<'static, ()>,
    events: Captured,
    saved_level: LogLevel,
    saved_sink: Option<LogSink>,
}

impl Fixture {
    fn new() -> Self {
        let guard = TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let saved_level = log_level();
        let saved_sink = log_sink();

        let events: Captured = Arc::new(Mutex::new(Vec::new()));
        let target = Arc::clone(&events);
        set_log_sink(move |level, message| {
            target
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push((level, message.to_owned()));
        });
        set_log_level(LogLevel::Debug); // accept everything by default

        Self {
            _guard: guard,
            events,
            saved_level,
            saved_sink,
        }
    }

    fn events(&self) -> Vec<(LogLevel, String)> {
        self.events
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    fn messages(&self) -> Vec<String> {
        self.events().into_iter().map(|(_, m)| m).collect()
    }

    fn levels(&self) -> Vec<LogLevel> {
        self.events().into_iter().map(|(l, _)| l).collect()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        match self.saved_sink.take() {
            Some(sink) => set_log_sink(move |level, message| sink(level, message)),
            None => reset_log_sink(),
        }
        set_log_level(self.saved_level);
    }
}

// --- parity with test_log.cpp ------------------------------------------------

/// `LogSinkTest.SinkReceivesLevelAndMessage`
#[test]
fn sink_receives_level_and_message() {
    let fx = Fixture::new();

    log_warn!("hello {}", "world");
    log_error!("answer is {}", 42);

    let events = fx.events();
    assert_eq!(events.len(), 2);
    assert_eq!(events[0], (LogLevel::Warn, "hello world".to_owned()));
    assert_eq!(events[1], (LogLevel::Error, "answer is 42".to_owned()));
}

/// `LogSinkTest.AllFourLevelsRouteThroughSink`
#[test]
fn all_four_levels_route_through_sink() {
    let fx = Fixture::new();

    log_error!("e");
    log_warn!("w");
    log_info!("i");
    log_debug!("d");

    assert_eq!(
        fx.levels(),
        vec![
            LogLevel::Error,
            LogLevel::Warn,
            LogLevel::Info,
            LogLevel::Debug
        ]
    );
}

/// `LogSinkTest.LevelFilterGatesSinkEntry`
#[test]
fn level_filter_gates_sink_entry() {
    let fx = Fixture::new();
    set_log_level(LogLevel::Warn);

    log_error!("err"); // <= Warn, admitted
    log_warn!("warn"); // <= Warn, admitted
    log_info!("info"); // >  Warn, dropped
    log_debug!("debug"); // >  Warn, dropped

    assert_eq!(fx.messages(), vec!["err".to_owned(), "warn".to_owned()]);
}

/// `LogSinkTest.SilentLevelDropsEverything`
#[test]
fn silent_level_drops_everything() {
    let fx = Fixture::new();
    set_log_level(LogLevel::Silent);

    log_error!("e");
    log_warn!("w");
    log_info!("i");
    log_debug!("d");

    assert!(fx.events().is_empty());
}

/// `LogSinkTest.NullSinkRestoresDefaultWithoutThrow`
///
/// The C++ case can only assert the sink stays callable, because its default
/// is itself a `std::function`. Here `reset_log_sink` is observable: the slot
/// goes back to `None`, meaning the built-in stdio sink is active.
#[test]
fn reset_sink_restores_default() {
    let fx = Fixture::new();
    assert!(log_sink().is_some(), "fixture installed a sink");

    reset_log_sink();
    assert!(log_sink().is_none(), "reset returns to the built-in sink");

    // Emitting against the default sink must not panic. Silent keeps the
    // stdio write out of the test log.
    set_log_level(LogLevel::Silent);
    log_error!("goes nowhere");
    drop(fx);
}

// --- behavior the C++ documents but does not pin ----------------------------

/// `log_channel!` bypasses the global level but not the sink — the
/// GStreamer-style carve-out documented on the C++ `log_channel`. Untested
/// upstream; a regression here would make `DRM_ALLOC_DEBUG=1` print nothing.
#[test]
fn channel_bypasses_level_but_not_sink() {
    let fx = Fixture::new();
    set_log_level(LogLevel::Silent);

    log_error!("gated away");
    log_channel!(LogLevel::Debug, "layer {} -> plane {}", 0, 31);

    assert_eq!(fx.messages(), vec!["layer 0 -> plane 31".to_owned()]);
    assert_eq!(fx.levels(), vec![LogLevel::Debug]);
}

/// Arguments must not be evaluated when the level rejects the record — the
/// C++ templates get this from lazy instantiation behind the level check.
#[test]
fn arguments_are_not_evaluated_when_gated() {
    let fx = Fixture::new();
    set_log_level(LogLevel::Error);

    let mut evaluated = false;
    let mut probe = || {
        evaluated = true;
        7
    };
    log_debug!("value {}", probe());

    assert!(!evaluated, "formatting args ran for a gated record");
    assert!(fx.events().is_empty());
}

/// The admission table, exhaustively. Silent admits nothing, including Silent.
#[test]
fn admits_is_exhaustive_and_silent_admits_nothing() {
    for active in LogLevel::ALL {
        for record in LogLevel::ALL {
            let expected = active != LogLevel::Silent && (active as u8) >= (record as u8);
            assert_eq!(
                active.admits(record),
                expected,
                "active={active} record={record}"
            );
        }
    }
}

/// Discriminants match the C++ `drm::LogLevel` byte-for-byte so consumers can
/// bridge both libraries.
#[test]
fn discriminants_match_cxx() {
    assert_eq!(LogLevel::Silent as u8, 0);
    assert_eq!(LogLevel::Error as u8, 1);
    assert_eq!(LogLevel::Warn as u8, 2);
    assert_eq!(LogLevel::Info as u8, 3);
    assert_eq!(LogLevel::Debug as u8, 4);
}

#[test]
fn parse_accepts_names_and_discriminants() {
    assert_eq!(LogLevel::parse("warn"), Some(LogLevel::Warn));
    assert_eq!(LogLevel::parse("  WARN "), Some(LogLevel::Warn));
    assert_eq!(LogLevel::parse("2"), Some(LogLevel::Warn));
    assert_eq!(LogLevel::parse("off"), Some(LogLevel::Silent));
    assert_eq!(LogLevel::parse("nonsense"), None);
    assert_eq!(LogLevel::parse("5"), None);
}

/// A sink that itself logs must not deadlock: `__emit` releases the slot guard
/// before invoking the sink. The C++ side has no equivalent hazard because it
/// holds no lock at all — and no thread-safety either.
#[test]
fn reentrant_sink_does_not_deadlock() {
    let fx = Fixture::new();
    let depth = Arc::new(Mutex::new(0_u32));
    let counter = Arc::clone(&depth);

    set_log_sink(move |_, _| {
        let mut d = counter
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if *d == 0 {
            *d += 1;
            drop(d);
            log_error!("nested");
        }
    });

    log_error!("outer");
    assert_eq!(
        *depth
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner),
        1
    );
    drop(fx);
}
