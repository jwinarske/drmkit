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

// --- EDID --------------------------------------------------------------------

#[cfg(feature = "edid")]
mod edid {
    use crate::edid::{EdidError, parse_edid};

    /// The fixture from drm-cxx's own unit suite, with one byte corrected.
    ///
    /// Byte 127 is the checksum, and upstream stores `0x21` where EDID's
    /// all-bytes-sum-to-zero rule needs `0x3F`. libdisplay-info rejects the
    /// blob as written, which is why every assertion in the reference's test
    /// sits inside an `if (result)` that has never run — see drm-cxx#241.
    const DELL: [u8; 128] = [
        0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x00, //
        0x10, 0xAC, 0x00, 0x40, 0x00, 0x00, 0x00, 0x00, //
        0x01, 0x11, 0x01, 0x03, 0x80, 0x34, 0x20, 0x78, //
        0xEA, 0xEE, 0x95, 0xA3, 0x54, 0x4C, 0x99, 0x26, //
        0x0F, 0x50, 0x54, 0xA5, 0x4B, 0x00, 0x71, 0x4F, //
        0x81, 0x80, 0xA9, 0xC0, 0xD1, 0xC0, 0x01, 0x01, //
        0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x02, 0x3A, //
        0x80, 0x18, 0x71, 0x38, 0x2D, 0x40, 0x58, 0x2C, //
        0x45, 0x00, 0x09, 0x25, 0x21, 0x00, 0x00, 0x1E, //
        0x00, 0x00, 0x00, 0xFF, 0x00, 0x46, 0x4F, 0x4F, //
        0x42, 0x41, 0x52, 0x0A, 0x20, 0x20, 0x20, 0x20, //
        0x20, 0x20, 0x00, 0x00, 0x00, 0xFC, 0x00, 0x44, //
        0x45, 0x4C, 0x4C, 0x0A, 0x20, 0x20, 0x20, 0x20, //
        0x20, 0x20, 0x20, 0x20, 0x00, 0x00, 0x00, 0xFD, //
        0x00, 0x38, 0x4C, 0x1E, 0x51, 0x11, 0x00, 0x0A, //
        0x20, 0x20, 0x20, 0x20, 0x20, 0x20, 0x00, 0x3F, //
    ];

    /// The fixture is a valid EDID, which the reference's copy is not.
    ///
    /// Checked here rather than assumed: a blob that fails checksum parses as
    /// nothing, and a test built on one asserts nothing while looking like it
    /// asserts a great deal.
    #[test]
    fn the_fixture_checksums() {
        let sum: u32 = DELL.iter().map(|b| u32::from(*b)).sum();
        assert_eq!(
            sum % 256,
            0,
            "EDID requires all 128 bytes to sum to zero mod 256"
        );
    }

    /// The refresh range the panel states, read off the `0xFD` descriptor.
    ///
    /// Bytes 5 and 6 of it are the vertical minimum and maximum, `0x38` and
    /// `0x4C` here. Worth its own case because the same descriptor also
    /// carries horizontal rates and a pixel clock, and reading one field
    /// where another was meant still yields a plausible number.
    #[test]
    fn the_fixture_states_a_refresh_range() {
        let info = parse_edid(&DELL).expect("parse");
        let range = info.vrefresh_range.expect("the 0xFD descriptor is there");

        assert_eq!((range.min_hz, range.max_hz), (56, 76));
        assert!(range.is_variable(), "a range, not a single rate");
    }

    /// A blob with no range descriptor says nothing rather than zero.
    ///
    /// Zero to zero would read as a panel that accepts no refresh rate at
    /// all, which is not a thing -- it is a panel that did not say.
    #[test]
    fn a_blob_without_the_descriptor_states_no_range() {
        let mut blob = DELL;
        // Turn the 0xFD descriptor into a 0xFE (unspecified text), keeping the
        // blob a valid EDID by fixing the checksum after.
        blob[0x6F] = 0xFE;
        let sum: u32 = blob[..127].iter().map(|b| u32::from(*b)).sum();
        blob[127] = u8::try_from((256 - (sum % 256)) % 256).expect("a checksum byte");

        let info = parse_edid(&blob).expect("still a valid EDID");
        assert_eq!(info.vrefresh_range, None);
    }

    /// Identity, physical size and primaries, against the reference's reading.
    ///
    /// Every value here came from running drm-cxx's own `parse_edid` over this
    /// blob (`parity/edid-oracle.cc`), not from reading the bytes by hand. EDID
    /// is a format where a field offset off by one still parses, still
    /// checksums, and yields a plausible wrong answer.
    #[test]
    fn a_real_blob_matches_the_reference() {
        let info = parse_edid(&DELL).expect("a checksummed EDID must parse");

        assert_eq!(info.make, "Dell Inc.");
        assert_eq!(info.model, "DELL");
        assert_eq!(info.name, "Dell Inc. DELL");
        assert_eq!(info.serial.as_deref(), Some("FOOBAR"));
        assert_eq!((info.width_mm, info.height_mm), (520, 320));

        let c = info
            .colorimetry
            .expect("this blob carries chromaticity bytes");
        assert!(c.has_primaries && c.has_default_white);
        let close = |a: f32, b: f32| (a - b).abs() < 1e-5;
        assert!(
            close(c.red.x, 0.639_648) && close(c.red.y, 0.330_078),
            "red {:?}",
            c.red
        );
        assert!(
            close(c.green.x, 0.299_805) && close(c.green.y, 0.599_609),
            "green {:?}",
            c.green
        );
        assert!(
            close(c.blue.x, 0.150_391) && close(c.blue.y, 0.059_570),
            "blue {:?}",
            c.blue
        );
        assert!(
            close(c.white.x, 0.313_477) && close(c.white.y, 0.329_102),
            "white {:?}",
            c.white
        );
    }

    /// A display that advertises no HDR reports none.
    ///
    /// **A deliberate divergence from drm-cxx as it stands.** It populates both
    /// optionals whenever the C accessor returns non-null, and
    /// libdisplay-info returns a zeroed struct rather than null when there is
    /// nothing to report — so upstream's `hdr` is `Some` for every display
    /// ever. Its own test asserts the opposite, and never ran to find out.
    ///
    /// This blob is pre-CTA EDID 1.3 with no extension block, so there is no
    /// HDR metadata in it to report. Tracked as drm-cxx#241; the port follows
    /// whichever way that goes.
    #[test]
    fn a_display_advertising_no_hdr_reports_none() {
        let info = parse_edid(&DELL).expect("parse");
        assert_eq!(
            info.hdr, None,
            "a pre-CTA EDID 1.3 blob carries no HDR static metadata block"
        );
        assert_eq!(info.wide_gamut, None, "nor any extended colorimetry block");
    }

    /// An empty blob is refused.
    #[test]
    fn an_empty_blob_is_refused() {
        assert!(matches!(parse_edid(&[]), Err(EdidError::Unparseable)));
    }

    /// Garbage is refused rather than half-read.
    #[test]
    fn garbage_is_refused() {
        assert!(matches!(
            parse_edid(&[0xDE, 0xAD, 0xBE, 0xEF]),
            Err(EdidError::Unparseable)
        ));
    }

    /// A blob whose checksum is wrong is refused.
    ///
    /// This is the exact failure that hid upstream's untested assertions, so
    /// it is worth its own case: the port should refuse it, loudly, rather
    /// than return a half-populated struct a caller would trust.
    #[test]
    fn a_bad_checksum_is_refused() {
        let mut broken = DELL;
        broken[127] = 0x21; // the reference's value
        assert!(
            matches!(parse_edid(&broken), Err(EdidError::Unparseable)),
            "a blob that does not checksum must not parse"
        );
    }
}
#[cfg(feature = "edid")]
#[test]
fn edid_fuzz_invariants_hold_on_random_blobs() {
    // A cheap xorshift so this needs no dependency; the point is volume and
    // shape variety, not cryptographic quality.
    let mut state = 0x2545_F491_4F6C_DD1Du64;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };

    let mut parsed = 0usize;
    for round in 0..20_000u32 {
        // Mix sizes, including exactly 128 and 256 so real EDID shapes appear.
        let len = match round % 4 {
            0 => 128,
            1 => 256,
            2 => usize::try_from(next() % 300).unwrap_or(0),
            _ => usize::try_from(next() % 32).unwrap_or(0),
        };
        let mut blob: Vec<u8> = (0..len)
            .map(|_| u8::try_from(next() & 0xFF).expect("masked to a byte"))
            .collect();
        // Half the time, give it a valid EDID header so it gets further in.
        if round % 2 == 0 && blob.len() >= 8 {
            blob[..8].copy_from_slice(&[0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x00]);
            if blob.len() >= 128 {
                let sum: u32 = blob[..127].iter().map(|b| u32::from(*b)).sum();
                blob[127] = u8::try_from((256 - (sum % 256)) % 256).expect("a byte");
            }
        }

        let Ok(info) = crate::edid::parse_edid(&blob) else {
            continue;
        };
        parsed += 1;

        if !info.make.is_empty() {
            assert!(
                info.name.contains(&info.make),
                "name {:?} omits make {:?}",
                info.name,
                info.make
            );
        }
        if !info.model.is_empty() {
            assert!(
                info.name.contains(&info.model),
                "name {:?} omits model {:?}",
                info.name,
                info.model
            );
        }
        assert!(
            info.width_mm >= 0 && info.height_mm >= 0,
            "negative size {} x {}",
            info.width_mm,
            info.height_mm
        );
        if let Some(hdr) = info.hdr {
            assert!(
                hdr.type1 || hdr.traditional_hdr || hdr.pq || hdr.hlg,
                "empty HDR {hdr:?}"
            );
        }
        if let Some(g) = info.wide_gamut {
            assert!(
                g.bt2020_cycc || g.bt2020_ycc || g.bt2020_rgb || g.st2113_rgb || g.ictcp,
                "empty gamut {g:?}"
            );
        }
    }
    eprintln!("note: {parsed} of 20000 random blobs parsed");
    assert!(
        parsed > 0,
        "no blob parsed at all; the smoke proved nothing"
    );
}
#[cfg(feature = "edid")]
#[test]
fn every_fuzz_seed_parses() {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../../fuzz/seeds/edid_blob");
    let mut checked = 0;
    for entry in std::fs::read_dir(dir).expect("the seed corpus must exist") {
        let path = entry.expect("entry").path();
        let blob = std::fs::read(&path).expect("read seed");
        assert!(
            crate::edid::parse_edid(&blob).is_ok(),
            "seed {} does not parse, so it teaches the fuzzer nothing",
            path.display()
        );
        checked += 1;
    }
    assert!(checked >= 2, "expected at least two seeds, found {checked}");
}
#[cfg(feature = "edid")]
#[test]
fn the_cta_seed_reaches_the_hdr_path() {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../fuzz/seeds/edid_blob/dell_with_hdr_cta.bin"
    );
    let blob = std::fs::read(path).expect("read the CTA seed");
    let info = crate::edid::parse_edid(&blob).expect("parse");
    assert!(
        info.hdr.is_some(),
        "this seed exists to carry the fuzzer into the HDR gate; without an \
         HDR block it is just another copy of the base seed"
    );
}

// --- DriverProfile -----------------------------------------------------------

mod profile {
    use crate::{PanelSelfRefresh, PrimeCaps, connector_type_self_refreshes, decode_prime_caps};

    /// The PRIME bitmask decodes to the right pair.
    ///
    /// Bit positions are the kind of thing that is silently wrong on hardware
    /// nobody has: a device that only imports would be told it can export, and
    /// the failure surfaces as a refused ioctl somewhere far away.
    #[test]
    fn prime_caps_decode_each_bit() {
        assert_eq!(decode_prime_caps(0), PrimeCaps::default());

        let import = decode_prime_caps(0x1);
        assert!(import.can_import && !import.can_export);

        let export = decode_prime_caps(0x2);
        assert!(!export.can_import && export.can_export);

        let both = decode_prime_caps(0x3);
        assert!(both.can_import && both.can_export);

        // Unknown bits are not import or export, and must not be read as either.
        let future = decode_prime_caps(0xFF00);
        assert!(!future.can_import && !future.can_export);
    }

    /// Only internal panels can hold an image without a flip.
    #[test]
    fn only_internal_panels_self_refresh() {
        const EDP: u32 = 14;
        const DSI: u32 = 16;
        const HDMI_A: u32 = 11;
        const DISPLAY_PORT: u32 = 10;
        const VGA: u32 = 1;

        assert!(connector_type_self_refreshes(EDP));
        assert!(connector_type_self_refreshes(DSI));
        for external in [HDMI_A, DISPLAY_PORT, VGA, 0] {
            assert!(
                !connector_type_self_refreshes(external),
                "connector type {external} is on a cable and has to be fed"
            );
        }
    }

    /// A driver that reports nothing gets the fallback.
    ///
    /// Zero counts as "said nothing": a maximum cursor of zero would mean no
    /// cursor at all, which no KMS driver means, and taking it literally gives
    /// a cursor path that silently refuses every plane. Tested directly
    /// because vkms reports a real 512x512 and never exercises the fallback.
    #[test]
    fn an_unreported_capability_falls_back() {
        use crate::profile::nonzero_or;
        assert_eq!(nonzero_or(Some(256), 64), 256, "a real value is kept");
        assert_eq!(nonzero_or(None, 64), 64, "an absent one falls back");
        assert_eq!(nonzero_or(Some(0), 64), 64, "and so does a zero");
    }

    /// Unknown is the default, and means "could not tell" rather than "no".
    ///
    /// The distinction matters because nothing may gate behaviour on this: a
    /// frame-economy decision that skipped a commit on a guess, and guessed
    /// wrong, freezes the display.
    #[test]
    fn self_refresh_defaults_to_unknown() {
        assert_eq!(PanelSelfRefresh::default(), PanelSelfRefresh::Unknown);
    }
}

#[cfg(test)]
mod profile_vkms {
    use std::sync::Mutex;

    use drmkit_core::Device;

    use crate::DriverProfile;

    static CARD_LOCK: Mutex<()> = Mutex::new(());

    fn open_card_or_skip() -> Option<Device> {
        let _guard = CARD_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let path =
            std::env::var("DRMKIT_TEST_CARD").unwrap_or_else(|_| "/dev/dri/card0".to_owned());
        match Device::open(&path) {
            Ok(device) => Some(device),
            Err(error) => {
                assert!(
                    std::env::var_os("DRMKIT_REQUIRE_MASTER").is_none(),
                    "{path}: {error}, but DRMKIT_REQUIRE_MASTER is set"
                );
                println!("note: skipped -- no DRM device at {path} ({error})");
                None
            }
        }
    }

    /// A real device probes to something coherent.
    ///
    /// The values themselves are the driver's to choose, so what is asserted is
    /// the shape: a name, a cursor size that could hold a cursor, and PRIME
    /// support, which every KMS driver that can share a buffer reports.
    #[test]
    #[ignore = "needs a DRM device"]
    fn a_real_device_probes_coherently_vkms() {
        let Some(device) = open_card_or_skip() else {
            return;
        };
        let profile = DriverProfile::probe(&device).expect("probe a KMS node");
        println!("note: {profile:?}");

        assert!(!profile.name.is_empty(), "every driver reports a name");
        assert!(
            profile.cursor_width >= 64 && profile.cursor_height >= 64,
            "the 64x64 fallback is the floor, not a value to fall below: {}x{}",
            profile.cursor_width,
            profile.cursor_height
        );
        assert!(
            profile.prime_import || profile.prime_export,
            "a KMS node that can share no buffer in either direction cannot \
             participate in anything"
        );
    }
}
