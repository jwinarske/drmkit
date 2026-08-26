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

/// The prefix identifying which card a run actually used.
pub const DEVICE_MARKER: &str = "DRMKIT-DEVICE";

/// Say which card this run opened, and what drives it.
///
/// The suites take their card from `DRMKIT_TEST_CARD`, defaulting to
/// `/dev/dri/card0`. Which driver that is depends on the machine, and nothing
/// in a run's output used to say. On the workstation this was written on,
/// card0 is vkms and amdgpu is card1 -- so a session's worth of results
/// labelled "amdgpu" were vkms, and nothing in the logs contradicted it.
///
/// Printed once per test binary. The driver is read from sysfs rather than
/// asked of the device, so it is the kernel's own name for what is bound.
pub fn announce_card(path: &str) {
    use std::sync::Once;
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        println!("{DEVICE_MARKER}\t{path}\t{}", driver_of(path));
    });
}

/// The driver's own name for itself.
///
/// From the DRM version ioctl, not from sysfs. sysfs reports the *bus* driver
/// -- vkms binds through the faux bus and shows up there as `faux_driver`,
/// which is exactly the sort of near-answer that let a vkms run be labelled
/// amdgpu in the first place. The ioctl gives the name the driver registers,
/// and needs neither master nor any capability.
fn driver_of(path: &str) -> String {
    use drm::Device as _;

    struct Node(std::fs::File);
    impl std::os::fd::AsFd for Node {
        fn as_fd(&self) -> std::os::fd::BorrowedFd<'_> {
            std::os::fd::AsFd::as_fd(&self.0)
        }
    }
    impl drm::Device for Node {}

    std::fs::File::open(path)
        .ok()
        .map(Node)
        .and_then(|node| node.get_driver().ok())
        .map_or_else(
            || "unknown".to_owned(),
            |driver| driver.name().to_string_lossy().into_owned(),
        )
}

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
