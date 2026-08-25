// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! A loaded cursor: pixels, hotspot, and animation timing.
//!
//! Pure CPU. No descriptors, no KMS. Rasterizing one onto a plane is somebody
//! else's job; this holds the frames and answers which of them is showing.

use std::time::Duration;

use crate::CursorError;
use crate::theme::{Theme, ThemeResolution};
use crate::xcursor::{self, Limits};

/// Substituted when a caller asks for size zero.
///
/// Zero is a foot-gun: libxcursor reads it as 24 px, which is tiny against a
/// real cursor plane -- 64 px on i915, 256 px on amdgpu. 64 is the smallest
/// commonly supported plane size, so the sprite reaches the buffer's edges
/// instead of floating in the middle of it.
const DEFAULT_SIZE: u32 = 64;

/// One frame of a cursor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    /// Premultiplied `0xAARRGGBB`, row-major, `width * height` of them.
    pub pixels: Vec<u32>,
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// Where the pointer actually points, in pixels from the left edge.
    pub xhot: u32,
    /// Where the pointer actually points, in pixels from the top edge.
    pub yhot: u32,
    /// How long this frame shows. Zero for a static cursor.
    pub delay: Duration,
}

/// A loaded cursor, immutable once built.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cursor {
    frames: Vec<Frame>,
    cycle: Duration,
}

impl Cursor {
    /// Load from a resolved theme entry at the nearest available size.
    ///
    /// Cursor files are multi-resolution packs, so this picks the nominal size
    /// closest to `requested_size` and takes every frame filed under it. A
    /// `requested_size` of zero means [`DEFAULT_SIZE`].
    ///
    /// # Errors
    ///
    /// - [`CursorError::Io`] if the file cannot be read.
    /// - [`CursorError::Malformed`] if it is not a cursor file, is truncated,
    ///   or contradicts itself.
    /// - [`CursorError::TooLarge`] if it declares more frames or larger frames
    ///   than the loader accepts.
    /// - [`CursorError::NoImages`] if it parses but holds no images.
    pub fn load(resolved: &ThemeResolution, requested_size: u32) -> Result<Self, CursorError> {
        let bytes = std::fs::read(&resolved.source).map_err(|e| CursorError::Io(e.to_string()))?;
        Self::from_bytes(&bytes, requested_size)
    }

    /// Resolve a name against `theme` and load it.
    ///
    /// # Errors
    ///
    /// As [`Theme::resolve`] and [`Cursor::load`].
    pub fn load_named(
        theme: &Theme,
        cursor_name: &str,
        preferred_theme: &str,
        requested_size: u32,
    ) -> Result<Self, CursorError> {
        let resolved = theme.resolve(cursor_name, preferred_theme)?;
        Self::load(&resolved, requested_size)
    }

    /// Load from the bytes of a cursor file.
    ///
    /// The seam the fuzz target drives, so it exercises the same bounds the
    /// real loader does.
    ///
    /// # Errors
    ///
    /// As [`Cursor::load`], less the I/O.
    pub fn from_bytes(bytes: &[u8], requested_size: u32) -> Result<Self, CursorError> {
        let size = if requested_size == 0 {
            DEFAULT_SIZE
        } else {
            requested_size
        };

        let images = xcursor::parse(bytes, Limits::default())?;
        let chosen = xcursor::nearest_size(&images, size).ok_or(CursorError::NoImages)?;

        let frames: Vec<Frame> = images
            .into_iter()
            .filter(|image| image.nominal_size == chosen)
            .map(|image| Frame {
                pixels: image.pixels,
                width: image.width,
                height: image.height,
                xhot: image.xhot,
                yhot: image.yhot,
                delay: Duration::from_millis(u64::from(image.delay_ms)),
            })
            .collect();

        Self::assemble(frames)
    }

    /// Build a static cursor from caller-supplied pixels.
    ///
    /// The in-process fallback for when no cursor theme is installed. Pixels
    /// are premultiplied `0xAARRGGBB`, row-major, and are copied.
    ///
    /// # Errors
    ///
    /// [`CursorError::InvalidImage`] for a zero or oversize dimension, or a
    /// pixel count that does not match the dimensions. A caller bug, surfaced
    /// here rather than as a surprise at render time.
    pub fn from_argb(
        pixels: &[u32],
        width: u32,
        height: u32,
        xhot: u32,
        yhot: u32,
    ) -> Result<Self, CursorError> {
        let limits = Limits::default();
        if width == 0 || height == 0 || width > limits.max_dim || height > limits.max_dim {
            return Err(CursorError::InvalidImage);
        }
        if pixels.len() != width as usize * height as usize {
            return Err(CursorError::InvalidImage);
        }
        Self::assemble(vec![Frame {
            pixels: pixels.to_vec(),
            width,
            height,
            xhot,
            yhot,
            delay: Duration::ZERO,
        }])
    }

    /// Build a cursor from frames the caller already has.
    ///
    /// The many-frame form of [`Cursor::from_argb`], for a caller generating
    /// cursors rather than loading them -- a compositor drawing its own
    /// spinner, or a test. Frames play in the order given.
    ///
    /// # Errors
    ///
    /// - [`CursorError::NoImages`] for an empty set.
    /// - [`CursorError::InvalidImage`] if any frame's pixel count disagrees
    ///   with its dimensions, or a dimension is zero or past the limit.
    pub fn from_frames(frames: Vec<Frame>) -> Result<Self, CursorError> {
        let limits = Limits::default();
        for frame in &frames {
            if frame.width == 0
                || frame.height == 0
                || frame.width > limits.max_dim
                || frame.height > limits.max_dim
            {
                return Err(CursorError::InvalidImage);
            }
            if frame.pixels.len() != frame.width as usize * frame.height as usize {
                return Err(CursorError::InvalidImage);
            }
        }
        if frames.len() > limits.max_frames {
            return Err(CursorError::TooLarge);
        }
        Self::assemble(frames)
    }

    /// Assemble from frames, computing the cycle.
    fn assemble(frames: Vec<Frame>) -> Result<Self, CursorError> {
        if frames.is_empty() {
            return Err(CursorError::NoImages);
        }
        let cycle = frames.iter().map(|frame| frame.delay).sum();
        Ok(Self { frames, cycle })
    }

    /// The first frame, which is what a static cursor shows.
    #[must_use]
    pub fn first(&self) -> &Frame {
        // `from_frames` refuses an empty set, so there is always one.
        &self.frames[0]
    }

    /// Every frame, in play order.
    #[must_use]
    pub fn frames(&self) -> &[Frame] {
        &self.frames
    }

    /// How long one full loop takes. Zero for a static cursor.
    #[must_use]
    pub const fn cycle(&self) -> Duration {
        self.cycle
    }

    /// Whether this cursor actually animates.
    ///
    /// More than one frame is not enough: some themes ship multi-frame packs
    /// with a zero delay on every frame, which would otherwise loop instantly
    /// and flicker.
    #[must_use]
    pub fn is_animated(&self) -> bool {
        self.frames.len() > 1 && !self.cycle.is_zero()
    }

    /// The frame showing at `elapsed` since the cursor started displaying.
    ///
    /// Deterministic and total: a static cursor, or one whose frames all
    /// declare no delay, always answers with the first frame.
    #[must_use]
    pub fn frame_at(&self, elapsed: Duration) -> &Frame {
        let index = self.frame_index_at(elapsed);
        self.frames.get(index).unwrap_or_else(|| self.first())
    }

    /// Which frame is showing at `elapsed`.
    ///
    /// The index rather than the frame, so a caller redrawing into a buffer
    /// can tell that nothing has changed and skip the work. A cursor ticking
    /// at 60Hz against a 100ms animation redraws on three frames in eighteen.
    #[must_use]
    pub fn frame_index_at(&self, elapsed: Duration) -> usize {
        if !self.is_animated() {
            return 0;
        }
        // Both are non-zero here, so the remainder is well defined.
        let into_cycle = Duration::from_nanos(
            u64::try_from(elapsed.as_nanos() % self.cycle.as_nanos()).unwrap_or(0),
        );

        let mut accumulated = Duration::ZERO;
        for (index, frame) in self.frames.iter().enumerate() {
            accumulated += frame.delay;
            if into_cycle < accumulated {
                return index;
            }
        }
        // Unreachable while the delays sum to the cycle, but the last frame is
        // the right answer if they ever stop agreeing.
        self.frames.len().saturating_sub(1)
    }
}
