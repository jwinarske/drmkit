// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! A source wrapping a caller-owned, externally allocated DMA-BUF.
//!
//! Port of `src/scene/external_dma_buf_source.{hpp,cpp}`.

use std::os::fd::{AsFd, BorrowedFd, OwnedFd};

use drm::control::{Device as ControlDevice, FbCmd2Flags, framebuffer};
use drmkit_core::Device;
use drmkit_scene::{AcquiredBuffer, BindingModel, LayerBufferSource, SourceError, SourceFormat};
use drmkit_sync::SyncFence;

/// One plane of an externally allocated buffer.
///
/// `fd` is caller-owned — the source duplicates it, so the caller may close
/// theirs the moment the constructor returns.
#[derive(Debug, Clone, Copy)]
pub struct ExternalPlane<'a> {
    /// The DMA-BUF descriptor.
    pub fd: BorrowedFd<'a>,
    /// Byte offset of this plane within the descriptor.
    pub offset: u32,
    /// Row stride in bytes.
    pub pitch: u32,
}

/// Why wrapping an external buffer failed.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ExternalError {
    /// A dimension, format, or plane description was unusable.
    #[error("invalid external buffer: {reason}")]
    Invalid {
        /// What was wrong.
        reason: &'static str,
    },
    /// An ioctl failed — importing a descriptor, or registering the
    /// framebuffer.
    #[error("external buffer operation failed: {0}")]
    Io(#[from] rustix::io::Errno),
}

/// Up to four planes, as KMS accepts.
const MAX_PLANES: usize = 4;

/// A caller-owned DMA-BUF wrapped as a scanout-ready KMS framebuffer.
///
/// The motivating consumer is a zero-copy capture pipeline — libcamera, V4L2,
/// an accelerator — where the buffer is allocated upstream and reaches the
/// scene as a descriptor plus a shape, rather than as something the scene
/// allocated.
///
/// # Single-use semantics
///
/// One buffer, one cached framebuffer id, returned from every acquire. The
/// release callback fires **exactly once** — from the first release, or from
/// the drop if the source is torn down before ever being released. A caller
/// uses it to re-queue the upstream request that owns the foreign buffer, so
/// firing twice would re-queue a buffer that is still in use and firing never
/// would stall the pipeline.
///
/// # Modifiers are passed through, not validated
///
/// Whatever modifier the caller supplies goes straight to the kernel, which
/// validates it against its driver tables and rejects an unsupported tiling.
/// The kernel is the ground truth, not this type. A caller that wants to know
/// in advance whether a tiled or compressed modifier will land should consult
/// the plane's `IN_FORMATS` or a `TEST_ONLY` commit rather than discover it
/// here.
pub struct ExternalDmaBufSource {
    /// Kept alive so the framebuffer's backing descriptors outlive the caller's.
    _duped_fds: Vec<OwnedFd>,
    fb: Option<framebuffer::Handle>,
    /// The descriptor the framebuffer was registered on, for teardown.
    device_fd: std::os::fd::RawFd,
    format: SourceFormat,
    /// The producer's render-done fence, duplicated on each acquire.
    pending_fence: Option<SyncFence>,
    on_release: Option<Box<dyn FnOnce()>>,
}

impl std::fmt::Debug for ExternalDmaBufSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExternalDmaBufSource")
            .field("format", &self.format)
            .field("fb", &self.fb)
            .field("has_fence", &self.pending_fence.is_some())
            .finish_non_exhaustive()
    }
}

impl ExternalDmaBufSource {
    /// Wrap the given planes as a KMS framebuffer.
    ///
    /// Duplicates each descriptor so the source's lifetime is independent of
    /// the caller's, imports them into GEM handles, and registers a
    /// framebuffer. On any failure the partial state is unwound and the release
    /// callback is **not** fired — the caller still owns the upstream buffer
    /// and can re-queue or drop it themselves.
    ///
    /// Plane-count-versus-format consistency is left to the kernel: `NV12`
    /// wants two planes, `YUV420` three, and the kernel returns `EINVAL` on a
    /// mismatch.
    ///
    /// # Errors
    ///
    /// [`ExternalError::Invalid`] for a zero dimension, a zero format, or a
    /// plane count outside 1..=4; [`ExternalError::Io`] if an ioctl fails.
    pub fn create(
        device: &Device,
        format: SourceFormat,
        planes: &[ExternalPlane<'_>],
        on_release: Option<Box<dyn FnOnce()>>,
    ) -> Result<Self, ExternalError> {
        if format.width == 0 || format.height == 0 {
            return Err(ExternalError::Invalid {
                reason: "width and height must be non-zero",
            });
        }
        if format.fourcc == 0 {
            return Err(ExternalError::Invalid {
                reason: "format must be non-zero",
            });
        }
        if planes.is_empty() || planes.len() > MAX_PLANES {
            return Err(ExternalError::Invalid {
                reason: "expected between 1 and 4 planes",
            });
        }
        if planes.iter().any(|plane| plane.pitch == 0) {
            return Err(ExternalError::Invalid {
                reason: "every plane needs a non-zero pitch",
            });
        }

        let fourcc = drm::buffer::DrmFourcc::try_from(format.fourcc).map_err(|_| {
            ExternalError::Invalid {
                reason: "format is not a recognized DRM FourCC",
            }
        })?;

        // Duplicate first: from here the source owns descriptors of its own and
        // the caller may close theirs immediately.
        let mut duped = Vec::with_capacity(planes.len());
        for plane in planes {
            duped.push(plane.fd.try_clone_to_owned().map_err(|e| {
                ExternalError::Io(
                    rustix::io::Errno::from_io_error(&e).unwrap_or(rustix::io::Errno::MFILE),
                )
            })?);
        }

        let mut handles = [None; MAX_PLANES];
        let mut offsets = [0u32; MAX_PLANES];
        let mut pitches = [0u32; MAX_PLANES];
        for (index, (plane, fd)) in planes.iter().zip(duped.iter()).enumerate() {
            handles[index] = Some(
                device
                    .prime_fd_to_buffer(fd.as_fd())
                    .map_err(|e| ExternalError::Io(errno_of(&e)))?,
            );
            offsets[index] = plane.offset;
            pitches[index] = plane.pitch;
        }

        // A non-trivial modifier must be declared; LINEAR and the INVALID
        // sentinel are passed as "no modifier" so the kernel takes its default
        // path rather than being told the buffer is explicitly linear.
        let trivial = format.modifier == 0 || format.modifier == drmkit_fmt_invalid();
        let layout = ImportedLayout {
            width: format.width,
            height: format.height,
            fourcc,
            modifier: (!trivial).then_some(format.modifier),
            handles,
            offsets,
            pitches,
        };
        let flags = if trivial {
            FbCmd2Flags::empty()
        } else {
            FbCmd2Flags::MODIFIERS
        };

        let fb = device
            .add_planar_framebuffer(&layout, flags)
            .map_err(|e| ExternalError::Io(errno_of(&e)))?;

        Ok(Self {
            _duped_fds: duped,
            fb: Some(fb),
            device_fd: device.raw_fd(),
            format,
            pending_fence: None,
            on_release,
        })
    }

    /// Set the producer's render-done fence for subsequent acquires.
    ///
    /// Replaces any previous one. The buffer's pixels are valid only once it
    /// signals; see [`acquire`](LayerBufferSource::acquire) for why each
    /// acquire hands back a duplicate rather than the fence itself.
    pub fn set_acquire_fence(&mut self, fence: SyncFence) {
        self.pending_fence = Some(fence);
    }

    /// Whether a producer fence is currently set.
    #[must_use]
    pub const fn has_acquire_fence(&self) -> bool {
        self.pending_fence.is_some()
    }

    /// The registered framebuffer id.
    #[must_use]
    pub fn fb_id(&self) -> Option<u32> {
        self.fb.map(Into::into)
    }

    /// Fire the release callback if it has not fired yet.
    fn fire_on_release_once(&mut self) {
        if let Some(callback) = self.on_release.take() {
            callback();
        }
    }
}

impl LayerBufferSource for ExternalDmaBufSource {
    fn acquire(&mut self) -> Result<AcquiredBuffer, SourceError> {
        let fb_id = self
            .fb
            .map(Into::into)
            .ok_or(SourceError::Failed(rustix::io::Errno::INVAL))?;

        // Hand back a **duplicate** of the producer's fence, without clearing
        // it (upstream PR #230).
        //
        // The scene acquires each source twice per frame -- a TEST commit, then
        // the real one. An owned fence handed over on the first acquire would
        // be gone by the second, so the real commit would go out unsynced and
        // the display engine could sample the buffer before the producer's
        // writes land. Duplicating keeps the pending fence live for the second
        // acquire; the next frame's `set_acquire_fence` replaces it, and
        // re-arming an already-signaled fence is harmless because the kernel
        // proceeds immediately.
        //
        // Each duplicate rides its own `AcquiredBuffer` and closes when that is
        // released, so two acquires yield independently closeable descriptors.
        let acquire_fence = self
            .pending_fence
            .as_ref()
            .and_then(SyncFence::as_fd)
            .and_then(|fd| SyncFence::import(fd).ok());

        Ok(AcquiredBuffer {
            fb_id,
            token: 0,
            acquire_fence,
            damage: Vec::new(),
        })
    }

    fn release(&mut self, _acquired: AcquiredBuffer) {
        // Single-use: this source is one upstream request's worth of pixels.
        // The first retire re-queues it upstream; later releases are no-ops, in
        // case the source is still attached when the scene tears down.
        self.fire_on_release_once();
    }

    fn binding_model(&self) -> BindingModel {
        BindingModel::SceneSubmitsFbId
    }

    fn format(&self) -> SourceFormat {
        self.format
    }

    // `map` and `export_dma_buf` keep their defaults for now: the pixels live
    // behind a descriptor this type does not mmap, so it is uncompositable
    // until the export path lands with the compositor that needs it.
}

impl Drop for ExternalDmaBufSource {
    fn drop(&mut self) {
        if let Some(fb) = self.fb.take() {
            // SAFETY: `device_fd` is the descriptor this framebuffer was
            // registered on, borrowed only for this ioctl.
            let borrowed = unsafe { BorrowedFd::borrow_raw(self.device_fd) };
            let device = RawDevice { fd: borrowed };
            let _ = device.destroy_framebuffer(fb);
        }
        // Torn down without ever being released: the upstream buffer still
        // needs re-queuing, or the producer's pipeline stalls waiting for a
        // slot that will never come back.
        self.fire_on_release_once();
    }
}

/// The `DRM_FORMAT_MOD_INVALID` sentinel, which is treated as linear.
const fn drmkit_fmt_invalid() -> u64 {
    // `fourcc_mod_code(NONE, (1 << 56) - 1)`.
    (1 << 56) - 1
}

/// Minimal adapter so `Drop` can issue one ioctl without holding a `&Device`.
struct RawDevice<'a> {
    fd: BorrowedFd<'a>,
}

impl AsFd for RawDevice<'_> {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.fd
    }
}

impl drm::Device for RawDevice<'_> {}
impl drm::control::Device for RawDevice<'_> {}

/// The plane layout handed to `add_planar_framebuffer`.
struct ImportedLayout {
    width: u32,
    height: u32,
    fourcc: drm::buffer::DrmFourcc,
    modifier: Option<u64>,
    handles: [Option<drm::buffer::Handle>; MAX_PLANES],
    offsets: [u32; MAX_PLANES],
    pitches: [u32; MAX_PLANES],
}

impl drm::buffer::PlanarBuffer for ImportedLayout {
    fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    fn format(&self) -> drm::buffer::DrmFourcc {
        self.fourcc
    }

    fn modifier(&self) -> Option<drm::buffer::DrmModifier> {
        self.modifier.map(drm::buffer::DrmModifier::from)
    }

    fn pitches(&self) -> [u32; 4] {
        self.pitches
    }

    fn handles(&self) -> [Option<drm::buffer::Handle>; 4] {
        self.handles
    }

    fn offsets(&self) -> [u32; 4] {
        self.offsets
    }
}

fn errno_of(error: &std::io::Error) -> rustix::io::Errno {
    rustix::io::Errno::from_io_error(error).unwrap_or(rustix::io::Errno::IO)
}
