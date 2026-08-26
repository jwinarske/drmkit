// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! Saying out loud when a device case asserted nothing.
//!
//! A device test that cannot reach its case returns early, and the harness
//! reports it as `ok`. That is the right thing to do -- the case is about
//! hardware this device does not have -- but it means a green run carries no
//! information about how much of it ran.
//!
//! It is not a small effect. There are 114 such bail-outs across 118 device
//! cases, so on any given board an unknown fraction of a passing run asserted
//! nothing at all. Every hardware claim in this tree that turned out to be
//! wrong was a claim nothing measured, and a case that skips silently is the
//! most comfortable way to stop measuring: the count goes up, the colour
//! stays green, and the assertion never runs.
//!
//! [`skipped`] prints a line the runner can find. It does not change what the
//! harness reports -- a skip is still not a failure -- it only makes the
//! difference visible.
//!
//! ```no_run
//! let Some(crtc) = find_a_crtc_with_a_pipeline() else {
//!     drmkit_testkit::skipped("no CRTC on this device has a colour pipeline");
//!     return;
//! };
//! # fn find_a_crtc_with_a_pipeline() -> Option<u32> { None }
//! ```

/// The prefix the runner greps for. Tab-separated so a reason may contain
/// anything but a tab.
pub const MARKER: &str = "DRMKIT-SKIP";

/// Record that this case bailed without asserting what it is named for.
///
/// Call it immediately before the `return`, with the reason in the terms of
/// the hardware -- "this CRTC has no gamma table", not "unsupported".
///
/// The test's name comes from the thread the harness runs it on, so the line
/// identifies itself without the caller repeating a name that can drift out
/// of sync with the function it labels.
pub fn skipped(reason: &str) {
    let thread = std::thread::current();
    let case = thread.name().unwrap_or("<unnamed>");
    // stdout, so it travels with the harness's own output under --nocapture
    // and lands in the same log the runner already collects.
    println!("{MARKER}\t{case}\t{reason}");
}
