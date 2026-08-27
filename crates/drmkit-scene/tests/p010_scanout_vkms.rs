// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! A 10-bit planar framebuffer, offered to a real primary plane.
//!
//! Parity port of the scanout half of
//! `tests/integration/test_dumb_buffer_p010_vkms.cpp`. The allocation half is
//! in `drmkit-dumb`, which is where `create_planar` lives; this is the half
//! that needs a CRTC, a connector and a commit.
//!
//! A `TEST_ONLY` rather than a real modeset. The question is whether the
//! kernel accepts a `P010` framebuffer on the primary, and a test commit
//! answers exactly that without putting a 10-bit frame on someone's screen.

mod common;

use common::{card_guard, open_card};
use drm::control::Device as ControlDevice;
use drmkit_core::{AtomicCommitFlags, AtomicRequest, ObjectType, PropertyStore};
use drmkit_fmt::fourcc;
use drmkit_modeset::ModeInfo as _;

/// `P010` reaches the primary plane, or the driver says why not.
#[test]
#[ignore = "needs a DRM device and DRM master"]
fn a_p010_framebuffer_is_accepted_on_the_primary_vkms() {
    let _guard = card_guard();
    let Some(device) = open_card() else { return };

    let Ok(resources) = device.resource_handles() else {
        drmkit_testkit::skipped("no resources");
        return;
    };
    let Some((connector_id, crtc, mode)) = resources.connectors().iter().find_map(|handle| {
        let connector = device.get_connector(*handle, false).ok()?;
        if connector.state() != drm::control::connector::State::Connected {
            return None;
        }
        let mode = *connector.modes().first()?;
        let encoder = connector
            .encoders()
            .iter()
            .find_map(|e| device.get_encoder(*e).ok())?;
        let crtc = resources
            .filter_crtcs(encoder.possible_crtcs())
            .first()
            .copied()?;
        Some((u32::from(*handle), crtc, mode))
    }) else {
        drmkit_testkit::skipped("no connected output");
        return;
    };
    let crtc_id = u32::from(crtc);

    // The primary plane on that CRTC, and only if it takes P010. A driver
    // that does not advertise the format is not a failure of anything here.
    let Ok(planes) = device.plane_handles() else {
        drmkit_testkit::skipped("no planes");
        return;
    };
    let primary = planes.iter().find_map(|handle| {
        let info = device.get_plane(*handle).ok()?;
        let allowed = resources.filter_crtcs(info.possible_crtcs());
        if !allowed.contains(&crtc) || !info.formats().contains(&fourcc::P010) {
            return None;
        }
        Some(u32::from(*handle))
    });
    let Some(plane_id) = primary else {
        drmkit_testkit::skipped("no plane on this CRTC advertises P010");
        return;
    };

    let (width, height) = (mode.width(), mode.height());
    let buffer = match drmkit_dumb::Buffer::create_planar(&device, fourcc::P010, width, height) {
        Ok(buffer) => buffer,
        Err(error) => {
            drmkit_testkit::skipped(&format!("this driver refuses a P010 dumb buffer ({error})"));
            return;
        }
    };
    let fb_id = buffer.fb_id().expect("a registered framebuffer");

    let request = match arm_full_screen(&device, crtc_id, connector_id, plane_id, fb_id, &mode) {
        Ok(request) => request,
        Err(error) => panic!("arming the commit: {error}"),
    };

    let result = request.commit(
        &device,
        AtomicCommitFlags::TEST_ONLY | AtomicCommitFlags::ALLOW_MODESET,
    );

    assert!(
        result.is_ok(),
        "the plane advertises P010 and the kernel registered the framebuffer, \
         so a test commit that refuses it means the two disagree: {result:?}"
    );
}

/// Build a modeset plus one full-screen plane, ready to commit.
///
/// Split out because it is the bulk of the case and none of its substance:
/// every property here is one that any atomic driver exposes, and the
/// question the test asks is about the framebuffer's format, not about these.
fn arm_full_screen(
    device: &drmkit_core::Device,
    crtc_id: u32,
    connector_id: u32,
    plane_id: u32,
    fb_id: u32,
    mode: &drmkit_core::Mode,
) -> Result<AtomicRequest, drmkit_core::CoreError> {
    let mut store = PropertyStore::new();
    store.cache_properties(device, crtc_id, ObjectType::Crtc)?;
    store.cache_properties(device, connector_id, ObjectType::Connector)?;
    store.cache_properties(device, plane_id, ObjectType::Plane)?;

    // The blob outlives the request only because the caller commits before
    // dropping what this returns -- the same hazard `emit_frame` documents.
    let blob = device.create_property_blob(mode)?;
    let (width, height) = (mode.width(), mode.height());

    let mut request = AtomicRequest::with_capacity(16);
    let mut set = |object: u32, name: &str, value: u64| -> Result<(), drmkit_core::CoreError> {
        let id = store.property_id(object, name)?;
        request.add_property(object, id, value)
    };

    set(crtc_id, "MODE_ID", blob.id())?;
    set(crtc_id, "ACTIVE", 1)?;
    set(connector_id, "CRTC_ID", u64::from(crtc_id))?;
    set(plane_id, "FB_ID", u64::from(fb_id))?;
    set(plane_id, "CRTC_ID", u64::from(crtc_id))?;
    set(plane_id, "CRTC_X", 0)?;
    set(plane_id, "CRTC_Y", 0)?;
    set(plane_id, "CRTC_W", u64::from(width))?;
    set(plane_id, "CRTC_H", u64::from(height))?;
    set(plane_id, "SRC_X", 0)?;
    set(plane_id, "SRC_Y", 0)?;
    set(plane_id, "SRC_W", u64::from(width) << 16)?;
    set(plane_id, "SRC_H", u64::from(height) << 16)?;

    // The blob has to live until the commit. Leaking it for the length of a
    // test is the smaller evil against threading its lifetime through a
    // return type; the process exits moments later.
    std::mem::forget(blob);
    Ok(request)
}
