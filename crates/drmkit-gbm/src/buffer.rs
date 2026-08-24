// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

use std::os::fd::OwnedFd;

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
        Ok(Self { inner })
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
