// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! Source-side HDR static metadata, and the property blob that carries it.
//!
//! A caller describes the content it is emitting -- transfer function,
//! mastering display primaries and luminance, content light levels -- and this
//! serialises it into the kernel's `struct hdr_output_metadata`, which the
//! connector's `HDR_OUTPUT_METADATA` property points at. The kernel turns that
//! into the HDR `InfoFrame` the sink receives, per CTA-861.3 and SMPTE ST 2086.
//!
//! Pointing the property at `0` instead clears HDR signalling and the sink
//! returns to SDR.

use drmkit_core::{Device, PropertyBlob, Result as CoreResult};

use crate::color::ColorimetryInfo;

/// Serialised size of `struct hdr_output_metadata`.
///
/// The struct is a `__u32` followed by a 26-byte infoframe at offset 4, which
/// pads to 32. Verified equal on x86-64 and aarch64 against the kernel's own
/// `<drm/drm_mode.h>` -- it is a UAPI struct, so this is fixed rather than
/// something to compute at build time, but it is worth stating where the
/// number came from.
pub const HDR_METADATA_SIZE: usize = 32;

/// Offset of the infoframe within `struct hdr_output_metadata`.
const INFOFRAME: usize = 4;

/// Static metadata descriptor type 1, CTA-861.3 §6.9.1.
///
/// The kernel calls this `HDMI_STATIC_METADATA_TYPE1` in `<linux/hdmi.h>`,
/// which is not part of the UAPI surface, so the constant is restated here
/// rather than pulled in.
const STATIC_METADATA_TYPE1: u8 = 0;

/// Source-side electro-optical transfer function.
///
/// The integer values are the kernel's `enum hdmi_eotf`. That enum is
/// kernel-internal, so the mapping is restated here to keep it stable for
/// userspace; [`TransferFunction::as_u8`] is the only place it is applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TransferFunction {
    /// BT.1886 / sRGB.
    #[default]
    TraditionalGammaSdr,
    /// Traditional gamma, HDR range.
    TraditionalGammaHdr,
    /// SMPTE ST 2084 (PQ) -- HDR10 and HDR10+.
    SmpteSt2084Pq,
    /// BT.2100 hybrid log-gamma.
    Bt2100Hlg,
}

impl TransferFunction {
    /// The value the kernel's `enum hdmi_eotf` gives this function.
    #[must_use]
    pub const fn as_u8(self) -> u8 {
        match self {
            Self::TraditionalGammaSdr => 0,
            Self::TraditionalGammaHdr => 1,
            Self::SmpteSt2084Pq => 2,
            Self::Bt2100Hlg => 3,
        }
    }
}

/// What the source is emitting.
///
/// Units follow CTA-861.3:
///
/// | field | unit |
/// |---|---|
/// | `max_display_mastering_luminance` | cd/m², 1 cd/m² steps |
/// | `min_display_mastering_luminance` | 0.0001 cd/m² steps |
/// | `max_content_light_level` | cd/m², 1 cd/m² steps |
/// | `max_frame_average_light_level` | cd/m², 1 cd/m² steps |
///
/// This describes the *content*, not the display it is going to. A sink that
/// cannot reach these levels tone-maps; that is the sink's business, and
/// misreporting here to flatter it produces worse results, not better.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct HdrSourceMetadata {
    /// Transfer function the content is encoded with.
    pub eotf: TransferFunction,
    /// Mastering display primaries and white point.
    ///
    /// Named by colour here; the serialiser reorders to the green/blue/red
    /// the wire format asks for.
    pub display_primaries: ColorimetryInfo,
    /// Peak luminance of the mastering display, cd/m².
    pub max_display_mastering_luminance: u16,
    /// Black level of the mastering display, in 0.0001 cd/m² steps.
    pub min_display_mastering_luminance: u16,
    /// Brightest pixel in the content, cd/m².
    pub max_content_light_level: u16,
    /// Brightest frame average in the content, cd/m².
    pub max_frame_average_light_level: u16,
}

/// CIE xy to the wire's 16-bit fixed point: 0.00002 steps, `0xC350` is 1.0.
///
/// Out-of-range and NaN both land on a defined value rather than a cast whose
/// result is whatever the hardware felt like. NaN is tested for explicitly:
/// `v < 0.0` alone is false for NaN, so it would fall through to the cast.
fn cie_to_u16(v: f32) -> u16 {
    if v.is_nan() || v < 0.0 {
        return 0;
    }
    let scaled = (v * 50000.0).round();
    if scaled >= 65535.0 {
        return u16::MAX;
    }
    // In range and non-negative by the two tests above, so this cannot wrap or
    // saturate to something surprising.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    {
        scaled as u16
    }
}

/// Serialise to the kernel's `struct hdr_output_metadata` byte layout.
///
/// The bytes go straight to `create_property_blob`. Written field by field at
/// measured offsets rather than by casting a `repr(C)` struct: the padding is
/// part of what gets hashed, so it has to be demonstrably zero rather than
/// whatever a struct literal left behind.
///
/// Primaries are reordered to green, blue, red -- CTA-861.3 §6.9.1.5 -- which
/// is the one transformation here that is easy to get backwards and impossible
/// to notice by looking at a working display, since it takes a saturated
/// wide-gamut source to show the difference.
#[must_use]
pub fn serialize_hdr_metadata(src: &HdrSourceMetadata) -> [u8; HDR_METADATA_SIZE] {
    let mut out = [0u8; HDR_METADATA_SIZE];

    // hdr_output_metadata.metadata_type, native endian like the rest.
    out[0..4].copy_from_slice(&u32::from(STATIC_METADATA_TYPE1).to_ne_bytes());

    out[INFOFRAME] = src.eotf.as_u8();
    out[INFOFRAME + 1] = STATIC_METADATA_TYPE1;

    let p = &src.display_primaries;
    let mut put = |offset: usize, value: u16| {
        out[offset..offset + 2].copy_from_slice(&value.to_ne_bytes());
    };
    // display_primaries[0..3] at infoframe offset 2, then white_point at 12.
    put(INFOFRAME + 2, cie_to_u16(p.green.x));
    put(INFOFRAME + 4, cie_to_u16(p.green.y));
    put(INFOFRAME + 6, cie_to_u16(p.blue.x));
    put(INFOFRAME + 8, cie_to_u16(p.blue.y));
    put(INFOFRAME + 10, cie_to_u16(p.red.x));
    put(INFOFRAME + 12, cie_to_u16(p.red.y));
    put(INFOFRAME + 14, cie_to_u16(p.white.x));
    put(INFOFRAME + 16, cie_to_u16(p.white.y));

    put(INFOFRAME + 18, src.max_display_mastering_luminance);
    put(INFOFRAME + 20, src.min_display_mastering_luminance);
    put(INFOFRAME + 22, src.max_content_light_level);
    put(INFOFRAME + 24, src.max_frame_average_light_level);

    // Bytes 30 and 31 are the struct's tail padding and stay zero.
    out
}

/// FNV-1a, 64-bit.
const fn fnv1a_64(data: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    let mut i = 0;
    while i < data.len() {
        hash ^= data[i] as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        i += 1;
    }
    hash
}

/// A stable hash of the serialised metadata.
///
/// Identical content hashes identically, and changing any single field changes
/// it. That is the whole job: it lets a cache tell "the same metadata again"
/// from "new metadata", so unchanged HDR does not rebuild a kernel blob every
/// frame.
#[must_use]
pub fn hdr_metadata_hash(src: &HdrSourceMetadata) -> u64 {
    fnv1a_64(&serialize_hdr_metadata(src))
}

/// What the cache needs of a blob, so it can be tested without a device.
///
/// The C++ grows a `synthesize_for_test` on the blob type itself -- a public
/// constructor that exists only so tests can make one without a DRM fd, and
/// which production code can call by accident. Naming the requirement instead
/// keeps the seam in the type system: the cache is generic, tests substitute
/// their own blob, and there is no back door on the real one.
pub trait HdrBlob {
    /// The blob id to write into `HDR_OUTPUT_METADATA`.
    fn blob_id(&self) -> u32;

    /// Give up the blob without asking the kernel to destroy it.
    ///
    /// For session loss, where the descriptor it was created on is gone. See
    /// [`PropertyBlob::forget`].
    fn forget(self);
}

/// A property blob holding serialised HDR metadata, freed when dropped.
///
/// The borrow of the device is what makes freeing it safe -- see
/// [`PropertyBlob`], whose reasoning this inherits along with its lifetime.
#[derive(Debug)]
pub struct HdrMetadataBlob<'a> {
    blob: PropertyBlob<'a>,
    content_hash: u64,
}

impl<'a> HdrMetadataBlob<'a> {
    /// Serialise `src` and hand it to the kernel as a property blob.
    ///
    /// # Errors
    ///
    /// [`drmkit_core::CoreError`] if the kernel refuses to create the blob.
    pub fn create(device: &'a Device, src: &HdrSourceMetadata) -> CoreResult<Self> {
        let bytes = serialize_hdr_metadata(src);
        Ok(Self {
            blob: device.create_property_blob(&bytes[..])?,
            content_hash: fnv1a_64(&bytes),
        })
    }

    /// The hash of the metadata this blob was built from.
    #[must_use]
    pub const fn content_hash(&self) -> u64 {
        self.content_hash
    }
}

impl HdrBlob for HdrMetadataBlob<'_> {
    fn blob_id(&self) -> u32 {
        // Blob ids are u32 in the UAPI; the crate widens them to u64 for
        // property values, which is where this comes back from.
        u32::try_from(self.blob.id()).unwrap_or(0)
    }

    fn forget(self) {
        self.blob.forget();
    }
}
