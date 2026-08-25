// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! HDR static metadata and its per-CRTC blob cache.
//!
//! The serialisation tests read the bytes back at hand-written offsets rather
//! than through the writer's own constants, so a shifted field has to be wrong
//! twice in the same direction to pass.

use crate::color::{Chromaticity, ColorimetryInfo};
use crate::hdr::{
    HDR_METADATA_SIZE, HdrBlob, HdrSourceMetadata, TransferFunction, hdr_metadata_hash,
    serialize_hdr_metadata,
};
use crate::hdr_cache::HdrMetadataCache;

/// Read a native-endian `u16` out of the serialised metadata.
///
/// Offsets are relative to the start of `hdr_output_metadata`, taken from the
/// kernel header rather than from the writer: infoframe at 4, so `eotf` is 4,
/// `display_primaries` 6, `white_point` 18, luminances 22, 24, 26, 28.
fn u16_at(bytes: &[u8; HDR_METADATA_SIZE], offset: usize) -> u16 {
    u16::from_ne_bytes([bytes[offset], bytes[offset + 1]])
}

/// BT.2020 primaries, D65 white, PQ -- a plausible HDR10 master.
fn bt2020_pq_sample() -> HdrSourceMetadata {
    HdrSourceMetadata {
        eotf: TransferFunction::SmpteSt2084Pq,
        display_primaries: ColorimetryInfo {
            red: Chromaticity { x: 0.708, y: 0.292 },
            green: Chromaticity { x: 0.170, y: 0.797 },
            blue: Chromaticity { x: 0.131, y: 0.046 },
            white: Chromaticity {
                x: 0.3127,
                y: 0.3290,
            },
            has_primaries: true,
            has_default_white: true,
        },
        max_display_mastering_luminance: 1000,
        min_display_mastering_luminance: 50, // 0.005 cd/m²
        max_content_light_level: 1000,
        max_frame_average_light_level: 400,
    }
}

#[test]
fn serialize_lays_out_the_kernel_struct() {
    let bytes = serialize_hdr_metadata(&bt2020_pq_sample());

    assert_eq!(bytes.len(), HDR_METADATA_SIZE);
    assert_eq!(
        u32::from_ne_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]),
        0,
        "metadata_type is HDMI_STATIC_METADATA_TYPE1"
    );
    assert_eq!(bytes[4], 2, "eotf is SMPTE ST 2084");
    assert_eq!(bytes[5], 0, "the infoframe restates the descriptor type");
    assert_eq!(u16_at(&bytes, 22), 1000, "max display mastering luminance");
    assert_eq!(u16_at(&bytes, 24), 50, "min display mastering luminance");
    assert_eq!(u16_at(&bytes, 26), 1000, "MaxCLL");
    assert_eq!(u16_at(&bytes, 28), 400, "MaxFALL");
    assert_eq!(
        &bytes[30..32],
        &[0, 0],
        "the struct's tail padding is hashed, so it has to be zero"
    );
}

#[test]
fn primaries_are_reordered_to_green_blue_red() {
    // CTA-861.3 §6.9.1.5 writes green, blue, red -- not the red, green, blue
    // the struct is named for. Getting this backwards produces a display that
    // looks correct on anything but a saturated wide-gamut source.
    let bytes = serialize_hdr_metadata(&bt2020_pq_sample());

    // 0.00002 steps: 0.170 -> 8500, 0.797 -> 39850, and so on. Rounding can
    // land a LSB either side.
    let near = |got: u16, want: u16, what: &str| {
        assert!(
            got.abs_diff(want) <= 1,
            "{what}: got {got}, want {want} +/- 1"
        );
    };
    near(u16_at(&bytes, 6), 8500, "primaries[0].x is green");
    near(u16_at(&bytes, 8), 39850, "primaries[0].y is green");
    near(u16_at(&bytes, 10), 6550, "primaries[1].x is blue");
    near(u16_at(&bytes, 12), 2300, "primaries[1].y is blue");
    near(u16_at(&bytes, 14), 35400, "primaries[2].x is red");
    near(u16_at(&bytes, 16), 14600, "primaries[2].y is red");
    near(u16_at(&bytes, 18), 15635, "white point x is D65");
    near(u16_at(&bytes, 20), 16450, "white point y is D65");
}

#[test]
fn out_of_range_coordinates_clamp() {
    let src = HdrSourceMetadata {
        display_primaries: ColorimetryInfo {
            red: Chromaticity { x: -0.1, y: 5.0 },
            green: Chromaticity { x: 0.0, y: 1.31 },
            blue: Chromaticity { x: 2.0, y: 0.0 },
            white: Chromaticity::default(),
            has_primaries: true,
            has_default_white: true,
        },
        ..HdrSourceMetadata::default()
    };
    let bytes = serialize_hdr_metadata(&src);

    assert_eq!(u16_at(&bytes, 14), 0, "red.x of -0.1 floors at 0");
    assert_eq!(u16_at(&bytes, 16), u16::MAX, "red.y of 5.0 saturates");
    assert_eq!(u16_at(&bytes, 6), 0, "green.x of 0.0 is 0");
    assert_eq!(
        u16_at(&bytes, 8),
        65500,
        "green.y of 1.31 is in range and must not saturate"
    );
    assert_eq!(u16_at(&bytes, 10), u16::MAX, "blue.x of 2.0 saturates");
}

#[test]
fn a_nan_coordinate_serialises_to_zero() {
    // Not in the C++ suite. NaN is tested for explicitly in the serialiser
    // because `v < 0.0` alone is false for NaN; without this case a
    // range check written that way would pass everything else here and cast
    // NaN to whatever the hardware produces.
    let src = HdrSourceMetadata {
        display_primaries: ColorimetryInfo {
            red: Chromaticity {
                x: f32::NAN,
                y: f32::INFINITY,
            },
            green: Chromaticity {
                x: f32::NEG_INFINITY,
                y: 0.5,
            },
            ..ColorimetryInfo::default()
        },
        ..HdrSourceMetadata::default()
    };
    let bytes = serialize_hdr_metadata(&src);

    assert_eq!(u16_at(&bytes, 14), 0, "NaN is not a coordinate");
    assert_eq!(u16_at(&bytes, 16), u16::MAX, "+inf saturates");
    assert_eq!(u16_at(&bytes, 6), 0, "-inf floors");
    assert_eq!(u16_at(&bytes, 8), 25000, "an ordinary 0.5 still works");
}

#[test]
fn eotf_integers_match_the_hdmi_enum() {
    for (eotf, want) in [
        (TransferFunction::TraditionalGammaSdr, 0u8),
        (TransferFunction::TraditionalGammaHdr, 1),
        (TransferFunction::SmpteSt2084Pq, 2),
        (TransferFunction::Bt2100Hlg, 3),
    ] {
        let src = HdrSourceMetadata {
            eotf,
            ..HdrSourceMetadata::default()
        };
        assert_eq!(
            serialize_hdr_metadata(&src)[4],
            want,
            "{eotf:?} is {want} in the kernel's enum hdmi_eotf"
        );
    }
}

#[test]
fn default_metadata_serialises_to_zeroes() {
    let bytes = serialize_hdr_metadata(&HdrSourceMetadata::default());
    assert_eq!(
        bytes, [0u8; HDR_METADATA_SIZE],
        "a default source is SDR with no primaries, which is all-zero"
    );
}

#[test]
fn identical_content_hashes_identically() {
    assert_eq!(
        hdr_metadata_hash(&bt2020_pq_sample()),
        hdr_metadata_hash(&bt2020_pq_sample())
    );
}

/// A named single-field edit.
type Mutation = (&'static str, fn(&mut HdrSourceMetadata));

#[test]
fn every_field_moves_the_hash() {
    // The cache skips kernel work on a hash match, so a field that does not
    // reach the hash is a field whose change never reaches the display.
    let base = bt2020_pq_sample();
    let base_hash = hdr_metadata_hash(&base);

    let mutations: [Mutation; 7] = [
        ("eotf", |m| m.eotf = TransferFunction::Bt2100Hlg),
        ("MaxCLL", |m| m.max_content_light_level = 999),
        ("MaxFALL", |m| m.max_frame_average_light_level = 399),
        ("max mastering luminance", |m| {
            m.max_display_mastering_luminance = 4000;
        }),
        ("min mastering luminance", |m| {
            m.min_display_mastering_luminance = 5;
        }),
        ("a single primary", |m| {
            m.display_primaries.red.x = 0.700;
        }),
        ("the white point", |m| {
            m.display_primaries.white.x = 0.300;
        }),
    ];

    for (what, mutate) in mutations {
        let mut changed = base;
        mutate(&mut changed);
        assert_ne!(
            hdr_metadata_hash(&changed),
            base_hash,
            "changing {what} has to change the hash"
        );
    }
}

#[test]
fn the_hash_covers_the_bytes_the_kernel_sees() {
    // FNV-1a over the serialised struct, checked against an independent
    // walk of the same bytes -- so a change to the hash function has to be
    // deliberate rather than incidental.
    let src = bt2020_pq_sample();
    let bytes = serialize_hdr_metadata(&src);
    let mut expected: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in bytes {
        expected ^= u64::from(byte);
        expected = expected.wrapping_mul(0x0000_0100_0000_01b3);
    }
    assert_eq!(hdr_metadata_hash(&src), expected);
}

// --- the cache -------------------------------------------------------------

/// A blob that never touches a device.
///
/// The C++ needs a `synthesize_for_test` constructor on the real blob type for
/// this; here the cache is generic over [`HdrBlob`], so the fake is just
/// another implementation and the real type keeps no test-only door.
#[derive(Debug)]
struct FakeBlob {
    id: u32,
    forgotten: std::rc::Rc<std::cell::Cell<usize>>,
}

impl HdrBlob for FakeBlob {
    fn blob_id(&self) -> u32 {
        self.id
    }
    fn forget(self) {
        self.forgotten.set(self.forgotten.get() + 1);
    }
}

/// Mints blobs with increasing ids and counts how often it was asked.
struct Factory {
    calls: usize,
    next_id: u32,
    forgotten: std::rc::Rc<std::cell::Cell<usize>>,
}

impl Factory {
    fn new() -> Self {
        Self {
            calls: 0,
            // Far from zero, which is the "nothing cached" sentinel.
            next_id: 1000,
            forgotten: std::rc::Rc::new(std::cell::Cell::new(0)),
        }
    }

    /// Always succeeds. The `Result` is the shape `set_with` asks of a
    /// factory, and `a_failing_factory_leaves_the_cache_as_it_was` covers the
    /// other arm with a closure of its own.
    #[allow(clippy::unnecessary_wraps)]
    fn make(&mut self, _: &HdrSourceMetadata) -> Result<FakeBlob, ()> {
        self.calls += 1;
        let id = self.next_id;
        self.next_id += 1;
        Ok(FakeBlob {
            id,
            forgotten: std::rc::Rc::clone(&self.forgotten),
        })
    }
}

/// Distinct metadata per seed, so successive samples hash differently.
fn sample(seed: u16) -> HdrSourceMetadata {
    HdrSourceMetadata {
        eotf: TransferFunction::SmpteSt2084Pq,
        max_content_light_level: seed,
        ..HdrSourceMetadata::default()
    }
}

#[test]
fn an_empty_cache_reports_nothing() {
    let cache: HdrMetadataCache<FakeBlob> = HdrMetadataCache::new();
    assert_eq!(cache.current_blob_id(1), 0);
    assert_eq!(cache.current_blob_id(42), 0);
    assert_eq!(cache.current_content_hash(1), 0);
    assert_eq!(cache.pending_destruction_count(), 0);
}

#[test]
fn the_first_set_builds_a_blob() {
    let mut cache = HdrMetadataCache::new();
    let mut factory = Factory::new();
    let src = sample(100);

    let id = cache
        .set_with(1, Some(&src), |s| factory.make(s))
        .expect("factory");

    assert_eq!(id, 1000);
    assert_eq!(factory.calls, 1);
    assert_eq!(cache.current_blob_id(1), 1000);
    assert_eq!(cache.current_content_hash(1), hdr_metadata_hash(&src));
    assert_eq!(cache.pending_destruction_count(), 0);
}

#[test]
fn identical_content_reuses_the_cached_blob() {
    // The point of the cache: unchanged HDR must not rebuild a kernel blob
    // every frame.
    let mut cache = HdrMetadataCache::new();
    let mut factory = Factory::new();
    let src = sample(100);

    let first = cache
        .set_with(1, Some(&src), |s| factory.make(s))
        .expect("factory");
    let second = cache
        .set_with(1, Some(&sample(100)), |s| factory.make(s))
        .expect("factory");

    assert_eq!(first, second);
    assert_eq!(factory.calls, 1, "the factory must not have run again");
    assert_eq!(cache.pending_destruction_count(), 0);
}

#[test]
fn changed_content_queues_the_old_blob_rather_than_freeing_it() {
    // The kernel still holds the old id until the commit repoints the
    // property, so freeing here would pull a blob out from under the display.
    let mut cache = HdrMetadataCache::new();
    let mut factory = Factory::new();

    cache
        .set_with(1, Some(&sample(100)), |s| factory.make(s))
        .expect("factory");
    let second = cache
        .set_with(1, Some(&sample(200)), |s| factory.make(s))
        .expect("factory");

    assert_eq!(second, 1001);
    assert_eq!(factory.calls, 2);
    assert_eq!(cache.current_blob_id(1), 1001);
    assert_eq!(cache.pending_destruction_count(), 1);
}

#[test]
fn clearing_returns_zero_and_queues_the_old_blob() {
    let mut cache = HdrMetadataCache::new();
    let mut factory = Factory::new();

    cache
        .set_with(1, Some(&sample(100)), |s| factory.make(s))
        .expect("factory");
    let cleared = cache
        .set_with(1, None, |s| factory.make(s))
        .expect("no factory call");

    assert_eq!(cleared, 0, "zero is what clears HDR on the connector");
    assert_eq!(cache.current_blob_id(1), 0);
    assert_eq!(cache.current_content_hash(1), 0);
    assert_eq!(cache.pending_destruction_count(), 1);
    assert_eq!(factory.calls, 1, "clearing builds nothing");
}

#[test]
fn clearing_a_crtc_with_nothing_cached_does_nothing() {
    let mut cache: HdrMetadataCache<FakeBlob> = HdrMetadataCache::new();
    let mut factory = Factory::new();

    let cleared = cache
        .set_with(7, None, |s| factory.make(s))
        .expect("no call");

    assert_eq!(cleared, 0);
    assert_eq!(cache.pending_destruction_count(), 0);
    assert_eq!(factory.calls, 0);
}

#[test]
fn acknowledging_a_commit_drains_the_queue() {
    let mut cache = HdrMetadataCache::new();
    let mut factory = Factory::new();

    cache
        .set_with(1, Some(&sample(100)), |s| factory.make(s))
        .expect("factory");
    cache
        .set_with(1, Some(&sample(200)), |s| factory.make(s))
        .expect("factory");
    assert_eq!(cache.pending_destruction_count(), 1);

    cache.acknowledge_committed();

    assert_eq!(cache.pending_destruction_count(), 0);
    assert_eq!(cache.current_blob_id(1), 1001, "the live blob is untouched");
}

#[test]
fn acknowledging_an_empty_queue_does_nothing() {
    let mut cache: HdrMetadataCache<FakeBlob> = HdrMetadataCache::new();
    cache.acknowledge_committed();
    assert_eq!(cache.pending_destruction_count(), 0);
}

#[test]
fn crtcs_do_not_share_state() {
    let mut cache = HdrMetadataCache::new();
    let mut factory = Factory::new();

    let a = cache
        .set_with(1, Some(&sample(100)), |s| factory.make(s))
        .expect("factory");
    let b = cache
        .set_with(2, Some(&sample(200)), |s| factory.make(s))
        .expect("factory");

    assert_ne!(a, b);
    assert_eq!(cache.current_blob_id(1), a);
    assert_eq!(cache.current_blob_id(2), b);
    assert_eq!(
        cache.pending_destruction_count(),
        0,
        "two CRTCs is not an eviction"
    );

    // Clearing one leaves the other alone.
    cache.set_with(1, None, |s| factory.make(s)).expect("clear");
    assert_eq!(cache.current_blob_id(1), 0);
    assert_eq!(cache.current_blob_id(2), b);
}

#[test]
fn a_failing_factory_leaves_the_cache_as_it_was() {
    // A caller that ignores the error and commits anyway should show the old
    // metadata, not a freed blob and not none.
    let mut cache = HdrMetadataCache::new();
    let mut factory = Factory::new();

    let first = cache
        .set_with(1, Some(&sample(100)), |s| factory.make(s))
        .expect("factory");

    let failed: Result<u32, &str> = cache.set_with(1, Some(&sample(200)), |_| Err("no blobs"));

    assert_eq!(failed, Err("no blobs"));
    assert_eq!(
        cache.current_blob_id(1),
        first,
        "the old blob is still live"
    );
    assert_eq!(
        cache.current_content_hash(1),
        hdr_metadata_hash(&sample(100))
    );
    assert_eq!(
        cache.pending_destruction_count(),
        0,
        "nothing was evicted, so nothing is queued"
    );
}

#[test]
fn clearing_then_setting_again_rebuilds() {
    let mut cache = HdrMetadataCache::new();
    let mut factory = Factory::new();

    cache
        .set_with(1, Some(&sample(100)), |s| factory.make(s))
        .expect("factory");
    cache.set_with(1, None, |s| factory.make(s)).expect("clear");
    let again = cache
        .set_with(1, Some(&sample(100)), |s| factory.make(s))
        .expect("factory");

    assert_eq!(
        factory.calls, 2,
        "the cleared entry is gone, so the same content builds afresh"
    );
    assert_eq!(again, 1001);
    assert_eq!(cache.pending_destruction_count(), 1);
}

#[test]
fn session_loss_abandons_blobs_instead_of_destroying_them() {
    // The descriptor they were made on is gone and the kernel already
    // reclaimed them. Destroying through a replacement descriptor could free
    // an unrelated blob that was handed the same id.
    let mut cache = HdrMetadataCache::new();
    let mut factory = Factory::new();
    let forgotten = std::rc::Rc::clone(&factory.forgotten);

    cache
        .set_with(1, Some(&sample(100)), |s| factory.make(s))
        .expect("factory");
    cache
        .set_with(1, Some(&sample(200)), |s| factory.make(s))
        .expect("factory");
    cache
        .set_with(2, Some(&sample(300)), |s| factory.make(s))
        .expect("factory");
    assert_eq!(cache.pending_destruction_count(), 1);

    cache.clear_for_session_loss();

    assert_eq!(
        forgotten.get(),
        3,
        "two live blobs and one queued, all abandoned rather than destroyed"
    );
    assert_eq!(cache.current_blob_id(1), 0);
    assert_eq!(cache.current_blob_id(2), 0);
    assert_eq!(cache.pending_destruction_count(), 0);
}

#[test]
fn flush_destroys_everything_it_holds() {
    let mut cache = HdrMetadataCache::new();
    let mut factory = Factory::new();
    let forgotten = std::rc::Rc::clone(&factory.forgotten);

    cache
        .set_with(1, Some(&sample(100)), |s| factory.make(s))
        .expect("factory");
    cache
        .set_with(1, Some(&sample(200)), |s| factory.make(s))
        .expect("factory");

    cache.flush();

    assert_eq!(cache.current_blob_id(1), 0);
    assert_eq!(cache.pending_destruction_count(), 0);
    assert_eq!(
        forgotten.get(),
        0,
        "flush destroys through a live descriptor -- abandoning would leak"
    );
}

// --- differential parity ---------------------------------------------------

/// Every vector drm-cxx's own serialiser produced, byte for byte.
///
/// Generated by `parity/hdr-oracle.cc` against the reference, not derived
/// here. Two things this catches that a reading of CTA-861.3 would not: the
/// green/blue/red reordering, and whether the struct's tail padding reaches
/// the hash. Neither is visible on a screen, and both change the bytes.
const ORACLE: &[(&str, &str, u64)] = &[
    (
        "default",
        "0000000000000000000000000000000000000000000000000000000000000000",
        0x0c82_1078_4d8a_f5a5,
    ),
    (
        "bt2020_pq",
        "0000000002003421aa9b9619fc08488a0839133d4240e8033200e80390010000",
        0x66f0_ff7b_ec7e_4004,
    ),
    (
        "eotf_0",
        "0000000000003421aa9b9619fc08488a0839133d4240e8033200e80390010000",
        0xf08c_7c96_2829_6006,
    ),
    (
        "eotf_1",
        "0000000001003421aa9b9619fc08488a0839133d4240e8033200e80390010000",
        0x1502_184a_d523_5ec7,
    ),
    (
        "eotf_2",
        "0000000002003421aa9b9619fc08488a0839133d4240e8033200e80390010000",
        0x66f0_ff7b_ec7e_4004,
    ),
    (
        "eotf_3",
        "0000000003003421aa9b9619fc08488a0839133d4240e8033200e80390010000",
        0xe45f_dea5_ea3a_dded,
    ),
    (
        "clamped",
        "0000000000000000dcffffff00000000ffff0000000000000000000000000000",
        0xea46_e34b_5e35_5648,
    ),
    (
        "luminance_only",
        "0000000000000000000000000000000000000000000022114433665588770000",
        0x5ce7_6441_6fc7_fc5d,
    ),
    (
        "only_red",
        "0000000000000000000000000000d4307c920000000000000000000000000000",
        0xa5f7_1dbc_dc47_5877,
    ),
    (
        "only_green",
        "000000000000d4307c9200000000000000000000000000000000000000000000",
        0xcaaf_c429_d879_e737,
    ),
    (
        "only_blue",
        "00000000000000000000d4307c92000000000000000000000000000000000000",
        0x0848_53dc_4057_4957,
    ),
    (
        "only_white",
        "000000000000000000000000000000000000d4307c9200000000000000000000",
        0x45f5_c3fc_a122_dd17,
    ),
];

/// Rebuild the oracle's cases by name.
fn oracle_case(name: &str) -> HdrSourceMetadata {
    let one = Chromaticity { x: 0.25, y: 0.75 };
    match name {
        "default" => HdrSourceMetadata::default(),
        "bt2020_pq" | "eotf_2" => bt2020_pq_sample(),
        "eotf_0" => HdrSourceMetadata {
            eotf: TransferFunction::TraditionalGammaSdr,
            ..bt2020_pq_sample()
        },
        "eotf_1" => HdrSourceMetadata {
            eotf: TransferFunction::TraditionalGammaHdr,
            ..bt2020_pq_sample()
        },
        "eotf_3" => HdrSourceMetadata {
            eotf: TransferFunction::Bt2100Hlg,
            ..bt2020_pq_sample()
        },
        "clamped" => HdrSourceMetadata {
            display_primaries: ColorimetryInfo {
                red: Chromaticity { x: -0.1, y: 5.0 },
                green: Chromaticity { x: 0.0, y: 1.31 },
                blue: Chromaticity { x: 2.0, y: 0.0 },
                ..ColorimetryInfo::default()
            },
            ..HdrSourceMetadata::default()
        },
        "luminance_only" => HdrSourceMetadata {
            max_display_mastering_luminance: 0x1122,
            min_display_mastering_luminance: 0x3344,
            max_content_light_level: 0x5566,
            max_frame_average_light_level: 0x7788,
            ..HdrSourceMetadata::default()
        },
        "only_red" => primaries_with(|p| p.red = one),
        "only_green" => primaries_with(|p| p.green = one),
        "only_blue" => primaries_with(|p| p.blue = one),
        "only_white" => primaries_with(|p| p.white = one),
        other => panic!("unknown oracle case {other}"),
    }
}

fn primaries_with(set: impl FnOnce(&mut ColorimetryInfo)) -> HdrSourceMetadata {
    let mut primaries = ColorimetryInfo::default();
    set(&mut primaries);
    HdrSourceMetadata {
        display_primaries: primaries,
        ..HdrSourceMetadata::default()
    }
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;

    bytes.iter().fold(String::new(), |mut out, b| {
        let _ = write!(out, "{b:02x}");
        out
    })
}

#[test]
fn serialisation_matches_drm_cxx_byte_for_byte() {
    for (name, want_bytes, want_hash) in ORACLE {
        let src = oracle_case(name);
        assert_eq!(
            hex(&serialize_hdr_metadata(&src)),
            *want_bytes,
            "{name}: bytes differ from drm-cxx"
        );
        assert_eq!(
            hdr_metadata_hash(&src),
            *want_hash,
            "{name}: hash differs from drm-cxx"
        );
    }
}

#[test]
fn the_oracle_distinguishes_every_case() {
    // A table where two rows accidentally carry the same bytes would pass the
    // comparison above while testing half as much as it looks like it does.
    // The two EOTF rows that are meant to coincide are the exception.
    let mut seen: Vec<(&str, &str)> = Vec::new();
    for (name, bytes, _) in ORACLE {
        if let Some((other, _)) = seen.iter().find(|(_, b)| b == bytes) {
            assert!(
                matches!((*other, *name), ("bt2020_pq", "eotf_2")),
                "{name} and {other} serialise identically, so one of them tests nothing"
            );
        }
        seen.push((name, bytes));
    }
    assert_eq!(seen.len(), 12);
}
