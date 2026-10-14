// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! A YUV layer's `COLOR_ENCODING` and `COLOR_RANGE`, committed for real and
//! read back from the kernel.
//!
//! The emission census pins what the scene *asks* for (P-52). This is the
//! other half: that a real commit carrying the request is accepted, and that
//! the plane then holds what the layer asked for. Not every driver takes
//! either property -- msm on the SA8155P has neither -- so the second case
//! puts the same request on an `NV12` plane without them, where the scene has
//! to write nothing and the commit still has to land.

mod common;

use common::{Fixture, card_guard, fixture, open_card};
use drmkit_fmt::fourcc;
use drmkit_planes::PlaneCapabilities;
use drmkit_planes::PlaneType;
use drmkit_scene::{
    AcquiredBuffer, ColorEncoding, ColorRange, CommitReport, DisplayParams, LayerBufferSource,
    LayerHandle, Placement, Rect, SourceError, SourceFormat,
};

/// One `NV12` dumb buffer, presented every frame.
struct Nv12Source {
    buffer: drmkit_dumb::Buffer,
    width: u32,
    height: u32,
}

impl LayerBufferSource for Nv12Source {
    fn acquire(&mut self) -> Result<AcquiredBuffer, SourceError> {
        let fb_id = self
            .buffer
            .fb_id()
            .ok_or(SourceError::Failed(rustix::io::Errno::INVAL))?;
        Ok(AcquiredBuffer {
            fb_id,
            ..AcquiredBuffer::default()
        })
    }

    fn release(&mut self, _acquired: AcquiredBuffer) {}

    fn format(&self) -> SourceFormat {
        SourceFormat {
            fourcc: fourcc::NV12,
            modifier: 0,
            width: self.width,
            height: self.height,
        }
    }
}

/// The plane's encoding and range follow the layer, through real commits.
#[test]
#[ignore = "needs a DRM device and DRM master"]
fn a_yuv_layers_encoding_and_range_are_what_the_plane_holds_vkms() {
    let _guard = card_guard();
    let Some(device) = open_card() else { return };
    let Some(mut fx) = fixture(device) else {
        drmkit_testkit::skipped("no connected output");
        return;
    };

    // A plane that takes NV12 and lists all four entries the case moves
    // between. The values come from the registry's own probe, not from the
    // scene's property map, so a map that resolved them wrongly is caught.
    let target = fx.planes().find_map(|plane| {
        let values = (
            plane.color_encodings.bt601?,
            plane.color_encodings.bt709?,
            plane.color_ranges.limited?,
            plane.color_ranges.full?,
        );
        (takes_nv12(plane) && plane.has_color_encoding && plane.has_color_range)
            .then_some((plane.id, values))
    });
    let Some((plane_id, (bt601, bt709, limited, full))) = target else {
        drmkit_testkit::skipped(&format!(
            "no plane on this CRTC takes NV12 with BT.601, BT.709, limited and full range ({})",
            census(&fx)
        ));
        return;
    };
    let Some(handle) = add_yuv_layer(&mut fx, plane_id) else {
        return;
    };

    ask(&mut fx, handle, ColorEncoding::Bt601, ColorRange::Full);
    let report = fx.commit().expect("the BT.601 full-range frame");
    assert_placed(&report, handle, plane_id);
    assert_eq!(
        (
            fx.plane_property(plane_id, "COLOR_ENCODING"),
            fx.plane_property(plane_id, "COLOR_RANGE")
        ),
        (Some(bt601), Some(full)),
        "the plane holds BT.601 and full range, as the layer asked"
    );
    hold_for_a_look("BT.601, full range");

    ask(&mut fx, handle, ColorEncoding::Bt709, ColorRange::Limited);
    fx.commit().expect("the BT.709 limited-range frame");
    assert_eq!(
        (
            fx.plane_property(plane_id, "COLOR_ENCODING"),
            fx.plane_property(plane_id, "COLOR_RANGE")
        ),
        (Some(bt709), Some(limited)),
        "a changed request reaches the plane"
    );
    hold_for_a_look("BT.709, limited range");

    fx.teardown();
}

/// On a YUV plane without either property, a request for one still commits.
///
/// The scene has nothing to write there, and must write nothing: a stray
/// `COLOR_ENCODING` against a plane without it fails the whole commit.
#[test]
#[ignore = "needs a DRM device and DRM master"]
fn a_yuv_layer_on_a_plane_without_colorimetry_still_commits_vkms() {
    let _guard = card_guard();
    let Some(device) = open_card() else { return };
    let Some(mut fx) = fixture(device) else {
        drmkit_testkit::skipped("no connected output");
        return;
    };

    let target = fx
        .planes()
        .find(|plane| takes_nv12(plane) && !plane.has_color_encoding && !plane.has_color_range)
        .map(|plane| plane.id);
    let Some(plane_id) = target else {
        drmkit_testkit::skipped(&format!(
            "every plane on this CRTC that takes NV12 has COLOR_ENCODING or COLOR_RANGE ({})",
            census(&fx)
        ));
        return;
    };
    let Some(handle) = add_yuv_layer(&mut fx, plane_id) else {
        return;
    };

    ask(&mut fx, handle, ColorEncoding::Bt601, ColorRange::Full);
    let report = fx
        .commit()
        .expect("a request the plane cannot carry still commits");
    assert_placed(&report, handle, plane_id);
    assert!(
        fx.plane_framebuffer(plane_id).is_some_and(|fb| fb != 0),
        "and the plane is scanning the layer out"
    );
    hold_for_a_look("BT.601 full range asked of a plane without either property");

    fx.teardown();
}

/// A plane the layer can be pinned to that scans out linear `NV12`.
fn takes_nv12(plane: &PlaneCapabilities) -> bool {
    plane.plane_type != PlaneType::Cursor && plane.scans_out_linear(fourcc::NV12)
}

/// What this CRTC's planes offer, so a skip on new hardware says which half
/// was missing.
fn census(fx: &Fixture) -> String {
    let count = |test: &dyn Fn(&PlaneCapabilities) -> bool| fx.planes().filter(|p| test(p)).count();
    format!(
        "{} planes: {} take NV12, {} have COLOR_ENCODING, {} have COLOR_RANGE",
        count(&|_| true),
        count(&|plane| takes_nv12(plane)),
        count(&|plane| plane.has_color_encoding),
        count(&|plane| plane.has_color_range),
    )
}

/// Add a full-screen `NV12` layer pinned to `plane_id`. `None`, having said
/// why, when the driver will not allocate one.
fn add_yuv_layer(fx: &mut Fixture, plane_id: u32) -> Option<LayerHandle> {
    let (width, height) = fx.mode_size();
    let (width, height) = (width & !1, height & !1);
    let buffer = match red_nv12(&fx.device, width, height) {
        Ok(buffer) => buffer,
        Err(error) => {
            drmkit_testkit::skipped(&format!(
                "this driver refuses an NV12 dumb buffer ({error})"
            ));
            return None;
        }
    };
    let handle = fx.scene.add_layer(Box::new(Nv12Source {
        buffer,
        width,
        height,
    }));
    let full_screen = Rect {
        x: 0,
        y: 0,
        w: width,
        h: height,
    };
    let layer = fx.scene.layer_mut(handle)?;
    layer.set_pinned_plane(Some(plane_id));
    layer.set_display(DisplayParams {
        src_rect: full_screen,
        dst_rect: full_screen,
        ..DisplayParams::default()
    });
    Some(handle)
}

/// Ask for `encoding` and `range`, leaving the rest of the layer alone.
fn ask(fx: &mut Fixture, handle: LayerHandle, encoding: ColorEncoding, range: ColorRange) {
    let layer = fx.scene.layer_mut(handle).expect("the layer");
    let display = DisplayParams {
        color_encoding: Some(encoding),
        color_range: Some(range),
        ..*layer.display()
    };
    layer.set_display(display);
}

fn assert_placed(report: &CommitReport, handle: LayerHandle, plane_id: u32) {
    let placement = report
        .placements
        .iter()
        .find(|entry| entry.layer == handle.layer_id())
        .expect("the layer is reported");
    assert_eq!(
        (placement.placement, placement.plane_id),
        (Placement::AssignedToPlane, Some(plane_id)),
        "the NV12 layer is on the plane it was pinned to"
    );
}

/// An `NV12` buffer filled with a saturated red in BT.601 limited range, so
/// a screen shows the matrix and range changing: through BT.709 it shifts
/// toward orange, and read as full range it darkens.
fn red_nv12(
    device: &drmkit_core::Device,
    width: u32,
    height: u32,
) -> Result<drmkit_dumb::Buffer, drmkit_dumb::DumbError> {
    let mut buffer = drmkit_dumb::Buffer::create_planar(device, fourcc::NV12, width, height)?;
    let luma_bytes = buffer.stride() as usize * height as usize;
    let data = buffer.data_mut();
    data[..luma_bytes].fill(81);
    for pair in data[luma_bytes..].chunks_exact_mut(2) {
        pair[0] = 90;
        pair[1] = 240;
    }
    Ok(buffer)
}

/// Leave the frame up for `DRMKIT_HOLD_SECS` seconds, for someone watching
/// the screen. Unset, the case does not wait.
fn hold_for_a_look(what: &str) {
    let Some(secs) = std::env::var("DRMKIT_HOLD_SECS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
    else {
        return;
    };
    eprintln!("showing {what} for {secs} s");
    std::thread::sleep(std::time::Duration::from_secs(secs));
}
