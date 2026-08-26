// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! Parity port of `tests/integration/test_dumb_ring_source.cpp` from drm-cxx
//! @ `4a0b64a`.
//!
//! Needs a dumb-buffer-capable card. Upstream prints a line and returns zero
//! when there is none, so its lane is green whether or not it tested anything;
//! these are `#[ignore]`d and report why they bailed, so a run that covered
//! nothing says so.

use std::sync::{Mutex, MutexGuard};

use drmkit_core::Device;
use drmkit_present::{DumbRingSource, Rect, Repaint};
use drmkit_scene::LayerBufferSource as _;

/// Serialises the cases: two of them opening the same card at once looks like
/// a device fault.
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

/// Eight frames through a three-slot ring: fresh slots repaint fully, and once
/// the ring starts reusing presented slots at least one frame repaints
/// partially.
///
/// The partial is the point. A ring that always answered `Full` would pass
/// every other assertion here and save no bandwidth at all.
#[test]
#[ignore = "needs a DRM device"]
fn a_reused_slot_eventually_asks_for_a_partial_repaint_vkms() {
    let _guard = card_guard();
    let Some(device) = open_card() else { return };

    let mut source = DumbRingSource::new(128, 128, drmkit_fmt::fourcc::XRGB8888, 3)
        .expect("XRGB8888 has a known bpp");

    let mut saw_full = false;
    let mut saw_partial = false;
    let mut previous = None;

    for frame in 0..8 {
        source
            .paint(&device, |mapping, repaint| {
                match repaint {
                    Repaint::Full => {
                        saw_full = true;
                        for row in 0..128 {
                            if let Some(row) = mapping.row_mut(row) {
                                row.fill(0);
                            }
                        }
                    }
                    Repaint::Region(region) => {
                        saw_partial = true;
                        assert!(
                            !region.is_empty(),
                            "frame {frame}: a region repaint with nothing in it is a full \
                             repaint wearing the wrong variant"
                        );
                    }
                }
                vec![Rect {
                    x: 0,
                    y: 0,
                    width: 32,
                    height: 32,
                }]
            })
            .unwrap_or_else(|error| panic!("frame {frame}: {error}"));

        let acquired = source.acquire().expect("a painted slot is acquirable");
        assert_ne!(acquired.fb_id, 0, "frame {frame}: the scene needs an fb id");

        // Release the *previous* frame's buffer, mirroring the scene retiring
        // it once the next frame is committed, so a slot is always free.
        if let Some(previous) = previous.take() {
            source.release(previous);
        }
        previous = Some(acquired);
    }
    if let Some(previous) = previous.take() {
        source.release(previous);
    }

    assert!(saw_full, "the first frames have undefined contents");
    assert!(
        saw_partial,
        "a three-slot ring reusing presented slots must eventually ask for less \
         than the whole buffer, or the age bookkeeping is doing nothing"
    );
}

/// Slots are allocated as the ring grows, not up front.
///
/// The reference documents this ("slots are allocated lazily") and has no case
/// for it. On a 4K display it is the difference between reserving one buffer
/// and reserving three.
#[test]
#[ignore = "needs a DRM device"]
fn slots_are_allocated_as_the_ring_grows_vkms() {
    let _guard = card_guard();
    let Some(device) = open_card() else { return };

    let mut source = DumbRingSource::new(64, 64, drmkit_fmt::fourcc::XRGB8888, 3)
        .expect("XRGB8888 has a known bpp");
    assert_eq!(source.allocated(), 0, "nothing before the first paint");

    source
        .paint(&device, |_, _| Vec::new())
        .expect("first paint");
    assert_eq!(source.allocated(), 1, "one slot, not the whole ring");

    // Holding the first slot forces the ring to grow for the second.
    let held = source.acquire().expect("first");
    source.paint(&device, |_, _| Vec::new()).expect("second");
    assert_eq!(source.allocated(), 2);
    source.release(held);
}

/// Acquiring without painting is `WouldBlock`, not a stale buffer.
#[test]
#[ignore = "needs a DRM device"]
fn acquiring_without_painting_blocks_rather_than_repeating_a_frame_vkms() {
    let _guard = card_guard();
    let Some(device) = open_card() else { return };

    let mut source = DumbRingSource::new(64, 64, drmkit_fmt::fourcc::XRGB8888, 2)
        .expect("XRGB8888 has a known bpp");
    assert!(
        source.acquire().is_err(),
        "nothing painted yet, so there is nothing to scan out"
    );

    source.paint(&device, |_, _| Vec::new()).expect("paint");
    let acquired = source.acquire().expect("painted");
    assert!(
        source.acquire().is_err(),
        "one paint is one frame: a second acquire must not re-present it"
    );
    source.release(acquired);
}
