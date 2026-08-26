// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! Choosing a modifier both the producer and the display can live with.

use drmkit_fmt::{BandwidthClass, FormatTable, Modifier, Rotation, classify, rotation_compatible};

/// Rank key: compression, then tiling, then linear.
const fn class_rank(class: BandwidthClass) -> u8 {
    match class {
        BandwidthClass::Compression => 0,
        BandwidthClass::Tiling => 1,
        BandwidthClass::Linear => 2,
    }
}

/// Intersect what a producer can export with what a plane can scan out, and
/// rank the survivors.
///
/// Returns the candidates most-preferred first, or empty when the two sets do
/// not overlap. Duplicates in `producer` are collapsed.
///
/// # What the ranking means
///
/// Compression first, then tiling, then linear — because that is the order of
/// how much memory bandwidth each costs to scan out, and bandwidth is what
/// runs out first on the parts this library targets. Within a class the
/// producer's own order is preserved, since a producer that lists two tiled
/// layouts has a reason for the order it listed them in and this has no better
/// information.
///
/// # What it does not do
///
/// It does not decide anything. A modifier surviving here means only that
/// nothing *known* rules it out: the plane advertises it, the producer can
/// export it, and the rotation does not need a fetch order the layout cannot
/// provide. Bandwidth limits, scaler counts and per-driver rules are not
/// advertised anywhere, so the `TEST_ONLY` commit remains the arbiter and this
/// is a pre-filter that keeps the number of proposals small.
///
/// # Rotation
///
/// Explicit, where the reference defaults it to `Rotate0`. A caller that has
/// not thought about rotation is exactly the caller who should: a 90-degree
/// layer on a linear buffer is refused at commit, and the refusal names
/// neither the rotation nor the modifier.
#[must_use]
pub fn negotiate(producer: &[Modifier], plane: &[Modifier], rotation: Rotation) -> Vec<Modifier> {
    let mut out: Vec<Modifier> = Vec::new();
    for &m in producer {
        if !rotation_compatible(m, rotation) {
            continue;
        }
        if !plane.contains(&m) {
            continue;
        }
        if !out.contains(&m) {
            out.push(m);
        }
    }
    // Stable, so producer preference survives within a bandwidth class.
    out.sort_by_key(|m| class_rank(classify(*m)));
    out
}

/// As [`negotiate`], taking the plane's modifiers from a [`FormatTable`] for
/// one `fourcc`.
///
/// The table is what an `IN_FORMATS` blob parses into, so this is the form a
/// caller holding a real plane reaches for.
#[must_use]
pub fn negotiate_for_format(
    producer: &[Modifier],
    plane: &FormatTable,
    fourcc: u32,
    rotation: Rotation,
) -> Vec<Modifier> {
    let plane: Vec<Modifier> = plane.modifiers_for(fourcc).collect();
    negotiate(producer, &plane, rotation)
}
