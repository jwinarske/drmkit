// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! Connector enumeration, against whatever card is there.
//!
//! `#[ignore]`d and run in the device lanes with `--include-ignored`. No DRM
//! master is taken: listing connectors does not need it.

use std::path::Path;
use std::sync::Mutex;

use drm::control::Device as _;
use drmkit_core::Device;
use drmkit_display::{Probe, query_connector_modes};
use drmkit_modeset::ModeInfo;

/// Serialized with every other device test: an open file description holding
/// the card can deny master to another.
static CARD_LOCK: Mutex<()> = Mutex::new(());

fn card_guard() -> std::sync::MutexGuard<'static, ()> {
    CARD_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// The card path, and the `cardN` basename sysfs names its connectors after.
fn card_path() -> String {
    std::env::var("DRMKIT_TEST_CARD").unwrap_or_else(|_| "/dev/dri/card0".to_owned())
}

fn open_card() -> Option<Device> {
    let path = card_path();
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
    device.enable_universal_planes().expect("universal planes");
    device.enable_atomic().expect("atomic");
    Some(device)
}

#[test]
#[ignore = "needs a DRM device"]
fn every_connector_is_listed_in_resource_order() {
    let _guard = card_guard();
    let Some(device) = open_card() else { return };
    let resources = device.resource_handles().expect("resources");

    let listed = query_connector_modes(&device, Probe::Cached).expect("query");
    let expected: Vec<u32> = resources
        .connectors()
        .iter()
        .map(|handle| (*handle).into())
        .collect();
    let got: Vec<u32> = listed.iter().map(|c| c.connector_id).collect();

    // Equal rather than a subset: a connector vanishing mid-enumeration is
    // tolerated by the query, but it does not happen while nothing is being
    // plugged, and a listing that silently drops one is the bug this catches.
    assert_eq!(got, expected);
}

#[test]
#[ignore = "needs a DRM device"]
fn the_connector_name_is_the_one_the_kernel_uses() {
    // The name is how a caller finds the sysfs node, matches `modetest`
    // output, or reads a user's `--output HDMI-A-1`. A local table that has
    // drifted from the kernel's spelling produces a name that looks right and
    // matches nothing.
    let _guard = card_guard();
    let Some(device) = open_card() else { return };

    let card = Path::new(&card_path())
        .file_name()
        .expect("a card path ends in a name")
        .to_string_lossy()
        .into_owned();

    let listed = query_connector_modes(&device, Probe::Cached).expect("query");
    assert!(
        !listed.is_empty(),
        "a card with no connectors cannot display"
    );

    for connector in &listed {
        let node = format!("/sys/class/drm/{card}-{}", connector.name());
        assert!(
            Path::new(&node).is_dir(),
            "connector {} is named {} but there is no {node}",
            connector.connector_id,
            connector.name()
        );
    }
}

#[test]
#[ignore = "needs a DRM device"]
fn a_connected_display_advertises_at_least_one_mode() {
    let _guard = card_guard();
    let Some(device) = open_card() else { return };

    // Forced, because this is the listing case: the cache is what a hotplug
    // has just invalidated.
    let listed = query_connector_modes(&device, Probe::Force).expect("query");
    for connector in &listed {
        if !connector.connected {
            continue;
        }
        assert!(
            !connector.modes.is_empty(),
            "{} reports a display with no modes",
            connector.name()
        );
        for mode in &connector.modes {
            assert!(
                mode.width() > 0 && mode.height() > 0,
                "{}: a mode with no pixels in it",
                connector.name()
            );
        }
        println!(
            "{}: {} modes, largest {}x{}@{}",
            connector.name(),
            connector.modes.len(),
            connector
                .modes
                .iter()
                .map(ModeInfo::width)
                .max()
                .unwrap_or(0),
            connector
                .modes
                .iter()
                .map(ModeInfo::height)
                .max()
                .unwrap_or(0),
            connector.modes.first().map_or(0, ModeInfo::refresh),
        );
    }
}

#[test]
#[ignore = "needs a DRM device"]
fn probing_and_not_probing_describe_the_same_connectors() {
    // They can differ in what is attached -- that is what a probe is for --
    // but not in which connectors exist. A caller choosing `Cached` to avoid
    // waking a sleeping panel should not also be choosing a shorter list.
    let _guard = card_guard();
    let Some(device) = open_card() else { return };

    let cached = query_connector_modes(&device, Probe::Cached).expect("query");
    let forced = query_connector_modes(&device, Probe::Force).expect("query");

    let ids = |list: &[drmkit_display::ConnectorModes]| -> Vec<u32> {
        list.iter().map(|c| c.connector_id).collect()
    };
    assert_eq!(ids(&cached), ids(&forced));
    let names = |list: &[drmkit_display::ConnectorModes]| -> Vec<String> {
        list.iter()
            .map(drmkit_display::ConnectorModes::name)
            .collect()
    };
    assert_eq!(names(&cached), names(&forced));
}

/// The connector names sysfs lists for this card, which is the ground truth
/// the query is checked against rather than the query's own answer.
fn sysfs_connectors(card: &str) -> Vec<String> {
    let mut names = Vec::new();
    let Ok(entries) = std::fs::read_dir("/sys/class/drm") else {
        return names;
    };
    let prefix = format!("{card}-");
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if let Some(rest) = name.strip_prefix(&prefix) {
            names.push(rest.to_owned());
        }
    }
    names.sort();
    names
}

#[test]
#[ignore = "needs a DRM device"]
fn every_connector_sysfs_knows_about_can_be_listed() {
    // Writeback connectors are the reliable way to get a connector with
    // nothing attached: the kernel hides them until a client asks, and on
    // amdgpu reports one as not connected because nothing is plugged into a
    // buffer. Without them the disconnected path goes unexercised on any
    // machine whose displays are all plugged in, which is most of them.
    //
    // sysfs is the reference here, not the query, so a query that quietly
    // drops a connector cannot also be the thing that says none was expected.
    let _guard = card_guard();
    let Some(device) = open_card() else { return };

    let card = Path::new(&card_path())
        .file_name()
        .expect("a card path ends in a name")
        .to_string_lossy()
        .into_owned();
    let expected = sysfs_connectors(&card);
    assert!(!expected.is_empty(), "sysfs lists no connectors for {card}");

    let before = query_connector_modes(&device, Probe::Cached).expect("query");
    device
        .set_client_cap(drm::ClientCapability::WritebackConnectors, true)
        .expect("every atomic-capable driver accepts this cap");
    let after = query_connector_modes(&device, Probe::Cached).expect("query");

    // A capability that reveals connectors must not hide any.
    for connector in &before {
        assert!(
            after
                .iter()
                .any(|c| c.connector_id == connector.connector_id),
            "{} disappeared when the writeback cap was set",
            connector.name()
        );
    }

    let mut listed: Vec<String> = after
        .iter()
        .map(drmkit_display::ConnectorModes::name)
        .collect();
    listed.sort();
    assert_eq!(
        listed, expected,
        "the listing and sysfs disagree about what is on {card}"
    );

    for connector in after.iter().filter(|c| !c.connected) {
        assert!(
            connector.modes.is_empty(),
            "{} has no display but advertises {} modes",
            connector.name(),
            connector.modes.len()
        );
        println!("{} is listed with nothing attached", connector.name());
    }
}
