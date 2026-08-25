// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! Output discovery against a real device.
//!
//! `#[ignore]`d and run in the device lanes with `--include-ignored`. Nothing
//! is committed and no DRM master is taken: discovery only reads.

use std::sync::Mutex;

use drm::control::Device as _;
use drm::control::connector::{Interface, State};
use drmkit_core::Device;
use drmkit_display::{ScanoutError, ScanoutTarget};
use drmkit_modeset::ModeInfo as _;
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
            println!("note: skipped -- no DRM device at {path} ({error})");
            return None;
        }
    };
    device.enable_atomic().expect("atomic");
    Some(device)
}

#[test]
#[ignore = "needs a DRM device"]
fn discovery_finds_an_output_that_is_actually_wired_up() {
    let _guard = card_guard();
    let Some(device) = open_card() else { return };

    let target = match ScanoutTarget::discover(&device).expect("discover") {
        Ok(target) => target,
        Err(ScanoutError::NothingConnected) => {
            assert!(
                std::env::var_os("DRMKIT_REQUIRE_MASTER").is_none(),
                "nothing connected, but DRMKIT_REQUIRE_MASTER is set"
            );
            println!("note: skipped -- no display attached");
            return;
        }
        Err(error) => panic!("{error}"),
    };

    println!(
        "connector {} -> crtc {} (index {}) -> plane {} at {}x{}@{}",
        target.connector_id,
        target.crtc.id,
        target.crtc.index,
        target.primary_plane_id,
        target.mode.width(),
        target.mode.height(),
        target.mode.refresh(),
    );

    // The connector it chose really has a display on it.
    let resources = device.resource_handles().expect("resources");
    let handle = resources
        .connectors()
        .iter()
        .find(|handle| u32::from(**handle) == target.connector_id)
        .expect("the connector is one of the device's");
    let connector = device.get_connector(*handle, false).expect("connector");
    assert_eq!(connector.state(), State::Connected);

    // The mode it chose is one that connector advertises.
    assert!(
        connector.modes().contains(&target.mode),
        "the chosen mode is not on the connector's list"
    );
    assert!(target.mode.width() > 0 && target.mode.height() > 0);

    // The CRTC index really addresses the CRTC id: these are the two halves
    // that get mixed up, and mixing them up offers planes to the wrong pipe.
    let crtc = resources.crtcs()[target.crtc.index as usize];
    assert_eq!(u32::from(crtc), target.crtc.id);

    // And the plane is a primary that can feed that CRTC.
    let registry = PlaneRegistry::probe(&device).expect("probe");
    let primary = registry
        .all()
        .iter()
        .find(|plane| plane.id == target.primary_plane_id)
        .expect("the plane is one of the device's");
    assert_eq!(primary.plane_type, PlaneType::Primary);
    assert!(
        primary.compatible_with_crtc(target.crtc.index),
        "plane {} cannot feed CRTC index {}",
        primary.id,
        target.crtc.index
    );

    assert!(
        !target.primary_formats.is_empty(),
        "a plane that can scan out nothing cannot scan out"
    );
}

#[test]
#[ignore = "needs a DRM device"]
fn discovery_enables_universal_planes_itself() {
    // Deliberately *without* the atomic capability, which is the only way to
    // see this work. Measured on amdgpu: a client with neither capability
    // sees 2 planes, and with either one sees 10 -- the kernel turns on
    // universal planes when a client goes atomic. So an atomic caller can
    // never tell whether discovery asked for it, and a caller that has not
    // gone atomic yet is exactly who would get `NoPrimaryPlane` on a
    // perfectly good device.
    let _guard = card_guard();
    let path = std::env::var("DRMKIT_TEST_CARD").unwrap_or_else(|_| "/dev/dri/card0".to_owned());
    let Ok(device) = Device::open(&path) else {
        println!("note: skipped -- no DRM device at {path}");
        return;
    };

    match ScanoutTarget::discover(&device).expect("discover") {
        Ok(target) => assert!(target.primary_plane_id > 0),
        Err(ScanoutError::NothingConnected) => {
            assert!(
                std::env::var_os("DRMKIT_REQUIRE_MASTER").is_none(),
                "nothing connected, but DRMKIT_REQUIRE_MASTER is set"
            );
            println!("note: skipped -- no display attached");
        }
        Err(error) => panic!("{error}"),
    }
}

#[test]
#[ignore = "needs a DRM device"]
fn the_chosen_crtc_is_one_the_connectors_encoders_allow() {
    // The cold-start path: a CRTC the encoders do not allow is rejected by
    // the kernel at modeset, after the caller has built a whole frame for it.
    let _guard = card_guard();
    let Some(device) = open_card() else { return };
    device.enable_universal_planes().expect("universal planes");

    let resources = device.resource_handles().expect("resources");
    let Ok(Ok(target)) = ScanoutTarget::discover(&device) else {
        println!("note: skipped -- no display attached");
        return;
    };

    let handle = resources
        .connectors()
        .iter()
        .find(|handle| u32::from(**handle) == target.connector_id)
        .expect("the connector is one of the device's");
    let connector = device.get_connector(*handle, false).expect("connector");

    let allowed: Vec<u32> = connector
        .encoders()
        .iter()
        .filter_map(|encoder| device.get_encoder(*encoder).ok())
        .flat_map(|encoder| resources.filter_crtcs(encoder.possible_crtcs()))
        .map(u32::from)
        .collect();
    assert!(
        allowed.contains(&target.crtc.id),
        "crtc {} is not among {allowed:?}",
        target.crtc.id
    );
}

#[test]
#[ignore = "needs a DRM device"]
fn a_rank_that_matches_nothing_still_finds_the_display() {
    // The rank is a preference, not a filter. A caller asking for an internal
    // panel on a machine that has none should still get a picture -- the
    // alternative is a board that reports itself headless with a monitor
    // plugged into it.
    let _guard = card_guard();
    let Some(device) = open_card() else { return };

    let Ok(Ok(preferred)) = ScanoutTarget::discover(&device) else {
        println!("note: skipped -- no display attached");
        return;
    };

    // Composite and TV are in none of the rank lists and on none of these
    // cards, so this exercises the fallback rather than a match.
    let ranks = [Interface::Composite, Interface::TV];
    let fallback = ScanoutTarget::discover_ranked(&device, &ranks)
        .expect("discover")
        .expect("a display is attached");

    assert_eq!(fallback.connector_id, preferred.connector_id);
    assert_eq!(fallback.crtc.id, preferred.crtc.id);
}

#[test]
#[ignore = "needs a DRM device"]
fn the_rank_decides_which_display_when_there_is_a_choice() {
    // Only meaningful with two displays of different kinds attached, which is
    // this machine and not the lane -- so it says which it was rather than
    // asserting into a single-output vacuum.
    let _guard = card_guard();
    let Some(device) = open_card() else { return };
    let resources = device.resource_handles().expect("resources");

    let kinds: Vec<Interface> = resources
        .connectors()
        .iter()
        .filter_map(|handle| device.get_connector(*handle, false).ok())
        .filter(|info| info.state() == State::Connected && !info.modes().is_empty())
        .map(|info| info.interface())
        .collect();
    if kinds.len() < 2 {
        println!("note: one display attached ({kinds:?}), nothing to choose between");
        return;
    }

    // Reverse the main rank: whatever it preferred, the other one should win.
    let mut reversed: Vec<Interface> = drmkit_display::rank::MAIN.to_vec();
    reversed.reverse();

    let forward = ScanoutTarget::discover(&device)
        .expect("discover")
        .expect("a display is attached");
    let backward = ScanoutTarget::discover_ranked(&device, &reversed)
        .expect("discover")
        .expect("a display is attached");

    println!(
        "{kinds:?}: main picked connector {}, reversed picked {}",
        forward.connector_id, backward.connector_id
    );
    assert_ne!(
        forward.connector_id, backward.connector_id,
        "reversing the rank changed nothing, so the rank is not being read"
    );
}
