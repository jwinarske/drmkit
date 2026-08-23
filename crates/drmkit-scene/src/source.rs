// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! Where a layer's content comes from.
//!
//! Port of `src/scene/buffer_source.hpp`.
//!
//! # Design invariants
//!
//! Carried from the C++, because the scene depends on all of them:
//!
//! - [`SourceFormat`] describes what the buffer **is** — format, modifier,
//!   intrinsic size. It belongs to the source; the scene never mutates it.
//!   How the buffer is *displayed* — source and destination rectangles,
//!   rotation, alpha, stacking — lives on the layer instead, because those are
//!   plane properties in KMS, not properties of a buffer. Keeping the two apart
//!   mirrors the KMS boundary exactly and makes it natural to show one buffer
//!   several ways.
//!
//! - [`release`](LayerBufferSource::release) is **infallible**. The source owns
//!   everything reachable through an [`AcquiredBuffer`], so releasing cannot
//!   fail by construction. A source whose release path genuinely can fail — a
//!   remote producer, a network-backed surface — must log and leak rather than
//!   propagate: the scene's cleanup paths cannot observe a release failure and
//!   still be simple enough to be correct.
//!
//! - [`bind_to_plane`](LayerBufferSource::bind_to_plane) and
//!   [`unbind_from_plane`](LayerBufferSource::unbind_from_plane) are the
//!   insertion points for [`BindingModel::DriverOwnsBinding`]. The no-op
//!   defaults are correct for every source that submits its own `FB_ID`.

use drmkit_sync::SyncFence;

/// How the scene and a source divide responsibility for `FB_ID`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub enum BindingModel {
    /// The scene writes `FB_ID` into the atomic commit.
    ///
    /// Covers dumb buffers, GBM buffer objects, imported DMA-BUFs, V4L2
    /// capture buffers — everything that becomes a DRM framebuffer.
    #[default]
    SceneSubmitsFbId,

    /// The driver binds a producer directly to the plane, and the scene must
    /// **not** write `FB_ID` for this layer.
    ///
    /// EGL Streams and relatives. The hook exists in the minimal cut as
    /// dormant surface (DR-4) so the scene does not need rewriting when the
    /// streams tier lands; no source in the minimal cut reports it.
    DriverOwnsBinding,
}

/// What a source's buffer intrinsically is.
///
/// Width and height are the buffer's own dimensions, not the on-screen
/// rectangle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SourceFormat {
    /// DRM `FourCC`.
    pub fourcc: u32,
    /// Layout modifier.
    pub modifier: u64,
    /// Buffer width in pixels.
    pub width: u32,
    /// Buffer height in pixels.
    pub height: u32,
}

/// A region of a buffer that changed since the slot was last scanned out, in
/// buffer pixels with a top-left origin.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DamageRect {
    /// Left edge.
    pub x: i32,
    /// Top edge.
    pub y: i32,
    /// Width.
    pub w: u32,
    /// Height.
    pub h: u32,
}

/// A buffer handed over by [`LayerBufferSource::acquire`], valid until the
/// matching [`release`](LayerBufferSource::release).
///
/// Move-only, because it carries an owned fence. That is load-bearing: it means
/// the two-deep release queue **cannot** hand a buffer back early by accident —
/// doing so is a move, and using it afterwards does not compile. Invariant 1's
/// ownership half is enforced by the type; only its *timing* half needs a test.
///
/// Releasing a buffer and then using it is a compile error, which is the whole
/// point:
///
/// ```compile_fail
/// # use drmkit_scene::AcquiredBuffer;
/// fn release(_buffer: AcquiredBuffer) {}
///
/// let buffer = AcquiredBuffer { fb_id: 1, ..AcquiredBuffer::default() };
/// release(buffer);
/// // The buffer is back with its source now; the display engine may already
/// // have stopped scanning it out. Reading it here would be exactly the
/// // use-after-release invariant 1 exists to prevent.
/// assert_eq!(buffer.fb_id, 1);
/// ```
///
/// Ordinary moves are of course fine:
///
/// ```
/// # use drmkit_scene::AcquiredBuffer;
/// let buffer = AcquiredBuffer { fb_id: 1, ..AcquiredBuffer::default() };
/// let moved = buffer;
/// assert_eq!(moved.fb_id, 1);
/// ```
#[derive(Debug, Default)]
pub struct AcquiredBuffer {
    /// The DRM framebuffer the scene will write to the plane's `FB_ID`.
    ///
    /// Non-zero for a [`BindingModel::SceneSubmitsFbId`] source. **Zero** for
    /// [`BindingModel::DriverOwnsBinding`], where the scene skips the write.
    pub fb_id: u32,

    /// A source-private token echoed back verbatim to `release` — typically a
    /// slot index in a ring.
    ///
    /// The C++ carries a `void*` here. A `u64` covers every in-tree use (all of
    /// them store an index) without inviting a raw pointer into a type that
    /// crosses the source boundary.
    pub token: u64,

    /// An optional render-done fence: the pixels are valid only once it
    /// signals.
    ///
    /// Set it when the producer renders asynchronously rather than CPU-blocking
    /// before returning. The scene takes ownership: where the assigned plane
    /// advertises `IN_FENCE_FD` it hands the descriptor to KMS so the kernel
    /// waits before scanout; otherwise it waits on the CPU before committing.
    /// Either way the fence rides this buffer's lifecycle and closes on release
    /// or drop — the kernel does not take ownership of `IN_FENCE_FD`.
    ///
    /// Sources that produce ready buffers synchronously leave this `None`.
    pub acquire_fence: Option<SyncFence>,

    /// Per-frame dirty regions.
    ///
    /// **Empty means full-frame** — the scene emits no `FB_DAMAGE_CLIPS` and
    /// assumes everything changed, which is correct but not power-optimal. A
    /// non-empty list names the changed regions; the scene clamps the count and
    /// falls back to full-frame above its bound, and omits the blob entirely on
    /// planes or drivers without the property.
    pub damage: Vec<DamageRect>,
}

/// Why an [`acquire`](LayerBufferSource::acquire) did not produce a buffer.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SourceError {
    /// **Flow control, not a failure.** The source has no frame to contribute
    /// this vblank.
    ///
    /// The scene skips the layer for this commit and calls `acquire` again on
    /// the next one; the layer is counted in
    /// [`layers_skipped_no_frame`](crate::CommitReport::layers_skipped_no_frame),
    /// never in the failure paths. A live source returns this before its first
    /// sample lands, and any time the producer falls behind with no cached
    /// frame to re-issue.
    ///
    /// This is DR-6: a distinct variant, never
    /// [`io::ErrorKind::WouldBlock`](std::io::ErrorKind::WouldBlock). Squinting
    /// at an I/O error to recover flow control is exactly how the EAGAIN
    /// contract gets lost in a refactor — and losing it turns a stalled
    /// producer into an aborted commit.
    #[error("no frame available this vblank")]
    WouldBlock,

    /// The operation is not supported by this source.
    ///
    /// Returned by the default [`map`](LayerBufferSource::map) and
    /// [`export_dma_buf`](LayerBufferSource::export_dma_buf). A source that
    /// supports neither is **uncompositable**: when the allocator cannot place
    /// it, the scene has no way to rescue it and the layer stays dropped for
    /// the frame.
    #[error("not supported by this source")]
    Unsupported,

    /// A real failure. Aborts the commit.
    #[error("source failed: {0}")]
    Failed(#[from] rustix::io::Errno),
}

/// A borrowed view of a buffer's DMA-BUF planes, for a consumer that imports
/// the buffer directly rather than reading CPU pixels.
///
/// The descriptors are **borrowed**: they stay owned by the source and are
/// valid only until the matching release. The consumer must not close them — an
/// image import references them internally, so no duplication is needed.
#[derive(Debug, Clone)]
pub struct DmaBufDesc<'a> {
    /// One descriptor per plane. A single-descriptor multi-plane format such as
    /// `NV12` repeats the same descriptor with distinct offsets.
    pub fds: Vec<std::os::fd::BorrowedFd<'a>>,
    /// Byte offset of each plane.
    pub offsets: Vec<u32>,
    /// Row stride of each plane.
    pub pitches: Vec<u32>,
    /// Shape, matching [`LayerBufferSource::format`].
    pub format: SourceFormat,
}

/// Where a layer's content comes from.
///
/// Owning a source is owning the DRM handles it holds: dropping a layer drops
/// its source and the buffers with it.
pub trait LayerBufferSource {
    /// Produce this frame's buffer.
    ///
    /// On success the source has arranged that `fb_id` names a ready-to-scan-out
    /// framebuffer. The scene pairs the result with a later
    /// [`release`](Self::release) once the commit completes.
    ///
    /// # Errors
    ///
    /// [`SourceError::WouldBlock`] is **flow control** and expected — see that
    /// variant. Any other error aborts the commit.
    fn acquire(&mut self) -> Result<AcquiredBuffer, SourceError>;

    /// Return a buffer to the free pool.
    ///
    /// Infallible by contract. The scene calls this after page-flip completion,
    /// on commit failure, and during shutdown; there is deliberately no error
    /// channel, so those cleanup paths stay simple enough to be correct.
    fn release(&mut self, acquired: AcquiredBuffer);

    /// Release carrying the `OUT_FENCE` of the commit that displaced this
    /// buffer.
    ///
    /// Once it signals, the buffer is off-screen and safe to render into again,
    /// so a GPU producer can wait on it GPU-side instead of blocking the CPU
    /// until the later release edge. `None` when the CRTC has no
    /// `OUT_FENCE_PTR` or the source did not opt in.
    ///
    /// The default forwards to [`release`](Self::release), so only a source
    /// that wants the fence overrides it — see
    /// [`wants_release_fence`](Self::wants_release_fence).
    fn release_with_fence(&mut self, acquired: AcquiredBuffer, release_fence: Option<SyncFence>) {
        let _ = release_fence;
        self.release(acquired);
    }

    /// Whether this source wants the per-buffer release fence.
    ///
    /// The scene only requests an internal `OUT_FENCE` when some live source
    /// says yes, so this costs nothing otherwise.
    fn wants_release_fence(&self) -> bool {
        false
    }

    /// Whether a producer submitted something new since the last commit.
    ///
    /// Drives the scene's whole-commit skip: when every live source says no and
    /// no layer is dirty, no atomic commit is issued at all — a power win, and
    /// what lets a self-refresh panel stay in self-refresh.
    ///
    /// **The default is `true`, deliberately.** A source that cannot tell
    /// whether its buffer changed — a CPU producer painting into a dumb buffer
    /// the scene cannot introspect — must say "changed", or a real update is
    /// silently skipped. Only a source that *knows* it is idle returns false.
    fn has_fresh_content(&self) -> bool {
        true
    }

    /// Which binding contract this source participates in.
    fn binding_model(&self) -> BindingModel {
        BindingModel::SceneSubmitsFbId
    }

    /// The buffer's shape, read during layer lowering to populate the
    /// properties the allocator consults.
    fn format(&self) -> SourceFormat;

    /// Acquire a CPU view of the pixel storage.
    ///
    /// Composition fallback reads through this to rescue layers the allocator
    /// could not place. A source whose pixels live only in GPU memory returns
    /// [`SourceError::Unsupported`] — and is then uncompositable unless it can
    /// [`export_dma_buf`](Self::export_dma_buf) instead.
    ///
    /// # Errors
    ///
    /// [`SourceError::Unsupported`] by default.
    fn map(
        &mut self,
        access: drmkit_dumb::MapAccess,
    ) -> Result<drmkit_dumb::Mapping<'_>, SourceError> {
        let _ = access;
        Err(SourceError::Unsupported)
    }

    /// Export the currently-acquired buffer's DMA-BUF planes.
    ///
    /// The escape from the map-uncompositable trap: a source whose pixels never
    /// reach CPU memory — a V4L2 capture buffer — can still be composited by
    /// importing its DMA-BUF, so the layer no longer blanks when the allocator
    /// cannot place it.
    ///
    /// # Errors
    ///
    /// [`SourceError::Unsupported`] by default.
    fn export_dma_buf(&mut self) -> Result<DmaBufDesc<'_>, SourceError> {
        Err(SourceError::Unsupported)
    }

    /// Bind a driver-owned producer to a plane.
    ///
    /// Called after the allocator settles on a plane and before the first
    /// acquire for it. The no-op default is correct for every
    /// [`BindingModel::SceneSubmitsFbId`] source.
    ///
    /// # Errors
    ///
    /// Never, by default.
    fn bind_to_plane(&mut self, plane_id: u32) -> Result<(), SourceError> {
        let _ = plane_id;
        Ok(())
    }

    /// Unbind from a plane, when the layer is reassigned or retired.
    fn unbind_from_plane(&mut self, plane_id: u32) {
        let _ = plane_id;
    }

    /// The session is losing DRM master; issue no ioctls until resumed.
    fn on_session_paused(&mut self) {}

    /// The session is back with a fresh device.
    ///
    /// A source holding descriptor-bound state must drop every handle tied to
    /// the dead descriptor — ioctls against it will fail — and re-allocate
    /// equivalent buffers. Dimensions and formats are preserved: callers rely
    /// on [`format`](Self::format) returning the same value afterwards.
    ///
    /// # Errors
    ///
    /// Whatever re-allocation failed with.
    fn on_session_resumed(&mut self, device: &drmkit_core::Device) -> Result<(), SourceError> {
        let _ = device;
        Ok(())
    }
}
