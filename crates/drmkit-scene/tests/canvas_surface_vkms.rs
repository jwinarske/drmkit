// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! The composition surface against a real device.
//!
//! The blend arithmetic is pinned host-side in `canvas_oracle.rs`, against
//! drm-cxx's own output. What needs a device is everything around it: that two
//! framebuffers really get allocated and registered, that painting lands in the
//! one about to be armed and not the one the kernel is reading, and that the
//! copy into the buffer reaches it at all.
//!
//! One thing it cannot cover: vkms never pads a dumb buffer's stride — it is
//! always exactly `width * 4`, and heights below 16 are refused outright — so
//! no test here can reach the padded-stride path. That is pinned host-side in
//! `canvas.rs` instead, over the row copy on its own.
//!
//! Every case is `#[ignore]`d and runs under the vkms lane with
//! `--include-ignored`.

use std::sync::Mutex;

use drmkit_core::Device;
use drmkit_fmt::fourcc;
use drmkit_scene::{CompositeCanvas, CompositeRect, CompositeSrc};

/// DRM master is per open file description, so card-dependent cases serialize.
static CARD_LOCK: Mutex<()> = Mutex::new(());

fn card_guard() -> std::sync::MutexGuard<'static, ()> {
    CARD_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Open the card, or `None` if this machine has no vkms.
///
/// Absence is a skip locally and a failure in the lane, the line
/// `DRMKIT_REQUIRE_MASTER` already draws. Everything past it asserts.
fn open_card_or_skip() -> Option<Device> {
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

fn solid(w: u32, h: u32, value: u32) -> Vec<u8> {
    value.to_ne_bytes().repeat((w * h) as usize)
}

fn source(pixels: &[u8], w: u32, h: u32) -> CompositeSrc<'_> {
    CompositeSrc {
        pixels,
        src_stride_bytes: w * 4,
        src_width: w,
        src_height: h,
        drm_fourcc: fourcc::ARGB8888,
        plane_alpha: 0xFFFF,
    }
}

const fn rect(w: u32, h: u32) -> CompositeRect {
    CompositeRect { x: 0, y: 0, w, h }
}

/// Both buffers are allocated and registered, so the canvas can be armed.
#[test]
#[ignore = "needs a DRM device"]
fn a_canvas_allocates_two_framebuffers_vkms() {
    let _guard = card_guard();
    let Some(device) = open_card_or_skip() else {
        return;
    };
    let mut canvas = CompositeCanvas::create(&device, 64, 32).expect("allocate the canvas");

    assert!(canvas.armable(), "a fresh canvas must be armable");
    let first = canvas.fb_id().expect("front buffer has a framebuffer");
    canvas.begin_frame();
    let second = canvas.fb_id().expect("back buffer has a framebuffer");

    assert_ne!(
        first, second,
        "the two buffers must be distinct, or the CPU paints what the kernel \
         is scanning out and the canvas tears"
    );

    canvas.begin_frame();
    assert_eq!(
        canvas.fb_id().expect("framebuffer"),
        first,
        "a third frame must return to the first buffer, not allocate a third"
    );
}

/// Painting reaches the buffer that is about to be armed.
///
/// The paint target is a shadow in ordinary memory, because a dumb buffer is
/// usually mapped write-combined and scattered writes into it are slow. That
/// makes `flush` load-bearing: skip it and every frame commits whatever the
/// buffer held before, which on a fresh allocation is zeroes — a canvas that
/// composites perfectly and displays nothing.
#[test]
#[ignore = "needs a DRM device"]
fn painting_lands_in_the_armed_buffer_vkms() {
    let _guard = card_guard();
    let Some(device) = open_card_or_skip() else {
        return;
    };
    let mut canvas = CompositeCanvas::create(&device, 64, 32).expect("allocate the canvas");

    canvas.begin_frame();
    canvas.clear();
    let pixels = solid(64, 32, 0xFFAB_CDEF);
    canvas.blend(&source(&pixels, 64, 32), rect(64, 32), rect(64, 32));
    canvas.flush();

    let armed = canvas.fb_id().expect("framebuffer");
    let painted = canvas.back_pixel(0, 0);
    let far_corner = canvas.back_pixel(63, 31);

    assert_eq!(painted, 0xFFAB_CDEF, "framebuffer {armed} was not painted");
    assert_eq!(
        far_corner, 0xFFAB_CDEF,
        "the last row landed wrong, which is what a mishandled stride looks like"
    );
}

/// A forgotten canvas cannot be armed.
///
/// Session pause revokes the descriptor, so the framebuffer ids the canvas
/// holds are no longer valid. Committing one would take down the whole atomic
/// request, so the canvas has to report that it cannot be used until it is
/// re-allocated.
#[test]
#[ignore = "needs a DRM device"]
fn a_forgotten_canvas_is_not_armable_vkms() {
    let _guard = card_guard();
    let Some(device) = open_card_or_skip() else {
        return;
    };
    let mut canvas = CompositeCanvas::create(&device, 64, 32).expect("allocate the canvas");
    assert!(canvas.armable());

    canvas.forget();

    assert!(
        !canvas.armable(),
        "a canvas whose device state was dropped must not offer a framebuffer id"
    );
    assert_eq!(canvas.fb_id(), None);
}
