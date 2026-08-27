// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! Getting a drawn decoration onto the screen.
//!
//! Three ways, in descending order of what the hardware will do for you:
//!
//! - **Plane** — each decoration on its own overlay. A hover costs a small
//!   blit and one property write; nothing is recomposited.
//! - **Composite** — blend them onto one canvas plane. Costs a canvas-sized
//!   blend per damaged region, and needs only one plane.
//! - **Framebuffer** — no KMS plane at all. For an fbdev fallback, where the
//!   decoration is written into the same buffer as everything else.

/// Which way a decoration reaches the screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tier {
    /// One overlay plane per decoration.
    Plane,
    /// All of them blended onto one canvas plane.
    Composite,
    /// Into a framebuffer, with no plane involved.
    Fb,
}

/// Pick a tier from what the hardware can offer.
///
/// `None` means neither a plane nor a canvas is available, and the caller
/// should drop to its own framebuffer path — this function does not choose
/// [`Tier::Fb`], because that tier is what a caller falls back *to* when KMS
/// has nothing, not something to be selected among KMS options.
///
/// Zero desired decorations is never [`Tier::Plane`]: reserving no planes
/// trivially "succeeds", and returning Plane on that basis would have a caller
/// build a plane presenter with no slots.
#[must_use]
pub const fn choose_presenter_tier(
    reservable: usize,
    desired: usize,
    has_canvas_plane: bool,
) -> Option<Tier> {
    if desired > 0 && reservable >= desired {
        // Every window gets its own overlay, which is the arrangement worth
        // having: a hover repaints one small buffer instead of a canvas.
        return Some(Tier::Plane);
    }
    if has_canvas_plane {
        // Plane-starved but not planeless: composite onto the primary.
        return Some(Tier::Composite);
    }
    None
}

/// One property write an `apply` produces.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PropertyWrite {
    /// The object to write on.
    pub object_id: u32,
    /// Which property.
    pub property_id: u32,
    /// The value.
    pub value: u64,
}

/// A decoration surface and where it goes.
#[derive(Debug, Clone, Copy, Default)]
pub struct SurfaceRef {
    /// The framebuffer to scan out, or zero for nothing.
    pub fb_id: u32,
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// Destination x.
    pub x: i32,
    /// Destination y.
    pub y: i32,
    /// The surface's content generation, for damage tracking.
    pub generation: u64,
}

impl SurfaceRef {
    /// Whether this reference names anything to draw.
    ///
    /// A zero framebuffer or a zero dimension is *nothing*, not an error: a
    /// caller with fewer windows than slots passes these, and a caller mid-
    /// resize passes them briefly.
    #[must_use]
    pub const fn is_armed(self) -> bool {
        self.fb_id != 0 && self.width != 0 && self.height != 0
    }
}
