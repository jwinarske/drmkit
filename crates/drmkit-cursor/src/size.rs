// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! How big the cursor buffer is.
//!
//! Two decisions. What size to ask for, which depends on the path and what the
//! driver advertises; and what size to settle on, which depends on what the
//! kernel will actually accept. The second needs a device, so it takes the
//! probe as a parameter and everything here stays testable.

use crate::plane::PlanePath;

/// What every driver hard-codes for `drmModeSetCursor`, and the floor
/// everywhere else.
///
/// The legacy call has no capability query, so there is nothing to ask.
const LEGACY_SIZE: u32 = 64;

/// Sizes worth probing, largest first.
///
/// Not a range: these are the sizes real hardware implements. i915 does 64,
/// amdgpu 256, and asking for anything between them gets rounded or refused
/// depending on the driver, so there is no point walking one pixel at a time.
const LADDER: [u32; 3] = [256, 128, 64];

/// The size to ask for, before finding out what is accepted.
///
/// A caller who names a size gets it considered as-is. Otherwise the atomic
/// paths take what the driver advertises through `DRM_CAP_CURSOR_WIDTH`, so a
/// plane that wants 256 gets 256; legacy takes 64, because that is what the
/// call means.
#[must_use]
pub fn preferred_size(requested: u32, path: PlanePath, driver_cap: Option<u32>) -> u32 {
    if requested != 0 {
        return requested;
    }
    match path {
        PlanePath::Legacy => LEGACY_SIZE,
        PlanePath::AtomicCursor | PlanePath::AtomicOverlay => {
            driver_cap.filter(|cap| *cap != 0).unwrap_or(LEGACY_SIZE)
        }
    }
}

/// Settle on a buffer size, asking `accepts` what the kernel will take.
///
/// Walks the ladder downward from `preferred`, and never above it: a caller
/// who asked for a small cursor does not get a large one because the driver
/// could manage it.
///
/// When nothing is accepted the answer is `preferred`, floored at 64 — there
/// has to be a buffer either way, and the first real commit will surface the
/// kernel's objection if there is one. That case is usually not about size at
/// all: with no mode set yet, every `TEST_ONLY` fails for unrelated reasons.
pub fn probe_size(preferred: u32, mut accepts: impl FnMut(u32) -> bool) -> u32 {
    for candidate in LADDER {
        if candidate <= preferred && accepts(candidate) {
            return candidate;
        }
    }
    preferred.max(LEGACY_SIZE)
}
