// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! The layer model: what the scene asks the allocator to place.

use std::cell::Cell;
use std::hash::{Hash as _, Hasher as _};

use crate::prop::{NUM_PROPS, PropClass, PropTag};

/// What a layer is showing, for placement priority.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub enum ContentType {
    /// Unclassified.
    #[default]
    Generic,
    /// Video, which benefits most from a dedicated plane.
    Video,
    /// Interface chrome.
    Ui,
    /// A cursor.
    Cursor,
}

/// An axis-aligned rectangle in CRTC pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Rect {
    /// Left edge.
    pub x: i32,
    /// Top edge.
    pub y: i32,
    /// Width.
    pub w: u32,
    /// Height.
    pub h: u32,
}

/// A snapshot of a layer's property state, for diffing against the last commit.
///
/// Trivially copyable, as in the C++: copying is a memcpy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PropertySnapshot {
    values: [u64; NUM_PROPS],
    set_mask: u16,
}

impl PropertySnapshot {
    /// The value of a property, if it was set when the snapshot was taken.
    #[must_use]
    pub const fn get(&self, tag: PropTag) -> Option<u64> {
        if self.set_mask & (1 << tag.index()) == 0 {
            None
        } else {
            Some(self.values[tag.index()])
        }
    }
}

/// One layer the scene wants on screen.
///
/// A layer is a property bag plus the flags the allocator consults when
/// deciding whether — and where — to place it.
///
/// The flags stay separate `bool`s rather than a bitfield: they are set and
/// read independently by different owners — the caller, the scene, and the
/// allocator — and the C++ keeps them separate for the same reason. Packing
/// them would save a few bytes on a type allocated a handful of times per
/// frame and cost the ability to name each one.
#[expect(
    clippy::struct_excessive_bools,
    reason = "independently-owned placement flags; see the type documentation"
)]
#[derive(Debug, Clone)]
pub struct Layer {
    values: [u64; NUM_PROPS],
    /// One bit per [`PropTag`]; 16 tags fit a `u16` exactly.
    set_mask: u16,
    /// Memoized [`Layer::property_hash`], invalidated on every mutation.
    cached_hash: Cell<Option<u64>>,
    force_composited: bool,
    transient_composited: bool,
    needs_composition: bool,
    dirty: bool,
    composition_output: bool,
    externally_bound: bool,
    pinned: bool,
    assigned_plane: Option<u32>,
    content_type: ContentType,
    update_hz: u32,
    app_priority: u8,
}

impl Default for Layer {
    fn default() -> Self {
        Self::new()
    }
}

impl Layer {
    /// A layer with no properties set.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            values: [0; NUM_PROPS],
            set_mask: 0,
            cached_hash: Cell::new(None),
            force_composited: false,
            transient_composited: false,
            needs_composition: false,
            dirty: true,
            composition_output: false,
            externally_bound: false,
            pinned: false,
            assigned_plane: None,
            content_type: ContentType::Generic,
            update_hz: 0,
            app_priority: 0,
        }
    }

    /// Set a property. The hot path — no string lookup.
    ///
    /// Writing a property's existing value is a **no-op**: the layer is not
    /// marked dirty and the memoized hash is not invalidated. That is not just
    /// tidiness — the steady-state path rewrites the same geometry every frame,
    /// so invalidating on a no-op would recompute the hash each time and defeat
    /// the memoization the FB-only fast path leans on.
    pub fn set_property(&mut self, tag: PropTag, value: u64) -> &mut Self {
        let index = tag.index();
        let was_set = self.set_mask & (1 << index) != 0;
        let changed = !was_set || self.values[index] != value;

        self.values[index] = value;
        self.set_mask |= 1 << index;

        if changed {
            self.cached_hash.set(None);
            self.dirty = true;
        }
        self
    }

    /// Set a property by canonical KMS name.
    ///
    /// An unrecognized name is dropped and reported, rather than silently
    /// ignored as in the C++: the kernel would reject it anyway, and a caller
    /// that mistyped a name wants to know.
    pub fn set_property_by_name(&mut self, name: &str, value: u64) -> bool {
        match PropTag::from_name(name) {
            Some(tag) => {
                self.set_property(tag, value);
                true
            }
            None => false,
        }
    }

    /// The value of a property, if set.
    #[must_use]
    pub const fn property(&self, tag: PropTag) -> Option<u64> {
        if self.set_mask & (1 << tag.index()) == 0 {
            None
        } else {
            Some(self.values[tag.index()])
        }
    }

    /// The value of a property by canonical KMS name, if set.
    #[must_use]
    pub fn property_by_name(&self, name: &str) -> Option<u64> {
        PropTag::from_name(name).and_then(|tag| self.property(tag))
    }

    /// Every set property, in tag order.
    pub fn properties(&self) -> impl Iterator<Item = (PropTag, u64)> + '_ {
        PropTag::ALL
            .into_iter()
            .filter_map(|tag| self.property(tag).map(|value| (tag, value)))
    }

    /// How many properties are set.
    #[must_use]
    pub const fn property_count(&self) -> u32 {
        self.set_mask.count_ones()
    }

    /// Snapshot the property state for diffing.
    #[must_use]
    pub const fn snapshot(&self) -> PropertySnapshot {
        PropertySnapshot {
            values: self.values,
            set_mask: self.set_mask,
        }
    }

    /// Disable the layer by detaching its framebuffer.
    ///
    /// Sets `FB_ID` to 0 and leaves everything else alone. Geometry, format,
    /// and stacking stay put, which is what lets a re-enabled layer keep its
    /// placement — and, because `FB_ID` is a content property, what keeps a
    /// disable from moving [`property_hash`](Self::property_hash).
    pub fn disable(&mut self) -> &mut Self {
        self.set_property(PropTag::FbId, 0)
    }

    /// Order-independent hash of the layer's **placement** properties.
    ///
    /// # Invariant 4 (corollary)
    ///
    /// This is what isolates content from placement. Every
    /// [`PropClass::Placement`] property moves the hash; every
    /// [`PropClass::Content`] property is skipped, because it changes each
    /// frame and says nothing about whether the plane can scan the buffer out.
    ///
    /// A frame whose hash is unchanged has only new content on already-placed
    /// layers, so the allocator reuses the cached assignment and skips the
    /// redundant `TEST_ONLY`. Misclassifying a placement property as content
    /// would let a real geometry change skip validation — which is why the
    /// classification lives in [`PropTag::class`]'s exhaustive `match` rather
    /// than in an `if` here.
    ///
    /// Memoized; invalidated by every mutation.
    #[must_use]
    pub fn property_hash(&self) -> u64 {
        if let Some(cached) = self.cached_hash.get() {
            return cached;
        }

        // Walking in tag order makes the result independent of the order the
        // properties were set in.
        let mut hash = GOLDEN_RATIO;
        for tag in PropTag::ALL {
            if tag.class() == PropClass::Content {
                continue;
            }
            let Some(value) = self.property(tag) else {
                continue;
            };
            hash = combine(hash, tag.index() as u64);
            hash = combine(hash, value);
        }

        self.cached_hash.set(Some(hash));
        hash
    }

    // --- placement flags -----------------------------------------------------

    /// Force this layer through composition rather than a plane.
    pub fn set_force_composited(&mut self, composited: bool) -> &mut Self {
        self.force_composited = composited;
        self
    }

    /// Whether the caller pinned this layer to composition.
    #[must_use]
    pub const fn is_force_composited(&self) -> bool {
        self.force_composited
    }

    /// Set the per-commit composition flag.
    ///
    /// The allocator treats this exactly like
    /// [`set_force_composited`](Self::set_force_composited), but the scene sets
    /// and clears it per commit rather than at layer creation. It lets
    /// scene-level constraints — an EGL Streams `Exclusive` mixing mode forcing
    /// FB-backed layers through composition when a stream layer shares the
    /// CRTC — coexist with a caller's own choice without conflating the two.
    pub fn set_transient_composited(&mut self, composited: bool) -> &mut Self {
        self.transient_composited = composited;
        self
    }

    /// Whether the scene set the per-commit composition flag.
    #[must_use]
    pub const fn is_transient_composited(&self) -> bool {
        self.transient_composited
    }

    /// Whether this layer must go through composition for any reason.
    #[must_use]
    pub const fn needs_composition(&self) -> bool {
        self.needs_composition || self.force_composited || self.transient_composited
    }

    #[expect(
        dead_code,
        reason = "set by the allocator, which lands in the next commit of this \
                  phase; expect rather than allow so the attribute has to be \
                  removed once it is"
    )]
    pub(crate) const fn set_needs_composition(&mut self, needs: bool) {
        self.needs_composition = needs;
    }

    /// Mark this layer as the output of the composition pass itself.
    pub fn set_composition_layer(&mut self, is_composition: bool) -> &mut Self {
        self.composition_output = is_composition;
        self
    }

    /// Whether this layer is the composition pass's own output.
    #[must_use]
    pub const fn is_composition_layer(&self) -> bool {
        self.composition_output
    }

    /// Whether the scene pinned this layer to a plane whose binding a driver
    /// owns.
    ///
    /// The allocator must skip such a layer during assignment, scoring, and
    /// test commits, exactly as it skips a composition layer. EGL stream
    /// sources establish their plane state out of band, so an allocator test
    /// commit would reject a stream-bound plane for lacking the kernel-internal
    /// `FB_ID` those extensions set up.
    #[must_use]
    pub const fn is_externally_bound(&self) -> bool {
        self.externally_bound
    }

    /// Mark this layer as driver-bound. Set by the scene when a source reports
    /// `DriverOwnsBinding`.
    pub fn set_externally_bound(&mut self, externally_bound: bool) -> &mut Self {
        self.externally_bound = externally_bound;
        self
    }

    /// Whether the scene pinned this layer to a caller-chosen plane.
    ///
    /// Like [`is_externally_bound`](Self::is_externally_bound), the allocator
    /// skips it. **Unlike** that flag, the layer keeps its `FB_ID` in the
    /// property bag — it is a normal source, not a driver-owned one — so this
    /// must not gate the `FB_ID` write.
    #[must_use]
    pub const fn is_pinned(&self) -> bool {
        self.pinned
    }

    /// Mark this layer as pinned to a specific plane.
    pub fn set_pinned(&mut self, pinned: bool) -> &mut Self {
        self.pinned = pinned;
        self
    }

    /// The plane this layer was placed on.
    #[must_use]
    pub const fn assigned_plane(&self) -> Option<u32> {
        self.assigned_plane
    }

    /// Record the plane this layer was placed on.
    pub fn set_assigned_plane(&mut self, plane_id: Option<u32>) -> &mut Self {
        self.assigned_plane = plane_id;
        self
    }

    // --- hints ---------------------------------------------------------------

    /// What this layer is showing.
    #[must_use]
    pub const fn content_type(&self) -> ContentType {
        self.content_type
    }

    /// Set what this layer is showing.
    pub fn set_content_type(&mut self, content_type: ContentType) -> &mut Self {
        self.content_type = content_type;
        self
    }

    /// The layer's expected update rate in Hz, or 0 if unknown.
    #[must_use]
    pub const fn update_hz(&self) -> u32 {
        self.update_hz
    }

    /// Set the expected update rate.
    pub fn set_update_hint(&mut self, hz: u32) -> &mut Self {
        self.update_hz = hz;
        self
    }

    /// Application-level placement priority within a content class.
    #[must_use]
    pub const fn app_priority(&self) -> u8 {
        self.app_priority
    }

    /// Set the application-level placement priority.
    pub fn set_app_priority(&mut self, priority: u8) -> &mut Self {
        self.app_priority = priority;
        self
    }

    /// Whether the layer changed since the last [`mark_clean`](Self::mark_clean).
    #[must_use]
    pub const fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// Clear the dirty flag.
    pub const fn mark_clean(&mut self) {
        self.dirty = false;
    }

    // --- derived geometry ----------------------------------------------------

    /// The `FourCC`, if set.
    ///
    /// The narrowing casts in this block are the KMS ABI's shape: `CRTC_*`,
    /// `pixel_format`, and the integer part of a 16.16 `SRC_*` are all 32-bit
    /// fields carried in the property bag's `u64` slots. A value too wide to
    /// fit came from a caller writing something the kernel would reject.
    #[must_use]
    #[expect(
        clippy::cast_possible_truncation,
        reason = "KMS carries these as 32-bit fields in u64 property slots"
    )]
    pub const fn format(&self) -> Option<u32> {
        match self.property(PropTag::PixelFormat) {
            Some(value) => Some(value as u32),
            None => None,
        }
    }

    /// The layout modifier, defaulting to `LINEAR`'s zero when unset.
    #[must_use]
    pub const fn modifier(&self) -> u64 {
        match self.property(PropTag::FbModifier) {
            Some(value) => value,
            None => 0,
        }
    }

    /// The rotate/reflect bitmask, or 0.
    #[must_use]
    pub const fn rotation(&self) -> u64 {
        match self.property(PropTag::Rotation) {
            Some(value) => value,
            None => 0,
        }
    }

    /// Destination width in pixels.
    #[must_use]
    #[expect(
        clippy::cast_possible_truncation,
        reason = "KMS carries these as 32-bit fields in u64 property slots"
    )]
    pub const fn width(&self) -> u32 {
        match self.property(PropTag::CrtcW) {
            Some(value) => value as u32,
            None => 0,
        }
    }

    /// Destination height in pixels.
    #[must_use]
    #[expect(
        clippy::cast_possible_truncation,
        reason = "KMS carries these as 32-bit fields in u64 property slots"
    )]
    pub const fn height(&self) -> u32 {
        match self.property(PropTag::CrtcH) {
            Some(value) => value as u32,
            None => 0,
        }
    }

    /// The destination rectangle.
    #[must_use]
    #[expect(
        clippy::cast_possible_truncation,
        reason = "KMS carries CRTC_X/Y as signed 32-bit fields in u64 property slots"
    )]
    pub const fn crtc_rect(&self) -> Rect {
        let x = match self.property(PropTag::CrtcX) {
            Some(value) => value as i32,
            None => 0,
        };
        let y = match self.property(PropTag::CrtcY) {
            Some(value) => value as i32,
            None => 0,
        };
        Rect {
            x,
            y,
            w: self.width(),
            h: self.height(),
        }
    }

    /// Whether the plane would have to scale.
    ///
    /// `SRC_*` are 16.16 fixed point; `CRTC_*` are whole pixels. A layer
    /// missing any of the four is treated as unscaled, matching the C++.
    #[must_use]
    #[expect(
        clippy::cast_possible_truncation,
        reason = "SRC_* are 16.16 fixed point; the integer part is a 32-bit field"
    )]
    pub const fn requires_scaling(&self) -> bool {
        let (Some(src_w), Some(crtc_w), Some(src_h), Some(crtc_h)) = (
            self.property(PropTag::SrcW),
            self.property(PropTag::CrtcW),
            self.property(PropTag::SrcH),
            self.property(PropTag::CrtcH),
        ) else {
            return false;
        };
        (src_w >> 16) as u32 != crtc_w as u32 || (src_h >> 16) as u32 != crtc_h as u32
    }
}

/// Fractional part of the golden ratio, the conventional hash seed.
const GOLDEN_RATIO: u64 = 0x9e37_79b9_7f4a_7c15;

/// Boost-style `hash_combine`. Order-dependent, which is why
/// [`Layer::property_hash`] walks tags in a fixed order.
fn combine(hash: u64, value: u64) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    value.hash(&mut hasher);
    hash ^ (hasher
        .finish()
        .wrapping_add(GOLDEN_RATIO)
        .wrapping_add(hash << 6)
        .wrapping_add(hash >> 2))
}
