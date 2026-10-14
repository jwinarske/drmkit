// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

use std::os::fd::{FromRawFd as _, OwnedFd};
use std::sync::Arc;

use crate::device::{DeviceInner, SCANOUT_RENDERING};
use crate::ffi::{self, BoPtr};
use crate::surface::SurfaceInner;
use crate::{GbmDevice, GbmError};

/// How a CPU mapping will be used.
///
/// GBM needs this up front: a driver may have to move or detile a buffer to
/// make it CPU-readable, and it can skip that when the caller only writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MapAccess {
    /// Read the existing pixels.
    Read,
    /// Overwrite them.
    Write,
    /// Both.
    ReadWrite,
}

/// Who gives the buffer back, and how.
enum Owner {
    /// Allocated here: `gbm_bo_destroy`.
    Allocated,
    /// Locked from a surface: `gbm_surface_release_buffer`, which hands it
    /// back to the swap chain rather than freeing it. Holding the surface is
    /// what keeps it alive until then.
    Surface(Arc<SurfaceInner>),
}

/// A GBM buffer: one this crate allocated, or one locked from a
/// [`GbmSurface`](crate::GbmSurface).
pub struct GbmBuffer {
    ptr: BoPtr,
    owner: Owner,
    /// The device and the descriptor the buffer's GEM handle lives on. Freed
    /// after the buffer, since fields drop after `Drop::drop` runs; see
    /// [`GbmDevice`].
    device: Arc<DeviceInner>,
}

// SAFETY: as `DeviceInner`'s: a `gbm_bo` is not tied to a thread, and the
// pointer is given back exactly once, on drop.
unsafe impl Send for GbmBuffer {}
// SAFETY: as above; every `&self` method only reads the buffer.
unsafe impl Sync for GbmBuffer {}

impl Drop for GbmBuffer {
    fn drop(&mut self) {
        let gbm = self.device.gbm;
        match &self.owner {
            // SAFETY: allocated by `gbm_bo_create*` and not yet destroyed.
            Owner::Allocated => unsafe { (gbm.bo_destroy)(self.ptr) },
            // SAFETY: locked from this surface, which `owner` keeps alive, and
            // not yet released.
            Owner::Surface(surface) => unsafe {
                (gbm.surface_release_buffer)(surface.ptr, self.ptr);
            },
        }
    }
}

impl std::fmt::Debug for GbmBuffer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GbmBuffer")
            .field("width", &self.width())
            .field("height", &self.height())
            .field("modifier", &format_args!("{:#x}", self.modifier()))
            .finish_non_exhaustive()
    }
}

impl GbmBuffer {
    /// Take ownership of a buffer locked from `surface`.
    pub(crate) fn locked(ptr: BoPtr, surface: Arc<SurfaceInner>) -> Self {
        Self {
            ptr,
            device: Arc::clone(&surface.device),
            owner: Owner::Surface(surface),
        }
    }

    /// Allocate a scanout-capable buffer.
    ///
    /// The driver chooses the layout and reports it through
    /// [`modifier`](Self::modifier). Asking for scanout up front matters: a
    /// buffer allocated only for rendering may land in a layout the display
    /// engine cannot read, and the failure then surfaces at commit time as a
    /// rejected framebuffer rather than here.
    ///
    /// # Errors
    ///
    /// [`GbmError::Allocation`] if the driver refuses the format or size.
    pub fn create(
        device: &GbmDevice,
        width: u32,
        height: u32,
        fourcc: u32,
    ) -> Result<Self, GbmError> {
        let inner = &device.inner;
        // SAFETY: the device is live.
        let ptr =
            unsafe { (inner.gbm.bo_create)(inner.ptr, width, height, fourcc, SCANOUT_RENDERING) };
        if ptr.is_null() {
            return Err(GbmError::Allocation(ffi::last_error("gbm_bo_create")));
        }
        Ok(Self {
            ptr,
            owner: Owner::Allocated,
            device: Arc::clone(inner),
        })
    }

    /// Allocate a scanout-capable buffer, constrained to `modifiers`.
    ///
    /// The driver picks one of them and reports which through
    /// [`modifier`](Self::modifier). Constraining matters when the display
    /// engine and the GPU disagree about layouts: left to itself the driver
    /// picks what renders fastest, which on a tiling GPU is a layout no plane
    /// can scan out, and the failure then surfaces at commit time as a rejected
    /// framebuffer rather than here.
    ///
    /// An empty list means "no constraint" and falls back to
    /// [`create`](Self::create). So does a driver that refuses every modifier
    /// offered, or a libgbm with no modifier entry point at all: the caller
    /// gets a buffer whose `modifier` may be outside the list it asked for,
    /// which is why the modifier is worth reading back rather than assumed.
    /// Drivers substitute on their own too -- measured, the SA8155P's GBM
    /// answers a tiled request with linear, and NXP's with `INVALID` -- so the
    /// fallback is not the only way to get there.
    ///
    /// `gbm_bo_create_with_modifiers2` is used where the library has it, for
    /// its usage flags, and `gbm_bo_create_with_modifiers` where it does not
    /// -- the SA8155P's libgbm has only the latter (P-39).
    ///
    /// # Errors
    ///
    /// [`GbmError::Allocation`] if the driver refuses the format or the size.
    /// Refusing every modifier offered is not an error; see above.
    pub fn create_with_modifiers(
        device: &GbmDevice,
        width: u32,
        height: u32,
        fourcc: u32,
        modifiers: &[u64],
    ) -> Result<Self, GbmError> {
        if modifiers.is_empty() {
            return Self::create(device, width, height, fourcc);
        }
        let inner = &device.inner;
        let Ok(count) = u32::try_from(modifiers.len()) else {
            return Self::create(device, width, height, fourcc);
        };
        let mut ptr = std::ptr::null_mut();
        if let Some(create) = inner.gbm.bo_create_with_modifiers2 {
            // SAFETY: the device is live; `modifiers` is `count` long and only
            // read for the duration of the call.
            ptr = unsafe {
                create(
                    inner.ptr,
                    width,
                    height,
                    fourcc,
                    modifiers.as_ptr(),
                    count,
                    SCANOUT_RENDERING,
                )
            };
        } else if let Some(create) = inner.gbm.bo_create_with_modifiers {
            // SAFETY: as above.
            ptr = unsafe { create(inner.ptr, width, height, fourcc, modifiers.as_ptr(), count) };
        }
        if ptr.is_null() {
            return Self::create(device, width, height, fourcc);
        }
        Ok(Self {
            ptr,
            owner: Owner::Allocated,
            device: Arc::clone(inner),
        })
    }

    /// Width in pixels.
    #[must_use]
    pub fn width(&self) -> u32 {
        // SAFETY: the buffer is live.
        unsafe { (self.device.gbm.bo_get_width)(self.ptr) }
    }

    /// Height in pixels.
    #[must_use]
    pub fn height(&self) -> u32 {
        // SAFETY: the buffer is live.
        unsafe { (self.device.gbm.bo_get_height)(self.ptr) }
    }

    /// Bytes per row.
    #[must_use]
    pub fn stride(&self) -> u32 {
        // SAFETY: the buffer is live.
        unsafe { (self.device.gbm.bo_get_stride)(self.ptr) }
    }

    /// The DRM `FourCC` the driver allocated.
    #[must_use]
    pub fn fourcc(&self) -> u32 {
        // SAFETY: the buffer is live.
        unsafe { (self.device.gbm.bo_get_format)(self.ptr) }
    }

    /// The layout modifier the driver chose.
    ///
    /// Not a formality. A tiled or compressed buffer scanned out as though it
    /// were linear is not slightly wrong — it is unreadable — so this has to
    /// travel with the buffer to whatever registers the framebuffer.
    #[must_use]
    pub fn modifier(&self) -> u64 {
        // SAFETY: the buffer is live.
        unsafe { (self.device.gbm.bo_get_modifier)(self.ptr) }
    }

    /// How many planes the layout uses.
    #[must_use]
    pub fn plane_count(&self) -> u32 {
        // SAFETY: the buffer is live.
        let count = unsafe { (self.device.gbm.bo_get_plane_count)(self.ptr) };
        u32::try_from(count).unwrap_or(0)
    }

    /// The buffer's GEM handle on the device's descriptor, which is what a
    /// framebuffer is registered over.
    #[must_use]
    pub fn handle(&self) -> u32 {
        // SAFETY: the buffer is live.
        let handle = unsafe { (self.device.gbm.bo_get_handle)(self.ptr) };
        // SAFETY: every arm of the union is plain data, and the 32-bit one is
        // the one a DRM GEM handle uses.
        unsafe { handle.u32_ }
    }

    /// Export a dma-buf descriptor for the buffer.
    ///
    /// The caller owns it. This is how a GBM buffer reaches anything outside
    /// this process, or reaches KMS through the external-source path.
    ///
    /// # Errors
    ///
    /// [`GbmError::Allocation`] if the driver cannot export it.
    pub fn export(&self) -> Result<OwnedFd, GbmError> {
        // SAFETY: the buffer is live.
        let fd = unsafe { (self.device.gbm.bo_get_fd)(self.ptr) };
        if fd < 0 {
            return Err(GbmError::Allocation(ffi::last_error("gbm_bo_get_fd")));
        }
        // SAFETY: `gbm_bo_get_fd` returns a new descriptor the caller owns.
        Ok(unsafe { OwnedFd::from_raw_fd(fd) })
    }

    /// The `struct gbm_bo *`, for an API that takes one: an EGL image import,
    /// say.
    ///
    /// Borrowed: valid while this buffer is alive.
    #[must_use]
    pub fn as_raw(&self) -> *mut std::ffi::c_void {
        self.ptr
    }
}
