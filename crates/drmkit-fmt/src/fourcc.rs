// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! DRM `FourCC` pixel-format codes and their bit depths.
//!
//! Port of the `format_bpp` half of `src/core/format.hpp` / `format.cpp`. The
//! constants mirror `drm_fourcc.h`; they are spelled out here rather than bound
//! from the header so the crate stays `no_std` and needs no build script.

/// Build a `FourCC` code from four ASCII bytes, matching the `fourcc_code`
/// macro in `drm_fourcc.h`.
///
/// ```
/// use drmkit_fmt::fourcc;
/// assert_eq!(fourcc::code(b'X', b'R', b'2', b'4'), fourcc::XRGB8888);
/// ```
#[must_use]
pub const fn code(a: u8, b: u8, c: u8, d: u8) -> u32 {
    (a as u32) | ((b as u32) << 8) | ((c as u32) << 16) | ((d as u32) << 24)
}

/// `C8` (`C8`).
pub const C8: u32 = code(b'C', b'8', b' ', b' ');

/// `RGB332` (`RGB8`).
pub const RGB332: u32 = code(b'R', b'G', b'B', b'8');

/// `BGR233` (`BGR8`).
pub const BGR233: u32 = code(b'B', b'G', b'R', b'8');

/// `XRGB4444` (`XR12`).
pub const XRGB4444: u32 = code(b'X', b'R', b'1', b'2');

/// `XBGR4444` (`XB12`).
pub const XBGR4444: u32 = code(b'X', b'B', b'1', b'2');

/// `RGBX4444` (`RX12`).
pub const RGBX4444: u32 = code(b'R', b'X', b'1', b'2');

/// `BGRX4444` (`BX12`).
pub const BGRX4444: u32 = code(b'B', b'X', b'1', b'2');

/// `ARGB4444` (`AR12`).
pub const ARGB4444: u32 = code(b'A', b'R', b'1', b'2');

/// `ABGR4444` (`AB12`).
pub const ABGR4444: u32 = code(b'A', b'B', b'1', b'2');

/// `RGBA4444` (`RA12`).
pub const RGBA4444: u32 = code(b'R', b'A', b'1', b'2');

/// `BGRA4444` (`BA12`).
pub const BGRA4444: u32 = code(b'B', b'A', b'1', b'2');

/// `XRGB1555` (`XR15`).
pub const XRGB1555: u32 = code(b'X', b'R', b'1', b'5');

/// `XBGR1555` (`XB15`).
pub const XBGR1555: u32 = code(b'X', b'B', b'1', b'5');

/// `RGBX5551` (`RX15`).
pub const RGBX5551: u32 = code(b'R', b'X', b'1', b'5');

/// `BGRX5551` (`BX15`).
pub const BGRX5551: u32 = code(b'B', b'X', b'1', b'5');

/// `ARGB1555` (`AR15`).
pub const ARGB1555: u32 = code(b'A', b'R', b'1', b'5');

/// `ABGR1555` (`AB15`).
pub const ABGR1555: u32 = code(b'A', b'B', b'1', b'5');

/// `RGBA5551` (`RA15`).
pub const RGBA5551: u32 = code(b'R', b'A', b'1', b'5');

/// `BGRA5551` (`BA15`).
pub const BGRA5551: u32 = code(b'B', b'A', b'1', b'5');

/// `RGB565` (`RG16`).
pub const RGB565: u32 = code(b'R', b'G', b'1', b'6');

/// `BGR565` (`BG16`).
pub const BGR565: u32 = code(b'B', b'G', b'1', b'6');

/// `RGB888` (`RG24`).
pub const RGB888: u32 = code(b'R', b'G', b'2', b'4');

/// `BGR888` (`BG24`).
pub const BGR888: u32 = code(b'B', b'G', b'2', b'4');

/// `XRGB8888` (`XR24`).
pub const XRGB8888: u32 = code(b'X', b'R', b'2', b'4');

/// `XBGR8888` (`XB24`).
pub const XBGR8888: u32 = code(b'X', b'B', b'2', b'4');

/// `RGBX8888` (`RX24`).
pub const RGBX8888: u32 = code(b'R', b'X', b'2', b'4');

/// `BGRX8888` (`BX24`).
pub const BGRX8888: u32 = code(b'B', b'X', b'2', b'4');

/// `ARGB8888` (`AR24`).
pub const ARGB8888: u32 = code(b'A', b'R', b'2', b'4');

/// `ABGR8888` (`AB24`).
pub const ABGR8888: u32 = code(b'A', b'B', b'2', b'4');

/// `RGBA8888` (`RA24`).
pub const RGBA8888: u32 = code(b'R', b'A', b'2', b'4');

/// `BGRA8888` (`BA24`).
pub const BGRA8888: u32 = code(b'B', b'A', b'2', b'4');

/// `XRGB2101010` (`XR30`).
pub const XRGB2101010: u32 = code(b'X', b'R', b'3', b'0');

/// `XBGR2101010` (`XB30`).
pub const XBGR2101010: u32 = code(b'X', b'B', b'3', b'0');

/// `RGBX1010102` (`RX30`).
pub const RGBX1010102: u32 = code(b'R', b'X', b'3', b'0');

/// `BGRX1010102` (`BX30`).
pub const BGRX1010102: u32 = code(b'B', b'X', b'3', b'0');

/// `ARGB2101010` (`AR30`).
pub const ARGB2101010: u32 = code(b'A', b'R', b'3', b'0');

/// `ABGR2101010` (`AB30`).
pub const ABGR2101010: u32 = code(b'A', b'B', b'3', b'0');

/// `RGBA1010102` (`RA30`).
pub const RGBA1010102: u32 = code(b'R', b'A', b'3', b'0');

/// `BGRA1010102` (`BA30`).
pub const BGRA1010102: u32 = code(b'B', b'A', b'3', b'0');

/// `XRGB16161616F` (`XR4H`).
pub const XRGB16161616F: u32 = code(b'X', b'R', b'4', b'H');

/// `XBGR16161616F` (`XB4H`).
pub const XBGR16161616F: u32 = code(b'X', b'B', b'4', b'H');

/// `ARGB16161616F` (`AR4H`).
pub const ARGB16161616F: u32 = code(b'A', b'R', b'4', b'H');

/// `ABGR16161616F` (`AB4H`).
pub const ABGR16161616F: u32 = code(b'A', b'B', b'4', b'H');

/// `Y210` (`Y210`).
pub const Y210: u32 = code(b'Y', b'2', b'1', b'0');

/// `Y212` (`Y212`).
pub const Y212: u32 = code(b'Y', b'2', b'1', b'2');

/// `Y216` (`Y216`).
pub const Y216: u32 = code(b'Y', b'2', b'1', b'6');

/// `NV12` (`NV12`).
pub const NV12: u32 = code(b'N', b'V', b'1', b'2');

/// `NV21` (`NV21`).
pub const NV21: u32 = code(b'N', b'V', b'2', b'1');

/// `NV15` (`NV15`).
pub const NV15: u32 = code(b'N', b'V', b'1', b'5');

/// `NV16` (`NV16`).
pub const NV16: u32 = code(b'N', b'V', b'1', b'6');

/// `NV61` (`NV61`).
pub const NV61: u32 = code(b'N', b'V', b'6', b'1');

/// `NV20` (`NV20`).
pub const NV20: u32 = code(b'N', b'V', b'2', b'0');

/// `NV24` (`NV24`).
pub const NV24: u32 = code(b'N', b'V', b'2', b'4');

/// `NV42` (`NV42`).
pub const NV42: u32 = code(b'N', b'V', b'4', b'2');

/// `P010` (`P010`).
pub const P010: u32 = code(b'P', b'0', b'1', b'0');

/// `P012` (`P012`).
pub const P012: u32 = code(b'P', b'0', b'1', b'2');

/// `P016` (`P016`).
pub const P016: u32 = code(b'P', b'0', b'1', b'6');

/// `P210` (`P210`).
pub const P210: u32 = code(b'P', b'2', b'1', b'0');

/// `YUV410` (`YUV9`).
pub const YUV410: u32 = code(b'Y', b'U', b'V', b'9');

/// `YVU410` (`YVU9`).
pub const YVU410: u32 = code(b'Y', b'V', b'U', b'9');

/// `YUV411` (`YU11`).
pub const YUV411: u32 = code(b'Y', b'U', b'1', b'1');

/// `YVU411` (`YV11`).
pub const YVU411: u32 = code(b'Y', b'V', b'1', b'1');

/// `YUV420` (`YU12`).
pub const YUV420: u32 = code(b'Y', b'U', b'1', b'2');

/// `YVU420` (`YV12`).
pub const YVU420: u32 = code(b'Y', b'V', b'1', b'2');

/// `YUV422` (`YU16`).
pub const YUV422: u32 = code(b'Y', b'U', b'1', b'6');

/// `YVU422` (`YV16`).
pub const YVU422: u32 = code(b'Y', b'V', b'1', b'6');

/// `YUV444` (`YU24`).
pub const YUV444: u32 = code(b'Y', b'U', b'2', b'4');

/// `YVU444` (`YV24`).
pub const YVU444: u32 = code(b'Y', b'V', b'2', b'4');

/// `YUYV` (`YUYV`).
pub const YUYV: u32 = code(b'Y', b'U', b'Y', b'V');

/// `YVYU` (`YVYU`).
pub const YVYU: u32 = code(b'Y', b'V', b'Y', b'U');

/// `UYVY` (`UYVY`).
pub const UYVY: u32 = code(b'U', b'Y', b'V', b'Y');

/// `VYUY` (`VYUY`).
pub const VYUY: u32 = code(b'V', b'Y', b'U', b'Y');

/// `AYUV` (`AYUV`).
pub const AYUV: u32 = code(b'A', b'Y', b'U', b'V');

/// The `FourCC`'s conventional name, or `"unknown"`.
///
/// For logging and diagnostics. Port of `drm::format_name`.
///
/// ```
/// use drmkit_fmt::{format_name, fourcc};
/// assert_eq!(format_name(fourcc::XRGB8888), "XRGB8888");
/// assert_eq!(format_name(fourcc::NV12), "NV12");
/// assert_eq!(format_name(0), "unknown");
/// ```
#[must_use]
pub const fn format_name(format: u32) -> &'static str {
    match format {
        C8 => "C8",
        RGB332 => "RGB332",
        BGR233 => "BGR233",
        XRGB4444 => "XRGB4444",
        XBGR4444 => "XBGR4444",
        RGBX4444 => "RGBX4444",
        BGRX4444 => "BGRX4444",
        ARGB4444 => "ARGB4444",
        ABGR4444 => "ABGR4444",
        RGBA4444 => "RGBA4444",
        BGRA4444 => "BGRA4444",
        XRGB1555 => "XRGB1555",
        XBGR1555 => "XBGR1555",
        RGBX5551 => "RGBX5551",
        BGRX5551 => "BGRX5551",
        ARGB1555 => "ARGB1555",
        ABGR1555 => "ABGR1555",
        RGBA5551 => "RGBA5551",
        BGRA5551 => "BGRA5551",
        RGB565 => "RGB565",
        BGR565 => "BGR565",
        RGB888 => "RGB888",
        BGR888 => "BGR888",
        XRGB8888 => "XRGB8888",
        XBGR8888 => "XBGR8888",
        RGBX8888 => "RGBX8888",
        BGRX8888 => "BGRX8888",
        ARGB8888 => "ARGB8888",
        ABGR8888 => "ABGR8888",
        RGBA8888 => "RGBA8888",
        BGRA8888 => "BGRA8888",
        XRGB2101010 => "XRGB2101010",
        XBGR2101010 => "XBGR2101010",
        RGBX1010102 => "RGBX1010102",
        BGRX1010102 => "BGRX1010102",
        ARGB2101010 => "ARGB2101010",
        ABGR2101010 => "ABGR2101010",
        RGBA1010102 => "RGBA1010102",
        BGRA1010102 => "BGRA1010102",
        XRGB16161616F => "XRGB16161616F",
        XBGR16161616F => "XBGR16161616F",
        ARGB16161616F => "ARGB16161616F",
        ABGR16161616F => "ABGR16161616F",
        Y210 => "Y210",
        Y212 => "Y212",
        Y216 => "Y216",
        NV12 => "NV12",
        NV21 => "NV21",
        NV15 => "NV15",
        NV16 => "NV16",
        NV61 => "NV61",
        NV20 => "NV20",
        NV24 => "NV24",
        NV42 => "NV42",
        P010 => "P010",
        P012 => "P012",
        P016 => "P016",
        P210 => "P210",
        YUV410 => "YUV410",
        YVU410 => "YVU410",
        YUV411 => "YUV411",
        YVU411 => "YVU411",
        YUV420 => "YUV420",
        YVU420 => "YVU420",
        YUV422 => "YUV422",
        YVU422 => "YVU422",
        YUV444 => "YUV444",
        YVU444 => "YVU444",
        YUYV => "YUYV",
        YVYU => "YVYU",
        UYVY => "UYVY",
        VYUY => "VYUY",
        AYUV => "AYUV",
        _ => "unknown",
    }
}

/// Bits per pixel for a packed format, or `0` when a single image-level scalar
/// would be misleading.
///
/// Planar and semi-planar layouts (the `NV*` family, `P010`/`P012`/`P016`,
/// `YUV*`/`YVU*`) report `0`: per-plane depth differs between the luma and
/// chroma planes. Callers needing total frame bytes go through
/// [`crate::scanout_cost_bytes`], which expands those cases by chroma
/// subsampling. A caller that reads `0` as "free" has misread the contract.
///
/// # Parity note
///
/// The packed 4:2:2 formats `YUYV`/`YVYU`/`UYVY`/`VYUY` and `AYUV` also report
/// `0`, because the C++ `format_bpp` switch does not list them. They are
/// genuinely 16bpp and 32bpp respectively, so this under-reports them — but
/// changing it would diverge from the reference implementation for a value
/// that is public API on both sides. It costs nothing downstream: the cost
/// model's fallback for an unlisted format is a conservative 32bpp, which is
/// what the C++ already charges them.
///
/// ```
/// use drmkit_fmt::{format_bpp, fourcc};
/// assert_eq!(format_bpp(fourcc::XRGB8888), 32);
/// assert_eq!(format_bpp(fourcc::RGB565), 16);
/// assert_eq!(format_bpp(fourcc::ARGB16161616F), 64);
/// assert_eq!(format_bpp(fourcc::Y210), 32, "packed 4:2:2 in u16 samples");
/// assert_eq!(format_bpp(fourcc::NV12), 0, "planar: per-plane depth differs");
/// ```
#[must_use]
pub const fn format_bpp(format: u32) -> u32 {
    match format {
        C8 | RGB332 | BGR233 => 8,
        XRGB4444 | XBGR4444 | RGBX4444 | BGRX4444 | ARGB4444 | ABGR4444 | RGBA4444 | BGRA4444
        | XRGB1555 | XBGR1555 | RGBX5551 | BGRX5551 | ARGB1555 | ABGR1555 | RGBA5551 | BGRA5551
        | RGB565 | BGR565 => 16,
        RGB888 | BGR888 => 24,
        XRGB8888 | XBGR8888 | RGBX8888 | BGRX8888 | ARGB8888 | ABGR8888 | RGBA8888 | BGRA8888
        | XRGB2101010 | XBGR2101010 | RGBX1010102 | BGRX1010102 | ARGB2101010 | ABGR2101010
        | RGBA1010102 | BGRA1010102 | Y210 | Y212 | Y216 => 32,
        XRGB16161616F | XBGR16161616F | ARGB16161616F | ABGR16161616F => 64,
        // Planar (NV*-family, P010/P012/P016, YUV*/YVU*) report 0: per-plane
        // bpp varies between the luma and chroma planes, so one image-level
        // scalar is misleading. Also everything not in the C++ switch.
        _ => 0,
    }
}
