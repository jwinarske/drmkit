// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! Turning plane assignments into atomic commits.
//!
//! This is the seam between the device-free search in `drmkit-planes` and the
//! kernel. The allocator decides *what* to try; this knows *how* to ask.

use std::collections::{HashMap, HashSet};

use drmkit_core::{
    AtomicCommitFlags, CoreError, Device, Mode, ObjectType, PropertyBlob, PropertyStore,
};
use drmkit_planes::{LayerRef, PropTag, TestCommitter, TestFailure};

use crate::frame::KernelResult;

/// A plane's colorimetry properties, resolved to the values this scene writes.
///
/// The KMS names are strings but the wire values are driver-assigned integers,
/// so both are looked up per plane rather than assumed.
#[derive(Debug, Clone, Copy)]
struct ColorProps {
    encoding_id: u32,
    encoding_value: u64,
    range_id: u32,
    range_value: u64,
}

/// The colorimetry this scene asks for on every plane it programs.
///
/// BT.709 with limited range, matching upstream's default. A per-layer
/// override is not modelled yet; when it is, it replaces these two values and
/// nothing else about the emission changes.
const DEFAULT_COLOR_ENCODING: &str = "ITU-R BT.709 YCbCr";
const DEFAULT_COLOR_RANGE: &str = "YCbCr limited range";

/// Resolves a layer's [`PropTag`]s to the DRM property ids of a given plane.
///
/// Property ids are per-object and stable for a device's lifetime, so they are
/// resolved once per plane and cached. Doing it per frame would be an ioctl per
/// property per layer.
#[derive(Debug, Default)]
pub struct PlanePropertyMap {
    /// `plane_id` to its resolved tag ids.
    pub(crate) planes: HashMap<u32, HashMap<PropTag, u32>>,
    /// Properties the kernel marks immutable, which a commit must never write.
    ///
    /// Writing one is rejected with `EINVAL` whatever the value, so a single
    /// stray write poisons the whole commit — including every other layer in
    /// it.
    immutable: HashMap<u32, Vec<PropTag>>,
    /// Colorimetry properties, for the planes that expose them.
    color: HashMap<u32, ColorProps>,
    /// `FB_DAMAGE_CLIPS`, for the planes that expose it.
    ///
    /// Kept apart from `planes` rather than given a `PropTag`, because its
    /// value does not exist until emission: it is a blob id, created from the
    /// rectangles a source reported for *this* frame. A tag would put it in
    /// the layer's property bag, which is built before any of that is known.
    damage: HashMap<u32, u32>,
    /// The inclusive zpos range each plane advertises, where it has one.
    ///
    /// zpos is the one plane property drmkit *derives* rather than echoes: the
    /// composition canvas asks to sit above every layer, and "above" is
    /// computed from the layer count. Nothing in that arithmetic knows what
    /// the plane will accept, and a plane whose range is `[1, 17]` rejects 19
    /// with `EINVAL` -- taking the whole frame down with it, every correctly
    /// programmed layer included.
    pub(crate) zpos_range: HashMap<u32, (u64, u64)>,
    /// The largest `alpha` each plane advertises, where it is not the full 16
    /// bits. Layers carry 16-bit alpha; see [`drmkit_planes::rescale_alpha`].
    pub(crate) alpha_max: HashMap<u32, u64>,
    /// Planes whose colorimetry the kernel has already taken.
    ///
    /// These properties are sticky across clients, so they have to be written
    /// once to displace whatever the previous compositor left -- and then not
    /// again, because restating an unchanged value every frame is exactly the
    /// per-frame traffic the rest of the emit path works to avoid.
    color_committed: HashSet<u32>,
}

impl PlanePropertyMap {
    /// An empty map.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Resolve and cache every tag this crate writes for `plane_id`.
    ///
    /// Tags the plane does not expose are simply absent: a plane without
    /// `rotation` or `alpha` is normal, and asking for one is not an error.
    ///
    /// # Errors
    ///
    /// [`CoreError`] if the plane's properties cannot be read at all.
    pub fn learn_plane(&mut self, device: &Device, plane_id: u32) -> Result<(), CoreError> {
        let mut store = PropertyStore::new();
        store.cache_properties(device, plane_id, ObjectType::Plane)?;

        // Colorimetry is optional -- plenty of planes expose neither property,
        // and a plane with only one of the pair is treated as having neither
        // rather than being left half-configured.
        let color = match (
            store.property_id(plane_id, "COLOR_ENCODING"),
            store.property_id(plane_id, "COLOR_RANGE"),
        ) {
            (Ok(encoding_id), Ok(range_id)) => Some(ColorProps {
                encoding_id,
                encoding_value: store.enum_value(
                    device,
                    plane_id,
                    "COLOR_ENCODING",
                    DEFAULT_COLOR_ENCODING,
                )?,
                range_id,
                range_value: store.enum_value(
                    device,
                    plane_id,
                    "COLOR_RANGE",
                    DEFAULT_COLOR_RANGE,
                )?,
            }),
            _ => None,
        };
        if let Some(color) = color {
            self.color.insert(plane_id, color);
        }

        if let Ok(Some(range)) = store.range(plane_id, PropTag::Zpos.name()) {
            self.zpos_range.insert(plane_id, range);
        }
        if let Ok(Some((_, max))) = store.range(plane_id, PropTag::Alpha.name()) {
            self.alpha_max.insert(plane_id, max);
        }

        if let Ok(id) = store.property_id(plane_id, "FB_DAMAGE_CLIPS") {
            self.damage.insert(plane_id, id);
        }

        let mut ids = HashMap::new();
        let mut immutable = Vec::new();
        for tag in PropTag::ALL {
            // `pixel_format` and `FB_MODIFIER` are drmkit's own hints on the
            // property bag, not KMS plane properties -- the allocator reads
            // them, the kernel never sees them.
            if matches!(tag, PropTag::PixelFormat | PropTag::FbModifier) {
                continue;
            }
            if let Ok(id) = store.property_id(plane_id, tag.name()) {
                if store.is_immutable(plane_id, tag.name()).unwrap_or(false) {
                    immutable.push(tag);
                    continue;
                }
                ids.insert(tag, id);
            }
        }

        self.planes.insert(plane_id, ids);
        self.immutable.insert(plane_id, immutable);
        Ok(())
    }

    /// Learn every plane the registry knows about.
    ///
    /// # Errors
    ///
    /// [`CoreError`] if a plane's properties cannot be read.
    pub fn learn_all(
        &mut self,
        device: &Device,
        registry: &drmkit_planes::PlaneRegistry,
    ) -> Result<(), CoreError> {
        for plane in registry.all() {
            self.learn_plane(device, plane.id)?;
        }
        Ok(())
    }

    /// Record that a real commit carried `plane_id`'s colorimetry.
    ///
    /// Call only after the kernel accepted the commit. A `TEST_ONLY` applies
    /// nothing, so noting one would suppress the write on the commit that
    /// actually matters and leave the plane on the previous client's settings.
    pub fn note_color_committed(&mut self, plane_id: u32) {
        self.color_committed.insert(plane_id);
    }

    /// The DRM property id for a tag on a plane, if it is writable there.
    #[must_use]
    pub fn property_id(&self, plane_id: u32, tag: PropTag) -> Option<u32> {
        self.planes.get(&plane_id)?.get(&tag).copied()
    }

    /// Whether the plane marks this property immutable.
    #[must_use]
    pub fn is_immutable(&self, plane_id: u32, tag: PropTag) -> bool {
        self.immutable
            .get(&plane_id)
            .is_some_and(|tags| tags.contains(&tag))
    }

    /// How many planes have been learned.
    #[must_use]
    pub fn len(&self) -> usize {
        self.planes.len()
    }

    /// Whether nothing has been learned yet.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.planes.is_empty()
    }
}

/// Emit one layer's properties onto a plane.
///
/// Skips tags the plane does not expose or marks immutable, and the two hint
/// tags the kernel never sees. Returns how many properties were written.
///
/// # Errors
///
/// [`CoreError`] if a property write is rejected before the commit — which
/// only happens for a malformed id.
pub fn emit_layer(
    request: &mut drmkit_core::AtomicRequest,
    map: &PlanePropertyMap,
    plane_id: u32,
    layer: &drmkit_planes::Layer,
    baseline: Option<&drmkit_planes::PropertySnapshot>,
) -> Result<LayerWrites, CoreError> {
    let mut written = LayerWrites::default();
    for (tag, value) in layer.properties() {
        let Some(property_id) = map.property_id(plane_id, tag) else {
            continue;
        };
        // An externally bound layer's FB_ID is set up by the producer's
        // extension stack. Writing it from here fights that, so suppress it
        // even if something has stuffed one into the property bag.
        if layer.is_externally_bound() && tag == PropTag::FbId {
            continue;
        }
        // The diff compares what the caller asked for, because that is what
        // the baseline records; the plane's own range is applied to what goes
        // out. The mapping is fixed per plane, so equal requests always mean
        // equal writes -- and comparing a rescaled value against a raw
        // baseline would rewrite alpha on every frame of an 8-bit plane.
        if !needs_write(tag, value, baseline) {
            continue;
        }
        let value = match tag {
            PropTag::Zpos => map.clamp_zpos(plane_id, value),
            PropTag::Alpha => map.rescale_alpha(plane_id, value),
            _ => value,
        };
        request.add_property(plane_id, property_id, value)?;
        written.properties += 1;
        if tag == PropTag::FbId {
            written.framebuffers += 1;
        }
    }
    Ok(written)
}

/// What emitting one layer put on the wire.
///
/// `framebuffers` is a subset of `properties`, kept apart because it answers a
/// different question: `FB_ID` is written every frame by contract, so it is
/// the floor a minimal frame is measured against rather than part of what the
/// diff decided.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LayerWrites {
    /// Properties written for this layer.
    pub properties: usize,
    /// Of those, `FB_ID` attachments.
    pub framebuffers: usize,
}

impl PlanePropertyMap {
    /// A layer's 16-bit alpha, on the scale this plane advertises.
    ///
    /// Every write of a layer's properties goes through
    /// [`emit_layer`], so this one call covers the allocator's tests, the real
    /// commit and a pinned layer alike -- upstream needed two fixes to reach
    /// all three.
    #[must_use]
    pub fn rescale_alpha(&self, plane_id: u32, value: u64) -> u64 {
        self.alpha_max
            .get(&plane_id)
            .map_or(value, |&max| drmkit_planes::rescale_alpha(value, max))
    }

    /// Bring a derived zpos inside what the plane will actually take.
    ///
    /// Only zpos is clamped, and only against an unsigned range. Every other
    /// plane property carries a value the caller measured or was handed -- a
    /// rectangle, a framebuffer id -- where a value outside the range is a
    /// real error that silently moving would hide. zpos alone is computed, so
    /// it alone is worth bending to fit; a canvas one slot lower than asked is
    /// still above every layer, whereas the alternative is no frame at all.
    #[must_use]
    pub fn clamp_zpos(&self, plane_id: u32, value: u64) -> u64 {
        match self.zpos_range.get(&plane_id) {
            Some((lo, hi)) => value.clamp(*lo, *hi),
            None => value,
        }
    }
}

/// Whether a property has to be written, given what the kernel last took.
///
/// `baseline` of `None` means write everything -- see
/// [`committed_baseline`](drmkit_planes::Allocator::committed_baseline).
fn needs_write(
    tag: PropTag,
    value: u64,
    baseline: Option<&drmkit_planes::PropertySnapshot>,
) -> bool {
    // Two properties are written every commit no matter what the diff says.
    //
    // FB_ID is how KMS is told this is a new frame: the kernel schedules the
    // page-flip event off the re-attach. A single-buffered source whose pixels
    // mutate in place presents the same id every frame, so diffing it away
    // leaves an otherwise-clean scene with an empty request -- which the kernel
    // accepts and then never queues an event for, wedging the caller's flip
    // forever.
    //
    // IN_FENCE_FD is a one-shot the kernel consumes each commit. An fd number
    // the diff happens to see unchanged, because the value was recycled, still
    // has to be re-armed or the plane scans out before its buffer is ready.
    if matches!(tag, PropTag::FbId | PropTag::InFenceFd) {
        return true;
    }
    baseline.is_none_or(|snapshot| snapshot.get(tag) != Some(value))
}

/// Emit the writes that detach a plane.
///
/// A plane left out of an assignment must be explicitly cleared, or it inherits
/// whatever the previous commit left on it — an old framebuffer, an old CRTC
/// binding, an old stacking position. Under a `TEST_ONLY` that shows up as a
/// spurious rejection when a layer migrates between planes on one CRTC: two
/// active planes, same stacking slot, same CRTC.
///
/// # Errors
///
/// [`CoreError`] if a property write is rejected.
pub fn emit_disable(
    request: &mut drmkit_core::AtomicRequest,
    map: &PlanePropertyMap,
    plane_id: u32,
) -> Result<usize, CoreError> {
    let mut written = 0;
    for tag in [PropTag::FbId, PropTag::CrtcId] {
        if let Some(property_id) = map.property_id(plane_id, tag) {
            request.add_property(plane_id, property_id, 0)?;
            written += 1;
        }
    }
    Ok(written)
}

/// Turn off what a [`rebind`](crate::LayerScene::rebind) left on the old CRTC.
///
/// One commit of its own, before the first frame on the new CRTC: every plane
/// the scene had lit there is detached, and the old CRTC is switched off
/// (`ACTIVE = 0`). It has to be separate because the kernel refuses to move a
/// plane between CRTCs in one commit, and a plane both pipes can use is one
/// the new frame may want. The CRTC goes off with its planes because some
/// drivers reject an active CRTC whose primary is disarmed; `ACTIVE = 0` keeps
/// its mode and connector, so a caller that wants the old output back sets it
/// active again. Upstream has no equivalent yet (drm-cxx#340).
///
/// Returns whether there was anything to commit. On success the scene can
/// build frames again; on failure the detach stays pending and
/// [`build_frame`](crate::LayerScene::build_frame) goes on refusing.
///
/// # Errors
///
/// [`CoreError`] if a property cannot be found or the kernel rejects the
/// commit.
pub fn commit_detach(device: &Device, scene: &mut crate::LayerScene) -> Result<bool, CoreError> {
    let Some(detach) = scene.pending_detach() else {
        return Ok(false);
    };
    // Looked up here rather than through the caller's property map: a plane
    // the map never learned would be skipped silently and stay lit, which is
    // the defect this exists to fix.
    let mut store = PropertyStore::new();
    let mut request = drmkit_core::AtomicRequest::with_capacity(2 * detach.planes.len() + 1);
    for &plane_id in &detach.planes {
        store.cache_properties(device, plane_id, ObjectType::Plane)?;
        request.add_property(plane_id, store.property_id(plane_id, "FB_ID")?, 0)?;
        request.add_property(plane_id, store.property_id(plane_id, "CRTC_ID")?, 0)?;
    }
    store.cache_properties(device, detach.crtc_id, ObjectType::Crtc)?;
    request.add_property(
        detach.crtc_id,
        store.property_id(detach.crtc_id, "ACTIVE")?,
        0,
    )?;
    request.commit(device, AtomicCommitFlags::ALLOW_MODESET)?;
    scene.detach_committed();
    Ok(true)
}

/// `layer` with the stacked zpos for `plane_id`, if the stack has one.
fn with_stacked_zpos(
    layer: &drmkit_planes::Layer,
    plane_id: u32,
    stacked: &[(u32, u64)],
) -> drmkit_planes::Layer {
    let mut layer = layer.clone();
    if let Some((_, zpos)) = stacked.iter().find(|(id, _)| *id == plane_id) {
        layer.set_property(PropTag::Zpos, *zpos);
    }
    layer
}

/// A [`TestCommitter`] that issues real `TEST_ONLY` commits.
///
/// Builds a fresh request per test that (a) disables every candidate plane not
/// in the assignment, then (b) applies each assigned layer's properties — the
/// order matters for the reason [`emit_disable`] describes.
pub struct DeviceCommitter<'a> {
    device: &'a Device,
    map: &'a PlanePropertyMap,
    /// Planes on this CRTC that a test may need to disable.
    candidates: Vec<u32>,
    /// Modeset writes to prepend to every test request, if the CRTC still
    /// needs bringing up.
    modeset: Option<&'a Modeset<'a>>,
    flags: AtomicCommitFlags,
    /// How many test commits have been issued, for diagnostics.
    pub commits: usize,
    /// A plane armed in every test alongside the assignment -- the
    /// composition canvas. See [`TestCommitter::set_extra_plane`].
    extra: Option<(u32, drmkit_planes::Layer)>,
    /// For the zpos each test writes: the same stack the frame will.
    registry: &'a drmkit_planes::PlaneRegistry,
}

impl<'a> DeviceCommitter<'a> {
    /// A committer over the planes this CRTC could use.
    ///
    /// The candidate list comes from
    /// [`force_disable_candidates`](drmkit_planes::PlaneRegistry::force_disable_candidates)
    /// rather than being supplied, so a test commit and the apply commit that
    /// follows it clear exactly the same set. Handing them different lists
    /// would let a configuration pass `TEST_ONLY` and then be applied with a
    /// plane the test had cleared still live.
    #[must_use]
    pub fn new(
        device: &'a Device,
        map: &'a PlanePropertyMap,
        registry: &'a drmkit_planes::PlaneRegistry,
        crtc_index: u32,
        flags: AtomicCommitFlags,
        modeset: Option<&'a Modeset<'a>>,
    ) -> Self {
        Self {
            device,
            map,
            candidates: registry
                .force_disable_candidates(crtc_index)
                .map(|plane| plane.id)
                .collect(),
            flags,
            modeset,
            commits: 0,
            extra: None,
            registry,
        }
    }
}

impl TestCommitter for DeviceCommitter<'_> {
    fn test_assignment(&mut self, assignment: &[(u32, LayerRef<'_>)]) -> Result<(), TestFailure> {
        self.commits += 1;
        let mut request = drmkit_core::AtomicRequest::new();

        // A test request is plane-only otherwise, and while the CRTC is still
        // inactive the kernel rejects every plane bound to it with EINVAL. The
        // search would then find nothing placeable and composite the whole
        // scene -- on the first frame, on a machine that boots without a DRM
        // fbdev client to leave the CRTC lit for it.
        if let Some(modeset) = self.modeset
            && modeset.emit(&mut request).is_err()
        {
            return Err(TestFailure::Rejected);
        }

        // Disable first: a plane inheriting stale state from the previous
        // commit is what makes an otherwise-valid migration look invalid.
        let extra_plane = self.extra.as_ref().map(|(plane_id, _)| *plane_id);
        for plane_id in &self.candidates {
            if assignment.iter().all(|(assigned, _)| assigned != plane_id)
                && extra_plane != Some(*plane_id)
                && emit_disable(&mut request, self.map, *plane_id).is_err()
            {
                return Err(TestFailure::Rejected);
            }
        }
        // Full writes, deliberately. A search commit proposes a configuration
        // the kernel has not seen; diffing it against what the kernel last
        // took would test a request that is only valid as a delta from the
        // current state, while the assignment being tried is not that state.
        // Upstream splits the same way -- the search path and the apply path
        // are different functions there for this reason.
        //
        // zpos goes out as the frame will write it: stacked over exactly the
        // planes this test arms, canvas included, so the kernel is asked
        // about the stack it will be handed.
        let mut armed: Vec<(u32, Option<u64>)> = assignment
            .iter()
            .map(|(plane_id, layer)| (*plane_id, layer.layer.property(PropTag::Zpos)))
            .collect();
        if let Some((plane_id, layer)) = &self.extra {
            armed.push((*plane_id, layer.property(PropTag::Zpos)));
        }
        let stacked = drmkit_planes::stacked_zpos(self.registry, &armed);
        for (plane_id, layer) in assignment {
            let layer = with_stacked_zpos(layer.layer, *plane_id, &stacked);
            if emit_layer(&mut request, self.map, *plane_id, &layer, None).is_err() {
                return Err(TestFailure::Rejected);
            }
        }
        // The canvas, when the caller has told us there will be one. It is
        // part of the frame the kernel will be asked to take, so it is part of
        // the frame the kernel is asked about.
        if let Some((plane_id, layer)) = &self.extra {
            let layer = with_stacked_zpos(layer, *plane_id, &stacked);
            if emit_layer(&mut request, self.map, *plane_id, &layer, None).is_err() {
                return Err(TestFailure::Rejected);
            }
        }

        match request.test(self.device, self.flags) {
            Ok(()) => Ok(()),
            Err(CoreError::NotMaster) => Err(TestFailure::NotMaster),
            Err(_) => Err(TestFailure::Rejected),
        }
    }

    fn set_extra_plane(&mut self, plane_id: u32, layer: drmkit_planes::Layer) {
        self.extra = Some((plane_id, layer));
    }

    fn clear_extra_plane(&mut self) {
        self.extra = None;
    }
}

/// Classify a real commit's outcome for [`FrameLifecycle`](crate::FrameLifecycle).
#[must_use]
pub fn classify(result: &Result<(), CoreError>) -> KernelResult {
    match result {
        Ok(()) => KernelResult::Ok,
        Err(CoreError::NotMaster) => KernelResult::NotMaster,
        Err(_) => KernelResult::Rejected,
    }
}

/// The CRTC and connector writes that bring a display up.
///
/// Plane properties alone do not light a screen. Until the CRTC has a mode and
/// is `ACTIVE`, and some connector routes to it, a commit that binds planes to
/// that CRTC is rejected -- and the planes have nowhere to scan out to even if
/// it were not. Upstream folds these writes into the first commit after
/// `create()` or `rebind()` and ORs in `ALLOW_MODESET` to let them through;
/// this is the same set.
///
/// Easy to leave out and not notice: most desktops run a DRM fbdev client that
/// re-enables the CRTC as soon as the previous master drops it, so a scene that
/// never writes these still commits successfully on a developer's machine. It
/// fails on a system that boots without one.
pub struct Modeset<'a> {
    crtc_id: u32,
    connector_id: u32,
    crtc_active: u32,
    crtc_mode_id: u32,
    connector_crtc_id: u32,
    mode_blob: PropertyBlob<'a>,
}

impl<'a> Modeset<'a> {
    /// Resolve the three property ids and stage `mode` as a blob.
    ///
    /// The blob is created once and reused for every commit: it describes the
    /// mode, which does not change while the scene is bound to this CRTC.
    ///
    /// # Errors
    ///
    /// [`CoreError`] if a property is missing -- every atomic driver exposes
    /// all three, so an absent one means this is not an atomic-capable device
    /// -- or if the kernel refuses the blob.
    pub fn learn(
        device: &'a Device,
        crtc_id: u32,
        connector_id: u32,
        mode: &Mode,
    ) -> Result<Self, CoreError> {
        let mut store = PropertyStore::new();
        store.cache_properties(device, crtc_id, ObjectType::Crtc)?;
        store.cache_properties(device, connector_id, ObjectType::Connector)?;

        let crtc_active = store.property_id(crtc_id, "ACTIVE")?;
        let crtc_mode_id = store.property_id(crtc_id, "MODE_ID")?;
        let connector_crtc_id = store.property_id(connector_id, "CRTC_ID")?;

        let mode_blob = device.create_property_blob(mode)?;

        Ok(Self {
            crtc_id,
            connector_id,
            crtc_active,
            crtc_mode_id,
            connector_crtc_id,
            mode_blob,
        })
    }

    /// Emit the modeset writes. Returns how many properties were written.
    ///
    /// # Errors
    ///
    /// [`CoreError`] if a property write is rejected before the commit.
    pub fn emit(&self, request: &mut drmkit_core::AtomicRequest) -> Result<usize, CoreError> {
        request.add_property(self.crtc_id, self.crtc_mode_id, self.mode_blob.id())?;
        request.add_property(self.crtc_id, self.crtc_active, 1)?;
        request.add_property(
            self.connector_id,
            self.connector_crtc_id,
            u64::from(self.crtc_id),
        )?;
        Ok(3)
    }

    /// The blob id backing `MODE_ID`.
    #[must_use]
    pub const fn mode_blob(&self) -> u64 {
        self.mode_blob.id()
    }
}

/// Emit a built frame's apply commit.
///
/// Writes, in order: the modeset properties if this is the first commit, then
/// a disable for every candidate plane the frame did not use, then each
/// assigned layer. The disables come before the applies for the reason
/// [`emit_disable`] gives.
///
/// Returns how many properties were written.
///
/// # Errors
///
/// [`CoreError`] if a property write is rejected before the commit.
/// # The modeset must outlive the commit, not this call
///
/// A [`Modeset`] holds a [`PropertyBlob`](drmkit_core::PropertyBlob) that is
/// destroyed when it drops, and the request carries only the blob's **id**.
/// Taking `&Modeset` means it is alive *here*; it says nothing about whether
/// it is alive at `commit`. A modeset built inside a loop that emits several
/// scenes into one request, and dropped at the end of its iteration, leaves
/// the request naming a blob the kernel has already freed — and the commit is
/// refused with `EINVAL`, naming nothing useful.
///
/// Build them all first, into something that outlives the commit:
///
/// ```ignore
/// let modesets: Vec<Modeset<'_>> = slots.iter().map(|s| Modeset::learn(..)).collect()?;
/// for (slot, modeset) in slots.iter().zip(&modesets) {
///     emit_frame(&mut request, &map, &mut build, Some(modeset))?;
/// }
/// request.commit(device, flags)?;  // every blob still alive
/// ```
pub fn emit_frame(
    request: &mut drmkit_core::AtomicRequest,
    map: &PlanePropertyMap,
    build: &mut crate::FrameBuild,
    modeset: Option<&Modeset<'_>>,
) -> Result<usize, CoreError> {
    emit_frame_damaged(request, map, build, modeset, None).map(|(written, _)| written)
}

/// [`emit_frame`], plus the `FB_DAMAGE_CLIPS` blobs partial repaint needs.
///
/// `device` is what creates the blobs, and is separate from everything else
/// because it is the only part of emission that allocates kernel objects.
/// Pass `None` to emit no damage at all — every frame then repaints whole,
/// which is correct, just more bandwidth than the sources asked for.
///
/// # The returned blobs must outlive the commit
///
/// Each is a kernel object destroyed when it drops, and the request holds only
/// its id. Dropping them before `commit` leaves the request pointing at blobs
/// the kernel has already freed. Bind them to a local that lives across the
/// commit:
///
/// ```ignore
/// let (_, _blobs) = emit_frame_damaged(&mut request, &map, &mut build, None, Some(device))?;
/// request.commit(device, flags)?;
/// ```
///
/// # Errors
///
/// [`CoreError`] if a property write is rejected or a blob cannot be created.
pub fn emit_frame_damaged<'a>(
    request: &mut drmkit_core::AtomicRequest,
    map: &PlanePropertyMap,
    build: &mut crate::FrameBuild,
    modeset: Option<&Modeset<'_>>,
    device: Option<&'a Device>,
) -> Result<(usize, Vec<drmkit_core::PropertyBlob<'a>>), CoreError> {
    let mut written = 0;
    let mut fbs = 0;
    let mut damaged = 0;
    let mut blobs = Vec::new();
    if let Some(modeset) = modeset {
        written += modeset.emit(request)?;
    }
    for plane_id in build.disables() {
        written += emit_disable(request, map, *plane_id)?;
    }
    for entry in build.plan() {
        let bag = match entry.zpos {
            Some(zpos) if entry.layer.property(PropTag::Zpos) != Some(zpos) => {
                let mut bag = entry.layer.clone();
                bag.set_property(PropTag::Zpos, zpos);
                std::borrow::Cow::Owned(bag)
            }
            _ => std::borrow::Cow::Borrowed(&entry.layer),
        };
        let layer = emit_layer(request, map, entry.plane_id, &bag, entry.baseline.as_ref())?;
        written += layer.properties;
        fbs += layer.framebuffers;
        written += emit_color_props(request, map, entry.plane_id)?;

        if let Some(device) = device
            && let Some(blob) = emit_damage(request, map, device, entry.plane_id, &entry.damage)?
        {
            blobs.push(blob);
            written += 1;
            damaged += 1;
        }
    }
    // Recorded rather than only returned: the caller has no way to put these
    // back into the report, and a discarded return is how `properties_written`
    // and `fbs_attached` came to be documented counters that were always zero.
    build.record_emission(written, fbs, damaged);
    Ok((written, blobs))
}

/// Convert damage rectangles into the `struct drm_mode_rect` array the kernel
/// reads out of an `FB_DAMAGE_CLIPS` blob.
///
/// `drm_mode_rect` is four `__s32`: `x1, y1, x2, y2` — **corners, not
/// origin-and-size**, and the second corner is exclusive. Writing a width
/// where `x2` belongs describes a rectangle in the wrong place *and* the wrong
/// size, and the driver repaints that instead — leaving the region that
/// actually changed stale, which looks exactly like a producer that failed to
/// draw.
///
/// The addition saturates rather than wrapping. A rectangle whose far corner
/// does not fit an `i32` is nothing a real framebuffer contains, and clamping
/// over-reports where wrapping would describe a rectangle with its corners the
/// wrong way round.
pub(crate) fn damage_rects(damage: &[crate::DamageRect]) -> Vec<[i32; 4]> {
    damage
        .iter()
        .map(|rect| {
            [
                rect.x,
                rect.y,
                rect.x.saturating_add(rect.w.cast_signed()),
                rect.y.saturating_add(rect.h.cast_signed()),
            ]
        })
        .collect()
}

/// Stage one layer's damage as a `FB_DAMAGE_CLIPS` blob and point the plane at
/// it.
///
/// `None` when there is nothing to say: the plane has no such property, or the
/// layer reported no damage. **An empty list is not an empty blob.** Empty
/// damage means *the whole frame changed*, and the way to say that is to write
/// nothing -- a zero-rectangle blob tells the driver nothing changed, and the
/// frame is never repainted.
fn emit_damage<'a>(
    request: &mut drmkit_core::AtomicRequest,
    map: &PlanePropertyMap,
    device: &'a Device,
    plane_id: u32,
    damage: &[crate::DamageRect],
) -> Result<Option<drmkit_core::PropertyBlob<'a>>, CoreError> {
    let Some(property_id) = map.damage.get(&plane_id).copied() else {
        return Ok(None);
    };
    if damage.is_empty() {
        return Ok(None);
    }

    let rects = damage_rects(damage);
    let blob = device.create_property_blob(rects.as_slice())?;
    request.add_property(plane_id, property_id, blob.id())?;
    Ok(Some(blob))
}

/// Emit a plane's colorimetry, if it has any.
///
/// Written on every frame, exempt from the diff that governs everything else
/// in [`emit_layer`]. These properties are sticky across clients: whatever the
/// last compositor left is what this one inherits, and inheriting the wrong
/// YCbCr matrix or range tints every YUV layer -- cyan or magenta, depending
/// which is wrong. Restating them costs two writes per programmed plane and
/// removes a dependency on what ran before.
///
fn emit_color_props(
    request: &mut drmkit_core::AtomicRequest,
    map: &PlanePropertyMap,
    plane_id: u32,
) -> Result<usize, CoreError> {
    let Some(color) = map.color.get(&plane_id) else {
        return Ok(0);
    };
    if map.color_committed.contains(&plane_id) {
        return Ok(0);
    }
    request.add_property(plane_id, color.encoding_id, color.encoding_value)?;
    request.add_property(plane_id, color.range_id, color.range_value)?;
    Ok(2)
}

/// What to do with a layer's acquire fence, given what its plane can take.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FenceAction {
    /// Hand the descriptor to KMS as `IN_FENCE_FD`.
    Arm {
        /// The plane's `IN_FENCE_FD` property.
        property_id: u32,
    },
    /// The plane cannot take one, so the caller has to wait before committing.
    CpuWait,
}

/// Decide between the two, from whether the plane exposes a writable
/// `IN_FENCE_FD`.
///
/// Split out so the decision is testable without a device: the fallback is the
/// branch that matters and the one a card that advertises `IN_FENCE_FD` on
/// every plane -- vkms among them -- can never reach.
#[must_use]
pub const fn fence_action(property_id: Option<u32>) -> FenceAction {
    match property_id {
        Some(property_id) => FenceAction::Arm { property_id },
        None => FenceAction::CpuWait,
    }
}

/// Give each fenced layer's fence to its plane, or wait on it here.
///
/// A buffer's `acquire_fence` says its pixels are not valid yet. Where the
/// assigned plane exposes `IN_FENCE_FD` the descriptor goes to KMS and the
/// kernel holds scanout until it signals. Where it does not, there is nowhere
/// to put it, so the wait happens here instead -- on the CPU, before the
/// commit, which is slower but is the difference between a stalled frame and a
/// half-rendered one reaching the screen.
///
/// This runs after allocation on purpose. Until a layer has a plane there is
/// nothing to ask about `IN_FENCE_FD`, which is why the fence cannot simply be
/// lowered into the property bag with everything else.
///
/// `real` gates the CPU wait only: a `TEST_ONLY` commit never scans out, so
/// blocking on a fence for one would stall the search for no reason. The
/// property write is not gated -- a test should carry what the apply will.
///
/// # Errors
///
/// [`CoreError`] if a property write is rejected before the commit.
pub fn arm_acquire_fences(
    build: &mut crate::FrameBuild,
    request: &mut drmkit_core::AtomicRequest,
    map: &PlanePropertyMap,
    real: bool,
) -> Result<(), CoreError> {
    let mut armed = 0;
    let mut waits = 0;

    // Driven from the acquisitions rather than the plan, because the fence
    // itself lives there. Carrying a bare descriptor on the plan instead would
    // mean reconstructing a borrow from an integer, and the whole reason this
    // is safe is that the buffer -- and so the fence -- is owned by the frame
    // being built.
    for acquisition in &build.acquisitions {
        let Some(fence) = acquisition.buffer.acquire_fence.as_ref() else {
            continue;
        };
        let Some(fd) = drmkit_sync::SyncFence::as_fd(fence) else {
            continue;
        };
        // A layer with no plane was composited or dropped. There is nothing to
        // arm, and the buffer's own lifecycle still owns the fence.
        let Some(entry) = build
            .plan
            .iter()
            .find(|entry| entry.layer_id == acquisition.layer)
        else {
            continue;
        };

        match fence_action(map.property_id(entry.plane_id, PropTag::InFenceFd)) {
            FenceAction::Arm { property_id } => {
                let raw = std::os::fd::AsRawFd::as_raw_fd(&fd);
                request.add_property(
                    entry.plane_id,
                    property_id,
                    u64::from(raw.cast_unsigned()),
                )?;
                armed += 1;
            }
            FenceAction::CpuWait => {
                if !real {
                    continue;
                }
                waits += 1;
                // Reported, not fatal. The producer may never signal this
                // fence, and refusing to commit would wedge the display on one
                // bad frame rather than showing it.
                if let Err(error) = fence.wait(std::time::Duration::from_secs(1)) {
                    drmkit_log::log_warn!("acquire-fence CPU wait failed: {error}");
                }
            }
        }
    }

    build.note_fences(armed, waits);
    Ok(())
}
