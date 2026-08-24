// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! The scene: layers, their sources, and the two-phase commit.
//!
//! Ports the layer table and frame flow from `src/scene/layer_scene.cpp`.

use drmkit_planes::{Allocator, LayerRef};
use drmkit_planes::{Layer as PlaneLayer, LayerId, PlaneRegistry, TestCommitter, TestFailure};

use crate::display::DisplayParams;
use crate::frame::{AcquireTally, CommitKind, FrameLifecycle, FrameOutcome, KernelResult};
use crate::lower::{LoweringInput, lower_layer};
use crate::release::{Acquisition, Released};
use crate::report::{CommitReport, LayerPlacement, Placement};
use crate::source::{LayerBufferSource, SourceError};

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

/// A frame that has been built and is awaiting the kernel's answer.
///
/// Holding one means holding acquisitions. **Dropping it without finalizing
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
}

/// leaks them** — the same contract the C++ states, and the reason this type
/// warns on drop.
#[derive(Debug)]
pub struct FrameBuild {
    acquisitions: Vec<Acquisition>,
    plan: Vec<PlanePlan>,
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
        if !self.finalized && !self.acquisitions.is_empty() {
            // Same hazard as invariant 5's, one level up: these buffers never
            // go back to their sources, so the producer's ring starves. There
            // is nothing safe to do about it here -- the sources live in the
            // scene, which is not reachable from a drop -- so say so loudly.
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
}

/// One CRTC's layers and the commit machinery around them.
pub struct LayerScene {
    crtc_id: u32,
    slots: Vec<Slot>,
    generations: Vec<u32>,
    free: Vec<u32>,
    allocator: Allocator,
    lifecycle: FrameLifecycle,
    /// Sources whose layers were removed while their buffers were still in
    /// flight.
    ///
    /// Dropping such a source immediately would strand the buffers: the release
    /// would have nowhere to go, and any handles it owns would be freed while
    /// the display engine may still be scanning them. They are kept until every
    /// buffer has come back.
    retiring: Vec<(LayerId, Box<dyn LayerBufferSource>)>,
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
            lifecycle: FrameLifecycle::new(),
            retiring: Vec::new(),
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
    pub const fn resume(&mut self) {
        self.lifecycle.resume();
    }

    /// Add a layer backed by `source`.
    pub fn add_layer(&mut self, source: Box<dyn LayerBufferSource>) -> LayerHandle {
        let layer = Box::new(SceneLayer {
            source,
            display: DisplayParams::default(),
            content_type: drmkit_planes::ContentType::Generic,
            update_hint_hz: 0,
            app_priority: 0,
            hints_dirty: false,
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
        let mut starved: Vec<LayerId> = Vec::new();
        let mut acquisitions = Vec::new();
        let mut plane_layers: Vec<(LayerId, PlaneLayer)> = Vec::new();

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
                    plane_layers.push((layer_id, lower(layer, fb_id, self.crtc_id, None)));
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

            // Borrowed for the commit only -- the kernel does not take
            // ownership of IN_FENCE_FD, and the fence closes with the buffer.
            let fence_fd = acquired
                .acquire_fence
                .as_ref()
                .and_then(drmkit_sync::SyncFence::as_fd)
                .map(|fd| std::os::fd::AsRawFd::as_raw_fd(&fd));
            let plane_layer = lower(layer, acquired.fb_id, self.crtc_id, fence_fd);

            layer.last_fb_id = Some(acquired.fb_id);
            plane_layers.push((layer_id, plane_layer));
            acquisitions.push(Acquisition::new(layer_id, acquired));
        }

        let refs: Vec<LayerRef<'_>> = plane_layers
            .iter()
            .map(|(id, layer)| LayerRef { id: *id, layer })
            .collect();

        let allocation = match self
            .allocator
            .allocate(&refs, registry, crtc_index, committer)
        {
            Ok(allocation) => allocation,
            Err(TestFailure::NotMaster) => {
                self.release_all(acquisitions);
                return Err(SceneError::NotMaster);
            }
            Err(TestFailure::Rejected) => {
                // The allocator handles ordinary rejections internally, so this
                // is unreachable in practice; treating it as "nothing placed"
                // keeps the frame honest rather than panicking.
                drmkit_planes::Allocation::default()
            }
        };

        let report = build_report(&tally, &allocation, registry, &starved);

        // Clear the hint flags now the allocation has seen them.
        for handle in self.handles().collect::<Vec<_>>() {
            if let Some(layer) = self.layer_mut(handle) {
                layer.hints_dirty = false;
            }
        }

        let (plan, disables) = build_plan(
            &self.allocator,
            &allocation,
            &plane_layers,
            registry,
            crtc_index,
        );

        Ok(FrameBuild {
            acquisitions,
            plan,
            disables,
            report,
            kind,
            finalized: false,
        })
    }

    /// Reconcile scene state with the kernel's answer, releasing buffers per
    /// invariants 1, 2 and 5.
    pub fn finalize_frame(&mut self, mut build: FrameBuild, result: KernelResult) -> CommitReport {
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
            self.allocator.record_commit(&applied);
        }

        let wants_fence = |_layer: LayerId| false;
        let outcome: FrameOutcome =
            self.lifecycle
                .finalize(kind, result, acquisitions, report, None, wants_fence);

        if outcome.invalidate_allocation {
            self.allocator.invalidate_allocation();
        }

        self.deliver(outcome.released);
        outcome.report
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
        .collect();

    CommitReport {
        layers_total: tally.considered(),
        layers_assigned: allocation.assignment.len() - starved_assigned,
        layers_unassigned: allocation.composited.len(),
        layers_skipped_no_frame: tally.skipped_no_frame,
        test_commits_issued: allocation.diagnostics.test_commits_issued,
        fb_delta_fast_path: allocation.diagnostics.fb_delta_fast_path,
        placements,
        ..CommitReport::default()
    }
}

/// Lower one scene layer into the property bag a plane is programmed from.
///
/// Shared by the ordinary path and the starved path, which differ in which
/// framebuffer they name -- a fresh acquisition, or the one the layer already
/// has on screen -- and in whether there is a fence to wait on. A held frame
/// has none: it was already on screen, so whatever fence it arrived with
/// signalled long ago and the scene no longer owns it.
fn lower(
    layer: &SceneLayer,
    fb_id: u32,
    crtc_id: u32,
    acquire_fence_fd: Option<i32>,
) -> drmkit_planes::Layer {
    let mut plane_layer = PlaneLayer::new();
    lower_layer(
        &LoweringInput {
            display: layer.display,
            format: layer.source.format(),
            binding: layer.source.binding_model(),
            fb_id,
            crtc_id,
            acquire_fence_fd,
            default_zpos: None,
        },
        &mut plane_layer,
    );
    plane_layer.set_content_type(layer.content_type);
    plane_layer.set_update_hint(layer.update_hint_hz);
    plane_layer.set_app_priority(layer.app_priority);
    plane_layer
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
) -> (Vec<PlanePlan>, Vec<u32>) {
    let plan: Vec<PlanePlan> = plane_layers
        .iter()
        .filter_map(|(id, layer)| {
            allocation
                .assignment
                .get_plane_of(*id)
                .map(|plane_id| PlanePlan {
                    plane_id,
                    layer_id: *id,
                    layer: layer.clone(),
                    baseline: allocator.committed_baseline(plane_id, *id).copied(),
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
        if self.lifecycle.has_pending_flip() {
            // Invariant 5. The flip still references a framebuffer this drop is
            // about to tear down. Nothing can be done here -- waiting is
            // exactly what a drop must not do -- so make it loud in
            // development rather than a rare tear in production.
            debug_assert!(
                false,
                "LayerScene dropped with a page-flip event still armed: call \
                 drain() or dispatch the event first (invariant 5)"
            );
        }
    }
}
