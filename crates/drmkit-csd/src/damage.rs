// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! Working out how little of a canvas has to be redrawn.

/// One decoration's footprint on the canvas, as of some frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DamageSlot {
    /// Whether anything is drawn here at all.
    pub armed: bool,
    /// Left edge.
    pub x: i32,
    /// Top edge.
    pub y: i32,
    /// Width.
    pub w: u32,
    /// Height.
    pub h: u32,
    /// The surface's content generation.
    ///
    /// What separates *moved* from *redrawn*: a decoration at the same place
    /// whose pixels changed has the same rectangle and a different generation,
    /// and without this it would look unchanged.
    pub generation: u64,
}

/// A rectangle of the canvas that has to be repainted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DamageRect {
    /// Left edge.
    pub x: i32,
    /// Top edge.
    pub y: i32,
    /// Width.
    pub w: u32,
    /// Height.
    pub h: u32,
}

impl DamageRect {
    /// Whether this rectangle covers nothing.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.w == 0 || self.h == 0
    }
}

/// Accumulates a bounding box.
#[derive(Default)]
struct Bounds {
    valid: bool,
    x0: i32,
    y0: i32,
    x1: i32,
    y1: i32,
}

impl Bounds {
    fn add(&mut self, x: i32, y: i32, w: u32, h: u32) {
        if w == 0 || h == 0 {
            return;
        }
        let x1 = x.saturating_add(i32::try_from(w).unwrap_or(i32::MAX));
        let y1 = y.saturating_add(i32::try_from(h).unwrap_or(i32::MAX));
        if self.valid {
            self.x0 = self.x0.min(x);
            self.y0 = self.y0.min(y);
            self.x1 = self.x1.max(x1);
            self.y1 = self.y1.max(y1);
        } else {
            *self = Self {
                valid: true,
                x0: x,
                y0: y,
                x1,
                y1,
            };
        }
    }
}

/// What changed between two frames' worth of slots.
///
/// One rectangle rather than a list: a canvas repaint is a loop over rows, and
/// the union of two scattered regions is cheaper to redraw whole than to
/// iterate separately.
///
/// **A different number of slots damages everything.** The slots are compared
/// pairwise by index, so a count change means the pairing itself is
/// meaningless — comparing slot 2 against what used to be slot 3 would report
/// changes in the wrong places and miss real ones.
#[must_use]
pub fn compute_damage(
    prev: &[DamageSlot],
    cur: &[DamageSlot],
    canvas_w: u32,
    canvas_h: u32,
) -> DamageRect {
    let whole = DamageRect {
        x: 0,
        y: 0,
        w: canvas_w,
        h: canvas_h,
    };
    // Empty `prev` is the first frame: nothing is on the canvas yet, so
    // everything has to be painted.
    if prev.len() != cur.len() || prev.is_empty() {
        return whole;
    }

    let mut bounds = Bounds::default();
    for (before, after) in prev.iter().zip(cur) {
        let changed = before.armed != after.armed
            || (after.armed
                && (before.x != after.x
                    || before.y != after.y
                    || before.w != after.w
                    || before.h != after.h
                    || before.generation != after.generation));
        if !changed {
            continue;
        }
        // Both footprints, not just the new one: the old has to be vacated or
        // a decoration that moved leaves a copy of itself behind.
        if before.armed {
            bounds.add(before.x, before.y, before.w, before.h);
        }
        if after.armed {
            bounds.add(after.x, after.y, after.w, after.h);
        }
    }

    if !bounds.valid {
        return DamageRect::default();
    }

    let x0 = bounds.x0.max(0);
    let y0 = bounds.y0.max(0);
    let x1 = bounds.x1.min(i32::try_from(canvas_w).unwrap_or(i32::MAX));
    let y1 = bounds.y1.min(i32::try_from(canvas_h).unwrap_or(i32::MAX));
    if x1 <= x0 || y1 <= y0 {
        return DamageRect::default();
    }

    DamageRect {
        x: x0,
        y: y0,
        w: (x1 - x0).unsigned_abs(),
        h: (y1 - y0).unsigned_abs(),
    }
}

/// The smallest rectangle covering both.
#[must_use]
pub fn union_rect(a: DamageRect, b: DamageRect) -> DamageRect {
    if a.is_empty() {
        return b;
    }
    if b.is_empty() {
        return a;
    }
    let mut bounds = Bounds::default();
    bounds.add(a.x, a.y, a.w, a.h);
    bounds.add(b.x, b.y, b.w, b.h);
    DamageRect {
        x: bounds.x0,
        y: bounds.y0,
        w: (bounds.x1 - bounds.x0).unsigned_abs(),
        h: (bounds.y1 - bounds.y0).unsigned_abs(),
    }
}

/// The overlap between a rectangle and a damage region.
#[must_use]
pub fn intersect_rect(x: i32, y: i32, w: u32, h: u32, region: DamageRect) -> DamageRect {
    let ax1 = x.saturating_add(i32::try_from(w).unwrap_or(i32::MAX));
    let ay1 = y.saturating_add(i32::try_from(h).unwrap_or(i32::MAX));
    let bx1 = region
        .x
        .saturating_add(i32::try_from(region.w).unwrap_or(i32::MAX));
    let by1 = region
        .y
        .saturating_add(i32::try_from(region.h).unwrap_or(i32::MAX));

    let x0 = x.max(region.x);
    let y0 = y.max(region.y);
    let x1 = ax1.min(bx1);
    let y1 = ay1.min(by1);
    if x1 <= x0 || y1 <= y0 {
        return DamageRect::default();
    }
    DamageRect {
        x: x0,
        y: y0,
        w: (x1 - x0).unsigned_abs(),
        h: (y1 - y0).unsigned_abs(),
    }
}
