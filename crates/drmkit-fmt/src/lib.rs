// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

#![no_std]

//! Format-modifier and hardware-compression support for drmkit's plane
//! allocator.
//!
//! Port of `src/fmt/` from drm-cxx @ `dc2915b`.
//!
//! # Design contract
//!
//! Carried verbatim from the C++, because the allocator depends on all three:
//!
//! - **`IN_FORMATS` prunes the bipartite graph — necessary, not sufficient.**
//! - **The atomic `TEST_ONLY` commit is ground truth** for what a plane can
//!   scan out.
//! - **[`BandwidthClass`] is a cost input to placement scoring, never a
//!   correctness gate.**
//!
//! This keeps every vendor on one code path — AFBC (Arm/Rockchip/MediaTek),
//! AMD DCC, Qualcomm UBWC, NVIDIA block-linear, Broadcom tiling: a
//! `(fourcc, modifier)` pair flows from the producer's allocation, through
//! `IN_FORMATS` eligibility, to a probed-and-memoized test commit.
//!
//! # Scope
//!
//! This crate is `no_std + alloc` and device-free. Two pieces of the C++
//! `fmt/` header live elsewhere in the port:
//!
//! - `FormatTable::from_plane(fd, plane_id)` — blob fetching belongs to
//!   `drmkit-core`'s property layer, which hands the bytes to
//!   [`FormatTable::from_blob`]. The C++ had already split the parser out for
//!   testability; this makes the split structural.
//! - `ScanoutBuffer` — GBM allocation and `AddFB2WithModifiers`, which land in
//!   `drmkit-gbm` (phase 4).
//!
//! # Example
//!
//! ```
//! use drmkit_fmt::{
//!     BandwidthClass, FormatTable, Modifier, classify, fourcc, layer_fits_plane,
//! };
//!
//! let plane = FormatTable::from_pairs([
//!     (fourcc::XRGB8888, Modifier::LINEAR),
//!     (fourcc::NV12, Modifier::LINEAR),
//! ]);
//!
//! assert!(layer_fits_plane(&plane, fourcc::XRGB8888, Modifier::LINEAR));
//! assert!(!layer_fits_plane(&plane, fourcc::ARGB8888, Modifier::LINEAR));
//!
//! // The INVALID sentinel is interchangeable with LINEAR.
//! assert!(layer_fits_plane(&plane, fourcc::NV12, Modifier::INVALID));
//!
//! assert_eq!(classify(Modifier::LINEAR), BandwidthClass::Linear);
//! ```

extern crate alloc;

pub mod fourcc;

mod cost;
mod describe;
mod family;
mod modifier;
mod probe;
mod table;

pub use cost::{DEFAULT_COMPRESSED_RATIO, scanout_cost_bytes};
pub use describe::describe;
pub use family::{ModifierFamily, sand_col_height, sand128_col_height};
pub use fourcc::{format_bpp, format_name};
pub use modifier::{
    ARM_16X16_BLOCK_U_INTERLEAVED, BandwidthClass, Modifier, Rotation, afbc_block, afbc_flag,
    arm_type, broadcom, classify, mod_code, rotation_compatible, vendor,
};
pub use probe::{ModifierProbeCache, Verdict};
pub use table::{FormatMod, FormatTable, layer_fits_plane};

#[cfg(test)]
mod tests;
