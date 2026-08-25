// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! Transfer functions.
//!
//! An *EOTF* turns an encoded signal into linear light; an *OETF* goes the
//! other way. Every one of these is defined by a standard, so the constants are
//! written as the fractions the specification gives rather than as decimals —
//! `2610 / 16384`, not `0.15930175`. A transposed digit in a hand-rounded
//! constant is invisible in review and shows up as a colour cast.
//!
//! All of it is `f64`. These run once per LUT entry at construction, not per
//! pixel, so the precision is free.

/// SMPTE ST 2084 (PQ) constants.
const PQ_M1: f64 = 2610.0 / 16384.0;
const PQ_M2: f64 = 2523.0 / 4096.0 * 128.0;
const PQ_C1: f64 = 3424.0 / 4096.0;
const PQ_C2: f64 = 2413.0 / 4096.0 * 32.0;
const PQ_C3: f64 = 2392.0 / 4096.0 * 32.0;

/// ARIB STD-B67 (HLG) constants.
const HLG_A: f64 = 0.178_832_77;
const HLG_B: f64 = 0.284_668_92;
const HLG_C: f64 = 0.559_910_73;

/// PQ encoded signal to linear light.
#[must_use]
pub fn pq_eotf(encoded: f64) -> f64 {
    let encoded = encoded.clamp(0.0, 1.0);
    let n = encoded.powf(1.0 / PQ_M2);
    let num = (n - PQ_C1).max(0.0);
    let den = PQ_C2 - (PQ_C3 * n);
    if den <= 0.0 {
        return 0.0;
    }
    (num / den).powf(1.0 / PQ_M1)
}

/// Linear light to PQ encoded signal.
#[must_use]
pub fn pq_oetf(linear: f64) -> f64 {
    let linear = linear.clamp(0.0, 1.0);
    let n = linear.powf(PQ_M1);
    ((PQ_C1 + (PQ_C2 * n)) / (1.0 + (PQ_C3 * n))).powf(PQ_M2)
}

/// HLG encoded signal to scene-linear light.
#[must_use]
pub fn hlg_oetf_inverse(encoded: f64) -> f64 {
    let encoded = encoded.clamp(0.0, 1.0);
    if encoded <= 0.5 {
        (encoded * encoded) / 3.0
    } else {
        (((encoded - HLG_C) / HLG_A).exp() + HLG_B) / 12.0
    }
}

/// sRGB encoded signal to linear light.
#[must_use]
pub fn srgb_eotf(encoded: f64) -> f64 {
    let encoded = encoded.clamp(0.0, 1.0);
    if encoded <= 0.040_45 {
        encoded / 12.92
    } else {
        ((encoded + 0.055) / 1.055).powf(2.4)
    }
}

/// Linear light to sRGB encoded signal.
#[must_use]
pub fn srgb_oetf(linear: f64) -> f64 {
    let linear = linear.clamp(0.0, 1.0);
    if linear <= 0.003_130_8 {
        12.92 * linear
    } else {
        (1.055 * linear.powf(1.0 / 2.4)) - 0.055
    }
}

/// Linear light to BT.1886 encoded signal.
///
/// A pure 2.4 gamma. Used as the SDR output curve rather than sRGB's piecewise
/// one: BT.1886 is what a display actually does, and the near-black linear
/// segment sRGB adds is an encoding convenience, not a display behaviour.
#[must_use]
pub fn bt1886_oetf(linear: f64) -> f64 {
    linear.max(0.0).powf(1.0 / 2.4)
}

// ── Kernel blob shapes ──────────────────────────────────────────────
//
// Declared here rather than pulled from `drm-ffi`, which this crate does not
// otherwise need. Both are fixed kernel ABI: `drm_color_lut` and
// `drm_color_ctm` in `include/uapi/drm/drm_mode.h`.

/// One LUT entry, as the kernel reads it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[repr(C)]
pub struct ColorLut {
    /// Red output for this input step.
    pub red: u16,
    /// Green output.
    pub green: u16,
    /// Blue output.
    pub blue: u16,
    /// Kernel padding. Must be zero.
    pub reserved: u16,
}

/// A 3x3 colour transform, as the kernel reads it.
///
/// Row-major, each entry S31.32 sign-magnitude -- see [`encode_s31_32`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(C)]
pub struct ColorCtm {
    /// The nine coefficients, row-major.
    pub matrix: [u64; 9],
}

/// `1.0` in S31.32.
const S31_32_ONE: u64 = 1 << 32;
/// The same, as the scale factor the arithmetic uses. Exact in `f64`.
const S31_32_SCALE: f64 = 4_294_967_296.0;
/// The sign bit. Sign-magnitude, not two's complement.
const S31_32_SIGN: u64 = 1 << 63;
/// The largest magnitude the format holds.
const S31_32_MAX: u64 = (1 << 63) - 1;
/// The saturation bound in floating point. `S31_32_MAX` has no exact `f64`,
/// so this is the next value up, which is what a comparison would round to
/// anyway.
const S31_32_MAX_AS_F64: f64 = 9_223_372_036_854_775_808.0;

/// Encode a coefficient the way `CTM` wants it.
///
/// S31.32 **sign-magnitude**: the top bit is the sign and the remaining
/// sixty-three are the absolute value scaled by 2^32. Not two's complement,
/// which is the mistake this exists to make hard -- a negative coefficient
/// encoded as two's complement reads to the kernel as an enormous positive
/// one, and the picture comes out with a channel blown to white.
#[must_use]
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
pub fn encode_s31_32(value: f64) -> u64 {
    if value.is_nan() {
        return 0;
    }
    let scaled = (value.abs() * S31_32_SCALE).round();
    // Saturating rather than wrapping. No colour matrix has a coefficient
    // anywhere near 2^31, so this is unreachable in practice; wrapping would
    // turn an overflow into a small number and hide it. The comparison is
    // against 2^63 rather than 2^63 - 1 because the latter is not
    // representable in an `f64` -- it rounds up to the former, so the two
    // bounds are the same number here.
    let magnitude = if scaled >= S31_32_MAX_AS_F64 {
        S31_32_MAX
    } else {
        // Non-negative and below 2^63 by the branch above.
        scaled as u64
    };
    magnitude | if value < 0.0 { S31_32_SIGN } else { 0 }
}

/// Read back what [`encode_s31_32`] wrote.
#[must_use]
pub fn decode_s31_32(encoded: u64) -> f64 {
    #[allow(clippy::cast_precision_loss)] // The magnitude is what it is.
    let magnitude = (encoded & !S31_32_SIGN) as f64 / S31_32_SCALE;
    if encoded & S31_32_SIGN == 0 {
        magnitude
    } else {
        -magnitude
    }
}

/// Quantize a normalized curve output into a LUT entry.
///
/// Clamped rather than wrapped: a curve that overshoots slightly at the top
/// end -- which floating point does -- would otherwise wrap to black at the
/// brightest step.
#[must_use]
pub fn quantize_lut_value(normalized: f64) -> u16 {
    if normalized.is_nan() || normalized <= 0.0 {
        return 0;
    }
    if normalized >= 1.0 {
        return u16::MAX;
    }
    // In `(0, 1)` by the guards above, so the product is in range and
    // non-negative.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let quantized = (normalized * f64::from(u16::MAX)).round() as u16;
    quantized
}

/// Fill `out` with `curve(i / (n - 1))`, the same value on all three channels.
///
/// The length of `out` is the quantization granularity, and has to be exactly
/// what the driver asks for -- see
/// [`CrtcCapabilities`](crate::CrtcCapabilities).
pub fn fill_lut(out: &mut [ColorLut], curve: impl Fn(f64) -> f64) {
    let last = out.len().saturating_sub(1);
    for (index, entry) in out.iter_mut().enumerate() {
        // A one-entry LUT has no range to walk, and dividing by zero would
        // make its single step NaN.
        #[allow(clippy::cast_precision_loss)] // A LUT is thousands of entries.
        let normalized = if last == 0 {
            0.0
        } else {
            index as f64 / last as f64
        };
        let value = quantize_lut_value(curve(normalized));
        *entry = ColorLut {
            red: value,
            green: value,
            blue: value,
            reserved: 0,
        };
    }
}

/// A LUT that changes nothing.
///
/// For a stage the kernel insists on a blob for while the pipeline is only
/// really using another one.
pub fn build_identity_lut(out: &mut [ColorLut]) {
    fill_lut(out, |value| value);
}

/// PQ encoded to linear, for `DEGAMMA_LUT` on HDR10 content.
pub fn build_pq_eotf_lut(out: &mut [ColorLut]) {
    fill_lut(out, pq_eotf);
}

/// Linear to PQ encoded, for `GAMMA_LUT` when the output is HDR10.
pub fn build_pq_oetf_lut(out: &mut [ColorLut]) {
    fill_lut(out, pq_oetf);
}

/// HLG encoded to scene-linear, for `DEGAMMA_LUT` on HLG content.
pub fn build_hlg_oetf_inverse_lut(out: &mut [ColorLut]) {
    fill_lut(out, hlg_oetf_inverse);
}

/// A matrix that changes nothing.
#[must_use]
pub fn build_identity_ctm() -> ColorCtm {
    let mut matrix = [0_u64; 9];
    matrix[0] = S31_32_ONE;
    matrix[4] = S31_32_ONE;
    matrix[8] = S31_32_ONE;
    ColorCtm { matrix }
}

/// BT.2020 to BT.709 in linear light, from ITU-R BT.2087.
///
/// Only valid after a degamma stage that produces linear values. Applied to
/// gamma-encoded ones it is simply wrong, and wrong in a way that looks like
/// a plausible picture with the colours shifted.
#[must_use]
pub fn build_bt2020_to_bt709_ctm() -> ColorCtm {
    encode_matrix([
        1.6605, -0.5876, -0.0728, //
        -0.1246, 1.1329, -0.0083, //
        -0.0182, -0.1006, 1.1187,
    ])
}

/// BT.709 to BT.2020 in linear light, the inverse of the above.
#[must_use]
pub fn build_bt709_to_bt2020_ctm() -> ColorCtm {
    encode_matrix([
        0.6274, 0.3293, 0.0433, //
        0.0691, 0.9195, 0.0114, //
        0.0164, 0.0880, 0.8956,
    ])
}

fn encode_matrix(coefficients: [f64; 9]) -> ColorCtm {
    ColorCtm {
        matrix: coefficients.map(encode_s31_32),
    }
}
