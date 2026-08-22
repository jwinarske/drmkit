// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! `sync_file` fences for explicit synchronization.
//!
//! Port of `src/sync/` from drm-cxx @ `dc2915b`.
//!
//! A [`SyncFence`] wraps a kernel `sync_file` descriptor — the objects that
//! flow through a plane's `IN_FENCE_FD` property and come back out of a commit
//! through `OUT_FENCE_PTR`. The type owns its descriptor and closes it on drop.
//!
//! # Ownership
//!
//! The kernel does **not** close an fd passed as `IN_FENCE_FD`. The fence
//! therefore keeps ownership across a commit and still closes the descriptor
//! when dropped — handing [`SyncFence::as_fd`] to a property write does not
//! transfer anything. This is the same contract the C++ `fd()` accessor
//! documents, made structural by returning a [`BorrowedFd`].

use std::io;
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::time::Duration;

use rustix::io::Errno;

/// Errors from fence operations.
///
/// The variants carry the same discriminants the C++ `std::error_code` paths
/// document, so a consumer bridging both implementations sees the same
/// classification. In particular `EAGAIN`-style flow control never hides
/// inside a generic I/O error.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SyncError {
    /// The fence holds no descriptor.
    ///
    /// The C++ reports `errc::bad_file_descriptor` for this, and for a
    /// negative fd handed to `import_fd`. The latter case cannot arise here:
    /// [`SyncFence::import`] takes a [`BorrowedFd`], so an invalid descriptor
    /// is a type error rather than a runtime one.
    #[error("fence holds no descriptor")]
    Empty,

    /// The wait expired before the fence signaled. `errc::timed_out`.
    #[error("fence wait timed out")]
    TimedOut,

    /// The merge ioctl succeeded but returned no descriptor.
    #[error("SYNC_IOC_MERGE returned no fence")]
    MergeReturnedNothing,

    /// An underlying syscall failed.
    #[error("fence syscall failed: {0}")]
    Io(#[from] Errno),
}

impl From<SyncError> for io::Error {
    fn from(e: SyncError) -> Self {
        match e {
            SyncError::Empty => Self::from(io::ErrorKind::InvalidInput),
            SyncError::TimedOut => Self::from(io::ErrorKind::TimedOut),
            SyncError::MergeReturnedNothing => Self::from(io::ErrorKind::Other),
            SyncError::Io(errno) => Self::from_raw_os_error(errno.raw_os_error()),
        }
    }
}

/// `struct sync_merge_data` from `linux/sync_file.h`.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct SyncMergeData {
    /// Name of the merged fence. The kernel copies it verbatim; unused here.
    name: [u8; 32],
    /// Descriptor of the fence to merge in.
    fd2: i32,
    /// Out: descriptor of the merged fence.
    fence: i32,
    flags: u32,
    pad: u32,
}

/// `SYNC_IOC_MERGE`, i.e. `_IOWR(SYNC_IOC_MAGIC, 3, struct sync_merge_data)`
/// with `SYNC_IOC_MAGIC == '>'`.
const SYNC_IOC_MERGE: rustix::ioctl::Opcode =
    rustix::ioctl::opcode::read_write::<SyncMergeData>(b'>', 3);

/// A kernel `sync_file` fence.
///
/// Default-constructs empty, so a caller can hand one to a commit as an
/// out-parameter the way the C++ does.
#[derive(Debug, Default)]
pub struct SyncFence {
    fd: Option<OwnedFd>,
}

impl SyncFence {
    /// An empty fence holding no descriptor.
    #[must_use]
    pub const fn new() -> Self {
        Self { fd: None }
    }

    /// Import a fence by **duplicating** `fd`.
    ///
    /// The caller keeps ownership of `fd` and may close it immediately; this
    /// fence owns the duplicate. Matches the C++ `import_fd`, which dups for
    /// the same reason.
    ///
    /// # Errors
    ///
    /// [`SyncError::Io`] if `dup` fails — in practice `EMFILE` or `ENFILE`.
    pub fn import(fd: BorrowedFd<'_>) -> Result<Self, SyncError> {
        let duped = fd.try_clone_to_owned().map_err(|e| {
            // io::Error from a failed dup always carries an errno.
            Errno::from_io_error(&e).unwrap_or(Errno::MFILE)
        })?;
        Ok(Self { fd: Some(duped) })
    }

    /// Take ownership of an existing descriptor without duplicating it.
    ///
    /// The natural constructor when a commit has just produced an
    /// `OUT_FENCE_PTR` descriptor that nothing else holds. The C++ has no
    /// equivalent: it cannot express "this fd is yours now" in a type, so it
    /// dups unconditionally and the caller closes its copy.
    #[must_use]
    pub const fn from_owned(fd: OwnedFd) -> Self {
        Self { fd: Some(fd) }
    }

    /// Whether this holds a real `sync_file` descriptor.
    #[must_use]
    pub const fn is_valid(&self) -> bool {
        self.fd.is_some()
    }

    /// The underlying descriptor, for a plane's `IN_FENCE_FD` property.
    ///
    /// Borrowed, never transferred: the kernel does not close `IN_FENCE_FD`,
    /// so this fence still closes it on drop.
    #[must_use]
    pub fn as_fd(&self) -> Option<BorrowedFd<'_>> {
        self.fd.as_ref().map(AsFd::as_fd)
    }

    /// Release the descriptor to the caller, leaving this fence empty.
    #[must_use]
    pub fn into_owned_fd(self) -> Option<OwnedFd> {
        self.fd
    }

    /// Wait for the fence to signal, up to `timeout`.
    ///
    /// # Interruption
    ///
    /// A signal landing mid-wait surfaces as `SyncError::Io(Errno::INTR)`,
    /// matching the C++, which returns `EINTR` to its caller. This is
    /// deliberately **not** the `PageFlip::dispatch` contract (invariant 3),
    /// which retries internally: a fence wait is a caller-owned wait, and
    /// swallowing `EINTR` here would make the timeout unobservable to a caller
    /// that wants to act on the signal.
    ///
    /// # Errors
    ///
    /// - [`SyncError::Empty`] if the fence holds no descriptor.
    /// - [`SyncError::TimedOut`] if `timeout` elapsed first.
    /// - [`SyncError::Io`] if `poll` failed.
    pub fn wait(&self, timeout: Duration) -> Result<(), SyncError> {
        let fd = self.as_fd().ok_or(SyncError::Empty)?;

        let mut fds = [rustix::event::PollFd::new(
            &fd,
            rustix::event::PollFlags::IN,
        )];
        // rustix takes a Timespec, so unlike the C++ -- which clamps
        // milliseconds into an int -- there is nothing to saturate.
        let spec = rustix::event::Timespec {
            tv_sec: timeout.as_secs().try_into().unwrap_or(i64::MAX),
            tv_nsec: timeout.subsec_nanos().into(),
        };

        match rustix::event::poll(&mut fds, Some(&spec))? {
            0 => Err(SyncError::TimedOut),
            _ => Ok(()),
        }
    }

    /// Merge `other` into this fence.
    ///
    /// The merged fence signals when both inputs have. `other` is consumed;
    /// this fence's descriptor is replaced by the merged one.
    ///
    /// # Errors
    ///
    /// - [`SyncError::Empty`] if either fence holds no descriptor.
    /// - [`SyncError::MergeReturnedNothing`] if the ioctl reported success but
    ///   produced no descriptor.
    /// - [`SyncError::Io`] if the ioctl failed.
    pub fn merge(&mut self, other: Self) -> Result<(), SyncError> {
        let ours = self.as_fd().ok_or(SyncError::Empty)?;
        let theirs = other.as_fd().ok_or(SyncError::Empty)?;

        let mut data = SyncMergeData {
            name: [0; 32],
            fd2: rustix::fd::AsRawFd::as_raw_fd(&theirs),
            fence: -1,
            flags: 0,
            pad: 0,
        };

        // SAFETY: SYNC_IOC_MERGE is _IOWR(SYNC_IOC_MAGIC, 3, struct
        // sync_merge_data) and `SyncMergeData` is a #[repr(C)] definition of
        // exactly that struct, so the opcode and the value type agree. `ours`
        // is a live sync_file descriptor for the duration of the call, and
        // `data.fd2` borrows `theirs`, which outlives the call because `other`
        // is still owned here.
        unsafe {
            rustix::ioctl::ioctl(
                ours,
                rustix::ioctl::Updater::<SYNC_IOC_MERGE, SyncMergeData>::new(&mut data),
            )?;
        }

        // Explicit: `other` is consumed, and dropping it closes its descriptor.
        // The C++ relies on the destructor at end of scope for the same effect
        // ("other's destructor will close its fd"). Doing it here, after the
        // ioctl has read `fd2`, makes the ordering visible rather than implied.
        drop(other);

        if data.fence < 0 {
            return Err(SyncError::MergeReturnedNothing);
        }

        // SAFETY: the ioctl succeeded and reported a non-negative descriptor,
        // which the kernel has just created and handed to this process. Taking
        // ownership here is what closes it; nothing else holds a copy.
        let merged = unsafe { <OwnedFd as rustix::fd::FromRawFd>::from_raw_fd(data.fence) };

        // Replaces -- and so closes -- our previous descriptor.
        self.fd = Some(merged);
        Ok(())
    }
}

#[cfg(test)]
mod tests;
