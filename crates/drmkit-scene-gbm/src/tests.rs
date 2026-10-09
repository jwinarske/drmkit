// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! Parity port of `tests/unit/test_gbm_surface_source.cpp` and
//! `tests/integration/test_gbm_surface_source_vkms.cpp` from drm-cxx @
//! `4a0b64a`.
//!
//! # What runs here, and what cannot
//!
//! **No case locks a front buffer.** Before a producer binds the surface,
//! `gbm_surface_lock_front_buffer` and `has_free_buffers` are a `SIGSEGV` in
//! Mesa, and drmkit creates no GL context, so nothing here binds one.
//! Upstream's cases do not lock either, for the same reason. Creating a
//! surface, pausing and resuming it, and the `require_bind` gate are all safe
//! without a producer, and run on any card with a GBM backend, vkms included.

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
        require_bind: false,
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

/// A surface reports the shape it was asked for.
///
/// Read without touching the surface: `has_free_buffers` would be the natural
/// check that it has somewhere to render, but before a producer binds it that
/// call is a `SIGSEGV` in Mesa.
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

/// `RequireBindGatesAcquireUntilBound`: with `require_bind`, an acquire before
/// the producer binds the surface is flow control rather than a call into it,
/// which in Mesa is a `SIGSEGV`. A resume rebuilds the surface, so the gate
/// closes again.
#[test]
fn require_bind_gates_acquire_until_bound() {
    let _guard = card_guard();
    let Some(device) = open_card() else { return };
    let config = SurfaceConfig {
        require_bind: true,
        ..good_config()
    };
    let Ok(mut source) = GbmSurfaceSource::create(&device, &config) else {
        drmkit_testkit::skipped("no GBM surface backend on this card");
        return;
    };

    assert!(
        matches!(source.acquire(), Err(SourceError::WouldBlock)),
        "nothing has bound the surface, so locking its front buffer would crash"
    );

    source.mark_bound();
    source.on_session_paused();
    source
        .on_session_resumed(&device)
        .expect("the same device is a device it can resume onto");
    assert!(
        matches!(source.acquire(), Err(SourceError::WouldBlock)),
        "the resume rebuilt the surface, and the producer has not bound the \
         new one"
    );
}
