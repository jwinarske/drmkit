// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! Parity port of `tests/unit/test_gbm_surface_source.cpp` and
//! `tests/integration/test_gbm_surface_source_vkms.cpp` from drm-cxx @
//! `4a0b64a`.
//!
//! # What runs here, and what cannot
//!
//! **No case locks a front buffer.** `gbm_surface_lock_front_buffer` is
//! undefined before the first `eglSwapBuffers`, and drmkit creates no GL
//! context — so the acquire path needs a producer that does not exist here.
//! Upstream's cases do not lock either, for the same reason.
//!
//! **No case creates a surface, either**, on the hardware this was written
//! against. Neither card here has a working DRI backend: vkms has no render
//! node, and the amdgpu node's `ACCEL_WORKING` query is refused. Mesa gives
//! both the minimal backend, where creating a surface *succeeds* and touching
//! it segfaults. So what the device cases pin is the refusal — that drmkit
//! declines rather than handing back a crashing handle — and the rest of the
//! surface's behaviour has no coverage anywhere. Recorded as P-38.

use std::sync::{Mutex, MutexGuard};

use drmkit_core::Device;
use drmkit_fmt::fourcc;
use drmkit_scene::{LayerBufferSource, SourceError};

use crate::{GbmSurfaceSource, SurfaceConfig, SurfaceError};

/// GBM opens the same node, so these serialize with everything else that does.
static CARD_LOCK: Mutex<()> = Mutex::new(());

fn card_guard() -> MutexGuard<'static, ()> {
    CARD_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn open_card() -> Option<Device> {
    let path = std::env::var("DRMKIT_TEST_CARD").unwrap_or_else(|_| "/dev/dri/card0".to_owned());
    drmkit_testkit::announce_card(&path);
    match Device::open(&path) {
        Ok(device) => Some(device),
        Err(error) => {
            assert!(
                std::env::var_os("DRMKIT_REQUIRE_MASTER").is_none(),
                "{path}: {error}, but DRMKIT_REQUIRE_MASTER is set"
            );
            drmkit_testkit::skipped(&format!("no DRM device at {path} ({error})"));
            None
        }
    }
}

fn good_config() -> SurfaceConfig {
    SurfaceConfig {
        width: 64,
        height: 64,
        fourcc: fourcc::XRGB8888,
        modifier: None,
    }
}

/// A zero dimension or format is refused, and says which.
///
/// Upstream has one case per field. They are one here because the interesting
/// part is not that each fails but that each names *itself* — a guard
/// reporting "invalid argument" leaves the caller to bisect its own config.
#[test]
fn a_zero_dimension_or_format_is_refused_by_name() {
    let _guard = card_guard();
    let Some(device) = open_card() else { return };

    for (field, config) in [
        (
            "width",
            SurfaceConfig {
                width: 0,
                ..good_config()
            },
        ),
        (
            "height",
            SurfaceConfig {
                height: 0,
                ..good_config()
            },
        ),
        (
            "fourcc",
            SurfaceConfig {
                fourcc: 0,
                ..good_config()
            },
        ),
    ] {
        let error = GbmSurfaceSource::create(&device, &config)
            .expect_err("a zero {field} is not a surface");
        assert!(
            matches!(error, SurfaceError::Invalid { field: named } if named == field),
            "asked about {field} and got {error}"
        );
    }
}

/// A format GBM cannot name is refused before the driver sees it.
///
/// `gbm_surface_create` reports an unknown format the same way it reports a
/// driver refusal, and a caller can do something about only one of them:
/// pick another format, versus give up on this device.
#[test]
fn a_format_gbm_does_not_know_is_refused_before_the_driver_sees_it() {
    let _guard = card_guard();
    let Some(device) = open_card() else { return };

    let error = GbmSurfaceSource::create(
        &device,
        &SurfaceConfig {
            fourcc: 0xDEAD_BEEF,
            ..good_config()
        },
    )
    .expect_err("no driver knows that format");

    assert!(
        matches!(error, SurfaceError::UnsupportedFormat { fourcc } if fourcc == 0xDEAD_BEEF),
        "got {error}, which does not say the format was the problem"
    );
}

/// A device that cannot make surfaces is refused, not handed a crashing one.
///
/// This is the case that matters most here, because the failure it prevents
/// is not an error return — it is `SIGSEGV` on the caller's next line.
#[test]
fn a_device_without_surface_support_is_refused() {
    let _guard = card_guard();
    let Some(device) = open_card() else { return };
    let gbm = drmkit_gbm::GbmDevice::new(&device).expect("gbm device");

    if gbm.supports_surfaces() {
        drmkit_testkit::skipped("this card has a render node; the refusal path needs one without");
        return;
    }

    let error = GbmSurfaceSource::create(&device, &good_config())
        .expect_err("a device with no surface backend must be refused here");
    assert!(
        matches!(error, SurfaceError::Create(_)),
        "got {error}, which does not tell a caller to stop asking for surfaces \
         on this device"
    );
}

/// A surface, where one can be had.
///
/// Skips everywhere the hardware cannot support one, which on the machine
/// this was written against is everywhere. Left in because it is the case
/// that will run first on a board with a working GPU, and because a skip that
/// says why is worth more than a case that is not there.
#[test]
fn a_surface_reports_the_shape_it_was_asked_for() {
    let _guard = card_guard();
    let Some(device) = open_card() else { return };
    let Ok(source) = GbmSurfaceSource::create(&device, &good_config()) else {
        drmkit_testkit::skipped("no working GBM surface backend on this card");
        return;
    };

    let format = LayerBufferSource::format(&source);
    assert_eq!(format.width, 64);
    assert_eq!(format.height, 64);
    assert_eq!(format.fourcc, fourcc::XRGB8888);
    assert!(
        source.has_free_buffers(),
        "a fresh surface must have somewhere to render, or the producer's \
         first swap has nowhere to go"
    );
}

/// The pixels live where the CPU cannot see them.
///
/// A surface is a render target, so `map` is `Unsupported` — and composition
/// reaching for it is how a caller learns to use the DMA-BUF path instead.
#[test]
fn a_surface_has_no_cpu_mapping() {
    let _guard = card_guard();
    let Some(device) = open_card() else { return };
    let Ok(mut source) = GbmSurfaceSource::create(&device, &good_config()) else {
        drmkit_testkit::skipped("no working GBM surface backend on this card");
        return;
    };

    assert!(matches!(
        source.map(drmkit_dumb::MapAccess::Read),
        Err(SourceError::Unsupported)
    ));
}

/// Releasing a token that was never handed out is ignored.
///
/// The scene releases on teardown as well as after a commit, so a token can
/// arrive twice. Treating the second as a real release would hand the same
/// buffer back to the surface twice and the producer would render into one
/// the display engine is still reading.
#[test]
fn releasing_an_unknown_token_is_ignored() {
    let _guard = card_guard();
    let Some(device) = open_card() else { return };
    let Ok(mut source) = GbmSurfaceSource::create(&device, &good_config()) else {
        drmkit_testkit::skipped("no working GBM surface backend on this card");
        return;
    };

    source.release(drmkit_scene::AcquiredBuffer {
        fb_id: 0,
        token: 42,
        acquire_fence: None,
        damage: Vec::new(),
    });
    source.release(drmkit_scene::AcquiredBuffer::default());
}

/// Pausing forgets the framebuffers and stops handing buffers out.
///
/// The ids were registered on a descriptor that is about to stop working.
/// Committing one afterwards is what takes a whole frame down, so `acquire`
/// reports flow control instead — a paused session has no frame, which is a
/// thing the scene already knows how to wait for.
#[test]
fn pausing_the_session_stops_the_source_handing_anything_out() {
    let _guard = card_guard();
    let Some(device) = open_card() else { return };
    let Ok(mut source) = GbmSurfaceSource::create(&device, &good_config()) else {
        drmkit_testkit::skipped("no working GBM surface backend on this card");
        return;
    };

    source.on_session_paused();

    assert!(
        matches!(source.acquire(), Err(SourceError::WouldBlock)),
        "a paused source has no frame; reporting a real failure would abort \
         the commit rather than skipping the layer"
    );
}

/// Resuming rebuilds against the new device.
///
/// The surface goes too, not just the framebuffers: it was created against a
/// `gbm_device` wrapping the dead descriptor, and buffers allocated through
/// that cannot be registered on the new one.
#[test]
fn resuming_rebuilds_the_surface_against_the_new_device() {
    let _guard = card_guard();
    let Some(device) = open_card() else { return };
    let Ok(mut source) = GbmSurfaceSource::create(&device, &good_config()) else {
        drmkit_testkit::skipped("no working GBM surface backend on this card");
        return;
    };
    let before = LayerBufferSource::format(&source);

    source.on_session_paused();
    source
        .on_session_resumed(&device)
        .expect("the same device is a device it can resume onto");

    assert_eq!(
        LayerBufferSource::format(&source),
        before,
        "callers rely on the shape surviving a resume"
    );
}
