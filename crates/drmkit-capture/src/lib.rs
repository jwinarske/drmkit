// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! Read back what a CRTC is scanning out, and write it to a file.
//!
//! [`snapshot`] composites the planes bound to a CRTC into an [`Image`] the
//! size of its active mode, reading each plane's buffer back through a
//! dma-buf. [`write_png`] puts one on disk.
//!
//! This is a readback path, not a rendering one: it reads buffers that already
//! exist and blends them on the CPU exactly as the display controller would.
//! What it cannot read -- tiled buffers, YUV, anything not host-mappable -- it
//! skips with a warning rather than failing the capture, because a screenshot
//! missing its video overlay is more use than no screenshot.

mod image;
mod png;
mod snapshot;

#[cfg(test)]
mod tests;

pub use image::{Image, src_over, unpremultiply};
pub use png::write_png;
pub use snapshot::snapshot;

/// What can go wrong capturing or writing a frame.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum CaptureError {
    /// The CRTC is not driving a mode, so there is no size to capture at.
    #[error("the CRTC is not driving a mode")]
    NoMode,

    /// No plane on the CRTC could be read.
    ///
    /// Either the CRTC really is blank, or every plane on it was skipped --
    /// see [`snapshot`] for what gets skipped, and note that a client which
    /// has not enabled the atomic cap cannot see plane geometry at all.
    #[error("no plane on the CRTC could be read back")]
    NothingReadable,

    /// The image has no pixels.
    #[error("the image is empty")]
    EmptyImage,

    /// The kernel or the filesystem refused.
    #[error("io: {0}")]
    Io(#[source] std::io::Error),
}
