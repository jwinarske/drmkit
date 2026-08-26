// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! Reading planes off a real device.
//!
//! `#[ignore]`d and run in the device lanes with `--include-ignored`. No DRM
//! master is taken: reading a plane's properties does not need it.

use std::sync::Mutex;

use drm::control::Device as _;
use drmkit_core::{Device, ObjectType, PropertyStore};
use drmkit_planes::{PlaneRegistry, PlaneType};

static CARD_LOCK: Mutex<()> = Mutex::new(());

fn card_guard() -> std::sync::MutexGuard<'static, ()> {
    CARD_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn open_card() -> Option<Device> {
    let path = std::env::var("DRMKIT_TEST_CARD").unwrap_or_else(|_| "/dev/dri/card0".to_owned());
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
    device.enable_universal_planes().expect("universal planes");
    device.enable_atomic().expect("atomic");
    Some(device)
}

#[test]
#[ignore = "needs a DRM device"]
fn every_plane_the_kernel_lists_is_read() {
    let _guard = card_guard();
    let Some(device) = open_card() else { return };

    let expected = device.plane_handles().expect("planes").len();
    let registry = PlaneRegistry::probe(&device).expect("probe");
    assert_eq!(registry.all().len(), expected);
    assert!(expected > 0, "a device with no planes cannot display");

    // A lane that means to exercise the allocator says how many planes it
    // needs to do that. Without this the vkms lane silently dropped to one
    // plane per CRTC when the module's overlay default changed, and a dozen
    // rigs went on passing while testing nothing about placement.
    if let Ok(minimum) = std::env::var("DRMKIT_MIN_PLANES") {
        let minimum: usize = minimum.parse().expect("DRMKIT_MIN_PLANES is a number");
        assert!(
            expected >= minimum,
            "this device has {expected} plane(s) and DRMKIT_MIN_PLANES asks for {minimum}"
        );
    }

    // Universal planes is enabled above, so a primary must be among them --
    // its absence is the symptom of a caller that forgot the capability, and
    // reads as a device that cannot put anything on screen.
    assert!(
        registry
            .all()
            .iter()
            .any(|plane| plane.plane_type == PlaneType::Primary),
        "no primary plane: was DRM_CLIENT_CAP_UNIVERSAL_PLANES set?"
    );
}

#[test]
#[ignore = "needs a DRM device"]
fn a_probe_agrees_with_the_property_set() {
    let _guard = card_guard();
    let Some(device) = open_card() else { return };
    let registry = PlaneRegistry::probe(&device).expect("probe");

    for plane in registry.all() {
        let mut properties = PropertyStore::new();
        properties
            .cache_properties(&device, plane.id, ObjectType::Plane)
            .expect("cache");
        let present = |name: &str| properties.property_id(plane.id, name).is_ok();

        assert_eq!(plane.supports_rotation, present("rotation"), "{}", plane.id);
        assert_eq!(
            plane.has_pixel_blend_mode,
            present("pixel blend mode"),
            "{}",
            plane.id
        );
        assert_eq!(plane.has_per_plane_alpha, present("alpha"), "{}", plane.id);
        assert_eq!(
            plane.has_color_encoding,
            present("COLOR_ENCODING"),
            "{}",
            plane.id
        );
        assert_eq!(
            plane.has_color_range,
            present("COLOR_RANGE"),
            "{}",
            plane.id
        );

        // A cached enum integer must be the one the driver assigned. Writing
        // another plane's integer is a wrong blend mode or a wrong YCbCr
        // matrix -- a visible defect that no ioctl reports.
        if plane.has_pixel_blend_mode {
            assert_eq!(
                plane.blend_modes.premultiplied,
                properties
                    .enum_value(&device, plane.id, "pixel blend mode", "Pre-multiplied")
                    .ok(),
                "{}",
                plane.id
            );
        }
        if plane.has_color_encoding {
            assert_eq!(
                plane.color_encodings.bt709,
                properties
                    .enum_value(&device, plane.id, "COLOR_ENCODING", "ITU-R BT.709 YCbCr")
                    .ok(),
                "{}",
                plane.id
            );
        }
    }
}

#[test]
#[ignore = "needs a DRM device"]
fn a_planes_crtc_mask_matches_what_the_kernel_says() {
    // The mask indexes the resource CRTC order, and getting the indexing wrong
    // gives an allocator that offers planes to a CRTC that cannot drive them
    // -- every test commit refused, and a frame composited for no reason.
    let _guard = card_guard();
    let Some(device) = open_card() else { return };
    let resources = device.resource_handles().expect("resources");
    let registry = PlaneRegistry::probe(&device).expect("probe");

    for plane in registry.all() {
        let handle = drm::control::plane::Handle::from(
            drm::control::RawResourceHandle::new(plane.id).expect("a plane id is not zero"),
        );
        let info = device.get_plane(handle).expect("plane");
        let allowed = resources.filter_crtcs(info.possible_crtcs());

        for (index, crtc) in resources.crtcs().iter().enumerate() {
            let index = u32::try_from(index).expect("a CRTC index fits in a u32");
            assert_eq!(
                plane.compatible_with_crtc(index),
                allowed.contains(crtc),
                "plane {} against CRTC index {index}",
                plane.id
            );
        }
    }
}

#[test]
#[ignore = "needs a DRM device"]
fn a_rotation_capable_plane_reports_which_rotations() {
    // The bits, not the property: a plane can expose `rotation` and implement
    // only 0/180 or a reflect, and a 90-degree request on one of those is
    // dropped by the driver with the commit still succeeding.
    let _guard = card_guard();
    let Some(device) = open_card() else { return };
    let registry = PlaneRegistry::probe(&device).expect("probe");

    let mut rotatable = 0;
    for plane in registry.all() {
        if !plane.supports_rotation {
            assert_eq!(plane.rotation_bits, 0, "plane {}", plane.id);
            continue;
        }
        rotatable += 1;

        // Counted against the property's own enum table, read here through a
        // raw ioctl rather than through the helper under test. Exact rather
        // than a sanity check: a helper that OR'd the entries' *values*
        // instead of shifting by them would still set bit 0 and still look
        // plausible, which is what a weaker assertion lets through.
        let advertised = rotation_entry_count(&device, plane.id);
        assert_eq!(
            plane.rotation_bits.count_ones(),
            advertised,
            "plane {} advertises {advertised} rotations but reports {:#x}",
            plane.id,
            plane.rotation_bits
        );
        // Every bit must be one the kernel defines: ROTATE_0/90/180/270 then
        // REFLECT_X/Y, bits 0 to 5.
        assert_eq!(
            plane.rotation_bits & !0x3f,
            0,
            "plane {} advertises a bit outside DRM_MODE_ROTATE/REFLECT: {:#x}",
            plane.id,
            plane.rotation_bits
        );
        println!(
            "plane {} rotation bits {:#x}",
            plane.id, plane.rotation_bits
        );
    }
    println!("{rotatable} of {} planes rotate", registry.all().len());
}

#[test]
#[ignore = "needs a DRM device"]
fn a_plane_without_in_formats_still_has_a_format_table() {
    // Older and simpler display controllers expose no IN_FORMATS blob. The
    // synthesized LINEAR-only table is what keeps them usable rather than
    // looking like planes that accept no format at all.
    let _guard = card_guard();
    let Some(device) = open_card() else { return };
    let registry = PlaneRegistry::probe(&device).expect("probe");

    for plane in registry.all() {
        assert!(
            !plane.formats.is_empty(),
            "plane {} advertises no format at all",
            plane.id
        );
        assert!(
            !plane.format_table.is_empty(),
            "plane {} has no queryable format table",
            plane.id
        );
        if !plane.has_format_modifiers {
            println!("plane {}: no IN_FORMATS, LINEAR synthesized", plane.id);
        }
    }
}

/// How many entries the plane's `rotation` property advertises.
///
/// Read straight from the ioctl so it is an independent answer to the one
/// `PropertyStore::bitmask_bits` gives.
fn rotation_entry_count(device: &Device, plane_id: u32) -> u32 {
    use std::os::fd::AsFd as _;

    let mut properties = PropertyStore::new();
    properties
        .cache_properties(device, plane_id, ObjectType::Plane)
        .expect("cache");
    let id = properties
        .property_id(plane_id, "rotation")
        .expect("the caller checked it is there");

    let mut values = Vec::new();
    let mut enums = Vec::new();
    drm_ffi::mode::get_property(device.as_fd(), id, Some(&mut values), Some(&mut enums))
        .expect("get_property");
    u32::try_from(enums.len()).expect("a property has few entries")
}
