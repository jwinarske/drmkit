// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! What a connected display says about itself.
//!
//! EDID is a blob the display hands over on connect, describing its make,
//! physical size, colour primaries, and what HDR it can accept. It is also
//! notoriously malformed in the field — truncated, mis-checksummed, carrying
//! extension blocks that disagree with the base block — so parsing it is
//! delegated to [`libdisplay-info`], which exists precisely because everyone
//! who wrote their own got it wrong.
//!
//! This module is the translation layer: `libdisplay-info`'s view into the
//! shapes the rest of drmkit uses, with the same field selection upstream
//! makes.
//!
//! Behind the `edid` feature, because the binding links a system library and
//! the cross lanes have no sysroot for it.
//!
//! [`libdisplay-info`]: https://gitlab.freedesktop.org/emersion/libdisplay-info

use libdisplay_info::info::Info;

/// A CIE 1931 chromaticity coordinate.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Chromaticity {
    /// x coordinate.
    pub x: f32,
    /// y coordinate.
    pub y: f32,
}

/// The display's colour primaries and white point.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct ColorimetryInfo {
    /// Red primary.
    pub red: Chromaticity,
    /// Green primary.
    pub green: Chromaticity,
    /// Blue primary.
    pub blue: Chromaticity,
    /// Default white point.
    pub white: Chromaticity,
    /// Whether the primaries are present. When false the three above are
    /// meaningless rather than zero-valued.
    pub has_primaries: bool,
    /// Whether the white point is present.
    pub has_default_white: bool,
}

/// The display's HDR static metadata block.
///
/// One field per flag in the CTA block, rather than a bitflags type: this is a
/// transcription of a wire format, and a reader checking it against the
/// standard should find the standard's field names.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "the shape is CTA-861's, not a design choice"
)]
pub struct HdrStaticMetadata {
    /// Peak luminance the display wants content mastered to, in nits.
    pub desired_content_max_luminance: f32,
    /// Desired frame-average peak, in nits.
    pub desired_content_max_frame_avg_luminance: f32,
    /// Desired black level, in nits.
    pub desired_content_min_luminance: f32,
    /// Static metadata type 1 is supported.
    pub type1: bool,
    /// Traditional SDR gamma.
    pub traditional_sdr: bool,
    /// Traditional HDR gamma.
    pub traditional_hdr: bool,
    /// SMPTE ST 2084 — HDR10.
    pub pq: bool,
    /// Hybrid Log-Gamma.
    pub hlg: bool,
}

/// Signal colorimetry encodings the display accepts beyond the default.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "one field per encoding in the CTA colorimetry data block"
)]
pub struct SupportedColorimetry {
    /// BT.2020 constant-luminance `YCbCr`.
    pub bt2020_cycc: bool,
    /// BT.2020 non-constant-luminance `YCbCr`.
    pub bt2020_ycc: bool,
    /// BT.2020 RGB.
    pub bt2020_rgb: bool,
    /// SMPTE ST 2113 — P3D65 and P3DCI.
    pub st2113_rgb: bool,
    /// BT.2100 `ICtCp`.
    pub ictcp: bool,
}

/// What a connector's attached display reports.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ConnectorInfo {
    /// Make and model joined, for display to a person.
    pub name: String,
    /// Manufacturer — a company name where the PNP ID is known, else the ID.
    pub make: String,
    /// Product name or model.
    pub model: String,
    /// Per-unit serial number, where the display gives one.
    pub serial: Option<String>,
    /// Physical width in millimetres.
    pub width_mm: i32,
    /// Physical height in millimetres.
    pub height_mm: i32,
    /// Colour primaries, where present.
    pub colorimetry: Option<ColorimetryInfo>,
    /// HDR static metadata, where the display advertises any.
    pub hdr: Option<HdrStaticMetadata>,
    /// Extra colorimetry encodings, where any are advertised.
    pub wide_gamut: Option<SupportedColorimetry>,
}

/// Why an EDID blob could not be read.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum EdidError {
    /// The blob was not parseable as EDID at all.
    ///
    /// A truncated read, a blob that is not EDID, or one malformed past what
    /// the parser is willing to guess at.
    #[error("EDID blob could not be parsed")]
    Unparseable,
}

/// Read a display's EDID blob.
///
/// # Errors
///
/// [`EdidError::Unparseable`] if the blob is not EDID.
pub fn parse_edid(blob: &[u8]) -> Result<ConnectorInfo, EdidError> {
    let info = Info::parse_edid(blob).map_err(|_| EdidError::Unparseable)?;

    let make = info.make().unwrap_or_default();
    let model = info.model().unwrap_or_default();
    // Joined the way upstream joins them, with a space only when both halves
    // are there — a display that reports only a model should read as its model,
    // not as a leading space.
    let name = match (make.is_empty(), model.is_empty()) {
        (false, false) => format!("{make} {model}"),
        (false, true) => make.clone(),
        (true, false) => model.clone(),
        (true, true) => String::new(),
    };

    let (mut width_mm, mut height_mm) = (0, 0);
    if let Some(edid) = info.edid() {
        let size = edid.screen_size();
        // EDID records whole centimetres; everything downstream works in
        // millimetres, so convert here rather than leaving two units in play.
        width_mm = size.width_cm.unwrap_or(0) * 10;
        height_mm = size.height_cm.unwrap_or(0) * 10;
    }

    let primaries = info.default_color_primaries();
    let colorimetry =
        (primaries.has_primaries || primaries.has_default_white_point).then(|| ColorimetryInfo {
            red: Chromaticity {
                x: primaries.primary[0].x,
                y: primaries.primary[0].y,
            },
            green: Chromaticity {
                x: primaries.primary[1].x,
                y: primaries.primary[1].y,
            },
            blue: Chromaticity {
                x: primaries.primary[2].x,
                y: primaries.primary[2].y,
            },
            white: Chromaticity {
                x: primaries.default_white.x,
                y: primaries.default_white.y,
            },
            has_primaries: primaries.has_primaries,
            has_default_white: primaries.has_default_white_point,
        });

    let raw_hdr = info.hdr_static_metadata();
    // `traditional_sdr` is deliberately absent from this test.
    // libdisplay-info sets it for any base EDID, so a gate that counted it
    // would be `Some` for every display ever made — the same defect as
    // upstream's, arrived at differently. This is *HDR* static metadata:
    // traditional SDR gamma alone means the display said nothing about HDR.
    let advertises_hdr = raw_hdr.type1 || raw_hdr.traditional_hdr || raw_hdr.pq || raw_hdr.hlg;
    let hdr = advertises_hdr.then_some(HdrStaticMetadata {
        desired_content_max_luminance: raw_hdr.desired_content_max_luminance,
        desired_content_max_frame_avg_luminance: raw_hdr.desired_content_max_frame_avg_luminance,
        desired_content_min_luminance: raw_hdr.desired_content_min_luminance,
        type1: raw_hdr.type1,
        traditional_sdr: raw_hdr.traditional_sdr,
        traditional_hdr: raw_hdr.traditional_hdr,
        pq: raw_hdr.pq,
        hlg: raw_hdr.hlg,
    });

    let raw_gamut = info.supported_signal_colorimetry();
    let any_gamut = raw_gamut.bt2020_cycc
        || raw_gamut.bt2020_ycc
        || raw_gamut.bt2020_rgb
        || raw_gamut.st2113_rgb
        || raw_gamut.ictcp;
    let wide_gamut = any_gamut.then_some(SupportedColorimetry {
        bt2020_cycc: raw_gamut.bt2020_cycc,
        bt2020_ycc: raw_gamut.bt2020_ycc,
        bt2020_rgb: raw_gamut.bt2020_rgb,
        st2113_rgb: raw_gamut.st2113_rgb,
        ictcp: raw_gamut.ictcp,
    });

    Ok(ConnectorInfo {
        name,
        make,
        model,
        serial: info.serial(),
        width_mm,
        height_mm,
        colorimetry,
        hdr,
        wide_gamut,
    })
}
