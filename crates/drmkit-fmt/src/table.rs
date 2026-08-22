// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! `FormatTable`: a parsed, queryable view of a plane's `IN_FORMATS` blob.
//!
//! Port of the `FormatTable` half of `src/fmt/format_mod.hpp`.
//!
//! # Design contract (carried verbatim from the C++)
//!
//! - `IN_FORMATS` prunes the bipartite graph — **necessary, not sufficient**.
//! - The atomic `TEST_ONLY` commit is ground truth for what a plane can scan
//!   out.
//! - [`BandwidthClass`](crate::`BandwidthClass`) is a cost input to placement
//!   scoring, never a correctness gate.
//!
//! # Deviation: no `from_plane`
//!
//! The C++ `FormatTable::from_plane(fd, plane_id)` reads the blob off a DRM fd.
//! Here the crate stays `no_std + alloc` and format-only: `drmkit-core`'s
//! property layer fetches the blob bytes and hands them to [`FormatTable::from_blob`].
//! Nothing is lost — the C++ already split the parser out for exactly this
//! testability reason — and the format logic stays usable without a device.

use alloc::vec::Vec;

use crate::modifier::Modifier;

/// A `(fourcc, modifier)` pair a plane accepts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FormatMod {
    /// The `FourCC` pixel format.
    pub fourcc: u32,
    /// The layout modifier.
    pub modifier: Modifier,
}

/// Byte layout of `struct drm_format_modifier_blob` (`drm_mode.h`).
const BLOB_HEADER_LEN: usize = 24;
/// Byte layout of `struct drm_format_modifier` (`drm_mode.h`): `formats` (u64),
/// `offset` (u32), `pad` (u32), `modifier` (u64).
const BLOB_MODIFIER_LEN: usize = 24;

/// Parsed header fields we actually use.
struct BlobHeader {
    count_formats: u32,
    formats_offset: u32,
    count_modifiers: u32,
    modifiers_offset: u32,
}

/// Read a little-endian `u32` at `offset`, or `None` if it would run past the
/// end.
fn read_u32(data: &[u8], offset: usize) -> Option<u32> {
    let bytes: &[u8; 4] = data.get(offset..offset.checked_add(4)?)?.try_into().ok()?;
    Some(u32::from_le_bytes(*bytes))
}

/// Read a little-endian `u64` at `offset`, or `None` if it would run past the
/// end.
fn read_u64(data: &[u8], offset: usize) -> Option<u64> {
    let bytes: &[u8; 8] = data.get(offset..offset.checked_add(8)?)?.try_into().ok()?;
    Some(u64::from_le_bytes(*bytes))
}

impl BlobHeader {
    fn parse(data: &[u8]) -> Option<Self> {
        if data.len() < BLOB_HEADER_LEN {
            return None;
        }
        // version (u32) and flags (u32) occupy 0..8 and are not consulted:
        // the kernel has only ever emitted version 1, and the C++ ignores them
        // too. Reading them would invite a version gate the kernel does not
        // honor.
        Some(Self {
            count_formats: read_u32(data, 8)?,
            formats_offset: read_u32(data, 12)?,
            count_modifiers: read_u32(data, 16)?,
            modifiers_offset: read_u32(data, 20)?,
        })
    }
}

/// A plane's accepted `(fourcc, modifier)` pairs, sorted and de-duplicated.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FormatTable {
    /// Sorted by `(fourcc, modifier)`; backs [`FormatTable::all`] and
    /// [`FormatTable::supports`].
    pairs: Vec<FormatMod>,
}

impl FormatTable {
    /// An empty table, accepting nothing.
    #[must_use]
    pub const fn new() -> Self {
        Self { pairs: Vec::new() }
    }

    /// Parse a raw `IN_FORMATS` blob payload.
    ///
    /// Malformed input yields an empty (or partial) table rather than an error:
    /// a plane whose blob does not parse is simply a plane nothing is eligible
    /// for, and the caller's fallback ("`formats[]` implies LINEAR only") is
    /// the same either way. This matches the C++, which returns a default-
    /// constructed table on every guard.
    ///
    /// # Robustness
    ///
    /// The blob's arrays may begin at offsets that are not naturally aligned.
    /// In C++ that made indexing a typed pointer into the blob undefined
    /// behavior, and it faulted on strict-alignment targets — the C++ fix was
    /// to `memcpy` each element out. Here every field is read byte-wise from a
    /// slice with a bounds-checked accessor, so alignment cannot arise as a
    /// concern at all. Counts are validated against the remaining length before
    /// use, in division form, so no `count * size` product can overflow.
    ///
    /// ```
    /// use drmkit_fmt::FormatTable;
    /// // Too short to hold a header: empty table, no panic.
    /// assert!(FormatTable::from_blob(&[0u8; 4]).all().is_empty());
    /// assert!(FormatTable::from_blob(&[]).all().is_empty());
    /// ```
    #[must_use]
    pub fn from_blob(data: &[u8]) -> Self {
        let mut table = Self::new();

        let Some(header) = BlobHeader::parse(data) else {
            return table;
        };

        let size = data.len();
        let formats_offset = header.formats_offset as usize;
        let modifiers_offset = header.modifiers_offset as usize;

        // Bounds-check both arrays before touching them. Written in division
        // form so the implied count * element-size products cannot overflow and
        // silently bypass the check.
        if formats_offset > size
            || modifiers_offset > size
            || header.count_formats as usize > (size - formats_offset) / 4
            || header.count_modifiers as usize > (size - modifiers_offset) / BLOB_MODIFIER_LEN
        {
            return table;
        }

        for i in 0..header.count_modifiers as usize {
            let record = modifiers_offset + i * BLOB_MODIFIER_LEN;
            let (Some(formats_bitmask), Some(offset), Some(modifier)) = (
                read_u64(data, record),
                read_u32(data, record + 8),
                read_u64(data, record + 16),
            ) else {
                continue;
            };

            // Each set bit in the mask names one format, at `offset + bit` in
            // the format array.
            for bit in 0..64u32 {
                if (formats_bitmask >> bit) & 1 == 0 {
                    continue;
                }
                let Some(index) = offset.checked_add(bit) else {
                    continue; // malformed blob guard
                };
                if index >= header.count_formats {
                    continue; // malformed blob guard
                }
                let Some(fourcc) = read_u32(data, formats_offset + index as usize * 4) else {
                    continue;
                };
                table.pairs.push(FormatMod {
                    fourcc,
                    modifier: Modifier(modifier),
                });
            }
        }

        table.build_index();
        table
    }

    /// Build a table from already-decoded pairs — for a caller that harvested
    /// `IN_FORMATS` by other means and wants the queryable table without
    /// re-reading the blob.
    ///
    /// Duplicates are removed; input order does not matter.
    ///
    /// ```
    /// use drmkit_fmt::{FormatTable, Modifier, fourcc};
    /// let table = FormatTable::from_pairs([
    ///     (fourcc::XRGB8888, Modifier::LINEAR),
    ///     (fourcc::XRGB8888, Modifier::LINEAR), // duplicate
    ///     (fourcc::NV12, Modifier::LINEAR),
    /// ]);
    /// assert_eq!(table.all().len(), 2);
    /// ```
    pub fn from_pairs<I>(pairs: I) -> Self
    where
        I: IntoIterator<Item = (u32, Modifier)>,
    {
        let mut table = Self {
            pairs: pairs
                .into_iter()
                .map(|(fourcc, modifier)| FormatMod { fourcc, modifier })
                .collect(),
        };
        table.build_index();
        table
    }

    /// Sort and de-duplicate. Some kernels list a pair twice.
    fn build_index(&mut self) {
        self.pairs.sort_unstable();
        self.pairs.dedup();
    }

    /// Whether the plane advertises this exact `(fourcc, modifier)` pair.
    ///
    /// This is exact-match only. Family-aware eligibility — which is what the
    /// allocator wants — is [`crate::layer_fits_plane`].
    #[must_use]
    pub fn supports(&self, fourcc: u32, modifier: Modifier) -> bool {
        self.pairs
            .binary_search(&FormatMod { fourcc, modifier })
            .is_ok()
    }

    /// Every modifier advertised for `fourcc`, ascending.
    pub fn modifiers_for(&self, fourcc: u32) -> impl Iterator<Item = Modifier> + '_ {
        let start = self.pairs.partition_point(|p| p.fourcc < fourcc);
        self.pairs[start..]
            .iter()
            .take_while(move |p| p.fourcc == fourcc)
            .map(|p| p.modifier)
    }

    /// Every advertised pair, sorted by `(fourcc, modifier)`.
    #[must_use]
    pub fn all(&self) -> &[FormatMod] {
        &self.pairs
    }

    /// Whether the table advertises nothing.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.pairs.is_empty()
    }

    /// Number of advertised pairs.
    #[must_use]
    pub fn len(&self) -> usize {
        self.pairs.len()
    }
}

/// Edge eligibility for the allocator's bipartite pre-solve: an `IN_FORMATS`
/// check with family awareness.
///
/// `LINEAR` and the legacy `INVALID` sentinel are interchangeable — a layer
/// tagged either matches a plane entry of either — mirroring the
/// pre-`FormatTable` `supports_format_modifier` semantics.
///
/// A parameterized modifier ([`ModifierFamily`](crate::`ModifierFamily`)) also
/// matches a plane advertising its family base; see that type for why the set
/// of such families is deliberately small. Every other modifier must match
/// exactly.
///
/// This prunes the graph. The atomic `TEST_ONLY` commit downstream remains the
/// ground truth.
///
/// ```
/// use drmkit_fmt::{FormatTable, Modifier, fourcc, layer_fits_plane, sand128_col_height};
///
/// // A vc4 plane advertises SAND128 with column height 0.
/// let plane = FormatTable::from_pairs([(fourcc::NV12, sand128_col_height(0))]);
///
/// // rpi-hevc-dec hands over SAND128 with the coded height baked in.
/// let produced = sand128_col_height(1080);
///
/// assert!(!plane.supports(fourcc::NV12, produced), "not an exact match");
/// assert!(layer_fits_plane(&plane, fourcc::NV12, produced), "but it is eligible");
/// ```
#[must_use]
pub fn layer_fits_plane(plane_formats: &FormatTable, fourcc: u32, m: Modifier) -> bool {
    if plane_formats.supports(fourcc, m) {
        return true;
    }
    if m.is_linear() || m.is_invalid() {
        return plane_formats.supports(fourcc, Modifier::LINEAR)
            || plane_formats.supports(fourcc, Modifier::INVALID);
    }
    if let Some(base) = m.family().base() {
        return plane_formats.supports(fourcc, base);
    }
    false
}
