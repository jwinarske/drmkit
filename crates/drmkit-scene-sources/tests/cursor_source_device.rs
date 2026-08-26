// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! A cursor scanned out as an ordinary layer, on a real device.
//!
//! Needs a dumb-capable KMS card and nothing else -- no DRM master, no commit
//! -- so it runs anywhere a card exists. `#[ignore]`d and run in the device
//! lanes with `--include-ignored`.

use std::sync::Mutex;
use std::time::Duration;

use drm::control::Device as _;
use drmkit_core::Device;
use drmkit_cursor::{Cursor, Frame};
use drmkit_dumb::MapAccess;
use drmkit_scene::LayerBufferSource as _;
use drmkit_scene_sources::CursorSource;

/// Shared with every other device test: they open the same card.
static CARD_LOCK: Mutex<()> = Mutex::new(());

fn card_guard() -> std::sync::MutexGuard<'static, ()> {
    CARD_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Sixteen, not four.
///
/// vkms sets `mode_config.min_width` and `min_height` to 10 and refuses
/// `AddFB2` below that in either dimension -- measured, 1 through 9 EINVAL and
/// 10 upwards fine, with `16x8` and `8x16` both refused. amdgpu accepts any
/// size, which is where a 4x4 fixture comes from and why it looks fine until
/// it meets the CI card. The reference's is 4x4 and skips there
/// ([drm-cxx#254](https://github.com/jwinarske/drm-cxx/issues/254)).
const W: u32 = 16;
const H: u32 = 16;

/// A distinct opaque value per pixel, so a stride or offset mistake shows up
/// as the wrong colour rather than as a plausible one.
fn pattern() -> Vec<u32> {
    (0..W * H)
        .map(|i| 0xFF00_0000 | (i * 0x0001_0203))
        .collect()
}

fn open_card() -> Option<Device> {
    let path = std::env::var("DRMKIT_TEST_CARD").unwrap_or_else(|_| "/dev/dri/card0".to_owned());
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
    // A render-only node accepts CREATE_DUMB and has no CRTC to scan out from,
    // so a cursor there could never be shown. Requiring a CRTC is what keeps
    // this on a card the source is actually for.
    let resources = device.resource_handles().ok()?;
    if resources.crtcs().is_empty() {
        drmkit_testkit::skipped(&format!("{path} has no CRTC"));
        return None;
    }
    Some(device)
}

/// Read the buffer back as packed `0xAARRGGBB`, honouring the stride.
fn read_back(source: &mut CursorSource) -> Vec<u32> {
    let (width, height) = source.size();
    let mapping = source.map(MapAccess::Read).expect("map");
    let mut out = Vec::with_capacity((width * height) as usize);
    for row in 0..height {
        let bytes = mapping.row(row).expect("row");
        for column in 0..width as usize {
            let at = column * 4;
            out.push(u32::from_le_bytes(
                bytes[at..at + 4].try_into().expect("four bytes"),
            ));
        }
    }
    out
}

/// One solid frame.
fn frame(width: u32, height: u32, colour: u32, delay: Duration) -> Frame {
    Frame {
        pixels: vec![colour; (width * height) as usize],
        width,
        height,
        xhot: 0,
        yhot: 0,
        delay,
    }
}

/// Acquire until the second frame is painted, or give up.
///
/// Polling rather than sleeping a fixed time: the clock starts at the first
/// acquire, so how many rounds this takes depends on the machine.
fn advance_to_second_frame(source: &mut CursorSource, colour: u32) -> bool {
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while std::time::Instant::now() < deadline {
        source.acquire().expect("acquire");
        if read_back(source)[0] == colour {
            return true;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    false
}

#[test]
#[ignore = "needs a DRM device"]
fn a_sprite_reaches_the_buffer_intact() {
    let _guard = card_guard();
    let Some(device) = open_card() else { return };

    let pixels = pattern();
    let mut source = CursorSource::from_argb(&device, &pixels, W, H, 1, 2).expect("cursor source");

    assert_eq!(source.hotspot(), (1, 2));
    assert_eq!(source.size(), (W, H));

    let acquired = source.acquire().expect("acquire");
    assert_ne!(
        acquired.fb_id, 0,
        "a cursor with no framebuffer cannot scan out"
    );

    assert_eq!(
        read_back(&mut source),
        pixels,
        "the sprite came back changed"
    );
}

#[test]
#[ignore = "needs a DRM device"]
fn the_buffer_is_sized_to_the_largest_frame() {
    // An animation whose frames differ in size still has to fit all of them,
    // and the plane is programmed once from `size()` -- so a buffer sized to
    // the first frame clips every larger one.
    let _guard = card_guard();
    let Some(device) = open_card() else { return };

    let frames = vec![
        frame(16, 16, 0xFF00_00FF, Duration::from_millis(50)),
        frame(24, 20, 0xFF00_FF00, Duration::from_millis(50)),
    ];
    let cursor = Cursor::from_frames(frames).expect("cursor");
    let source = CursorSource::new(&device, cursor).expect("cursor source");

    assert_eq!(source.size(), (24, 20));
}

#[test]
#[ignore = "needs a DRM device"]
fn a_smaller_frame_does_not_leave_the_previous_one_around_it() {
    // The failure this catches is a cursor that grows a halo: paint a large
    // frame, then a small one without clearing, and the large one's edges stay
    // lit. Only visible with frames of different sizes, which is why the
    // fixture has them.
    let _guard = card_guard();
    let Some(device) = open_card() else { return };

    let frames = vec![
        frame(20, 20, 0xFFFF_0000, Duration::from_millis(20)),
        frame(12, 12, 0xFF00_FF00, Duration::from_millis(20)),
    ];
    let cursor = Cursor::from_frames(frames).expect("cursor");
    let mut source = CursorSource::new(&device, cursor).expect("cursor source");

    // Frame 0 is painted at creation: red everywhere.
    assert!(read_back(&mut source).iter().all(|px| *px == 0xFFFF_0000));

    assert!(
        advance_to_second_frame(&mut source, 0xFF00_FF00),
        "the animation never advanced"
    );

    let pixels = read_back(&mut source);
    assert_eq!(pixels[0], 0xFF00_FF00, "top-left is the new frame");
    // Inside the 20x20 frame that was painted first, outside the 12x12 one
    // that replaced it.
    assert_eq!(
        pixels[19 * 20 + 19],
        0,
        "bottom-right still carries the previous frame"
    );
}

#[test]
#[ignore = "needs a DRM device"]
fn an_unchanged_frame_reports_no_damage() {
    // A repaint is a full-frame damage report, and reporting one every acquire
    // would keep a self-refresh panel awake for a cursor that is not moving.
    let _guard = card_guard();
    let Some(device) = open_card() else { return };

    let frames = vec![
        frame(16, 16, 0xFFFF_0000, Duration::from_secs(3600)),
        frame(16, 16, 0xFF00_FF00, Duration::from_secs(3600)),
    ];
    let cursor = Cursor::from_frames(frames).expect("cursor");
    let mut source = CursorSource::new(&device, cursor).expect("cursor source");

    // An hour per frame, so nothing advances between these two acquires.
    let first = source.acquire().expect("acquire");
    let second = source.acquire().expect("acquire");
    assert_eq!(first.fb_id, second.fb_id, "one buffer, handed out again");
    assert!(
        second.damage.is_empty(),
        "nothing changed, nothing to report"
    );
}

#[test]
#[ignore = "needs a DRM device"]
fn a_static_cursor_never_advances() {
    let _guard = card_guard();
    let Some(device) = open_card() else { return };

    let pixels = pattern();
    let mut source = CursorSource::from_argb(&device, &pixels, W, H, 0, 0).expect("cursor source");
    assert!(!source.cursor().is_animated());

    for _ in 0..4 {
        source.acquire().expect("acquire");
    }
    assert_eq!(read_back(&mut source), pixels, "a static cursor repainted");
}
