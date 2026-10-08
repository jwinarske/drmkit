// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! Placement where the planes have no `zpos`: the plane id is the stacking
//! order.
//!
//! Port of `place_in_plane_order` and `plane_order_consistent` from
//! `src/planes/allocator.cpp` (drm-cxx `03bc1d7`, drm-cxx#239, drm-cxx#240).
//!
//! When no non-cursor plane on a CRTC exposes `zpos` (vkms, plenty of `SoC`
//! display controllers) the kernel stacks planes by id, and no written value
//! reorders them. The kernel accepts an assignment in any order, so a
//! `TEST_ONLY` never catches an inverted stack; the order has to be kept by
//! construction. Layers in zpos order map onto planes in id order, and the
//! layers left over form one contiguous zpos run whose canvas plane sits
//! between that run's neighbors.

use std::ops::Range;

use crate::registry::{PlaneRegistry, PlaneType};

/// Whether the planes on `crtc_index` stack by plane id: no non-cursor plane
/// exposes `zpos`.
#[must_use]
pub fn stacks_by_plane_id(registry: &PlaneRegistry, crtc_index: u32) -> bool {
    let mut planes = registry.for_crtc(crtc_index).peekable();
    planes.peek().is_some()
        && planes.all(|plane| plane.plane_type == PlaneType::Cursor || plane.zpos_min.is_none())
}

/// One way to split zpos-ordered layers: `run` goes to the canvas, the rest
/// to planes in id order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Choice {
    /// The composited layers, a contiguous range of the zpos order.
    pub run: Range<usize>,
    /// The plane index carrying the canvas, when the run is not empty and
    /// there is a canvas.
    pub canvas: Option<usize>,
}

/// The earliest-first and latest-first greedy fits of `n` zpos-ordered layers
/// onto `m` id-ordered planes.
///
/// Greedy is optimal for each: placing layers `[0, a)` as low as they go
/// leaves the most room above, and placing `[b, n)` as high as they go leaves
/// the most room below.
#[derive(Debug)]
pub(crate) struct PlaneOrder {
    /// `pre[a]`: first plane index free above layers `[0, a)` placed
    /// earliest-first; `None` when they do not fit.
    pre: Vec<Option<usize>>,
    pre_plane: Vec<Option<usize>>,
    /// `suf[b]`: lowest plane index used by layers `[b, n)` placed
    /// latest-first; `None` when they do not fit.
    suf: Vec<Option<usize>>,
    suf_plane: Vec<Option<usize>>,
}

impl PlaneOrder {
    /// Fit `n` layers onto `m` planes; `fits(layer, plane)` is the static
    /// check.
    pub(crate) fn new(n: usize, m: usize, mut fits: impl FnMut(usize, usize) -> bool) -> Self {
        let mut pre = vec![None; n + 1];
        let mut pre_plane = vec![None; n];
        pre[0] = Some(0);
        for i in 0..n {
            let Some(from) = pre[i] else { break };
            if let Some(j) = (from..m).find(|&j| fits(i, j)) {
                pre_plane[i] = Some(j);
                pre[i + 1] = Some(j + 1);
            }
        }
        let mut suf = vec![None; n + 1];
        let mut suf_plane = vec![None; n];
        suf[n] = Some(m);
        for i in (0..n).rev() {
            let Some(below) = suf[i + 1] else { break };
            if let Some(j) = (0..below).rev().find(|&j| fits(i, j)) {
                suf_plane[i] = Some(j);
                suf[i] = Some(j);
            }
        }
        Self {
            pre,
            pre_plane,
            suf,
            suf_plane,
        }
    }

    /// The best split with at most `max_placed` layers on planes: most placed,
    /// then the lowest total `cost` composited.
    ///
    /// `forced` is the range every forced-composited layer lies in, which the
    /// run must cover. `hosts_canvas` is `None` without a canvas: the run is
    /// then dropped rather than composited, and needs no plane of its own.
    pub(crate) fn choose(
        &self,
        max_placed: usize,
        forced: Option<&Range<usize>>,
        hosts_canvas: Option<&dyn Fn(usize) -> bool>,
        cost: impl Fn(usize) -> i64,
    ) -> Option<Choice> {
        let n = self.pre_plane.len();
        let mut best: Option<(Choice, usize, i64)> = None;
        for a in 0..=n {
            let Some(low) = self.pre[a] else { break };
            for b in a..=n {
                let Some(high) = self.suf[b] else { continue };
                if forced.is_some_and(|forced| a > forced.start || b < forced.end) {
                    continue;
                }
                let placed = n - (b - a);
                if placed > max_placed {
                    continue;
                }
                let canvas = match hosts_canvas {
                    Some(hosts) if a < b => {
                        // Topmost free canvas host between the two placed
                        // halves.
                        let Some(j) = (low..high).rev().find(|&j| hosts(j)) else {
                            continue;
                        };
                        Some(j)
                    }
                    _ if low > high => continue,
                    _ => None,
                };
                let total: i64 = (a..b).map(&cost).sum();
                if best
                    .as_ref()
                    .is_none_or(|(_, p, c)| placed > *p || (placed == *p && total < *c))
                {
                    best = Some((Choice { run: a..b, canvas }, placed, total));
                }
            }
        }
        best.map(|(choice, _, _)| choice)
    }

    /// The plane index layer `i` takes under `choice`; `None` inside the run.
    pub(crate) fn plane_of(&self, i: usize, choice: &Choice) -> Option<usize> {
        if i < choice.run.start {
            self.pre_plane[i]
        } else if i >= choice.run.end {
            self.suf_plane[i]
        } else {
            None
        }
    }
}

/// One layer's place in a plane-id-ordered stack.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Position {
    /// The zpos the layer asks for.
    pub zpos: u64,
    /// The plane it stacks at: its own, or the canvas's. `None` when it is
    /// composited with no canvas plane to stack it at.
    pub plane: Option<u32>,
    /// Whether it rides the canvas.
    pub on_canvas: bool,
}

/// Whether the positions stack in zpos order (warm start).
///
/// Strictly lower zpos must sit on a strictly lower plane id, unless both
/// layers ride the canvas, which blends them in order itself.
pub(crate) fn plane_order_consistent(positions: &[Position]) -> bool {
    positions.iter().all(|x| {
        positions.iter().filter(|y| x.zpos < y.zpos).all(|y| {
            match (x.plane, y.plane) {
                (Some(px), Some(py)) => px < py || (px == py && x.on_canvas && y.on_canvas),
                // Composited with no canvas plane to stack it at.
                _ => false,
            }
        })
    })
}
