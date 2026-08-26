// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! A single-buffer source backed by a GBM allocation.
//!
//! Port of `src/scene/gbm_buffer_source.{hpp,cpp}`.

use drmkit_core::Device;
use drmkit_gbm::{GbmBuffer, GbmDevice};
use drmkit_scene::{
    AcquiredBuffer, BindingModel, DmaBufDesc, LayerBufferSource, SourceError, SourceFormat,
};

use crate::{ExternalDmaBufSource, ExternalError, ExternalPlane};

/// One GPU-allocated buffer, created once and scanned out every frame.
///
/// The GBM counterpart of [`DumbBufferSource`](crate::DumbBufferSource), and
/// the source a `ScanoutProducer` hands to the scene: the producer renders
/// into it and the display engine reads it directly, with no copy in between.
///
/// The buffer reaches KMS as a DMA-BUF import rather than through its GEM
/// handle. That is not a detour -- it is the path that carries the modifier,
/// so a tiled or compressed allocation arrives at `add_planar_framebuffer`
/// described as what it actually is. It also means this reuses the import that
/// [`ExternalDmaBufSource`] already implements and the vkms lane already
/// covers, rather than a second copy of it.
///
/// **Single-buffered**, like its dumb sibling: a producer racing scanout can
/// tear against the display engine reading it. A ring is the answer where that
/// matters; this is for a producer that repaints and presents in step, which
/// is what `drmkit-present`'s scanout backend drives.
#[derive(Debug)]
pub struct GbmBufferSource {
    /// The allocation. Kept because it owns the memory the import refers to:
    /// the imported framebuffer holds its own reference, but dropping the
    /// buffer object here would still be dropping the producer's render
    /// target out from under it.
    buffer: GbmBuffer,
    imported: ExternalDmaBufSource,
    /// The exported DMA-BUF, kept rather than dropped after the import.
    ///
    /// The import takes its own reference, so holding this is not what keeps
    /// the framebuffer alive. It is what makes the source *compositable*:
    /// `export_dma_buf` hands out a borrow of it, and a source that dropped
    /// the descriptor would have nothing to offer -- see that method.
    dma_buf: std::os::fd::OwnedFd,
    /// The exported buffer's row stride, reported alongside the descriptor.
    pitch: u32,
    format: SourceFormat,
    /// Held so the source can re-allocate on session resume, where the old
    /// descriptor is dead and everything derived from it has to be rebuilt.
    gbm: GbmDevice,
}

/// Why a GBM-backed source could not be built.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum GbmSourceError {
    /// The allocation failed.
    #[error("allocating a GBM buffer: {0}")]
    Allocate(#[from] drmkit_gbm::GbmError),

    /// The allocation succeeded but could not be imported as a framebuffer.
    #[error("importing the GBM buffer: {0}")]
    Import(#[from] ExternalError),

    /// The driver returned a multi-planar allocation.
    ///
    /// Refused rather than importing the first plane and calling it the
    /// buffer, which would scan out one channel of a planar YUV frame as if it
    /// were the whole image.
    #[error("the driver allocated {planes} planes; only single-plane buffers are supported")]
    MultiPlanar {
        /// How many planes the allocation has.
        planes: u32,
    },
}

impl GbmBufferSource {
    /// Allocate a scanout buffer and import it as a framebuffer.
    ///
    /// `modifiers` constrains the layout the driver may pick -- pass what the
    /// planes can actually scan out, which is what
    /// `PlaneRegistry::candidate_modifiers` in `drmkit-planes` answers. An
    /// empty slice is *no constraint*, not *nothing is acceptable*: it is
    /// the answer on a driver that exposes no `IN_FORMATS`, and refusing there
    /// would turn "does not advertise layouts" into "cannot allocate".
    ///
    /// The modifier is read back from the allocation rather than assumed to be
    /// what was asked for, because a driver without the constrained entry
    /// point falls back to an unconstrained allocation.
    ///
    /// # Errors
    ///
    /// [`GbmSourceError::Allocate`] if the driver refuses the format, size, or
    /// every modifier offered; [`GbmSourceError::Import`] if the result cannot
    /// be made into a framebuffer; [`GbmSourceError::MultiPlanar`] if the
    /// driver returns more than one plane.
    pub fn create(
        device: &Device,
        width: u32,
        height: u32,
        fourcc: u32,
        modifiers: &[u64],
    ) -> Result<Self, GbmSourceError> {
        let gbm = GbmDevice::new(device)?;
        let buffer = GbmBuffer::create_with_modifiers(&gbm, width, height, fourcc, modifiers)?;
        let (imported, dma_buf, format) = Self::import(device, &buffer)?;

        Ok(Self {
            pitch: buffer.stride(),
            buffer,
            imported,
            dma_buf,
            format,
            gbm,
        })
    }

    /// Export `buffer` as a DMA-BUF and register it as a framebuffer.
    fn import(
        device: &Device,
        buffer: &GbmBuffer,
    ) -> Result<(ExternalDmaBufSource, std::os::fd::OwnedFd, SourceFormat), GbmSourceError> {
        let planes = buffer.plane_count();
        if planes != 1 {
            return Err(GbmSourceError::MultiPlanar { planes });
        }

        let format = SourceFormat {
            fourcc: buffer.fourcc(),
            // Read back, not assumed: the constrained allocation falls back to
            // an unconstrained one where the driver has no modifier entry
            // point, and the framebuffer must describe the layout that exists.
            modifier: buffer.modifier(),
            width: buffer.width(),
            height: buffer.height(),
        };

        let dma_buf = buffer.export()?;
        let source = ExternalDmaBufSource::create(
            device,
            format,
            &[ExternalPlane {
                fd: std::os::fd::AsFd::as_fd(&dma_buf),
                offset: 0,
                pitch: buffer.stride(),
            }],
            None,
        )?;

        Ok((source, dma_buf, format))
    }

    /// The allocation, for a caller that needs to render into it.
    #[must_use]
    pub const fn buffer(&self) -> &GbmBuffer {
        &self.buffer
    }

    /// The registered framebuffer id.
    #[must_use]
    pub fn fb_id(&self) -> Option<u32> {
        self.imported.fb_id()
    }
}

impl LayerBufferSource for GbmBufferSource {
    fn acquire(&mut self) -> Result<AcquiredBuffer, SourceError> {
        self.imported.acquire()
    }

    fn release(&mut self, acquired: AcquiredBuffer) {
        self.imported.release(acquired);
    }

    fn binding_model(&self) -> BindingModel {
        BindingModel::SceneSubmitsFbId
    }

    fn format(&self) -> SourceFormat {
        self.format
    }

    /// Composition reads this buffer through its DMA-BUF, not a CPU map.
    ///
    /// A GBM allocation may be tiled or in device-local memory, so there is no
    /// meaningful CPU view to hand back -- `map` stays `Unsupported`. Without
    /// this the layer would be uncompositable and would blank whenever the
    /// allocator could not place it on a plane, which is exactly the case
    /// composition exists to rescue.
    ///
    /// Not forwarded to the wrapped import, which does not implement this: it
    /// consumes the descriptor it is handed and keeps only the framebuffer.
    /// This source keeps its own, so it has one to lend.
    fn export_dma_buf(&mut self) -> Result<DmaBufDesc<'_>, SourceError> {
        Ok(DmaBufDesc {
            fds: vec![std::os::fd::AsFd::as_fd(&self.dma_buf)],
            offsets: vec![0],
            pitches: vec![self.pitch],
            format: self.format,
        })
    }

    fn on_session_paused(&mut self) {
        self.imported.on_session_paused();
    }

    fn on_session_resumed(&mut self, device: &Device) -> Result<(), SourceError> {
        // Everything here is descriptor-bound: the GBM device wraps the dead
        // fd, the buffer was allocated against it, and the framebuffer id was
        // registered on it. Re-allocating is the only correct answer -- and the
        // shape is preserved, because callers rely on `format()` reporting the
        // same value afterwards.
        let SourceFormat {
            fourcc,
            modifier,
            width,
            height,
        } = self.format;

        let gbm = GbmDevice::new(device).map_err(|_| SourceError::Unsupported)?;
        let buffer = GbmBuffer::create_with_modifiers(&gbm, width, height, fourcc, &[modifier])
            .map_err(|_| SourceError::Unsupported)?;
        let (imported, dma_buf, format) =
            Self::import(device, &buffer).map_err(|_| SourceError::Unsupported)?;

        self.gbm = gbm;
        self.pitch = buffer.stride();
        self.buffer = buffer;
        self.imported = imported;
        self.dma_buf = dma_buf;
        self.format = format;
        Ok(())
    }
}
