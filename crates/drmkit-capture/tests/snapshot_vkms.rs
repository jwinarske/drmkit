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

/// Serializes the tests that take DRM master.
///
/// The lane runs each test binary in turn but lets the tests inside one run in
/// parallel, so two of these racing for master means one of them fails with
/// "not DRM master" on a machine where nothing else is using the display.
static CARD_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn card_guard() -> std::sync::MutexGuard<'static, ()> {
    CARD_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Open the card under test, or skip.
fn card() -> Option<Device> {
    let path = std::env::var("DRMKIT_TEST_CARD").unwrap_or_else(|_| "/dev/dri/card0".to_owned());
    drmkit_testkit::announce_card(&path);
    match Device::open(&path) {
        Ok(device) => Some(device),
        Err(error) => {
            assert!(
                std::env::var_os("DRMKIT_REQUIRE_MASTER").is_none(),
                "{path}: {error}, but DRMKIT_REQUIRE_MASTER is set"
            );
            drmkit_testkit::skipped(&format!("{path}: {error}"));
            None
        }
    }
}

/// The four quadrant colors, premultiplied and opaque, so encoding them
/// involves no scaling and any difference on the way back is a real one.
const RED: u32 = 0xffff_0000;
const GREEN: u32 = 0xff00_ff00;
const BLUE: u32 = 0xff00_00ff;
const WHITE: u32 = 0xffff_ffff;

/// A CRTC driving a known four-quadrant pattern, unbound when dropped.
///
/// Each test sets up its own output rather than capturing whatever happened to
/// be on screen. Relying on a pre-existing mode makes the tests
/// order-dependent -- whichever one unbinds first leaves the others with
/// nothing active, and they skip rather than fail, so the lane goes green
/// having tested nothing.
struct Output<'a> {
    device: &'a Device,
    crtc: drm::control::crtc::Handle,
    crtc_id: u32,
    width: u32,
    height: u32,
    /// Holds the framebuffer alive for as long as it is being scanned out.
    _buffer: drmkit_dumb::Buffer,
}

impl Drop for Output<'_> {
    fn drop(&mut self) {
        // Unbind before the buffer goes, so the display is never pointed at a
        // framebuffer that is being torn down.
        let _ = self.device.set_crtc(self.crtc, None, (0, 0), &[], None);
    }
}

/// Paint the quadrant pattern and put it on screen, or `None` if this device
/// has no connected output.
fn paint(device: &Device) -> Option<Output<'_>> {
    let resources = device.resource_handles().ok()?;
    let (crtc, connector, mode) = resources.connectors().iter().find_map(|handle| {
        let connector = device.get_connector(*handle, false).ok()?;
        if connector.state() != drm::control::connector::State::Connected {
            return None;
        }
        let mode = *connector.modes().first()?;
        connector.encoders().iter().find_map(|encoder| {
            let encoder = device.get_encoder(*encoder).ok()?;
            let crtc = *resources.filter_crtcs(encoder.possible_crtcs()).first()?;
            Some((crtc, *handle, mode))
        })
    })?;

    let (width, height) = (u32::from(mode.size().0), u32::from(mode.size().1));
    assert!(
        width >= 16 && height >= 16,
        "a {width}x{height} mode is too small to quarter"
    );

    let mut buffer = drmkit_dumb::Buffer::create(
        device,
        &drmkit_dumb::Config {
            width,
            height,
            fourcc: drmkit_fmt::fourcc::XRGB8888,
            bpp: 32,
            add_fb: true,
        },
    )
    .expect("dumb buffer");

    {
        let mut mapping = buffer.map(drmkit_dumb::MapAccess::Write);
        for y in 0..height {
            let row = mapping.row_mut(y).expect("row within the buffer");
            for x in 0..width {
                let color = match (x >= width / 2, y >= height / 2) {
                    (false, false) => RED,
                    (true, false) => GREEN,
                    (false, true) => BLUE,
                    (true, true) => WHITE,
                };
                let at = (x * 4) as usize;
                row[at..at + 4].copy_from_slice(&color.to_ne_bytes());
            }
        }
    }

    let fb = buffer.fb_id().expect("framebuffer id");
    let fb_handle =
        drm::control::framebuffer::Handle::from(core::num::NonZeroU32::new(fb).expect("non-zero"));

    // Legacy SetCrtc, which atomic drivers reflect into the primary plane's
    // FB_ID -- which is exactly what `snapshot` reads back.
    device
        .set_crtc(crtc, Some(fb_handle), (0, 0), &[connector], Some(mode))
        .expect("set_crtc");

    Some(Output {
        device,
        crtc,
        crtc_id: crtc.into(),
        width,
        height,
        _buffer: buffer,
    })
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
        drmkit_testkit::skipped("another client holds DRM master");
        return None;
    }
    Some(device)
}

#[test]
#[ignore = "needs a DRM device and DRM master"]
fn a_snapshot_is_the_size_of_the_mode_vkms() {
    let _guard = card_guard();
    let Some(device) = ready() else { return };
    let Some(output) = paint(&device) else {
        drmkit_testkit::skipped("no connected connector with a usable CRTC");
        return;
    };
    println!(
        "note: capturing crtc {} at {}x{}",
        output.crtc_id, output.width, output.height
    );

    let image = snapshot(&device, output.crtc_id).expect("snapshot");
    assert_eq!(image.width(), output.width);
    assert_eq!(image.height(), output.height);
    assert_eq!(
        image.pixels().len(),
        (output.width * output.height) as usize
    );
    assert_eq!(image.stride_bytes(), output.width * 4);
}

#[test]
#[ignore = "needs a DRM device and DRM master"]
fn a_snapshot_writes_a_png_that_decodes_back_vkms() {
    let _guard = card_guard();
    let Some(device) = ready() else { return };
    let Some(output) = paint(&device) else {
        drmkit_testkit::skipped("no connected connector with a usable CRTC");
        return;
    };

    let image = snapshot(&device, output.crtc_id).expect("snapshot");
    let mut path = std::env::temp_dir();
    path.push(format!("drmkit_snapshot_{}.png", std::process::id()));
    let _ = std::fs::remove_file(&path);

    write_png(&image, &path).expect("write");

    let file = std::io::BufReader::new(std::fs::File::open(&path).expect("open"));
    let mut reader = png::Decoder::new(file).read_info().expect("header");
    let info = reader.info();
    assert_eq!((info.width, info.height), (output.width, output.height));
    assert_eq!(info.color_type, png::ColorType::Rgba);
    let mut buffer = vec![0; reader.output_buffer_size().expect("size")];
    reader.next_frame(&mut buffer).expect("decode");

    let _ = std::fs::remove_file(&path);
}

#[test]
#[ignore = "needs a DRM device and DRM master"]
fn a_known_pattern_survives_the_round_trip_vkms() {
    // The one test that says the whole path works rather than that each piece
    // does: paint four quadrants, put them on screen for real, read them back,
    // and check the corners came back the colors they went out as. Plane
    // enumeration, the geometry properties, the dma-buf export, the stride,
    // the blend and the row order all have to be right for this to pass, and
    // any one of them being wrong produces a different, recognizable failure.
    //
    // With one exception, worth knowing before trusting this to cover the
    // whole path: it cannot catch a stride bug. vkms hands back a tightly
    // packed buffer -- 1024x768 at 32bpp has a pitch of exactly width * 4 --
    // so reading rows at `width * 4` instead of `pitch` passes here and shears
    // every image on hardware that aligns its strides.
    // `a_pitch_larger_than_the_width_skips_the_padding` is what covers that,
    // with a deliberately loose pitch. Verified by injecting the bug: the unit
    // test fails and this one does not.
    let _guard = card_guard();
    let Some(device) = ready() else { return };
    let Some(output) = paint(&device) else {
        drmkit_testkit::skipped("no connected connector with a usable CRTC");
        return;
    };
    let (w, h) = (output.width, output.height);
    println!("note: round-tripping crtc {} at {w}x{h}", output.crtc_id);

    let image = snapshot(&device, output.crtc_id).expect("snapshot");
    assert_eq!(image.width(), w);
    assert_eq!(image.height(), h);

    let at = |x: u32, y: u32| image.pixels()[(y * image.width() + x) as usize];
    assert_eq!(at(w / 4, h / 4), RED, "top-left");
    assert_eq!(at(3 * w / 4, h / 4), GREEN, "top-right");
    assert_eq!(at(w / 4, 3 * h / 4), BLUE, "bottom-left");
    assert_eq!(at(3 * w / 4, 3 * h / 4), WHITE, "bottom-right");
}

#[test]
#[ignore = "needs a DRM device and DRM master"]
fn without_the_atomic_cap_there_is_nothing_to_read_vkms() {
    // The kernel hides CRTC_X/CRTC_Y/CRTC_W/CRTC_H from clients that have not
    // set DRM_CLIENT_CAP_ATOMIC, so a non-atomic client sees planes with no
    // geometry and cannot place any of them. That is a documented
    // precondition of `snapshot`, and it is worth a test because the symptom
    // -- a blank capture from a CRTC that plainly has something on it -- looks
    // nothing like its cause. It is the same filtering that makes
    // `modetest -p` report no IN_FENCE_FD on hardware that has it.
    let _guard = card_guard();
    let Some(device) = ready() else { return };
    let Some(output) = paint(&device) else {
        drmkit_testkit::skipped("no connected connector with a usable CRTC");
        return;
    };
    let crtc_id = output.crtc_id;

    // With the cap, this CRTC captures.
    snapshot(&device, crtc_id).expect("the atomic client can read it");

    // Without it, and on the very same display, it must not.
    drop(output);
    drop(device);
    let Some(plain) = card() else { return };
    plain.enable_universal_planes().expect("universal planes");
    // Deliberately no enable_atomic().
    if plain.set_master().is_err() {
        drmkit_testkit::skipped("could not retake master");
        return;
    }
    // Every case in this binary holds the card lock, so nothing else logs.
    let captured: std::sync::Arc<std::sync::Mutex<Vec<String>>> = std::sync::Arc::default();
    let sink = std::sync::Arc::clone(&captured);
    drmkit_log::set_log_sink(move |_, message| {
        sink.lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(message.to_owned());
    });
    let result = snapshot(&plain, crtc_id);
    drmkit_log::reset_log_sink();
    assert!(
        matches!(
            result,
            Err(CaptureError::NothingReadable | CaptureError::NoMode)
        ),
        "a non-atomic client must fail rather than return a blank capture"
    );
    // Upstream's warning carries the counts that tell a missing cap from a
    // blank CRTC: how many planes the client was shown, and how many of them
    // were bound elsewhere or empty.
    if matches!(result, Err(CaptureError::NothingReadable)) {
        let captured = captured
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert!(
            captured
                .iter()
                .any(|message| message.contains("enumerated planes") && message.contains("unbound")),
            "the warning names the counts: {captured:?}"
        );
    }
}

/// A snapshot closes the GEM handles its framebuffer queries open.
///
/// GETFB2 opens a handle on the caller's descriptor for each buffer object,
/// and each one keeps the object alive until it is closed, so a recorder that
/// left them open would pin every buffer it ever captured. Handles are
/// numbered lowest-free, so the next one the kernel hands out counts what is
/// still open: the same number before and after means nothing leaked.
#[test]
#[ignore = "needs a DRM device and DRM master"]
fn a_snapshot_leaves_no_handle_open_vkms() {
    let _guard = card_guard();
    let Some(device) = ready() else { return };
    let Some(output) = paint(&device) else {
        drmkit_testkit::skipped("no connected connector with a usable CRTC");
        return;
    };
    let fb = device
        .get_crtc(output.crtc)
        .expect("crtc")
        .framebuffer()
        .expect("the painted framebuffer is bound");

    // Open a handle the way GETFB2 does, note its number, and close it.
    let next_handle = || {
        let info = device.get_planar_framebuffer(fb).expect("GETFB2");
        let handle = info.buffers()[0].expect("a buffer object");
        device
            .close_buffer(handle)
            .expect("close the probe's handle");
        u32::from(handle)
    };

    let before = next_handle();
    for _ in 0..3 {
        snapshot(&device, output.crtc_id).expect("snapshot");
    }
    let after = next_handle();
    assert_eq!(
        after,
        before,
        "three snapshots left {} GEM handles open",
        after.saturating_sub(before)
    );
}

/// A framebuffer whose image starts partway into its buffer object.
struct OffsetFramebuffer {
    handle: drm::buffer::Handle,
    size: (u32, u32),
    pitch: u32,
    offset: u32,
}

impl drm::buffer::PlanarBuffer for OffsetFramebuffer {
    fn size(&self) -> (u32, u32) {
        self.size
    }
    fn format(&self) -> drm::buffer::DrmFourcc {
        drm::buffer::DrmFourcc::Xrgb8888
    }
    fn modifier(&self) -> Option<drm::buffer::DrmModifier> {
        None
    }
    fn pitches(&self) -> [u32; 4] {
        [self.pitch, 0, 0, 0]
    }
    fn handles(&self) -> [Option<drm::buffer::Handle>; 4] {
        [Some(self.handle), None, None, None]
    }
    fn offsets(&self) -> [u32; 4] {
        [self.offset, 0, 0, 0]
    }
}

/// The image is read from where the framebuffer says it starts.
///
/// A framebuffer's first plane need not begin at byte 0 of its buffer
/// object: `offsets[0]` says where it does, as a sub-allocated or cropped
/// buffer has it. Reading from byte 0 shifts the capture by that many rows and
/// stops short of the last ones. Here each buffer row is painted with its own
/// row number, and the framebuffer starts sixteen rows in.
#[test]
#[ignore = "needs a DRM device and DRM master"]
fn the_image_starts_at_the_framebuffers_offset_vkms() {
    const SKIP_ROWS: u32 = 16;

    let _guard = card_guard();
    let Some(device) = ready() else { return };
    let resources = device.resource_handles().expect("resources");
    let Some((crtc, connector, mode)) = resources.connectors().iter().find_map(|handle| {
        let info = device.get_connector(*handle, false).ok()?;
        if info.state() != drm::control::connector::State::Connected {
            return None;
        }
        let mode = *info.modes().first()?;
        info.encoders().iter().find_map(|encoder| {
            let encoder = device.get_encoder(*encoder).ok()?;
            let crtc = *resources.filter_crtcs(encoder.possible_crtcs()).first()?;
            Some((crtc, *handle, mode))
        })
    }) else {
        drmkit_testkit::skipped("no connected connector with a usable CRTC");
        return;
    };
    let (width, height) = (u32::from(mode.size().0), u32::from(mode.size().1));

    let mut buffer = device
        .create_dumb_buffer(
            (width, height + SKIP_ROWS),
            drm::buffer::DrmFourcc::Xrgb8888,
            32,
        )
        .expect("dumb buffer");
    let pitch = drm::buffer::Buffer::pitch(&buffer);
    {
        let mut mapping = device.map_dumb_buffer(&mut buffer).expect("map");
        for (row, bytes) in mapping.chunks_exact_mut(pitch as usize).enumerate() {
            let color = 0xff00_0000 | u32::try_from(row).expect("small");
            for pixel in bytes[..width as usize * 4].chunks_exact_mut(4) {
                pixel.copy_from_slice(&color.to_ne_bytes());
            }
        }
    }
    let framebuffer = OffsetFramebuffer {
        handle: drm::buffer::Buffer::handle(&buffer),
        size: (width, height),
        pitch,
        offset: SKIP_ROWS * pitch,
    };
    let fb = device
        .add_planar_framebuffer(&framebuffer, drm::control::FbCmd2Flags::empty())
        .expect("a framebuffer with an offset");
    device
        .set_crtc(crtc, Some(fb), (0, 0), &[connector], Some(mode))
        .expect("set_crtc");

    let image = snapshot(&device, crtc.into());

    let _ = device.set_crtc(crtc, None, (0, 0), &[], None);
    let _ = device.destroy_framebuffer(fb);
    let _ = device.destroy_dumb_buffer(buffer);

    let image = image.expect("snapshot");
    let row_of = |y: u32| image.pixels()[(y * image.width()) as usize] & 0x00ff_ffff;
    assert_eq!(
        row_of(0),
        SKIP_ROWS,
        "the first row is the framebuffer's, not the buffer's"
    );
    assert_eq!(
        row_of(height - 1),
        height - 1 + SKIP_ROWS,
        "and so is the last"
    );
}
