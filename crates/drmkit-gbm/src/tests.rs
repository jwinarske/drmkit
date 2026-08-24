// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

use std::sync::Mutex;

use std::os::fd::AsRawFd as _;

use drmkit_core::Device;
use drmkit_fmt::fourcc;

use super::{GbmBuffer, GbmDevice, GbmError};

/// DRM master is per open file description, so card-dependent cases serialize.
static CARD_LOCK: Mutex<()> = Mutex::new(());

fn card_guard() -> std::sync::MutexGuard<'static, ()> {
    CARD_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Open the card, or `None` if this machine has no DRM device.
///
/// Absence is a skip locally and a failure in the lane — the line
/// `DRMKIT_REQUIRE_MASTER` already draws. Everything past it asserts.
fn open_card() -> Option<Device> {
    let path = std::env::var("DRMKIT_TEST_CARD").unwrap_or_else(|_| "/dev/dri/card0".to_owned());
    match Device::open(&path) {
        Ok(device) => Some(device),
        Err(error) => {
            assert!(
                std::env::var_os("DRMKIT_REQUIRE_MASTER").is_none(),
                "{path}: {error}, but DRMKIT_REQUIRE_MASTER is set"
            );
            println!("note: skipped -- no DRM device at {path} ({error})");
            None
        }
    }
}

/// Any DRM node opens as a GBM allocator.
///
/// Including a display-only one: the `drm` backend falls back to dumb
/// allocation where there is no render engine, which is what makes the rest of
/// these runnable against vkms rather than needing a GPU.
#[test]
fn a_drm_node_opens_as_a_gbm_device() {
    let _guard = card_guard();
    let Some(device) = open_card() else { return };
    let gbm = GbmDevice::new(&device).expect("open a GBM device");
    println!("note: gbm backend is {}", gbm.backend_name());
}

/// An allocated buffer reports the geometry that was asked for.
#[test]
fn a_buffer_reports_its_geometry() {
    let _guard = card_guard();
    let Some(device) = open_card() else { return };
    let gbm = GbmDevice::new(&device).expect("gbm device");
    let buffer = GbmBuffer::create(&gbm, 64, 32, fourcc::ARGB8888).expect("allocate");

    assert_eq!(buffer.width(), 64);
    assert_eq!(buffer.height(), 32);
    assert_eq!(buffer.fourcc(), fourcc::ARGB8888);
    assert!(
        buffer.stride() >= 64 * 4,
        "a stride below the row's own width cannot be right; got {}",
        buffer.stride()
    );
    assert!(buffer.plane_count() >= 1);
}

/// The buffer exports a dma-buf the caller owns.
///
/// This is the whole point of allocating through GBM rather than as a dumb
/// buffer: the descriptor is what carries the buffer to a scene, another
/// process, or the external-source import path.
#[test]
fn a_buffer_exports_a_dma_buf() {
    let _guard = card_guard();
    let Some(device) = open_card() else { return };
    let gbm = GbmDevice::new(&device).expect("gbm device");
    let buffer = GbmBuffer::create(&gbm, 64, 64, fourcc::ARGB8888).expect("allocate");

    let first = buffer.export().expect("export a dma-buf");
    let second = buffer.export().expect("export again");

    assert!(first.as_raw_fd() >= 0);
    assert_ne!(
        first.as_raw_fd(),
        second.as_raw_fd(),
        "each export must hand back its own descriptor; sharing one would mean \
         closing either invalidates the other"
    );
}

/// The modifier travels with the buffer, consistently.
///
/// A tiled or compressed buffer scanned out as though it were linear is not
/// slightly wrong, it is unreadable — so whatever the driver chose has to be
/// reportable, including the `INVALID` sentinel a driver uses when it is not
/// saying.
///
/// **What this cannot prove on vkms.** Its buffers really are linear, so a
/// `modifier()` that ignored the driver and returned zero would pass every
/// assertion here — verified by injecting exactly that. Catching it needs a
/// driver that reports something else, which is a GPU or vc4. Recorded in
/// `docs/parity-findings.md` with the other cases vkms structurally cannot
/// reach.
#[test]
fn a_buffer_reports_a_modifier() {
    let _guard = card_guard();
    let Some(device) = open_card() else { return };
    let gbm = GbmDevice::new(&device).expect("gbm device");
    let buffer = GbmBuffer::create(&gbm, 64, 64, fourcc::ARGB8888).expect("allocate");

    // What the value *is* cannot be asserted: vkms has no render engine so its
    // buffers come back linear, a GPU may pick a tiled layout, and a driver
    // that is not saying reports the INVALID sentinel. All three are
    // legitimate, and a test that accepted all three would assert nothing.
    //
    // What can be asserted is that it is the driver's answer and a stable one:
    // two identical requests to the same device must describe the same layout,
    // or a caller could not pass a modifier alongside a buffer at all.
    let modifier = buffer.modifier();
    println!("note: modifier is {modifier:#x}");

    let again = GbmBuffer::create(&gbm, 64, 64, fourcc::ARGB8888).expect("allocate again");
    assert_eq!(
        again.modifier(),
        modifier,
        "the same request to the same device must describe the same layout"
    );
    assert_eq!(again.stride(), buffer.stride(), "and the same stride");
}

/// An unsupported format is refused at allocation, not at commit.
///
/// A format the driver cannot allocate should fail here, where the caller can
/// pick another, rather than at the atomic commit where it takes the whole
/// frame down.
#[test]
fn an_unknown_format_is_refused() {
    let _guard = card_guard();
    let Some(device) = open_card() else { return };
    let gbm = GbmDevice::new(&device).expect("gbm device");

    let refused = GbmBuffer::create(&gbm, 64, 64, 0xDEAD_BEEF);

    assert!(
        matches!(refused, Err(GbmError::Allocation(_))),
        "a format no driver knows must be refused at allocation"
    );
}
