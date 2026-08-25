// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! `snapshot` against a real device.
//!
//! These need a CRTC that is actually driving a mode, which is a thing a test
//! cannot conjure without taking over the display. On a board with a console
//! on HDMI that is the normal state; on a bare vkms instance it is not, and
//! the tests say so and skip rather than passing on a CRTC with nothing to
//! read.

use drm::control::Device as _;
use drmkit_capture::{CaptureError, snapshot, write_png};
use drmkit_core::Device;

/// Open the card under test, or skip.
fn card() -> Option<Device> {
    let path = std::env::var("DRMKIT_TEST_CARD").unwrap_or_else(|_| "/dev/dri/card0".to_owned());
    match Device::open(&path) {
        Ok(device) => Some(device),
        Err(error) => {
            assert!(
                std::env::var_os("DRMKIT_REQUIRE_MASTER").is_none(),
                "{path}: {error}, but DRMKIT_REQUIRE_MASTER is set"
            );
            println!("note: skipped -- {path}: {error}");
            None
        }
    }
}

/// The first CRTC that is driving a mode, with its size.
fn active_crtc(device: &Device) -> Option<(u32, u32, u32)> {
    let resources = device.resource_handles().ok()?;
    for handle in resources.crtcs() {
        let Ok(info) = device.get_crtc(*handle) else {
            continue;
        };
        if let Some(mode) = info.mode() {
            let (w, h) = mode.size();
            return Some(((*handle).into(), u32::from(w), u32::from(h)));
        }
    }
    None
}

/// A device with master, universal planes and atomic, or `None`.
fn ready() -> Option<Device> {
    let device = card()?;
    device.enable_universal_planes().expect("universal planes");
    device.enable_atomic().expect("atomic");
    if device.set_master().is_err() {
        assert!(
            std::env::var_os("DRMKIT_REQUIRE_MASTER").is_none(),
            "not DRM master, but DRMKIT_REQUIRE_MASTER is set"
        );
        println!("note: skipped -- another client holds DRM master");
        return None;
    }
    Some(device)
}

#[test]
#[ignore = "needs a DRM device with an active CRTC"]
fn a_snapshot_is_the_size_of_the_mode_vkms() {
    let Some(device) = ready() else { return };
    let Some((crtc_id, w, h)) = active_crtc(&device) else {
        println!("note: skipped -- no CRTC is driving a mode");
        return;
    };
    println!("note: capturing crtc {crtc_id} at {w}x{h}");

    let image = snapshot(&device, crtc_id).expect("snapshot");
    assert_eq!(image.width(), w);
    assert_eq!(image.height(), h);
    assert_eq!(image.pixels().len(), (w * h) as usize);
    assert_eq!(image.stride_bytes(), w * 4);
}

#[test]
#[ignore = "needs a DRM device with an active CRTC"]
fn a_snapshot_writes_a_png_that_decodes_back_vkms() {
    let Some(device) = ready() else { return };
    let Some((crtc_id, w, h)) = active_crtc(&device) else {
        println!("note: skipped -- no CRTC is driving a mode");
        return;
    };

    let image = snapshot(&device, crtc_id).expect("snapshot");
    let mut path = std::env::temp_dir();
    path.push(format!("drmkit_snapshot_{}.png", std::process::id()));
    let _ = std::fs::remove_file(&path);

    write_png(&image, &path).expect("write");

    let file = std::io::BufReader::new(std::fs::File::open(&path).expect("open"));
    let mut reader = png::Decoder::new(file).read_info().expect("header");
    let info = reader.info();
    assert_eq!((info.width, info.height), (w, h));
    assert_eq!(info.color_type, png::ColorType::Rgba);
    let mut buffer = vec![0; reader.output_buffer_size().expect("size")];
    reader.next_frame(&mut buffer).expect("decode");

    let _ = std::fs::remove_file(&path);
}

#[test]
#[ignore = "needs a DRM device with an active CRTC"]
fn without_the_atomic_cap_there_is_nothing_to_read_vkms() {
    // The kernel hides CRTC_X/CRTC_Y/CRTC_W/CRTC_H from clients that have not
    // set DRM_CLIENT_CAP_ATOMIC, so a non-atomic client sees planes with no
    // geometry and cannot place any of them. That is a documented
    // precondition of `snapshot`, and it is worth a test because the symptom
    // -- a blank capture from a CRTC that plainly has something on it -- looks
    // nothing like its cause. It is the same filtering that makes
    // `modetest -p` report no IN_FENCE_FD on hardware that has it.
    let Some(device) = ready() else { return };
    let Some((crtc_id, _, _)) = active_crtc(&device) else {
        println!("note: skipped -- no CRTC is driving a mode");
        return;
    };
    // Sanity: with the cap, this CRTC does capture.
    snapshot(&device, crtc_id).expect("the atomic client can read it");
    drop(device);

    let Some(plain) = card() else { return };
    plain.enable_universal_planes().expect("universal planes");
    // Deliberately no enable_atomic().
    if plain.set_master().is_err() {
        println!("note: skipped -- could not retake master");
        return;
    }
    assert!(
        matches!(
            snapshot(&plain, crtc_id),
            Err(CaptureError::NothingReadable)
        ),
        "a non-atomic client must fail rather than return a blank capture"
    );
}

/// The four quadrant colours of the round-trip pattern, premultiplied and
/// opaque, so encoding them involves no scaling and any difference on the way
/// back is a real one.
const RED: u32 = 0xffff_0000;
const GREEN: u32 = 0xff00_ff00;
const BLUE: u32 = 0xff00_00ff;
const WHITE: u32 = 0xffff_ffff;

/// A connected connector, the CRTC it can drive, and a mode for it.
fn output(device: &Device) -> Option<(u32, drm::control::connector::Handle, drm::control::Mode)> {
    let resources = device.resource_handles().ok()?;
    for handle in resources.connectors() {
        let Ok(connector) = device.get_connector(*handle, false) else {
            continue;
        };
        if connector.state() != drm::control::connector::State::Connected {
            continue;
        }
        let mode = *connector.modes().first()?;
        for encoder_handle in connector.encoders() {
            let Ok(encoder) = device.get_encoder(*encoder_handle) else {
                continue;
            };
            if let Some(crtc) = resources.filter_crtcs(encoder.possible_crtcs()).first() {
                return Some(((*crtc).into(), *handle, mode));
            }
        }
    }
    None
}

#[test]
#[ignore = "needs a DRM device and DRM master"]
fn a_known_pattern_survives_the_round_trip_vkms() {
    // The one test that says the whole path works rather than that each piece
    // does: paint four quadrants into a dumb buffer, put it on screen for
    // real, read it back, and check the corners came back the colours they
    // went out as. Everything in between -- plane enumeration, geometry
    // properties, the dma-buf export, the stride, the blend, the row order --
    // has to be right for this to pass, and any one of them being wrong
    // produces a different, recognisable failure.
    //
    // With one exception, which is worth knowing before trusting this test to
    // cover the whole path: it cannot catch a stride bug. vkms hands back a
    // tightly packed buffer -- 1024x768 at 32bpp has a pitch of exactly
    // width * 4 -- so reading rows at `width * 4` instead of `pitch` passes
    // here and shears every image on hardware that aligns its strides.
    // `a_pitch_larger_than_the_width_skips_the_padding` is what covers that,
    // with a pitch chosen to be deliberately loose. Verified by injecting the
    // bug: the unit test fails, this one does not.
    let Some(device) = ready() else { return };
    let Some((crtc_id, connector, mode)) = output(&device) else {
        println!("note: skipped -- no connected connector with a usable CRTC");
        return;
    };
    let (w, h) = (u32::from(mode.size().0), u32::from(mode.size().1));
    assert!(w >= 16 && h >= 16, "a {w}x{h} mode is too small to quarter");
    println!("note: round-tripping crtc {crtc_id} at {w}x{h}");

    let mut buffer = drmkit_dumb::Buffer::create(
        &device,
        &drmkit_dumb::Config {
            width: w,
            height: h,
            fourcc: drmkit_fmt::fourcc::XRGB8888,
            bpp: 32,
            add_fb: true,
        },
    )
    .expect("dumb buffer");

    {
        let mut mapping = buffer.map(drmkit_dumb::MapAccess::Write);
        for y in 0..h {
            let row = mapping.row_mut(y).expect("row within the buffer");
            for x in 0..w {
                let colour = match (x >= w / 2, y >= h / 2) {
                    (false, false) => RED,
                    (true, false) => GREEN,
                    (false, true) => BLUE,
                    (true, true) => WHITE,
                };
                let at = (x * 4) as usize;
                row[at..at + 4].copy_from_slice(&colour.to_ne_bytes());
            }
        }
    }

    let fb = buffer.fb_id().expect("framebuffer id");
    let fb_handle =
        drm::control::framebuffer::Handle::from(core::num::NonZeroU32::new(fb).expect("non-zero"));

    // Legacy SetCrtc, which atomic drivers reflect into the primary plane's
    // FB_ID -- which is exactly what `snapshot` reads back.
    device
        .set_crtc(
            drm::control::crtc::Handle::from(core::num::NonZeroU32::new(crtc_id).expect("nonzero")),
            Some(fb_handle),
            (0, 0),
            &[connector],
            Some(mode),
        )
        .expect("set_crtc");

    let captured = snapshot(&device, crtc_id);

    // Unbind before asserting, so a failure does not leave the display
    // pointing at a buffer this test is about to drop.
    let _ = device.set_crtc(
        drm::control::crtc::Handle::from(core::num::NonZeroU32::new(crtc_id).expect("nonzero")),
        None,
        (0, 0),
        &[],
        None,
    );

    let image = captured.expect("snapshot");
    assert_eq!(image.width(), w);
    assert_eq!(image.height(), h);

    let at = |x: u32, y: u32| image.pixels()[(y * image.width() + x) as usize];
    assert_eq!(at(w / 4, h / 4), RED, "top-left");
    assert_eq!(at(3 * w / 4, h / 4), GREEN, "top-right");
    assert_eq!(at(w / 4, 3 * h / 4), BLUE, "bottom-left");
    assert_eq!(at(3 * w / 4, 3 * h / 4), WHITE, "bottom-right");
}
