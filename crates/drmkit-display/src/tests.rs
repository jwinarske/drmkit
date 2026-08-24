// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! The tone mapper, diffed against drm-cxx's own output.
//!
//! Every expectation in `CASES` was produced by running the reference's
//! `ToneMapper` over the same pixel and recording what came back — not derived
//! by hand. Colour maths has too many plausible readings for a paper
//! derivation to be worth much: which matrix, absolute or relative scaling,
//! where in the chain the curve applies, how the tables interpolate. A wrong
//! reading tends to produce a wrong expectation and an implementation that
//! agrees with it.
//!
//! Regenerate with `parity/tonemap-oracle.cc` against a built drm-cxx.

use super::{Direction, ToneMapCurve, ToneMapper};

const fn px(r: u16, g: u16, b: u16, a: u16) -> u64 {
    (r as u64) | ((g as u64) << 16) | ((b as u64) << 32) | ((a as u64) << 48)
}

fn mapper_for(name: &str) -> ToneMapper {
    match name {
        "up" => ToneMapper::bt709_to_bt2020_pq(100.0),
        "up203" => ToneMapper::bt709_to_bt2020_pq(203.0),
        "down" => ToneMapper::bt2020_pq_to_bt709(100.0, ToneMapCurve::Reinhard),
        "down_hable" => ToneMapper::bt2020_pq_to_bt709(100.0, ToneMapCurve::Hable),
        "down_none" => ToneMapper::bt2020_pq_to_bt709(100.0, ToneMapCurve::None),
        "hlg" => ToneMapper::hlg_to_bt709(100.0),
        other => panic!("unknown mapper {other}"),
    }
}

/// `(case, mapper, input, what drm-cxx returns)`.
const CASES: &[(&str, &str, u64, u64)] = &[
    ("up_black", "up", px(0, 0, 0, 0xFFFF), 0xFFFF_0000_0000_0000),
    (
        "up_white",
        "up",
        px(0xFFFF, 0xFFFF, 0xFFFF, 0xFFFF),
        0xFFFF_820C_820C_820C,
    ),
    (
        "up_mid_grey",
        "up",
        px(0x8000, 0x8000, 0x8000, 0xFFFF),
        0xFFFF_5CA0_5CA0_5CA0,
    ),
    (
        "up_red",
        "up",
        px(0xFFFF, 0, 0, 0xFFFF),
        0xFFFF_0CCE_35F2_7639,
    ),
    (
        "up_alpha_kept",
        "up",
        px(0x4000, 0x4000, 0x4000, 0x1234),
        0x1234_27B9_27B9_27B9,
    ),
    (
        "up203_mid_grey",
        "up203",
        px(0x8000, 0x8000, 0x8000, 0xFFFF),
        0xFFFF_6D2E_6D2E_6D2E,
    ),
    (
        "down_black",
        "down",
        px(0, 0, 0, 0xFFFF),
        0xFFFF_0000_0000_0000,
    ),
    (
        "down_white",
        "down",
        px(0xFFFF, 0xFFFF, 0xFFFF, 0xFFFF),
        0xFFFF_FEF0_FEF0_FEF0,
    ),
    (
        "down_mid",
        "down",
        px(0x8000, 0x8000, 0x8000, 0xFFFF),
        0xFFFF_BC84_BC85_BC86,
    ),
    (
        "down_green",
        "down",
        px(0, 0xC000, 0, 0xFFFF),
        0xFFFF_0000_F6FD_0000,
    ),
    (
        "down_hable_mid",
        "down_hable",
        px(0x8000, 0x8000, 0x8000, 0xFFFF),
        0xFFFF_97EC_97ED_97EF,
    ),
    (
        "down_none_mid",
        "down_none",
        px(0x8000, 0x8000, 0x8000, 0xFFFF),
        0xFFFF_F785_F788_F78A,
    ),
    (
        "hlg_black",
        "hlg",
        px(0, 0, 0, 0xFFFF),
        0xFFFF_0000_0000_0000,
    ),
    (
        "hlg_half",
        "hlg",
        px(0x8000, 0x8000, 0x8000, 0xFFFF),
        0xFFFF_AABA_AABB_AABC,
    ),
    (
        "hlg_white",
        "hlg",
        px(0xFFFF, 0xFFFF, 0xFFFF, 0xFFFF),
        0xFFFF_F799_F799_F799,
    ),
];

/// Fifteen pixels, bit for bit against the reference.
#[test]
fn the_tone_mapper_matches_drm_cxx() {
    let mut wrong = Vec::new();
    for (name, which, input, want) in CASES {
        let got = mapper_for(which).map(*input);
        if got != *want {
            wrong.push(format!("{name}: got {got:016X}, drm-cxx gives {want:016X}"));
        }
    }
    assert!(
        wrong.is_empty(),
        "the tone mapper diverges from the reference:\n{}",
        wrong.join("\n")
    );
}

/// Alpha is carried through untouched.
///
/// Tone mapping is a colour operation and has no opinion about coverage.
/// Running alpha through the transfer functions would make every translucent
/// pixel change opacity as a side effect of a colour conversion.
#[test]
fn alpha_passes_through_unchanged() {
    let mapper = ToneMapper::bt709_to_bt2020_pq(100.0);
    for alpha in [0u16, 0x1234, 0x8000, 0xFFFF] {
        let out = mapper.map(px(0x4000, 0x8000, 0xC000, alpha));
        assert_eq!(
            (out >> 48) as u16,
            alpha,
            "alpha {alpha:#06x} was altered by a colour transform"
        );
    }
}

/// Black stays black in every direction.
///
/// Zero in, zero out is the one point every one of these curves passes
/// through, so a mapper that shifts it has something wrong before any
/// subjective choice enters.
#[test]
fn black_is_preserved_in_every_direction() {
    let mappers = [
        ToneMapper::bt709_to_bt2020_pq(100.0),
        ToneMapper::bt2020_pq_to_bt709(100.0, ToneMapCurve::Reinhard),
        ToneMapper::bt2020_pq_to_bt709(100.0, ToneMapCurve::Hable),
        ToneMapper::hlg_to_bt709(100.0),
    ];
    for mapper in &mappers {
        let out = mapper.map(px(0, 0, 0, 0xFFFF));
        assert_eq!(
            out,
            px(0, 0, 0, 0xFFFF),
            "{:?} moved black",
            mapper.direction()
        );
    }
}

/// The transform is monotonic: brighter in, no darker out.
///
/// Every curve here is monotonic by construction, so a ramp that dips means
/// the tables are being sampled wrongly — an off-by-one bucket, or a fraction
/// taken from the wrong bits. That shows on screen as banding, which is easy
/// to see and hard to attribute.
#[test]
fn a_ramp_stays_monotonic() {
    for mapper in [
        ToneMapper::bt709_to_bt2020_pq(100.0),
        ToneMapper::bt2020_pq_to_bt709(100.0, ToneMapCurve::Reinhard),
        ToneMapper::hlg_to_bt709(100.0),
    ] {
        let mut previous = 0u16;
        for step in 0..=255u32 {
            let v = u16::try_from(step * 257).expect("a 0..=255 ramp expands inside u16");
            let out = (mapper.map(px(v, v, v, 0xFFFF)) & 0xFFFF) as u16;
            assert!(
                out >= previous,
                "{:?} dipped at input {v:#06x}: {out:#06x} after {previous:#06x}",
                mapper.direction()
            );
            previous = out;
        }
    }
}

/// A brighter SDR white reference lands the same input higher.
///
/// PQ is absolute — its 1.0 is 10,000 nits — so where SDR white sits is a real
/// decision, not a normalisation. Ignoring it would light an SDR layer at a
/// hundred times its intent.
#[test]
fn a_higher_sdr_white_encodes_brighter() {
    let dim = ToneMapper::bt709_to_bt2020_pq(100.0);
    let bright = ToneMapper::bt709_to_bt2020_pq(203.0);
    let input = px(0x8000, 0x8000, 0x8000, 0xFFFF);
    assert!(
        (bright.map(input) & 0xFFFF) > (dim.map(input) & 0xFFFF),
        "203-nit SDR white must encode above 100-nit"
    );
}

/// `apply` matches `map`, pixel for pixel.
#[test]
fn apply_matches_the_single_pixel_path() {
    let mapper = ToneMapper::bt2020_pq_to_bt709(100.0, ToneMapCurve::Reinhard);
    let src: Vec<u64> = (0..64u16)
        .map(|i| px(i * 1024, 0x4000, 0x8000, 0xFFFF))
        .collect();
    let mut dst = vec![0u64; src.len()];
    mapper.apply(&src, &mut dst);
    for (i, pixel) in src.iter().enumerate() {
        assert_eq!(dst[i], mapper.map(*pixel), "pixel {i} differs");
    }
}

/// The reported configuration is what was asked for.
#[test]
fn a_mapper_reports_its_configuration() {
    let m = ToneMapper::bt2020_pq_to_bt709(203.0, ToneMapCurve::Hable);
    assert_eq!(m.direction(), Direction::Bt2020PqToBt709);
    assert_eq!(m.curve(), ToneMapCurve::Hable);
    assert!((m.target_max_nits() - 203.0).abs() < f32::EPSILON);
}
