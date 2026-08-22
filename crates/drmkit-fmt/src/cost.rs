// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! Scanout bandwidth cost for the allocator's placement scoring.

use crate::fourcc::{self, format_bpp};
use crate::modifier::BandwidthClass;

/// Total bytes the display engine fetches for one frame, summed over all
/// planes.
///
/// Packed formats come straight from [`format_bpp`]. Planar YUV — which
/// `format_bpp` reports as `0`, since per-plane depth differs — is expanded by
/// chroma subsampling and sample width. An unrecognized format falls back to a
/// conservative 32bpp so the cost model never scores an unknown layer as free.
#[must_use]
const fn frame_bytes(fourcc_code: u32, width: u32, height: u32) -> u64 {
    let pixels = (width as u64) * (height as u64);

    let bpp = format_bpp(fourcc_code);
    if bpp != 0 {
        // bpp is always a multiple of 8 for the packed formats.
        return pixels * (bpp as u64 / 8);
    }

    match fourcc_code {
        // 4:2:0, 8-bit (luma + half-res chroma): 1.5 bytes/px.
        fourcc::NV12 | fourcc::NV21 | fourcc::YUV420 | fourcc::YVU420 => pixels * 3 / 2,

        // 4:2:2, 8-bit: 2 bytes/px.
        fourcc::NV16 | fourcc::NV61 | fourcc::YUV422 | fourcc::YVU422 => pixels * 2,

        // 3 bytes/px, two ways: 4:4:4 8-bit (full-res Y+Cb+Cr) and 4:2:0 in
        // 16-bit containers (2B luma + 1B half-res chroma).
        fourcc::NV24
        | fourcc::NV42
        | fourcc::YUV444
        | fourcc::YVU444
        | fourcc::P010
        | fourcc::P012
        | fourcc::P016 => pixels * 3,

        // Unknown layout, including 4:2:2 16-bit like P210 (exactly 4 bytes/px):
        // assume 32bpp rather than score it free.
        _ => pixels * 4,
    }
}

/// The default worst-case compression ratio used when a modifier's
/// [`BandwidthClass`] is [`Compression`](`BandwidthClass`::Compression).
pub const DEFAULT_COMPRESSED_RATIO: f32 = 0.6;

/// Approximate per-frame scanout **read** in bytes, for the allocator's
/// content-priority and spatial-split cost model.
///
/// Compression is data-dependent, so this scores with a conservative worst-case
/// ratio: `Linear` and `Tiling` move the full byte count; only real compression
/// cuts the fetch.
///
/// # Contract
///
/// `class` **must** be [`classify`](crate::classify) of the modifier the
/// display engine actually fetches — never a producer-internal compression
/// state. On render-offload `SoCs` (`PowerVR` into `tidss`/`rcar-du`) the GPU may use
/// PVRIC to cut its own DRAM traffic, but the display reads a decompressed
/// buffer; crediting that here double-counts a saving on a bus this allocator
/// does not manage.
///
/// ```
/// use drmkit_fmt::{BandwidthClass, fourcc, scanout_cost_bytes, DEFAULT_COMPRESSED_RATIO};
///
/// let raw = scanout_cost_bytes(
///     1920, 1080, fourcc::XRGB8888, BandwidthClass::Linear, DEFAULT_COMPRESSED_RATIO,
/// );
/// assert_eq!(raw, 1920 * 1080 * 4);
///
/// // Tiling moves the same bytes; only compression cuts the fetch.
/// let tiled = scanout_cost_bytes(
///     1920, 1080, fourcc::XRGB8888, BandwidthClass::Tiling, DEFAULT_COMPRESSED_RATIO,
/// );
/// assert_eq!(tiled, raw);
/// ```
#[must_use]
pub fn scanout_cost_bytes(
    width: u32,
    height: u32,
    fourcc_code: u32,
    class: BandwidthClass,
    compressed_ratio: f32,
) -> u64 {
    let raw = frame_bytes(fourcc_code, width, height);
    if class == BandwidthClass::Compression {
        // The C++ widens to double before scaling; match that so the two
        // implementations agree bit for bit on the same inputs.
        #[expect(
            clippy::cast_precision_loss,
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "parity with the C++ cost model, which scales through double; \
                      this is a scoring heuristic, not an exact byte count"
        )]
        {
            (raw as f64 * f64::from(compressed_ratio)) as u64
        }
    } else {
        raw
    }
}
