// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! A scene source backed by a GBM **surface**.
//!
//! Port of `src/scene/gbm_surface_source.{hpp,cpp}`.
//!
//! # Surface, not buffer
//!
//! `drmkit-scene-sources`'s `GbmBufferSource` hands out one
//! allocation the caller renders into. A *surface* is a swap chain: EGL or
//! Vulkan renders into it, `eglSwapBuffers` rotates it, and this source locks
//! whichever buffer came to the front. That is the shape a GL compositor
//! needs, and the reason both exist.
//!
//! # What this crate does not do
//!
//! It creates no GL context and issues no draw calls. The producer owns the
//! rendering; drmkit owns getting the result on screen. [`native_surface`] is
//! what a producer passes to `eglCreateWindowSurface`.
//!
//! # Bind before the first acquire
//!
//! A `gbm_surface` is an EGL native window. Mesa routes `lock_front_buffer`,
//! `has_free_buffers` and `release_buffer` into the window surface EGL (or
//! Vulkan) creates on it, and until one exists each of them is a `SIGSEGV`,
//! not an error, on every Mesa device. GBM has no safe query for this, so the
//! producer has to say it happened. Either bind the surface and render a
//! frame before adding the layer to a scene, or set
//! [`SurfaceConfig::require_bind`] and call
//! [`mark_bound`](GbmSurfaceSource::mark_bound) once the window surface
//! exists: until then [`acquire`](GbmSurfaceSource::acquire) reports
//! [`SourceError::WouldBlock`] instead of touching the surface.
//!
//! [`native_surface`]: GbmSurfaceSource::native_surface

use std::collections::HashMap;

use drm::control::{Device as ControlDevice, FbCmd2Flags, framebuffer};
use drmkit_core::Device;
use drmkit_scene::{AcquiredBuffer, BindingModel, LayerBufferSource, SourceError, SourceFormat};
use drmkit_sync::SyncFence;

/// How to create the surface.
#[derive(Debug, Clone, Copy)]
pub struct SurfaceConfig {
    /// Width in pixels. Non-zero.
    pub width: u32,
    /// Height in pixels. Non-zero.
    pub height: u32,
    /// The `FourCC` to render and scan out. Non-zero.
    pub fourcc: u32,
    /// A layout to constrain the surface to.
    ///
    /// `None` lets the driver choose, which is right when the caller has not
    /// negotiated one. When it has — from
    /// `PlaneRegistry::candidate_modifiers` intersected with what the render
    /// API can export — passing it here is what keeps the result scannable.
    pub modifier: Option<u64>,
    /// Hold `acquire` at [`SourceError::WouldBlock`] until
    /// [`GbmSurfaceSource::mark_bound`].
    ///
    /// Rather than lock a front buffer on a surface no producer has bound yet,
    /// which is a `SIGSEGV` in Mesa. A resume rebuilds the surface and closes
    /// the gate again. Off by default, so a producer that binds before the
    /// first commit needs nothing.
    pub require_bind: bool,
}

impl Default for SurfaceConfig {
    fn default() -> Self {
        Self {
            width: 0,
            height: 0,
            fourcc: drmkit_fmt::fourcc::XRGB8888,
            modifier: None,
            require_bind: false,
        }
    }
}

/// Why a surface source could not be built or resumed.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SurfaceError {
    /// A dimension or format was zero.
    ///
    /// Refused here rather than passed to GBM, which reports the same thing
    /// less clearly and only sometimes.
    #[error("{field} must be non-zero")]
    Invalid {
        /// Which one.
        field: &'static str,
    },

    /// The format is not one GBM can name.
    #[error("format {fourcc:#x} is not a GBM format")]
    UnsupportedFormat {
        /// The `FourCC` asked for.
        fourcc: u32,
    },

    /// The device could not be opened as a GBM device.
    #[error("opening a GBM device: {0}")]
    Device(#[from] drmkit_gbm::GbmError),

    /// The driver refused the surface.
    #[error("creating the GBM surface: {0}")]
    Create(String),

    /// A locked buffer could not be registered as a framebuffer.
    #[error("registering a framebuffer: {0}")]
    Framebuffer(rustix::io::Errno),
}

/// A GBM swap chain the scene can scan out of.
pub struct GbmSurfaceSource {
    surface: gbm::Surface<()>,
    format: SourceFormat,
    config: SurfaceConfig,
    /// Framebuffers registered for the surface's buffers, keyed by GEM handle.
    ///
    /// A GBM surface cycles a small fixed set of buffers, so the same handles
    /// come round frame after frame. Registering a framebuffer each time would
    /// leak one per frame; caching by handle registers each once and destroys
    /// them together when the surface goes.
    framebuffers: HashMap<u32, framebuffer::Handle>,
    /// Buffers currently locked, keyed by the token handed to the scene.
    ///
    /// Holding the `BufferObject` is what keeps the buffer locked: the Rust
    /// binding releases it back to the surface on drop, so `release` is the
    /// act of dropping the entry.
    locked: HashMap<u64, gbm::BufferObject<()>>,
    next_token: u64,
    /// The producer's render-done fence, if it set one.
    pending_fence: Option<SyncFence>,
    /// The descriptor the framebuffers were registered on, for teardown.
    device_fd: std::os::fd::RawFd,
    /// Set by `on_session_paused`: the descriptor is gone, so the framebuffer
    /// ids must not be committed and must not be destroyed through it either.
    paused: bool,
    /// Set by `mark_bound`: the producer has created its window surface, so
    /// locking a front buffer is safe. Gates `acquire` only when
    /// `config.require_bind`, and cleared when a resume rebuilds the surface.
    bound: bool,
    /// Declared last, so it drops last. Fields drop in declaration order, and
    /// the surface and the locked buffers free their GEM handles through this
    /// device's descriptor -- a libgbm that keeps no descriptor of its own
    /// (the SA8155P's) would otherwise free them through a closed one.
    gbm: drmkit_gbm::GbmDevice,
}

impl std::fmt::Debug for GbmSurfaceSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GbmSurfaceSource")
            .field("format", &self.format)
            .field("registered", &self.framebuffers.len())
            .field("locked", &self.locked.len())
            .finish_non_exhaustive()
    }
}

impl GbmSurfaceSource {
    /// Create a surface on `device`.
    ///
    /// # Errors
    ///
    /// [`SurfaceError::Invalid`] for a zero dimension or format,
    /// [`SurfaceError::UnsupportedFormat`] for a `FourCC` GBM does not know,
    /// [`SurfaceError::Device`] if the node is not a GBM device, and
    /// [`SurfaceError::Create`] if the driver refuses the surface.
    pub fn create(device: &Device, config: &SurfaceConfig) -> Result<Self, SurfaceError> {
        if config.width == 0 {
            return Err(SurfaceError::Invalid { field: "width" });
        }
        if config.height == 0 {
            return Err(SurfaceError::Invalid { field: "height" });
        }
        if config.fourcc == 0 {
            return Err(SurfaceError::Invalid { field: "fourcc" });
        }
        // Refused here rather than by GBM: `gbm_surface_create` reports an
        // unknown format the same way it reports a driver refusal, and a
        // caller can do something about only one of them.
        if gbm::Format::try_from(config.fourcc).is_err() {
            return Err(SurfaceError::UnsupportedFormat {
                fourcc: config.fourcc,
            });
        }

        let gbm = drmkit_gbm::GbmDevice::new(device)?;
        let surface = Self::make_surface(&gbm, config)?;

        Ok(Self {
            format: SourceFormat {
                fourcc: config.fourcc,
                // Not known until a buffer is locked: the driver picks, and
                // `acquire` reads it back off the first one.
                modifier: config.modifier.unwrap_or(0),
                width: config.width,
                height: config.height,
            },
            surface,
            config: *config,
            framebuffers: HashMap::new(),
            locked: HashMap::new(),
            next_token: 1,
            pending_fence: None,
            device_fd: device.raw_fd(),
            paused: false,
            bound: false,
            gbm,
        })
    }

    /// Build the surface, constrained or not.
    fn make_surface(
        gbm: &drmkit_gbm::GbmDevice,
        config: &SurfaceConfig,
    ) -> Result<gbm::Surface<()>, SurfaceError> {
        let modifiers: Vec<u64> = config.modifier.into_iter().collect();
        gbm.create_surface(config.width, config.height, config.fourcc, &modifiers)
            .map_err(|error| SurfaceError::Create(error.to_string()))
    }

    /// The `gbm_surface` a producer renders into.
    ///
    /// What `eglCreateWindowSurface` takes. Borrowed, not owned: the source
    /// destroys it, and a producer outliving the source would be rendering
    /// into freed memory.
    #[must_use]
    pub const fn native_surface(&self) -> &gbm::Surface<()> {
        &self.surface
    }

    /// The `gbm_device` the surface belongs to.
    #[must_use]
    pub const fn native_device(&self) -> &drmkit_gbm::GbmDevice {
        &self.gbm
    }

    /// Set the producer's render-done fence for the next acquire.
    ///
    /// The scene hands it to `IN_FENCE_FD` where the plane takes one, so the
    /// display engine waits for the render rather than the CPU doing it.
    pub fn set_acquire_fence(&mut self, fence: SyncFence) {
        self.pending_fence = Some(fence);
    }

    /// The producer has created its window surface on
    /// [`native_surface`](Self::native_surface).
    ///
    /// Opens the gate [`SurfaceConfig::require_bind`] closes. Call it again
    /// after every resume: the surface is rebuilt, and the producer has to
    /// bind the new one.
    pub const fn mark_bound(&mut self) {
        self.bound = true;
    }

    /// Whether the surface has a buffer free to render into.
    ///
    /// A producer must check before drawing: with every buffer locked, the
    /// next `eglSwapBuffers` has nowhere to go. Only once the surface is bound
    /// -- before that, Mesa segfaults here.
    #[must_use]
    pub fn has_free_buffers(&self) -> bool {
        self.surface.has_free_buffers()
    }

    /// Register a framebuffer over a locked buffer, or reuse the one this
    /// buffer already has.
    fn framebuffer_for(
        &mut self,
        device: &RawDevice<'_>,
        buffer: &gbm::BufferObject<()>,
    ) -> Result<framebuffer::Handle, SurfaceError> {
        // SAFETY: `gbm_bo_handle` is a union of equivalent integer widths and
        // the u32 arm is the one every DRM GEM handle uses; the C API has no
        // other way to read it.
        let handle = unsafe { buffer.handle().u32_ };
        if let Some(existing) = self.framebuffers.get(&handle) {
            return Ok(*existing);
        }

        let fourcc = drm::buffer::DrmFourcc::try_from(self.format.fourcc).map_err(|_| {
            SurfaceError::UnsupportedFormat {
                fourcc: self.format.fourcc,
            }
        })?;
        let modifier: u64 = buffer.modifier().into();
        // The same rule every import follows; see
        // `drmkit_core::framebuffer_modifier`.
        let declared = drmkit_core::framebuffer_modifier(device, modifier);
        let layout = Layout {
            width: self.format.width,
            height: self.format.height,
            fourcc,
            modifier: declared,
            handle,
            pitch: buffer.stride(),
        };
        let flags = if declared.is_some() {
            FbCmd2Flags::MODIFIERS
        } else {
            FbCmd2Flags::empty()
        };

        let fb = device
            .add_planar_framebuffer(&layout, flags)
            .map_err(|error| {
                SurfaceError::Framebuffer(
                    rustix::io::Errno::from_io_error(&error).unwrap_or(rustix::io::Errno::INVAL),
                )
            })?;
        self.framebuffers.insert(handle, fb);
        Ok(fb)
    }
}

/// One buffer's layout, for `add_planar_framebuffer`.
///
/// A GBM surface buffer is single-plane by construction, so the other three
/// slots are always empty.
struct Layout {
    width: u32,
    height: u32,
    fourcc: drm::buffer::DrmFourcc,
    modifier: Option<u64>,
    handle: u32,
    pitch: u32,
}

impl drm::buffer::PlanarBuffer for Layout {
    fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    fn format(&self) -> drm::buffer::DrmFourcc {
        self.fourcc
    }

    fn modifier(&self) -> Option<drm::buffer::DrmModifier> {
        self.modifier.map(drm::buffer::DrmModifier::from)
    }

    fn pitches(&self) -> [u32; 4] {
        [self.pitch, 0, 0, 0]
    }

    fn handles(&self) -> [Option<drm::buffer::Handle>; 4] {
        [
            std::num::NonZeroU32::new(self.handle).map(drm::buffer::Handle::from),
            None,
            None,
            None,
        ]
    }

    fn offsets(&self) -> [u32; 4] {
        [0; 4]
    }
}

/// A bare descriptor wrapper, so framebuffers can be registered and destroyed
/// without the source owning a `Device`.
struct RawDevice<'a> {
    fd: std::os::fd::BorrowedFd<'a>,
}

impl std::os::fd::AsFd for RawDevice<'_> {
    fn as_fd(&self) -> std::os::fd::BorrowedFd<'_> {
        self.fd
    }
}

impl drm::Device for RawDevice<'_> {}
impl ControlDevice for RawDevice<'_> {}

impl LayerBufferSource for GbmSurfaceSource {
    /// Lock whatever the producer last rendered.
    ///
    /// # Safety of the underlying call
    ///
    /// `gbm_surface_lock_front_buffer` must be called exactly once per
    /// `eglSwapBuffers`, and never before the first. Calling it otherwise is
    /// undefined behaviour in GBM, not merely an error — which is why this is
    /// the producer's contract to keep and cannot be checked here. A source
    /// whose producer has not swapped reports [`SourceError::WouldBlock`] only
    /// where GBM says so. Before the producer binds the surface the call is a
    /// `SIGSEGV`; [`SurfaceConfig::require_bind`] turns that into
    /// [`SourceError::WouldBlock`].
    ///
    /// # Errors
    ///
    /// [`SourceError::WouldBlock`] when there is no front buffer to lock,
    /// which is flow control. Anything else means the buffer could not be made
    /// into a framebuffer.
    fn acquire(&mut self) -> Result<AcquiredBuffer, SourceError> {
        if self.paused || (self.config.require_bind && !self.bound) {
            return Err(SourceError::WouldBlock);
        }

        // SAFETY: the contract is the producer's, and is documented above. The
        // surface is live and this source is the only thing that locks it.
        let buffer =
            unsafe { self.surface.lock_front_buffer() }.map_err(|_| SourceError::WouldBlock)?;

        // SAFETY: the descriptor belongs to the device this source was built
        // against, which outlives it; the borrow lasts only for the ioctl.
        let borrowed = unsafe { std::os::fd::BorrowedFd::borrow_raw(self.device_fd) };
        let device = RawDevice { fd: borrowed };
        let fb = self
            .framebuffer_for(&device, &buffer)
            .map_err(|_| SourceError::Failed(rustix::io::Errno::INVAL))?;

        // The layout is whatever the driver chose, which is only knowable once
        // a buffer exists. A caller that negotiated one still has to read this
        // back: the constrained entry point falls back where it is missing.
        self.format.modifier = buffer.modifier().into();

        let token = self.next_token;
        self.next_token = self.next_token.wrapping_add(1).max(1);
        self.locked.insert(token, buffer);

        Ok(AcquiredBuffer {
            fb_id: u32::from(fb),
            token,
            acquire_fence: self
                .pending_fence
                .as_ref()
                .and_then(SyncFence::as_fd)
                .and_then(|fd| SyncFence::import(fd).ok()),
            damage: Vec::new(),
        })
    }

    /// Give the buffer back to the surface.
    ///
    /// Dropping the `BufferObject` is the release: the binding calls
    /// `gbm_surface_release_buffer` for us. A token with no entry is ignored —
    /// the scene releases on teardown too, and a double release must not
    /// hand the same buffer back twice.
    fn release(&mut self, acquired: AcquiredBuffer) {
        self.locked.remove(&acquired.token);
    }

    fn binding_model(&self) -> BindingModel {
        BindingModel::SceneSubmitsFbId
    }

    fn format(&self) -> SourceFormat {
        self.format
    }

    /// The session is losing DRM master.
    ///
    /// Every framebuffer id was registered on a descriptor that is about to
    /// stop working, so they are forgotten rather than destroyed — the ioctl
    /// would fail, and the kernel reclaims them when the descriptor closes.
    /// Locked buffers go back to the surface, which is a GBM object and
    /// unaffected by DRM master.
    fn on_session_paused(&mut self) {
        self.paused = true;
        self.framebuffers.clear();
        self.locked.clear();
    }

    /// The session is back with a fresh device.
    ///
    /// The surface is rebuilt too, not just the framebuffers: it was created
    /// against a `gbm_device` wrapping the dead descriptor, and buffers
    /// allocated through it cannot be registered on the new one.
    ///
    /// # Errors
    ///
    /// [`SourceError::Unsupported`] if the new device cannot be opened as a
    /// GBM device or refuses the surface.
    fn on_session_resumed(&mut self, device: &Device) -> Result<(), SourceError> {
        let gbm = drmkit_gbm::GbmDevice::new(device).map_err(|_| SourceError::Unsupported)?;
        let surface =
            Self::make_surface(&gbm, &self.config).map_err(|_| SourceError::Unsupported)?;

        // Old buffers, then the old surface, then the old device: each frees
        // through the descriptor the next one owns.
        self.locked.clear();
        self.surface = surface;
        self.gbm = gbm;
        self.framebuffers.clear();
        self.device_fd = device.raw_fd();
        self.paused = false;
        // A new surface: the producer must bind it again.
        self.bound = false;
        Ok(())
    }
}

impl Drop for GbmSurfaceSource {
    fn drop(&mut self) {
        if self.paused || self.framebuffers.is_empty() {
            return;
        }
        // SAFETY: as in `acquire` -- the device outlives this source, and the
        // borrow lasts only for these ioctls.
        let borrowed = unsafe { std::os::fd::BorrowedFd::borrow_raw(self.device_fd) };
        let device = RawDevice { fd: borrowed };
        for fb in self.framebuffers.values() {
            // Unactionable in a destructor, and the kernel reclaims them when
            // the descriptor closes either way.
            let _ = device.destroy_framebuffer(*fb);
        }
    }
}

#[cfg(test)]
mod tests;
