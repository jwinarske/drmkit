// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! Claiming overlay planes for decorations, before the scene wants them.
//!
//! Decorations want their own planes. Compositing them into the window's
//! buffer means the client redraws its whole surface every time the pointer
//! crosses a button; a plane means the decoration is its own scanout layer and
//! a hover costs a small blit.
//!
//! But the scene's allocator will take every free plane it can place a layer
//! on, so a decoration that waits until commit time gets nothing. Reserving
//! first is what makes it work — the reservation is then handed to
//! `Allocator::set_reserved_planes`, which is the other half of the same
//! arrangement.

use std::collections::{HashMap, HashSet};

use drmkit_planes::{PlaneRegistry, PlaneType};

/// Why a reservation could not be made.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum ReserveError {
    /// Not enough planes on this CRTC match what was asked for.
    ///
    /// Distinct from a hard failure: a caller can ask again for fewer, or
    /// draw its decorations into the window instead. It is a shortfall, not a
    /// broken device.
    #[error("wanted {wanted} overlay planes on CRTC {crtc_index}, {found} are available")]
    Shortfall {
        /// Which CRTC.
        crtc_index: u32,
        /// How many were asked for.
        wanted: usize,
        /// How many matched.
        found: usize,
    },
}

/// Overlay planes claimed for decorations, per CRTC.
#[derive(Debug, Default)]
pub struct OverlayReservation {
    /// What each CRTC holds, in zpos order.
    by_crtc: HashMap<u32, Vec<u32>>,
    /// Every claimed plane, across CRTCs.
    ///
    /// A separate set because a plane can be reachable from more than one
    /// CRTC: on such hardware the first CRTC to claim it takes it, and the
    /// second must not be offered it again. The per-CRTC map alone cannot
    /// answer that without scanning every entry.
    all: HashSet<u32>,
}

impl OverlayReservation {
    /// An empty reservation.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Claim `count` overlay planes on `crtc_index` that can scan out
    /// `fourcc`.
    ///
    /// Returns them in ascending zpos order, which is the order a caller
    /// stacks decorations in: the lowest is the one behind.
    ///
    /// `min_zpos` excludes planes that cannot sit above the window. A
    /// decoration below the content it decorates is invisible, so a caller
    /// that knows the window's stacking position passes it here rather than
    /// discovering the problem on screen.
    ///
    /// **Re-reserving a CRTC releases what it held first.** A caller
    /// responding to a mode change asks again with a new count, and the old
    /// claim would otherwise keep planes it no longer wants out of everyone
    /// else's reach.
    ///
    /// # Errors
    ///
    /// [`ReserveError::Shortfall`] when fewer than `count` planes match. The
    /// reservation is left as it was — released, since the release happens
    /// first — rather than partially filled: a caller that asked for three and
    /// got two would have to work out which of its decorations to drop.
    pub fn reserve(
        &mut self,
        registry: &PlaneRegistry,
        crtc_index: u32,
        fourcc: u32,
        count: usize,
        min_zpos: u64,
    ) -> Result<Vec<u32>, ReserveError> {
        if count == 0 {
            // Still records the CRTC, so `reserved_for` answers with an empty
            // slice rather than nothing -- a caller that asked for none has a
            // reservation, it is just empty.
            self.by_crtc.entry(crtc_index).or_default();
            return Ok(Vec::new());
        }

        self.release(crtc_index);

        let mut candidates: Vec<(u64, u32)> = registry
            .for_crtc(crtc_index)
            .filter(|plane| plane.plane_type == PlaneType::Overlay)
            .filter(|plane| plane.supports_format(fourcc))
            .filter(|plane| !self.all.contains(&plane.id))
            .filter_map(|plane| match plane.zpos_min {
                // A plane that advertises no zpos cannot be shown to sit above
                // anything, so it qualifies only where nothing was required.
                None => (min_zpos == 0).then_some((0, plane.id)),
                Some(zpos) => (zpos >= min_zpos).then_some((zpos, plane.id)),
            })
            .collect();

        if candidates.len() < count {
            return Err(ReserveError::Shortfall {
                crtc_index,
                wanted: count,
                found: candidates.len(),
            });
        }

        // By zpos, then by id: two planes at the same zpos would otherwise
        // come back in whatever order the registry happened to hold them, and
        // a caller stacking decorations by index would get a different result
        // run to run.
        candidates.sort_unstable();

        let claimed: Vec<u32> = candidates
            .into_iter()
            .take(count)
            .map(|(_, id)| id)
            .collect();
        self.all.extend(claimed.iter().copied());
        self.by_crtc.insert(crtc_index, claimed.clone());
        Ok(claimed)
    }

    /// Give back everything this CRTC holds.
    ///
    /// A CRTC that holds nothing, or was never reserved for, is a no-op —
    /// both are what a caller tearing down an output does, sometimes twice.
    pub fn release(&mut self, crtc_index: u32) {
        if let Some(planes) = self.by_crtc.remove(&crtc_index) {
            for plane in planes {
                self.all.remove(&plane);
            }
        }
    }

    /// What this CRTC holds, in zpos order.
    #[must_use]
    pub fn reserved_for(&self, crtc_index: u32) -> &[u32] {
        self.by_crtc.get(&crtc_index).map_or(&[], Vec::as_slice)
    }

    /// Every claimed plane, across every CRTC.
    ///
    /// What goes to `Allocator::set_reserved_planes`, which is why it is one
    /// list rather than a map: the allocator is told which planes it may not
    /// have, and it does not care who has them.
    #[must_use]
    pub fn all_reserved(&self) -> Vec<u32> {
        let mut all: Vec<u32> = self.all.iter().copied().collect();
        // Sorted so the value is stable: it reaches the allocator, and a set
        // whose order changed run to run would make a plane assignment look
        // unstable for no reason.
        all.sort_unstable();
        all
    }
}
