// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! The DRM device handle.

use std::os::fd::{AsFd, AsRawFd as _, BorrowedFd, OwnedFd, RawFd};
use std::path::Path;

use drm::ClientCapability;
use drm::control::Device as ControlDevice;

use rustix::io::Errno;

use crate::error::{CoreError, Result};

/// How a [`Device`] holds its descriptor.
#[derive(Debug)]
enum Fd {
    /// Opened by this crate; closed on drop.
    Owned(OwnedFd),
    /// Owned elsewhere — typically a revocable descriptor from a libseat
    /// session. Never closed here.
    Borrowed(RawFd),
}

/// An open DRM device.
///
/// Port of `drm::Device` from `src/core/device.{hpp,cpp}`.
///
/// Implements the `drm` crate's [`Device`](drm::Device) and
/// [`control::Device`](drm::control::Device) traits, so every KMS operation
/// those traits provide is available directly. drmkit wraps rather than
/// re-binds (DR-2); the methods here are the ones the C++ adds on top or gives
/// a different contract.
#[derive(Debug)]
pub struct Device {
    fd: Fd,
}

impl Device {
    /// Open a DRM device node and verify the kernel agrees it is one.
    ///
    /// Sets DRM master if possible. Failing to become master is **not** an
    /// error: a client running under a compositor never will be, and the C++
    /// ignores the result for the same reason.
    ///
    /// # Errors
    ///
    /// - [`CoreError::Io`] if the node cannot be opened.
    /// - [`CoreError::NotADrmDevice`] if `drmGetVersion` fails on it, which is
    ///   how the C++ distinguishes a real card from any other character device.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let file = rustix::fs::open(
            path,
            rustix::fs::OFlags::RDWR | rustix::fs::OFlags::CLOEXEC,
            rustix::fs::Mode::empty(),
        )
        .map_err(CoreError::from_errno)?;

        let device = Self {
            fd: Fd::Owned(file),
        };

        // Verify this is actually a DRM device.
        if drm::Device::get_driver(&device).is_err() {
            return Err(CoreError::NotADrmDevice {
                path: path.display().to_string(),
            });
        }

        // Non-fatal: a client under a compositor is never master.
        if let Err(e) = drm::Device::acquire_master_lock(&device) {
            drmkit_log::log_debug!(
                "could not become DRM master on {}: {e} (expected under a compositor)",
                path.display()
            );
        }

        Ok(device)
    }

    /// Take ownership of an already-open DRM descriptor.
    ///
    /// The descriptor is closed when the device is dropped. This has no C++
    /// counterpart: `Device::from_fd` there is always non-owning, because C++
    /// cannot express the transfer in a type.
    #[must_use]
    pub const fn from_owned_fd(fd: OwnedFd) -> Self {
        Self { fd: Fd::Owned(fd) }
    }

    /// Wrap a descriptor owned by someone else.
    ///
    /// The returned device does **not** close `fd` on drop; the caller — for
    /// instance a seat session holding a revocable libseat descriptor — keeps
    /// that responsibility. Resume flows replace the device wholesale with a
    /// freshly built one, exactly as the C++ move-assigns a new `from_fd`.
    ///
    /// # Safety
    ///
    /// `fd` must be a valid descriptor that stays open for the whole life of
    /// the returned `Device` and of everything derived from it. This cannot be
    /// checked: the borrow has no lifetime to tie it to, which is precisely why
    /// the safe [`Device::from_owned_fd`] exists and should be preferred.
    #[must_use]
    pub const unsafe fn from_borrowed_fd(fd: RawFd) -> Self {
        Self {
            fd: Fd::Borrowed(fd),
        }
    }

    /// The raw descriptor, for interop with code that needs one.
    ///
    /// Borrowed, never transferred.
    #[must_use]
    pub fn raw_fd(&self) -> RawFd {
        match &self.fd {
            Fd::Owned(fd) => fd.as_raw_fd(),
            Fd::Borrowed(fd) => *fd,
        }
    }

    /// Whether this device will close its descriptor on drop.
    #[must_use]
    pub const fn owns_fd(&self) -> bool {
        matches!(self.fd, Fd::Owned(_))
    }

    /// Enable the universal-planes client capability.
    ///
    /// Without it the kernel hides primary and cursor planes from
    /// `plane_handles`, and the allocator sees only overlays.
    ///
    /// # Errors
    ///
    /// [`CoreError::Io`] if the driver rejects the capability.
    pub fn enable_universal_planes(&self) -> Result<()> {
        self.set_client_cap(ClientCapability::UniversalPlanes, true)
    }

    /// Enable the atomic client capability.
    ///
    /// # Errors
    ///
    /// [`CoreError::Io`] if the driver does not support atomic modesetting.
    pub fn enable_atomic(&self) -> Result<()> {
        self.set_client_cap(ClientCapability::Atomic, true)
    }

    /// Set an arbitrary client capability.
    ///
    /// # Errors
    ///
    /// [`CoreError::Io`] if the driver rejects it.
    pub fn set_client_cap(&self, cap: ClientCapability, enable: bool) -> Result<()> {
        drm::Device::set_client_capability(self, cap, enable).map_err(|e| CoreError::from_io(&e))
    }

    /// Become DRM master.
    ///
    /// # Errors
    ///
    /// [`CoreError::NotMaster`] if another client already holds it.
    pub fn set_master(&self) -> Result<()> {
        drm::Device::acquire_master_lock(self).map_err(|e| CoreError::from_io(&e))
    }

    /// Release DRM master.
    ///
    /// # Errors
    ///
    /// [`CoreError::Io`] if the ioctl fails.
    pub fn drop_master(&self) -> Result<()> {
        drm::Device::release_master_lock(self).map_err(|e| CoreError::from_io(&e))
    }

    /// Read an immutable blob property's payload.
    ///
    /// This is the seam `drmkit-fmt` is built around: the `IN_FORMATS` blob is
    /// fetched here and parsed by
    /// [`FormatTable::from_blob`](https://docs.rs/drmkit-fmt), which keeps the
    /// format crate device-free and `no_std`.
    ///
    /// # Errors
    ///
    /// [`CoreError::Io`] if the blob id is unknown to the kernel.
    pub fn property_blob(&self, blob_id: u64) -> Result<Vec<u8>> {
        self.get_property_blob(blob_id)
            .map_err(|e| CoreError::from_io(&e))
    }
}

/// A kernel property blob, freed when it is dropped.
///
/// `MODE_ID` takes a blob id rather than a value, so a scene that sets a mode
/// has to create one and keep it alive for as long as the mode is set. Letting
/// it leak would be invisible until a long-running compositor that re-modesets
/// often ran the kernel out of blob ids.
///
/// The borrow of the device is what makes freeing it safe. Holding a raw
/// descriptor instead would compile, and would destroy a blob on whatever the
/// kernel had since recycled that number into if the blob ever outlived the
/// device it came from.
pub struct PropertyBlob<'a> {
    id: u64,
    device: &'a Device,
}

impl PropertyBlob<'_> {
    /// The blob id, to be written into a blob-typed property.
    #[must_use]
    pub const fn id(&self) -> u64 {
        self.id
    }
}

impl Drop for PropertyBlob<'_> {
    fn drop(&mut self) {
        // Nothing useful to do if this fails: the blob dies with the device
        // either way, and panicking in `drop` would be worse than the leak.
        let _ = drm::control::Device::destroy_property_blob(self.device, self.id);
    }
}

impl Device {
    /// Stage `data` as a kernel property blob.
    ///
    /// The blob borrows the device, so it cannot outlive the descriptor that
    /// has to free it.
    ///
    /// # Errors
    ///
    /// [`CoreError`] if the kernel refuses to create the blob.
    pub fn create_property_blob<T: ?Sized>(&self, data: &T) -> Result<PropertyBlob<'_>> {
        let value = drm::control::Device::create_property_blob(self, data)
            .map_err(|e| CoreError::Io(Errno::from_io_error(&e).unwrap_or(Errno::INVAL)))?;
        // The call is documented to return a blob value. A different variant
        // would mean the crate changed under us, and writing whatever integer
        // it carried into a blob property would be worse than failing here.
        let drm::control::property::Value::Blob(id) = value else {
            return Err(CoreError::Io(Errno::INVAL));
        };
        Ok(PropertyBlob { id, device: self })
    }
}

impl AsFd for Device {
    fn as_fd(&self) -> BorrowedFd<'_> {
        match &self.fd {
            Fd::Owned(fd) => fd.as_fd(),
            // SAFETY: the constructor is `unsafe` precisely because the caller
            // promised this descriptor outlives the `Device`. The borrow here
            // is bounded by `&self`, which is shorter.
            Fd::Borrowed(raw) => unsafe { BorrowedFd::borrow_raw(*raw) },
        }
    }
}

impl drm::Device for Device {}
impl drm::control::Device for Device {}
