// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

use std::ffi::CStr;
use std::os::fd::{AsFd, AsRawFd as _, OwnedFd};
use std::sync::Arc;

use crate::ffi::{self, DevicePtr, Gbm};
use crate::{GbmError, GbmSurface};

/// `GBM_BO_USE_SCANOUT | GBM_BO_USE_RENDERING`.
pub(crate) const SCANOUT_RENDERING: u32 = (1 << 0) | (1 << 2);

/// The `gbm_device` and the descriptor it was opened on, destroyed in that
/// order when the last thing holding them goes.
///
/// A GEM handle is freed through the descriptor it was allocated on, and
/// buffers and surfaces outlive the [`GbmDevice`] that made them. Not every
/// libgbm keeps a descriptor of its own -- the SA8155P's frees through this
/// one -- so every buffer and surface holds a share of this, and the device
/// and the descriptor stay open until the last of them is gone.
pub(crate) struct DeviceInner {
    pub(crate) gbm: &'static Gbm,
    pub(crate) ptr: DevicePtr,
    /// Closed after `ptr` is destroyed: fields drop after `Drop::drop` runs.
    _fd: OwnedFd,
}

// SAFETY: a `gbm_device` is not tied to the thread that opened it, and the
// `gbm` crate this replaced made the same claim for the same pointers. The
// pointer is destroyed exactly once, by the last owner.
unsafe impl Send for DeviceInner {}
// SAFETY: as above.
unsafe impl Sync for DeviceInner {}

impl Drop for DeviceInner {
    fn drop(&mut self) {
        // SAFETY: `ptr` came from `gbm_create_device` and nothing else holds
        // it: every buffer and surface keeps this alive, so they are gone.
        unsafe { (self.gbm.device_destroy)(self.ptr) };
    }
}

/// A GBM allocator bound to a DRM device.
///
/// It holds its **own** duplicate of the descriptor rather than borrowing the
/// caller's. A borrow would tie every buffer's lifetime to the device handle,
/// and buffers routinely outlive the scope that allocated them — they are
/// handed to a scene, a swapchain, a compositor. The duplicate shares the same
/// open file description, so the GEM namespace is the one the DRM device sees:
/// a buffer allocated here can be scanned out there.
///
/// libgbm is opened at run time, on the first device: a process without it
/// gets [`GbmError::NotGbmCapable`], not a binary that will not start.
#[derive(Clone)]
pub struct GbmDevice {
    pub(crate) inner: Arc<DeviceInner>,
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
    /// As [`from_fd`](Self::from_fd).
    #[cfg(feature = "drmkit-core")]
    pub fn new(device: &drmkit_core::Device) -> Result<Self, GbmError> {
        Self::from_fd(device)
    }

    /// Open the DRM node `fd` refers to as a GBM allocator.
    ///
    /// The descriptor is duplicated, so the caller's stays theirs to close.
    ///
    /// # Errors
    ///
    /// [`GbmError::Io`] if the descriptor cannot be duplicated, and
    /// [`GbmError::NotGbmCapable`] if libgbm cannot be opened or will not
    /// open this node.
    pub fn from_fd(fd: impl AsFd) -> Result<Self, GbmError> {
        let gbm = ffi::gbm()?;
        let duped = fd.as_fd().try_clone_to_owned().map_err(|e| {
            GbmError::Io(rustix::io::Errno::from_io_error(&e).unwrap_or(rustix::io::Errno::MFILE))
        })?;
        // SAFETY: the descriptor is open and is kept open by `_fd` for as long
        // as the device exists.
        let ptr = unsafe { (gbm.create_device)(duped.as_raw_fd()) };
        if ptr.is_null() {
            return Err(GbmError::NotGbmCapable(ffi::last_error(
                "gbm_create_device",
            )));
        }
        Ok(Self {
            inner: Arc::new(DeviceInner {
                gbm,
                ptr,
                _fd: duped,
            }),
        })
    }

    /// Create a rendering surface — a swap chain a GL or Vulkan producer
    /// draws into.
    ///
    /// Distinct from a buffer: a surface holds several, and `eglSwapBuffers`
    /// rotates them. `modifiers` constrains the layout the driver may pick,
    /// and an empty slice means no constraint. A driver that refuses every
    /// modifier offered gets the unconstrained surface instead, as
    /// [`GbmBuffer::create_with_modifiers`](crate::GbmBuffer::create_with_modifiers)
    /// does.
    ///
    /// The surface asks for `SCANOUT | RENDERING` up front. Asking for one
    /// alone is how a driver ends up choosing a layout the other cannot use,
    /// and that failure surfaces at the atomic commit rather than here.
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
    /// [`GbmError::Allocation`] if the driver refuses the surface, the format
    /// included.
    pub fn create_surface(
        &self,
        width: u32,
        height: u32,
        fourcc: u32,
        modifiers: &[u64],
    ) -> Result<GbmSurface, GbmError> {
        let gbm = self.inner.gbm;
        let device = self.inner.ptr;
        let count = u32::try_from(modifiers.len())
            .map_err(|_| GbmError::Allocation("too many modifiers".to_owned()))?;

        let mut ptr = std::ptr::null_mut();
        if !modifiers.is_empty() {
            // v2 where the library has it, for the usage flags; v1 otherwise,
            // which is all the SA8155P's libgbm offers. v1 takes no usage, and
            // Mesa's asks for SCANOUT alone: see
            // `GbmBuffer::create_with_modifiers`.
            if let Some(create) = gbm.surface_create_with_modifiers2 {
                // SAFETY: `device` is live; `modifiers` is `count` long and is
                // only read for the duration of the call.
                ptr = unsafe {
                    create(
                        device,
                        width,
                        height,
                        fourcc,
                        modifiers.as_ptr(),
                        count,
                        SCANOUT_RENDERING,
                    )
                };
            } else if let Some(create) = gbm.surface_create_with_modifiers {
                // SAFETY: as above.
                ptr = unsafe { create(device, width, height, fourcc, modifiers.as_ptr(), count) };
            }
        }
        if ptr.is_null() {
            // SAFETY: `device` is live.
            ptr = unsafe { (gbm.surface_create)(device, width, height, fourcc, SCANOUT_RENDERING) };
        }
        if ptr.is_null() {
            return Err(GbmError::Allocation(ffi::last_error("gbm_surface_create")));
        }
        Ok(GbmSurface::adopt(ptr, Arc::clone(&self.inner)))
    }

    /// The backend the driver bound, for diagnostics.
    ///
    /// `drm` is the generic path — dumb allocation on a display-only driver,
    /// the kernel's own allocator elsewhere.
    #[must_use]
    pub fn backend_name(&self) -> String {
        // SAFETY: `ptr` is live, and the name is a static string libgbm owns.
        let name = unsafe { (self.inner.gbm.device_get_backend_name)(self.inner.ptr) };
        if name.is_null() {
            return String::new();
        }
        // SAFETY: non-null, and NUL-terminated per gbm.h.
        unsafe { CStr::from_ptr(name) }
            .to_string_lossy()
            .into_owned()
    }

    /// The `struct gbm_device *`, for an API that takes one:
    /// `EGL_KHR_platform_gbm`'s `eglGetPlatformDisplay`, say.
    ///
    /// Borrowed. It stays valid while this device, or any buffer or surface
    /// made from it, is alive.
    #[must_use]
    pub fn as_raw(&self) -> *mut std::ffi::c_void {
        self.inner.ptr
    }
}
