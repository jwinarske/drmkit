// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! An in-memory image, and the blend that builds one.

/// A premultiplied ARGB8888 image, one `u32` per pixel, rows contiguous.
///
/// Premultiplied because that is what KMS scans out: a plane's `ARGB8888`
/// pixels already have their colour scaled by their coverage, and compositing
/// them any other way would double-apply it. The layout is `0xAARRGGBB` in a
/// native-endian `u32`, which is what a little-endian host sees in a
/// `DRM_FORMAT_ARGB8888` buffer.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Image {
    width: u32,
    height: u32,
    pixels: Vec<u32>,
}

impl Image {
    /// A transparent image of the given size.
    ///
    /// A zero in either dimension gives the same empty image as
    /// [`Image::default`] -- there is no such thing as a 0x100 picture, and
    /// carrying one would mean every consumer checking both dimensions.
    #[must_use]
    pub fn new(width: u32, height: u32) -> Self {
        if width == 0 || height == 0 {
            return Self::default();
        }
        Self {
            width,
            height,
            pixels: vec![0; width as usize * height as usize],
        }
    }

    /// Width in pixels.
    #[must_use]
    pub const fn width(&self) -> u32 {
        self.width
    }

    /// Height in pixels.
    #[must_use]
    pub const fn height(&self) -> u32 {
        self.height
    }

    /// Bytes per row. Always `width * 4`; the storage is tightly packed.
    #[must_use]
    pub const fn stride_bytes(&self) -> u32 {
        self.width * 4
    }

    /// Whether either dimension is zero.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.width == 0 || self.height == 0
    }

    /// The pixels, row-major.
    #[must_use]
    pub fn pixels(&self) -> &[u32] {
        &self.pixels
    }

    /// The pixels, row-major, mutable.
    pub fn pixels_mut(&mut self) -> &mut [u32] {
        &mut self.pixels
    }
}

/// Split a premultiplied `0xAARRGGBB` pixel into its channels.
const fn channels(px: u32) -> (u32, u32, u32, u32) {
    (px >> 24, (px >> 16) & 0xff, (px >> 8) & 0xff, px & 0xff)
}

/// Round `x / 255` to nearest, without dividing.
///
/// The `+ 128` then fold-and-shift is the standard trick: exact for every
/// product of two bytes, which is the only input it ever gets. Truncating with
/// a plain `>> 8` instead loses a little coverage on every composite, which
/// shows up as a seam where two translucent layers meet.
const fn div255(x: u32) -> u32 {
    let t = x + 128;
    (t + (t >> 8)) >> 8
}

/// Premultiplied `SRC_OVER`: `dst = src + dst * (1 - src_alpha)`.
#[must_use]
pub const fn src_over(dst: u32, src: u32) -> u32 {
    let (sa, sr, sg, sb) = channels(src);
    if sa == 255 {
        return src;
    }
    if sa == 0 {
        return dst;
    }
    let (da, dr, dg, db) = channels(dst);
    let inv = 255 - sa;
    ((sa + div255(da * inv)) << 24)
        | ((sr + div255(dr * inv)) << 16)
        | ((sg + div255(dg * inv)) << 8)
        | (sb + div255(db * inv))
}

/// Recover straight-alpha channels from a premultiplied pixel.
///
/// PNG stores straight alpha, so this runs on the way out. Fully opaque and
/// fully transparent are exact; everything between divides by the alpha it was
/// multiplied by, rounding to nearest, and cannot recover precision that the
/// premultiply threw away. A pixel at alpha 1 has three bits of colour left,
/// and no amount of care here puts them back.
///
/// Returns `[r, g, b, a]`, the order PNG wants.
#[must_use]
pub fn unpremultiply(px: u32) -> [u8; 4] {
    let (a, r, g, b) = channels(px);
    if a == 0 {
        // Nothing was multiplied in, so there is nothing to recover, and the
        // colour channels of a fully transparent premultiplied pixel are zero
        // by construction anyway.
        return [0, 0, 0, 0];
    }
    // Clamped because a buffer can hold a pixel whose colour exceeds its
    // alpha -- nothing stops a producer writing one, and the division would
    // then land above 255.
    let straight = |c: u32| u8::try_from((((c * 255) + (a / 2)) / a).min(255)).unwrap_or(u8::MAX);
    let alpha = u8::try_from(a).unwrap_or(u8::MAX);
    [straight(r), straight(g), straight(b), alpha]
}
