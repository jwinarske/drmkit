// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! Health checks for the vkms lane itself.
//!
//! These do not test drmkit — no drmkit subsystem that touches a card exists
//! yet. They test that **the lane is real**: that a modeset card is present
//! and that it is the VKMS one the lane believes it booted.
//!
//! A CI lane that passes whether or not the hardware it claims to exercise is
//! present is worse than no lane, because it reports confidence it has not
//! earned. drm-cxx's own integration tests self-skip on hosted runners, which
//! is exactly the failure mode this port set out to avoid (plan §9 T5). These
//! checks are the tripwire: if the vkms module stops loading, or the card node
//! never appears, the lane goes red instead of quietly testing nothing.
//!
//! Every test here is `#[ignore]` so a plain `cargo test` on a developer
//! machine with no vkms stays green; the lane runs them with
//! `--include-ignored`.
//!
//! The card is taken from `DRMKIT_TEST_CARD`, defaulting to `/dev/dri/card0`.

use std::path::{Path, PathBuf};

/// The card node under test.
fn test_card() -> PathBuf {
    std::env::var_os("DRMKIT_TEST_CARD")
        .map_or_else(|| PathBuf::from("/dev/dri/card0"), PathBuf::from)
}

/// Driver name reported by sysfs for a `/dev/dri/cardN` node, if resolvable.
///
/// Note this is the *bus* driver, not the DRM driver name. VKMS on current
/// kernels registers through `faux_driver`, so this reports `faux_driver`
/// rather than `vkms`. Getting the DRM driver name needs `DRM_IOCTL_VERSION`,
/// which arrives with `drmkit-core` in phase 1.
fn bus_driver_for(card: &Path) -> Option<String> {
    let name = card.file_name()?.to_str()?;
    let link = PathBuf::from("/sys/class/drm")
        .join(name)
        .join("device/driver");
    let target = std::fs::read_link(link).ok()?;
    Some(target.file_name()?.to_str()?.to_owned())
}

/// Whether sysfs agrees the node is a DRM device.
fn is_drm_device(card: &Path) -> bool {
    card.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|name| {
            PathBuf::from("/sys/class/drm")
                .join(name)
                .join("device/drm")
                .is_dir()
        })
}

#[test]
#[ignore = "needs a modeset card; run under the vkms lane with --include-ignored"]
fn card_node_exists() {
    let card = test_card();
    assert!(
        card.exists(),
        "no card node at `{}`. The lane believes it booted with VKMS, so this \
         means the module did not load or the node was not created -- not that \
         the test should be skipped.",
        card.display()
    );
}

#[test]
#[ignore = "needs a modeset card; run under the vkms lane with --include-ignored"]
fn card_node_is_openable() {
    let card = test_card();
    // Read-only: opening a DRM node read-write would take DRM master, which
    // this check has no business doing.
    if let Err(e) = std::fs::File::open(&card) {
        panic!(
            "cannot open `{}`: {e}. Under virtme-ng the runner is root, so a \
             permission error here means the node is not what it claims to be.",
            card.display()
        );
    }
}

#[test]
#[ignore = "needs a modeset card; run under the vkms lane with --include-ignored"]
fn card_is_a_drm_device() {
    let card = test_card();

    assert!(
        is_drm_device(&card),
        "`{}` has no `device/drm` entry in sysfs -- it is not a DRM device, so \
         whatever the lane is testing, it is not a display card",
        card.display()
    );

    // Reported, not asserted. VKMS registers through `faux_driver` on current
    // kernels, and pointing DRMKIT_TEST_CARD at real hardware is legitimate
    // (the C++ side allows the same via DRM_CXX_TEST_CARD), so the bus driver
    // name cannot distinguish a healthy lane from an unhealthy one. Asserting
    // `vkms` here would fail on precisely the configuration the lane runs.
    // The real check -- DRM_IOCTL_VERSION reporting the driver name -- lands
    // with `drmkit-core` in phase 1 and should replace this note.
    match bus_driver_for(&card) {
        Some(driver) => println!("note: `{}` bus driver is `{driver}`", card.display()),
        None => println!("note: `{}` has no bus driver symlink", card.display()),
    }
}
