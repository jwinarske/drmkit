// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! The buffer a decoration is drawn into.
//!
//! Always `ARGB8888`. A decoration is translucent by design — the panel is a
//! gradient with alpha, the shadow is nothing but alpha — so a format without
//! an alpha channel cannot represent one. Making it a constant rather than a
//! parameter means a caller cannot ask for a format the renderer would then
//! have to refuse.

use drm::control::Device as ControlDevice;
use drmkit_core::Device;
use drmkit_dumb::{Buffer, Config, MapAccess, Mapping};

/// How big a decoration surface is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SurfaceConfig {
    /// Width in pixels. Non-zero.
    pub width: u32,
    /// Height in pixels. Non-zero.
    pub height: u32,
}

/// Why a surface could not be created or used.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SurfaceError {
    /// A dimension was zero.
    #[error("{field} must be non-zero")]
    Invalid {
        /// Which one.
        field: &'static str,
    },

    /// The allocation failed.
    #[error("allocating a decoration surface: {0}")]
    Allocate(#[from] drmkit_dumb::DumbError),

    /// The surface holds no buffer.
    ///
    /// A default surface, or one that has been forgotten. Distinct from an
    /// allocation failure: nothing went wrong, there is simply nothing here.
    #[error("this surface holds no buffer")]
    Empty,
}

/// The one format a decoration is ever drawn in.
pub const SURFACE_FOURCC: u32 = drmkit_fmt::fourcc::ARGB8888;

/// A decoration's pixels, and the framebuffer they scan out through.
#[derive(Debug, Default)]
pub struct Surface {
    buffer: Option<Buffer>,
    /// Bumped on every `paint`, so the compositor can tell a decoration that
    /// moved from one that was redrawn.
    ///
    /// Without it, a decoration at the same place with different pixels looks
    /// unchanged to [`compute_damage`](crate::compute_damage) and is never
    /// recomposited.
    generation: u64,
}

impl Surface {
    /// Allocate a decoration surface.
    ///
    /// # Errors
    ///
    /// [`SurfaceError::Invalid`] for a zero dimension — refused here rather
    /// than passed to the kernel, which reports the same thing less clearly —
    /// and [`SurfaceError::Allocate`] if the driver refuses it.
    pub fn create(device: &Device, config: SurfaceConfig) -> Result<Self, SurfaceError> {
        if config.width == 0 {
            return Err(SurfaceError::Invalid { field: "width" });
        }
        if config.height == 0 {
            return Err(SurfaceError::Invalid { field: "height" });
        }

        let buffer = Buffer::create(
            device,
            &Config {
                width: config.width,
                height: config.height,
                fourcc: SURFACE_FOURCC,
                ..Config::default()
            },
        )?;

        Ok(Self {
            buffer: Some(buffer),
            generation: 0,
        })
    }

    /// Whether this surface holds no buffer.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.buffer.is_none()
    }

    /// The framebuffer id, or zero when empty.
    ///
    /// Zero rather than an error because that is what a presenter writes to
    /// disarm a plane — an empty surface and a disarmed slot are the same
    /// thing said twice.
    #[must_use]
    pub fn fb_id(&self) -> u32 {
        self.buffer.as_ref().and_then(Buffer::fb_id).unwrap_or(0)
    }

    /// Width in pixels, or zero when empty.
    #[must_use]
    pub fn width(&self) -> u32 {
        self.buffer.as_ref().map_or(0, Buffer::width)
    }

    /// Height in pixels, or zero when empty.
    #[must_use]
    pub fn height(&self) -> u32 {
        self.buffer.as_ref().map_or(0, Buffer::height)
    }

    /// Bytes between rows, or zero when empty.
    #[must_use]
    pub fn stride(&self) -> u32 {
        self.buffer.as_ref().map_or(0, Buffer::stride)
    }

    /// Always [`SURFACE_FOURCC`], empty or not.
    ///
    /// A constant rather than a stored field: an empty surface still answers,
    /// because a caller laying out a decoration it has not allocated yet
    /// needs to know what it will be.
    #[must_use]
    pub const fn format(&self) -> u32 {
        SURFACE_FOURCC
    }

    /// How many times this surface has been painted.
    #[must_use]
    pub const fn generation(&self) -> u64 {
        self.generation
    }

    /// Map the pixels for drawing, and count the paint.
    ///
    /// The generation is bumped whether or not anything is actually written:
    /// a caller that mapped for writing intends to, and under-reporting a
    /// change leaves a stale decoration on screen where over-reporting only
    /// costs a recomposite.
    ///
    /// # Errors
    ///
    /// [`SurfaceError::Empty`] when there is no buffer.
    pub fn paint(&mut self, access: MapAccess) -> Result<Mapping<'_>, SurfaceError> {
        let buffer = self.buffer.as_mut().ok_or(SurfaceError::Empty)?;
        if !matches!(access, MapAccess::Read) {
            self.generation = self.generation.wrapping_add(1);
        }
        Ok(buffer.map(access))
    }

    /// Export the buffer as a DMA-BUF.
    ///
    /// # Errors
    ///
    /// [`SurfaceError::Empty`] when there is no buffer, and whatever the
    /// export failed with otherwise.
    pub fn export(&self, device: &Device) -> Result<std::os::fd::OwnedFd, SurfaceError> {
        let buffer = self.buffer.as_ref().ok_or(SurfaceError::Empty)?;
        let handle = buffer
            .gem_handle()
            .and_then(drm::control::from_u32)
            .ok_or(SurfaceError::Empty)?;
        device
            .buffer_to_prime_fd(handle, 0)
            .map_err(|_| SurfaceError::Empty)
    }

    /// Drop the buffer without issuing ioctls against its descriptor.
    ///
    /// For a session that lost its device: the handles are already dead, so
    /// destroying them would fail, and the kernel reclaims them when the
    /// descriptor closes. Forgetting an already-empty surface is a no-op,
    /// which is what a caller tearing down twice does.
    pub fn forget(&mut self) {
        if let Some(mut buffer) = self.buffer.take() {
            buffer.forget();
        }
    }
}
