// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! A source wrapping a caller-owned, externally allocated DMA-BUF.
//!
//! Port of `src/scene/external_dma_buf_source.{hpp,cpp}`.

use std::os::fd::{AsFd, BorrowedFd, OwnedFd};

use drm::control::{Device as ControlDevice, FbCmd2Flags, framebuffer};
use drmkit_core::Device;
use drmkit_scene::{
    AcquiredBuffer, BindingModel, DmaBufDesc, LayerBufferSource, SourceError, SourceFormat,
};
use drmkit_sync::SyncFence;

/// One plane of an externally allocated buffer.
///
/// `fd` is caller-owned — the source duplicates it, so the caller may close
/// theirs the moment the constructor returns.
#[derive(Debug, Clone, Copy)]
#[allow(
    clippy::exhaustive_structs,
    reason = "a value defined by its fields; there is nothing to add to it"
)]
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
    /// An ioctl or syscall failed — duplicating or importing a descriptor, or
    /// registering the framebuffer.
    #[error("external buffer operation failed: {0}")]
    Io(#[from] rustix::io::Errno),
    /// The key was [`retire`](crate::ExternalDmaBufPool::retire)d and its old
    /// import is not torn down yet, so the frame was not taken. Try again once
    /// a later frame has displaced it.
    #[error("buffer key retired and not yet torn down")]
    Retired,
}

/// Up to four planes, as KMS accepts.
const MAX_PLANES: usize = 4;

/// A DMA-BUF imported into the kernel and registered as a framebuffer.
///
/// Split out of [`ExternalDmaBufSource`] because a ring needs one of these per
/// slot: the import, its duplicated descriptors and the framebuffer's teardown
/// are the same work whether one buffer is presented or eight are rotated.
pub(crate) struct ImportedFramebuffer {
    /// Kept alive so the framebuffer's backing descriptors outlive the
    /// caller's — the caller may close theirs the moment this returns.
    ///
    /// Lent back out by [`dma_buf_planes`](Self::dma_buf_planes), which is what
    /// makes an imported layer compositable: these sources expose no CPU
    /// mapping, so the descriptors are the only view of the pixels there is.
    duped_fds: Vec<OwnedFd>,
    /// Per-plane byte offset and row stride, as imported.
    offsets: Vec<u32>,
    pitches: Vec<u32>,
    fb: Option<framebuffer::Handle>,
    /// The GEM handles the import made, closed after the framebuffer.
    ///
    /// Each holds a reference to its dma-buf, so one left open pins the
    /// buffer until the device descriptor closes, however long after the
    /// producer freed it.
    handles: Vec<drm::buffer::Handle>,
    /// The descriptor the framebuffer was registered on, for teardown.
    device_fd: std::os::fd::RawFd,
}

impl ImportedFramebuffer {
    /// The imported planes: their descriptors, offsets, and row strides.
    ///
    /// Empty for an import that has been taken (`into_imported`) or never
    /// made, which is what a caller checks before offering a descriptor.
    pub(crate) fn dma_buf_planes(&self) -> (&[OwnedFd], &[u32], &[u32]) {
        (&self.duped_fds, &self.offsets, &self.pitches)
    }

    /// Import `planes` and register a framebuffer over them.
    ///
    /// # Errors
    ///
    /// [`ExternalError`] if a descriptor cannot be duplicated or imported, or
    /// the kernel refuses the framebuffer.
    pub(crate) fn import(
        device: &Device,
        format: SourceFormat,
        planes: &[ExternalPlane<'_>],
    ) -> Result<Self, ExternalError> {
        let source = ExternalDmaBufSource::create(device, format, planes, None)?;
        Ok(source.into_imported())
    }

    /// The registered framebuffer id, or `None` once forgotten.
    pub(crate) fn fb_id(&self) -> Option<u32> {
        self.fb.map(Into::into)
    }

    /// Abandon the framebuffer and its GEM handles without issuing an ioctl.
    ///
    /// For a revoked descriptor, which cannot service the teardown and took
    /// both with it. Leaves `fb_id` reporting `None`, so nothing commits an id
    /// the kernel has forgotten.
    pub(crate) fn forget(&mut self) {
        self.fb = None;
        self.handles.clear();
    }

    /// Import the kept descriptors again on `device`, after a session resume.
    ///
    /// The old framebuffer and handles went with the old descriptor, so they
    /// are forgotten rather than torn down -- an ioctl on that descriptor
    /// would at best fail and at worst land on whatever reused its number.
    /// The buffer itself is unchanged; only its import is new. On failure
    /// nothing is left imported, and `fb_id` reports `None`.
    ///
    /// # Errors
    ///
    /// [`ExternalError`] if the format is not a recognized DRM format, a
    /// descriptor cannot be imported, or the kernel refuses the framebuffer.
    pub(crate) fn reimport(
        &mut self,
        device: &Device,
        format: SourceFormat,
    ) -> Result<(), ExternalError> {
        self.forget();
        if self.duped_fds.is_empty() {
            return Ok(());
        }
        let fourcc = drm::buffer::DrmFourcc::try_from(format.fourcc).map_err(|_| {
            ExternalError::Invalid {
                reason: "format is not a recognized DRM FourCC",
            }
        })?;
        let (fb, handles) = import_planes(
            device,
            format,
            fourcc,
            &self.duped_fds,
            &self.offsets,
            &self.pitches,
        )?;
        self.fb = Some(fb);
        self.handles = handles;
        self.device_fd = device.raw_fd();
        Ok(())
    }

    /// Tear the import down on its own descriptor, as a drop would, keeping
    /// the descriptors for another try.
    pub(crate) fn release(&mut self) {
        // SAFETY: as in `Drop`: the descriptor these were made on, borrowed
        // only for these ioctls.
        let borrowed = unsafe { BorrowedFd::borrow_raw(self.device_fd) };
        let device = RawDevice { fd: borrowed };
        if let Some(fb) = self.fb.take() {
            let _ = device.destroy_framebuffer(fb);
        }
        close_handles(&device, &mut self.handles);
    }
}

impl Drop for ImportedFramebuffer {
    fn drop(&mut self) {
        if self.fb.is_none() && self.handles.is_empty() {
            return;
        }
        // SAFETY: `device_fd` is the descriptor this framebuffer was
        // registered on, borrowed only for these ioctls.
        let borrowed = unsafe { BorrowedFd::borrow_raw(self.device_fd) };
        let device = RawDevice { fd: borrowed };
        // The framebuffer first, then the handles, as the reference's
        // `teardown_slot` does.
        if let Some(fb) = self.fb.take() {
            let _ = device.destroy_framebuffer(fb);
        }
        close_handles(&device, &mut self.handles);
    }
}

/// Close each distinct GEM handle in `handles`, leaving it empty.
///
/// Planes that share a dma-buf import to the same handle, and the kernel
/// takes a handle back once: closing it per plane, as the reference does, only
/// adds refused ioctls.
fn close_handles(device: &impl drm::control::Device, handles: &mut Vec<drm::buffer::Handle>) {
    handles.sort_unstable_by_key(|handle| u32::from(*handle));
    handles.dedup();
    for handle in handles.drain(..) {
        let _ = device.close_buffer(handle);
    }
}

/// Import `fds` into GEM handles on `device` and register a framebuffer over
/// them: the kernel half of an import, shared by a first import and a
/// re-import after a session resume.
///
/// On failure every handle imported so far is closed again, as the
/// reference's `import_slot` unwinds.
fn import_planes(
    device: &Device,
    format: SourceFormat,
    fourcc: drm::buffer::DrmFourcc,
    fds: &[OwnedFd],
    offsets: &[u32],
    pitches: &[u32],
) -> Result<(framebuffer::Handle, Vec<drm::buffer::Handle>), ExternalError> {
    let mut layout_handles = [None; MAX_PLANES];
    let mut layout_offsets = [0u32; MAX_PLANES];
    let mut layout_pitches = [0u32; MAX_PLANES];
    let mut imported = Vec::with_capacity(fds.len());
    for (index, fd) in fds.iter().enumerate() {
        let handle = match device.prime_fd_to_buffer(fd.as_fd()) {
            Ok(handle) => handle,
            Err(e) => {
                close_handles(device, &mut imported);
                return Err(ExternalError::Io(errno_of(&e)));
            }
        };
        imported.push(handle);
        layout_handles[index] = Some(handle);
        layout_offsets[index] = offsets[index];
        layout_pitches[index] = pitches[index];
    }

    // Which path the modifier takes is the device's to decide; see
    // `drmkit_core::framebuffer_modifier`.
    let declared = drmkit_core::framebuffer_modifier(device, format.modifier);
    let layout = ImportedLayout {
        width: format.width,
        height: format.height,
        fourcc,
        modifier: declared,
        handles: layout_handles,
        offsets: layout_offsets,
        pitches: layout_pitches,
    };
    let flags = if declared.is_some() {
        FbCmd2Flags::MODIFIERS
    } else {
        FbCmd2Flags::empty()
    };

    match device.add_planar_framebuffer(&layout, flags) {
        Ok(fb) => Ok((fb, imported)),
        Err(e) => {
            close_handles(device, &mut imported);
            Err(ExternalError::Io(errno_of(&e)))
        }
    }
}

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
    imported: ImportedFramebuffer,
    format: SourceFormat,
    /// The producer's render-done fence, duplicated on each acquire.
    pending_fence: Option<SyncFence>,
    on_release: Option<Box<dyn FnOnce()>>,
}

impl std::fmt::Debug for ExternalDmaBufSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExternalDmaBufSource")
            .field("format", &self.format)
            .field("fb", &self.imported.fb_id())
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
    /// [`ExternalError::Invalid`] for a zero dimension, a zero or unrecognized
    /// format, a plane count outside 1..=4, or a zero pitch;
    /// [`ExternalError::Io`] if a descriptor cannot be duplicated or an ioctl
    /// fails.
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

        let offsets: Vec<u32> = planes.iter().map(|plane| plane.offset).collect();
        let pitches: Vec<u32> = planes.iter().map(|plane| plane.pitch).collect();
        let (fb, imported) = import_planes(device, format, fourcc, &duped, &offsets, &pitches)?;

        Ok(Self {
            imported: ImportedFramebuffer {
                duped_fds: duped,
                offsets,
                pitches,
                fb: Some(fb),
                handles: imported,
                device_fd: device.raw_fd(),
            },
            format,
            pending_fence: None,
            on_release,
        })
    }

    /// Set the producer's render-done fence for subsequent acquires.
    ///
    /// Replaces any previous one. The buffer's pixels are valid only once it
    /// signals. Each acquire hands back a duplicate rather than the fence
    /// itself, because the scene acquires twice per frame -- a TEST commit,
    /// then the real one -- and a fence moved out on the first would leave the
    /// real commit unsynced.
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
        self.imported.fb_id()
    }

    /// Take the imported framebuffer, leaving the source inert.
    ///
    /// Used by the ring, which wants the import without the single-shot
    /// release semantics wrapped around it.
    fn into_imported(mut self) -> ImportedFramebuffer {
        // The callback must not fire: nothing was ever presented, and the
        // import is being kept rather than torn down.
        self.on_release = None;
        std::mem::replace(
            &mut self.imported,
            ImportedFramebuffer {
                duped_fds: Vec::new(),
                offsets: Vec::new(),
                pitches: Vec::new(),
                fb: None,
                handles: Vec::new(),
                device_fd: -1,
            },
        )
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
            .imported
            .fb_id()
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

    /// Lend the imported descriptors back, so the layer can be composited.
    ///
    /// A foreign buffer exposes no CPU mapping, so `map` is correctly
    /// `Unsupported` -- and that alone would make this layer *uncompositable*:
    /// it would blank whenever the allocator could not find it a plane, which
    /// is the case composition exists to rescue. The descriptors are the only
    /// view of these pixels there is, so lending them is what keeps the layer
    /// recoverable. Upstream's `ExternalDmaBufSource` does the same from its
    /// per-plane `duped_fd`.
    ///
    /// # Errors
    ///
    /// [`SourceError::Unsupported`] once the import has been taken by a ring,
    /// which leaves nothing to lend.
    fn export_dma_buf(&mut self) -> Result<DmaBufDesc<'_>, SourceError> {
        let (fds, offsets, pitches) = self.imported.dma_buf_planes();
        if fds.is_empty() {
            return Err(SourceError::Unsupported);
        }
        Ok(DmaBufDesc {
            fds: fds.iter().map(std::os::fd::AsFd::as_fd).collect(),
            offsets: offsets.to_vec(),
            pitches: pitches.to_vec(),
            format: self.format,
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

    // `map` keeps its default: the pixels live behind a descriptor this type
    // does not mmap, and composition reaches them through `export_dma_buf`.

    /// Re-import the buffer on the new descriptor, as the reference does.
    ///
    /// The buffer is the producer's and survives the pause; its framebuffer
    /// and GEM handles do not. Without this, every acquire after a resume
    /// would hand out a framebuffer id the new descriptor has never heard of.
    fn on_session_resumed(&mut self, device: &Device) -> Result<(), SourceError> {
        self.imported
            .reimport(device, self.format)
            .map_err(|error| source_error(&error))
    }
}

/// An import failure as the scene reports it.
pub(crate) fn source_error(error: &ExternalError) -> SourceError {
    match error {
        ExternalError::Io(errno) => SourceError::Failed(*errno),
        ExternalError::Invalid { .. } => SourceError::Failed(rustix::io::Errno::INVAL),
        // Only a pool's submit returns this, never an import.
        ExternalError::Retired => SourceError::WouldBlock,
    }
}

impl ExternalDmaBufSource {
    /// Abandon the import without an ioctl: for an owner replacing this
    /// source after a resume, whose descriptor took the import with it.
    #[cfg(feature = "gbm")]
    pub(crate) fn forget(&mut self) {
        self.imported.forget();
    }
}

impl Drop for ExternalDmaBufSource {
    fn drop(&mut self) {
        // The framebuffer goes with `imported`'s own drop.
        //
        // Torn down without ever being released: the upstream buffer still
        // needs re-queuing, or the producer's pipeline stalls waiting for a
        // slot that will never come back.
        self.fire_on_release_once();
    }
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
