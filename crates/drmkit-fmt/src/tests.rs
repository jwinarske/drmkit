// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! Parity port of `tests/unit/test_format_mod.cpp` and the `format_bpp` half of
//! `tests/unit/test_format.cpp`, from drm-cxx @ `dc2915b`.
//!
//! The C++ `test_format_mod.cpp` is framework-free: one `main()` calling eight
//! `test_*()` functions, each accumulating into a global failure counter. Each
//! becomes a `#[test]` here, so a failure names the case instead of a line
//! number in a wall of `CHECK`s.

use super::*;
use alloc::vec::Vec;

// --- helpers -----------------------------------------------------------------

/// `DRM_FORMAT_MOD_ARM_AFBC(body)`.
const fn afbc(body: u64) -> Modifier {
    Modifier(mod_code(vendor::ARM, (arm_type::AFBC << 52) | body))
}

/// `DRM_FORMAT_MOD_ARM_16X16_BLOCK_U_INTERLEAVED`, verified against
/// `/usr/include/drm/drm_fourcc.h` as `0x0810_0000_0000_0001`.
const fn arm_16x16_interleaved() -> Modifier {
    ARM_16X16_BLOCK_U_INTERLEAVED
}

/// The two are distinct values. `fourcc_mod_code(ARM, 1)` is AFBC(16x16), not
/// the interleaved layout -- the trap the C++ `#ifndef` fallback sets.
#[test]
fn afbc_16x16_is_not_the_interleaved_layout() {
    assert_eq!(afbc(afbc_block::SIZE_16X16).0, 0x0800_0000_0000_0001);
    assert_eq!(arm_16x16_interleaved().0, 0x0810_0000_0000_0001);
    assert_ne!(afbc(afbc_block::SIZE_16X16), arm_16x16_interleaved());
}

/// `DRM_FORMAT_MOD_NVIDIA_BLOCK_LINEAR_2D(compression, ...)`: the compression
/// type occupies bits 25:23.
const fn nvidia_block_linear(compression: u64) -> Modifier {
    Modifier(mod_code(vendor::NVIDIA, (compression << 23) | 0x10))
}

/// `AMD_FMT_MOD` with GFX9 tiling and an explicit DCC bit.
const fn amd(dcc: u64) -> Modifier {
    // TILE_VERSION at 2:0 (GFX9 == 1), TILE at 7:3, DCC at 13.
    Modifier(mod_code(vendor::AMD, 1 | (9 << 3) | (dcc << 13)))
}

const VS_BASE: u64 = (vendor::VS as u64) << 56;

/// Build the synthetic `IN_FORMATS` blob the C++ `make_blob()` builds:
///
/// - formats: `[XRGB8888, ARGB8888]`
/// - modifiers: LINEAR over both (mask `0b11`), AFBC(16x16) over XRGB only
///   (mask `0b01`)
fn make_blob() -> Vec<u8> {
    make_blob_at(24, 24 + 8) // header is 24 bytes; formats then modifiers
}

/// Same blob, with the two array offsets chosen by the caller — used to prove
/// the parser tolerates unaligned offsets.
#[expect(
    clippy::cast_possible_truncation,
    reason = "every value here is a small literal fixture size; a truncating \
              cast would need a blob larger than addressable memory"
)]
fn make_blob_at(formats_offset: usize, modifiers_offset: usize) -> Vec<u8> {
    let formats = [fourcc::XRGB8888, fourcc::ARGB8888];
    let modifiers: [(u64, u32, u64); 2] = [
        (0b11, 0, Modifier::LINEAR.0),
        (0b01, 0, afbc(afbc_block::SIZE_16X16).0),
    ];

    let end = modifiers_offset + modifiers.len() * 24;
    let mut blob = alloc::vec![0u8; end];

    // Header: version, flags, count_formats, formats_offset, count_modifiers,
    // modifiers_offset -- six u32s.
    blob[0..4].copy_from_slice(&1u32.to_le_bytes()); // version
    blob[4..8].copy_from_slice(&0u32.to_le_bytes()); // flags
    blob[8..12].copy_from_slice(&(formats.len() as u32).to_le_bytes());
    blob[12..16].copy_from_slice(&(formats_offset as u32).to_le_bytes());
    blob[16..20].copy_from_slice(&(modifiers.len() as u32).to_le_bytes());
    blob[20..24].copy_from_slice(&(modifiers_offset as u32).to_le_bytes());

    for (i, f) in formats.iter().enumerate() {
        let at = formats_offset + i * 4;
        blob[at..at + 4].copy_from_slice(&f.to_le_bytes());
    }
    for (i, (mask, offset, modifier)) in modifiers.iter().enumerate() {
        let at = modifiers_offset + i * 24;
        blob[at..at + 8].copy_from_slice(&mask.to_le_bytes());
        blob[at + 8..at + 12].copy_from_slice(&offset.to_le_bytes());
        // bytes 12..16 are padding
        blob[at + 16..at + 24].copy_from_slice(&modifier.to_le_bytes());
    }
    blob
}

// --- test_format_table -------------------------------------------------------

#[test]
fn format_table_from_blob() {
    let blob = make_blob();
    let t = FormatTable::from_blob(&blob);

    assert_eq!(t.all().len(), 3);

    let lin = Modifier::LINEAR;
    let afbc16 = afbc(afbc_block::SIZE_16X16);

    assert!(t.supports(fourcc::XRGB8888, lin));
    assert!(t.supports(fourcc::ARGB8888, lin));
    assert!(t.supports(fourcc::XRGB8888, afbc16));
    assert!(
        !t.supports(fourcc::ARGB8888, afbc16),
        "AFBC is listed only for XRGB"
    );
    assert!(!t.supports(fourcc::RGB565, lin), "absent fourcc");

    assert_eq!(t.modifiers_for(fourcc::XRGB8888).count(), 2);
    assert_eq!(t.modifiers_for(fourcc::ARGB8888).count(), 1);
    assert_eq!(t.modifiers_for(fourcc::RGB565).count(), 0);
}

/// The C++ parser had a misaligned-read bug here and was fixed by `memcpy`-ing
/// each record out of the blob. The Rust parser reads byte-wise from a slice,
/// so unaligned offsets cannot arise as a concern — pinned so a future
/// "optimization" to a typed cast reintroduces the bug loudly.
#[test]
fn format_table_tolerates_unaligned_offsets() {
    // Formats at 25 (not 4-aligned), modifiers at 37 (not 8-aligned).
    let blob = make_blob_at(25, 37);
    let t = FormatTable::from_blob(&blob);

    assert_eq!(t.all().len(), 3);
    assert!(t.supports(fourcc::XRGB8888, afbc(afbc_block::SIZE_16X16)));
    assert!(t.supports(fourcc::ARGB8888, Modifier::LINEAR));
}

// --- test_format_table_malformed --------------------------------------------

#[test]
fn format_table_malformed_yields_empty() {
    assert!(FormatTable::from_blob(&[]).all().is_empty());
    assert!(FormatTable::from_blob(&[0u8; 8]).all().is_empty());
}

/// A hostile blob claiming more records than it holds must not read past the
/// end. In C++ this was a bounds check; here it is also a slice guard.
#[test]
fn format_table_rejects_out_of_range_counts() {
    let mut blob = make_blob();
    // Claim 4 billion modifiers in a 56-byte buffer.
    blob[16..20].copy_from_slice(&u32::MAX.to_le_bytes());
    assert!(FormatTable::from_blob(&blob).all().is_empty());

    let mut blob = make_blob();
    blob[8..12].copy_from_slice(&u32::MAX.to_le_bytes()); // count_formats
    assert!(FormatTable::from_blob(&blob).all().is_empty());

    let mut blob = make_blob();
    blob[12..16].copy_from_slice(&u32::MAX.to_le_bytes()); // formats_offset
    assert!(FormatTable::from_blob(&blob).all().is_empty());
}

/// A modifier record whose format index runs past `count_formats` is skipped,
/// not treated as a parse failure — the rest of the blob is still usable.
#[test]
fn format_table_skips_out_of_range_format_index() {
    let mut blob = make_blob();
    // Point the second modifier's format window past the end of the array.
    let second = 24 + 8 + 24;
    blob[second + 8..second + 12].copy_from_slice(&999u32.to_le_bytes());

    let t = FormatTable::from_blob(&blob);
    assert_eq!(
        t.all().len(),
        2,
        "LINEAR pairs survive; the AFBC pair is dropped"
    );
    assert!(t.supports(fourcc::XRGB8888, Modifier::LINEAR));
    assert!(!t.supports(fourcc::XRGB8888, afbc(afbc_block::SIZE_16X16)));
}

// --- test_from_pairs ---------------------------------------------------------

#[test]
fn from_pairs_dedups_and_indexes() {
    let lin = Modifier::LINEAR;
    let afbc16 = afbc(afbc_block::SIZE_16X16);

    let t = FormatTable::from_pairs([
        (fourcc::ARGB8888, lin),
        (fourcc::XRGB8888, afbc16),
        (fourcc::XRGB8888, lin),
        (fourcc::XRGB8888, lin), // duplicate
    ]);

    assert_eq!(t.all().len(), 3, "duplicate collapsed");
    assert!(t.supports(fourcc::XRGB8888, lin));
    assert!(t.supports(fourcc::ARGB8888, lin));
    assert!(t.supports(fourcc::XRGB8888, afbc16));
    assert!(!t.supports(fourcc::ARGB8888, afbc16));
    assert_eq!(t.modifiers_for(fourcc::XRGB8888).count(), 2);

    assert!(FormatTable::from_pairs([]).all().is_empty());
}

// --- test_classify -----------------------------------------------------------

#[test]
fn classify_linear_and_invalid() {
    assert_eq!(classify(Modifier::LINEAR), BandwidthClass::Linear);
    assert_eq!(classify(Modifier::INVALID), BandwidthClass::Linear);
}

#[test]
fn classify_arm() {
    assert_eq!(
        classify(afbc(afbc_block::SIZE_16X16)),
        BandwidthClass::Compression
    );
    assert_eq!(
        classify(arm_16x16_interleaved()),
        BandwidthClass::Tiling,
        "legacy 16x16 interleaved predates the type field and is tiling"
    );
    assert_eq!(
        classify(Modifier(mod_code(vendor::ARM, arm_type::AFRC << 52))),
        BandwidthClass::Compression
    );
    assert_eq!(
        classify(Modifier(mod_code(vendor::ARM, arm_type::MISC << 52))),
        BandwidthClass::Tiling
    );
}

#[test]
fn classify_qcom_and_broadcom() {
    assert_eq!(
        classify(Modifier(mod_code(vendor::QCOM, 1))),
        BandwidthClass::Compression,
        "QCOM_COMPRESSED is UBWC"
    );
    assert_eq!(
        classify(Modifier(mod_code(vendor::BROADCOM, broadcom::VC4_T_TILED))),
        BandwidthClass::Tiling,
        "Broadcom buys locality, never byte savings"
    );
}

/// Bits 25:23 are the full 3-bit compression-type field. A 2-bit mask would
/// miss CDE vertical (type 4 = `0b100`) and call it Tiling.
#[test]
fn classify_nvidia_uses_full_three_bit_field() {
    assert_eq!(classify(nvidia_block_linear(0)), BandwidthClass::Tiling);
    assert_eq!(
        classify(nvidia_block_linear(1)),
        BandwidthClass::Compression
    );
    assert_eq!(
        classify(nvidia_block_linear(4)),
        BandwidthClass::Compression,
        "CDE vertical must not be misread as Tiling"
    );
}

#[test]
fn classify_verisilicon() {
    assert_eq!(classify(Modifier(VS_BASE)), BandwidthClass::Linear); // VS_LINEAR
    assert_eq!(classify(Modifier(VS_BASE | 0x04)), BandwidthClass::Tiling);
    assert_eq!(classify(Modifier(VS_BASE | 0x0c)), BandwidthClass::Tiling);
    assert_eq!(
        classify(Modifier(VS_BASE | (1 << 54) | 0x02)),
        BandwidthClass::Compression,
        "TYPE_COMPRESSED is DEC400"
    );
}

#[test]
fn classify_amd_dcc() {
    assert_eq!(classify(amd(1)), BandwidthClass::Compression);
    assert_eq!(classify(amd(0)), BandwidthClass::Tiling);
}

#[test]
fn classify_unknown_vendor_is_tiling_not_free() {
    assert_eq!(
        classify(Modifier(mod_code(0x7e, 0x1234))),
        BandwidthClass::Tiling,
        "an unknown non-linear layout must never score as free"
    );
}

// --- test_rotation_compatible ------------------------------------------------

#[test]
fn rotation_zero_and_180_accept_everything() {
    let tiled = Modifier(mod_code(vendor::BROADCOM, broadcom::VC4_T_TILED));
    for m in [
        Modifier::LINEAR,
        afbc(afbc_block::SIZE_16X16),
        tiled,
        amd(1),
    ] {
        assert!(rotation_compatible(m, Rotation::Rotate0));
        assert!(rotation_compatible(m, Rotation::Rotate180));
    }
}

#[test]
fn rotation_90_270_rejects_linear_and_dcc() {
    let tiled = Modifier(mod_code(vendor::BROADCOM, broadcom::VC4_T_TILED));

    for r in [Rotation::Rotate90, Rotation::Rotate270] {
        assert!(
            !rotation_compatible(Modifier::LINEAR, r),
            "raw bytes cannot be fetched transposed"
        );
        assert!(
            !rotation_compatible(amd(1), r),
            "AMD DCC metadata cannot be walked transposed"
        );

        assert!(rotation_compatible(tiled, r));
        assert!(rotation_compatible(amd(0), r), "tiled non-DCC is fine");
        assert!(
            rotation_compatible(afbc(afbc_block::SIZE_16X16), r),
            "AFBC is left to the commit -- the design contract's ground truth"
        );
    }
}

// --- test_cost ---------------------------------------------------------------

#[test]
fn cost_linear_and_tiling_move_the_same_bytes() {
    let linear = scanout_cost_bytes(
        1920,
        1080,
        fourcc::XRGB8888,
        BandwidthClass::Linear,
        DEFAULT_COMPRESSED_RATIO,
    );
    assert_eq!(linear, 1920 * 1080 * 4);

    let tiling = scanout_cost_bytes(
        1920,
        1080,
        fourcc::XRGB8888,
        BandwidthClass::Tiling,
        DEFAULT_COMPRESSED_RATIO,
    );
    assert_eq!(tiling, linear);
}

#[test]
fn cost_compression_scales_by_ratio() {
    let linear = scanout_cost_bytes(
        1920,
        1080,
        fourcc::XRGB8888,
        BandwidthClass::Linear,
        DEFAULT_COMPRESSED_RATIO,
    );
    let half = scanout_cost_bytes(
        1920,
        1080,
        fourcc::XRGB8888,
        BandwidthClass::Compression,
        0.5,
    );
    assert_eq!(half, linear / 2);
}

#[test]
fn cost_expands_planar_formats() {
    let px = 1920u64 * 1080;
    let r = DEFAULT_COMPRESSED_RATIO;

    assert_eq!(
        scanout_cost_bytes(1920, 1080, fourcc::RGB565, BandwidthClass::Linear, r),
        px * 2
    );
    assert_eq!(
        scanout_cost_bytes(1920, 1080, fourcc::NV12, BandwidthClass::Linear, r),
        px * 3 / 2
    );
    assert_eq!(
        scanout_cost_bytes(1920, 1080, fourcc::P010, BandwidthClass::Tiling, r),
        px * 3
    );
}

#[test]
fn cost_unknown_format_is_not_free() {
    let px = 640u64 * 480;
    assert_eq!(
        scanout_cost_bytes(
            640,
            480,
            0x1234_5678,
            BandwidthClass::Linear,
            DEFAULT_COMPRESSED_RATIO
        ),
        px * 4,
        "an unrecognized layout falls back to 32bpp, never zero"
    );
}

// --- test_describe -----------------------------------------------------------

#[test]
fn describe_linear_and_invalid() {
    assert_eq!(describe(Modifier::LINEAR), "LINEAR");
    assert_eq!(describe(Modifier::INVALID), "INVALID");
}

#[test]
fn describe_afbc_surfaces_block_size_and_flags() {
    let s = describe(afbc(
        afbc_block::SIZE_16X16 | afbc_flag::SPARSE | afbc_flag::YTR,
    ));
    assert!(s.contains("AFBC"), "{s}");
    assert!(s.contains("16x16"), "{s}");
    assert!(s.contains("SPARSE"), "{s}");
    assert!(s.contains("YTR"), "{s}");
}

#[test]
fn describe_broadcom() {
    assert_eq!(
        describe(Modifier(mod_code(vendor::BROADCOM, broadcom::VC4_T_TILED))),
        "BROADCOM(VC4_T_TILED)"
    );
    assert!(describe(Modifier(mod_code(vendor::BROADCOM, broadcom::UIF))).contains("UIF"));
}

#[test]
fn describe_qcom() {
    assert_eq!(
        describe(Modifier(mod_code(vendor::QCOM, 1))),
        "QCOM_COMPRESSED(UBWC)"
    );
}

#[test]
fn describe_verisilicon() {
    assert_eq!(describe(Modifier(VS_BASE)), "VS_LINEAR");
    assert_eq!(describe(Modifier(VS_BASE | 0x02)), "VS_SUPER_TILED_XMAJOR");
    assert_eq!(describe(Modifier(VS_BASE | 0x04)), "VS_TILE_8X8");
    assert_eq!(
        describe(Modifier(VS_BASE | 0x0c)),
        "VS_SUPER_TILED_YMAJOR_4X8"
    );

    let dec = describe(Modifier(VS_BASE | (1 << 54) | 0x07 | (1 << 6)));
    assert!(dec.contains("VS_DEC"), "{dec}");
    assert!(dec.contains("tile=7"), "{dec}");
    assert!(dec.contains("align32"), "{dec}");
}

#[test]
fn describe_amd() {
    let s = describe(amd(1));
    assert!(s.contains("AMD"), "{s}");
    assert!(s.contains("GFX9"), "{s}");
    assert!(s.contains("dcc=1"), "{s}");
}

// --- test_probe_cache --------------------------------------------------------

#[test]
fn probe_cache_records_updates_and_invalidates() {
    let mut c = ModifierProbeCache::new();
    let m = afbc(afbc_block::SIZE_16X16);

    assert_eq!(c.lookup(1, 2, fourcc::XRGB8888, m), Verdict::Unknown);

    c.record(1, 2, fourcc::XRGB8888, m, false);
    assert_eq!(c.lookup(1, 2, fourcc::XRGB8888, m), Verdict::Rejected);

    c.record(1, 2, fourcc::XRGB8888, m, true); // update in place
    assert_eq!(c.lookup(1, 2, fourcc::XRGB8888, m), Verdict::Ok);
    assert_eq!(c.len(), 1, "updated in place, not appended");

    assert_eq!(
        c.lookup(9, 2, fourcc::XRGB8888, m),
        Verdict::Unknown,
        "a distinct crtc is an independent key"
    );

    c.invalidate_plane(2);
    assert_eq!(c.lookup(1, 2, fourcc::XRGB8888, m), Verdict::Unknown);
}

// --- format_bpp (test_format.cpp) -------------------------------------------

#[test]
fn format_bpp_known_formats() {
    assert_eq!(format_bpp(fourcc::C8), 8);
    assert_eq!(format_bpp(fourcc::RGB565), 16);
    assert_eq!(format_bpp(fourcc::RGB888), 24);
    assert_eq!(format_bpp(fourcc::XRGB8888), 32);
    assert_eq!(format_bpp(fourcc::ARGB8888), 32);
    assert_eq!(format_bpp(fourcc::XRGB2101010), 32);
    assert_eq!(format_bpp(fourcc::ARGB16161616F), 64);
}

/// `FormatTest.PackedHighBitDepthYuvIs32Bpp`
#[test]
fn format_bpp_packed_high_bit_depth_yuv_is_32() {
    for f in [fourcc::Y210, fourcc::Y212, fourcc::Y216] {
        assert_eq!(format_bpp(f), 32, "4:2:2 in u16 samples: 64 bits per 2 px");
    }
}

/// The 8-bit packed 4:2:2 formats are absent from the C++ `format_bpp` switch
/// and so report 0 here too. Pinned deliberately: it is a parity choice, not an
/// oversight, and the cost model charges them a conservative 32bpp either way.
#[test]
fn format_bpp_packed_8bit_yuv_matches_cxx_zero() {
    for f in [
        fourcc::YUYV,
        fourcc::YVYU,
        fourcc::UYVY,
        fourcc::VYUY,
        fourcc::AYUV,
    ] {
        assert_eq!(format_bpp(f), 0);
    }
    let px = 64u64 * 32;
    assert_eq!(
        scanout_cost_bytes(
            64,
            32,
            fourcc::YUYV,
            BandwidthClass::Linear,
            DEFAULT_COMPRESSED_RATIO
        ),
        px * 4,
        "conservative 32bpp fallback, matching the C++"
    );
}

/// `FormatTest.KnownFormatsReturnCorrectName`
#[test]
fn format_name_known_formats() {
    assert_eq!(format_name(fourcc::XRGB8888), "XRGB8888");
    assert_eq!(format_name(fourcc::ARGB8888), "ARGB8888");
    assert_eq!(format_name(fourcc::RGB565), "RGB565");
    assert_eq!(format_name(fourcc::NV12), "NV12");
    assert_eq!(format_name(fourcc::ABGR16161616F), "ABGR16161616F");
}

/// `FormatTest.HighBitDepthYuvFormatsHaveNames`
#[test]
fn format_name_high_bit_depth_yuv() {
    for (f, name) in [
        (fourcc::P010, "P010"),
        (fourcc::P012, "P012"),
        (fourcc::P016, "P016"),
        (fourcc::NV15, "NV15"),
        (fourcc::NV20, "NV20"),
        (fourcc::Y210, "Y210"),
        (fourcc::Y212, "Y212"),
        (fourcc::Y216, "Y216"),
    ] {
        assert_eq!(format_name(f), name);
    }
}

/// `FormatTest.UnknownFormatReturnsUnknown`
#[test]
fn format_name_unknown() {
    assert_eq!(format_name(0), "unknown");
    assert_eq!(format_name(0xdead_beef), "unknown");
}

#[test]
fn format_bpp_planar_formats_are_zero() {
    for f in [
        fourcc::NV12,
        fourcc::NV15,
        fourcc::NV16,
        fourcc::NV20,
        fourcc::NV21,
        fourcc::YUV420,
        fourcc::YUV422,
        fourcc::P010,
        fourcc::P012,
        fourcc::P016,
    ] {
        assert_eq!(format_bpp(f), 0, "planar: per-plane depth differs");
    }
}

#[test]
fn format_bpp_unknown_is_zero() {
    assert_eq!(format_bpp(0), 0);
    assert_eq!(format_bpp(0xdead_beef), 0);
}

// --- `ModifierFamily` / SAND (plan §4.7, upstream PR #231) --------------------

#[test]
fn sand_layer_fits_plane_advertising_the_base() {
    // A vc4 plane advertises SAND128 with column height 0.
    let plane = FormatTable::from_pairs([(fourcc::NV12, sand128_col_height(0))]);
    // rpi-hevc-dec hands over SAND128 with the coded image height baked in.
    let produced = sand128_col_height(1080);

    assert!(
        !plane.supports(fourcc::NV12, produced),
        "not an exact match"
    );
    assert!(
        layer_fits_plane(&plane, fourcc::NV12, produced),
        "but the kernel reads the column height from the FB, so it is eligible"
    );
}

#[test]
fn sand_match_is_vendor_and_code_gated() {
    let plane = FormatTable::from_pairs([(fourcc::NV12, sand128_col_height(0))]);

    // A different SAND code does not match SAND128's base.
    let sand64 = Modifier(mod_code(vendor::BROADCOM, (1080 << 8) | broadcom::SAND64));
    assert!(!layer_fits_plane(&plane, fourcc::NV12, sand64));

    // The relaxation is inert for every non-Broadcom modifier.
    assert!(!layer_fits_plane(
        &plane,
        fourcc::NV12,
        afbc(afbc_block::SIZE_16X16)
    ));
    assert_eq!(afbc(afbc_block::SIZE_16X16).family(), ModifierFamily::Exact);
}

/// SAND32 (code 2) is deliberately outside the family: upstream PR #231 names
/// `SAND64/128/256` explicitly, and widening it here would change plane
/// eligibility with no driver evidence behind it.
#[test]
fn sand32_is_not_a_family_member() {
    assert_eq!(
        Modifier(mod_code(vendor::BROADCOM, (1080 << 8) | broadcom::SAND32)).family(),
        ModifierFamily::Exact
    );
    for code in [broadcom::SAND64, broadcom::SAND128, broadcom::SAND256] {
        assert!(matches!(
            Modifier(mod_code(vendor::BROADCOM, (1080 << 8) | code)).family(),
            ModifierFamily::BroadcomSand { .. }
        ));
    }
}

#[test]
fn sand_col_height_round_trips() {
    assert_eq!(sand_col_height(sand128_col_height(1080)), Some(1080));
    assert_eq!(sand_col_height(sand128_col_height(0)), Some(0));
    assert_eq!(sand_col_height(Modifier::LINEAR), None);
}

// --- layer_fits_plane: LINEAR / INVALID interchangeability ------------------

#[test]
fn linear_and_invalid_are_interchangeable() {
    let plane_linear = FormatTable::from_pairs([(fourcc::XRGB8888, Modifier::LINEAR)]);
    assert!(layer_fits_plane(
        &plane_linear,
        fourcc::XRGB8888,
        Modifier::LINEAR
    ));
    assert!(layer_fits_plane(
        &plane_linear,
        fourcc::XRGB8888,
        Modifier::INVALID
    ));

    let plane_invalid = FormatTable::from_pairs([(fourcc::XRGB8888, Modifier::INVALID)]);
    assert!(layer_fits_plane(
        &plane_invalid,
        fourcc::XRGB8888,
        Modifier::LINEAR
    ));
    assert!(layer_fits_plane(
        &plane_invalid,
        fourcc::XRGB8888,
        Modifier::INVALID
    ));

    // A non-trivial modifier still needs an exact (or family) match.
    assert!(!layer_fits_plane(
        &plane_linear,
        fourcc::XRGB8888,
        afbc(afbc_block::SIZE_16X16)
    ));
    // And the fourcc must be present at all.
    assert!(!layer_fits_plane(
        &plane_linear,
        fourcc::NV12,
        Modifier::LINEAR
    ));
}

// --- parser robustness (T2 seed, runnable on stable) ------------------------

/// The `cargo-fuzz` target in `fuzz/fuzz_targets/in_formats_blob.rs` needs a
/// nightly toolchain, so it runs only in the fuzz lane. This covers the same
/// property in-process on stable, with a deterministic generator, so a
/// regression is caught by `cargo test` rather than waiting for the lane.
///
/// The property: `from_blob` is total. Any byte string yields a table that is
/// sorted, de-duplicated, and self-consistent — never a panic, never a read
/// past the end.
#[test]
#[expect(
    clippy::cast_possible_truncation,
    reason = "a pseudo-random generator feeding a fuzz-style input builder; \
              truncating the sample is exactly as valid as any other sample"
)]
fn from_blob_is_total_over_arbitrary_input() {
    // xorshift64*: deterministic, no dependency, good enough to shake the
    // offset and count fields through their interesting ranges.
    let mut state = 0x2545_f491_4f6c_dd1d_u64;
    let mut next = move || {
        state ^= state >> 12;
        state ^= state << 25;
        state ^= state >> 27;
        state.wrapping_mul(0x2545_f491_4f6c_dd1d)
    };

    let valid = make_blob();

    for case in 0..4_000 {
        let mut blob = match case % 4 {
            // Pure noise of varying length, including shorter than a header.
            0 => {
                let len = (next() % 96) as usize;
                (0..len).map(|_| (next() & 0xff) as u8).collect::<Vec<u8>>()
            }
            // A valid blob with one byte corrupted -- the header fields are the
            // interesting targets, and they live in the first 24 bytes.
            1 => {
                let mut b = valid.clone();
                let at = (next() as usize) % b.len();
                b[at] ^= (next() & 0xff) as u8;
                b
            }
            // A valid blob truncated at an arbitrary point.
            2 => {
                let mut b = valid.clone();
                b.truncate((next() as usize) % b.len());
                b
            }
            // A valid header with wild counts and offsets.
            _ => {
                let mut b = valid.clone();
                for field in 2..6 {
                    let at = field * 4;
                    let v = (next() as u32).to_le_bytes();
                    b[at..at + 4].copy_from_slice(&v);
                }
                b
            }
        };
        // Occasionally grow the buffer so large declared counts can be
        // satisfiable rather than always rejected by the bounds check.
        if next() % 8 == 0 {
            blob.resize(blob.len() + (next() % 512) as usize, 0);
        }

        let table = FormatTable::from_blob(&blob);

        for w in table.all().windows(2) {
            assert!(w[0] < w[1], "case {case}: table not sorted/deduplicated");
        }
        for pair in table.all() {
            assert!(
                table.supports(pair.fourcc, pair.modifier),
                "case {case}: listed pair not reported as supported"
            );
            assert!(
                table.modifiers_for(pair.fourcc).any(|m| m == pair.modifier),
                "case {case}: grouped index disagrees with the flat list"
            );
        }
    }
}
