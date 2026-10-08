// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! The stacking rules for `zpos`: which planes can be written, where a layer
//! actually stacks, and the dense numbering the allocator writes.
//!
//! Port of `src/planes/zpos_order.hpp` (drm-cxx `8bf20e6`, `1384526`).
//!
//! A mutable-zpos plane takes a written value, so it stacks where its layer
//! asked relative to the others. A fixed-slot plane (`zpos_min == zpos_max`:
//! i.MX LCDIF and vc4 primaries at 0, amdgpu's at 2) is never written and
//! stacks at its slot whatever the layer asked for. Requiring the layer's zpos
//! to equal the slot is stricter than needed -- on a single-plane controller
//! it shuts every layer above 0 out of the only plane. What matters is relative
//! order, and the kernel accepts an inverted stack without complaint, so a
//! `TEST_ONLY` never catches one: [`stacking_consistent`] has to.

use crate::registry::{PlaneCapabilities, PlaneRegistry};

/// Whether the plane's zpos is a single fixed slot that cannot be written.
#[must_use]
pub fn zpos_fixed(plane: &PlaneCapabilities) -> bool {
    matches!((plane.zpos_min, plane.zpos_max), (Some(min), Some(max)) if min == max)
}

/// Where a layer requesting `layer_zpos` stacks on `plane`: the fixed slot,
/// else the layer's own written zpos. `None` when neither is known.
#[must_use]
pub fn effective_zpos(plane: &PlaneCapabilities, layer_zpos: Option<u64>) -> Option<u64> {
    if zpos_fixed(plane) {
        plane.zpos_min
    } else {
        layer_zpos
    }
}

/// Whether layer A (zpos `za`) on `pa` and layer B (zpos `zb`) on `pb` stack in
/// the order their zpos values request.
///
/// Layers without a zpos, or with equal ones, request no order. Different
/// requests landing on the same effective slot are ambiguous -- the kernel
/// breaks the tie by plane id -- and rejected. On mutable planes the effective
/// zpos is the requested one, so this only ever rules on fixed-slot planes.
#[must_use]
pub fn stacking_consistent(
    pa: &PlaneCapabilities,
    za: Option<u64>,
    pb: &PlaneCapabilities,
    zb: Option<u64>,
) -> bool {
    let (Some(za), Some(zb)) = (za, zb) else {
        return true;
    };
    if za == zb {
        return true;
    }
    let (Some(ea), Some(eb)) = (effective_zpos(pa, Some(za)), effective_zpos(pb, Some(zb))) else {
        // A plane without a zpos property: nothing to check against.
        return true;
    };
    ea != eb && (za < zb) == (ea < eb)
}

/// One armed plane in a frame's stack.
#[derive(Debug, Clone, Copy)]
pub struct StackEntry<'a> {
    /// The plane.
    pub plane: &'a PlaneCapabilities,
    /// The zpos its layer requests.
    pub requested: Option<u64>,
    /// The zpos to write, filled by [`stack_zpos`].
    pub written: Option<u64>,
}

impl<'a> StackEntry<'a> {
    /// An entry with nothing written yet.
    #[must_use]
    pub const fn new(plane: &'a PlaneCapabilities, requested: Option<u64>) -> Self {
        Self {
            plane,
            requested,
            written: None,
        }
    }
}

/// Number the armed planes' zpos densely, in requested order.
///
/// A mutable plane gets `max(previous + 1, zpos_min)`; a fixed-slot plane keeps
/// its slot. Only the planes actually armed take values, so layers that end up
/// composited do not use up the range, and the stack stays clear of its top,
/// which some controllers advertise but refuse (the SA8155P advertises
/// `[0, 10]` and rejects 9 and 10 even on a lone plane). Ties are broken by
/// plane id. Order is preserved, so [`stacking_consistent`]'s rulings stand.
///
/// Greedy-lowest is optimal, so when it overflows a plane's `zpos_max`, or meets
/// a fixed slot at or below the previous value, no dense numbering exists:
/// every `written` is left equal to `requested` and this returns `false`.
/// Entries with no requested zpos or no zpos range are not numbered.
pub fn stack_zpos(entries: &mut [StackEntry<'_>]) -> bool {
    struct Rankable {
        index: usize,
        requested: u64,
        plane_id: u32,
        min: u64,
        max: u64,
        fixed: bool,
    }

    let mut order = Vec::with_capacity(entries.len());
    for (index, entry) in entries.iter_mut().enumerate() {
        entry.written = entry.requested;
        if let (Some(requested), Some(min), Some(max)) =
            (entry.requested, entry.plane.zpos_min, entry.plane.zpos_max)
        {
            order.push(Rankable {
                index,
                requested,
                plane_id: entry.plane.id,
                min,
                max,
                fixed: min == max,
            });
        }
    }
    order.sort_by_key(|rank| (rank.requested, rank.plane_id));

    let mut values = Vec::with_capacity(order.len());
    let mut previous: Option<u64> = None;
    for rank in &order {
        let value = match previous {
            Some(previous) if rank.fixed && rank.min <= previous => return false,
            Some(previous) if !rank.fixed => previous.saturating_add(1).max(rank.min),
            _ => rank.min,
        };
        if value > rank.max {
            return false;
        }
        values.push(value);
        previous = Some(value);
    }
    for (rank, value) in order.iter().zip(values) {
        entries[rank.index].written = Some(value);
    }
    true
}

/// The zpos to write on each armed plane, as `(plane_id, value)`.
///
/// `armed` is every plane the frame programs -- allocated, pinned and the
/// composition canvas alike -- with the zpos its layer requests. One function
/// for the frame that is committed and the assignments that are tested, so the
/// kernel is never asked about one stack and handed another. Planes the
/// numbering does not rank are left out; when no dense numbering fits, every
/// rankable plane gets its requested value and the `TEST_ONLY` decides.
#[must_use]
pub fn stacked_zpos(registry: &PlaneRegistry, armed: &[(u32, Option<u64>)]) -> Vec<(u32, u64)> {
    let mut entries = Vec::with_capacity(armed.len());
    let mut ids = Vec::with_capacity(armed.len());
    for &(plane_id, requested) in armed {
        if let Some(plane) = registry.by_id(plane_id) {
            entries.push(StackEntry::new(plane, requested));
            ids.push(plane_id);
        }
    }
    // On overflow every `written` is the requested value, which is what goes
    // out; the return value only says the numbering could not be dense.
    let _ = stack_zpos(&mut entries);
    ids.into_iter()
        .zip(entries)
        .filter_map(|(id, entry)| entry.written.map(|value| (id, value)))
        .collect()
}
