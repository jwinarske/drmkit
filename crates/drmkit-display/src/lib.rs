// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! Display characterisation and colour handling.
//!
//! What a connected display can do, and what has to happen to pixels before
//! they reach it.

pub mod curves;
#[cfg(feature = "edid")]
pub mod edid;
mod profile;
mod tone_mapper;

#[cfg(test)]
mod tests;

pub use profile::{
    DriverProfile, PanelSelfRefresh, PrimeCaps, connector_type_self_refreshes, decode_prime_caps,
};
pub use tone_mapper::{Direction, LUT_SIZE, ToneMapCurve, ToneMapper};
