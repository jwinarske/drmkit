// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! A rejected fast-path frame is warned about, once.
//!
//! Its own binary because the log sink is process-wide: a case in a shared
//! binary would count warnings from whichever other cases ran beside it.

use std::sync::{Arc, Mutex, PoisonError};

use drmkit_log::LogLevel;
use drmkit_scene::{CommitKind, CommitReport, FrameLifecycle, KernelResult};

fn reject_a_fast_path_frame(lifecycle: &mut FrameLifecycle) {
    // `#[non_exhaustive]`, so no struct literal from outside the crate.
    let mut report = CommitReport::default();
    report.fb_delta_fast_path = true;
    let outcome = lifecycle.finalize(
        CommitKind::Real { arms_flip: true },
        KernelResult::Rejected,
        Vec::new(),
        report,
        None,
        |_| false,
    );
    assert!(outcome.invalidate_allocation);
}

/// Upstream warns on the first fast-path frame the kernel refuses and stays
/// quiet after: a recurring rejection means a property was misclassified as
/// content, which is worth saying, and saying it every frame would bury it.
/// An ordinary rejection, after a real `TEST_ONLY`, says nothing new and is
/// not warned about at all.
#[test]
fn a_rejected_fast_path_frame_is_warned_about_once() {
    let captured: Arc<Mutex<Vec<(LogLevel, String)>>> = Arc::default();
    let sink = Arc::clone(&captured);
    drmkit_log::set_log_sink(move |level, message| {
        sink.lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push((level, message.to_owned()));
    });

    let mut lifecycle = FrameLifecycle::new();
    lifecycle.finalize(
        CommitKind::Real { arms_flip: true },
        KernelResult::Rejected,
        Vec::new(),
        CommitReport::default(),
        None,
        |_| false,
    );
    let after_ordinary = captured
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .len();

    for _ in 0..3 {
        reject_a_fast_path_frame(&mut lifecycle);
    }
    drmkit_log::reset_log_sink();

    let captured = captured.lock().unwrap_or_else(PoisonError::into_inner);
    assert_eq!(after_ordinary, 0, "an ordinary rejection: {captured:?}");
    let warnings: Vec<_> = captured
        .iter()
        .filter(|(_, message)| message.contains("fast-path"))
        .collect();
    assert_eq!(
        warnings.len(),
        1,
        "three rejections, one warning: {captured:?}"
    );
    assert_eq!(warnings[0].0, LogLevel::Warn);
    assert!(lifecycle.has_warned_fast_path_reject());
}
