// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! Composing a cursor frame into a plane buffer.
//!
//! A cursor plane is a fixed size the driver chooses -- 64x64 on i915, 256x256
//! on amdgpu -- and a themed cursor is whatever size the theme ships. So a
//! frame has to be rotated if the display is, shrunk if it is too big,
//! centred, and the hotspot has to move with it. All of that is arithmetic,
//! and all of it is here, away from anything that needs a device.
//!
//! The hotspot comes back from the same call that writes the pixels.
//! The reference computes it separately, from state the blit left behind, so
//! the two can disagree if they are called out of order; returning them
//! together makes that unrepresentable.

use crate::cursor::Frame;

/// Half a unit in the 16.16 fixed point the shrink ratio uses.
const HALF_16_16: u64 = 1 << 15;

/// How the sprite is turned before it reaches the plane.
///
/// Reflections are not here. No driver has asked for them, and a flag nobody
/// sets is a branch nobody tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Rotation {
    /// Upright.
    #[default]
    None,
    /// A quarter turn clockwise.
    Quarter,
    /// Half a turn.
    Half,
    /// Three quarters clockwise.
    ThreeQuarter,
}

impl Rotation {
    /// The `DRM_MODE_ROTATE_*` bit for this rotation.
    ///
    /// Restated rather than pulled from a binding: these are UAPI constants,
    /// and the whole point of asking is to compare against a plane's supported
    /// mask before deciding whether the hardware can do the turn.
    #[must_use]
    pub const fn drm_mask(self) -> u64 {
        match self {
            Self::None => 1 << 0,
            Self::Quarter => 1 << 1,
            Self::Half => 1 << 2,
            Self::ThreeQuarter => 1 << 3,
        }
    }

    /// Whether this rotation exchanges width and height.
    #[must_use]
    pub const fn swaps_axes(self) -> bool {
        matches!(self, Self::Quarter | Self::ThreeQuarter)
    }

    /// The size of `(width, height)` after turning.
    const fn rotated_size(self, width: u32, height: u32) -> (u32, u32) {
        if self.swaps_axes() {
            (height, width)
        } else {
            (width, height)
        }
    }
}

/// Where the sprite ended up in the buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Placement {
    /// Size of the sprite as written, after rotation and any shrink.
    pub width: u32,
    /// As [`Placement::width`].
    pub height: u32,
    /// Top-left corner of the sprite within the buffer.
    pub x: u32,
    /// As [`Placement::x`].
    pub y: u32,
    /// The hotspot, in buffer coordinates.
    ///
    /// This is what goes in `HOTSPOT_X`, and what a position has to be
    /// adjusted by: the plane is placed so the hotspot lands on the pointer,
    /// not so its corner does.
    pub hotspot_x: u32,
    /// As [`Placement::hotspot_x`].
    pub hotspot_y: u32,
}

/// One pixel of the rotated view of `frame`.
///
/// `(rx, ry)` are coordinates in the rotated image. The formulas are inverse
/// rotations -- they map where a pixel is going back to where it came from --
/// which is what lets rotation and scaling compose without an intermediate
/// buffer.
fn sample_rotated(frame: &Frame, rotation: Rotation, rx: u32, ry: u32) -> u32 {
    let (sx, sy) = match rotation {
        Rotation::None => (rx, ry),
        Rotation::Quarter => (ry, frame.height - 1 - rx),
        Rotation::Half => (frame.width - 1 - rx, frame.height - 1 - ry),
        Rotation::ThreeQuarter => (frame.width - 1 - ry, rx),
    };
    frame.pixels[(sy * frame.width + sx) as usize]
}

/// The region of a plane buffer a sprite is being written into.
struct Target<'a> {
    pixels: &'a mut [u32],
    /// Index of the region's top-left pixel within `pixels`.
    offset: usize,
    stride_px: usize,
    width: u32,
    height: u32,
}

/// Area-weighted box downscale of the rotated frame into `target`.
///
/// Pixels are premultiplied, so averaging the components straight is correct
/// -- there is no un-premultiply detour, and doing one would be wrong.
///
/// Fixed point 24.8 throughout: each destination pixel's preimage is a box in
/// source space, and every source pixel it touches contributes the fraction of
/// itself that falls inside. Integer arithmetic only, so the result does not
/// depend on the floating-point mode of whoever is calling.
fn box_downscale(frame: &Frame, rotation: Rotation, rotated: (u32, u32), target: &mut Target<'_>) {
    let (rotated_w, rotated_h) = rotated;
    let fx = (u64::from(rotated_w) << 8) / u64::from(target.width);
    let fy = (u64::from(rotated_h) << 8) / u64::from(target.height);

    for dy in 0..target.height {
        let top = u64::from(dy) * fy;
        let bottom = top + fy;
        let first_row = top >> 8;
        let last_row = (bottom + 0xff) >> 8;

        for dx in 0..target.width {
            let left = u64::from(dx) * fx;
            let right = left + fx;
            let first_col = left >> 8;
            let last_col = (right + 0xff) >> 8;

            let mut acc = [0u64; 4];
            let mut total = 0u64;
            for sy in first_row..last_row {
                let covered_h = bottom.min((sy + 1) << 8) - top.max(sy << 8);
                for sx in first_col..last_col {
                    let covered_w = right.min((sx + 1) << 8) - left.max(sx << 8);
                    let weight = covered_w * covered_h;

                    // Clamped because the ceilings above can reach one past
                    // the last source pixel when the box ends exactly on a
                    // boundary; that column contributes zero width anyway.
                    let pixel = sample_rotated(
                        frame,
                        rotation,
                        u32::try_from(sx).unwrap_or(0).min(rotated_w - 1),
                        u32::try_from(sy).unwrap_or(0).min(rotated_h - 1),
                    );
                    for (channel, shift) in [24, 16, 8, 0].into_iter().enumerate() {
                        acc[channel] += weight * u64::from((pixel >> shift) & 0xff);
                    }
                    total += weight;
                }
            }

            if total == 0 {
                continue;
            }
            let half = total / 2;
            let mut out = 0u32;
            for (channel, shift) in [24, 16, 8, 0].into_iter().enumerate() {
                let value = u32::try_from((acc[channel] + half) / total).unwrap_or(0xff) & 0xff;
                out |= value << shift;
            }
            let at = target.offset + dy as usize * target.stride_px + dx as usize;
            target.pixels[at] = out;
        }
    }
}

/// Compose `frame` into a cursor plane buffer, and say where it landed.
///
/// The buffer is wiped first. A frame smaller than the buffer must not leave
/// the previous frame showing around its edges, and a rotation can move the
/// sprite off its own former footprint entirely -- either would ghost on
/// screen.
///
/// A frame larger than the buffer on either axis is shrunk, both axes by the
/// same ratio so it stays proportional. That is not a rare path: a theme picks
/// the nearest size it ships, not the nearest size at or below what was asked
/// for, so a 64-pixel plane routinely gets a 72-pixel sprite.
///
/// # Panics
///
/// If `dst` is too small for `dst_h` rows of `stride_px`, which is a caller
/// error rather than anything a cursor file can cause.
pub fn compose(
    frame: &Frame,
    rotation: Rotation,
    dst: &mut [u32],
    dst_w: u32,
    dst_h: u32,
    stride_px: usize,
) -> Placement {
    assert!(
        dst.len() >= dst_h as usize * stride_px,
        "destination holds {} pixels, needs {}",
        dst.len(),
        dst_h as usize * stride_px
    );

    for y in 0..dst_h as usize {
        let row = y * stride_px;
        dst[row..row + dst_w as usize].fill(0);
    }

    let (rotated_w, rotated_h) = rotation.rotated_size(frame.width, frame.height);
    let shrink = rotated_w > dst_w || rotated_h > dst_h;

    let (blit_w, blit_h) = if shrink {
        // The more aggressive of the two ratios drives both axes, so a sprite
        // that overflows on one axis only still keeps its proportions.
        let rx = (u64::from(dst_w) << 16) / u64::from(rotated_w);
        let ry = (u64::from(dst_h) << 16) / u64::from(rotated_h);
        let ratio = rx.min(ry);
        // Rounded, not truncated: a 72 -> 64 shrink has to land on 64. Losing
        // the last row and column to truncation would leave a black border
        // for nothing.
        let scaled = |extent: u32| {
            let value =
                u32::try_from(((u64::from(extent) * ratio) + HALF_16_16) >> 16).unwrap_or(1);
            value.max(1)
        };
        (scaled(rotated_w).min(dst_w), scaled(rotated_h).min(dst_h))
    } else {
        (rotated_w.min(dst_w), rotated_h.min(dst_h))
    };

    let x_off = if dst_w > blit_w {
        (dst_w - blit_w) / 2
    } else {
        0
    };
    let y_off = if dst_h > blit_h {
        (dst_h - blit_h) / 2
    } else {
        0
    };
    let base = y_off as usize * stride_px + x_off as usize;

    if shrink {
        box_downscale(
            frame,
            rotation,
            (rotated_w, rotated_h),
            &mut Target {
                pixels: dst,
                offset: base,
                stride_px,
                width: blit_w,
                height: blit_h,
            },
        );
    } else {
        for y in 0..blit_h {
            for x in 0..blit_w {
                let at = base + y as usize * stride_px + x as usize;
                dst[at] = sample_rotated(frame, rotation, x, y);
            }
        }
    }

    let (hotspot_x, hotspot_y) = hotspot_in_buffer(frame, rotation, blit_w, blit_h, x_off, y_off);

    Placement {
        width: blit_w,
        height: blit_h,
        x: x_off,
        y: y_off,
        hotspot_x,
        hotspot_y,
    }
}

/// Where the hotspot ends up after the same rotation, shrink and centring the
/// pixels got.
///
/// Scaling truncates here where the pixels round. That is the reference's
/// behaviour and it is the conservative direction: the hotspot stays inside
/// the sprite rather than stepping one pixel past its edge.
fn hotspot_in_buffer(
    frame: &Frame,
    rotation: Rotation,
    blit_w: u32,
    blit_h: u32,
    x_off: u32,
    y_off: u32,
) -> (u32, u32) {
    let (mut hx, mut hy) = match rotation {
        Rotation::None => (frame.xhot, frame.yhot),
        Rotation::Quarter => (frame.height.saturating_sub(1 + frame.yhot), frame.xhot),
        Rotation::Half => (
            frame.width.saturating_sub(1 + frame.xhot),
            frame.height.saturating_sub(1 + frame.yhot),
        ),
        Rotation::ThreeQuarter => (frame.yhot, frame.width.saturating_sub(1 + frame.xhot)),
    };
    let (rotated_w, rotated_h) = rotation.rotated_size(frame.width, frame.height);

    if blit_w != 0 && blit_w != rotated_w && rotated_w != 0 {
        hx = u32::try_from(u64::from(hx) * u64::from(blit_w) / u64::from(rotated_w)).unwrap_or(0);
    }
    if blit_h != 0 && blit_h != rotated_h && rotated_h != 0 {
        hy = u32::try_from(u64::from(hy) * u64::from(blit_h) / u64::from(rotated_h)).unwrap_or(0);
    }

    (hx + x_off, hy + y_off)
}
