// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! Device selection shared by the on-device lanes.

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
pub(crate) fn pick_crtc(device: &Device, resources: &ResourceHandles) -> (u32, u32) {
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
