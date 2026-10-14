// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

use std::sync::Arc;

use crate::GbmBuffer;
use crate::device::DeviceInner;
use crate::ffi::SurfacePtr;

/// The `gbm_surface`, destroyed when the last thing holding it goes: the
/// [`GbmSurface`] and every buffer locked from it.
pub(crate) struct SurfaceInner {
    pub(crate) ptr: SurfacePtr,
    /// The device the surface was made on, outliving it.
    pub(crate) device: Arc<DeviceInner>,
}

// SAFETY: as `DeviceInner`'s: a `gbm_surface` is not tied to a thread, and the
// pointer is destroyed exactly once, by the last owner.
unsafe impl Send for SurfaceInner {}
// SAFETY: as above.
unsafe impl Sync for SurfaceInner {}

impl Drop for SurfaceInner {
    fn drop(&mut self) {
        // SAFETY: from `gbm_surface_create*`; every locked buffer holds this,
        // so all of them have been released.
        unsafe { (self.device.gbm.surface_destroy)(self.ptr) };
    }
}

/// A GBM swap chain: what a GL or Vulkan producer renders into, and what the
/// scene locks finished frames out of.
///
/// Made by [`GbmDevice::create_surface`](crate::GbmDevice::create_surface),
/// whose documentation says what must happen before any of its methods but
/// [`as_raw`](Self::as_raw) is safe to call.
pub struct GbmSurface {
    inner: Arc<SurfaceInner>,
}

impl std::fmt::Debug for GbmSurface {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GbmSurface").finish_non_exhaustive()
    }
}

impl GbmSurface {
    pub(crate) fn adopt(ptr: SurfacePtr, device: Arc<DeviceInner>) -> Self {
        Self {
            inner: Arc::new(SurfaceInner { ptr, device }),
        }
    }

    /// Whether the surface has a buffer free to render into.
    ///
    /// Only once the surface is bound: before EGL or Vulkan has made a window
    /// surface on it, Mesa segfaults here.
    #[must_use]
    pub fn has_free_buffers(&self) -> bool {
        // SAFETY: the surface is live. Being bound is the caller's contract,
        // stated above and on `create_surface`.
        unsafe { (self.inner.device.gbm.surface_has_free_buffers)(self.inner.ptr) != 0 }
    }

    /// Lock the buffer the producer last swapped to the front.
    ///
    /// `None` when there is none. Dropping the returned buffer releases it
    /// back to the surface.
    ///
    /// # Safety
    ///
    /// Exactly once per `eglSwapBuffers` (or Vulkan present), and never before
    /// the first: anything else is undefined behaviour in GBM, not an error.
    /// Before the surface is bound the call is a `SIGSEGV` on Mesa.
    #[must_use]
    pub unsafe fn lock_front_buffer(&self) -> Option<GbmBuffer> {
        // SAFETY: the surface is live; the rest is the caller's contract.
        let ptr = unsafe { (self.inner.device.gbm.surface_lock_front_buffer)(self.inner.ptr) };
        (!ptr.is_null()).then(|| GbmBuffer::locked(ptr, Arc::clone(&self.inner)))
    }

    /// The `struct gbm_surface *`: what `eglCreateWindowSurface` takes as the
    /// native window.
    ///
    /// Borrowed: valid while this surface, or a buffer locked from it, is
    /// alive. A producer that outlives both is rendering into freed memory.
    #[must_use]
    pub fn as_raw(&self) -> *mut std::ffi::c_void {
        self.inner.ptr
    }
}
