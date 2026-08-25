// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! Staging a cursor onto a real plane.
//!
//! The arithmetic is tested in the crate; what needs a device is the property
//! set itself — that every name resolves, and that the values land in the
//! right units. Nothing here commits: the request is inspected instead, so
//! the test says what was written rather than only that the kernel tolerated
//! it.

use drm::control::Device as _;
use drmkit_core::{AtomicRequest, Device, ObjectType, PropertyStore};
use drmkit_cursor::{PlaneState, Rotation, stage};

/// Serialises the tests that open the card. See the note in
/// `drmkit-scene/tests/composition_vkms.rs`: the lane runs binaries in turn
/// but tests inside one in parallel.
static CARD_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn card_guard() -> std::sync::MutexGuard<'static, ()> {
    CARD_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// A device with the atomic cap, and one plane's properties cached.
fn fixture() -> Option<(Device, PropertyStore, u32)> {
    let path = std::env::var("DRMKIT_TEST_CARD").unwrap_or_else(|_| "/dev/dri/card0".to_owned());
    let Ok(device) = Device::open(&path) else {
        println!("note: skipped -- {path} did not open");
        return None;
    };
    device.enable_universal_planes().expect("universal planes");
    // Without this the kernel hides CRTC_X and friends, and every lookup below
    // fails for a reason that has nothing to do with the code.
    device.enable_atomic().expect("atomic");

    // Nothing here commits, so nothing here needs DRM master -- but opening a
    // node nobody holds makes you master, and `cargo test` runs test binaries
    // in parallel. Holding it would deny it to the suites that do need it, and
    // they would fail intermittently for a reason nowhere near their own code.
    // (The vkms lane runs binaries in turn, so it would have hidden this.)
    let _ = device.drop_master();

    let plane = (*device.plane_handles().expect("planes").first()?).into();
    let mut props = PropertyStore::new();
    props
        .cache_properties(&device, plane, ObjectType::Plane)
        .expect("plane properties");
    Some((device, props, plane))
}

fn state(plane_id: u32) -> PlaneState {
    PlaneState {
        plane_id,
        crtc_id: 1,
        fb_id: 2,
        width: 64,
        height: 64,
        pointer_x: 100,
        pointer_y: 200,
        hotspot: Some((4, 7)),
        rotation: Rotation::None,
        rotation_mask: 0,
    }
}

/// The value written for `name`, if any.
fn written(request: &AtomicRequest, props: &PropertyStore, plane: u32, name: &str) -> Option<u64> {
    let id = props.property_id(plane, name).ok()?;
    request
        .writes()
        .iter()
        .find(|w| w.object_id == plane && w.property_id == id)
        .map(|w| w.value)
}

#[test]
#[ignore = "needs a DRM device"]
fn every_required_property_is_written_vkms() {
    let _guard = card_guard();
    let Some((_device, props, plane)) = fixture() else {
        return;
    };
    let mut request = AtomicRequest::new();
    let count = stage(&mut request, &props, &state(plane)).expect("stage");

    // The ten every plane must have. A driver missing one of these is broken,
    // and the caller finds out here rather than at commit time.
    for name in [
        "FB_ID", "CRTC_ID", "CRTC_X", "CRTC_Y", "CRTC_W", "CRTC_H", "SRC_X", "SRC_Y", "SRC_W",
        "SRC_H",
    ] {
        assert!(
            written(&request, &props, plane, name).is_some(),
            "{name} was not written"
        );
    }
    assert!(count >= 10, "wrote only {count} properties");
}

#[test]
#[ignore = "needs a DRM device"]
fn the_source_rect_is_fixed_point_and_the_crtc_rect_is_not_vkms() {
    // Sending either in the other's units gives a cursor that fills the screen
    // or vanishes, and both look like a driver problem.
    let _guard = card_guard();
    let Some((_device, props, plane)) = fixture() else {
        return;
    };
    let mut request = AtomicRequest::new();
    stage(&mut request, &props, &state(plane)).expect("stage");

    assert_eq!(written(&request, &props, plane, "CRTC_W"), Some(64));
    assert_eq!(written(&request, &props, plane, "CRTC_H"), Some(64));
    assert_eq!(written(&request, &props, plane, "SRC_W"), Some(64 << 16));
    assert_eq!(written(&request, &props, plane, "SRC_H"), Some(64 << 16));
    assert_eq!(written(&request, &props, plane, "SRC_X"), Some(0));
    assert_eq!(written(&request, &props, plane, "SRC_Y"), Some(0));
}

#[test]
#[ignore = "needs a DRM device"]
fn the_position_is_offset_by_the_hotspot_vkms() {
    let _guard = card_guard();
    let Some((_device, props, plane)) = fixture() else {
        return;
    };
    let mut request = AtomicRequest::new();
    stage(&mut request, &props, &state(plane)).expect("stage");

    assert_eq!(written(&request, &props, plane, "CRTC_X"), Some(96));
    assert_eq!(written(&request, &props, plane, "CRTC_Y"), Some(193));
}

#[test]
#[ignore = "needs a DRM device"]
fn a_negative_position_is_written_as_twos_complement_vkms() {
    // CRTC_X is signed and carried in an unsigned slot. A cursor at the top
    // left corner has its buffer hanging off the edge, and the kernel reads
    // the value back as the negative it is.
    let _guard = card_guard();
    let Some((_device, props, plane)) = fixture() else {
        return;
    };
    let mut request = AtomicRequest::new();
    let mut placement = state(plane);
    placement.pointer_x = 1;
    placement.pointer_y = 1;
    placement.hotspot = Some((4, 4));
    stage(&mut request, &props, &placement).expect("stage");

    let x = written(&request, &props, plane, "CRTC_X").expect("CRTC_X");
    assert_eq!(
        u32::try_from(x).expect("fits").cast_signed(),
        -3,
        "written as {x:#x}, which should read back as -3"
    );
}

#[test]
#[ignore = "needs a DRM device"]
fn the_hotspot_is_left_alone_until_something_is_drawn_vkms() {
    // Before a cursor is set the buffer exists but holds nothing, and writing
    // a zero hotspot would clobber whatever the previous client left with a
    // value that is not true yet.
    //
    // HOTSPOT_X/Y exist only on virtualized drivers -- virtio-gpu, vmwgfx,
    // qxl -- which need to know where inside the sprite the pointer is so the
    // host can place its own. vkms, vc4 and rockchip all lack them, so this
    // skips everywhere drmkit is currently tested. It is here so that it runs
    // the day someone points the suite at a VM, which is exactly when the
    // property starts mattering.
    let _guard = card_guard();
    let Some((_device, props, plane)) = fixture() else {
        return;
    };
    if props.property_id(plane, "HOTSPOT_X").is_err() {
        println!("note: skipped -- no HOTSPOT_X (expected: not a virtualized driver)");
        return;
    }

    let mut request = AtomicRequest::new();
    let mut placement = state(plane);
    placement.hotspot = None;
    stage(&mut request, &props, &placement).expect("stage");
    assert_eq!(written(&request, &props, plane, "HOTSPOT_X"), None);

    let mut request = AtomicRequest::new();
    stage(&mut request, &props, &state(plane)).expect("stage");
    assert_eq!(written(&request, &props, plane, "HOTSPOT_X"), Some(4));
    assert_eq!(written(&request, &props, plane, "HOTSPOT_Y"), Some(7));
}

#[test]
#[ignore = "needs a DRM device"]
fn a_rotation_the_plane_cannot_do_is_not_asked_for_vkms() {
    let _guard = card_guard();
    let Some((_device, props, plane)) = fixture() else {
        return;
    };
    if props.property_id(plane, "rotation").is_err() {
        println!("note: skipped -- this plane has no rotation property");
        return;
    }

    let mut request = AtomicRequest::new();
    let mut placement = state(plane);
    placement.rotation = Rotation::Quarter;
    placement.rotation_mask = 0; // the plane advertises nothing
    stage(&mut request, &props, &placement).expect("stage");
    assert_eq!(
        written(&request, &props, plane, "rotation"),
        Some(Rotation::None.drm_mask()),
        "the pixels were turned in software; the hardware must not turn them again"
    );

    let mut request = AtomicRequest::new();
    placement.rotation_mask = Rotation::Quarter.drm_mask();
    stage(&mut request, &props, &placement).expect("stage");
    assert_eq!(
        written(&request, &props, plane, "rotation"),
        Some(Rotation::Quarter.drm_mask())
    );
}
