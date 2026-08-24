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
