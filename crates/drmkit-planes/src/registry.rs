// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! What the hardware's display planes can do.

use drmkit_fmt::{BandwidthClass, FormatTable, Modifier, classify};

/// The kernel's plane type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
#[non_exhaustive]
pub enum PlaneType {
    /// The CRTC's primary plane.
    Primary,
    /// An overlay plane.
    #[default]
    Overlay,
    /// The cursor plane.
    Cursor,
}

/// Plane `COLOR_ENCODING`: the YCbCr-to-RGB matrix applied when scanning out a
/// YUV format. Ignored by the kernel for RGB.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[allow(
    clippy::exhaustive_enums,
    reason = "drmkit-scene resolves it with an exhaustive match, so a new encoding fails to compile until it is mapped"
)]
pub enum ColorEncoding {
    /// BT.601.
    Bt601,
    /// BT.709.
    Bt709,
    /// BT.2020.
    Bt2020,
}

/// Plane `COLOR_RANGE`: whether 8-bit YCbCr is full-range (0..255) or
/// limited-range (16..235 luma, 16..240 chroma).
///
/// Most camera and broadcast YCbCr is limited-range; PC-generated YCbCr is
/// sometimes full.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[allow(
    clippy::exhaustive_enums,
    reason = "a closed set: there is no further variant to add"
)]
pub enum ColorRange {
    /// Limited range.
    Limited,
    /// Full range.
    Full,
}

/// Driver-assigned enum integers for a property's named values.
///
/// The kernel hands back enum values in the order the driver registered them,
/// so a caller must look up by name and cache the integer — which is what this
/// holds. `None` means the plane does not advertise that value at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[allow(
    clippy::exhaustive_structs,
    reason = "built by callers with `..Default::default()`, which a new field does not break"
)]
pub struct BlendModeValues {
    /// `"Pre-multiplied"`. Prefer this: it matches the canvas's premultiplied
    /// output convention.
    pub premultiplied: Option<u64>,
    /// `"Coverage"` — straight-alpha `SRC_OVER`.
    pub coverage: Option<u64>,
    /// `"None"` — pixel replace, no blending.
    pub none: Option<u64>,
}

/// Driver-assigned enum integers for `COLOR_ENCODING`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[allow(
    clippy::exhaustive_structs,
    reason = "built by callers with `..Default::default()`, which a new field does not break"
)]
pub struct ColorEncodingValues {
    /// BT.601, if advertised.
    pub bt601: Option<u64>,
    /// BT.709, if advertised.
    pub bt709: Option<u64>,
    /// BT.2020, if advertised. Older drivers often omit it.
    pub bt2020: Option<u64>,
}

/// Driver-assigned enum integers for `COLOR_RANGE`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[allow(
    clippy::exhaustive_structs,
    reason = "built by callers with `..Default::default()`, which a new field does not break"
)]
pub struct ColorRangeValues {
    /// Limited range, if advertised.
    pub limited: Option<u64>,
    /// Full range, if advertised.
    pub full: Option<u64>,
}

/// Per-plane color pipeline support (pre-blend CMS).
///
/// When all three are present the plane can apply per-layer EOTF inversion,
/// gamut conversion, and an SDR-luminance multiplier *before* the kernel blends
/// in linear light — the only correct way to mix HDR and SDR layers on one
/// CRTC. Hardware without them (RDNA2, vkms, older i915) leaves them false and
/// the scene falls back to the CRTC pipeline or to pre-composition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[allow(
    clippy::exhaustive_structs,
    reason = "built by callers with `..Default::default()`, which a new field does not break"
)]
pub struct ColorPipeline {
    /// `PLANE_DEGAMMA_LUT` is present.
    pub has_degamma_lut: bool,
    /// `PLANE_CTM` is present.
    pub has_ctm: bool,
    /// `PLANE_HDR_MULT` is present — a 16.16 fixed-point scalar applied to
    /// post-degamma linear values, used to lift SDR luminance into an HDR
    /// blend space.
    pub has_hdr_mult: bool,
    /// How many `drm_color_lut` entries `PLANE_DEGAMMA_LUT` expects. Zero when
    /// the property is absent.
    pub degamma_lut_size: u32,
}

/// Everything the allocator needs to know about one plane.
///
/// The `has_*` flags mirror the kernel's own property presence one-for-one, so
/// they stay independent booleans rather than a bitfield: a caller building a
/// fixture writes exactly the properties the hardware it is modeling exposes,
/// and packing them would make that unreadable.
#[expect(
    clippy::struct_excessive_bools,
    reason = "one flag per kernel property; see the type documentation"
)]
#[derive(Debug, Clone, Default)]
#[allow(
    clippy::exhaustive_structs,
    reason = "built by callers with `..Default::default()`, which a new field does not break"
)]
pub struct PlaneCapabilities {
    /// The plane's KMS object id.
    pub id: u32,
    /// Bitmask of CRTC indices this plane can feed.
    pub possible_crtcs: u32,
    /// Primary, overlay, or cursor.
    pub plane_type: PlaneType,
    /// Advertised `FourCC`s, from the bare `formats` list.
    pub formats: Vec<u32>,
    /// The queryable view of `IN_FORMATS`.
    ///
    /// Built from the blob at enumeration. When the driver exposes no
    /// `IN_FORMATS`, [`build_format_metadata`](Self::build_format_metadata)
    /// synthesizes `LINEAR` for each advertised format. The allocator's edge
    /// predicate queries this through `drmkit_fmt::layer_fits_plane`.
    pub format_table: FormatTable,
    /// Minimum `zpos`, if the plane exposes the property.
    pub zpos_min: Option<u64>,
    /// Maximum `zpos`, if the plane exposes the property.
    pub zpos_max: Option<u64>,
    /// For a multirect "virtual" plane -- the second rectangle of a hardware
    /// pipe, which some controllers publish as its own plane -- the id of the
    /// parent plane it is only valid alongside. `None` for ordinary planes.
    /// Read from the driver's read-only `capabilities` blob
    /// (`primary_smart_plane_id=`); see [`parse_multirect_parent`](crate::parse_multirect_parent).
    pub multirect_parent: Option<u32>,
    /// Supported rotate/reflect angles as a `DRM_MODE_ROTATE_*` /
    /// `DRM_MODE_REFLECT_*` mask; 0 when the plane exposes no rotation.
    ///
    /// A plane can have [`supports_rotation`](Self::supports_rotation) true and
    /// still omit 90/270 here — 0/180- or reflect-only hardware — which is why
    /// the allocator gates on these bits rather than the flag. Not
    /// hypothetical: measured on a Raspberry Pi 5, every one of vc4's 56
    /// planes reports `0x35` — 0, 180, and both reflects, with no 90 or 270.
    /// vkms reports `0x3f` and amdgpu `0xf` on the same probe.
    pub rotation_bits: u64,
    /// Whether the plane exposes a rotation property at all.
    pub supports_rotation: bool,
    /// Whether the plane may be asked to scale.
    ///
    /// Not a measured capability, and the name is the most optimistic reading
    /// of what KMS offers. [`PlaneRegistry::probe`] sets it from the presence
    /// of `SRC_W`, which every atomic plane has -- so on a real device it is
    /// true everywhere, vkms and its absent scaler included. There is no
    /// property that answers the question; the `TEST_ONLY` commit is what
    /// finds out, and the allocator's search is built around that.
    ///
    /// It stays because a fixture, a replay, or a caller that knows its
    /// hardware can set it false and have the matcher skip those planes
    /// before spending a test commit on them. See
    /// [drm-cxx#252](https://github.com/jwinarske/drm-cxx/issues/252).
    pub supports_scaling: bool,
    /// Whether the driver exposed `IN_FORMATS`.
    pub has_format_modifiers: bool,
    /// Whether the plane exposes `"pixel blend mode"`.
    ///
    /// Absence means scanout blending is hardwired by the driver — typically
    /// opaque pixel-replace, so a transparent source pixel paints opaque black
    /// over whatever is beneath it.
    pub has_pixel_blend_mode: bool,
    /// Whether the plane exposes `FB_DAMAGE_CLIPS`.
    ///
    /// Per plane, not per device, because it is not all-or-nothing: amdgpu
    /// attaches it to six of its ten planes. A scene that reports partial
    /// damage on a plane without it gets full-frame repaints there and
    /// partial ones elsewhere, which is correct but worth being able to see.
    ///
    /// Narrower than it looks. `drm_plane_enable_fb_damage_clips` is opt-in
    /// per driver: amdgpu calls it, vkms and vc4 do not.
    pub has_fb_damage_clips: bool,

    /// Whether the plane exposes the `"alpha"` per-plane alpha property.
    /// Independent of [`has_pixel_blend_mode`](Self::has_pixel_blend_mode) —
    /// some hardware has one without the other.
    pub has_per_plane_alpha: bool,
    /// Cached blend-mode enum integers.
    pub blend_modes: BlendModeValues,
    /// Whether the plane exposes `COLOR_ENCODING`. Only YCbCr-capable planes
    /// do; the scene skips the write for the rest.
    pub has_color_encoding: bool,
    /// Cached `COLOR_ENCODING` enum integers.
    pub color_encodings: ColorEncodingValues,
    /// Whether the plane exposes `COLOR_RANGE`.
    pub has_color_range: bool,
    /// Cached `COLOR_RANGE` enum integers.
    pub color_ranges: ColorRangeValues,
    /// Per-plane color pipeline support.
    pub color_pipeline: ColorPipeline,
    /// The device's `DRM_CAP_CURSOR_WIDTH`, on a cursor plane. Zero on any
    /// other plane, and where the driver reports none; zero leaves the
    /// allocator's cursor-size guard off.
    pub cursor_max_w: u32,
    /// The device's `DRM_CAP_CURSOR_HEIGHT`, on a cursor plane, as
    /// [`cursor_max_w`](Self::cursor_max_w).
    pub cursor_max_h: u32,
    /// One bandwidth class per distinct advertised modifier, sorted by
    /// modifier, so the allocator hot path never re-decodes modifier bits.
    ///
    /// Filled by [`build_format_metadata`](Self::build_format_metadata); query
    /// through [`bandwidth_class`](Self::bandwidth_class).
    ///
    /// Public because a caller building a fixture constructs
    /// `PlaneCapabilities` with struct-update syntax, which a private field
    /// would block — the same reason the C++ leaves it a public member. It is
    /// derived state: leave it empty and let
    /// [`PlaneRegistry::from_capabilities`] fill it.
    pub modifier_classes: Vec<(u64, BandwidthClass)>,
}

impl PlaneCapabilities {
    /// Whether the bare `formats` list advertises a `FourCC`.
    ///
    /// This is the pre-`IN_FORMATS` check and ignores modifiers entirely, so
    /// it does not say whether a given buffer will scan out: a plane can list
    /// a format it takes only compressed. Ask [`scans_out`](Self::scans_out)
    /// that.
    #[must_use]
    pub fn supports_format(&self, fourcc: u32) -> bool {
        self.formats.contains(&fourcc)
    }

    /// Whether the plane scans out `fourcc` in the `modifier` layout.
    ///
    /// By the plane's `IN_FORMATS` table, with `layer_fits_plane`'s
    /// equivalences, or by the bare format list on a plane without one. The
    /// RK3566's Cluster windows are why the list alone is not enough: they
    /// list `ARGB8888` and take it only as AFBC, and refuse a linear buffer at
    /// check time (P-48).
    #[must_use]
    pub fn scans_out(&self, fourcc: u32, modifier: Modifier) -> bool {
        if self.format_table.is_empty() {
            self.supports_format(fourcc)
        } else {
            drmkit_fmt::layer_fits_plane(&self.format_table, fourcc, modifier)
        }
    }

    /// Whether the plane scans out a linear buffer of `fourcc`: what a dumb
    /// buffer is.
    #[must_use]
    pub fn scans_out_linear(&self, fourcc: u32) -> bool {
        self.scans_out(fourcc, Modifier::LINEAR)
    }

    /// Whether this plane can feed the CRTC at `crtc_index`.
    #[must_use]
    pub const fn compatible_with_crtc(&self, crtc_index: u32) -> bool {
        if crtc_index >= u32::BITS {
            return false;
        }
        self.possible_crtcs & (1 << crtc_index) != 0
    }

    /// The precomputed bandwidth class for a modifier.
    ///
    /// Falls back to a live `classify` for a modifier outside the advertised
    /// set — decoding it rather than guessing.
    #[must_use]
    pub fn bandwidth_class(&self, modifier: u64) -> BandwidthClass {
        self.modifier_classes
            .binary_search_by_key(&modifier, |(m, _)| *m)
            .map_or_else(
                |_| classify(Modifier(modifier)),
                |index| self.modifier_classes[index].1,
            )
    }

    /// Populate the format table and bandwidth-class index.
    ///
    /// Called once per plane at registry construction, and safe to re-run. A
    /// plane exposing no `IN_FORMATS` gets `LINEAR` synthesized for each
    /// advertised format, so the allocator can still place linear buffers on it.
    pub fn build_format_metadata(&mut self) {
        if self.format_table.is_empty() && !self.formats.is_empty() {
            self.format_table = FormatTable::from_pairs(
                self.formats
                    .iter()
                    .map(|&fourcc| (fourcc, Modifier::LINEAR)),
            );
        }

        self.modifier_classes.clear();
        self.modifier_classes.extend(
            self.format_table
                .all()
                .iter()
                .map(|fm| (fm.modifier.0, classify(fm.modifier))),
        );
        self.modifier_classes.sort_unstable_by_key(|(m, _)| *m);
        self.modifier_classes.dedup_by_key(|(m, _)| *m);
    }

    /// How many distinct modifiers have a precomputed class.
    #[must_use]
    pub fn distinct_modifier_count(&self) -> usize {
        self.modifier_classes.len()
    }
}

/// Every display plane the device exposes.
///
/// # Deviation: no interior-mutability cache
///
/// The C++ memoizes `for_crtc` in a `mutable` map of pointers into its own
/// vector, and deletes the copy constructor so those pointers cannot dangle.
/// Here [`for_crtc`](Self::for_crtc) returns a borrowed iterator computed on
/// demand: the filter is a bitmask test over a handful of planes, so the cache
/// bought little, and dropping it removes both the interior mutability and the
/// reason copying had to be forbidden.
#[derive(Debug, Clone, Default)]
pub struct PlaneRegistry {
    planes: Vec<PlaneCapabilities>,
    /// How many planes each CRTC can stack, by CRTC index, where the driver
    /// says. See [`blend_stage_limit`](Self::blend_stage_limit).
    stage_limits: Vec<Option<usize>>,
}

impl PlaneRegistry {
    /// An empty registry.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            planes: Vec::new(),
            stage_limits: Vec::new(),
        }
    }

    /// Build a registry from a caller-supplied capability list.
    ///
    /// The linchpin for testing: it lets the whole allocator run against
    /// synthetic fixtures with no device, which is how the ported allocator
    /// suite reaches its coverage. Also used by replay and snapshot tools that
    /// hold a serialized capability set.
    ///
    /// The result behaves identically to an enumerated registry — the format
    /// table and bandwidth-class metadata are derived the same way.
    #[must_use]
    pub fn from_capabilities(mut capabilities: Vec<PlaneCapabilities>) -> Self {
        for plane in &mut capabilities {
            plane.build_format_metadata();
        }
        Self {
            planes: capabilities,
            stage_limits: Vec::new(),
        }
    }

    /// How many planes the CRTC at `crtc_index` can stack at once, where the
    /// driver publishes it; `None` for no limit known.
    ///
    /// Not the plane count, and not the width of the advertised zpos range:
    /// the SA8155P offers more planes than it has blend stages, and
    /// advertises zpos up to 10 while only 0 to 8 are usable. The allocator
    /// never asks the kernel about more armed planes than this. Read by
    /// [`probe`](Self::probe) from the CRTC's `capabilities` blob
    /// ([`parse_blend_stage_limit`](crate::parse_blend_stage_limit)).
    #[must_use]
    pub fn blend_stage_limit(&self, crtc_index: u32) -> Option<usize> {
        self.stage_limits
            .get(usize::try_from(crtc_index).ok()?)
            .copied()
            .flatten()
    }

    /// Set [`blend_stage_limit`](Self::blend_stage_limit) for one CRTC, for
    /// a registry built with
    /// [`from_capabilities`](Self::from_capabilities).
    pub fn set_blend_stage_limit(&mut self, crtc_index: u32, limit: Option<usize>) {
        let Ok(index) = usize::try_from(crtc_index) else {
            return;
        };
        if self.stage_limits.len() <= index {
            self.stage_limits.resize(index + 1, None);
        }
        self.stage_limits[index] = limit;
    }

    /// Every plane.
    #[must_use]
    pub fn all(&self) -> &[PlaneCapabilities] {
        &self.planes
    }

    /// Planes compatible with a CRTC.
    pub fn for_crtc(&self, crtc_index: u32) -> impl Iterator<Item = &PlaneCapabilities> + '_ {
        self.planes
            .iter()
            .filter(move |plane| plane.compatible_with_crtc(crtc_index))
    }

    /// Every modifier any non-cursor plane on `crtc_index` can scan `fourcc`
    /// out with, deduplicated and in ascending order.
    ///
    /// The union across all of them, not the primary's `IN_FORMATS` alone.
    /// On a split `SoC` the primary can advertise only a compressed layout while
    /// the producer exports `LINEAR`: intersecting against the primary would
    /// come back empty and miss the `LINEAR`-capable overlay the allocator was
    /// always going to place the layer on. The `TEST_ONLY` commit during
    /// placement remains the real arbiter of which plane takes it.
    ///
    /// Cursor planes are excluded for the same reason they are excluded from
    /// [`force_disable_candidates`](Self::force_disable_candidates): the cursor
    /// path owns them, and a scanout layer is never placed there.
    #[must_use]
    pub fn candidate_modifiers(&self, crtc_index: u32, fourcc: u32) -> Vec<u64> {
        let mut modifiers: Vec<u64> = self
            .for_crtc(crtc_index)
            .filter(|plane| plane.plane_type != PlaneType::Cursor)
            .flat_map(|plane| plane.format_table.modifiers_for(fourcc))
            .map(|modifier| modifier.0)
            .collect();
        modifiers.sort_unstable();
        modifiers.dedup();
        modifiers
    }

    /// Planes on `crtc_index` that a commit may force-disable.
    ///
    /// [`for_crtc`](Self::for_crtc) minus the cursor planes. A cursor plane is
    /// owned by the cursor path, not the scene: clearing it here would erase
    /// the cursor on every frame the scene commits. Upstream carves out the
    /// same exception in `disable_unused_planes`.
    pub fn force_disable_candidates(
        &self,
        crtc_index: u32,
    ) -> impl Iterator<Item = &PlaneCapabilities> + '_ {
        self.for_crtc(crtc_index)
            .filter(|plane| plane.plane_type != PlaneType::Cursor)
    }

    /// A plane by its KMS object id.
    #[must_use]
    pub fn by_id(&self, plane_id: u32) -> Option<&PlaneCapabilities> {
        self.planes.iter().find(|plane| plane.id == plane_id)
    }

    /// How many planes the registry holds.
    #[must_use]
    pub fn len(&self) -> usize {
        self.planes.len()
    }

    /// Whether the registry holds no planes.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.planes.is_empty()
    }
}

/// Map a 16-bit layer alpha onto the range a plane advertises.
///
/// Layers carry a 16-bit alpha because that is what the DRM documentation
/// describes, but the range is the driver's to declare: vendor drivers ship
/// 8-bit alpha advertising `[0, 255]`, and writing `0xFFFF` to one of those
/// fails the whole atomic commit with `EINVAL` -- which a `TEST_ONLY` reports
/// only as "this assignment does not fit", so it surfaces as a dropped layer
/// that mentions nothing about alpha (drm-cxx#265).
///
/// Rescaled rather than clamped: alpha is a fraction of full opacity, so half
/// alpha must become half of `alpha_max`. Clamping would turn every partially
/// transparent layer opaque on an 8-bit plane, and would agree with this on
/// exactly one value, full opacity, which is the one value most testing uses.
/// Rounds to nearest, so full opacity lands exactly on `alpha_max`; the
/// identity for a plane advertising the full 16 bits; saturates an oversized
/// input rather than scaling past the maximum.
#[must_use]
pub const fn rescale_alpha(value: u64, alpha_max: u64) -> u64 {
    const FULL: u64 = 0xFFFF;
    let clamped = if value < FULL { value } else { FULL };
    if alpha_max >= FULL {
        return clamped;
    }
    (clamped * alpha_max + FULL / 2) / FULL
}
