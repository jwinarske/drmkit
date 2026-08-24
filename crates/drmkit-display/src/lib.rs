// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! Display characterisation and colour handling.
//!
//! What a connected display can do, and what has to happen to pixels before
//! they reach it.

pub mod curves;
mod tone_mapper;

#[cfg(test)]
mod tests;

pub use tone_mapper::{Direction, LUT_SIZE, ToneMapCurve, ToneMapper};
