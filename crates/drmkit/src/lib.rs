// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! Umbrella crate for **drmkit**, a layered Linux DRM/KMS display framework —
//! modesetting, plane allocation, scene composition, and scanout.
//!
//! drmkit is a behavioral-parity Rust port of
//! [drm-cxx](https://github.com/jwinarske/drm-cxx).
//!
//! # Status
//!
//! **This crate is a placeholder and exports nothing yet.** The port is at
//! Phase 0 of the plan: the workspace, pinned toolchain, CI, and drift linters
//! are in place, along with two leaf crates (`drmkit-log`, `drmkit-time`).
//! Nothing here is usable as a display library.
//!
//! Do not depend on this crate yet. Follow
//! <https://github.com/jwinarske/drmkit> for progress; the porting plan,
//! per-phase scope, and the invariant contracts the port must hold live there.
//!
//! Once the member crates publish, this crate becomes their feature-gated
//! re-export surface, so a consumer can take `drmkit` with the features it
//! needs instead of naming a dozen crates.

// The crate exports nothing yet by design; the lint would otherwise fire on an
// empty root.
#![allow(clippy::empty_docs)]

/// The upstream drm-cxx revision this port targets.
///
/// Kept as a constant so a consumer — and the port's own tooling — can tell at
/// a glance which C++ baseline a given drmkit build claims parity with.
pub const UPSTREAM_BASELINE: &str = "4a0b64a";

#[cfg(test)]
mod tests {
    #[test]
    fn baseline_is_recorded() {
        assert_eq!(super::UPSTREAM_BASELINE, "4a0b64a");
    }
}
