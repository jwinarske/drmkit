// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! Connector and CRTC capability probing, against whatever card is there.
//!
//! Every case is `#[ignore]`d and runs in the device lanes with
//! `--include-ignored`. None of them takes DRM master: reading a property set
//! does not need it, and taking it would blank a running display.
//!
//! What can be asserted here is not what the capabilities are -- those differ
//! per driver, which is the whole reason for the cache -- but that the probe
//! agrees with a raw read of the same properties, and that the derived
//! answers follow from the parts.

use std::sync::Mutex;

use drm::control::Device as _;
use drmkit_core::{Device, ObjectType, PropertyStore};
use drmkit_display::{BroadcastRgb, Colorspace, ConnectorCapabilities, CrtcCapabilities};

/// Opening the card is serialized with every other device test in the tree:
/// an open file description holding the card can deny master to another.
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
    // Some of these are atomic-only, and a client that has not set the cap is
    // not shown them -- which is how a probe reports a capable connector as
    // having nothing.
    device.enable_atomic().expect("atomic");
    Some(device)
}

#[test]
#[ignore = "needs a DRM device"]
fn a_connector_probe_agrees_with_the_property_set() {
    let _guard = card_guard();
    let Some(device) = open_card() else { return };
    let resources = device.resource_handles().expect("resources");

    let mut probed = 0;
    for handle in resources.connectors() {
        let id: u32 = (*handle).into();
        let capabilities = ConnectorCapabilities::probe(&device, id).expect("probe");
        probed += 1;

        let mut properties = PropertyStore::new();
        properties
            .cache_properties(&device, id, ObjectType::Connector)
            .expect("cache");

        assert_eq!(
            capabilities.has_hdr_output_metadata,
            properties.property_id(id, "HDR_OUTPUT_METADATA").is_ok(),
            "connector {id}: HDR_OUTPUT_METADATA"
        );
        assert_eq!(
            capabilities.max_bpc.is_some(),
            properties.range(id, "max bpc").ok().flatten().is_some(),
            "connector {id}: max bpc"
        );
        if let Some(bpc) = capabilities.max_bpc {
            assert!(bpc.min <= bpc.max, "connector {id}: {bpc:?}");
            assert!(
                bpc.min <= bpc.current && bpc.current <= bpc.max,
                "connector {id}: current outside its own range: {bpc:?}"
            );
        }

        // Every entry the probe cached must round-trip: the driver's integer
        // for a name is the whole point of caching it.
        for entry in Colorspace::ALL {
            let live = properties
                .enum_value(&device, id, "Colorspace", entry.name())
                .ok();
            assert_eq!(
                capabilities.colorspace(entry),
                live,
                "connector {id}: Colorspace {}",
                entry.name()
            );
        }
        for entry in BroadcastRgb::ALL {
            let live = properties
                .enum_value(&device, id, "Broadcast RGB", entry.name())
                .ok();
            assert_eq!(
                capabilities.broadcast_rgb(entry),
                live,
                "connector {id}: Broadcast RGB {}",
                entry.name()
            );
        }

        println!(
            "connector {id}: colorspace {} broadcast_rgb {} max_bpc {:?} hdr {} -> can_signal_hdr {}",
            capabilities.has_colorspace(),
            capabilities.has_broadcast_rgb(),
            capabilities.max_bpc,
            capabilities.has_hdr_output_metadata,
            capabilities.can_signal_hdr(),
        );
    }
    assert!(probed > 0, "a card with no connectors cannot display");
}

#[test]
#[ignore = "needs a DRM device"]
fn a_colorspace_property_always_carries_its_default() {
    // The failure this guards against has no error attached: writing an
    // integer the driver assigned to a different name tells the sink the
    // pixels mean something else, and the picture comes out in the wrong
    // gamut with every ioctl succeeding.
    let _guard = card_guard();
    let Some(device) = open_card() else { return };
    let resources = device.resource_handles().expect("resources");

    for handle in resources.connectors() {
        let id: u32 = (*handle).into();
        let capabilities = ConnectorCapabilities::probe(&device, id).expect("probe");
        if !capabilities.has_colorspace() {
            continue;
        }
        assert!(
            capabilities.colorspace(Colorspace::Default).is_some(),
            "connector {id} has Colorspace but not its Default entry"
        );
    }
}

#[test]
#[ignore = "needs a DRM device"]
fn a_crtc_probe_agrees_with_the_property_set() {
    let _guard = card_guard();
    let Some(device) = open_card() else { return };
    let resources = device.resource_handles().expect("resources");

    let mut probed = 0;
    for handle in resources.crtcs() {
        let id: u32 = (*handle).into();
        let capabilities = CrtcCapabilities::probe(&device, id).expect("probe");
        probed += 1;

        let mut properties = PropertyStore::new();
        properties
            .cache_properties(&device, id, ObjectType::Crtc)
            .expect("cache");

        for (present, name) in [
            (capabilities.has_degamma_lut, "DEGAMMA_LUT"),
            (capabilities.has_ctm, "CTM"),
            (capabilities.has_gamma_lut, "GAMMA_LUT"),
        ] {
            assert_eq!(
                present,
                properties.property_id(id, name).is_ok(),
                "crtc {id}: {name}"
            );
        }

        // A stage whose LUT length the driver never states cannot be
        // programmed, so the two have to travel together.
        if capabilities.has_degamma_lut {
            assert!(
                capabilities.degamma_lut_size > 0,
                "crtc {id}: DEGAMMA_LUT with no size"
            );
        }
        if capabilities.has_gamma_lut {
            assert!(
                capabilities.gamma_lut_size > 0,
                "crtc {id}: GAMMA_LUT with no size"
            );
        }

        println!(
            "crtc {id}: degamma {} ({}) ctm {} gamma {} ({}) -> full {}",
            capabilities.has_degamma_lut,
            capabilities.degamma_lut_size,
            capabilities.has_ctm,
            capabilities.has_gamma_lut,
            capabilities.gamma_lut_size,
            capabilities.has_full_pipeline(),
        );
    }
    assert!(probed > 0, "a card with no CRTC cannot display");
}
