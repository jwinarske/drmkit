// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

use std::os::fd::{AsFd, OwnedFd};

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
    inner: gbm::Device<OwnedFd>,
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
        let inner = gbm::Device::new(duped).map_err(|_| GbmError::NotGbmCapable)?;
        Ok(Self { inner })
    }

    /// The backend the driver bound, for diagnostics.
    ///
    /// `drm` is the generic path — dumb allocation on a display-only driver,
    /// the kernel's own allocator elsewhere.
    #[must_use]
    pub fn backend_name(&self) -> String {
        self.inner.backend_name().to_owned()
    }

    pub(crate) const fn raw(&self) -> &gbm::Device<OwnedFd> {
        &self.inner
    }
}
