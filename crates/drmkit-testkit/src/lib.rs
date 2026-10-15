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
//! most comfortable way to stop measuring: the count goes up, the color
//! stays green, and the assertion never runs.
//!
//! [`skipped`] prints a line the runner can find. It does not change what the
//! harness reports -- a skip is still not a failure -- it only makes the
//! difference visible.
//!
//! ```no_run
//! let Some(crtc) = find_a_crtc_with_a_pipeline() else {
//!     drmkit_testkit::skipped("no CRTC on this device has a color pipeline");
//!     return;
//! };
//! # fn find_a_crtc_with_a_pipeline() -> Option<u32> { None }
//! ```

/// Picking the CRTC a device suite should drive.
///
/// Lived in `drmkit-scene/tests/common` and was reachable only from that
/// crate, so `drmkit-planes` -- which needs the same answer to check how many
/// planes the CRTC under test actually has -- could not have it. Two copies of
/// this rule would drift, and the whole point of it is that every suite
/// measures the same CRTC.
pub mod crtc {

    use drm::control::{Device as _, ResourceHandles};
    use drmkit_core::Device;

    /// Pick a CRTC to drive, returning its id and its index in the resource list.
    ///
    /// Reaching for `crtcs().first()` is a vkms-shaped habit: vkms has one CRTC and
    /// it is the one with the display on it, so the first is always right and the
    /// index is always zero. Real hardware does not oblige. On a Raspberry Pi 5,
    /// vc4 lists four: index 0 is inactive with no connector attached and
    /// advertises 34 candidate planes, while the connected HDMI sits on index 2
    /// with 18. Sizing plane arithmetic against the first one measures a CRTC that
    /// cannot scan out, and the resulting rejections read as library bugs.
    ///
    /// Prefers a CRTC a connected connector is currently routed to, and falls back
    /// to the first when nothing is connected -- which keeps the vkms lane, where
    /// no connector reports `Connected`, behaving exactly as before.
    /// `DRMKIT_TEST_CRTC` overrides with an explicit index.
    /// # Panics
    ///
    /// If the device lists no CRTC, which is not a device a display suite has
    /// anything to say about.
    #[must_use]
    pub fn pick_crtc(device: &Device, resources: &ResourceHandles) -> (u32, u32) {
        let crtcs = resources.crtcs();
        assert!(!crtcs.is_empty(), "a device with no CRTC cannot display");

        let chosen = std::env::var("DRMKIT_TEST_CRTC")
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
            .filter(|index| *index < crtcs.len())
            .or_else(|| connected_crtc(device, resources))
            .unwrap_or(0);

        let index = u32::try_from(chosen).expect("a CRTC index fits in a u32");
        (crtcs[chosen].into(), index)
    }

    /// Index of the first CRTC a connected connector is routed to.
    fn connected_crtc(device: &Device, resources: &ResourceHandles) -> Option<usize> {
        let crtcs = resources.crtcs();
        for handle in resources.connectors() {
            // `false`: ask for the cached state rather than forcing a probe. A
            // probe on every connector is slow and can wake a sleeping sink.
            let Ok(connector) = device.get_connector(*handle, false) else {
                continue;
            };
            if connector.state() != drm::control::connector::State::Connected {
                continue;
            }
            let Some(encoder) = connector.current_encoder() else {
                continue;
            };
            let Ok(encoder) = device.get_encoder(encoder) else {
                continue;
            };
            let Some(crtc) = encoder.crtc() else {
                continue;
            };
            if let Some(index) = crtcs.iter().position(|candidate| *candidate == crtc) {
                return Some(index);
            }
        }
        None
    }
}

/// The prefix identifying which card a run actually used.
pub const DEVICE_MARKER: &str = "DRMKIT-DEVICE";

/// Say which card this run opened, and what drives it.
///
/// The suites take their card from `DRMKIT_TEST_CARD`, defaulting to
/// `/dev/dri/card0`. Which driver that is depends on the machine, and nothing
/// in a run's output used to say. On the workstation this was written on,
/// card0 is vkms and amdgpu is card1 -- so a session's worth of results
/// labeled "amdgpu" were vkms, and nothing in the logs contradicted it.
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
/// Public because the lane-health checks need it too: telling vkms from any
/// other DRM device is the whole of what they assert.
///
/// From the DRM version ioctl, not from sysfs. sysfs reports the *bus* driver
/// -- vkms binds through the faux bus and shows up there as `faux_driver`,
/// which is exactly the sort of near-answer that let a vkms run be labeled
/// amdgpu in the first place. The ioctl gives the name the driver registers,
/// and needs neither master nor any capability.
#[must_use]
pub fn driver_of(path: &str) -> String {
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

/// Picking the output a device suite should drive.
pub mod connector {
    use drm::control::Device as _;
    use drmkit_core::Device;

    /// The connector `DRMKIT_TEST_CONNECTORS` names, if it is set and one of
    /// the names it lists, in order, is connected with a mode.
    ///
    /// `None` when unset, which keeps first-connected. Names are the kernel's
    /// `<type>-<n>` (`DP-4`, `HDMI-A-1`). For boards whose other connected
    /// outputs are not the test's to light: on the SA8155P the first one is
    /// the platform's shared display. The scene fixture reads the same
    /// variable.
    #[must_use]
    pub fn wanted(device: &Device) -> Option<u32> {
        let list = std::env::var("DRMKIT_TEST_CONNECTORS").ok()?;
        let resources = device.resource_handles().ok()?;
        let connectors: Vec<_> = resources
            .connectors()
            .iter()
            .filter_map(|handle| device.get_connector(*handle, false).ok())
            .filter(|info| {
                info.state() == drm::control::connector::State::Connected
                    && !info.modes().is_empty()
            })
            .collect();
        list.split(',').map(str::trim).find_map(|want| {
            connectors
                .iter()
                .find(|info| {
                    format!("{}-{}", info.interface().as_str(), info.interface_id()) == want
                })
                .map(|info| u32::from(info.handle()))
        })
    }
}

/// Sizing a buffer the device will frame.
pub mod framebuffer {
    use std::ops::{Bound, RangeBounds};

    use drm::control::Device as _;
    use drmkit_core::Device;

    /// `width` x `height`, each pulled into the range the device creates
    /// framebuffers in.
    ///
    /// The range is the driver's `mode_config` minimum and maximum, which most
    /// drivers set wide enough that a case's 64x64 buffer is never near it. A
    /// `panel-mipi-dbi` panel sets both to its one mode, so it creates no
    /// framebuffer that is not exactly 240x240, and a case asking for less
    /// fails on the size it picked rather than on anything it tests. The size
    /// asked for comes back where the device takes it, and where the device
    /// cannot be asked (no resources) it comes back unchanged.
    #[must_use]
    pub fn size(device: &Device, width: u32, height: u32) -> (u32, u32) {
        let Ok(resources) = device.resource_handles() else {
            return (width, height);
        };
        (
            within(&resources.supported_fb_width(), width),
            within(&resources.supported_fb_height(), height),
        )
    }

    fn within(range: &impl RangeBounds<u32>, value: u32) -> u32 {
        let low = match range.start_bound() {
            Bound::Included(low) => *low,
            Bound::Excluded(low) => low.saturating_add(1),
            Bound::Unbounded => 0,
        };
        let high = match range.end_bound() {
            Bound::Included(high) => *high,
            Bound::Excluded(high) => high.saturating_sub(1),
            Bound::Unbounded => u32::MAX,
        };
        // A driver reporting no maximum (0) is not saying nothing fits.
        if high < low {
            return value.max(low);
        }
        value.clamp(low, high)
    }

    #[cfg(test)]
    mod tests {
        use super::within;

        #[test]
        fn a_size_inside_the_range_is_kept() {
            assert_eq!(within(&(16..=4096), 64), 64);
        }

        #[test]
        fn a_size_below_a_fixed_panel_grows_to_it() {
            assert_eq!(within(&(240..=240), 64), 240);
        }

        #[test]
        fn a_size_above_the_maximum_shrinks_to_it() {
            assert_eq!(within(&(0..=2048), 4096), 2048);
        }

        #[test]
        fn a_driver_with_no_maximum_does_not_shrink_anything() {
            // What the kernel reports as `max_width` 0: an empty range.
            let (min, max) = (16, 0);
            assert_eq!(within(&(min..=max), 64), 64);
        }
    }
}
