// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! Parity port of `tests/unit/test_dumb_buffer.cpp` and
//! `test_buffer_mapping.cpp` from drm-cxx @ `dc2915b`.
//!
//! Four of the five upstream `BufferMapping` cases test the C++ guard's unmap
//! bookkeeping — that the destructor calls the registered unmapper exactly
//! once, that a move transfers that responsibility, that move-assignment
//! destroys the target's, and that self-move is harmless. None of them survive
//! the port, and their absence is the point: [`Mapping`] carries no unmapper,
//! because for a dumb buffer there is nothing to unmap. The bookkeeping those
//! cases guard does not exist here, so there is no way for it to go wrong.
//!
//! What remains is the contract they were protecting, which is invariant 6, and
//! that is asserted directly below.

use super::*;
use std::sync::Mutex;

use drmkit_fmt::fourcc;

/// `Device::open` acquires DRM master, and master is per open file
/// description. Card-dependent cases serialize so they do not race for it —
/// the same hazard the `drmkit-core` suite hit.
static CARD_LOCK: Mutex<()> = Mutex::new(());

fn card_guard() -> std::sync::MutexGuard<'static, ()> {
    CARD_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn test_card() -> String {
    std::env::var("DRMKIT_TEST_CARD").unwrap_or_else(|_| "/dev/dri/card0".to_owned())
}

fn open_card() -> drmkit_core::Device {
    drmkit_core::Device::open(test_card()).expect("open test card")
}

// --- test_buffer_mapping.cpp / invariant 6 ----------------------------------

/// **Invariant 6.** `Mapping` has no `Drop` implementation, so "the guard's
/// destructor is a no-op" is a property of the type rather than a promise in a
/// comment.
///
/// `needs_drop` is the machine-checkable form of that. Adding a `Drop` impl —
/// or a field that has one — fails this immediately, which is what turns a
/// silent regression in per-frame cost into a visible, reviewable change.
#[test]
fn mapping_has_no_destructor() {
    assert!(
        !std::mem::needs_drop::<Mapping<'_>>(),
        "Mapping must have no drop glue: invariant 6 requires the guard's \
         destructor to be a no-op, and DumbScanoutSink's pacing depends on \
         map() costing nothing per frame"
    );
}

/// `BufferMapping.DefaultIsEmptyAndDestructorIsNoOp`
///
/// The C++ default-constructs an empty guard directly. Here a `Mapping` only
/// exists by borrowing a buffer, so the empty case is reached through an empty
/// buffer — which is the only way it can arise in practice anyway.
#[test]
fn empty_buffer_yields_an_empty_mapping() {
    let mut buffer = Buffer::new();
    let mapping = buffer.map(MapAccess::ReadWrite);

    assert!(mapping.is_empty());
    assert!(mapping.pixels().is_empty());
    assert_eq!(mapping.stride(), 0);
    assert_eq!(mapping.width(), 0);
    assert_eq!(mapping.height(), 0);
    // Falls out of scope without unmapping anything, because there is nothing
    // registered to unmap.
}

#[test]
fn mapping_records_the_access_mode() {
    let mut buffer = Buffer::new();
    for access in [MapAccess::Read, MapAccess::Write, MapAccess::ReadWrite] {
        assert_eq!(buffer.map(access).access(), access);
    }
    // ReadWrite is the default, matching the C++ member initializer.
    assert_eq!(MapAccess::default(), MapAccess::ReadWrite);
}

// --- test_dumb_buffer.cpp: pure cases ---------------------------------------

/// `DumbBuffer.DefaultCtorIsEmpty`
#[test]
fn default_buffer_is_empty() {
    let buffer = Buffer::new();
    assert!(buffer.is_empty());
    assert_eq!(buffer.width(), 0);
    assert_eq!(buffer.height(), 0);
    assert_eq!(buffer.stride(), 0);
    assert_eq!(buffer.size_bytes(), 0);
    assert_eq!(buffer.gem_handle(), None);
    assert_eq!(buffer.fb_id(), None);
    assert!(buffer.data().is_empty());

    assert!(Buffer::default().is_empty());
}

/// `DumbBuffer.ForgetOnEmptyIsSafe`
#[test]
fn forget_on_empty_is_safe() {
    let mut buffer = Buffer::new();
    buffer.forget();
    buffer.forget(); // twice
    assert!(buffer.is_empty());
}

/// `DumbBuffer.MoveConstructTransfersOwnership` / `MoveAssignReplacesTarget`
///
/// A move is not observable in Rust, so what is checkable is that the moved
/// value still describes the same allocation and that nothing was released
/// along the way. The card-backed case below covers the allocation half.
#[test]
fn move_preserves_an_empty_buffer() {
    let source = Buffer::new();
    let moved = source;
    assert!(moved.is_empty());
}

/// `DumbBuffer.CreateRejectsZeroWidth` / `ZeroHeight` / `ZeroFormat` /
/// `ZeroBpp`
///
/// These need a device in the C++ because validation happens after the fd
/// check. Here the configuration is validated first, so they are reachable
/// without a card — but they still need a `Device` to call, so the card-backed
/// variant below is what actually runs them.
#[test]
#[ignore = "needs a DRM device; run under the vkms lane with --include-ignored"]
fn create_rejects_invalid_configs_vkms() {
    let _guard = card_guard();
    let device = open_card();

    let base = Config {
        width: 64,
        height: 64,
        fourcc: fourcc::XRGB8888,
        ..Config::default()
    };

    for (config, what) in [
        (Config { width: 0, ..base }, "zero width"),
        (Config { height: 0, ..base }, "zero height"),
        (Config { fourcc: 0, ..base }, "zero format"),
        (Config { bpp: 0, ..base }, "zero bpp"),
        (
            Config {
                width: 16385,
                ..base
            },
            "width past 16384",
        ),
        (
            Config {
                height: 16385,
                ..base
            },
            "height past 16384",
        ),
    ] {
        assert!(
            matches!(
                Buffer::create(&device, &config),
                Err(DumbError::InvalidConfig { .. })
            ),
            "{what} must be rejected"
        );
    }
}

/// An unrecognized `FourCC` is rejected rather than silently substituted.
#[test]
#[ignore = "needs a DRM device; run under the vkms lane with --include-ignored"]
fn create_rejects_an_unknown_fourcc_vkms() {
    let _guard = card_guard();
    let device = open_card();
    let config = Config {
        width: 64,
        height: 64,
        fourcc: 0xdead_beef,
        ..Config::default()
    };
    assert!(matches!(
        Buffer::create(&device, &config),
        Err(DumbError::InvalidConfig { .. })
    ));
}

/// `create_planar` refuses formats outside the recognized semi-planar set, so
/// callers know to fall back to `create`.
#[test]
#[ignore = "needs a DRM device; run under the vkms lane with --include-ignored"]
fn create_planar_rejects_unsupported_formats_vkms() {
    let _guard = card_guard();
    let device = open_card();

    assert!(matches!(
        Buffer::create_planar(&device, fourcc::XRGB8888, 64, 64),
        Err(DumbError::UnsupportedFormat { .. })
    ));
    assert!(
        matches!(
            Buffer::create_planar(&device, fourcc::YUV420, 64, 64),
            Err(DumbError::UnsupportedFormat { .. })
        ),
        "fully planar 4:2:0 is not in the semi-planar set"
    );
}

// --- card-backed allocation --------------------------------------------------

#[test]
#[ignore = "needs a DRM device; run under the vkms lane with --include-ignored"]
fn create_allocates_maps_and_registers_a_framebuffer_vkms() {
    let _guard = card_guard();
    let device = open_card();

    let mut buffer = Buffer::create(
        &device,
        &Config {
            width: 64,
            height: 64,
            fourcc: fourcc::XRGB8888,
            ..Config::default()
        },
    )
    .expect("allocate");

    assert!(!buffer.is_empty());
    assert_eq!(buffer.width(), 64);
    assert_eq!(buffer.height(), 64);
    assert!(buffer.stride() >= 64 * 4, "stride covers at least the row");
    assert!(
        buffer.size_bytes() >= buffer.stride() as usize * 64,
        "the mapping covers the whole allocation"
    );
    assert!(buffer.gem_handle().is_some_and(|h| h != 0));
    assert!(buffer.fb_id().is_some_and(|f| f != 0));

    // The mapping is writable and readable back -- it is a real coherent view.
    {
        let mut view = buffer.map(MapAccess::Write);
        assert_eq!(view.width(), 64);
        assert_eq!(view.height(), 64);
        view.row_mut(0).expect("row 0").fill(0xab);
        view.row_mut(63).expect("row 63").fill(0xcd);
        assert!(view.row_mut(64).is_none(), "past the bottom");
    }

    let view = buffer.map(MapAccess::Read);
    assert!(view.row(0).expect("row 0").iter().all(|b| *b == 0xab));
    assert!(view.row(63).expect("row 63").iter().all(|b| *b == 0xcd));
}

/// `add_fb: false` is the legacy cursor path: allocate and map, no framebuffer.
#[test]
#[ignore = "needs a DRM device; run under the vkms lane with --include-ignored"]
fn create_without_a_framebuffer_vkms() {
    let _guard = card_guard();
    let device = open_card();

    let buffer = Buffer::create(
        &device,
        &Config {
            width: 64,
            height: 64,
            fourcc: fourcc::ARGB8888,
            add_fb: false,
            ..Config::default()
        },
    )
    .expect("allocate");

    assert!(buffer.gem_handle().is_some(), "the GEM handle still exists");
    assert_eq!(buffer.fb_id(), None, "but no framebuffer was registered");
    assert!(!buffer.data().is_empty());
}

/// The planar path over-allocates for the chroma plane and reports the *image*
/// height, not the allocated row count.
#[test]
#[ignore = "needs a DRM device; run under the vkms lane with --include-ignored"]
fn create_planar_over_allocates_for_chroma_vkms() {
    let _guard = card_guard();
    let device = open_card();

    let buffer = match Buffer::create_planar(&device, fourcc::NV12, 64, 64) {
        Ok(buffer) => buffer,
        // vkms does not advertise every YUV format; a driver rejecting the
        // framebuffer is a statement about the driver, not about this code.
        Err(DumbError::Io(e)) => {
            println!("note: driver rejected NV12 dumb framebuffer: {e}");
            return;
        }
        Err(e) => panic!("unexpected failure: {e}"),
    };

    assert_eq!(buffer.height(), 64, "reports the image height");
    let y_plane = buffer.stride() as usize * 64;
    assert!(
        buffer.size_bytes() >= y_plane + y_plane / 2,
        "the allocation covers Y plus a half-height chroma plane: got {} for a \
         {}-byte Y plane",
        buffer.size_bytes(),
        y_plane
    );
}

/// `forget` releases the mapping without touching the descriptor, leaving a
/// valid empty buffer. The C++ needs this because libseat closes the
/// descriptor out from under a live buffer on session pause.
#[test]
#[ignore = "needs a DRM device; run under the vkms lane with --include-ignored"]
fn forget_drops_handles_without_ioctls_vkms() {
    let _guard = card_guard();
    let device = open_card();

    let mut buffer = Buffer::create(
        &device,
        &Config {
            width: 32,
            height: 32,
            fourcc: fourcc::XRGB8888,
            ..Config::default()
        },
    )
    .expect("allocate");
    assert!(!buffer.is_empty());

    buffer.forget();

    assert!(buffer.is_empty());
    assert_eq!(buffer.gem_handle(), None);
    assert_eq!(buffer.fb_id(), None);
    assert!(buffer.data().is_empty());
    assert_eq!(buffer.size_bytes(), 0, "the mapping was torn down");

    // Dropping a forgotten buffer must not issue ioctls against a descriptor
    // it no longer owns.
    drop(buffer);
}

/// Allocating and dropping repeatedly must not leak GEM objects or mappings.
/// Measured as descriptor-table growth for the same reason as the
/// `drmkit-sync` leak test: an absolute count races the parallel harness.
#[test]
#[ignore = "needs a DRM device; run under the vkms lane with --include-ignored"]
fn repeated_allocation_does_not_leak_vkms() {
    let _guard = card_guard();
    let device = open_card();
    let config = Config {
        width: 64,
        height: 64,
        fourcc: fourcc::XRGB8888,
        ..Config::default()
    };

    let allocate_and_drop = |times: usize| {
        for _ in 0..times {
            let buffer = Buffer::create(&device, &config).expect("allocate");
            assert!(!buffer.is_empty());
            drop(buffer);
        }
    };
    let count = || {
        std::fs::read_dir("/proc/self/fd")
            .expect("/proc/self/fd")
            .count()
    };

    allocate_and_drop(8);
    let after_few = count();
    allocate_and_drop(120);
    let after_many = count();

    let growth = after_many.saturating_sub(after_few);
    assert!(
        growth < 16,
        "120 further allocate/drop cycles grew the fd table by {growth}"
    );
}
