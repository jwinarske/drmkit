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
    planes: HashMap<u32, HashMap<PropTag, u32>>,
    /// Properties the kernel marks immutable, which a commit must never write.
    ///
    /// Writing one is rejected with `EINVAL` whatever the value, so a single
    /// stray write poisons the whole commit — including every other layer in
    /// it.
    immutable: HashMap<u32, Vec<PropTag>>,
    /// Colorimetry properties, for the planes that expose them.
    color: HashMap<u32, ColorProps>,
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
) -> Result<usize, CoreError> {
    let mut written = 0;
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
        if !needs_write(tag, value, baseline) {
            continue;
        }
        request.add_property(plane_id, property_id, value)?;
        written += 1;
    }
    Ok(written)
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
        registry: &drmkit_planes::PlaneRegistry,
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
        for plane_id in &self.candidates {
            if assignment.iter().all(|(assigned, _)| assigned != plane_id)
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
        for (plane_id, layer) in assignment {
            if emit_layer(&mut request, self.map, *plane_id, layer.layer, None).is_err() {
                return Err(TestFailure::Rejected);
            }
        }

        match request.test(self.device, self.flags) {
            Ok(()) => Ok(()),
            Err(CoreError::NotMaster) => Err(TestFailure::NotMaster),
            Err(_) => Err(TestFailure::Rejected),
        }
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
pub fn emit_frame(
    request: &mut drmkit_core::AtomicRequest,
    map: &PlanePropertyMap,
    plan: &[crate::PlanePlan],
    disables: &[u32],
    modeset: Option<&Modeset<'_>>,
) -> Result<usize, CoreError> {
    let mut written = 0;
    if let Some(modeset) = modeset {
        written += modeset.emit(request)?;
    }
    for plane_id in disables {
        written += emit_disable(request, map, *plane_id)?;
    }
    for entry in plan {
        written += emit_layer(
            request,
            map,
            entry.plane_id,
            &entry.layer,
            entry.baseline.as_ref(),
        )?;
        written += emit_color_props(request, map, entry.plane_id)?;
    }
    Ok(written)
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
