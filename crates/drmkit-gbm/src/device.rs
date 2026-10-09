// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

use std::os::fd::{AsFd, OwnedFd};
use std::sync::Arc;

use drmkit_core::Device;

use crate::GbmError;

/// A GBM allocator bound to a DRM device.
///
/// It holds its **own** duplicate of the descriptor rather than borrowing the
/// [`Device`]. A borrow would tie every buffer's lifetime to the device handle,
/// and buffers routinely outlive the scope that allocated them — they are
/// handed to a scene, a swapchain, a compositor. The duplicate shares the same
/// open file description, so the GEM namespace is the one the DRM device sees:
/// a buffer allocated here can be scanned out there.
pub struct GbmDevice {
    inner: gbm::Device<Arc<OwnedFd>>,
    /// The descriptor `inner` was opened on, shared with every buffer
    /// allocated here.
    ///
    /// A GEM handle is freed through the descriptor it was allocated on, and a
    /// buffer can outlive this device. Not every libgbm keeps a descriptor of
    /// its own -- the SA8155P's frees through this one -- so each buffer holds
    /// a clone, and the number stays open until the last of them is gone.
    fd: Arc<OwnedFd>,
}

impl std::fmt::Debug for GbmDevice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GbmDevice").finish_non_exhaustive()
    }
}

impl GbmDevice {
    /// Open `device` as a GBM allocator.
    ///
    /// # Errors
    ///
    /// [`GbmError::Io`] if the descriptor cannot be duplicated;
    /// [`GbmError::NotGbmCapable`] if it is not a DRM node.
    pub fn new(device: &Device) -> Result<Self, GbmError> {
        let duped = device.as_fd().try_clone_to_owned().map_err(|e| {
            GbmError::Io(rustix::io::Errno::from_io_error(&e).unwrap_or(rustix::io::Errno::MFILE))
        })?;
        let fd = Arc::new(duped);
        let inner = gbm::Device::new(Arc::clone(&fd)).map_err(|_| GbmError::NotGbmCapable)?;
        Ok(Self { inner, fd })
    }

    /// Create a rendering surface — a swap chain a GL or Vulkan producer
    /// draws into.
    ///
    /// Distinct from a buffer: a surface holds several, and `eglSwapBuffers`
    /// rotates them. `modifiers` constrains the layout the driver may pick,
    /// and an empty slice means no constraint.
    ///
    /// The surface asks for `SCANOUT | RENDERING` up front. Asking for one
    /// alone is how a driver ends up choosing a layout the other cannot use,
    /// and that failure surfaces at the atomic commit rather than here.
    ///
    /// Unlike a [`GbmBuffer`](crate::GbmBuffer), the surface does not hold a
    /// share of this device's descriptor, so the device has to outlive it and
    /// every buffer locked from it: a libgbm that keeps no descriptor of its
    /// own frees them through this one.
    ///
    /// # Bind before you touch it
    ///
    /// A `gbm_surface` is an EGL native window. Mesa routes
    /// `gbm_surface_has_free_buffers`, `lock_front_buffer` and
    /// `release_buffer` into the window surface EGL (or Vulkan) creates on it,
    /// and until one exists each of them is a `SIGSEGV`, on every Mesa device
    /// and not just one driver. An initialized EGL display alone is not enough.
    /// Creating and dropping the surface is safe either way.
    ///
    /// # Errors
    ///
    /// [`GbmError::Allocation`] if the format is not one GBM knows, or the
    /// driver refuses the surface.
    pub fn create_surface(
        &self,
        width: u32,
        height: u32,
        fourcc: u32,
        modifiers: &[u64],
    ) -> Result<gbm::Surface<()>, GbmError> {
        let format = gbm::Format::try_from(fourcc)
            .map_err(|_| GbmError::Allocation(format!("unsupported format {fourcc:#x}")))?;
        let usage = gbm::BufferObjectFlags::SCANOUT | gbm::BufferObjectFlags::RENDERING;

        #[cfg(not(drmkit_gbm_v1))]
        let constrained = || {
            self.raw().create_surface_with_modifiers2::<()>(
                width,
                height,
                format,
                modifiers.iter().copied().map(gbm::Modifier::from),
                usage,
            )
        };
        // v1 implies the same usage. See `build.rs` for when this is built.
        #[cfg(drmkit_gbm_v1)]
        let constrained = || {
            self.raw().create_surface_with_modifiers::<()>(
                width,
                height,
                format,
                modifiers.iter().copied().map(gbm::Modifier::from),
            )
        };

        if !modifiers.is_empty()
            && let Ok(surface) = constrained()
        {
            // A driver with no modifier entry point falls through, the same as
            // `GbmBuffer::create_with_modifiers`. The caller reads the layout
            // back off a locked buffer rather than assuming it got what it
            // asked for.
            return Ok(surface);
        }

        self.raw()
            .create_surface::<()>(width, height, format, usage)
            .map_err(|error| GbmError::Allocation(error.to_string()))
    }

    /// The backend the driver bound, for diagnostics.
    ///
    /// `drm` is the generic path — dumb allocation on a display-only driver,
    /// the kernel's own allocator elsewhere.
    #[must_use]
    pub fn backend_name(&self) -> String {
        self.inner.backend_name().to_owned()
    }

    pub(crate) const fn raw(&self) -> &gbm::Device<Arc<OwnedFd>> {
        &self.inner
    }

    /// A share of the descriptor, for anything that must outlive this device.
    pub(crate) fn fd(&self) -> Arc<OwnedFd> {
        Arc::clone(&self.fd)
    }
}
