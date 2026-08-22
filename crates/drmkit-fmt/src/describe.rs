// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! Human-readable modifier rendering. **For logging only** — nothing parses
//! these strings, and their exact spelling is not a stability guarantee.

use alloc::format;
use alloc::string::{String, ToString as _};

use crate::modifier::{Modifier, afbc_flag, arm_type, broadcom, vendor};

/// Vendor name for the top 8 bits.
fn vendor_name(v: u8) -> &'static str {
    match v {
        vendor::NONE => "NONE",
        vendor::INTEL => "INTEL",
        vendor::AMD => "AMD",
        vendor::NVIDIA => "NVIDIA",
        vendor::SAMSUNG => "SAMSUNG",
        vendor::QCOM => "QCOM",
        vendor::VIVANTE => "VIVANTE",
        vendor::BROADCOM => "BROADCOM",
        vendor::ARM => "ARM",
        vendor::ALLWINNER => "ALLWINNER",
        vendor::AMLOGIC => "AMLOGIC",
        vendor::VS => "VS",
        _ => "?",
    }
}

fn hex_suffix(v: u64) -> String {
    format!("({v:#018x})")
}

/// AMD tile-version field (bits 2:0).
fn amd_tile_version(v: u64) -> &'static str {
    match v {
        1 => "GFX9",
        2 => "GFX10",
        3 => "GFX10_RBPLUS",
        4 => "GFX11",
        _ => "GFX?",
    }
}

/// `AMD_FMT_MOD` packs tile version, swizzle mode, and DCC state. `dcc != 0` is
/// "displayable DCC" — the case `classify` reports as `Compression`.
fn amd_describe(m: u64) -> String {
    let tile_version = m & 0x7;
    let tile = (m >> 3) & 0x1f;
    let dcc = (m >> 13) & 0x1;
    let retile = (m >> 14) & 0x1;
    let pipe_align = (m >> 15) & 0x1;

    let mut s = String::from("AMD(");
    s.push_str(amd_tile_version(tile_version));
    s.push_str(" tile");
    s.push_str(&tile.to_string());
    s.push_str(" dcc=");
    s.push_str(&dcc.to_string());
    if retile != 0 {
        s.push_str(" retile");
    }
    if pipe_align != 0 {
        s.push_str(" pipe_align");
    }
    s.push(')');
    s
}

/// Decode an Arm modifier. AFBC (type field 0) is the lossless framebuffer
/// compression Mali-fed displays scan out; the superblock size and flags decide
/// whether a producer's buffer is scannable compressed.
fn arm_describe(m: u64) -> String {
    if m == crate::modifier::ARM_16X16_BLOCK_U_INTERLEAVED.0 {
        return String::from("ARM(16x16 interleaved)");
    }
    if ((m >> 52) & 0xf) != arm_type::AFBC {
        return format!("ARM{}", hex_suffix(m)); // AFRC / MISC -- not decoded
    }

    let body = m & 0x000f_ffff_ffff_ffff;
    let mut s = String::from("ARM_AFBC(");
    s.push_str(match body & 0xf {
        crate::modifier::afbc_block::SIZE_16X16 => "16x16",
        crate::modifier::afbc_block::SIZE_32X8 => "32x8",
        crate::modifier::afbc_block::SIZE_64X4 => "64x4",
        crate::modifier::afbc_block::SIZE_32X8_64X4 => "32x8_64x4",
        _ => "blk?",
    });

    for (flag, name) in [
        (afbc_flag::YTR, "YTR"),
        (afbc_flag::SPLIT, "SPLIT"),
        (afbc_flag::SPARSE, "SPARSE"),
        (afbc_flag::CBR, "CBR"),
        (afbc_flag::TILED, "TILED"),
        (afbc_flag::SC, "SC"),
        (afbc_flag::DB, "DB"),
        (afbc_flag::BCH, "BCH"),
        (afbc_flag::USM, "USM"),
    ] {
        if body & flag != 0 {
            s.push('|');
            s.push_str(name);
        }
    }

    s.push(')');
    s
}

/// Decode a Broadcom (Pi V3D / vc4) modifier. The low 8 bits are the layout
/// code; SAND variants carry a column height in the next 48 bits. UIF is V3D's
/// tiled render layout — locality only, no byte savings, which is why
/// `classify` reports Broadcom as `Tiling`.
fn broadcom_describe(m: u64) -> String {
    let code = m & 0xff;
    let param = (m >> 8) & ((1 << 48) - 1);
    match code {
        broadcom::VC4_T_TILED => String::from("BROADCOM(VC4_T_TILED)"),
        broadcom::SAND32 => format!("BROADCOM(SAND32 col={param})"),
        broadcom::SAND64 => format!("BROADCOM(SAND64 col={param})"),
        broadcom::SAND128 => format!("BROADCOM(SAND128 col={param})"),
        broadcom::SAND256 => format!("BROADCOM(SAND256 col={param})"),
        broadcom::UIF => String::from("BROADCOM(UIF)"),
        _ => format!("BROADCOM{}", hex_suffix(m)),
    }
}

/// Name for a `VeriSilicon` NORMAL (non-compressed) tile mode — the set the
/// DC8200 advertises. Values from the `starfive-tech/linux` uapi
/// `drm_fourcc.h`. `None` for reserved or unknown modes, so the caller falls
/// back to a numeric form.
fn vs_norm_tile(tile: u64) -> Option<&'static str> {
    Some(match tile {
        0x00 => "LINEAR",
        0x01 => "TILED4x4",
        0x02 => "SUPER_TILED_XMAJOR",
        0x03 => "SUPER_TILED_YMAJOR",
        0x04 => "TILE_8X8",
        0x05 => "TILE_MODE1",
        0x06 => "TILE_MODE2",
        0x07 => "TILE_8X4",
        0x08 => "TILE_MODE4",
        0x09 => "TILE_MODE5",
        0x0a => "TILE_MODE6",
        0x0b => "SUPER_TILED_XMAJOR_8X4",
        0x0c => "SUPER_TILED_YMAJOR_4X8",
        0x0d => "TILE_Y",
        0x0f => "TILE_128X1",
        0x10 => "TILE_256X1",
        0x11 => "TILE_32X1",
        0x12 => "TILE_64X1",
        0x15 => "TILE_MODE4X4",
        _ => return None,
    })
}

/// Decode a `VeriSilicon` (`StarFive` DC8200) modifier. A 2-bit TYPE sits at bits
/// 55:54; `VS_LINEAR` is `TYPE_NORMAL` with tile mode 0.
fn vs_describe(m: u64) -> String {
    match (m >> 54) & 0x3 {
        // `TYPE_NORMAL` (tiling). `NORM_MODE_MASK` is 5 bits.
        0x0 => {
            let tile = m & 0x1f;
            vs_norm_tile(tile).map_or_else(
                || format!("VS_TILED(mode={tile})"),
                |name| format!("VS_{name}"),
            )
        }
        // `TYPE_COMPRESSED` (`DEC400`): 6-bit tile mode plus align32/align64 flags.
        0x1 => {
            let mut s = format!("VS_DEC(tile={}", m & 0x3f);
            if m & (1 << 6) != 0 {
                s.push_str(", align32");
            }
            if m & (1 << 7) != 0 {
                s.push_str(", align64");
            }
            s.push(')');
            s
        }
        0x2 => format!("VS_CUSTOM_10BIT{}", hex_suffix(m)),
        _ => format!("VS{}", hex_suffix(m)),
    }
}

/// A human-readable rendering, e.g. `ARM_AFBC(16x16|SPARSE|YTR)` or
/// `QCOM_COMPRESSED(UBWC)`.
///
/// ```
/// use drmkit_fmt::{Modifier, describe};
/// assert_eq!(describe(Modifier::LINEAR), "LINEAR");
/// assert_eq!(describe(Modifier::INVALID), "INVALID");
/// ```
#[must_use]
pub fn describe(m: Modifier) -> String {
    if m.is_linear() {
        return String::from("LINEAR");
    }
    if m.is_invalid() {
        return String::from("INVALID");
    }

    match m.vendor() {
        vendor::ARM => arm_describe(m.0),
        vendor::AMD => amd_describe(m.0),
        vendor::QCOM => String::from("QCOM_COMPRESSED(UBWC)"),
        vendor::VS => vs_describe(m.0),
        vendor::NVIDIA => String::from("NVIDIA_BLOCK_LINEAR"),
        vendor::BROADCOM => broadcom_describe(m.0),
        other => format!("{}{}", vendor_name(other), hex_suffix(m.0)),
    }
}
