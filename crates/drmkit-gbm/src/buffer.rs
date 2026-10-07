// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

use std::os::fd::OwnedFd;
use std::sync::Arc;

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

/// A GBM-allocated buffer.
pub struct GbmBuffer {
    inner: gbm::BufferObject<()>,
    /// The device descriptor the buffer's GEM handle lives on. Declared after
    /// `inner` so the buffer is freed while it is still open; see
    /// [`GbmDevice`]'s field of the same name.
    _fd: Arc<OwnedFd>,
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
        let format = gbm::Format::try_from(fourcc)
            .map_err(|_| GbmError::Allocation(format!("unsupported format {fourcc:#x}")))?;
        let inner = device
            .raw()
            .create_buffer_object::<()>(
                width,
                height,
                format,
                gbm::BufferObjectFlags::SCANOUT | gbm::BufferObjectFlags::RENDERING,
            )
            .map_err(|e| GbmError::Allocation(e.to_string()))?;
        Ok(Self {
            inner,
            _fd: device.fd(),
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
    /// offered: the caller gets a buffer whose `modifier` may be outside the
    /// list it asked for, which is why the modifier is worth reading back
    /// rather than assumed. Drivers substitute on their own too -- measured,
    /// the SA8155P's GBM answers a tiled request with linear, and NXP's with
    /// `INVALID` -- so the fallback is not the only way to get there.
    ///
    /// A libgbm without the v2 entry point is a build-time matter, not this
    /// fallback: see `build.rs`.
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
        let format = gbm::Format::try_from(fourcc)
            .map_err(|_| GbmError::Allocation(format!("unsupported format {fourcc:#x}")))?;
        #[cfg(not(drmkit_gbm_v1))]
        let result = device.raw().create_buffer_object_with_modifiers2::<()>(
            width,
            height,
            format,
            modifiers.iter().copied().map(gbm::Modifier::from),
            gbm::BufferObjectFlags::SCANOUT | gbm::BufferObjectFlags::RENDERING,
        );
        // v1 takes no usage and implies scanout and rendering, which is what
        // v2 is asked for above. See `build.rs` for when this is built.
        #[cfg(drmkit_gbm_v1)]
        let result = device.raw().create_buffer_object_with_modifiers::<()>(
            width,
            height,
            format,
            modifiers.iter().copied().map(gbm::Modifier::from),
        );
        match result {
            Ok(inner) => Ok(Self {
                inner,
                _fd: device.fd(),
            }),
            // Not every driver implements the modifier path, and one that
            // does may still refuse every layout offered -- v3d refuses all
            // but linear once scanout is asked for. Falling back is what keeps
            // this usable there; reading the modifier back is what keeps it
            // honest.
            Err(_) => Self::create(device, width, height, fourcc),
        }
    }

    /// Width in pixels.
    #[must_use]
    pub fn width(&self) -> u32 {
        self.inner.width()
    }

    /// Height in pixels.
    #[must_use]
    pub fn height(&self) -> u32 {
        self.inner.height()
    }

    /// Bytes per row.
    #[must_use]
    pub fn stride(&self) -> u32 {
        self.inner.stride()
    }

    /// The DRM `FourCC` the driver allocated.
    #[must_use]
    pub fn fourcc(&self) -> u32 {
        self.inner.format() as u32
    }

    /// The layout modifier the driver chose.
    ///
    /// Not a formality. A tiled or compressed buffer scanned out as though it
    /// were linear is not slightly wrong — it is unreadable — so this has to
    /// travel with the buffer to whatever registers the framebuffer.
    #[must_use]
    pub fn modifier(&self) -> u64 {
        self.inner.modifier().into()
    }

    /// How many planes the layout uses.
    #[must_use]
    pub fn plane_count(&self) -> u32 {
        self.inner.plane_count()
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
        self.inner
            .fd()
            .map_err(|e| GbmError::Allocation(e.to_string()))
    }

    /// The underlying buffer object.
    #[must_use]
    pub const fn raw(&self) -> &gbm::BufferObject<()> {
        &self.inner
    }
}
