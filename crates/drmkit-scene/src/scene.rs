// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! The scene: layers, their sources, and the two-phase commit.
//!
//! Ports the layer table and frame flow from `src/scene/layer_scene.cpp`.

use drmkit_planes::{Allocator, LayerRef};
use drmkit_planes::{
    Layer as PlaneLayer, LayerId, PlaneRegistry, PropTag, Rect, TestCommitter, TestFailure,
};
use std::collections::HashMap;

use drmkit_core::Device;
use drmkit_sync::SyncFence;

use crate::canvas::{CompositeCanvas, CompositeRect, CompositeSrc};
use crate::display::DisplayParams;
use crate::frame::{AcquireTally, CommitKind, FrameLifecycle, FrameOutcome, KernelResult};
use crate::lower::{LoweringInput, lower_layer};
use crate::release::{Acquisition, Released};
use crate::report::{CommitReport, LayerPlacement, Placement};
use crate::source::{BindingModel, LayerBufferSource, SourceError, SourceFormat};

/// Opaque, generation-tagged identity for a scene layer.
///
/// The scene recycles table slots, so a bare index would let a handle to a
/// removed layer address whatever took its place. The generation counter makes
/// that impossible: a stale handle no longer matches, and
/// [`layer`](LayerScene::layer) returns `None`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct LayerHandle {
    /// 1-based slot index; 0 is the invalid sentinel.
    id: u32,
    /// Bumped each time the slot is reused.
    generation: u32,
}

impl LayerHandle {
    /// Whether this is not the default sentinel.
    ///
    /// Says nothing about liveness — a valid handle can still be stale. Resolve
    /// with [`LayerScene::layer`].
    #[must_use]
    pub const fn is_valid(self) -> bool {
        self.id != 0
    }

    /// A stable identity for the allocator.
    ///
    /// Packs slot and generation, so a recycled slot yields a **different**
    /// id. That is what the allocator's warm-start state needs: a new layer
    /// must never be able to masquerade as the removed one whose place it took.
    #[must_use]
    pub const fn layer_id(self) -> LayerId {
        LayerId(((self.id as u64) << 32) | self.generation as u64)
    }
}

impl From<LayerHandle> for LayerId {
    /// The same packing [`LayerHandle::layer_id`] does.
    ///
    /// Here as well so anything taking a layer's identity can accept either,
    /// which is what lets a report be queried with a handle by a caller and
    /// with a bare id by a test that has no scene to get a handle from.
    fn from(handle: LayerHandle) -> Self {
        handle.layer_id()
    }
}

/// A layer's live scene state.
pub struct SceneLayer {
    source: Box<dyn LayerBufferSource>,
    display: DisplayParams,
    content_type: drmkit_planes::ContentType,
    update_hint_hz: u32,
    app_priority: u8,
    /// Set by the hint setters: these change plane *scoring*, not just the
    /// values written to a chosen plane, so the scene drops the allocator's
    /// warm start for that frame and lets the layer move.
    hints_dirty: bool,
    /// A plane the caller insists this layer scans out on.
    ///
    /// A *request*, not a guarantee: the plane has to be on this CRTC, take
    /// the layer's format, and not already be spoken for. When it is not, the
    /// layer falls back to normal allocation and the frame reports it through
    /// [`CommitReport::pin_requests_unhonored`] rather than dropping it — a
    /// caller's deterministic-plane assumption being violated is worth saying
    /// out loud, and worth saying more than it is worth blanking a layer over.
    pinned_plane: Option<u32>,
    /// Whether the caller insists this layer is composited rather than
    /// placed on a plane of its own.
    ///
    /// The allocator reads it through
    /// [`Layer::is_force_composited`](drmkit_planes::Layer::is_force_composited)
    /// and scores the layer out of plane candidacy. For a caller that knows
    /// something the allocator does not — content that must blend with what
    /// is under it, or a layer it would rather spend no plane on so the
    /// others have more to choose from.
    force_composited: bool,
    /// A caller-chosen identity, stable across a rebind.
    ///
    /// A [`LayerHandle`] identifies a layer *within one scene's lifetime*.
    /// This identifies what the layer **is** to the caller — a window, a
    /// video stream, a cursor — so a caller holding its own state can find its
    /// layer again without keeping a parallel map keyed by handle.
    ///
    /// Upstream uses a `const void*`, which works there because the caller
    /// already has a stable object to point at. A `u64` says the same thing
    /// without inviting a raw pointer into a type the scene stores.
    identity_tag: Option<u64>,
    /// Whether the geometry changed since the last commit.
    ///
    /// Separate from `hints_dirty`, which drops the warm start: geometry only
    /// changes what is written to the plane a layer already has. This exists
    /// so [`LayerScene::content_changed`] can tell a moved layer from an idle
    /// one, since a source with nothing new to say would otherwise make a
    /// repositioned layer look like no change at all.
    display_dirty: bool,
    /// The framebuffer this layer last put on screen.
    ///
    /// Kept so a frame where the source has nothing ready can re-attach it:
    /// the layer goes on showing what it already had, rather than being
    /// dropped from the commit and switched off.
    last_fb_id: Option<u32>,
}

impl std::fmt::Debug for SceneLayer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SceneLayer")
            .field("display", &self.display)
            .field("content_type", &self.content_type)
            .field("update_hint_hz", &self.update_hint_hz)
            .field("app_priority", &self.app_priority)
            .finish_non_exhaustive()
    }
}

impl SceneLayer {
    /// The buffer source. The scene keeps ownership.
    #[must_use]
    pub fn source(&self) -> &dyn LayerBufferSource {
        self.source.as_ref()
    }

    /// The buffer source, mutably — for painting into it, not for replacing it.
    pub fn source_mut(&mut self) -> &mut dyn LayerBufferSource {
        self.source.as_mut()
    }

    /// How this layer is displayed.
    #[must_use]
    pub const fn display(&self) -> &DisplayParams {
        &self.display
    }

    /// Change how this layer is displayed.
    ///
    /// Geometry only affects the values written to whatever plane the layer
    /// already has, so this does **not** drop the warm start.
    pub const fn set_display(&mut self, display: DisplayParams) {
        self.display = display;
        self.display_dirty = true;
    }

    /// Whether this layer is forced through composition.
    #[must_use]
    pub const fn is_force_composited(&self) -> bool {
        self.force_composited
    }

    /// Force this layer through composition rather than onto its own plane.
    ///
    /// Flags for re-allocation: it changes what the allocator will consider,
    /// and therefore what is left for every other layer.
    pub const fn set_force_composited(&mut self, force: bool) {
        self.force_composited = force;
        self.hints_dirty = true;
    }

    /// The caller's identity for this layer, if it set one.
    #[must_use]
    pub const fn identity_tag(&self) -> Option<u64> {
        self.identity_tag
    }

    /// Label this layer with a caller-chosen identity.
    ///
    /// Purely for the caller's own lookups — see
    /// [`identity_tag`](Self::identity_tag). The scene never interprets it,
    /// and does not require it to be unique; a duplicate simply means
    /// [`find_by_identity_tag`](LayerScene::find_by_identity_tag) answers with
    /// the first match.
    pub const fn set_identity_tag(&mut self, tag: Option<u64>) {
        self.identity_tag = tag;
    }

    /// The plane this layer is pinned to, if any.
    #[must_use]
    pub const fn pinned_plane(&self) -> Option<u32> {
        self.pinned_plane
    }

    /// Pin this layer to a plane, or clear the pin with `None`.
    ///
    /// For a caller that needs a specific plane rather than whichever one the
    /// allocator picks: a driver-bound producer, a plane with hardware the
    /// others lack, or a layout the caller has already validated. Honoured
    /// where it can be, reported where it cannot — see
    /// [`pinned_plane`](Self::pinned_plane).
    ///
    /// Flags for re-allocation, because it changes where this layer goes and
    /// therefore what is left for everything else.
    pub const fn set_pinned_plane(&mut self, plane_id: Option<u32>) {
        self.pinned_plane = plane_id;
        self.hints_dirty = true;
    }

    /// The allocator's content-type hint.
    #[must_use]
    pub const fn content_type(&self) -> drmkit_planes::ContentType {
        self.content_type
    }

    /// Change the content-type hint.
    ///
    /// Unlike the display setters this changes plane **scoring**, so it also
    /// flags the layer for re-allocation: the scene drops the warm start that
    /// frame and lets the layer move to a plane its new hint prefers.
    pub const fn set_content_type(&mut self, content_type: drmkit_planes::ContentType) {
        self.content_type = content_type;
        self.hints_dirty = true;
    }

    /// The producer's expected refresh rate, or 0.
    #[must_use]
    pub const fn update_hint_hz(&self) -> u32 {
        self.update_hint_hz
    }

    /// Change the refresh-rate hint. Flags for re-allocation, as above.
    pub const fn set_update_hint(&mut self, hz: u32) {
        self.update_hint_hz = hz;
        self.hints_dirty = true;
    }

    /// The application's placement priority within a content class.
    #[must_use]
    pub const fn app_priority(&self) -> u8 {
        self.app_priority
    }

    /// Change the placement priority. Flags for re-allocation, as above.
    pub const fn set_app_priority(&mut self, priority: u8) {
        self.app_priority = priority;
        self.hints_dirty = true;
    }

    /// Whether a hint changed since the last commit.
    #[must_use]
    pub const fn hints_dirty(&self) -> bool {
        self.hints_dirty
    }
}

/// A table slot: either occupied or free, with a generation either way.
#[derive(Debug)]
enum Slot {
    Occupied(Box<SceneLayer>),
    Free,
}

/// What a [`rebind`](LayerScene::rebind) found that will not fit.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RebindReport {
    /// Layers the new output cannot display as they stand.
    pub incompatibilities: Vec<LayerIncompatibility>,
}

impl RebindReport {
    /// Whether every layer fits the new output.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.incompatibilities.is_empty()
    }
}

/// One layer the new output cannot display as it stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LayerIncompatibility {
    /// Which layer. Still valid: a rebind does not invalidate handles.
    pub handle: LayerHandle,
    /// Why it does not fit.
    pub reason: IncompatibilityReason,
}

/// Why a layer does not fit its new output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum IncompatibilityReason {
    /// The destination rectangle falls outside the new mode.
    ///
    /// Reported rather than clamped. A layer at the old mode's bottom-right
    /// is not *wrong*, it is somewhere the caller now has to decide about, and
    /// silently moving it would put a window somewhere the caller never asked
    /// for and cannot detect.
    DstRectOffScreen,
}

/// One plane's share of a built frame.
#[derive(Debug, Clone)]
pub struct PlanePlan {
    /// The plane this layer landed on.
    pub plane_id: u32,
    /// Which layer it is, for the allocator's committed baseline.
    pub layer_id: LayerId,
    /// The lowered property bag to write.
    pub layer: drmkit_planes::Layer,
    /// What the kernel last took on this plane, or `None` to write it all.
    ///
    /// Captured at build time on purpose: the baseline is replaced when the
    /// frame is finalized, so reading it afterwards would diff this frame
    /// against itself and suppress every property.
    pub baseline: Option<drmkit_planes::PropertySnapshot>,
    /// What this frame's source said changed, in destination pixels.
    ///
    /// Empty means whole-frame, which is what an absent `FB_DAMAGE_CLIPS`
    /// already says -- so an empty list writes nothing rather than an empty
    /// blob, which the kernel reads as *nothing changed* and would leave the
    /// frame unrepainted.
    pub damage: Vec<crate::DamageRect>,
    /// The zpos to write in place of the layer's own: the frame's stack,
    /// numbered densely over the planes it arms (see
    /// [`stacked_zpos`](drmkit_planes::stacked_zpos)). `None` where the plane
    /// is not ranked, and the layer's own value goes out.
    pub zpos: Option<u64>,
}

/// One pass over the sources: what they gave, and what it cost.
struct Acquired {
    /// Layers holding their previous frame because the source had none.
    starved: Vec<LayerId>,
    /// Buffers taken this frame, owed back after the commit.
    acquisitions: Vec<Acquisition>,
    /// Each layer lowered into the property bag the allocator reads.
    plane_layers: Vec<(LayerId, PlaneLayer)>,
    /// What each source said changed, for `FB_DAMAGE_CLIPS`.
    frame_damage: HashMap<LayerId, Vec<crate::DamageRect>>,
    /// Pin requests the hardware could not honour.
    pins_unhonored: usize,
    /// Pins that were honoured, layer to plane.
    pinned: Vec<(LayerId, u32)>,
}

/// A frame that has been built and is awaiting the kernel's answer.
///
/// Holding one means holding acquisitions. **Dropping it without finalizing
/// leaks them** — the same contract the C++ states, and the reason this type
/// warns on drop.
#[derive(Debug)]
pub struct FrameBuild {
    pub(crate) acquisitions: Vec<Acquisition>,
    pub(crate) plan: Vec<PlanePlan>,
    disables: Vec<u32>,
    report: CommitReport,
    kind: CommitKind,
    finalized: bool,
}

impl FrameBuild {
    /// The report so far, complete except for what the kernel's answer decides.
    #[must_use]
    pub const fn report(&self) -> &CommitReport {
        &self.report
    }

    /// Record what this frame's emission wrote.
    ///
    /// Called by [`emit_frame`](crate::emit_frame), which is the only thing
    /// that knows: the counts are decided by the diff against what the kernel
    /// last took, and that happens at emission rather than at build time.
    pub(crate) const fn record_emission(
        &mut self,
        properties: usize,
        framebuffers: usize,
        damaged: usize,
    ) {
        self.report.properties_written = properties;
        self.report.fbs_attached = framebuffers;
        self.report.damaged_layers = damaged;
    }

    /// How many buffers this frame is holding.
    #[must_use]
    pub fn held(&self) -> usize {
        self.acquisitions.len()
    }

    /// Which layer landed on which plane, as lowered property bags.
    ///
    /// This is what an apply commit writes. The search that produced it ran
    /// against `TEST_ONLY` commits, so by the time a caller sees this the
    /// kernel has already accepted this exact set -- emitting it is the cheap
    /// part.
    #[must_use]
    pub fn plan(&self) -> &[PlanePlan] {
        &self.plan
    }

    /// Record what the fence arming pass did, for the commit report.
    pub(crate) fn note_fences(&mut self, armed: usize, cpu_waits: usize) {
        self.report.in_fences_armed = armed;
        self.report.in_fence_cpu_waits = cpu_waits;
    }

    /// Planes this frame has to switch off.
    ///
    /// Candidate planes the frame did not use **and** that the kernel does not
    /// already have off. Re-disabling an already-off plane is two property
    /// writes per plane per frame that change nothing.
    ///
    /// Computed at build time for the same reason as
    /// [`PlanePlan::baseline`]: finalizing the frame replaces the state this
    /// is derived from.
    #[must_use]
    pub fn disables(&self) -> &[u32] {
        &self.disables
    }
}

impl Drop for FrameBuild {
    fn drop(&mut self) {
        if !self.finalized && !self.acquisitions.is_empty() && !std::thread::panicking() {
            // Same hazard as invariant 5's, one level up: these buffers never
            // go back to their sources, so the producer's ring starves. There
            // is nothing safe to do about it here -- the sources live in the
            // scene, which is not reachable from a drop -- so say so loudly.
            //
            // Not while already unwinding, though: a second panic during
            // cleanup aborts the process, which replaces the failure the
            // caller needs to read with this one.
            debug_assert!(
                false,
                "FrameBuild dropped without finalize_frame: {} acquisitions leaked",
                self.acquisitions.len()
            );
        }
    }
}

/// Why a frame could not be built.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SceneError {
    /// The scene is suspended after losing DRM master, and short-circuits until
    /// resumed.
    #[error("scene is suspended")]
    Suspended,

    /// A source failed for a real reason. Distinguished from a source that
    /// merely had no frame, which is not an error at all.
    #[error("layer source failed: {0}")]
    Source(#[from] SourceError),

    /// The allocator could not run because DRM master was lost mid-search.
    #[error("lost DRM master during allocation")]
    NotMaster,

    /// A [`rebind`](LayerScene::rebind) to another CRTC left planes lit on the
    /// old one, and they have not been turned off yet. Commit
    /// [`commit_detach`](crate::commit_detach) first.
    #[error("planes from the previous CRTC are still lit; commit the detach first")]
    DetachPending,
}

/// One CRTC's layers and the commit machinery around them.
///
/// Layers are added once and kept: [`add_layer`](Self::add_layer) returns a
/// [`LayerHandle`] that stays valid until the layer is removed, and a stale
/// one resolves to nothing rather than to whatever now occupies the slot. Per
/// frame, the caller changes what it wants — geometry through
/// [`set_display`](SceneLayer::set_display), content through the source — and
/// asks for a [`build_frame`](Self::build_frame).
///
/// The scene is deliberately not a commit loop. It builds a plan and reads the
/// kernel's answer; issuing the commit, attaching a modeset, and dispatching
/// the page-flip event belong to the caller, because a compositor has its own
/// ideas about all three. See the crate documentation for the shape of a
/// frame.
///
/// Four of the six pinned invariants are the scene's: buffers released a flip
/// late rather than an ioctl early (1), a failed commit releasing immediately
/// because no flip will gate it (2), the FB-only fast path (4), and teardown
/// draining an outstanding flip before letting go of what it scans out (5).
/// None of them is something a caller opts into; they are what the type does.
pub struct LayerScene {
    crtc_id: u32,
    slots: Vec<Slot>,
    generations: Vec<u32>,
    free: Vec<u32>,
    allocator: Allocator,
    canvas: Option<CompositeCanvas>,
    lifecycle: FrameLifecycle,
    /// Sources whose layers were removed while their buffers were still in
    /// flight.
    ///
    /// Dropping such a source immediately would strand the buffers: the release
    /// would have nowhere to go, and any handles it owns would be freed while
    /// the display engine may still be scanning them. They are kept until every
    /// buffer has come back.
    retiring: Vec<(LayerId, Box<dyn LayerBufferSource>)>,
    /// Whether a layer was added or removed since the last commit.
    topology_dirty: bool,
    /// Whether any real commit has succeeded yet.
    ///
    /// Until one has, the scanout contents are undefined and *nothing changed*
    /// describes nothing -- so the first frame is never skipped, however
    /// quiet the sources are.
    committed_once: bool,
    /// Whether the kernel may hold planes on this CRTC that the scene did not
    /// light: until a real commit lands, and again after a rebind or a
    /// resume.
    ///
    /// Another client -- the session the scene took over from, a splash, a
    /// compositor that held the CRTC while the scene was suspended -- can
    /// leave a plane armed, and the allocator's baseline, knowing nothing of
    /// it, reports it off. Every `TEST_ONLY` disables it, so the frame the
    /// kernel accepts is one without it; without this the real commit leaves
    /// it lit, on screen over or under the scene and counted against a
    /// controller's plane limit.
    planes_unknown: bool,
    /// The plane the canvas was armed on last frame, if composition ran: the
    /// canvas's first preference this frame (drm-cxx `6d787f9`). Forgotten on
    /// a frame that composites nothing, and on rebind and resume.
    last_canvas_plane: Option<u32>,
    /// What the kernel said about each pin it was asked about, so a pin is
    /// tested once per layer, plane and geometry (drm-cxx `c766e97`).
    /// Forgotten on rebind and resume.
    pin_verdicts: Vec<PinVerdict>,
    /// What a rebind to another CRTC left lit on the old one.
    detach: Option<PendingDetach>,
}

/// Planes a scene left lit on the CRTC it moved away from.
///
/// The kernel will not move a plane from one CRTC to another in one commit
/// ("switching CRTC directly"), so one both pipes can use has to be turned off
/// in a commit of its own before the new CRTC can have it. And nothing else
/// would turn them off: the baseline that recorded them describes the old
/// output, and goes with it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingDetach {
    /// The CRTC the scene left.
    pub crtc_id: u32,
    /// The planes it had lit there.
    pub planes: Vec<u32>,
}

impl std::fmt::Debug for LayerScene {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LayerScene")
            .field("crtc_id", &self.crtc_id)
            .field("layers", &self.len())
            .field("suspended", &self.lifecycle.is_suspended())
            .field("retiring_sources", &self.retiring.len())
            .finish_non_exhaustive()
    }
}

impl LayerScene {
    /// A scene driving `crtc_id`, with no layers.
    #[must_use]
    pub fn new(crtc_id: u32) -> Self {
        Self {
            crtc_id,
            slots: Vec::new(),
            generations: Vec::new(),
            free: Vec::new(),
            allocator: Allocator::new(),
            canvas: None,
            lifecycle: FrameLifecycle::new(),
            retiring: Vec::new(),
            topology_dirty: true,
            committed_once: false,
            planes_unknown: true,
            last_canvas_plane: None,
            pin_verdicts: Vec::new(),
            detach: None,
        }
    }

    /// The CRTC this scene drives.
    #[must_use]
    pub const fn crtc_id(&self) -> u32 {
        self.crtc_id
    }

    /// How many layers the scene holds.
    #[must_use]
    pub fn len(&self) -> usize {
        self.slots
            .iter()
            .filter(|slot| matches!(slot, Slot::Occupied(_)))
            .count()
    }

    /// Whether the scene holds no layers.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Whether the scene is suspended after losing DRM master.
    #[must_use]
    pub const fn is_suspended(&self) -> bool {
        self.lifecycle.is_suspended()
    }

    /// Whether a page-flip event is armed and undispatched.
    ///
    /// **Invariant 5.** Dropping the scene while this is true tears down a
    /// framebuffer the flip still references.
    #[must_use]
    pub const fn has_pending_flip(&self) -> bool {
        self.lifecycle.has_pending_flip()
    }

    /// Record that the armed page-flip event was dispatched.
    pub const fn flip_landed(&mut self) {
        self.lifecycle.flip_landed();
    }

    /// Lift a suspension after the session resumes.
    ///
    /// Whoever held the CRTC meanwhile may have left planes lit, so the next
    /// frame turns off every plane it does not use.
    pub fn resume(&mut self) {
        self.lifecycle.resume();
        self.planes_unknown = true;
        self.last_canvas_plane = None;
        self.pin_verdicts.clear();
    }

    /// Add a layer backed by `source`.
    pub fn add_layer(&mut self, source: Box<dyn LayerBufferSource>) -> LayerHandle {
        self.topology_dirty = true;
        let layer = Box::new(SceneLayer {
            source,
            display: DisplayParams::default(),
            content_type: drmkit_planes::ContentType::Generic,
            update_hint_hz: 0,
            app_priority: 0,
            hints_dirty: false,
            pinned_plane: None,
            force_composited: false,
            identity_tag: None,
            display_dirty: true,
            last_fb_id: None,
        });

        if let Some(slot_index) = self.free.pop() {
            let index = slot_index as usize - 1;
            self.slots[index] = Slot::Occupied(layer);
            return LayerHandle {
                id: slot_index,
                generation: self.generations[index],
            };
        }

        self.slots.push(Slot::Occupied(layer));
        self.generations.push(1);
        let id = u32::try_from(self.slots.len()).unwrap_or(u32::MAX);
        LayerHandle { id, generation: 1 }
    }

    /// Remove a layer. A stale handle is a no-op.
    ///
    /// If the layer's buffers are still in flight, its source is kept alive
    /// until they come back — releasing to a dropped source would strand them.
    pub fn remove_layer(&mut self, handle: LayerHandle) -> bool {
        let Some(index) = self.resolve(handle) else {
            return false;
        };

        let Slot::Occupied(layer) = std::mem::replace(&mut self.slots[index], Slot::Free) else {
            return false;
        };

        // Bump first, so any handle to this slot is stale from here on.
        self.generations[index] = self.generations[index].wrapping_add(1);
        self.free.push(handle.id);

        if self.lifecycle.buffers_in_flight() > 0 {
            self.topology_dirty = true;
            self.retiring.push((handle.layer_id(), layer.source));
        }

        // Prune just this layer rather than dropping the whole warm-start
        // cache. Removing a layer only frees resources, so the assignment the
        // kernel already accepted stays valid without it -- invalidating would
        // force a full search, and a full search is several test commits.
        self.allocator.forget_layer(handle.layer_id());
        true
    }

    /// Borrow a layer, or `None` if the handle is stale.
    #[must_use]
    pub fn layer(&self, handle: LayerHandle) -> Option<&SceneLayer> {
        let index = self.resolve(handle)?;
        match &self.slots[index] {
            Slot::Occupied(layer) => Some(layer),
            Slot::Free => None,
        }
    }

    /// Borrow a layer mutably, or `None` if the handle is stale.
    pub fn layer_mut(&mut self, handle: LayerHandle) -> Option<&mut SceneLayer> {
        let index = self.resolve(handle)?;
        match &mut self.slots[index] {
            Slot::Occupied(layer) => Some(layer),
            Slot::Free => None,
        }
    }

    /// Every live handle, in slot order.
    pub fn handles(&self) -> impl Iterator<Item = LayerHandle> + '_ {
        self.slots
            .iter()
            .enumerate()
            .filter_map(|(index, slot)| match slot {
                Slot::Occupied(_) => Some(LayerHandle {
                    id: u32::try_from(index + 1).unwrap_or(u32::MAX),
                    generation: self.generations[index],
                }),
                Slot::Free => None,
            })
    }

    /// Resolve a handle to a slot index, checking the generation.
    fn resolve(&self, handle: LayerHandle) -> Option<usize> {
        if !handle.is_valid() {
            return None;
        }
        let index = handle.id as usize - 1;
        if self.generations.get(index).copied()? != handle.generation {
            return None;
        }
        Some(index)
    }

    /// Acquire every layer's buffer and lower it into a property bag.
    ///
    /// Split out of [`build_frame`](Self::build_frame) because it is the one
    /// pass that touches the sources, and because everything after it works
    /// only from what this returns.
    fn acquire_every_layer(
        &mut self,
        registry: &PlaneRegistry,
        crtc_index: u32,
        tally: &mut AcquireTally,
    ) -> Result<Acquired, SceneError> {
        let mut starved: Vec<LayerId> = Vec::new();
        let mut acquisitions = Vec::new();
        let mut plane_layers: Vec<(LayerId, PlaneLayer)> = Vec::new();
        let mut frame_damage: HashMap<LayerId, Vec<crate::DamageRect>> = HashMap::new();
        let mut pins_unhonored = 0usize;
        // Honoured pins, layer to plane. The allocator skips these layers by
        // design -- the scene owns their planes -- so the plan and the report
        // have to carry them, or a pinned layer is programmed by nobody and
        // reported by nobody while still being counted as considered.
        let mut pinned: Vec<(LayerId, u32)> = Vec::new();

        for handle in self.handles().collect::<Vec<_>>() {
            let layer_id = handle.layer_id();
            let Some(index) = self.resolve(handle) else {
                continue;
            };
            let Slot::Occupied(layer) = &mut self.slots[index] else {
                continue;
            };

            let acquired = match layer.source.acquire() {
                Ok(buffer) => buffer,
                Err(SourceError::WouldBlock) => {
                    // Flow control, not failure: the source has no frame this
                    // vblank. The layer keeps its plane and its last
                    // framebuffer, so it goes on showing what it already had.
                    //
                    // Dropping it from the frame instead would take its plane
                    // out of the assignment and the commit would disable it --
                    // a layer whose source hiccups would blink off, which is
                    // the visible failure this path exists to avoid.
                    tally.record(false);
                    let Some(fb_id) = layer.last_fb_id else {
                        // Starved before it ever produced anything. There is
                        // no previous frame to hold, so there is nothing to
                        // program and the layer really is absent.
                        continue;
                    };
                    starved.push(layer_id);
                    let (mut lowered, pin) =
                        lower(layer, fb_id, self.crtc_id, registry, crtc_index);
                    let pin = revoke_refused_pin(&self.pin_verdicts, layer_id, &mut lowered, pin);
                    match pin {
                        PinOutcome::Refused => pins_unhonored += 1,
                        PinOutcome::Honoured(plane_id) => pinned.push((layer_id, plane_id)),
                        PinOutcome::NotRequested => {}
                    }
                    plane_layers.push((layer_id, lowered));
                    continue;
                }
                Err(other) => {
                    // Hand back whatever this pass already took: the commit is
                    // not happening, so holding them would stall those rings.
                    self.release_all(acquisitions);
                    return Err(SceneError::Source(other));
                }
            };
            tally.record(true);

            let (mut plane_layer, pin) =
                lower(layer, acquired.fb_id, self.crtc_id, registry, crtc_index);
            let pin = revoke_refused_pin(&self.pin_verdicts, layer_id, &mut plane_layer, pin);
            match pin {
                PinOutcome::Refused => pins_unhonored += 1,
                PinOutcome::Honoured(plane_id) => pinned.push((layer_id, plane_id)),
                PinOutcome::NotRequested => {}
            }

            layer.last_fb_id = Some(acquired.fb_id);
            plane_layers.push((layer_id, plane_layer));
            // Taken before the acquisition owns it: the damage describes this
            // frame and is consumed at emission, whereas the acquisition
            // outlives the commit by two generations.
            if !acquired.damage.is_empty() {
                frame_damage.insert(layer_id, acquired.damage.clone());
            }
            acquisitions.push(Acquisition::new(layer_id, acquired));
        }

        Ok(Acquired {
            starved,
            acquisitions,
            plane_layers,
            frame_damage,
            pins_unhonored,
            pinned,
        })
    }

    /// Acquire from every layer, lower it, and run the allocator.
    ///
    /// A source with nothing to contribute is **skipped and counted**, never
    /// treated as a failure — see [`SourceError::WouldBlock`].
    ///
    /// # Errors
    ///
    /// - [`SceneError::Suspended`] while the scene is suspended.
    /// - [`SceneError::Source`] if a source failed for a real reason.
    /// - [`SceneError::NotMaster`] if the allocator lost DRM master.
    pub fn build_frame<C: TestCommitter>(
        &mut self,
        registry: &PlaneRegistry,
        crtc_index: u32,
        kind: CommitKind,
        committer: &mut C,
    ) -> Result<FrameBuild, SceneError> {
        if self.lifecycle.is_suspended() {
            return Err(SceneError::Suspended);
        }
        if self.detach.is_some() {
            return Err(SceneError::DetachPending);
        }

        // A changed hint affects plane scoring, so the warm start must go or
        // the layer can never move to the plane it now prefers.
        if self.handles().any(|h| {
            self.layer(h)
                .is_some_and(super::scene::SceneLayer::hints_dirty)
        }) {
            self.allocator.invalidate_allocation();
        }

        let mut tally = AcquireTally::default();
        // Layers holding their previous frame. They are programmed but not
        // acquired, so the report must not count them as assigned -- the
        // identity is assigned + composited + unassigned + skipped.
        let Acquired {
            starved,
            acquisitions,
            plane_layers,
            frame_damage,
            pins_unhonored,
            mut pinned,
        } = self.acquire_every_layer(registry, crtc_index, &mut tally)?;
        let mut plane_layers = plane_layers;
        let (allocation, refused) = match self.allocate_frame(
            &mut plane_layers,
            &mut pinned,
            registry,
            crtc_index,
            committer,
        ) {
            Ok(allocated) => allocated,
            Err(error) => {
                self.release_all(acquisitions);
                return Err(error);
            }
        };
        let pins_unhonored = pins_unhonored + refused;

        // Rescue what the allocator dropped, before the plan is built: the
        // canvas takes a plane, and a plane carrying the canvas must not also
        // appear in the disable pass.
        let canvas_plane = self.compose_unassigned(&allocation, registry, crtc_index);
        self.last_canvas_plane = canvas_plane.as_ref().map(|c| c.plane_id);
        // What actually landed in the canvas, not what the allocator dropped.
        // A layer whose source the CPU cannot read, or whose format the blend
        // does not handle, is dropped *and* unrescued -- counting it composited
        // would report a layer as on screen when it is not. The identity holds
        // either way, so only the split tells them apart.
        let composited = canvas_plane.as_ref().map_or(0, |c| c.blended);

        let report = build_report(
            &tally,
            &allocation,
            registry,
            &starved,
            composited,
            &pinned,
            pins_unhonored,
        );

        // Clear the change flags now the allocation has seen them.
        for handle in self.handles().collect::<Vec<_>>() {
            if let Some(layer) = self.layer_mut(handle) {
                layer.hints_dirty = false;
                layer.display_dirty = false;
            }
        }
        self.topology_dirty = false;
        self.committed_once = true;

        let (mut plan, mut disables) = build_plan(
            &self.allocator,
            &allocation,
            &plane_layers,
            registry,
            crtc_index,
            &frame_damage,
            &pinned,
        );
        self.disable_foreign(&mut disables, &plan, registry, crtc_index, committer);
        // A scene with no layers keeps its planes, and the last frame stays
        // up: an active CRTC's only primary cannot be disabled on some
        // controllers (i.MX LCDIF), so turning it off would get the commit
        // refused (drm-cxx `d895c8d`). A scene whose layers were replaced
        // still turns off the planes they left.
        if self.is_empty() {
            disables.clear();
        }
        let disables = if let Some(composition) = canvas_plane {
            let plane_id = composition.plane_id;
            plan.push(PlanePlan {
                plane_id,
                // The canvas is the scene's own surface, not any one layer's,
                // so it carries no layer identity and no committed baseline:
                // every property is written every frame it is armed.
                layer_id: LayerId(0),
                layer: composition.layer,
                baseline: None,
                // The canvas is composited fresh each frame it is armed, and
                // its content is the union of what it rescued -- there is no
                // one source's damage to report, so it repaints whole.
                damage: Vec::new(),
                zpos: None,
            });
            disables.into_iter().filter(|id| *id != plane_id).collect()
        } else {
            disables
        };

        // One stack over everything armed -- allocated, pinned and the canvas,
        // which asks to sit above every layer and so lands just above the
        // highest armed one. Numbering only the armed planes is what keeps
        // composited layers from using up the range (drm-cxx `1384526`).
        let armed: Vec<(u32, Option<u64>)> = plan
            .iter()
            .map(|entry| (entry.plane_id, entry.layer.property(PropTag::Zpos)))
            .collect();
        for (plane_id, zpos) in drmkit_planes::stacked_zpos(registry, &armed) {
            if let Some(entry) = plan.iter_mut().find(|entry| entry.plane_id == plane_id) {
                entry.zpos = Some(zpos);
            }
        }

        Ok(FrameBuild {
            acquisitions,
            plan,
            disables,
            report,
            kind,
            finalized: false,
        })
    }

    /// Turn off the planes another client left lit, while the scene does not
    /// yet know what the kernel holds (see `planes_unknown`).
    ///
    /// Asks the committer what is lit on this CRTC; one that cannot tell gets
    /// every plane the frame does not use turned off, as every test does. Not
    /// for an empty frame: that keeps whatever is there, since an active
    /// CRTC's only primary cannot be disabled on some controllers (i.MX
    /// LCDIF), the rule upstream's `d895c8d` keeps too.
    fn disable_foreign<C: TestCommitter>(
        &self,
        disables: &mut Vec<u32>,
        plan: &[PlanePlan],
        registry: &PlaneRegistry,
        crtc_index: u32,
        committer: &mut C,
    ) {
        if !self.planes_unknown || plan.is_empty() {
            return;
        }
        let candidates: Vec<u32> = registry
            .force_disable_candidates(crtc_index)
            .map(|plane| plane.id)
            .collect();
        let lit = committer
            .lit_planes(self.crtc_id)
            .unwrap_or_else(|| candidates.clone());
        for plane_id in lit {
            if candidates.contains(&plane_id)
                && !disables.contains(&plane_id)
                && !plan.iter().any(|entry| entry.plane_id == plane_id)
            {
                disables.push(plane_id);
            }
        }
    }

    /// Give the scene a composition canvas.
    ///
    /// Layers the allocator cannot place are blended into this surface and it
    /// goes on a plane of its own. Without one they are reported unassigned
    /// and simply do not reach the screen.
    ///
    /// Separate from [`new`](Self::new) because the search itself is
    /// device-free and this is not: the canvas owns two dumb buffers.
    ///
    /// The canvas is `ARGB8888`, which every plane that matters takes.
    /// [`enable_composition_on`](Self::enable_composition_on) is for the ones
    /// that do not.
    ///
    /// # Errors
    ///
    /// `drmkit_dumb::DumbError` if either buffer cannot be allocated.
    pub fn enable_composition(
        &mut self,
        device: &Device,
        width: u32,
        height: u32,
    ) -> Result<(), drmkit_dumb::DumbError> {
        self.canvas = Some(CompositeCanvas::create(device, width, height)?);
        Ok(())
    }

    /// Give the scene a canvas in a format `plane` can scan out.
    ///
    /// On a controller with one plane and no `ARGB8888` -- tilcdc on a
    /// `BeagleBone`, i.MX LCDIF, the small SPI panels -- an `ARGB8888` canvas
    /// cannot be armed at all, so every layer that overflows the plane count
    /// is dropped rather than composited. This negotiates instead, and blends
    /// in `ARGB8888` regardless: the conversion happens once at flush rather
    /// than quantizing at every layer.
    ///
    /// # Errors
    ///
    /// `drmkit_dumb::DumbError::InvalidConfig` if the plane advertises nothing
    /// the canvas
    /// can write -- a YUV-only overlay, say -- and otherwise as
    /// [`enable_composition`](Self::enable_composition).
    pub fn enable_composition_on(
        &mut self,
        device: &Device,
        width: u32,
        height: u32,
        plane: &drmkit_planes::PlaneCapabilities,
    ) -> Result<(), drmkit_dumb::DumbError> {
        let fourcc = crate::canvas::canvas_format_for_plane(plane).ok_or(
            drmkit_dumb::DumbError::InvalidConfig {
                reason: "this plane scans out nothing the canvas can write",
            },
        )?;
        self.canvas = Some(CompositeCanvas::create_in(device, width, height, fourcc)?);
        Ok(())
    }

    /// What the composition canvas scans out as, if there is one.
    #[must_use]
    pub fn canvas_fourcc(&self) -> Option<u32> {
        self.canvas.as_ref().map(CompositeCanvas::fourcc)
    }

    /// Blend the layers the allocator dropped, and say which plane to put them
    /// on.
    ///
    /// Returns the canvas plane and its lowered properties, or `None` when
    /// nothing needs compositing or the frame cannot rescue them.
    ///
    /// The rescue is best-effort and silent by design, matching upstream: no
    /// canvas, no free plane, or a source that cannot be read leaves those
    /// layers off the screen for this frame rather than failing the commit.
    /// That asymmetry is deliberate — the canvas's properties go into the same
    /// atomic request as everything else, so once it is armed a kernel
    /// rejection takes the whole frame down, including every layer that *was*
    /// placed. Dropping one layer beats dropping all of them.
    /// Allocate planes, holding one back for the composition canvas when the
    /// frame turns out to need one.
    ///
    /// The canvas takes a plane, and until this ran nothing had validated a
    /// frame that included it: the allocator tests the layer assignment, and
    /// `compose_unassigned` adds the canvas afterwards. On hardware that can
    /// light fewer planes than it advertises the difference is fatal --
    /// rockchip's VOP2 on an RK3566 offers three planes and accepts two, so a
    /// validated two-plane assignment plus a canvas is a frame the kernel
    /// refuses, and a refused frame puts nothing on screen at all.
    ///
    /// [`canvas_reservation`] alone cannot see this. It reserves when the
    /// layer count exceeds the *candidate* count, a proxy for "the canvas will
    /// be needed" that says no here: two layers, two candidates, and a
    /// hardware limit of two including the canvas. So when composition turns
    /// out to be needed and nothing was reserved up front, hold a plane back
    /// and allocate again. The second pass runs only on frames that composite.
    ///
    /// This narrows the problem rather than closing it. Where the hardware
    /// limit is at or below the candidate count, `assigned` is already the
    /// most the kernel would take, so `assigned` plus a canvas overshoots
    /// whatever is held back. Closing it means putting the canvas plane into
    /// the assignments the allocator tests -- see P-18 and drm-cxx#244.
    fn allocate_with_canvas<C: TestCommitter>(
        &mut self,
        refs: &[LayerRef<'_>],
        registry: &PlaneRegistry,
        crtc_index: u32,
        committer: &mut C,
    ) -> Result<drmkit_planes::Allocation, SceneError> {
        let reserved = canvas_reservation(
            self.canvas.is_some(),
            refs,
            registry,
            crtc_index,
            self.last_canvas_plane,
        );
        self.allocator.set_reserved_planes(&reserved);
        // A plane reserved up front is the canvas's, so the search arms the
        // canvas there in its tests. Without that the tests left it out, and
        // where the kernel lights fewer planes than it offers the frame
        // committed one plane over what it took (the SA8155P, P-18).
        self.allocator.hold_canvas_plane(reserved.first().copied());
        // Where planes stack by id the allocator picks the canvas plane
        // itself, so it has to know which ones can carry it.
        let hosts: Vec<u32> = self.canvas.as_ref().map_or_else(Vec::new, |canvas| {
            registry
                .force_disable_candidates(crtc_index)
                .filter(|plane| plane.supports_format(canvas.fourcc()))
                .map(|plane| plane.id)
                .collect()
        });
        let canvas_layer = self
            .canvas
            .as_ref()
            .and_then(|canvas| lower_canvas(canvas, self.crtc_id, self.canvas_zpos()));
        self.allocator.set_canvas(&hosts, canvas_layer.as_ref());

        let mut allocation = match self
            .allocator
            .allocate(refs, registry, crtc_index, committer)
        {
            Ok(allocation) => allocation,
            Err(TestFailure::NotMaster) => return Err(SceneError::NotMaster),
            // The allocator handles ordinary rejections internally, so this is
            // unreachable in practice; treating it as "nothing placed" keeps
            // the frame honest rather than panicking.
            Err(TestFailure::Rejected) => drmkit_planes::Allocation::default(),
        };

        // The canvas already has a plane it was tested on: the one held up
        // front, the one a cached assignment kept, or, where planes stack by
        // id, the one between the run's neighbors. A spare held back there
        // would sit wherever its id puts it.
        if self.canvas.is_none()
            || allocation.composited.is_empty()
            || self.allocator.canvas_plane().is_some()
            || drmkit_planes::stacks_by_plane_id(registry, crtc_index)
        {
            return Ok(allocation);
        }
        // Not a multirect virtual plane: held, it is armed in every test of
        // the second pass, and the search may give its parent away.
        let Some(spare) = canvas_plane_order(&hosts, registry, crtc_index, self.last_canvas_plane)
            .find(|id| {
                allocation.assignment.get(*id).is_none() && !is_multirect_virtual(registry, *id)
            })
        else {
            return Ok(allocation);
        };

        let first_pass_tests = allocation.diagnostics.test_commits_issued;
        let first_pass_budget = allocation.diagnostics.budget_exhausted;
        // Hold the spare for the canvas, which arms it in every test of the
        // second pass. This is what makes the search account for it: the
        // allocator stops when the kernel accepts, and now what the kernel is
        // accepting is the whole frame rather than the frame minus a plane.
        // The first pass's assignment does not use the spare, so its warm
        // start re-tests that assignment with the canvas armed, and searches
        // only if the kernel refuses the pair.
        self.allocator.set_reserved_planes(&[spare]);
        self.allocator.hold_canvas_plane(Some(spare));
        let outcome = self
            .allocator
            .allocate(refs, registry, crtc_index, committer);
        match outcome {
            Ok(mut second) => {
                // `allocate` zeroes its own per-frame counter, so without this
                // the report would show the second pass's cost and hide the
                // first.
                second.diagnostics.test_commits_issued += first_pass_tests;
                // Nor its verdict: the second pass re-tests what the first
                // settled on, so a budget that bound the first decided what
                // is composited, and the report has to say so.
                second.diagnostics.budget_exhausted |= first_pass_budget;
                allocation = second;
            }
            Err(TestFailure::NotMaster) => return Err(SceneError::NotMaster),
            // Keep the first pass rather than dropping the frame: a refused
            // second attempt is worse than an unvalidated canvas, which at
            // least usually works.
            Err(TestFailure::Rejected) => {}
        }
        Ok(allocation)
    }

    /// Allocate planes, then test the pins: the allocation, and how many
    /// pins the kernel refused.
    fn allocate_frame<C: TestCommitter>(
        &mut self,
        plane_layers: &mut [(LayerId, PlaneLayer)],
        pinned: &mut Vec<(LayerId, u32)>,
        registry: &PlaneRegistry,
        crtc_index: u32,
        committer: &mut C,
    ) -> Result<(drmkit_planes::Allocation, usize), SceneError> {
        lower_bottom_layer_to_primary_slot(plane_layers, registry, crtc_index);
        let refs: Vec<LayerRef<'_>> = plane_layers
            .iter()
            .map(|(id, layer)| LayerRef { id: *id, layer })
            .collect();
        let mut allocation = self.allocate_with_canvas(&refs, registry, crtc_index, committer)?;
        drop(refs);
        let refused = self.test_pins(&mut allocation, plane_layers, pinned, committer)?;
        Ok((allocation, refused))
    }

    /// Test each pin the kernel has not been asked about, once per layer,
    /// plane and geometry, and say how many it refused (drm-cxx `c766e97`).
    ///
    /// A pin the static checks accept can still be one the driver will not
    /// drive -- a plane it refuses on this CRTC -- and the allocator's tests
    /// leave pinned planes off, so nothing else would find out before the real
    /// commit failed whole. Each pin is tested with the assignment and the
    /// pins already proven. A refused one is composited this frame, and from
    /// the next is not honoured, so the layer takes normal allocation.
    fn test_pins<C: TestCommitter>(
        &mut self,
        allocation: &mut drmkit_planes::Allocation,
        plane_layers: &mut [(LayerId, PlaneLayer)],
        pinned: &mut Vec<(LayerId, u32)>,
        committer: &mut C,
    ) -> Result<usize, SceneError> {
        let (refused, tests) = self.ask_about_pins(allocation, plane_layers, pinned, committer)?;
        allocation.diagnostics.test_commits_issued += tests;
        if refused.is_empty() {
            return Ok(0);
        }
        // Composited this frame; from the next the pin is not honoured and the
        // layer takes normal allocation.
        for (layer_id, plane_layer) in plane_layers.iter_mut() {
            if refused.contains(layer_id) {
                plane_layer.set_pinned(false).set_assigned_plane(None);
            }
        }
        pinned.retain(|(layer_id, _)| !refused.contains(layer_id));
        allocation.composited.extend(refused.iter().copied());
        self.allocator.invalidate_allocation();
        Ok(refused.len())
    }

    /// The pins `test_pins` refused this frame, and how many tests it took.
    fn ask_about_pins<C: TestCommitter>(
        &mut self,
        allocation: &drmkit_planes::Allocation,
        plane_layers: &[(LayerId, PlaneLayer)],
        pinned: &[(LayerId, u32)],
        committer: &mut C,
    ) -> Result<(Vec<LayerId>, usize), SceneError> {
        let layer_of = |id: LayerId| {
            plane_layers
                .iter()
                .find(|(layer_id, _)| *layer_id == id)
                .map(|(_, layer)| layer)
        };
        let mut pairs: Vec<(u32, LayerRef<'_>)> = allocation
            .assignment
            .entries()
            .iter()
            .filter_map(|(plane_id, id)| {
                layer_of(*id).map(|layer| (*plane_id, LayerRef { id: *id, layer }))
            })
            .collect();
        let mut untested = Vec::new();
        for (id, plane_id) in pinned {
            let Some(layer) = layer_of(*id) else {
                continue;
            };
            match pin_verdict(&self.pin_verdicts, *id, *plane_id, layer.property_hash()) {
                Some(true) => pairs.push((*plane_id, LayerRef { id: *id, layer })),
                // Revoked when lowered; not here.
                Some(false) => {}
                None => untested.push((*id, *plane_id, layer)),
            }
        }

        let mut refused = Vec::new();
        let mut tests = 0;
        for (id, plane_id, layer) in untested {
            pairs.push((plane_id, LayerRef { id, layer }));
            tests += 1;
            let accepted = match committer.test_assignment(&pairs) {
                Ok(()) => true,
                Err(TestFailure::NotMaster) => return Err(SceneError::NotMaster),
                Err(TestFailure::Rejected) => false,
            };
            remember_pin(
                &mut self.pin_verdicts,
                PinVerdict {
                    layer: id,
                    plane: plane_id,
                    hash: layer.property_hash(),
                    accepted,
                },
            );
            if !accepted {
                pairs.pop();
                refused.push(id);
            }
        }
        Ok((refused, tests))
    }

    /// Where the canvas sits in the allocator's tests: above every layer.
    ///
    /// The tests run before anything is composited, so they cannot know the
    /// run's own zpos, which is where the built frame stacks the canvas (see
    /// `compose_unassigned`).
    fn canvas_zpos(&self) -> u64 {
        let mut zpos = 0u64;
        for handle in self.handles().collect::<Vec<_>>() {
            if let Some(layer) = self.layer(handle) {
                zpos = zpos.max(layer.display().zpos.unwrap_or(0));
            }
        }
        zpos.saturating_add(1)
    }

    fn compose_unassigned(
        &mut self,
        allocation: &drmkit_planes::Allocation,
        registry: &PlaneRegistry,
        crtc_index: u32,
    ) -> Option<Composition> {
        if allocation.composited.is_empty() {
            return None;
        }

        // A plane the assignment did not take. Cursor planes are excluded for
        // the same reason they are never force-disabled: they belong to the
        // cursor path.
        //
        // Where planes stack by id, the allocator's pick is the only plane
        // that stacks the canvas between its neighbors; without one, the
        // topmost free plane is the closest to "above every layer".
        // A multirect virtual plane can carry the canvas only alongside its
        // parent, so only when the assignment armed the parent.
        let free = |id: &u32| {
            allocation.assignment.get(*id).is_none()
                && registry.by_id(*id).is_none_or(|plane| {
                    drmkit_planes::multirect_pairing_ok(plane.multirect_parent, |parent| {
                        allocation.assignment.get(parent).is_some()
                    })
                })
        };
        let plane_id = if drmkit_planes::stacks_by_plane_id(registry, crtc_index) {
            self.allocator.canvas_plane().filter(free).or_else(|| {
                registry
                    .force_disable_candidates(crtc_index)
                    .map(|plane| plane.id)
                    .filter(free)
                    .max()
            })?
        } else {
            // The plane its tests armed it on, else upstream's order.
            let hosts: Vec<u32> = registry
                .force_disable_candidates(crtc_index)
                .filter(|plane| {
                    self.canvas
                        .as_ref()
                        .is_some_and(|canvas| plane.supports_format(canvas.fourcc()))
                })
                .map(|plane| plane.id)
                .collect();
            self.allocator.canvas_plane().filter(free).or_else(|| {
                canvas_plane_order(&hosts, registry, crtc_index, self.last_canvas_plane)
                    .find(|id| free(id))
            })?
        };

        // Resolve every composited layer to its slot before touching the
        // canvas: `LayerId` packs a handle and a generation, and undoing that
        // inside the blend loop would duplicate `resolve`'s staleness rules.
        let mut targets: Vec<(u64, usize)> = Vec::new();
        for handle in self.handles().collect::<Vec<_>>() {
            let id = handle.layer_id();
            if !allocation.composited.contains(&id) {
                continue;
            }
            let Some(index) = self.resolve(handle) else {
                continue;
            };
            let order = self
                .layer(handle)
                .and_then(|l| l.display().zpos)
                .unwrap_or(0);
            targets.push((order, index));
        }
        if targets.is_empty() {
            return None;
        }
        // Bottom-up, so stacking inside the canvas matches what the layers
        // asked for. `composited` comes back in allocation order, which is not
        // it.
        targets.sort_unstable();

        // Where the run asked to be: at its topmost layer's zpos, so placed
        // layers above the run stay above the canvas and those below stay
        // below. Above every layer, as this used to be, the canvas covered
        // any placed layer stacked over what it carries (P-44, drm-cxx#343),
        // which overlay-first placement makes the common case: on a Pi 5 the
        // plane-limit scene's top tile went under a canvas carrying the
        // background. A run with placed layers inside it still cannot stack
        // right; on these CRTCs the search does not keep the run contiguous.
        //
        // The tests armed the canvas above every layer (see `canvas_zpos`),
        // since which layers are composited is not known before the search;
        // the planes and buffers are the ones tested, only the order differs.
        let zpos = targets.last().map_or(0, |(order, _)| *order);

        // Disjoint field borrows: the sources live in `slots`, the canvas does
        // not, and the blend needs both at once.
        let Self {
            slots,
            canvas,
            crtc_id,
            ..
        } = self;
        let canvas = canvas.as_mut()?;

        canvas.begin_frame();
        canvas.clear();

        let blended = blend_targets(canvas, slots, &targets);

        if blended == 0 {
            return None;
        }
        canvas.flush();

        Some(Composition {
            plane_id,
            layer: lower_canvas(canvas, *crtc_id, zpos)?,
            blended,
        })
    }

    /// Reconcile scene state with the kernel's answer, releasing buffers per
    /// invariants 1, 2 and 5.
    pub fn finalize_frame(&mut self, build: FrameBuild, result: KernelResult) -> CommitReport {
        self.finalize_frame_with_fence(build, result, None)
    }

    /// Finalize a frame, handing over the commit's `OUT_FENCE`.
    ///
    /// `release_fence` signals once this commit's buffers are on screen —
    /// which is the moment the buffers it *displaced* are off screen and safe
    /// to render into again. Sources that opted in through
    /// [`wants_release_fence`](LayerBufferSource::wants_release_fence) get a
    /// duplicate of it alongside each released buffer, so a GPU producer waits
    /// on it GPU-side instead of blocking until a later release edge.
    ///
    /// Ask [`wants_release_fence`](Self::wants_release_fence) whether to arm
    /// `OUT_FENCE_PTR` at all: with no source listening, the fence costs a
    /// descriptor per commit and nothing reads it.
    ///
    /// Passing `None` is [`finalize_frame`](Self::finalize_frame), and is
    /// correct whenever the CRTC has no `OUT_FENCE_PTR` or the commit did not
    /// ask for one.
    pub fn finalize_frame_with_fence(
        &mut self,
        mut build: FrameBuild,
        result: KernelResult,
        release_fence: Option<&SyncFence>,
    ) -> CommitReport {
        build.finalized = true;
        let acquisitions = std::mem::take(&mut build.acquisitions);
        let report = std::mem::take(&mut build.report);
        let kind = build.kind;
        let plan = std::mem::take(&mut build.plan);

        // Record what the kernel actually took, so the next frame's FB-only
        // fast path has a baseline to diff against (invariant 4).
        //
        // Only after a real commit that succeeded. A `TEST_ONLY` applies
        // nothing, so recording one would let a later commit diff against
        // state the kernel never held and suppress properties it still needs.
        //
        // Without this the baseline stays empty forever, `is_fb_only_frame`
        // can never be true, and the fast path is unreachable outside the
        // unit tests that populate the baseline by hand -- which is exactly
        // how it went unnoticed until the parity harness counted the test
        // commits the reference did not issue.
        if matches!(kind, CommitKind::Real { .. }) && matches!(result, KernelResult::Ok) {
            let applied: Vec<(u32, drmkit_planes::LayerRef<'_>)> = plan
                .iter()
                .map(|entry| {
                    (
                        entry.plane_id,
                        drmkit_planes::LayerRef {
                            id: entry.layer_id,
                            layer: &entry.layer,
                        },
                    )
                })
                .collect();
            let stacked: Vec<(u32, u64)> = plan
                .iter()
                .filter_map(|entry| entry.zpos.map(|zpos| (entry.plane_id, zpos)))
                .collect();
            self.allocator.record_commit_stacked(&applied, &stacked);
            // An empty frame swept nothing, so it settles nothing.
            if !plan.is_empty() {
                self.planes_unknown = false;
            }
        }

        // Which layers asked for the release fence, decided before the
        // lifecycle borrow: the answer lives in the sources, and the closure
        // cannot reach them while `finalize` holds `self` mutably.
        let wanting: Vec<LayerId> = if release_fence.is_some() {
            self.handles()
                .filter(|handle| {
                    self.layer(*handle)
                        .is_some_and(|layer| layer.source().wants_release_fence())
                })
                .map(LayerHandle::layer_id)
                .collect()
        } else {
            Vec::new()
        };
        let wants_fence = |layer: LayerId| wanting.contains(&layer);
        let outcome: FrameOutcome = self.lifecycle.finalize(
            kind,
            result,
            acquisitions,
            report,
            release_fence,
            wants_fence,
        );

        if outcome.invalidate_allocation {
            self.allocator.invalidate_allocation();
        }

        self.deliver(outcome.released);
        outcome.report
    }

    /// Re-emit every property on every commit, for a driver that mishandles a
    /// partial write.
    ///
    /// Off by default. A quirk escape hatch, not a tuning knob: it defeats
    /// invariant 4's minimal-write path and multiplies per-frame property
    /// traffic, and the only reason to reach for it is a driver that gets the
    /// minimal set wrong. Exposed here because the allocator it configures is
    /// the scene's, and a caller stuck on such a driver otherwise has no way
    /// in.
    pub const fn set_force_full_property_writes(&mut self, force: bool) {
        self.allocator.set_force_full_property_writes(force);
    }

    /// Whether full property writes are being forced.
    #[must_use]
    pub const fn force_full_property_writes(&self) -> bool {
        self.allocator.force_full_property_writes()
    }

    /// Whether this frame is worth committing at all.
    ///
    /// **The whole-commit skip.** When every live source says it has nothing
    /// new and no layer has moved, the frame can be dropped without issuing an
    /// atomic commit — the display goes on scanning out what is already there.
    /// That is a power win on any device and the thing that lets a
    /// self-refresh panel stay in self-refresh, which it cannot do if a
    /// commit arrives every vblank saying nothing changed.
    ///
    /// Answers `true` for anything it cannot rule out. A source that cannot
    /// tell whether its buffer changed reports "changed" by default, so a CPU
    /// producer painting into a dumb buffer never gets skipped; the layer set
    /// changing counts, geometry changing counts, and the first frame always
    /// counts, because nothing is on screen yet for "unchanged" to describe.
    #[must_use]
    pub fn content_changed(&self) -> bool {
        if !self.committed_once || self.topology_dirty {
            return true;
        }
        self.handles().any(|handle| {
            self.layer(handle).is_some_and(|layer| {
                layer.hints_dirty() || layer.display_dirty || layer.source().has_fresh_content()
            })
        })
    }

    /// The first layer carrying `tag`, if any.
    ///
    /// The lookup that makes [`SceneLayer::set_identity_tag`] worth setting:
    /// a caller that survived a rebind, a session resume, or its own restart
    /// can find its layers again from its own identifiers rather than from
    /// handles it may no longer hold.
    #[must_use]
    pub fn find_by_identity_tag(&self, tag: u64) -> Option<LayerHandle> {
        self.handles().find(|handle| {
            self.layer(*handle)
                .is_some_and(|layer| layer.identity_tag() == Some(tag))
        })
    }

    /// What a rebind left lit on the CRTC the scene moved away from, until it
    /// is committed.
    #[must_use]
    pub const fn pending_detach(&self) -> Option<&PendingDetach> {
        self.detach.as_ref()
    }

    /// Record that the pending detach reached the kernel.
    ///
    /// [`commit_detach`](crate::commit_detach) calls this after a successful
    /// commit; a caller committing the detach some other way must too.
    pub fn detach_committed(&mut self) {
        self.detach = None;
    }

    /// Move this scene to another output, keeping its layers.
    ///
    /// For an output that changed underneath the caller: a mode set, a
    /// different CRTC after a session resume, a display swapped for another.
    /// **Layer handles survive** — that is the point, since a caller that had
    /// to rebuild its layers would have to rebuild everything it knows about
    /// them too.
    ///
    /// Everything cached about the previous output is dropped -- the
    /// assignment, the committed baseline, the failure cache -- and the next
    /// commit is a full emit, with which the caller must set the new mode.
    ///
    /// **Moving to another CRTC leaves the old one's planes lit**, and the
    /// scene cannot turn them off itself: it does not commit. They are queued
    /// as a [`PendingDetach`], and until the caller commits it through
    /// [`commit_detach`](crate::commit_detach), [`build_frame`](Self::build_frame)
    /// refuses with [`SceneError::DetachPending`]. Two reasons it cannot be
    /// skipped: the kernel will not move a plane from one CRTC to another in a
    /// single commit, so a plane both pipes can use is rejected outright on the
    /// new one; and a plane the new pipe cannot use would go on showing this
    /// scene's last frame on the old output indefinitely. A rebind to the same
    /// CRTC queues nothing. Upstream has the same defect (drm-cxx#340).
    ///
    /// Returns what will not fit. A layer whose destination lies outside the
    /// new mode is **not** dropped — the caller may be about to move it, and
    /// deciding on its behalf would be worse than saying so.
    pub fn rebind(&mut self, crtc_id: u32, width: u32, height: u32) -> RebindReport {
        if crtc_id != self.crtc_id {
            // Read before the baseline that records them is forgotten. A
            // second rebind before the first detach landed keeps the first:
            // those planes are still lit on that CRTC, not on this one.
            let planes = self.allocator.lit_planes();
            if self.detach.is_none() && !planes.is_empty() {
                self.detach = Some(PendingDetach {
                    crtc_id: self.crtc_id,
                    planes,
                });
            }
        }
        self.crtc_id = crtc_id;
        self.allocator.forget_output();
        // Everything has to be re-emitted against the new pipe, and the layer
        // set is effectively new to it.
        self.topology_dirty = true;
        self.committed_once = false;
        self.planes_unknown = true;
        self.last_canvas_plane = None;
        self.pin_verdicts.clear();
        for handle in self.handles().collect::<Vec<_>>() {
            if let Some(layer) = self.layer_mut(handle) {
                layer.hints_dirty = true;
                layer.display_dirty = true;
                layer.last_fb_id = None;
            }
        }

        let incompatibilities = self
            .handles()
            .filter_map(|handle| {
                let layer = self.layer(handle)?;
                let dst = layer.display().dst_rect;
                let off_screen = dst.x < 0
                    || dst.y < 0
                    || dst.x.saturating_add(dst.w.cast_signed()) > width.cast_signed()
                    || dst.y.saturating_add(dst.h.cast_signed()) > height.cast_signed();
                off_screen.then_some(LayerIncompatibility {
                    handle,
                    reason: IncompatibilityReason::DstRectOffScreen,
                })
            })
            .collect();

        RebindReport { incompatibilities }
    }

    /// Whether any live source wants this commit's `OUT_FENCE`.
    ///
    /// The commit path asks before arming `OUT_FENCE_PTR`: the fence costs a
    /// descriptor per commit that has to be closed, and with nothing listening
    /// it is a descriptor leak dressed as a feature.
    #[must_use]
    pub fn wants_release_fence(&self) -> bool {
        self.handles().any(|handle| {
            self.layer(handle)
                .is_some_and(|layer| layer.source().wants_release_fence())
        })
    }

    /// Hand every held buffer back, without waiting on the kernel.
    ///
    /// **Invariant 5.** For teardown, session pause, and rebind. Landing an
    /// armed flip first is the caller's job — see
    /// [`has_pending_flip`](Self::has_pending_flip).
    pub fn drain(&mut self) {
        let released = self.lifecycle.drain();
        self.deliver(released);
    }

    /// Route released buffers to the sources that produced them.
    ///
    /// A buffer whose layer was removed goes to the retired source that is
    /// being kept alive for exactly this. Once a retired source has no buffers
    /// left in flight it is dropped.
    fn deliver(&mut self, released: Released) {
        for acquisition in released.acquisitions {
            let layer_id = acquisition.layer;
            let fence = acquisition.release_fence;

            let live = self.handles().find(|h| h.layer_id() == layer_id);
            if let Some(handle) = live
                && let Some(layer) = self.layer_mut(handle)
            {
                layer.source.release_with_fence(acquisition.buffer, fence);
                continue;
            }

            if let Some(entry) = self.retiring.iter_mut().find(|(id, _)| *id == layer_id) {
                entry.1.release_with_fence(acquisition.buffer, fence);
            }
            // Otherwise the source is already gone, which can only happen if a
            // caller dropped the scene mid-flight; the buffer drops with it.
        }

        if self.lifecycle.buffers_in_flight() == 0 {
            // Every retired source has had its buffers back.
            self.retiring.clear();
        }
    }

    /// Release a set of acquisitions immediately, for a build that aborted.
    fn release_all(&mut self, acquisitions: Vec<Acquisition>) {
        self.deliver(Released {
            acquisitions,
            reason: crate::release::ReleaseReason::CommitFailed,
        });
    }
}

/// Assemble the report for a built frame.
///
/// Split out of `build_frame` because it is pure over the tally, the
/// allocation, and the registry — nothing here touches scene state, so it reads
/// and reviews on its own.
fn build_report(
    tally: &AcquireTally,
    allocation: &drmkit_planes::Allocation,
    registry: &PlaneRegistry,
    starved: &[LayerId],
    composited: usize,
    pinned: &[(LayerId, u32)],
    pins_unhonored: usize,
) -> CommitReport {
    // A starved layer holds its plane so it keeps showing its last frame, so
    // it is in the assignment -- but it is also counted as skipped, and the
    // report's identity is
    //
    //     total = assigned + composited + unassigned + skipped
    //
    // so counting it in both would make every starved frame report one layer
    // too many. It is skipped, not assigned: nothing new was put on screen.
    let starved_assigned = starved
        .iter()
        .filter(|layer| allocation.assignment.get_plane_of(**layer).is_some())
        .count();
    let placements = allocation
        .assignment
        .entries()
        .iter()
        .map(|(plane_id, layer)| LayerPlacement {
            layer: *layer,
            placement: Placement::AssignedToPlane,
            plane_id: Some(*plane_id),
            plane_rotation_bits: registry
                .by_id(*plane_id)
                .map_or(0, |plane| plane.rotation_bits),
        })
        .chain(allocation.composited.iter().map(|layer| LayerPlacement {
            layer: *layer,
            placement: Placement::Unassigned,
            plane_id: None,
            plane_rotation_bits: 0,
        }))
        // A pinned layer never entered the allocation, so it has to be
        // reported from the pin itself. It reached a plane exactly as surely
        // as an allocated one did.
        .chain(pinned.iter().map(|(layer, plane_id)| {
            LayerPlacement {
                layer: *layer,
                placement: Placement::AssignedToPlane,
                plane_id: Some(*plane_id),
                plane_rotation_bits: registry
                    .by_id(*plane_id)
                    .map_or(0, |plane| plane.rotation_bits),
            }
        }))
        .collect();

    CommitReport {
        layers_total: tally.considered(),
        // Pinned layers are assigned too, and counting them keeps the
        // accounting identity -- they are in `considered` either way.
        layers_assigned: allocation.assignment.len() - starved_assigned + pinned.len(),
        pin_requests_unhonored: pins_unhonored,
        // A layer the allocator dropped is only *unassigned* if composition
        // did not rescue it. One that reached the canvas reached hardware, so
        // reporting it unassigned would tell a caller a frame was lost when it
        // was not.
        layers_composited: composited,
        layers_unassigned: allocation.composited.len() - composited,
        layers_skipped_no_frame: tally.skipped_no_frame,
        test_commits_issued: allocation.diagnostics.test_commits_issued,
        fb_delta_fast_path: allocation.diagnostics.fb_delta_fast_path,
        budget_exhausted: allocation.diagnostics.budget_exhausted,
        placements,
        ..CommitReport::default()
    }
}

/// What a frame's composition pass produced.
struct Composition {
    /// The plane the canvas is armed on.
    plane_id: u32,
    /// The canvas's lowered properties.
    layer: PlaneLayer,
    /// How many layers actually reached the canvas.
    ///
    /// Not the same as how many the allocator dropped: a source the CPU cannot
    /// read has no route in and stays off the screen.
    blended: usize,
}

/// Which planes to hold back from the search, if any.
///
/// Two reasons to reserve, and they are not the same reason.
///
/// **Overflow.** The canvas is armed after the search, onto a plane the search
/// did not take -- so with more layers than planes there would be none left,
/// and the overflow the canvas exists to rescue would be dropped instead.
///
/// Which plane follows [`canvas_plane_order`]: last frame's canvas plane, else
/// the first overlay, else a primary, so the primary stays free for the
/// bottom layer (drm-cxx `6d787f9`). Only planes that can carry the canvas
/// count, toward the overflow as well as for the pick. This used to take the
/// *last* candidate, so the search kept the lower-indexed planes for the
/// layers that stack below the canvas -- which matters on hardware where plane
/// index is the stacking order and nothing can reorder it. Such hardware now
/// takes the plane-order path, which reserves nothing.
///
/// **Primary anchor.** A primary whose zpos is pinned, with no live layer
/// eligible to land on it. Nothing then assigns the primary, so the disable
/// pass writes `FB_ID = 0` and `CRTC_ID = 0` to it in **every** test -- and a
/// driver that enforces "an active CRTC has an armed primary" refuses each
/// one. Every test fails, the allocator places nothing, and the whole scene
/// falls through to software composition. The frame is still correct and
/// still arrives; it simply stops using the hardware, and no counter says so.
///
/// amdgpu pins its primary at zpos 2 and rejects the disable; ARM Mali display
/// cores pin similarly. The reference documents both the behaviour and the
/// remedy, and this is the remedy: reserving the primary takes it out of the
/// disable pass, so tests validate against whatever it was already showing --
/// the fbcon framebuffer on a cold start, the previous canvas when warm.
///
/// A layer counts as eligible when its zpos matches the pin, or when it has
/// none at all: an unpinned layer is free to land there and the scoring bonus
/// in `plane_score` will put it there. So does the unique lowest layer when
/// every other one sits above a fixed slot -- see
/// [`lower_bottom_layer_to_primary_slot`], which is what then lands it there.
///
/// Not reachable on vkms, which exposes no zpos property at all, so nothing
/// in CI can show this. See P-26.
pub(crate) fn canvas_reservation(
    has_canvas: bool,
    refs: &[LayerRef<'_>],
    registry: &PlaneRegistry,
    crtc_index: u32,
    previous: Option<u32>,
) -> Vec<u32> {
    // Planes stacked by id: the allocator places the canvas itself, between
    // the layers it carries.
    if !has_canvas || drmkit_planes::stacks_by_plane_id(registry, crtc_index) {
        return Vec::new();
    }
    let candidates: Vec<&drmkit_planes::PlaneCapabilities> =
        registry.force_disable_candidates(crtc_index).collect();
    if candidates.is_empty() {
        return Vec::new();
    }

    // The anchor comes first: it is about a test commit failing outright,
    // where overflow is only about a layer being composited that need not be.
    if !refs.is_empty()
        && let Some(primary) = candidates
            .iter()
            .find(|plane| plane.plane_type == drmkit_planes::PlaneType::Primary)
        && let Some(pin) = primary.zpos_min
        && crate::canvas_format_for_plane(primary).is_some()
    {
        let zposes: Vec<Option<u64>> = refs
            .iter()
            .map(|layer| layer.layer.property(drmkit_planes::PropTag::Zpos))
            .collect();
        let anyone_eligible = zposes
            .iter()
            .any(|zpos| zpos.is_none_or(|zpos| zpos == pin))
            || (drmkit_planes::zpos_fixed(primary) && bottom_slot_layer(&zposes, pin).is_some());
        if !anyone_eligible {
            return vec![primary.id];
        }
    }

    let hosts: Vec<u32> = candidates
        .iter()
        .filter(|plane| crate::canvas_format_for_plane(plane).is_some())
        .map(|plane| plane.id)
        .collect();
    // Not a multirect virtual plane: reserved, it stays armed through the
    // allocator's tests while the allocator gives its parent away, so every
    // test fails (drm-cxx `4b366b5`). Left free, the canvas is re-picked each
    // frame.
    if refs.len() > hosts.len() {
        canvas_plane_order(&hosts, registry, crtc_index, previous)
            .find(|id| !is_multirect_virtual(registry, *id))
            .into_iter()
            .collect()
    } else {
        Vec::new()
    }
}

/// Whether `plane_id` is a multirect virtual plane, valid only alongside its
/// parent.
fn is_multirect_virtual(registry: &PlaneRegistry, plane_id: u32) -> bool {
    registry
        .by_id(plane_id)
        .is_some_and(|plane| plane.multirect_parent.is_some())
}

/// The planes the canvas prefers, best first: `previous`, the plane it had
/// last frame, then overlays, then primaries, each in registry order. Only
/// `hosts`, the planes that can carry it.
///
/// Upstream's order (`6d787f9`, closing P-24): a primary is a preference for
/// the bottom layer, not a contract for the canvas, so on a CRTC with two
/// primaries and one taken a free overlay comes before the other primary.
/// The sticky first choice keeps the canvas from moving between planes frame
/// to frame.
pub(crate) fn canvas_plane_order<'a>(
    hosts: &'a [u32],
    registry: &'a PlaneRegistry,
    crtc_index: u32,
    previous: Option<u32>,
) -> impl Iterator<Item = u32> + 'a {
    let of_type = move |plane_type: drmkit_planes::PlaneType| {
        registry
            .for_crtc(crtc_index)
            .filter(move |plane| {
                plane.plane_type == plane_type
                    && hosts.contains(&plane.id)
                    && previous != Some(plane.id)
            })
            .map(|plane| plane.id)
    };
    previous
        .filter(|id| hosts.contains(id))
        .into_iter()
        .chain(of_type(drmkit_planes::PlaneType::Overlay))
        .chain(of_type(drmkit_planes::PlaneType::Primary))
}

/// A 16.16 value rounded to the nearest whole pixel.
pub(crate) const fn round_16_16(value: u32) -> u32 {
    // Half rounds up: the bit below the binary point.
    (value >> 16) + ((value >> 15) & 1)
}

/// Blend each target's source into the canvas, and say how many landed.
///
/// A source that cannot be CPU-read, or whose format the blend does not
/// support, is skipped rather than failing the frame — it simply stays off the
/// screen. Returns zero when none could be read, which the caller treats as
/// "no canvas this frame".
fn blend_targets(
    canvas: &mut CompositeCanvas,
    slots: &mut [Slot],
    targets: &[(u64, usize)],
) -> usize {
    let mut blended = 0;
    for (_, index) in targets {
        let Slot::Occupied(layer) = &mut slots[*index] else {
            continue;
        };
        let display = layer.display;
        let format = layer.source.format();
        if !crate::format_supported(format.fourcc) {
            continue;
        }
        let Ok(mapping) = layer.source.map(drmkit_dumb::MapAccess::Read) else {
            // A source whose pixels never reach the CPU cannot be rescued this
            // way. It stays dropped for the frame.
            continue;
        };
        let src = CompositeSrc {
            pixels: mapping.pixels(),
            src_stride_bytes: mapping.stride(),
            src_width: mapping.width(),
            src_height: mapping.height(),
            drm_fourcc: format.fourcc,
            plane_alpha: display.alpha.unwrap_or(u16::MAX),
        };
        // The canvas samples whole pixels, so a sub-pixel crop rounds to the
        // nearest one.
        let src_rect = display
            .src_rect_fixed
            .map_or(display.src_rect, |fixed| Rect {
                x: round_16_16(fixed.x).cast_signed(),
                y: round_16_16(fixed.y).cast_signed(),
                w: round_16_16(fixed.w),
                h: round_16_16(fixed.h),
            });
        canvas.blend(
            &src,
            CompositeRect {
                x: src_rect.x,
                y: src_rect.y,
                w: if src_rect.w == 0 {
                    format.width
                } else {
                    src_rect.w
                },
                h: if src_rect.h == 0 {
                    format.height
                } else {
                    src_rect.h
                },
            },
            CompositeRect {
                x: display.dst_rect.x,
                y: display.dst_rect.y,
                w: display.dst_rect.w,
                h: display.dst_rect.h,
            },
        );
        blended += 1;
    }
    blended
}

/// The layer that can take a fixed primary slot `pin` though it asks for
/// another zpos: the unique lowest, with every other layer asking strictly
/// above the slot.
///
/// Port of upstream's `bottom_slot_layer` (drm-cxx `8bf20e6`). On it, the stack
/// comes out as asked whichever plane each layer lands on. `None` when any
/// layer leaves zpos unset -- the existing primary hint covers that -- when the
/// lowest is tied, or when another layer asks at or below the slot (amdgpu
/// layers at zpos 2 or lower keep the existing behavior).
pub(crate) fn bottom_slot_layer(zposes: &[Option<u64>], pin: u64) -> Option<usize> {
    let all: Option<Vec<u64>> = zposes.iter().copied().collect();
    let all = all?;
    let lowest = *all.iter().min()?;
    let mut at_lowest = all.iter().enumerate().filter(|(_, z)| **z == lowest);
    let (index, _) = at_lowest.next()?;
    if at_lowest.next().is_some() {
        return None;
    }
    all.iter()
        .enumerate()
        .all(|(i, z)| i == index || *z > pin)
        .then_some(index)
}

/// Lower the bottom layer at the primary's fixed slot, when it can take it.
///
/// On a single-primary controller (i.MX LCDIF, tilcdc: slot 0) a lone layer
/// at zpos 1 or above otherwise never reaches the only plane, and on one that
/// enforces an armed primary (amdgpu, slot 2) the disable every test would
/// carry refuses them all. Asking at the slot keeps the requested stacking --
/// every other layer asks above it -- and is what the primary's scoring bonus
/// keys on.
fn lower_bottom_layer_to_primary_slot(
    plane_layers: &mut [(LayerId, PlaneLayer)],
    registry: &PlaneRegistry,
    crtc_index: u32,
) {
    let Some(primary) = registry.for_crtc(crtc_index).find(|plane| {
        plane.plane_type == drmkit_planes::PlaneType::Primary && drmkit_planes::zpos_fixed(plane)
    }) else {
        return;
    };
    let Some(pin) = primary.zpos_min else {
        return;
    };
    let zposes: Vec<Option<u64>> = plane_layers
        .iter()
        .map(|(_, layer)| layer.property(PropTag::Zpos))
        .collect();
    if let Some(index) = bottom_slot_layer(&zposes, pin) {
        plane_layers[index].1.set_property(PropTag::Zpos, pin);
    }
}

/// Lower the canvas itself into the property bag its plane is programmed from.
fn lower_canvas(canvas: &CompositeCanvas, crtc_id: u32, zpos: u64) -> Option<PlaneLayer> {
    let fb_id = canvas.fb_id()?;
    let (w, h) = (canvas.width(), canvas.height());
    let mut plane_layer = PlaneLayer::new();
    lower_layer(
        &LoweringInput {
            display: DisplayParams {
                src_rect: Rect { x: 0, y: 0, w, h },
                dst_rect: Rect { x: 0, y: 0, w, h },
                zpos: Some(zpos),
                ..DisplayParams::default()
            },
            format: SourceFormat {
                fourcc: drmkit_fmt::fourcc::ARGB8888,
                modifier: 0,
                width: w,
                height: h,
            },
            binding: BindingModel::SceneSubmitsFbId,
            fb_id,
            crtc_id,
            default_zpos: None,
        },
        &mut plane_layer,
    );
    Some(plane_layer)
}

/// Lower one scene layer into the property bag a plane is programmed from.
///
/// Shared by the ordinary path and the starved path, which differ only in
/// which framebuffer they name: a fresh acquisition, or the one the layer
/// already has on screen.
fn lower(
    layer: &SceneLayer,
    fb_id: u32,
    crtc_id: u32,
    registry: &PlaneRegistry,
    crtc_index: u32,
) -> (drmkit_planes::Layer, PinOutcome) {
    let mut plane_layer = PlaneLayer::new();
    lower_layer(
        &LoweringInput {
            display: layer.display,
            format: layer.source.format(),
            binding: layer.source.binding_model(),
            fb_id,
            crtc_id,
            default_zpos: None,
        },
        &mut plane_layer,
    );
    plane_layer.set_content_type(layer.content_type);
    plane_layer.set_update_hint(layer.update_hint_hz);
    plane_layer.set_app_priority(layer.app_priority);
    plane_layer.set_force_composited(layer.force_composited);

    // The pin, if it can be honoured. `set_pinned` makes the allocator skip
    // this layer entirely -- the scene owns the plane from here -- so an
    // unhonourable pin must not set it, or the layer would be skipped by the
    // allocator *and* placed on nothing.
    let pin = match layer.pinned_plane {
        None => PinOutcome::NotRequested,
        Some(plane_id) if pin_is_honourable(&plane_layer, plane_id, registry, crtc_index) => {
            plane_layer.set_pinned(true);
            plane_layer.set_assigned_plane(Some(plane_id));
            PinOutcome::Honoured(plane_id)
        }
        Some(_) => PinOutcome::Refused,
    };
    (plane_layer, pin)
}

/// What became of a layer's pin request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PinOutcome {
    /// The caller asked for nothing.
    NotRequested,
    /// The layer owns this plane; the allocator will not offer it elsewhere.
    Honoured(u32),
    /// The plane cannot take this layer, so it goes through normal allocation.
    Refused,
}

/// The kernel's answer to one pin: this layer on this plane with this
/// geometry (`Layer::property_hash`, which leaves content out).
#[derive(Debug, Clone, Copy)]
struct PinVerdict {
    layer: LayerId,
    plane: u32,
    hash: u64,
    accepted: bool,
}

/// What the kernel said about this pin, if it was asked.
fn pin_verdict(verdicts: &[PinVerdict], layer: LayerId, plane: u32, hash: u64) -> Option<bool> {
    verdicts
        .iter()
        .find(|v| v.layer == layer && v.plane == plane && v.hash == hash)
        .map(|v| v.accepted)
}

/// Keep `verdict`. Bounded: a pinned layer whose geometry keeps changing adds
/// one entry per geometry, so past the bound the cache starts over.
fn remember_pin(verdicts: &mut Vec<PinVerdict>, verdict: PinVerdict) {
    const MAX_PIN_VERDICTS: usize = 64;
    if verdicts.len() >= MAX_PIN_VERDICTS {
        verdicts.clear();
    }
    verdicts.push(verdict);
}

/// A pin the kernel refused is not honoured again: the layer is unpinned and
/// takes normal allocation, and the refusal is counted.
fn revoke_refused_pin(
    verdicts: &[PinVerdict],
    layer_id: LayerId,
    plane_layer: &mut PlaneLayer,
    pin: PinOutcome,
) -> PinOutcome {
    match pin {
        PinOutcome::Honoured(plane_id)
            if pin_verdict(verdicts, layer_id, plane_id, plane_layer.property_hash())
                == Some(false) =>
        {
            plane_layer.set_pinned(false).set_assigned_plane(None);
            PinOutcome::Refused
        }
        other => other,
    }
}

/// Whether a plane can actually take the layer pinned to it.
///
/// Three ways a pin fails, and all three are the caller describing hardware
/// that is not there: a plane on another CRTC, a plane that cannot scan the
/// layer's format out, or a plane the scene has already reserved for something
/// else. Committing an impossible pin would fail the `TEST_ONLY` and take the
/// whole frame down -- every correctly placed layer with it -- so the check
/// happens here, and a refused pin costs the caller determinism rather than a
/// frame.
fn pin_is_honourable(
    layer: &PlaneLayer,
    plane_id: u32,
    registry: &PlaneRegistry,
    crtc_index: u32,
) -> bool {
    let Some(plane) = registry.by_id(plane_id) else {
        return false;
    };
    if !plane.compatible_with_crtc(crtc_index) {
        return false;
    }
    // The cursor path owns cursor planes, the same carve-out
    // `force_disable_candidates` makes.
    if plane.plane_type == drmkit_planes::PlaneType::Cursor {
        return false;
    }
    layer
        .property(PropTag::PixelFormat)
        .and_then(|fourcc| u32::try_from(fourcc).ok())
        .is_none_or(|fourcc| plane.supports_format(fourcc))
}

/// Work out what this frame writes: which layer goes on which plane, with what
/// baseline to diff against, and which planes have to be switched off.
///
/// Both halves read the allocator's committed baseline, so both have to be
/// computed **before** the frame is finalized -- finalizing replaces that
/// baseline with this frame's own, and a plan derived from it afterwards would
/// diff the frame against itself and write nothing at all.
fn build_plan(
    allocator: &drmkit_planes::Allocator,
    allocation: &drmkit_planes::Allocation,
    plane_layers: &[(LayerId, drmkit_planes::Layer)],
    registry: &PlaneRegistry,
    crtc_index: u32,
    damage: &HashMap<LayerId, Vec<crate::DamageRect>>,
    pinned: &[(LayerId, u32)],
) -> (Vec<PlanePlan>, Vec<u32>) {
    let plan: Vec<PlanePlan> = plane_layers
        .iter()
        .filter_map(|(id, layer)| {
            // A pinned layer never entered the allocation -- the allocator
            // skips it by design -- so its plane comes from the pin. Without
            // this it is reported as assigned and programmed by nobody, and
            // the plane it claimed stays dark.
            allocation
                .assignment
                .get_plane_of(*id)
                .or_else(|| {
                    pinned
                        .iter()
                        .find(|(layer_id, _)| layer_id == id)
                        .map(|(_, plane_id)| *plane_id)
                })
                .map(|plane_id| PlanePlan {
                    plane_id,
                    layer_id: *id,
                    layer: layer.clone(),
                    baseline: allocator.committed_baseline(plane_id, *id).copied(),
                    damage: damage.get(id).cloned().unwrap_or_default(),
                    zpos: None,
                })
        })
        .collect();

    let disables = registry
        .force_disable_candidates(crtc_index)
        .map(|plane| plane.id)
        .filter(|plane_id| {
            !plan.iter().any(|entry| entry.plane_id == *plane_id)
                && !allocator.plane_is_off(*plane_id)
        })
        .collect();

    (plan, disables)
}

impl Drop for LayerScene {
    fn drop(&mut self) {
        if self.lifecycle.has_pending_flip() && !std::thread::panicking() {
            // Invariant 5. The flip still references a framebuffer this drop is
            // about to tear down. Nothing can be done here -- waiting is
            // exactly what a drop must not do -- so make it loud in
            // development rather than a rare tear in production.
            //
            // Except while unwinding: a panicking caller drops the scene on
            // its way out, and asserting here aborts the process and hides
            // the panic that actually started it.
            debug_assert!(
                false,
                "LayerScene dropped with a page-flip event still armed: call \
                 drain() or dispatch the event first (invariant 5)"
            );
        }
    }
}
