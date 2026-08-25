// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! Scanning a cursor out as an ordinary layer.
//!
//! The usual path drives a cursor plane directly. This one puts the sprite in
//! a dumb buffer and lets the scene place it like anything else, which is what
//! hardware with no cursor-type plane needs -- rockchip's VOP2 among them,
//! where the pointer has to ride an overlay and compositing it into the canvas
//! does not work. Reserve the plane up front and put the layer's destination
//! at `pointer - hotspot`.

use std::time::Instant;

/// `DRM_FORMAT_ARGB8888`. The one format a cursor sprite is in, and the one a
/// dumb buffer can always be allocated as.
const ARGB8888: u32 = drmkit_fmt::fourcc::ARGB8888;

use drmkit_core::Device;
use drmkit_cursor::Cursor;
use drmkit_dumb::{Buffer, Config, DumbError, MapAccess, Mapping};
use drmkit_scene::{
    AcquiredBuffer, BindingModel, DamageRect, LayerBufferSource, SourceError, SourceFormat,
};

/// A cursor presented as a scene layer.
///
/// Static cursors are painted once. An animated one repaints when the frame
/// under the clock changes, and says so as full-frame damage.
#[derive(Debug)]
pub struct CursorSource {
    cursor: Cursor,
    buffer: Buffer,
    format: SourceFormat,
    /// The hotspot of the frame currently painted.
    ///
    /// Per frame rather than per cursor: an animated cursor may move its
    /// hotspot between frames, and a caller positioning by a stale one makes
    /// the pointer drift as the animation plays.
    hotspot: (u32, u32),
    /// When the animation started. Set on the first acquire, so a cursor
    /// created long before it is shown does not begin mid-cycle.
    started: Option<Instant>,
    /// Which frame is in the buffer, so an unchanged one is not repainted.
    painted: Option<usize>,
    /// Reported on the next acquire, then cleared.
    pending_damage: Vec<DamageRect>,
}

impl CursorSource {
    /// Wrap a cursor that is already loaded.
    ///
    /// The buffer is sized to the largest frame, so an animation whose frames
    /// differ in size still has room for all of them, and the first frame is
    /// painted immediately.
    ///
    /// # Errors
    ///
    /// [`DumbError`] if the buffer cannot be allocated or mapped.
    pub fn new(device: &Device, cursor: Cursor) -> Result<Self, DumbError> {
        let width = cursor.frames().iter().map(|frame| frame.width).max();
        let height = cursor.frames().iter().map(|frame| frame.height).max();
        // A cursor with no frames cannot be built -- `Cursor` rejects it -- so
        // these are always present; the fallback keeps the arithmetic total
        // rather than relying on that from another crate.
        let (width, height) = (width.unwrap_or(1).max(1), height.unwrap_or(1).max(1));

        let buffer = Buffer::create(
            device,
            &Config {
                width,
                height,
                fourcc: ARGB8888,
                ..Config::default()
            },
        )?;

        let mut source = Self {
            cursor,
            buffer,
            format: SourceFormat {
                fourcc: ARGB8888,
                // A dumb buffer is linear by construction.
                modifier: 0,
                width,
                height,
            },
            hotspot: (0, 0),
            started: None,
            painted: None,
            pending_damage: Vec::new(),
        };
        source.paint(0);
        Ok(source)
    }

    /// Build a static cursor from raw premultiplied `0xAARRGGBB` pixels.
    ///
    /// The themeless fallback: what a caller uses when there is no cursor
    /// theme on the system, which is most embedded images.
    ///
    /// # Errors
    ///
    /// [`SourceError::Failed`] with `EINVAL` if the pixel count does not match
    /// the dimensions, or whatever the allocation failed with.
    pub fn from_argb(
        device: &Device,
        pixels: &[u32],
        width: u32,
        height: u32,
        xhot: u32,
        yhot: u32,
    ) -> Result<Self, SourceError> {
        let cursor = Cursor::from_argb(pixels, width, height, xhot, yhot)
            .map_err(|_| SourceError::Failed(rustix::io::Errno::INVAL))?;
        Self::new(device, cursor).map_err(|error| SourceError::Failed(errno(&error)))
    }

    /// Where the pointer actually points, in buffer pixels.
    ///
    /// The caller puts the layer's destination at `pointer - hotspot` so the
    /// active point tracks the pointer rather than the sprite's corner.
    #[must_use]
    pub const fn hotspot(&self) -> (u32, u32) {
        self.hotspot
    }

    /// The buffer's size, which is the largest frame's.
    #[must_use]
    pub const fn size(&self) -> (u32, u32) {
        (self.format.width, self.format.height)
    }

    /// The cursor being shown.
    #[must_use]
    pub const fn cursor(&self) -> &Cursor {
        &self.cursor
    }

    /// Paint frame `index` into the buffer, clearing what was there.
    ///
    /// The clear is what makes a smaller frame following a larger one correct:
    /// without it the previous frame's edges stay lit around the new one.
    fn paint(&mut self, index: usize) {
        let frame = self
            .cursor
            .frames()
            .get(index)
            .unwrap_or_else(|| self.cursor.first());
        let (xhot, yhot) = (frame.xhot, frame.yhot);
        let pixels = frame.pixels.clone();
        let (width, height) = (frame.width, frame.height);

        blit(&mut self.buffer, &pixels, width, height, self.format.width);

        self.hotspot = (xhot, yhot);
        self.painted = Some(index);
    }

    /// Which frame the clock is on, and whether it differs from the painted one.
    fn due(&mut self) -> Option<usize> {
        if !self.cursor.is_animated() {
            return None;
        }
        let started = *self.started.get_or_insert_with(Instant::now);
        let index = self.cursor.frame_index_at(started.elapsed());
        (self.painted != Some(index)).then_some(index)
    }
}

/// Copy `pixels` into `buffer`, clearing the rest of it.
fn blit(buffer: &mut Buffer, pixels: &[u32], width: u32, height: u32, buffer_width: u32) {
    let mut mapping = buffer.map(MapAccess::Write);

    // Every row, not just the frame's: a smaller frame following a larger one
    // would otherwise keep the previous frame's edges lit around it.
    for row in 0..mapping.height() {
        let Some(bytes) = mapping.row_mut(row) else {
            break;
        };
        bytes.fill(0);
        if row >= height {
            continue;
        }
        // `row_mut` addresses by the buffer's stride, which is what keeps this
        // correct on a driver that pads its rows -- writing at `width * 4`
        // shears the sprite on every one that does.
        let columns = width.min(buffer_width) as usize;
        let source = &pixels[row as usize * width as usize..][..columns];
        for (column, pixel) in source.iter().enumerate() {
            let at = column * 4;
            bytes[at..at + 4].copy_from_slice(&pixel.to_le_bytes());
        }
    }
}

/// A dumb-buffer failure as an errno.
///
/// The kernel's own answer where there is one. Flattening these to a single
/// value would turn a driver refusing a size into "out of memory", which sends
/// whoever reads the log looking in the wrong place.
fn errno(error: &DumbError) -> rustix::io::Errno {
    match error {
        DumbError::Io(errno) => *errno,
        DumbError::InvalidConfig { .. } | DumbError::UnsupportedFormat { .. } => {
            rustix::io::Errno::INVAL
        }
        DumbError::TooLarge { .. } => rustix::io::Errno::OVERFLOW,
        _ => rustix::io::Errno::INVAL,
    }
}

impl LayerBufferSource for CursorSource {
    fn acquire(&mut self) -> Result<AcquiredBuffer, SourceError> {
        if let Some(index) = self.due() {
            self.paint(index);
            // Full-frame: the paint cleared the buffer, so everything in it
            // changed even where the new frame is transparent.
            self.pending_damage.clear();
        }

        let fb_id = self
            .buffer
            .fb_id()
            .ok_or(SourceError::Failed(rustix::io::Errno::INVAL))?;
        Ok(AcquiredBuffer {
            fb_id,
            token: 0,
            acquire_fence: None,
            damage: std::mem::take(&mut self.pending_damage),
        })
    }

    fn release(&mut self, _acquired: AcquiredBuffer) {
        // One buffer, owned here and handed out again next acquire.
    }

    fn binding_model(&self) -> BindingModel {
        BindingModel::SceneSubmitsFbId
    }

    fn format(&self) -> SourceFormat {
        self.format
    }

    fn map(&mut self, access: MapAccess) -> Result<Mapping<'_>, SourceError> {
        Ok(self.buffer.map(access))
    }

    fn on_session_paused(&mut self) {
        // Nothing outstanding: acquire runs from the scene's commit path,
        // which does not run while paused.
    }

    fn on_session_resumed(&mut self, device: &Device) -> Result<(), SourceError> {
        let (width, height) = (self.format.width, self.format.height);
        // The old descriptor is dead: drop the GEM handle and framebuffer id
        // without issuing ioctls against it, since the kernel reclaimed both
        // when the descriptor closed.
        self.buffer.forget();
        self.buffer = Buffer::create(
            device,
            &Config {
                width,
                height,
                fourcc: self.format.fourcc,
                ..Config::default()
            },
        )
        .map_err(|error| SourceError::Failed(errno(&error)))?;

        // The buffer is new and empty, so whatever was painted is gone.
        let index = self.painted.unwrap_or(0);
        self.painted = None;
        self.paint(index);
        Ok(())
    }
}
