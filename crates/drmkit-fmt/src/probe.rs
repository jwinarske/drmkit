// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! Memoized atomic `TEST_ONLY` verdicts.

use alloc::vec::Vec;

use crate::modifier::Modifier;

/// The cached outcome of probing a `(crtc, plane, fourcc, modifier)` tuple with
/// an atomic `TEST_ONLY` commit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Verdict {
    /// Never probed.
    #[default]
    Unknown,
    /// The commit accepted it.
    Ok,
    /// The commit rejected it.
    Rejected,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Key {
    crtc: u32,
    plane: u32,
    fourcc: u32,
    modifier: Modifier,
}

/// Memoizes atomic `TEST_ONLY` verdicts.
///
/// An `IN_FORMATS` that advertises a modifier the hardware cannot actually
/// program — historically some pre-6.16 `MediaTek` OVL instances — then costs one
/// probe rather than one glitch per frame.
///
/// Backed by a linear scan: the entry count is bounded by
/// `crtcs * planes * formats` actually exercised, which is small, and a scan
/// beats hashing at this size.
///
/// ```
/// use drmkit_fmt::{Modifier, ModifierProbeCache, Verdict, fourcc};
///
/// let mut cache = ModifierProbeCache::new();
/// assert_eq!(cache.lookup(1, 31, fourcc::XRGB8888, Modifier::LINEAR), Verdict::Unknown);
///
/// cache.record(1, 31, fourcc::XRGB8888, Modifier::LINEAR, true);
/// assert_eq!(cache.lookup(1, 31, fourcc::XRGB8888, Modifier::LINEAR), Verdict::Ok);
///
/// // A modeset or hotplug invalidates everything cached for that plane.
/// cache.invalidate_plane(31);
/// assert_eq!(cache.lookup(1, 31, fourcc::XRGB8888, Modifier::LINEAR), Verdict::Unknown);
/// ```
#[derive(Debug, Clone, Default)]
pub struct ModifierProbeCache {
    entries: Vec<(Key, bool)>,
}

impl ModifierProbeCache {
    /// An empty cache.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    /// The cached verdict, or [`Verdict::Unknown`].
    #[must_use]
    pub fn lookup(&self, crtc: u32, plane: u32, fourcc: u32, modifier: Modifier) -> Verdict {
        let key = Key {
            crtc,
            plane,
            fourcc,
            modifier,
        };
        self.entries
            .iter()
            .find(|(k, _)| *k == key)
            .map_or(Verdict::Unknown, |(_, ok)| {
                if *ok { Verdict::Ok } else { Verdict::Rejected }
            })
    }

    /// Record a probe outcome, replacing any previous verdict for the tuple.
    pub fn record(&mut self, crtc: u32, plane: u32, fourcc: u32, modifier: Modifier, ok: bool) {
        let key = Key {
            crtc,
            plane,
            fourcc,
            modifier,
        };
        if let Some(entry) = self.entries.iter_mut().find(|(k, _)| *k == key) {
            entry.1 = ok;
        } else {
            self.entries.push((key, ok));
        }
    }

    /// Drop every verdict for a plane. Call on hotplug or modeset, after which
    /// a previously rejected modifier may well be accepted.
    pub fn invalidate_plane(&mut self, plane: u32) {
        self.entries.retain(|(k, _)| k.plane != plane);
    }

    /// Number of cached verdicts.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether nothing is cached.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}
