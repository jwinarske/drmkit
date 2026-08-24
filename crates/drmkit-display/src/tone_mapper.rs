// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! CPU tone mapping, for when the hardware pipeline is not exposed.
//!
//! Mixing HDR and SDR layers is properly the display pipeline's job, but the
//! kernel does not expose one on every driver. Without a fallback an
//! application either refuses HDR content or shows it wrong, so this covers the
//! three cases that matter: SDR up into HDR, and HDR or HLG back down to SDR.
//!
//! # Pixel layout
//!
//! A `u64` holding four `u16` channels, little-endian: R in bits 0..16, G in
//! 16..32, B in 32..48, A in 48..64. Alpha passes through untouched — tone
//! mapping is a colour operation and has no opinion about coverage. Callers on
//! 8-bit surfaces expand by 257 first and re-quantise after.
//!
//! # Why there are lookup tables
//!
//! Every transfer function here is a `powf` or an `exp`. Running one per
//! channel per pixel is three transcendentals per pixel each way, which at
//! 1080p60 is not a fallback, it is a stall. The curves are sampled once at
//! construction into 1024 buckets plus a sentinel, and per-pixel cost drops to
//! a lookup and a lerp.

use crate::curves::{bt1886_oetf, hlg_oetf_inverse, pq_eotf, pq_oetf, srgb_eotf};

/// Entries in each lookup table: 1024 buckets plus one sentinel.
///
/// The sentinel is what lets the lerp read `lut[i + 1]` without a bounds check
/// on the last bucket.
pub const LUT_SIZE: usize = 1025;

const LUT_BUCKETS: usize = 1024;
/// The same count as an `f64`, written out rather than cast: 1024 is exact in
/// both, and a literal keeps the conversion out of the hot path. Kept beside
/// `LUT_BUCKETS` so the pair is edited together.
const LUT_BUCKETS_F: f64 = 1024.0;
/// A `u16` sample's top 10 bits select the bucket, the bottom 6 interpolate.
const INPUT_INDEX_SHIFT: u32 = 6;
const INPUT_FRAC_DIV: f64 = 64.0;

/// The curve applied in linear light when compressing HDR into SDR.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ToneMapCurve {
    /// Clamp at 1.0.
    ///
    /// For HLG, whose OOTF has already done the work, and for pipelines that
    /// want the matrix and transfer functions without a subjective curve.
    None,
    /// `x / (1 + x)`. Cheap, keeps shadows, rolls highlights off smoothly.
    #[default]
    Reinhard,
    /// The Uncharted 2 filmic curve. Holds contrast better, at some mid-tone
    /// shift.
    Hable,
}

/// Which transform a mapper performs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// SDR up into an HDR composition.
    Bt709ToBt2020Pq,
    /// HDR down to an SDR display.
    Bt2020PqToBt709,
    /// HLG down to an SDR display.
    HlgToBt709,
}

/// BT.709 primaries to BT.2020.
const BT709_TO_BT2020: [f64; 9] = [
    0.6274, 0.3293, 0.0433, //
    0.0691, 0.9195, 0.0114, //
    0.0164, 0.0880, 0.8956,
];

/// BT.2020 primaries to BT.709.
///
/// Not the inverse of the above to full precision — both are the rounded
/// matrices the reference carries, and they are reproduced digit for digit so
/// the two implementations agree pixel for pixel.
const BT2020_TO_BT709: [f64; 9] = [
    1.6605, -0.5876, -0.0728, //
    -0.1246, 1.1329, -0.0083, //
    -0.0182, -0.1006, 1.1187,
];

/// BT.2020 luminance coefficients, for the HLG system-gamma factor.
const BT2020_Y_R: f64 = 0.2627;
const BT2020_Y_G: f64 = 0.6780;
const BT2020_Y_B: f64 = 0.0593;

fn reinhard(x: f64) -> f64 {
    x / (1.0 + x)
}

fn hable_curve(x: f64) -> f64 {
    const A: f64 = 0.15;
    const B: f64 = 0.50;
    const C: f64 = 0.10;
    const D: f64 = 0.20;
    const E: f64 = 0.02;
    const F: f64 = 0.30;
    (((x * ((A * x) + (C * B))) + (D * E)) / ((x * ((A * x) + B)) + (D * F))) - (E / F)
}

fn hable(x: f64) -> f64 {
    // The curve is normalised against its own value at the reference white
    // point, so that white maps to 1.0 rather than to wherever the raw curve
    // happens to land.
    const WHITE: f64 = 11.2;
    (hable_curve(x) / hable_curve(WHITE)).clamp(0.0, 1.0)
}

fn apply_curve(x: f64, curve: ToneMapCurve) -> f64 {
    match curve {
        ToneMapCurve::None => x.clamp(0.0, 1.0),
        ToneMapCurve::Reinhard => reinhard(x.max(0.0)),
        ToneMapCurve::Hable => hable(x.max(0.0)),
    }
}

fn matrix_mul(m: &[f64; 9], v: [f64; 3]) -> [f64; 3] {
    [
        (m[0] * v[0]) + (m[1] * v[1]) + (m[2] * v[2]),
        (m[3] * v[0]) + (m[4] * v[1]) + (m[5] * v[2]),
        (m[6] * v[0]) + (m[7] * v[1]) + (m[8] * v[2]),
    ]
}

/// Sample the encoded-to-linear table for a `u16` channel.
fn sample_input(lut: &[f32; LUT_SIZE], encoded: u16) -> f64 {
    let idx = (encoded >> INPUT_INDEX_SHIFT) as usize;
    let frac = f64::from(encoded & 0x3F) / INPUT_FRAC_DIV;
    let lo = f64::from(lut[idx]);
    let hi = f64::from(lut[idx + 1]);
    lo + (frac * (hi - lo))
}

/// Sample the linear-to-encoded table.
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "the clamp on the line above bounds `scaled` to 0.0..=1024.0, so \
              the conversion is exact and non-negative by construction"
)]
fn sample_output(lut: &[f32; LUT_SIZE], linear: f64) -> f64 {
    let linear = linear.clamp(0.0, 1.0);
    let scaled = linear * LUT_BUCKETS_F;
    let idx = scaled as usize;
    // `linear == 1.0` lands exactly on the last bucket, where there is nothing
    // to interpolate towards; the sentinel is the answer.
    if idx >= LUT_BUCKETS {
        return f64::from(lut[LUT_BUCKETS]);
    }
    let frac = scaled - f64::from(u32::try_from(idx).unwrap_or(u32::MAX));
    let lo = f64::from(lut[idx]);
    let hi = f64::from(lut[idx + 1]);
    lo + (frac * (hi - lo))
}

/// A configured tone-mapping transform.
///
/// Immutable: the tables are built once from the parameters, so changing a
/// parameter means building another mapper.
pub struct ToneMapper {
    direction: Direction,
    curve: ToneMapCurve,
    sdr_white_nits: f32,
    target_max_nits: f32,
    input_lut: Box<[f32; LUT_SIZE]>,
    output_lut: Box<[f32; LUT_SIZE]>,
}

impl std::fmt::Debug for ToneMapper {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToneMapper")
            .field("direction", &self.direction)
            .field("curve", &self.curve)
            .field("sdr_white_nits", &self.sdr_white_nits)
            .field("target_max_nits", &self.target_max_nits)
            .finish_non_exhaustive()
    }
}

impl ToneMapper {
    /// SDR into an HDR composition.
    ///
    /// `sdr_white_nits` is where SDR's `1.0` lands on the PQ curve. 100 is the
    /// conventional default and matches how most HDR displays render an SDR
    /// signal; 203 is the common "diffuse white" reference and makes the SDR
    /// part brighter within the composition.
    #[must_use]
    pub fn bt709_to_bt2020_pq(sdr_white_nits: f32) -> Self {
        Self::new(
            Direction::Bt709ToBt2020Pq,
            sdr_white_nits,
            100.0,
            ToneMapCurve::None,
        )
    }

    /// HDR down to an SDR display.
    ///
    /// `target_max_nits` is what SDR's `1.0` means in absolute terms; anything
    /// above it goes through `curve` so highlights compress instead of
    /// clipping.
    #[must_use]
    pub fn bt2020_pq_to_bt709(target_max_nits: f32, curve: ToneMapCurve) -> Self {
        Self::new(Direction::Bt2020PqToBt709, 100.0, target_max_nits, curve)
    }

    /// HLG down to an SDR display.
    ///
    /// Always Reinhard: HLG's OOTF has already shaped the highlights, so a
    /// second subjective curve on top would double-compress them.
    #[must_use]
    pub fn hlg_to_bt709(target_max_nits: f32) -> Self {
        Self::new(
            Direction::HlgToBt709,
            100.0,
            target_max_nits,
            ToneMapCurve::Reinhard,
        )
    }

    #[expect(
        clippy::cast_possible_truncation,
        reason = "the tables are f32 on purpose: two 4KB tables stay in cache \
                  where two 8KB ones do not, and the values are transfer-curve \
                  samples in 0.0..=1.0 where f32 has far more precision than \
                  the 16-bit channels being sampled"
    )]
    fn new(
        direction: Direction,
        sdr_white_nits: f32,
        target_max_nits: f32,
        curve: ToneMapCurve,
    ) -> Self {
        let mut input_lut = Box::new([0.0f32; LUT_SIZE]);
        let mut output_lut = Box::new([0.0f32; LUT_SIZE]);
        for i in 0..LUT_SIZE {
            let t = f64::from(u32::try_from(i).unwrap_or(u32::MAX)) / LUT_BUCKETS_F;
            input_lut[i] = match direction {
                Direction::Bt709ToBt2020Pq => srgb_eotf(t),
                Direction::Bt2020PqToBt709 => pq_eotf(t),
                Direction::HlgToBt709 => hlg_oetf_inverse(t),
            } as f32;
            output_lut[i] = match direction {
                Direction::Bt709ToBt2020Pq => pq_oetf(t),
                Direction::Bt2020PqToBt709 | Direction::HlgToBt709 => bt1886_oetf(t),
            } as f32;
        }
        Self {
            direction,
            curve,
            sdr_white_nits,
            target_max_nits,
            input_lut,
            output_lut,
        }
    }

    /// The direction this mapper transforms.
    #[must_use]
    pub const fn direction(&self) -> Direction {
        self.direction
    }

    /// The curve it applies.
    #[must_use]
    pub const fn curve(&self) -> ToneMapCurve {
        self.curve
    }

    /// Where SDR white lands, in nits.
    #[must_use]
    pub const fn sdr_white_nits(&self) -> f32 {
        self.sdr_white_nits
    }

    /// What SDR `1.0` represents, in nits.
    #[must_use]
    pub const fn target_max_nits(&self) -> f32 {
        self.target_max_nits
    }

    /// Transform one pixel.
    #[must_use]
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "the quantiser clamps to 0.0..=1.0 before scaling by 65535, \
                  so the rounded result is a whole number in 0..=65535 and \
                  both the truncation and the sign are ruled out by the clamp"
    )]
    pub fn map(&self, pixel: u64) -> u64 {
        let r_q = (pixel & 0xFFFF) as u16;
        let g_q = ((pixel >> 16) & 0xFFFF) as u16;
        let b_q = ((pixel >> 32) & 0xFFFF) as u16;
        let a_q = (pixel >> 48) & 0xFFFF;

        let mut r = sample_input(&self.input_lut, r_q);
        let mut g = sample_input(&self.input_lut, g_q);
        let mut b = sample_input(&self.input_lut, b_q);

        match self.direction {
            Direction::Bt709ToBt2020Pq => {
                let rgb = matrix_mul(&BT709_TO_BT2020, [r, g, b]);
                // PQ is absolute: its 1.0 is 10,000 nits, so SDR white has to
                // be placed at its real luminance rather than reused as full
                // scale, or an SDR layer would light up at 100x its intent.
                let scale = f64::from(self.sdr_white_nits) / 10_000.0;
                r = (rgb[0].max(0.0) * scale).clamp(0.0, 1.0);
                g = (rgb[1].max(0.0) * scale).clamp(0.0, 1.0);
                b = (rgb[2].max(0.0) * scale).clamp(0.0, 1.0);
            }
            Direction::Bt2020PqToBt709 => {
                let rgb = matrix_mul(&BT2020_TO_BT709, [r, g, b]);
                let scale = 10_000.0 / f64::from(self.target_max_nits);
                r = apply_curve(rgb[0].max(0.0) * scale, self.curve);
                g = apply_curve(rgb[1].max(0.0) * scale, self.curve);
                b = apply_curve(rgb[2].max(0.0) * scale, self.curve);
            }
            Direction::HlgToBt709 => {
                // The BT.2100 OOTF, which PQ does not have and HLG cannot skip.
                // HLG encodes *scene* light; a display shows *display* light,
                // and the system gamma between them is what stops HLG content
                // looking flat and washed out. It is a function of overall
                // luminance rather than of each channel, so it cannot be folded
                // into the input table with the rest.
                let y = (BT2020_Y_R * r) + (BT2020_Y_G * g) + (BT2020_Y_B * b);
                let ootf = y.max(0.0).powf(0.2);
                r = (r * ootf).max(0.0);
                g = (g * ootf).max(0.0);
                b = (b * ootf).max(0.0);

                let rgb = matrix_mul(&BT2020_TO_BT709, [r, g, b]);
                // HLG is relative, not absolute: its nominal peak is 12x
                // diffuse white rather than a fixed 10,000 nits, so the scale
                // is expressed against the target's own white point.
                let scale = 12.0 / (f64::from(self.target_max_nits) / 100.0);
                r = reinhard(rgb[0].max(0.0) * scale);
                g = reinhard(rgb[1].max(0.0) * scale);
                b = reinhard(rgb[2].max(0.0) * scale);
            }
        }

        let to_u16 = |v: f64| (v.clamp(0.0, 1.0) * 65535.0).round() as u64;
        to_u16(sample_output(&self.output_lut, r))
            | (to_u16(sample_output(&self.output_lut, g)) << 16)
            | (to_u16(sample_output(&self.output_lut, b)) << 32)
            | (a_q << 48)
    }

    /// Transform a run of pixels.
    ///
    /// The direction is decided once rather than per pixel, which is the whole
    /// reason this exists rather than the caller looping over
    /// [`map`](Self::map).
    pub fn apply(&self, src: &[u64], dst: &mut [u64]) {
        for (out, pixel) in dst.iter_mut().zip(src) {
            *out = self.map(*pixel);
        }
    }
}
