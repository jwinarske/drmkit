// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! The 64-bit DRM format modifier, its vendor families, and what each does to
//! bus traffic.
//!
//! Port of the `Modifier` / `BandwidthClass` / `classify` / `rotation_compatible`
//! surface of `src/fmt/format_mod.hpp`.

use core::fmt;

/// DRM modifier vendor identifiers (`DRM_FORMAT_MOD_VENDOR_*`, the top 8 bits).
pub mod vendor {
    /// No vendor: `LINEAR` and the `INVALID` sentinel live here.
    pub const NONE: u8 = 0x00;
    /// Intel.
    pub const INTEL: u8 = 0x01;
    /// AMD.
    pub const AMD: u8 = 0x02;
    /// NVIDIA.
    pub const NVIDIA: u8 = 0x03;
    /// Samsung.
    pub const SAMSUNG: u8 = 0x04;
    /// Qualcomm.
    pub const QCOM: u8 = 0x05;
    /// Vivante.
    pub const VIVANTE: u8 = 0x06;
    /// Broadcom (Raspberry Pi vc4 / V3D).
    pub const BROADCOM: u8 = 0x07;
    /// Arm.
    pub const ARM: u8 = 0x08;
    /// Allwinner.
    pub const ALLWINNER: u8 = 0x09;
    /// Amlogic.
    pub const AMLOGIC: u8 = 0x0a;
    /// `StarFive` / `VeriSilicon` DC8200.
    ///
    /// Out-of-tree (`starfive-tech/linux`): mainline `drm_fourcc.h` stops its
    /// vendor list at [`AMLOGIC`]. Carried because the `VisionFive` 2 is a
    /// first-class HIL target.
    pub const VS: u8 = 0x0b;
}

/// Build a modifier value from a vendor and a 56-bit body, matching the
/// `fourcc_mod_code` macro.
#[must_use]
pub const fn mod_code(vendor: u8, body: u64) -> u64 {
    ((vendor as u64) << 56) | (body & 0x00ff_ffff_ffff_ffff)
}

/// Legacy `DRM_FORMAT_MOD_ARM_16X16_BLOCK_U_INTERLEAVED`.
///
/// Type field [`arm_type::MISC`] with body 1, i.e. `0x0810_0000_0000_0001`.
/// Worth naming explicitly: `fourcc_mod_code(ARM, 1)` — which the C++ carries
/// as an `#ifndef` fallback for headers predating the type-field encoding —
/// is **not** this value. It is `DRM_FORMAT_MOD_ARM_AFBC(BLOCK_SIZE_16x16)`,
/// so a build picking up that fallback would classify 16x16 AFBC as tiling and
/// silently under-count its bandwidth saving.
pub const ARM_16X16_BLOCK_U_INTERLEAVED: Modifier =
    Modifier(mod_code(vendor::ARM, (arm_type::MISC << 52) | 1));

/// Arm modifier type field (bits 55:52).
pub mod arm_type {
    /// Arm Framebuffer Compression.
    pub const AFBC: u64 = 0x0;
    /// Miscellaneous tiling.
    pub const MISC: u64 = 0x1;
    /// Arm Fixed Rate Compression.
    pub const AFRC: u64 = 0x2;
}

/// AFBC superblock-size codes (low 4 bits of the modifier body).
pub mod afbc_block {
    /// 16x16 superblocks.
    pub const SIZE_16X16: u64 = 1;
    /// 32x8 superblocks.
    pub const SIZE_32X8: u64 = 2;
    /// 64x4 superblocks.
    pub const SIZE_64X4: u64 = 3;
    /// Mixed 32x8 / 64x4 superblocks.
    pub const SIZE_32X8_64X4: u64 = 4;
}

/// AFBC feature flags.
pub mod afbc_flag {
    /// YUV transform.
    pub const YTR: u64 = 1 << 4;
    /// Split-block layout.
    pub const SPLIT: u64 = 1 << 5;
    /// Sparse layout.
    pub const SPARSE: u64 = 1 << 6;
    /// Constant bit rate.
    pub const CBR: u64 = 1 << 7;
    /// Tiled header.
    pub const TILED: u64 = 1 << 8;
    /// Solid color opaque blocks.
    pub const SC: u64 = 1 << 9;
    /// Double buffering.
    pub const DB: u64 = 1 << 10;
    /// Block-check header.
    pub const BCH: u64 = 1 << 11;
    /// Uncompressed storage mode.
    pub const USM: u64 = 1 << 12;
}

/// Broadcom layout codes (low byte of the modifier body).
pub mod broadcom {
    /// vc4 T-tiled.
    pub const VC4_T_TILED: u64 = 1;
    /// SAND, 32-byte columns.
    pub const SAND32: u64 = 2;
    /// SAND, 64-byte columns.
    pub const SAND64: u64 = 3;
    /// SAND, 128-byte columns.
    pub const SAND128: u64 = 4;
    /// SAND, 256-byte columns.
    pub const SAND256: u64 = 5;
    /// V3D UIF.
    pub const UIF: u64 = 6;
}

/// Bit position of the AMD "displayable DCC" flag within the modifier.
///
/// `AMD_FMT_MOD_DCC_SHIFT` is 13 and is stable across GFX generations — only
/// the tile encoding is generation-specific — so reading this bit is exactly
/// what the canonical `AMD_FMT_MOD_GET(DCC, ...)` accessor extracts.
const AMD_DCC_SHIFT: u64 = 13;

/// A DRM format modifier.
///
/// The default is [`Modifier::LINEAR`], matching the C++ `Modifier` whose
/// member initializer is `DRM_FORMAT_MOD_LINEAR`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Modifier(pub u64);

impl Default for Modifier {
    fn default() -> Self {
        Self::LINEAR
    }
}

impl Modifier {
    /// Raw, untiled bytes (`DRM_FORMAT_MOD_LINEAR`).
    pub const LINEAR: Self = Self(0);

    /// The legacy "implementation-defined layout" sentinel
    /// (`DRM_FORMAT_MOD_INVALID`).
    ///
    /// Treated as interchangeable with [`Modifier::LINEAR`] for eligibility,
    /// mirroring the pre-`FormatTable` `supports_format_modifier` semantics.
    pub const INVALID: Self = Self(mod_code(vendor::NONE, (1 << 56) - 1));

    /// The vendor field (top 8 bits), as `fourcc_mod_get_vendor` reports it.
    #[must_use]
    pub const fn vendor(self) -> u8 {
        (self.0 >> 56) as u8
    }

    /// The 56-bit body below the vendor field.
    #[must_use]
    pub const fn body(self) -> u64 {
        self.0 & 0x00ff_ffff_ffff_ffff
    }

    /// Whether this is [`Modifier::LINEAR`].
    #[must_use]
    pub const fn is_linear(self) -> bool {
        self.0 == Self::LINEAR.0
    }

    /// Whether this is the [`Modifier::INVALID`] sentinel.
    #[must_use]
    pub const fn is_invalid(self) -> bool {
        self.0 == Self::INVALID.0
    }

    /// Whether the AMD "displayable DCC" bit is set.
    #[must_use]
    const fn amd_has_dcc(self) -> bool {
        self.vendor() == vendor::AMD && ((self.0 >> AMD_DCC_SHIFT) & 0x1) != 0
    }
}

impl From<u64> for Modifier {
    fn from(value: u64) -> Self {
        Self(value)
    }
}

impl From<Modifier> for u64 {
    fn from(value: Modifier) -> Self {
        value.0
    }
}

/// What a modifier does to *bus traffic*.
///
/// Best-effort classification by vendor and bit inspection, used only for
/// allocator scoring. **Correctness never depends on this being right** — the
/// atomic `TEST_ONLY` commit is the arbiter of what a plane can scan out.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub enum BandwidthClass {
    /// Raw bytes, poor DRAM locality.
    Linear,
    /// Same byte count, better locality: Broadcom UIF/SAND/T, Arm 16x16
    /// interleaved, NVIDIA block-linear with compression type 0.
    Tiling,
    /// Fewer bytes on the bus, data-dependent: AFBC/AFRC, AMD DCC, Qualcomm
    /// UBWC, NVIDIA block-linear with a nonzero compression type.
    Compression,
}

/// Classify a modifier's effect on scanout bandwidth.
///
/// ```
/// use drmkit_fmt::{BandwidthClass, Modifier, classify};
/// assert_eq!(classify(Modifier::LINEAR), BandwidthClass::Linear);
/// assert_eq!(classify(Modifier::INVALID), BandwidthClass::Linear);
/// ```
#[must_use]
pub const fn classify(m: Modifier) -> BandwidthClass {
    if m.is_linear() || m.is_invalid() {
        return BandwidthClass::Linear;
    }

    match m.vendor() {
        vendor::ARM => {
            // Legacy 16x16 interleaved is tiling, not AFBC. Its type field is
            // already MISC, so the type check below would reach the same
            // verdict; the explicit arm mirrors the C++ and documents intent.
            if m.0 == ARM_16X16_BLOCK_U_INTERLEAVED.0 {
                return BandwidthClass::Tiling;
            }
            let ty = (m.0 >> 52) & 0xf;
            // AFBC and AFRC are true compression; MISC is tiling.
            if ty == arm_type::AFBC || ty == arm_type::AFRC {
                BandwidthClass::Compression
            } else {
                BandwidthClass::Tiling
            }
        }
        vendor::AMD => {
            if m.amd_has_dcc() {
                BandwidthClass::Compression
            } else {
                BandwidthClass::Tiling
            }
        }
        // QCOM_COMPRESSED is UBWC.
        vendor::QCOM => BandwidthClass::Compression,
        vendor::NVIDIA => {
            // Bits 25:23 are the block-linear "lossless framebuffer compression
            // type" field: 0 = none, 1-4 = compressed (ROP/3D layouts 1 and 2,
            // CDE horizontal, CDE vertical). Mask the full 3 bits -- a 2-bit
            // mask misses CDE vertical (type 4 = 0b100) and calls it Tiling.
            if ((m.0 >> 23) & 0x7) != 0 {
                BandwidthClass::Compression
            } else {
                BandwidthClass::Tiling
            }
        }
        vendor::VS => {
            // VS packs a 2-bit TYPE at bits 55:54 -- NORMAL (tiling) vs
            // COMPRESSED (`DEC400` lossless FB compression). VS_LINEAR is the
            // all-zero body.
            if m.body() == 0 {
                return BandwidthClass::Linear;
            }
            if ((m.0 >> 54) & 0x3) == 0x1 {
                BandwidthClass::Compression
            } else {
                BandwidthClass::Tiling
            }
        }
        // Broadcom UIF/SAND/T buy locality only, never byte savings.
        // Unknown non-linear: assume tiled, never free.
        _ => BandwidthClass::Tiling,
    }
}

/// Display-plane rotation. 90/270 transpose the scanout fetch order; 0/180 do
/// not.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub enum Rotation {
    /// No rotation.
    #[default]
    Rotate0,
    /// 90 degrees.
    Rotate90,
    /// 180 degrees.
    Rotate180,
    /// 270 degrees.
    Rotate270,
}

impl Rotation {
    /// Whether this rotation transposes the scanout fetch order.
    #[must_use]
    pub const fn transposes(self) -> bool {
        matches!(self, Self::Rotate90 | Self::Rotate270)
    }
}

/// Can a buffer with modifier `m` be scanned out under rotation `r`?
///
/// A 90/270 rotation transposes the fetch order, which the display engine
/// cannot follow for a `LINEAR` buffer (a rotated fetch needs a tiled layout)
/// or for an AMD DCC metadata surface. Every other layout is left to the atomic
/// `TEST_ONLY` commit to confirm. 0/180 never transpose, so all modifiers pass.
///
/// This is a pre-filter for the negotiator only — correctness still rests on
/// the commit.
///
/// ```
/// use drmkit_fmt::{Modifier, Rotation, rotation_compatible};
/// assert!(rotation_compatible(Modifier::LINEAR, Rotation::Rotate180));
/// assert!(!rotation_compatible(Modifier::LINEAR, Rotation::Rotate90));
/// ```
#[must_use]
pub const fn rotation_compatible(m: Modifier, r: Rotation) -> bool {
    if !r.transposes() {
        return true; // 0/180 keep the walk order: any modifier is fine
    }
    if m.is_linear() {
        return false; // a rotated fetch needs a tiled layout, not raw bytes
    }
    // AMD DCC metadata cannot be walked transposed (the same bit classify uses).
    !m.amd_has_dcc()
}

impl fmt::Display for Modifier {
    /// Renders the human-readable description from [`crate::describe`].
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&crate::describe(*self))
    }
}
