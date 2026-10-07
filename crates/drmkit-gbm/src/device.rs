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

    /// Whether this device can make GBM **surfaces** at all.
    ///
    /// A display-only DRM node has no render node, so Mesa gives it the
    /// minimal GBM backend: buffers work, surfaces do not. That backend does
    /// not *refuse* a surface — `gbm_surface_create` returns a handle whose
    /// entry points are absent, and the first call through one takes the
    /// process down with `SIGSEGV`. Measured on vkms, where
    /// `gbm_surface_create` succeeds and `gbm_surface_has_free_buffers`
    /// segfaults immediately.
    ///
    /// So the question has to be answered before asking for a surface, and
    /// the only answer available is whether the kernel gave this device a
    /// render node: that is the DRI backend's precondition, and without the
    /// DRI backend there are no surfaces. Read from sysfs by device number,
    /// rather than from the driver name — vkms reports `faux_driver` there,
    /// and a name-based check would be wrong on the first device that
    /// disagrees.
    ///
    /// Conservative in the safe direction: an unreadable sysfs answers `false`
    /// and costs a caller a surface it might have had, where the opposite
    /// costs it the process.
    ///
    /// # Necessary, not sufficient
    ///
    /// A render node is what the DRI backend needs, not a promise it started.
    /// Measured on this machine's amdgpu, which *has* one: Mesa's
    /// `amdgpu_query_info(ACCEL_WORKING)` failed with `EACCES`, the DRI
    /// backend fell back to the same minimal one, and the surface crashed
    /// identically. GBM exposes nothing that separates the two — both report
    /// backend `drm`, both allocate buffers, both create a surface — so a
    /// caller on a device that passes this check can still be handed one that
    /// is unsafe to touch. That is a defect in what GBM offers, and this is
    /// the best answer available on top of it.
    #[must_use]
    pub fn supports_surfaces(&self) -> bool {
        let Ok(stat) = rustix::fs::fstat(self.inner.as_fd()) else {
            return false;
        };
        let (major, minor) = (
            rustix::fs::major(stat.st_rdev),
            rustix::fs::minor(stat.st_rdev),
        );
        let siblings = format!("/sys/dev/char/{major}:{minor}/device/drm");
        let Ok(entries) = std::fs::read_dir(siblings) else {
            return false;
        };
        entries
            .flatten()
            .any(|entry| entry.file_name().to_string_lossy().starts_with("renderD"))
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
        // Before anything else: a device with no surface backend hands back a
        // handle that crashes on first use rather than refusing. See
        // `supports_surfaces`.
        if !self.supports_surfaces() {
            return Err(GbmError::NoSurfaceSupport);
        }

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
