// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! Putting a cursor on a plane and keeping it there.
//!
//! This is where the pieces meet: [`select_plane`] picks the plane,
//! [`probe_size`] settles the buffer size, [`compose`] draws a frame into the
//! buffer that is not on screen, [`PingPong`] says which that is, and [`stage`]
//! writes the result onto the plane.
//!
//! The staging is deliberately separate from committing. A compositor wants
//! the cursor in the same atomic request as the rest of its frame -- a cursor
//! that lands a commit later than the window it belongs to is visibly wrong
//! when either is moving. [`Renderer::commit`] exists for callers with nothing
//! to coalesce with.

use std::time::Duration;

use drmkit_core::{AtomicCommitFlags, AtomicRequest, Device, ObjectType, PropertyStore};
use drmkit_dumb::{Buffer, Config, MapAccess};
use drmkit_planes::PlaneRegistry;

use crate::CursorError;
use crate::cursor::Cursor;
use crate::pingpong::PingPong;
use crate::plane::{PlanePath, SelectedPlane, select_plane};
use crate::size::{preferred_size, probe_size};
use crate::sprite::{self, Placement, Rotation};
use crate::stage::{PlaneState, stage};

/// How to set a renderer up.
#[derive(Debug, Clone, Copy)]
pub struct RendererConfig {
    /// The CRTC the cursor belongs to, and its index in the resource list.
    ///
    /// Both, because the id is what gets written to `CRTC_ID` and the index is
    /// what `possible_crtcs` is a bitmask over. Deriving one from the other
    /// needs the resource list, which the caller already has.
    pub crtc_id: u32,
    /// As [`RendererConfig::crtc_id`].
    pub crtc_index: u32,
    /// Cursor size in pixels, or zero to take the driver's preference.
    pub preferred_size: u32,
    /// Use this plane rather than choosing one. Zero to choose.
    pub forced_plane_id: u32,
    /// Whether falling back to `drmModeSetCursor` is acceptable.
    pub allow_legacy: bool,
    /// Which way up the cursor is drawn.
    pub rotation: Rotation,
}

impl Default for RendererConfig {
    fn default() -> Self {
        Self {
            crtc_id: 0,
            crtc_index: 0,
            preferred_size: 0,
            forced_plane_id: 0,
            // A cursor that cannot be committed with the frame is still better
            // than none, so the permissive default matches the reference.
            allow_legacy: true,
            rotation: Rotation::None,
        }
    }
}

/// A cursor on a plane.
///
/// Borrows the device: the buffers and the framebuffers registered for them
/// belong to it, and outliving it would leave this holding ids the kernel has
/// recycled.
#[derive(Debug)]
pub struct Renderer<'a> {
    device: &'a Device,
    crtc_id: u32,
    plane: SelectedPlane,
    rotation_mask: u64,
    props: PropertyStore,
    buffers: [Buffer; 2],
    which: PingPong,
    /// The loaded cursor, and where its current frame landed in the buffer.
    cursor: Option<Cursor>,
    placement: Option<Placement>,
    /// Which animation frame is in the buffer, so a tick that does not change
    /// it does no work.
    drawn_frame: Option<usize>,
    /// Where the pointer is.
    position: (i32, i32),
    visible: bool,
    /// Which way up the cursor is drawn.
    rotation: Rotation,
    /// Legacy only: whether the cursor has been handed to the CRTC yet.
    ///
    /// The first commit installs it; later ones are cheap moves.
    installed: bool,
}

impl<'a> Renderer<'a> {
    /// Set up a cursor on `config.crtc_id`.
    ///
    /// Chooses a plane, settles a buffer size by asking the kernel what it
    /// will take, and allocates. No cursor is loaded yet.
    ///
    /// # Errors
    ///
    /// - [`CursorError::NoPlane`] or [`CursorError::PlaneUnusable`] from
    ///   [`select_plane`].
    /// - [`CursorError::Unsupported`] if the only option is the legacy path,
    ///   which is not implemented here yet.
    /// - [`CursorError::Io`] if a buffer cannot be allocated or the plane's
    ///   properties cannot be read.
    pub fn create(
        device: &'a Device,
        registry: &PlaneRegistry,
        config: &RendererConfig,
    ) -> Result<Self, CursorError> {
        let forced = (config.forced_plane_id != 0).then_some(config.forced_plane_id);
        let bound_elsewhere =
            |plane_id: u32| plane_bound_elsewhere(device, plane_id, config.crtc_id);
        let plane = select_plane(
            registry,
            config.crtc_index,
            forced,
            config.allow_legacy,
            &bound_elsewhere,
        )?;

        if plane.path == PlanePath::Legacy {
            return Self::create_legacy(device, config, plane);
        }

        let mut props = PropertyStore::new();
        props
            .cache_properties(device, plane.plane_id, ObjectType::Plane)
            .map_err(|error| CursorError::Io(error.to_string()))?;

        let rotation_mask = registry
            .by_id(plane.plane_id)
            .map_or(0, |caps| caps.rotation_bits);

        // The driver's own preference, bounded by what the plane says it can
        // carry.
        let cap = (plane.cursor_max_w != 0).then_some(plane.cursor_max_w);
        let preferred = preferred_size(config.preferred_size, plane.path, cap);

        let mut buffers: Option<[Buffer; 2]> = None;
        let size = probe_size(preferred, |candidate| {
            match allocate_pair(device, candidate, plane.fourcc) {
                Ok(pair) => {
                    // The kernel is asked about the real thing: a request that
                    // arms this plane with this framebuffer at this size.
                    let accepted = pair[0].fb_id().is_some_and(|fb_id| {
                        test_plane(device, &props, &plane, config.crtc_id, fb_id, candidate)
                    });
                    if accepted {
                        buffers = Some(pair);
                    }
                    accepted
                }
                Err(_) => false,
            }
        });

        let buffers = match buffers {
            Some(pair) => pair,
            // Nothing was accepted, which is usually not about size at all --
            // with no mode set every test fails for unrelated reasons. Take
            // the fallback size and let the first real commit say what is
            // wrong.
            None => allocate_pair(device, size, plane.fourcc)?,
        };

        Ok(Self {
            device,
            crtc_id: config.crtc_id,
            plane,
            rotation_mask,
            props,
            buffers,
            which: PingPong::new(),
            cursor: None,
            placement: None,
            drawn_frame: None,
            position: (0, 0),
            visible: true,
            rotation: config.rotation,
            installed: false,
        })
    }

    /// Set up on the legacy `drmModeSetCursor` path.
    ///
    /// The call has no `TEST_ONLY`, so the probe is a cursor *disable*: a
    /// no-op on any controller that has cursor hardware, and a refusal on one
    /// that does not. i.MX's LCDIF and tilcdc have no cursor at all, and are
    /// exactly the parts where plane selection fell through to here -- finding
    /// out now lets the caller composite a cursor instead of getting a
    /// renderer that dies on its first show.
    ///
    /// A permission failure is reported separately, because "not DRM master"
    /// and "no cursor hardware" want different responses and look identical
    /// from the errno alone.
    #[allow(
        deprecated,
        reason = "drmModeSetCursor is deprecated in favour of a cursor plane, \
                  which is what every other path here uses. This is the \
                  fallback for controllers that have no such plane."
    )]
    fn create_legacy(
        device: &'a Device,
        config: &RendererConfig,
        plane: SelectedPlane,
    ) -> Result<Self, CursorError> {
        use drm::control::Device as _;

        if config.rotation != Rotation::None {
            // drmModeSetCursor has nowhere to put a rotation, and pre-rotating
            // in software would still leave the hotspot wrong for a caller who
            // thinks the hardware did it.
            return Err(CursorError::Unsupported);
        }

        let crtc = crtc_handle(config.crtc_id).ok_or(CursorError::Unsupported)?;
        if let Err(error) = device.set_cursor::<LegacyCursor>(crtc, None) {
            // EACCES and EPERM mean "not DRM master", which is a different
            // thing to tell a caller than "this controller has no cursor".
            return Err(match error.kind() {
                std::io::ErrorKind::PermissionDenied => {
                    CursorError::Io("not DRM master".to_owned())
                }
                _ => CursorError::Unsupported,
            });
        }

        // One buffer: the call uploads on install and would need reinstalling
        // for a content change anyway.
        let size = preferred_size(config.preferred_size, PlanePath::Legacy, None);
        let buffers = allocate_pair(device, size, plane.fourcc)?;

        Ok(Self {
            device,
            crtc_id: config.crtc_id,
            plane,
            rotation_mask: 0,
            props: PropertyStore::new(),
            buffers,
            which: PingPong::single(),
            cursor: None,
            placement: None,
            drawn_frame: None,
            position: (0, 0),
            visible: true,
            rotation: Rotation::None,
            installed: false,
        })
    }

    /// The plane this is drawing on.
    #[must_use]
    pub const fn plane(&self) -> SelectedPlane {
        self.plane
    }

    /// The size of the cursor buffer, in pixels.
    #[must_use]
    pub const fn size(&self) -> (u32, u32) {
        (self.buffers[0].width(), self.buffers[0].height())
    }

    /// Whether the plane can turn the sprite itself.
    ///
    /// When it can, the pixels are drawn upright and the hardware does the
    /// turn. When it cannot -- i915's cursor planes expose the property and
    /// list only 0 and 180 -- the pixels are turned as they are drawn, and
    /// the hardware is asked for none. Doing both turns it twice.
    const fn rotates_in_hardware(&self) -> bool {
        self.rotation_mask & self.rotation.drm_mask() != 0
    }

    /// How the sprite is turned as it is drawn.
    const fn draw_rotation(&self) -> Rotation {
        if self.rotates_in_hardware() {
            Rotation::None
        } else {
            self.rotation
        }
    }

    /// Change which way up the cursor is, redrawing it.
    ///
    /// # Errors
    ///
    /// [`CursorError::Io`] if the buffer cannot be mapped.
    pub fn set_rotation(&mut self, rotation: Rotation) -> Result<(), CursorError> {
        self.rotation = rotation;
        match self.drawn_frame {
            Some(index) => self.draw(index),
            None => Ok(()),
        }
    }

    /// Load a cursor and draw its first frame.
    ///
    /// # Errors
    ///
    /// [`CursorError::Io`] if the buffer cannot be mapped.
    pub fn set_cursor(&mut self, cursor: Cursor) -> Result<(), CursorError> {
        self.cursor = Some(cursor);
        self.drawn_frame = None;
        self.draw(0)
    }

    /// Move the pointer.
    pub const fn move_to(&mut self, x: i32, y: i32) {
        self.position = (x, y);
    }

    /// Show or hide the cursor without unloading it.
    pub const fn set_visible(&mut self, visible: bool) {
        self.visible = visible;
    }

    /// Advance an animation, drawing a new frame if one is due.
    ///
    /// Returns whether the buffer changed, so a caller can skip a commit that
    /// would publish the same pixels. A static cursor never changes.
    ///
    /// # Errors
    ///
    /// [`CursorError::Io`] if the buffer cannot be mapped.
    pub fn tick(&mut self, elapsed: Duration) -> Result<bool, CursorError> {
        let Some(cursor) = self.cursor.as_ref() else {
            return Ok(false);
        };
        if !cursor.is_animated() {
            return Ok(false);
        }
        let wanted = cursor.frame_index_at(elapsed);
        if self.drawn_frame == Some(wanted) {
            return Ok(false);
        }
        self.draw(wanted)?;
        Ok(true)
    }

    /// Write the cursor's current state into `request`.
    ///
    /// For coalescing with the rest of a frame. The caller commits, and calls
    /// [`Renderer::published`] afterwards.
    ///
    /// # Errors
    ///
    /// [`CursorError::MissingProperty`] if the plane lacks a required
    /// property.
    pub fn stage_into(&self, request: &mut AtomicRequest) -> Result<usize, CursorError> {
        let buffer = &self.buffers[self.which.commit_target()];
        let fb_id = if self.visible {
            buffer.fb_id().unwrap_or(0)
        } else {
            // Disarming the plane is how a cursor hides: there is no property
            // for "invisible", and a zero-alpha buffer still costs bandwidth.
            0
        };
        let state = PlaneState {
            plane_id: self.plane.plane_id,
            crtc_id: if self.visible { self.crtc_id } else { 0 },
            fb_id,
            width: buffer.width(),
            height: buffer.height(),
            pointer_x: self.position.0,
            pointer_y: self.position.1,
            hotspot: self
                .placement
                .map(|placed| (placed.hotspot_x, placed.hotspot_y)),
            // What the hardware is asked for. When it cannot do the turn,
            // `draw` already did it and this must ask for none.
            rotation: if self.rotates_in_hardware() {
                self.rotation
            } else {
                Rotation::None
            },
            rotation_mask: self.rotation_mask,
        };
        stage(request, &self.props, &state)
    }

    /// Note that a commit carrying [`Renderer::stage_into`]'s writes landed.
    ///
    /// See [`PingPong::published`] for what happens when it did not.
    pub const fn published(&mut self) {
        self.which.published();
    }

    /// Stage and commit on their own.
    ///
    /// For callers with nothing to coalesce with. A compositor should prefer
    /// [`Renderer::stage_into`].
    ///
    /// # Errors
    ///
    /// [`CursorError::MissingProperty`] from staging, or [`CursorError::Io`]
    /// if the kernel refuses the commit.
    pub fn commit(&mut self) -> Result<(), CursorError> {
        if self.plane.path == PlanePath::Legacy {
            return self.commit_legacy();
        }
        let mut request = AtomicRequest::new();
        self.stage_into(&mut request)?;
        // Enabling a plane is a modeset; moving one already enabled is not.
        // Blocking either way: a non-blocking cursor commit contends with
        // vblank pacing and returns EBUSY as soon as the mouse outpaces the
        // refresh rate.
        let flags = if self.installed {
            AtomicCommitFlags::empty()
        } else {
            AtomicCommitFlags::ALLOW_MODESET
        };
        request
            .commit(self.device, flags)
            .map_err(|error| CursorError::Io(error.to_string()))?;
        self.installed = true;
        self.published();
        Ok(())
    }

    /// Install or move the cursor through `drmModeSetCursor`.
    #[allow(
        deprecated,
        reason = "see create_legacy: this path exists for hardware with no \
                  cursor plane to use instead"
    )]
    fn commit_legacy(&mut self) -> Result<(), CursorError> {
        use drm::control::Device as _;

        let crtc = crtc_handle(self.crtc_id).ok_or(CursorError::Unsupported)?;
        let hotspot = self
            .placement
            .map_or((0, 0), |placed| (placed.hotspot_x, placed.hotspot_y));

        if !self.visible {
            self.device
                .set_cursor::<LegacyCursor>(crtc, None)
                .map_err(|error| CursorError::Io(error.to_string()))?;
            self.installed = false;
            return Ok(());
        }

        if !self.installed {
            let buffer = LegacyCursor {
                handle: self.buffers[0]
                    .gem_handle()
                    .ok_or_else(|| CursorError::Io("cursor buffer has no handle".to_owned()))?,
                width: self.buffers[0].width(),
                height: self.buffers[0].height(),
                pitch: self.buffers[0].stride(),
            };
            self.device
                .set_cursor(crtc, Some(&buffer))
                .map_err(|error| CursorError::Io(error.to_string()))?;
            self.installed = true;
        }

        let (x, y) = crate::stage::plane_origin(self.position, hotspot);
        self.device
            .move_cursor(crtc, (x, y))
            .map_err(|error| CursorError::Io(error.to_string()))?;
        Ok(())
    }

    /// Give up the device for a session pause.
    ///
    /// See [`Paused`] for what is released and what cannot be.
    #[must_use]
    pub fn pause(mut self) -> Paused {
        let size = self.buffers[0].width();
        for buffer in &mut self.buffers {
            buffer.forget();
        }
        Paused {
            crtc_id: self.crtc_id,
            plane: self.plane,
            rotation_mask: self.rotation_mask,
            rotation: self.rotation,
            cursor: self.cursor,
            drawn_frame: self.drawn_frame,
            position: self.position,
            visible: self.visible,
            size,
        }
    }

    /// Draw frame `index` into the buffer that is not on screen.
    fn draw(&mut self, index: usize) -> Result<(), CursorError> {
        let Some(cursor) = self.cursor.as_ref() else {
            return Ok(());
        };
        let frame = cursor
            .frames()
            .get(index)
            .unwrap_or_else(|| cursor.first())
            .clone();

        // Read what depends on `self` before the buffer is borrowed mutably.
        let rotation = self.draw_rotation();
        let fourcc = self.plane.fourcc;
        let target = self.which.draw_target();
        let buffer = &mut self.buffers[target];
        let (width, height) = (buffer.width(), buffer.height());
        let stride_px = (buffer.stride() / 4) as usize;

        let mut scratch = vec![0u32; height as usize * stride_px];
        let placement = sprite::compose(&frame, rotation, &mut scratch, width, height, stride_px);

        {
            let mut mapping = buffer.map(MapAccess::Write);
            for y in 0..height {
                let row = mapping
                    .row_mut(y)
                    .ok_or_else(|| CursorError::Io("cursor buffer row out of range".to_owned()))?;
                let from = y as usize * stride_px;
                for (x, pixel) in scratch[from..from + width as usize].iter().enumerate() {
                    let at = x * 4;
                    row[at..at + 4].copy_from_slice(&pack(*pixel, fourcc).to_ne_bytes());
                }
            }
        }

        self.placement = Some(placement);
        self.drawn_frame = Some(index);
        self.which.mark_drawn();
        Ok(())
    }
}

/// Permute a shadow pixel into the plane's channel order.
///
/// The shadow is `0xAARRGGBB` in a host word, which is `ARGB8888` in memory on
/// a little-endian host. The other three cursor formats are the same eight-bit
/// channels in a different order, so each is a permutation of that word rather
/// than a conversion: no precision is lost and no channel is invented.
///
/// A format not in [`CURSOR_FORMATS`] cannot be selected, so the fallthrough
/// is unreachable by construction; it returns the pixel unchanged rather than
/// panicking, because a wrongly ordered cursor is a visible bug and a dead
/// process is a black screen.
pub(crate) const fn pack(pixel: u32, fourcc: u32) -> u32 {
    let (a, r, g, b) = (
        pixel >> 24,
        (pixel >> 16) & 0xff,
        (pixel >> 8) & 0xff,
        pixel & 0xff,
    );
    match fourcc {
        drmkit_fmt::fourcc::RGBA8888 => (r << 24) | (g << 16) | (b << 8) | a,
        drmkit_fmt::fourcc::ABGR8888 => (a << 24) | (b << 16) | (g << 8) | r,
        drmkit_fmt::fourcc::BGRA8888 => (b << 24) | (g << 16) | (r << 8) | a,
        // `ARGB8888`, and anything that never reaches selection.
        _ => pixel,
    }
}

/// A dumb buffer, described the way `drmModeSetCursor` wants it.
struct LegacyCursor {
    handle: u32,
    width: u32,
    height: u32,
    pitch: u32,
}

impl drm::buffer::Buffer for LegacyCursor {
    fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }
    fn format(&self) -> drm::buffer::DrmFourcc {
        drm::buffer::DrmFourcc::Argb8888
    }
    fn pitch(&self) -> u32 {
        self.pitch
    }
    fn handle(&self) -> drm::buffer::Handle {
        // The kernel is about to be handed this, so a zero would be a bug
        // upstream of here rather than something to paper over.
        drm::buffer::Handle::from(
            core::num::NonZeroU32::new(self.handle)
                .unwrap_or(core::num::NonZeroU32::new(1).expect("one is not zero")),
        )
    }
}

/// A CRTC id as a handle.
fn crtc_handle(crtc_id: u32) -> Option<drm::control::crtc::Handle> {
    core::num::NonZeroU32::new(crtc_id).map(drm::control::crtc::Handle::from)
}

/// Allocate a pair of cursor buffers with framebuffers registered.
fn allocate_pair(device: &Device, size: u32, fourcc: u32) -> Result<[Buffer; 2], CursorError> {
    let config = Config {
        width: size,
        height: size,
        fourcc,
        bpp: 32,
        add_fb: true,
    };
    let first = Buffer::create(device, &config).map_err(|e| CursorError::Io(e.to_string()))?;
    let second = Buffer::create(device, &config).map_err(|e| CursorError::Io(e.to_string()))?;
    Ok([first, second])
}

/// Ask the kernel whether this plane will take a buffer of this size.
fn test_plane(
    device: &Device,
    props: &PropertyStore,
    plane: &SelectedPlane,
    crtc_id: u32,
    fb_id: u32,
    size: u32,
) -> bool {
    let mut request = AtomicRequest::new();
    let state = PlaneState {
        plane_id: plane.plane_id,
        crtc_id,
        fb_id,
        width: size,
        height: size,
        pointer_x: 0,
        pointer_y: 0,
        // Nothing has been drawn, so there is no hotspot to state yet.
        hotspot: None,
        rotation: Rotation::None,
        rotation_mask: 0,
    };
    if stage(&mut request, props, &state).is_err() {
        return false;
    }
    request.test(device, AtomicCommitFlags::empty()).is_ok()
}

/// Whether a plane is currently feeding some CRTC other than `crtc_id`.
fn plane_bound_elsewhere(device: &Device, plane_id: u32, crtc_id: u32) -> bool {
    use drm::control::Device as _;

    let Some(handle) = core::num::NonZeroU32::new(plane_id) else {
        return false;
    };
    device
        .get_plane(drm::control::plane::Handle::from(handle))
        .ok()
        .and_then(|info| info.crtc())
        .is_some_and(|bound| u32::from(bound) != crtc_id)
}

/// A cursor renderer whose device has gone away.
///
/// A session pause revokes the descriptor: the kernel state behind it is gone,
/// so the buffers cannot be destroyed and the framebuffers cannot be removed
/// -- those ioctls would go to a dead descriptor, or worse to whatever the
/// number has since been reused for.
///
/// The mappings are a different matter and are released here. Closing a
/// descriptor does not unmap anything -- a VMA holds its own reference to the
/// file -- so a compositor that skipped this would leak one mapping per VT
/// switch, quietly, for as long as it ran.
///
/// This exists as a type rather than a flag so that a paused renderer cannot
/// be committed. There is nothing to commit to.
#[derive(Debug)]
pub struct Paused {
    crtc_id: u32,
    plane: SelectedPlane,
    rotation_mask: u64,
    rotation: Rotation,
    cursor: Option<Cursor>,
    drawn_frame: Option<usize>,
    position: (i32, i32),
    visible: bool,
    size: u32,
}

impl Paused {
    /// Rebuild on a new descriptor.
    ///
    /// The same plane, the same size, and the cursor redrawn at whatever frame
    /// it had reached -- so the first commit after a resume carries the right
    /// pixels rather than an empty buffer or a stale one.
    ///
    /// The size is not re-probed. The hardware has not changed, and a resume
    /// is when a compositor is trying to get something on screen quickly
    /// rather than spend test commits rediscovering what it already knew.
    ///
    /// # Errors
    ///
    /// [`CursorError::Io`] if the buffers cannot be reallocated or the plane's
    /// properties cannot be read on the new descriptor.
    pub fn resume(self, device: &Device) -> Result<Renderer<'_>, CursorError> {
        let mut props = PropertyStore::new();
        if self.plane.path != PlanePath::Legacy {
            props
                .cache_properties(device, self.plane.plane_id, ObjectType::Plane)
                .map_err(|error| CursorError::Io(error.to_string()))?;
        }

        let mut renderer = Renderer {
            device,
            crtc_id: self.crtc_id,
            plane: self.plane,
            rotation_mask: self.rotation_mask,
            props,
            buffers: allocate_pair(device, self.size, self.plane.fourcc)?,
            which: if self.plane.path == PlanePath::Legacy {
                PingPong::single()
            } else {
                PingPong::new()
            },
            cursor: self.cursor,
            placement: None,
            drawn_frame: None,
            position: self.position,
            visible: self.visible,
            rotation: self.rotation,
            // The new descriptor knows nothing of the old plane state, so the
            // next commit re-arms everything.
            installed: false,
        };

        if renderer.cursor.is_some() {
            renderer.draw(self.drawn_frame.unwrap_or(0))?;
        }
        Ok(renderer)
    }

    /// Whether a cursor is still loaded, and will be redrawn on resume.
    #[must_use]
    pub const fn has_cursor(&self) -> bool {
        self.cursor.is_some()
    }
}
