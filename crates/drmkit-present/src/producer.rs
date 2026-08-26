// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! The seam between a renderer and the scanout path.
//!
//! Port of `src/present/scanout_producer.hpp` and
//! `src/present/gbm_producer.{hpp,cpp}`.

use drmkit_scene::LayerBufferSource;

/// Why a producer could not hand over a buffer.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ProducerError {
    /// The producer could not allocate what was asked for.
    #[error("the producer could not allocate: {0}")]
    Allocate(String),
}

/// Something that allocates the buffers a scanout path presents.
///
/// Two questions, asked once each when a scanout path is built: *what layouts
/// can you produce for this format*, and *give me a buffer in one of these*. The
/// backend intersects the first answer against what the CRTC's planes can scan
/// out, and the intersection is what it passes back in.
///
/// Splitting it this way is what lets a GBM, GL, or Vulkan renderer plug into
/// the same backend: each knows what it can export, none of them needs to know
/// anything about planes.
pub trait ScanoutProducer {
    /// Which modifiers this producer can allocate `fourcc` in and hand to KMS.
    ///
    /// An empty answer means "no opinion" and the backend falls back to
    /// `LINEAR`.
    fn exportable_modifiers(&mut self, fourcc: u32) -> Vec<u64>;

    /// Allocate a buffer and wrap it as a scene source.
    ///
    /// `allowed` is the negotiated intersection: the layouts that both this
    /// producer said it can export and some plane on the CRTC can scan out.
    /// **Honour it.** A buffer allocated outside that set is one no plane will
    /// take, and the refusal arrives at the atomic commit, naming the
    /// framebuffer rather than the layout that made it unscannable.
    ///
    /// An empty `allowed` means no constraint -- the backend reaches it when no
    /// plane exposes `IN_FORMATS` -- not that nothing is acceptable.
    ///
    /// # Errors
    ///
    /// [`ProducerError::Allocate`] if the buffer cannot be produced.
    fn create_buffer(
        &mut self,
        width: u32,
        height: u32,
        fourcc: u32,
        allowed: &[u64],
    ) -> Result<Box<dyn LayerBufferSource>, ProducerError>;
}
