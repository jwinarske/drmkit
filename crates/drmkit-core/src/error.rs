// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! Error type for `drmkit-core`.

use std::io;

use rustix::io::Errno;

/// Errors from device, property, and atomic-request operations.
///
/// The variants carry the same discriminants the C++ `std::error_code` paths
/// document, so a consumer bridging both implementations classifies failures
/// identically. `EACCES` in particular stays distinguishable: the scene's
/// suspend-on-`EACCES` rule (invariant 2) depends on telling it apart from
/// every other commit failure, and it must never be flattened into a generic
/// I/O error.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum CoreError {
    /// The path opened, but the kernel does not report it as a DRM device.
    /// `errc::no_such_device`.
    #[error("`{path}` is not a DRM device")]
    NotADrmDevice {
        /// The path that was opened.
        path: String,
    },

    /// No properties are cached for the object.
    /// `errc::no_such_file_or_directory`.
    #[error("no properties cached for object {object_id}")]
    NoSuchObject {
        /// The DRM object id.
        object_id: u32,
    },

    /// The object has no property by that name.
    /// `errc::no_such_file_or_directory`.
    #[error("object {object_id} has no property `{name}`")]
    NoSuchProperty {
        /// The DRM object id.
        object_id: u32,
        /// The property name that was looked up.
        name: String,
    },

    /// The operation needs a live descriptor and the handle has none.
    /// `errc::bad_file_descriptor`.
    #[error("device holds no descriptor")]
    NoDevice,

    /// The commit was rejected because the caller is not DRM master.
    ///
    /// Split out from [`CoreError::Io`] deliberately: a scene suspends on this
    /// and on nothing else (invariant 2), so burying it in a generic errno
    /// would break the contract silently.
    #[error("not DRM master")]
    NotMaster,

    /// An underlying syscall failed.
    #[error("DRM operation failed: {0}")]
    Io(#[from] Errno),
}

impl CoreError {
    /// Classify an `errno` from a commit or ioctl, promoting `EACCES` to
    /// [`CoreError::NotMaster`].
    pub(crate) fn from_errno(errno: Errno) -> Self {
        if errno == Errno::ACCESS {
            Self::NotMaster
        } else {
            Self::Io(errno)
        }
    }

    /// Classify an [`io::Error`] from the `drm` crate, which returns
    /// `io::Result` throughout.
    pub(crate) fn from_io(error: &io::Error) -> Self {
        Errno::from_io_error(error).map_or(Self::NoDevice, Self::from_errno)
    }

    /// The underlying `errno`, when there is one.
    #[must_use]
    pub const fn errno(&self) -> Option<Errno> {
        match self {
            Self::Io(errno) => Some(*errno),
            Self::NotMaster => Some(Errno::ACCESS),
            _ => None,
        }
    }
}

impl From<CoreError> for io::Error {
    fn from(e: CoreError) -> Self {
        e.errno().map_or_else(
            || Self::from(io::ErrorKind::NotFound),
            |errno| Self::from_raw_os_error(errno.raw_os_error()),
        )
    }
}

/// Convenience alias.
pub type Result<T> = std::result::Result<T, CoreError>;
