// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! Scoped CPU views over a buffer's linear pixel storage.
//!
//! Port of `src/buffer_mapping.hpp`. Home of **invariant 6**.

/// Intent of a CPU mapping.
///
/// Backends that distinguish read-only from read-write traffic (GBM, through
/// `gbm_bo_map`'s transfer flags) honor this; dumb buffers ignore it. Picking
/// the narrowest mode that fits is always correct: on a backend that stages,
/// a read-only mapping needs no flush back on unmap.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
#[repr(u8)]
pub enum MapAccess {
    /// Read only.
    Read = 0,
    /// Write only.
    Write = 1,
    /// Read and write.
    #[default]
    ReadWrite = 2,
}

/// A borrowed view over a dumb buffer's linear pixel storage.
///
/// # Invariant 6: a zero-cost view of a lifetime mmap
///
/// A dumb buffer is mmap'd once at creation and the mapping is held for the
/// buffer's whole life, cache-coherent by kernel construction. Acquiring this
/// view therefore costs nothing — no ioctl, no syscall — and **dropping it
/// costs nothing either**.
///
/// In C++ that is a documented promise: `BufferMapping` carries a function
/// pointer that happens to be null for dumb buffers, and a comment explains
/// that the destructor is a no-op. Here it is structural. This type has no
/// [`Drop`] implementation at all, so there is no destructor to become
/// non-trivial by accident — adding one would be a visible, reviewable change
/// rather than a silent regression in per-frame cost.
///
/// The borrow is equally load-bearing: the view cannot outlive the buffer it
/// came from, so the use-after-free the C++ warns about ("holding a mapping
/// past the buffer's destructor") does not compile.
///
/// The lifetime-mmap decision matters beyond convenience — `DumbScanoutSink`'s
/// pacing depends on `map()` being free per frame — so it is pinned rather
/// than merely documented.
#[derive(Debug)]
pub struct Mapping<'a> {
    pixels: &'a mut [u8],
    stride: u32,
    width: u32,
    height: u32,
    access: MapAccess,
}

impl<'a> Mapping<'a> {
    pub(crate) fn new(
        pixels: &'a mut [u8],
        stride: u32,
        width: u32,
        height: u32,
        access: MapAccess,
    ) -> Self {
        Self {
            pixels,
            stride,
            width,
            height,
            access,
        }
    }

    /// Whether the view covers no storage.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.pixels.is_empty()
    }

    /// The linear pixel storage, read only.
    ///
    /// The first row starts at index 0; subsequent rows are [`stride`](Self::stride)
    /// bytes apart.
    #[must_use]
    pub const fn pixels(&self) -> &[u8] {
        self.pixels
    }

    /// The linear pixel storage, mutable.
    #[must_use]
    pub const fn pixels_mut(&mut self) -> &mut [u8] {
        self.pixels
    }

    /// Bytes per row.
    ///
    /// The kernel may pad this above `width * bytes_per_pixel` — amdgpu's DC
    /// applies a 256-byte rule, for one. Honor it rather than recomputing.
    #[must_use]
    pub const fn stride(&self) -> u32 {
        self.stride
    }

    /// Buffer width in pixels.
    #[must_use]
    pub const fn width(&self) -> u32 {
        self.width
    }

    /// Buffer height in pixels.
    #[must_use]
    pub const fn height(&self) -> u32 {
        self.height
    }

    /// The access mode this view was acquired with.
    ///
    /// Recorded but unused at the dumb-buffer level; a backend maintaining
    /// dirty state across map cycles can switch on it.
    #[must_use]
    pub const fn access(&self) -> MapAccess {
        self.access
    }

    /// One row, read only, or `None` if `row` is past the bottom.
    #[must_use]
    pub fn row(&self, row: u32) -> Option<&[u8]> {
        let (start, end) = self.row_range(row)?;
        self.pixels.get(start..end)
    }

    /// One row, mutable, or `None` if `row` is past the bottom.
    #[must_use]
    pub fn row_mut(&mut self, row: u32) -> Option<&mut [u8]> {
        let (start, end) = self.row_range(row)?;
        self.pixels.get_mut(start..end)
    }

    fn row_range(&self, row: u32) -> Option<(usize, usize)> {
        if row >= self.height {
            return None;
        }
        let stride = self.stride as usize;
        let start = (row as usize).checked_mul(stride)?;
        Some((start, start.checked_add(stride)?))
    }
}
