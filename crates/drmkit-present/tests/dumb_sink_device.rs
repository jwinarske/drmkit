// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! Parity port of `tests/integration/test_dumb_scanout_sink_vkms.cpp` from
//! drm-cxx @ `4a0b64a`.
//!
//! Needs a card and DRM master: these commit for real and pace on the
//! flip-complete event.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use drm::control::Device as _;
use drmkit_core::{AtomicCommitFlags, Device, Mode};
use drmkit_modeset::{ModeInfo as _, PageFlip, Timeout};
use drmkit_present::{Config, DumbScanoutSink};

static CARD_LOCK: Mutex<()> = Mutex::new(());

fn card_guard() -> MutexGuard<'static, ()> {
    CARD_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// A connected output: the CRTC that drives it, its index, its connector, and
/// a mode.
struct Output {
    crtc_id: u32,
    crtc_index: u32,
    connector_id: u32,
    mode: Mode,
}

fn open_card() -> Option<Device> {
    let path = std::env::var("DRMKIT_TEST_CARD").unwrap_or_else(|_| "/dev/dri/card0".to_owned());
    drmkit_testkit::announce_card(&path);
    let device = match Device::open(&path) {
        Ok(device) => device,
        Err(error) => {
            assert!(
                std::env::var_os("DRMKIT_REQUIRE_MASTER").is_none(),
                "{path}: {error}, but DRMKIT_REQUIRE_MASTER is set"
            );
            drmkit_testkit::skipped(&format!("no DRM device at {path} ({error})"));
            return None;
        }
    };
    device.enable_universal_planes().ok()?;
    device.enable_atomic().ok()?;
    if device.set_master().is_err() {
        assert!(
            std::env::var_os("DRMKIT_REQUIRE_MASTER").is_none(),
            "not DRM master, but DRMKIT_REQUIRE_MASTER is set"
        );
        drmkit_testkit::skipped("another client holds DRM master");
        return None;
    }
    Some(device)
}

/// The first connected connector, the CRTC its encoder can drive, and its
/// first mode.
fn pick_output(device: &Device) -> Option<Output> {
    let resources = device.resource_handles().ok()?;
    for handle in resources.connectors() {
        let Ok(connector) = device.get_connector(*handle, false) else {
            continue;
        };
        if connector.state() != drm::control::connector::State::Connected {
            continue;
        }
        let mode = *connector.modes().first()?;
        for encoder in connector.encoders() {
            let Ok(encoder) = device.get_encoder(*encoder) else {
                continue;
            };
            let Some(crtc) = resources
                .filter_crtcs(encoder.possible_crtcs())
                .first()
                .copied()
            else {
                continue;
            };
            let index = resources.crtcs().iter().position(|c| *c == crtc)?;
            return Some(Output {
                crtc_id: u32::from(crtc),
                crtc_index: u32::try_from(index).ok()?,
                connector_id: u32::from(*handle),
                mode,
            });
        }
    }
    None
}

/// Three frames, each paced on its own flip: the modeset commit, then steady
/// state, then a reused ring slot.
///
/// Pacing on the event is the point. Presenting three times without waiting
/// would pass on a sink that never armed a flip at all.
#[test]
#[ignore = "needs a DRM device and DRM master"]
fn three_frames_each_land_their_own_flip_vkms() {
    let _guard = card_guard();
    let Some(device) = open_card() else { return };
    let Some(output) = pick_output(&device) else {
        drmkit_testkit::skipped("no connected output");
        return;
    };

    let mut sink = DumbScanoutSink::create(
        &device,
        output.crtc_id,
        output.crtc_index,
        output.connector_id,
        &output.mode,
        &Config::default(),
    )
    .expect("build a sink over the connected output");

    assert_eq!(
        sink.size(),
        (output.mode.width(), output.mode.height()),
        "the sink scans out the mode it was given"
    );

    let (width, height) = sink.size();
    let stride = width * 4;
    let frame = vec![0x40_u8; stride as usize * height as usize];

    let mut flip = PageFlip::new(&device).expect("page flip");
    let flips = Arc::new(AtomicU32::new(0));
    let counted = Arc::clone(&flips);
    flip.set_handler(Box::new(move |_| {
        counted.fetch_add(1, Ordering::Relaxed);
    }));

    for index in 0..3 {
        sink.present(
            &device,
            &frame,
            stride,
            &[],
            AtomicCommitFlags::PAGE_FLIP_EVENT,
        )
        .unwrap_or_else(|error| panic!("frame {index}: {error}"));

        let before = flips.load(Ordering::Relaxed);
        while flips.load(Ordering::Relaxed) == before {
            flip.dispatch(Timeout::Bounded(std::time::Duration::from_secs(2)))
                .unwrap_or_else(|error| panic!("frame {index} flip: {error}"));
        }
        sink.flip_landed();
    }

    assert_eq!(
        flips.load(Ordering::Relaxed),
        3,
        "one flip completed per presented frame"
    );
}

/// A frame shorter than `height * stride` is refused before any IO.
///
/// The reference checks the same thing. It matters because the copy would
/// otherwise read past the caller's buffer.
#[test]
#[ignore = "needs a DRM device and DRM master"]
fn a_short_frame_is_refused_before_anything_is_touched_vkms() {
    let _guard = card_guard();
    let Some(device) = open_card() else { return };
    let Some(output) = pick_output(&device) else {
        drmkit_testkit::skipped("no connected output");
        return;
    };

    let mut sink = DumbScanoutSink::create(
        &device,
        output.crtc_id,
        output.crtc_index,
        output.connector_id,
        &output.mode,
        &Config::default(),
    )
    .expect("sink");

    let (width, _) = sink.size();
    let error = sink
        .present(
            &device,
            &[0u8; 16],
            width * 4,
            &[],
            AtomicCommitFlags::empty(),
        )
        .expect_err("sixteen bytes is not a frame");
    assert!(
        matches!(error, drmkit_present::PresentError::ShortFrame { .. }),
        "got {error:?}, which does not say what was wrong with the frame"
    );
}

/// The negotiated format is one the caller can actually render into, and the
/// sink reports it.
#[test]
#[ignore = "needs a DRM device"]
fn the_sink_reports_the_format_it_negotiated_vkms() {
    let _guard = card_guard();
    let Some(device) = open_card() else { return };
    let Some(output) = pick_output(&device) else {
        drmkit_testkit::skipped("no connected output");
        return;
    };

    let sink = DumbScanoutSink::create(
        &device,
        output.crtc_id,
        output.crtc_index,
        output.connector_id,
        &output.mode,
        &Config::default(),
    )
    .expect("sink");

    assert_ne!(sink.format(), 0, "a caller renders in whatever this says");
    assert!(
        drmkit_fmt::format_bpp(sink.format()) != 0,
        "and must be able to size a buffer from it"
    );
}
