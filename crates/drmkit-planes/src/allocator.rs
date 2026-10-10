// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! The plane allocator.
//!
//! Port of the search in `src/planes/allocator.cpp`. Home of **invariant 4**'s
//! FB-only fast path and of the v2.0.1 warm-start stability bonus.

use std::collections::HashMap;

use drmkit_fmt::{Modifier, ModifierProbeCache, Verdict};

use crate::layer::Layer;
use crate::matching::BipartiteMatching;
use crate::plane_order::{PlaneOrder, Position, plane_order_consistent, stacks_by_plane_id};
use crate::prop::PropTag;
use crate::registry::{PlaneCapabilities, PlaneRegistry, PlaneType};
use crate::scoring::{
    ScoreContext, keep_priority, layers_intersect, plane_statically_compatible, score_pair,
    split_independent_groups,
};

/// Stable identity for a layer across frames.
///
/// # Deviation: identity by value, not by address
///
/// The C++ allocator remembers `const Layer*` and therefore needs
/// `forget_layer()`, which the header explains must run before a `Layer` is
/// destroyed:
///
/// > without invalidation, heap reuse can give a freshly-added layer the same
/// > address, fool the diff path into treating the two as the same logical
/// > layer, and silently skip property writes the kernel needs.
///
/// Keying on a caller-assigned id removes that hazard rather than mitigating
/// it: a recycled allocation cannot masquerade as a previous layer, so there is
/// nothing to invalidate and no window in which forgetting to do so corrupts a
/// commit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LayerId(pub u64);

/// One layer offered to the allocator.
#[derive(Debug, Clone, Copy)]
pub struct LayerRef<'a> {
    /// Stable identity across frames.
    pub id: LayerId,
    /// The layer itself.
    pub layer: &'a Layer,
}

/// Why a test commit was rejected.
///
/// The distinction matters: a scene suspends on lost DRM master and on nothing
/// else (invariant 2), so flattening every rejection into one variant would
/// break that contract.
///
/// Deliberately **not** `#[non_exhaustive]`. A caller must decide what each
/// failure means — retry a smaller assignment, or stop the scene — and an open
/// enum forces a catch-all arm, which is exactly how a new failure kind would
/// silently inherit the wrong handling. Adding a variant should break every
/// caller so each one chooses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TestFailure {
    /// The kernel rejected the configuration. The ordinary case: try a smaller
    /// assignment.
    Rejected,
    /// The caller is no longer DRM master. Propagated up rather than retried —
    /// no smaller assignment will help.
    NotMaster,
}

/// Validates tentative plane assignments.
///
/// # Why this is a trait
///
/// The C++ allocator holds a `Device` and issues `TEST_ONLY` commits itself.
/// Doing the same here would make `drmkit-planes` depend on `drmkit-core` and
/// give up being device-free — and the plan wants this crate `no_std`-adjacent
/// (§5).
///
/// Handing the commit back to the caller keeps the split clean: the allocator
/// decides *what* to try, the scene knows *how* to ask the kernel, and the
/// property-name-to-id resolution stays next to the `PropertyStore` that owns
/// it. It also makes the whole search testable against a fake, which is how the
/// suite reaches its coverage with no hardware.
pub trait TestCommitter {
    /// Validate an assignment with a `TEST_ONLY` commit.
    ///
    /// `assignment` is `(plane_id, layer)` pairs. An implementation is expected
    /// to disable every other CRTC-compatible plane in the same request:
    /// without that, the test inherits stale state from the previously
    /// committed plane and reports a spurious rejection when a layer migrates
    /// between planes on one CRTC.
    ///
    /// # Errors
    ///
    /// [`TestFailure::Rejected`] for an ordinary kernel rejection, or
    /// [`TestFailure::NotMaster`] when DRM master was lost.
    fn test_assignment(&mut self, assignment: &[(u32, LayerRef<'_>)]) -> Result<(), TestFailure>;

    /// Arm one more plane in every test, alongside the assignment.
    ///
    /// The composition canvas lands on a plane the assignment never names, so
    /// without this the search validates a frame one plane smaller than the
    /// one that gets committed. Where the hardware lights fewer planes than it
    /// advertises, that difference is a frame the kernel refuses -- and a
    /// refused frame shows nothing at all. Telling the committer about the
    /// canvas makes the search account for it, so it drops a layer into the
    /// composition instead of proposing something uncommittable.
    ///
    /// The plane is excluded from the test's disable pass for as long as it is
    /// set. Call [`TestCommitter::clear_extra_plane`] afterwards.
    ///
    /// Defaults to doing nothing, which is right for a committer that accepts
    /// everything or one that is not talking to a device.
    fn set_extra_plane(&mut self, plane_id: u32, layer: Layer) {
        let _ = (plane_id, layer);
    }

    /// Stop arming the plane set by [`TestCommitter::set_extra_plane`].
    fn clear_extra_plane(&mut self) {}

    /// The planes the kernel has lit on `crtc_id` right now, whoever lit
    /// them; `None` when the committer cannot tell.
    ///
    /// A scene asks this while it does not yet know what the kernel holds --
    /// before its first commit lands, after a rebind or a resume -- so the
    /// frame can turn off a plane another client left armed, as every test
    /// already does. `None`, the default, makes the scene turn off every plane
    /// it does not use instead, which is right but writes more.
    fn lit_planes(&mut self, crtc_id: u32) -> Option<Vec<u32>> {
        let _ = crtc_id;
        None
    }
}

/// A committer that accepts everything. For tests that care about which
/// assignment the search *proposes* rather than how it narrows one down.
#[derive(Debug, Default, Clone, Copy)]
pub struct AlwaysAccept {
    /// How many test commits were requested.
    pub calls: usize,
}

impl TestCommitter for AlwaysAccept {
    fn test_assignment(&mut self, _assignment: &[(u32, LayerRef<'_>)]) -> Result<(), TestFailure> {
        self.calls += 1;
        Ok(())
    }
}

/// Plane-to-layer assignment.
///
/// A flat vector rather than a map: CRTCs have at most a handful of planes, so
/// a linear scan beats hashing, and reassigning one reuses the allocation
/// instead of rehashing. Same reasoning the C++ `PlaneAssignment` records.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PlaneAssignment {
    entries: Vec<(u32, LayerId)>,
}

impl PlaneAssignment {
    /// An empty assignment.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    /// Assign a plane, replacing any previous layer on it.
    pub fn insert(&mut self, plane_id: u32, layer: LayerId) {
        if let Some(entry) = self.entries.iter_mut().find(|(id, _)| *id == plane_id) {
            entry.1 = layer;
        } else {
            self.entries.push((plane_id, layer));
        }
    }

    /// The layer assigned to a plane.
    #[must_use]
    pub fn get(&self, plane_id: u32) -> Option<LayerId> {
        self.entries
            .iter()
            .find(|(id, _)| *id == plane_id)
            .map(|(_, layer)| *layer)
    }

    /// The plane a layer is assigned to, if any.
    #[must_use]
    pub fn get_plane_of(&self, layer: LayerId) -> Option<u32> {
        self.entries
            .iter()
            .find(|(_, id)| *id == layer)
            .map(|(plane_id, _)| *plane_id)
    }

    /// Remove a plane's assignment.
    pub fn remove(&mut self, plane_id: u32) -> Option<LayerId> {
        let index = self.entries.iter().position(|(id, _)| *id == plane_id)?;
        Some(self.entries.remove(index).1)
    }

    /// Every `(plane_id, layer)` pair.
    #[must_use]
    pub fn entries(&self) -> &[(u32, LayerId)] {
        &self.entries
    }

    /// How many planes are assigned.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether nothing is assigned.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Drop every assignment.
    pub fn clear(&mut self) {
        self.entries.clear();
    }
}

/// Memoized test-commit verdicts, keyed by `(plane, property_hash)`.
#[derive(Debug, Default, Clone)]
pub struct TestCache {
    entries: HashMap<(u32, u64), Entry>,
}

#[derive(Debug, Clone, Copy, Default)]
struct Entry {
    /// The latest verdict.
    passed: bool,
    /// How many times this combination has been rejected. Successes do not
    /// increment it, so it is a penalty and not a visit tally.
    failures: u32,
}

impl TestCache {
    /// An empty cache.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The cached verdict, if any.
    #[must_use]
    pub fn lookup(&self, plane_id: u32, property_hash: u64) -> Option<bool> {
        self.entries
            .get(&(plane_id, property_hash))
            .map(|entry| entry.passed)
    }

    /// Record a verdict, counting failures so scoring can back off.
    pub fn record(&mut self, plane_id: u32, property_hash: u64, passed: bool) {
        let entry = self.entries.entry((plane_id, property_hash)).or_default();
        entry.passed = passed;
        if !passed {
            entry.failures = entry.failures.saturating_add(1);
        }
    }

    /// How many times this combination has been rejected; zero while its
    /// latest verdict is a pass.
    ///
    /// The score subtracts this. Counting every verdict instead -- what this
    /// did until drm-cxx#267 -- decayed a plane the kernel keeps accepting as
    /// fast as one it keeps rejecting, so the search walked away from the
    /// plane that worked. Zero after a pass, so a combination rejected while
    /// some other state was wrong stops being penalized once it succeeds.
    #[must_use]
    pub fn failure_count(&self, plane_id: u32, property_hash: u64) -> u32 {
        self.entries
            .get(&(plane_id, property_hash))
            .filter(|entry| !entry.passed)
            .map_or(0, |entry| entry.failures)
    }

    /// Drop everything.
    pub fn clear(&mut self) {
        self.entries.clear();
    }
}

/// What one allocation pass did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct Diagnostics {
    /// `TEST_ONLY` commits issued while probing for a viable assignment.
    ///
    /// 1 in steady state — warm re-validation of the cached assignment. Higher
    /// when the cache misses and the full search has to preseed, greedy, and
    /// backtrack. **0 when the FB-only fast path skipped re-validation.**
    pub test_commits_issued: usize,
    /// Whether the cached allocation was reused and the `TEST_ONLY` skipped
    /// because only content changed on already-placed layers.
    ///
    /// Implies `test_commits_issued == 0` for the frame. This is the counter
    /// invariant 4 is stated in terms of.
    pub fb_delta_fast_path: bool,
    /// Whether the per-frame test-commit budget ran out before every layer had
    /// been offered a plane.
    ///
    /// The budget bounds how many round trips to the kernel one frame may
    /// cost. Spending it is not an error -- the layers it could not reach fall
    /// through to composition and still reach the screen -- but it is a
    /// *capacity* limit rather than a hardware one, and the two look identical
    /// in `layers_composited`. On a device with many planes and a scene of
    /// spatially disjoint layers, each layer forms its own group costing one
    /// test commit, so a 17-layer scene exhausts the default budget of 16 and
    /// composites its last layer with a free, compatible plane sitting unused.
    /// Without this flag that reads as "the hardware ran out of planes".
    pub budget_exhausted: bool,
}

/// The result of one allocation pass.
#[derive(Debug, Clone, Default)]
pub struct Allocation {
    /// Which layer goes on which plane.
    pub assignment: PlaneAssignment,
    /// Layers that could not be placed and must be composited.
    pub composited: Vec<LayerId>,
    /// What the pass did.
    pub diagnostics: Diagnostics,
}

/// What the kernel accepted for a plane last commit.
#[derive(Debug, Clone, Copy)]
struct LastCommitted {
    /// `None` once the layer is gone but the plane is still lit -- see
    /// [`Allocator::forget_layer`].
    layer: Option<LayerId>,
    /// Every property the kernel took, so the next commit can write only what
    /// actually changed.
    properties: crate::PropertySnapshot,
    /// `property_hash` at commit time, which excludes `FB_ID` and
    /// `IN_FENCE_FD`. The fast path compares against this to prove geometry,
    /// format, and modifier are unchanged since the kernel accepted them.
    hash: u64,
}

/// Assigns layers to display planes.
///
/// One allocator belongs to one CRTC and is kept across frames, because most
/// of what makes it cheap is memory of the last one. A frame goes through at
/// most four stages, and a steady scene stops at the first:
///
/// 1. **The FB-only fast path.** Only framebuffer ids changed, so the kernel
///    has already accepted this arrangement — no test commit at all. This is
///    invariant 4, and [`Layer::property_hash`](crate::Layer::property_hash)
///    is what makes it safe to claim.
/// 2. **Warm start.** The layer set is unchanged but something else moved, so
///    last frame's assignment is re-offered and validated with one test.
/// 3. **A full search.** Bipartite matching seeds it, scored candidates order
///    it, and backtracking drops the layer with the lowest keep-priority when
///    the kernel refuses.
/// 4. **Composition.** Whatever is still unplaced is handed back for the
///    caller to blend, rather than the frame failing.
///
/// The search is budgeted: [`DEFAULT_MAX_TEST_COMMITS`](Self::DEFAULT_MAX_TEST_COMMITS)
/// test commits per frame, after which it takes the best arrangement it has
/// proved. Exhausting the budget is reported rather than hidden, because "this
/// device is out of planes" and "raise the budget" want different answers from
/// a caller.
///
/// A refusal is never treated as a defect. The kernel is the only authority on
/// what a device can scan out simultaneously, and a `TEST_ONLY` that comes back
/// `EINVAL` is that authority answering — which is why the allocator asks
/// rather than predicting.
#[derive(Debug)]
pub struct Allocator {
    previous: PlaneAssignment,
    previous_valid: bool,
    last_committed: HashMap<u32, LastCommitted>,
    /// Driver-quirk opt-out; see `set_force_full_property_writes`.
    force_full_writes: bool,
    /// Planes the caller has claimed; see `set_reserved_planes`.
    reserved: Vec<u32>,
    failure_cache: TestCache,
    probe_cache: ModifierProbeCache,
    max_test_commits: usize,
    test_commits_this_frame: usize,
    /// Whether the budget stopped a test this frame -- see
    /// [`Diagnostics::budget_exhausted`].
    budget_exhausted_this_frame: bool,
    matcher: BipartiteMatching,
    /// Planes that can carry the composition canvas, and the canvas as its
    /// plane would be programmed; empty and `None` without one. See
    /// [`set_canvas`](Self::set_canvas).
    canvas_hosts: Vec<u32>,
    canvas_layer: Option<Layer>,
    /// The canvas plane for the last pass, and the one cached with
    /// `previous`.
    canvas_plane: Option<u32>,
    previous_canvas_plane: Option<u32>,
    /// Where planes take a `zpos`: the reserved plane the caller holds back
    /// for the canvas. See [`hold_canvas_plane`](Self::hold_canvas_plane).
    canvas_hold: Option<u32>,
    /// The layers the last full search left unplaced, which the cached paths
    /// composite again rather than treat as new, and the planes it had free
    /// when it decided that.
    previous_composited: Vec<LayerId>,
    previous_free: Vec<u32>,
    /// Where planes take a `zpos`: the zpos the canvas asks for after the
    /// last pass. See [`canvas_zpos`](Self::canvas_zpos).
    canvas_slot: Option<u64>,
}

impl Default for Allocator {
    fn default() -> Self {
        Self::new()
    }
}

impl Allocator {
    /// Default per-frame test-commit budget.
    pub const DEFAULT_MAX_TEST_COMMITS: usize = 16;

    /// A fresh allocator with no cached state.
    #[must_use]
    pub fn new() -> Self {
        Self {
            previous: PlaneAssignment::new(),
            previous_valid: false,
            last_committed: HashMap::new(),
            force_full_writes: false,
            reserved: Vec::new(),
            failure_cache: TestCache::new(),
            probe_cache: ModifierProbeCache::new(),
            max_test_commits: Self::DEFAULT_MAX_TEST_COMMITS,
            test_commits_this_frame: 0,
            budget_exhausted_this_frame: false,
            matcher: BipartiteMatching::new(),
            canvas_hosts: Vec::new(),
            canvas_layer: None,
            canvas_plane: None,
            previous_canvas_plane: None,
            canvas_hold: None,
            previous_composited: Vec::new(),
            previous_free: Vec::new(),
            canvas_slot: None,
        }
    }

    /// The composition canvas: which planes can carry it, and its plane's
    /// properties. Empty `hosts` or no `layer` means the caller has none.
    ///
    /// Where planes have no `zpos` the allocator picks the canvas plane
    /// itself, since only a plane between the composited run's neighbors
    /// stacks the canvas where the run asked. Elsewhere the caller names it
    /// with [`hold_canvas_plane`](Self::hold_canvas_plane). Either way the
    /// allocator arms `layer` there in every test it issues, so that what the
    /// kernel accepts is the whole frame (P-18).
    pub fn set_canvas(&mut self, hosts: &[u32], layer: Option<&Layer>) {
        self.canvas_hosts.clear();
        self.canvas_layer = layer.cloned();
        if self.canvas_layer.is_some() {
            self.canvas_hosts.extend_from_slice(hosts);
        }
    }

    /// The plane the last pass left for the canvas, tested with it armed.
    ///
    /// `None` when nothing is composited, when no host fits, or where planes
    /// take a `zpos` and the pass held no plane for it; the caller arms the
    /// canvas here when it is `Some`. A cached pass reports the plane the
    /// search it reuses left, since that is the plane its test armed.
    #[must_use]
    pub const fn canvas_plane(&self) -> Option<u32> {
        self.canvas_plane
    }

    /// Where planes take a `zpos`: hold `plane` for the canvas this pass.
    ///
    /// The plane must also be reserved (see
    /// [`set_reserved_planes`](Self::set_reserved_planes)). The search arms
    /// the canvas there in every test, and when it leaves layers composited
    /// reports the plane as [`canvas_plane`](Self::canvas_plane) and keeps it
    /// with the cached assignment, so a warm start re-tests the canvas with
    /// the layers rather than searching again. Ignored where planes stack by
    /// id, which pick their own.
    pub const fn hold_canvas_plane(&mut self, plane: Option<u32>) {
        self.canvas_hold = plane;
    }

    /// Set the per-search test-commit budget.
    ///
    /// A frame that composites runs two searches -- the second holds a plane
    /// back for the composition canvas and re-validates with it armed -- so
    /// such a frame can spend up to twice this. That is deliberate: starving
    /// the second search would leave it unable to validate, which is the one
    /// thing it exists to do.
    pub const fn set_max_test_commits(&mut self, max: usize) {
        self.max_test_commits = max;
    }

    /// Drop the warm-start cache so the next pass does a full search.
    ///
    /// The scene calls this when a layer's content type or update hint changed:
    /// those affect scoring but not the layer set, so warm-start — keyed on
    /// "same layers, still valid" — would otherwise keep the stale assignment
    /// and never move the layer to the plane its new hint prefers.
    pub fn invalidate_allocation(&mut self) {
        self.previous_valid = false;
    }

    /// Forget everything cached about the previous output.
    ///
    /// For a scene moving to a different CRTC. Both the assignment and the
    /// committed baseline describe *that* output's planes, and a plane id
    /// means nothing on another pipe -- diffing against a baseline from the
    /// old one would suppress properties the new one has never been told.
    ///
    /// Stronger than [`invalidate_allocation`](Self::invalidate_allocation),
    /// which only forces a fresh search and keeps the baseline.
    pub fn forget_output(&mut self) {
        self.previous = PlaneAssignment::new();
        self.previous_valid = false;
        self.canvas_plane = None;
        self.previous_canvas_plane = None;
        self.previous_composited.clear();
        self.previous_free.clear();
        self.last_committed.clear();
        self.failure_cache = TestCache::default();
    }

    /// Record what the kernel accepted, so the next frame's fast path has a
    /// baseline. Call after a successful real commit, never after a test.
    ///
    /// A `TEST_ONLY` applies nothing to hardware, so recording one would let a
    /// later commit diff against state the kernel never took and suppress
    /// properties it still requires.
    pub fn record_committed(&mut self, plane_id: u32, layer: LayerRef<'_>) {
        self.last_committed.insert(
            plane_id,
            LastCommitted {
                layer: Some(layer.id),
                properties: layer.layer.snapshot(),
                hash: layer.layer.property_hash(),
            },
        );
    }

    /// Replace the committed baseline with what a real commit just applied.
    ///
    /// Planes absent from `applied` are dropped rather than left behind: the
    /// commit that carried this assignment explicitly disabled every candidate
    /// plane it did not use, so any baseline they had describes state the
    /// kernel no longer holds.
    ///
    /// Call after a successful **real** commit, never after a test -- see
    /// [`record_committed`](Self::record_committed).
    pub fn record_commit(&mut self, applied: &[(u32, LayerRef<'_>)]) {
        self.record_commit_stacked(applied, &[]);
    }

    /// [`record_commit`](Self::record_commit), for a commit that wrote a
    /// stacked zpos in place of some layers' own (see
    /// [`stacked_zpos`](crate::stacked_zpos)).
    ///
    /// The snapshot takes the written value, because the next frame's diff
    /// has to compare against what the kernel holds. The hash stays the
    /// layer's own, because the fast path compares it against the layer as
    /// the caller will present it next frame, which is the requested zpos --
    /// and an unchanged assignment of unchanged requests stacks the same.
    pub fn record_commit_stacked(&mut self, applied: &[(u32, LayerRef<'_>)], zpos: &[(u32, u64)]) {
        self.last_committed.clear();
        for (plane_id, entry) in applied {
            let mut properties = entry.layer.snapshot();
            if let Some((_, written)) = zpos.iter().find(|(id, _)| id == plane_id) {
                properties = properties.with(PropTag::Zpos, *written);
            }
            self.last_committed.insert(
                *plane_id,
                LastCommitted {
                    layer: Some(entry.id),
                    properties,
                    hash: entry.layer.property_hash(),
                },
            );
        }
    }

    /// What the kernel last took on `plane_id`, if this commit may diff
    /// against it.
    ///
    /// `None` means every property must be written:
    ///
    /// * the plane has no baseline -- first use, or it was detached since;
    /// * the baseline belongs to a **different** layer, so the incoming one
    ///   would silently inherit any property the outgoing layer set and it
    ///   does not (a stale rotation, a stale alpha);
    /// * full writes were forced for a driver that mishandles partial ones.
    #[must_use]
    pub fn committed_baseline(
        &self,
        plane_id: u32,
        layer: LayerId,
    ) -> Option<&crate::PropertySnapshot> {
        if self.force_full_writes {
            return None;
        }
        self.last_committed
            .get(&plane_id)
            .filter(|baseline| baseline.layer == Some(layer))
            .map(|baseline| &baseline.properties)
    }

    /// Hold these planes back from the search.
    ///
    /// A reserved plane is not offered to any layer, and the caller arms it
    /// itself. The composition canvas needs this: with more layers than planes
    /// the search would otherwise claim every one of them, and the canvas that
    /// exists to rescue the overflow would have nowhere to land — so the
    /// overflow is dropped instead of composited, which is the opposite of
    /// what the fallback is for.
    ///
    /// Reserving costs a plane whether or not it ends up used, so a caller
    /// should reserve only when it can tell the search will overflow.
    pub fn set_reserved_planes(&mut self, planes: &[u32]) {
        self.reserved.clear();
        self.reserved.extend_from_slice(planes);
    }

    /// Whether the kernel currently has nothing on `plane_id`.
    ///
    /// A plane with no baseline was never activated, or was detached by the
    /// commit that dropped it. Either way the kernel already has it off, and
    /// disabling it again is a pair of property writes the kernel still has to
    /// walk on every frame.
    #[must_use]
    pub fn plane_is_off(&self, plane_id: u32) -> bool {
        self.last_committed.get(&plane_id).is_none_or(|baseline| {
            baseline
                .properties
                .get(PropTag::FbId)
                .is_none_or(|fb| fb == 0)
        })
    }

    /// Every plane the kernel has a framebuffer on, as far as this allocator
    /// committed it.
    ///
    /// What a scene leaving its CRTC has to turn off before the baseline that
    /// records it is forgotten: nothing else knows these planes are lit.
    #[must_use]
    pub fn lit_planes(&self) -> Vec<u32> {
        let mut planes: Vec<u32> = self
            .last_committed
            .keys()
            .copied()
            .filter(|plane_id| !self.plane_is_off(*plane_id))
            .collect();
        planes.sort_unstable();
        planes
    }

    /// Re-emit every property on every commit, for drivers that mishandle a
    /// partial write. Off by default; this is a quirk escape hatch, not a
    /// tuning knob -- it multiplies per-frame property traffic.
    pub const fn set_force_full_property_writes(&mut self, force: bool) {
        self.force_full_writes = force;
    }

    /// Whether full property writes are being forced.
    #[must_use]
    pub const fn force_full_property_writes(&self) -> bool {
        self.force_full_writes
    }

    /// Drop every trace of a layer the scene has removed.
    ///
    /// Without this the cached assignment still names the departed layer, so
    /// the FB-only fast path sees "a previously-placed layer is gone" and
    /// falls back to a tested pass on the frame after every removal -- one
    /// wasted `TEST_ONLY` commit for a configuration that cannot have become
    /// invalid, since dropping a layer only ever frees resources.
    ///
    /// Upstream nulls the committed baseline's layer pointer here rather than
    /// erasing it, because it keys identity on the `Layer`'s address and the
    /// allocator would otherwise mistake a fresh layer at a recycled address
    /// for a continuation of the old one -- and suppress property writes the
    /// kernel needs. Generation-tagged [`LayerId`]s make that unrepresentable
    /// here, so the entry is simply dropped.
    pub fn forget_layer(&mut self, layer: LayerId) {
        while let Some(plane_id) = self.previous.get_plane_of(layer) {
            self.previous.remove(plane_id);
        }
        // Clear the identity but keep the entry. The entry is the only record
        // that the kernel still has this plane lit: dropping it would make the
        // plane look already-off, so nothing would disable it and it would go
        // on scanning out the departed layer's last framebuffer.
        //
        // Losing the identity is what forces a full property write for the
        // next layer to land here, which is the other half of what upstream's
        // pointer-nulling buys.
        for baseline in self.last_committed.values_mut() {
            if baseline.layer == Some(layer) {
                baseline.layer = None;
            }
        }
        if self.previous.is_empty() {
            self.previous_valid = false;
        }
    }

    /// Forget a plane's committed baseline, after detaching it.
    pub fn forget_plane(&mut self, plane_id: u32) {
        self.last_committed.remove(&plane_id);
    }

    /// The cached assignment from the previous pass.
    #[must_use]
    pub const fn previous_allocation(&self) -> &PlaneAssignment {
        &self.previous
    }

    /// **Invariant 4.** Whether only content changed on already-placed layers.
    ///
    /// True when every plane committed last frame still maps to the same layer,
    /// and that layer's current `property_hash` matches what was recorded at
    /// commit. Since the hash excludes `FB_ID` and `IN_FENCE_FD`, a match
    /// proves geometry, format, and modifier are identical to the assignment
    /// the kernel already accepted — so the redundant `TEST_ONLY` can be
    /// skipped and the real commit's own `atomic_check` left as the sole
    /// arbiter.
    ///
    /// Any placement, format, or modifier edit defeats it, as does a
    /// previously-placed layer vanishing this frame.
    #[must_use]
    pub fn is_fb_only_frame(&self, present: &[LayerRef<'_>]) -> bool {
        if self.previous.is_empty() {
            return false;
        }
        self.previous.entries().iter().all(|(plane_id, layer_id)| {
            let Some(current) = present.iter().find(|entry| entry.id == *layer_id) else {
                return false; // a previously-placed layer is gone this frame
            };
            self.last_committed.get(plane_id).is_some_and(|baseline| {
                baseline.layer == Some(*layer_id) && baseline.hash == current.layer.property_hash()
            })
        })
    }

    /// Assign layers to planes.
    ///
    /// Layers that cannot be placed come back in
    /// [`Allocation::composited`] for the caller's composition fallback.
    ///
    /// # Errors
    ///
    /// [`TestFailure::NotMaster`] if DRM master was lost mid-search. Ordinary
    /// rejections are handled internally by trying smaller assignments.
    pub fn allocate<C: TestCommitter>(
        &mut self,
        layers: &[LayerRef<'_>],
        registry: &PlaneRegistry,
        crtc_index: u32,
        committer: &mut C,
    ) -> Result<Allocation, TestFailure> {
        self.canvas_slot = None;
        let allocation = self.allocate_planes(layers, registry, crtc_index, committer)?;
        if !allocation.composited.is_empty() && !stacks_by_plane_id(registry, crtc_index) {
            let bounds = canvas_bounds(&allocation.assignment, layers);
            self.canvas_slot = bounds.above.filter(|_| bounds.feasible());
        }
        Ok(allocation)
    }

    /// Where planes take a `zpos`, the zpos the composition canvas asks for
    /// after the last [`allocate`](Self::allocate): that of the lowest placed
    /// layer that overlaps a composited one and must cover it, for the canvas
    /// to stack just under (drm-cxx `07226e4`, #343). `None` when no placed
    /// layer must cover the canvas, which may then stack on top, and where
    /// planes stack by id, where the canvas plane's id places it.
    ///
    /// The slot asks for a placed layer's own zpos, so the canvas has to stack
    /// under ties: [`stacked_zpos_beneath`](crate::stacked_zpos_beneath).
    #[must_use]
    pub const fn canvas_zpos(&self) -> Option<u64> {
        self.canvas_slot
    }

    fn allocate_planes<C: TestCommitter>(
        &mut self,
        layers: &[LayerRef<'_>],
        registry: &PlaneRegistry,
        crtc_index: u32,
        committer: &mut C,
    ) -> Result<Allocation, TestFailure> {
        self.test_commits_this_frame = 0;
        self.budget_exhausted_this_frame = false;
        self.canvas_plane = None;
        let by_plane_id = stacks_by_plane_id(registry, crtc_index);

        // Layers the allocator must not touch: the scene owns their planes.
        let placeable: Vec<LayerRef<'_>> = layers
            .iter()
            .filter(|entry| {
                !entry.layer.is_externally_bound()
                    && !entry.layer.is_pinned()
                    && !entry.layer.is_transient_composited()
            })
            .copied()
            .collect();

        // Bottom-up, so the search hands the lowest layer the lowest plane.
        // Only a preference where planes take a written `zpos`; where they do
        // not, the plane-order path below keeps the order by construction.
        // Cheap here, and it cannot cost a placement: the matcher maximizes
        // cardinality first and uses score only to break ties, so reordering
        // the input changes which valid assignment is chosen, never whether
        // one is found.
        let mut placeable = placeable;
        placeable.sort_by_key(|entry| entry.layer.property(PropTag::Zpos).unwrap_or(0));

        let stacked = if by_plane_id {
            Self::plane_stack(layers)
        } else {
            Vec::new()
        };
        // The cached assignment is still valid to the kernel after a restack,
        // just stacked in the old order, and nothing but this notices
        // (drm-cxx#239).
        let order_holds = !by_plane_id || self.plane_order_holds(&stacked);
        // Where planes take a zpos, a move or restack can leave a placed layer
        // that would have to sit on both sides of the one canvas; the cached
        // assignment cannot fix that, so search again (drm-cxx `07226e4`).
        let order_holds =
            order_holds && (by_plane_id || canvas_bounds(&self.previous, layers).feasible());

        let has_new_layer =
            self.previous_valid && self.has_new_layer(&placeable, layers, registry, crtc_index);
        // A plane held for the canvas that the cached assignment was not
        // tested with needs a test before the pair is reused.
        let canvas_tested =
            self.canvas_hold.is_none() || self.canvas_hold == self.previous_canvas_plane;
        let canvas = self.canvas_hold.or(self.previous_canvas_plane);

        // --- invariant 4: the FB-only fast path ---------------------------
        if self.previous_valid
            && !has_new_layer
            && order_holds
            && canvas_tested
            && self.is_fb_only_frame(layers)
        {
            let assignment = self.previous.clone();
            self.canvas_plane = self.previous_canvas_plane;
            let composited = Self::unplaced(&placeable, &assignment);
            return Ok(Allocation {
                assignment,
                composited,
                diagnostics: Diagnostics {
                    test_commits_issued: 0,
                    fb_delta_fast_path: true,
                    budget_exhausted: false,
                },
            });
        }

        // --- warm start: re-validate the cached assignment ----------------
        if self.previous_valid
            && !has_new_layer
            && order_holds
            && !self.previous.is_empty()
            && self.previous_still_applies(&placeable)
            && {
                let planes: Vec<&PlaneCapabilities> = registry.for_crtc(crtc_index).collect();
                stacking_consistent_all(&planes, &self.previous, &placeable)
                    && multirect_complete(&planes, &self.previous)
            }
            && let Some(allocation) = self.warm_start(&placeable, canvas, committer)?
        {
            return Ok(allocation);
        }

        let assignment = if by_plane_id {
            self.place_in_plane_order(&stacked, layers, registry, crtc_index, committer)?
        } else {
            self.canvas_plane = self.canvas_hold;
            self.arm_canvas(self.canvas_hold, committer);
            let searched = self
                .full_search(&placeable, registry, crtc_index, committer)
                .and_then(|searched| {
                    self.keep_canvas_bounds(
                        searched, &placeable, layers, registry, crtc_index, committer,
                    )
                });
            self.arm_canvas(None, committer);
            searched?
        };
        let composited = Self::unplaced(&placeable, &assignment);
        if composited.is_empty() {
            self.canvas_plane = None;
        }

        self.previous = assignment.clone();
        self.previous_valid = !assignment.is_empty();
        self.previous_canvas_plane = self.canvas_plane;
        self.previous_composited.clone_from(&composited);
        self.previous_free = self.free_planes(&self.previous, layers, registry, crtc_index);

        Ok(Allocation {
            assignment,
            composited,
            diagnostics: Diagnostics {
                test_commits_issued: self.test_commits_this_frame,
                fb_delta_fast_path: false,
                budget_exhausted: self.budget_exhausted_this_frame,
            },
        })
    }

    /// Re-test the cached assignment, with the canvas armed on `canvas`.
    /// `None` when the kernel refuses it, which drops the cache for a full
    /// search.
    fn warm_start<C: TestCommitter>(
        &mut self,
        placeable: &[LayerRef<'_>],
        canvas: Option<u32>,
        committer: &mut C,
    ) -> Result<Option<Allocation>, TestFailure> {
        let pairs = Self::pairs_for(&self.previous, placeable);
        if pairs.len() != self.previous.len() {
            return Ok(None);
        }
        self.test_commits_this_frame += 1;
        self.arm_canvas(canvas, committer);
        let verdict = committer.test_assignment(&pairs);
        self.arm_canvas(None, committer);
        match verdict {
            Ok(()) => {
                let assignment = self.previous.clone();
                let composited = Self::unplaced(placeable, &assignment);
                // The plane the test armed is the canvas's now, and stays
                // paired with the cached assignment.
                self.canvas_plane = canvas.filter(|_| !composited.is_empty());
                self.previous_canvas_plane = self.canvas_plane;
                Ok(Some(Allocation {
                    assignment,
                    composited,
                    diagnostics: Diagnostics {
                        test_commits_issued: self.test_commits_this_frame,
                        fb_delta_fast_path: false,
                        budget_exhausted: false,
                    },
                }))
            }
            Err(TestFailure::NotMaster) => Err(TestFailure::NotMaster),
            Err(TestFailure::Rejected) => {
                self.previous_valid = false;
                Ok(None)
            }
        }
    }

    /// A full search's assignment, or one the one canvas can stack with.
    ///
    /// One canvas stacks at one zpos. When a placed layer would have to sit
    /// on both sides of it, composite one contiguous zpos run instead;
    /// failing that, move the caught layers into the composition and keep the
    /// smaller set if the kernel takes it. Otherwise the search's assignment
    /// stands, and the canvas stacks on top. Port of upstream's
    /// `full_search` tail (`07226e4`).
    fn keep_canvas_bounds<C: TestCommitter>(
        &mut self,
        searched: PlaneAssignment,
        placeable: &[LayerRef<'_>],
        layers: &[LayerRef<'_>],
        registry: &PlaneRegistry,
        crtc_index: u32,
        committer: &mut C,
    ) -> Result<PlaneAssignment, TestFailure> {
        if canvas_bounds(&searched, layers).feasible() {
            return Ok(searched);
        }
        if let Some(run) =
            self.place_around_run(searched.len(), layers, registry, crtc_index, committer)?
        {
            return Ok(run);
        }
        let planes: Vec<&PlaneCapabilities> = registry.for_crtc(crtc_index).collect();
        let mut resolved = searched.clone();
        if resolve_canvas_bounds(&mut resolved, layers, &planes)
            && (resolved.is_empty() || self.try_test(&resolved, placeable, committer)?)
        {
            return Ok(resolved);
        }
        Ok(searched)
    }

    /// Place every layer outside one contiguous zpos run of at least the
    /// layers the search left unplaced: for each run length, smallest first,
    /// the run with the lowest total keep-priority, the higher on a tie (a
    /// run at the top keeps the canvas on top). Layers the caller or the
    /// scene sent to the canvas must be in the run. `None` when no run leaves
    /// the canvas bounds feasible within the test budget.
    fn place_around_run<C: TestCommitter>(
        &mut self,
        placed: usize,
        layers: &[LayerRef<'_>],
        registry: &PlaneRegistry,
        crtc_index: u32,
        committer: &mut C,
    ) -> Result<Option<PlaneAssignment>, TestFailure> {
        let mut candidates: Vec<LayerRef<'_>> = layers
            .iter()
            .filter(|entry| {
                !entry.layer.is_composition_layer()
                    && !entry.layer.is_externally_bound()
                    && !entry.layer.is_pinned()
            })
            .copied()
            .collect();
        candidates.sort_by_key(|entry| entry.layer.property(PropTag::Zpos).unwrap_or(0));
        let n = candidates.len();
        let forced: Vec<usize> = (0..n)
            .filter(|&i| forced_composited(candidates[i].layer))
            .collect();
        let available: Vec<&PlaneCapabilities> = registry
            .for_crtc(crtc_index)
            .filter(|plane| !self.reserved.contains(&plane.id))
            .collect();

        for width in (n - placed.min(n)).max(1)..n {
            if self.test_commits_this_frame >= self.max_test_commits {
                self.budget_exhausted_this_frame = true;
                break;
            }
            let mut best: Option<(usize, i64)> = None;
            for start in 0..=n - width {
                let covers_forced = forced.iter().all(|&i| (start..start + width).contains(&i));
                if !covers_forced {
                    continue;
                }
                let cost: i64 = candidates[start..start + width]
                    .iter()
                    .map(|entry| i64::from(keep_priority(entry.layer)))
                    .sum();
                if best.is_none_or(|(_, best_cost)| cost <= best_cost) {
                    best = Some((start, cost));
                }
            }
            let Some((start, _)) = best else {
                continue;
            };
            let outside: Vec<LayerRef<'_>> = candidates
                .iter()
                .enumerate()
                .filter(|(i, _)| !(start..start + width).contains(i))
                .map(|(_, entry)| *entry)
                .collect();
            let assignment = self.place_group(&outside, &available, crtc_index, committer)?;
            if !assignment.is_empty() && canvas_bounds(&assignment, layers).feasible() {
                return Ok(Some(assignment));
            }
        }
        Ok(None)
    }

    /// Whether a layer present this frame needs a full search to have a
    /// fair shot at a plane: the previous frame did not place it, and either
    /// did not composite it either, or something the last search decided on
    /// has changed in a way that could let it place more.
    ///
    /// Both cached paths iterate the *previous* assignment to decide what to
    /// emit, so neither can place a layer that is not already in it:
    /// warm-start would succeed with the old set, the new layer would be
    /// composited, and the cached assignment would never grow -- so the same
    /// fate would hit every subsequent frame.
    ///
    /// A layer composited last frame is not new (drm-cxx `6e58d23`): counting
    /// it as one forced a full search on every frame composition was active.
    /// The cached paths composite it again, on the last search's verdict --
    /// unless a composited layer has gone, which can leave the rest few enough
    /// to fit, or a plane has come free that the search did not have. Upstream
    /// keeps the run on the canvas either way, so a scene that shrinks back
    /// under the plane count goes on blending forever (drm-cxx#341).
    ///
    /// A free plane alone is no reason: on a controller that lights fewer
    /// planes than it offers (RK3566 VOP2: three eligible, two usable) the
    /// search composited those layers with that plane free, and would again.
    fn has_new_layer(
        &self,
        placeable: &[LayerRef<'_>],
        layers: &[LayerRef<'_>],
        registry: &PlaneRegistry,
        crtc_index: u32,
    ) -> bool {
        let mut unplaced = 0;
        for entry in placeable {
            if self.previous.get_plane_of(entry.id).is_some() {
                continue;
            }
            if !self.previous_composited.contains(&entry.id) {
                return true;
            }
            unplaced += 1;
        }
        if unplaced == 0 {
            return false;
        }
        unplaced < self.previous_composited.len()
            || self
                .free_planes(&self.previous, layers, registry, crtc_index)
                .iter()
                .any(|plane| !self.previous_free.contains(plane))
    }

    /// The planes on `crtc_index` a search could still use beside
    /// `assignment`: not a cursor, not reserved, not a pinned layer's, not the canvas's.
    fn free_planes(
        &self,
        assignment: &PlaneAssignment,
        layers: &[LayerRef<'_>],
        registry: &PlaneRegistry,
        crtc_index: u32,
    ) -> Vec<u32> {
        let pinned: Vec<u32> = layers
            .iter()
            .filter(|entry| entry.layer.is_pinned())
            .filter_map(|entry| entry.layer.assigned_plane())
            .collect();
        registry
            .for_crtc(crtc_index)
            .filter(|plane| {
                plane.plane_type != PlaneType::Cursor
                    && !self.reserved.contains(&plane.id)
                    && !pinned.contains(&plane.id)
                    && assignment.get(plane.id).is_none()
                    // The canvas's plane is taken, held back or not.
                    && self.previous_canvas_plane != Some(plane.id)
            })
            .map(|plane| plane.id)
            .collect()
    }

    /// Whether every layer in the cached assignment is still present.
    fn previous_still_applies(&self, present: &[LayerRef<'_>]) -> bool {
        self.previous
            .entries()
            .iter()
            .all(|(_, id)| present.iter().any(|entry| entry.id == *id))
    }

    /// Resolve an assignment's layer ids against this frame's layers.
    fn pairs_for<'a>(
        assignment: &PlaneAssignment,
        present: &[LayerRef<'a>],
    ) -> Vec<(u32, LayerRef<'a>)> {
        assignment
            .entries()
            .iter()
            .filter_map(|(plane_id, layer_id)| {
                present
                    .iter()
                    .find(|entry| entry.id == *layer_id)
                    .map(|entry| (*plane_id, *entry))
            })
            .collect()
    }

    /// Layers that did not get a plane.
    fn unplaced(placeable: &[LayerRef<'_>], assignment: &PlaneAssignment) -> Vec<LayerId> {
        placeable
            .iter()
            .filter(|entry| !assignment.entries().iter().any(|(_, id)| *id == entry.id))
            .map(|entry| entry.id)
            .collect()
    }

    /// Every layer that reaches the screen through the allocator, in the one
    /// order planes stacked by id can show them: transient-composited layers
    /// too, which land on the canvas.
    ///
    /// Equal zpos asks for no order, so ties go lowest keep-priority first:
    /// the composited run has to be contiguous, and this is what lets it take
    /// the low-priority layers of a tie rather than whichever the caller added
    /// first. Upstream keeps caller order there. Stable, so equal priorities
    /// keep caller order.
    fn plane_stack<'a>(layers: &[LayerRef<'a>]) -> Vec<LayerRef<'a>> {
        let mut stacked: Vec<LayerRef<'a>> = layers
            .iter()
            .filter(|entry| !entry.layer.is_externally_bound() && !entry.layer.is_pinned())
            .copied()
            .collect();
        stacked.sort_by_key(|entry| {
            (
                entry.layer.property(PropTag::Zpos).unwrap_or(0),
                keep_priority(entry.layer),
            )
        });
        stacked
    }

    /// Whether the cached assignment and canvas plane still stack `stacked`
    /// in zpos order, on a CRTC whose planes stack by id.
    ///
    /// Without a canvas a layer left off the planes is not on screen at all,
    /// so it constrains nothing. Upstream fails the check there, which costs
    /// a canvas-less scene with a dropped layer its warm start every frame.
    fn plane_order_holds(&self, stacked: &[LayerRef<'_>]) -> bool {
        let with_canvas = !self.canvas_hosts.is_empty();
        let positions: Vec<Position> = stacked
            .iter()
            .filter_map(|entry| {
                let plane = self.previous.get_plane_of(entry.id);
                if plane.is_none() && !with_canvas {
                    return None;
                }
                Some(Position {
                    zpos: entry.layer.property(PropTag::Zpos).unwrap_or(0),
                    plane: plane.or(self.previous_canvas_plane),
                    on_canvas: plane.is_none(),
                })
            })
            .collect();
        plane_order_consistent(&positions)
    }

    /// Place zpos-ordered `stacked` on a CRTC whose planes stack by id.
    ///
    /// Layers map onto planes in id order, and those left over form one
    /// contiguous zpos run on [`canvas_plane`](Self::canvas_plane), between
    /// the run's neighbors. Most layers placed wins, then the lowest total
    /// keep-priority composited; a refused `TEST_ONLY` retries with one placed
    /// layer fewer. The spatial split does not apply -- the canvas spans every
    /// group -- and cursor planes are left out: their size and update rules
    /// are the cursor path's.
    ///
    /// Conservative for disjoint layers, which keep plane order too.
    fn place_in_plane_order<C: TestCommitter>(
        &mut self,
        stacked: &[LayerRef<'_>],
        layers: &[LayerRef<'_>],
        registry: &PlaneRegistry,
        crtc_index: u32,
        committer: &mut C,
    ) -> Result<PlaneAssignment, TestFailure> {
        // A pinned layer's plane is the scene's.
        let pinned: Vec<u32> = layers
            .iter()
            .filter(|entry| entry.layer.is_pinned())
            .filter_map(|entry| entry.layer.assigned_plane())
            .collect();
        // Left out rather than modeled: a multirect virtual plane, which needs
        // its parent armed alongside.
        let mut planes: Vec<&PlaneCapabilities> = registry
            .for_crtc(crtc_index)
            .filter(|plane| {
                plane.plane_type != PlaneType::Cursor
                    && plane.multirect_parent.is_none()
                    && !self.reserved.contains(&plane.id)
                    && !pinned.contains(&plane.id)
            })
            .collect();
        planes.sort_by_key(|plane| plane.id);
        if stacked.is_empty() {
            return Ok(PlaneAssignment::new());
        }

        let order = PlaneOrder::new(stacked.len(), planes.len(), |i, j| {
            let (plane, layer) = (planes[j], stacked[i].layer);
            plane_statically_compatible(plane, layer, crtc_index)
                && !self.probe_rejected(crtc_index, plane.id, layer)
                && self.failure_cache.lookup(plane.id, layer.property_hash()) != Some(false)
        });
        let forced = stacked
            .iter()
            .position(|entry| forced_composited(entry.layer))
            .zip(
                stacked
                    .iter()
                    .rposition(|entry| forced_composited(entry.layer)),
            )
            .map(|(first, last)| first..last + 1);
        let host_ids = self.canvas_hosts.clone();
        let hosts = |j: usize| host_ids.contains(&planes[j].id);
        let hosts_canvas: Option<&dyn Fn(usize) -> bool> = (!host_ids.is_empty()).then_some(&hosts);
        let cost = |i: usize| i64::from(keep_priority(stacked[i].layer));

        let mut max_placed = stacked.len();
        loop {
            let Some(choice) = order.choose(max_placed, forced.as_ref(), hosts_canvas, cost) else {
                return Ok(PlaneAssignment::new());
            };
            let mut assignment = PlaneAssignment::new();
            for (i, entry) in stacked.iter().enumerate() {
                if let Some(j) = order.plane_of(i, &choice) {
                    assignment.insert(planes[j].id, entry.id);
                }
            }
            self.canvas_plane = choice.canvas.map(|j| planes[j].id);
            if assignment.is_empty() {
                return Ok(assignment);
            }
            self.arm_canvas(self.canvas_plane, committer);
            let passed = self.try_test(&assignment, stacked, committer);
            self.arm_canvas(None, committer);
            if passed? {
                return Ok(assignment);
            }
            if self.test_commits_this_frame >= self.max_test_commits {
                // Out of tests: composite everything rather than arm an
                // untested stack.
                self.budget_exhausted_this_frame = true;
                self.canvas_plane = order
                    .choose(0, forced.as_ref(), hosts_canvas, cost)
                    .and_then(|all| all.canvas)
                    .map(|j| planes[j].id);
                return Ok(PlaneAssignment::new());
            }
            max_placed = assignment.len() - 1;
        }
    }

    /// Arm the canvas on `plane` in the tests that follow, or disarm it.
    fn arm_canvas<C: TestCommitter>(&self, plane: Option<u32>, committer: &mut C) {
        match (plane, &self.canvas_layer) {
            (Some(plane_id), Some(layer)) => committer.set_extra_plane(plane_id, layer.clone()),
            _ => committer.clear_extra_plane(),
        }
    }

    /// Full search: split into independent groups, place each from a shared
    /// plane pool.
    fn full_search<C: TestCommitter>(
        &mut self,
        placeable: &[LayerRef<'_>],
        registry: &PlaneRegistry,
        crtc_index: u32,
        committer: &mut C,
    ) -> Result<PlaneAssignment, TestFailure> {
        let mut available: Vec<&PlaneCapabilities> = registry
            .for_crtc(crtc_index)
            .filter(|plane| !self.reserved.contains(&plane.id))
            .collect();
        let layer_refs: Vec<&Layer> = placeable.iter().map(|entry| entry.layer).collect();
        let groups = split_independent_groups(&layer_refs);

        let mut result = PlaneAssignment::new();
        for group in groups {
            let members: Vec<LayerRef<'_>> = group.iter().map(|&i| placeable[i]).collect();
            let placed = self.place_group(&members, &available, crtc_index, committer)?;

            for (plane_id, layer_id) in placed.entries() {
                result.insert(*plane_id, *layer_id);
            }
            // Planes this group took are gone for the next one.
            available.retain(|plane| placed.get(plane.id).is_none());
        }

        Ok(result)
    }

    /// Place one spatially independent group: preseed, greedy, then backtrack.
    fn place_group<C: TestCommitter>(
        &mut self,
        members: &[LayerRef<'_>],
        planes: &[&PlaneCapabilities],
        crtc_index: u32,
        committer: &mut C,
    ) -> Result<PlaneAssignment, TestFailure> {
        // Highest keep-priority first, so when planes are scarce the matching
        // and the greedy pass keep the higher-priority layers. The matching is
        // weight-agnostic about *which* layer it drops, so input order is what
        // decides who survives. Stable, so equal priorities keep caller order.
        let mut ordered: Vec<LayerRef<'_>> = members.to_vec();
        ordered.sort_by(|a, b| keep_priority(b.layer).cmp(&keep_priority(a.layer)));

        // --- bipartite preseed --------------------------------------------
        let mut assignment = self.preseed(&ordered, planes, crtc_index);
        if assignment.is_empty() {
            return Ok(assignment);
        }
        // The matching knows nothing about stacking, so an inverted preseed
        // goes straight to the greedy pass rather than to a TEST that would
        // accept it. Likewise a multirect virtual plane matched without its
        // parent, which the TEST would refuse.
        if stacking_consistent_all(planes, &assignment, &ordered)
            && multirect_complete(planes, &assignment)
            && self.try_test(&assignment, &ordered, committer)?
        {
            return Ok(assignment);
        }

        // --- greedy from ranked candidates ---------------------------------
        assignment.clear();
        let mut candidates = self.rank_candidates(&ordered, planes, crtc_index);
        candidates.sort_by(|a, b| b.2.cmp(&a.2));

        // Ordinary planes first, then multirect virtual planes, each only
        // once the first pass has taken its parent.
        let (ordinary, virtual_planes): (Vec<_>, Vec<_>) =
            candidates.into_iter().partition(|(plane_id, _, _)| {
                caps_in(planes, *plane_id).is_none_or(|plane| plane.multirect_parent.is_none())
            });
        let mut used_planes: Vec<u32> = Vec::new();
        let mut used_layers: Vec<LayerId> = Vec::new();
        for (plane_id, layer, _) in ordinary.into_iter().chain(virtual_planes) {
            if used_planes.contains(&plane_id) || used_layers.contains(&layer.id) {
                continue;
            }
            if self
                .failure_cache
                .lookup(plane_id, layer.layer.property_hash())
                == Some(false)
            {
                continue;
            }
            if self.probe_rejected(crtc_index, plane_id, layer.layer) {
                continue;
            }
            // The kernel accepts an inverted stack, so no TEST would catch
            // one: a layer is placed only where it stacks in the order its
            // zpos asks relative to what is already placed.
            if !fits_stacking(planes, &assignment, &ordered, plane_id, layer.layer)
                || !fits_multirect(planes, &assignment, plane_id)
            {
                continue;
            }
            assignment.insert(plane_id, layer.id);
            used_planes.push(plane_id);
            used_layers.push(layer.id);
        }

        if assignment.is_empty() {
            return Ok(assignment);
        }
        if self.try_test(&assignment, &ordered, committer)? {
            return Ok(assignment);
        }

        // --- backtrack: drop lowest keep-priority first ---------------------
        let mut by_priority: Vec<(u32, LayerId)> = assignment.entries().to_vec();
        by_priority.sort_by_key(|(_, layer_id)| {
            ordered
                .iter()
                .find(|entry| entry.id == *layer_id)
                .map_or(i32::MAX, |entry| keep_priority(entry.layer))
        });

        for (plane_id, _) in by_priority {
            if assignment.remove(plane_id).is_none() {
                continue; // already dropped as an orphaned multirect child
            }
            // Dropping a multirect parent orphans its virtual plane; drop that
            // too.
            let children: Vec<u32> = assignment
                .entries()
                .iter()
                .map(|(id, _)| *id)
                .filter(|id| {
                    caps_in(planes, *id).is_some_and(|c| c.multirect_parent == Some(plane_id))
                })
                .collect();
            for child in children {
                assignment.remove(child);
            }
            if assignment.is_empty() {
                break;
            }
            if self.try_test(&assignment, &ordered, committer)? {
                break;
            }
            if self.test_commits_this_frame >= self.max_test_commits {
                self.budget_exhausted_this_frame = true;
                break;
            }
        }

        Ok(assignment)
    }

    /// Run one test commit, honoring the per-frame budget and recording the
    /// verdict.
    fn try_test<C: TestCommitter>(
        &mut self,
        assignment: &PlaneAssignment,
        present: &[LayerRef<'_>],
        committer: &mut C,
    ) -> Result<bool, TestFailure> {
        if self.test_commits_this_frame >= self.max_test_commits {
            self.budget_exhausted_this_frame = true;
            return Ok(false);
        }
        let pairs = Self::pairs_for(assignment, present);
        if pairs.len() != assignment.len() {
            return Ok(false);
        }

        self.test_commits_this_frame += 1;
        match committer.test_assignment(&pairs) {
            Ok(()) => {
                for (plane_id, layer) in &pairs {
                    self.failure_cache
                        .record(*plane_id, layer.layer.property_hash(), true);
                }
                Ok(true)
            }
            Err(TestFailure::NotMaster) => Err(TestFailure::NotMaster),
            Err(TestFailure::Rejected) => {
                for (plane_id, layer) in &pairs {
                    self.failure_cache
                        .record(*plane_id, layer.layer.property_hash(), false);
                }
                Ok(false)
            }
        }
    }

    /// Maximum-cardinality preseed over the statically compatible edges.
    fn preseed(
        &mut self,
        ordered: &[LayerRef<'_>],
        planes: &[&PlaneCapabilities],
        crtc_index: u32,
    ) -> PlaneAssignment {
        self.matcher.reset(ordered.len(), planes.len());

        for (left, entry) in ordered.iter().enumerate() {
            for (right, plane) in planes.iter().enumerate() {
                if !plane_statically_compatible(plane, entry.layer, crtc_index) {
                    continue;
                }
                if self
                    .failure_cache
                    .lookup(plane.id, entry.layer.property_hash())
                    == Some(false)
                {
                    continue;
                }
                if self.probe_rejected(crtc_index, plane.id, entry.layer) {
                    continue;
                }
                let score = self.score(plane, entry);
                self.matcher.add_scored_edge(left, right, score);
            }
        }

        self.matcher.solve();

        let mut assignment = PlaneAssignment::new();
        for (left, entry) in ordered.iter().enumerate() {
            if let Some(right) = self.matcher.match_for_left(left) {
                assignment.insert(planes[right].id, entry.id);
            }
        }
        assignment
    }

    /// Every statically compatible `(plane, layer)` pair with its score.
    fn rank_candidates<'a>(
        &self,
        ordered: &[LayerRef<'a>],
        planes: &[&PlaneCapabilities],
        crtc_index: u32,
    ) -> Vec<(u32, LayerRef<'a>, i32)> {
        let mut candidates = Vec::new();
        for entry in ordered {
            for plane in planes {
                if plane_statically_compatible(plane, entry.layer, crtc_index) {
                    candidates.push((plane.id, *entry, self.score(plane, entry)));
                }
            }
        }
        candidates
    }

    /// Score a candidate, folding in the warm-start bonus and failure history.
    fn score(&self, plane: &PlaneCapabilities, entry: &LayerRef<'_>) -> i32 {
        let held_last_frame = self.previous_valid && self.previous.get(plane.id) == Some(entry.id);
        score_pair(
            plane,
            entry.layer,
            ScoreContext {
                held_last_frame,
                failure_hits: self
                    .failure_cache
                    .failure_count(plane.id, entry.layer.property_hash()),
            },
        )
    }

    /// Whether a prior single-plane probe proved this layer's
    /// `(fourcc, modifier)` cannot scan out here — a lying `IN_FORMATS`.
    fn probe_rejected(&self, crtc_index: u32, plane_id: u32, layer: &Layer) -> bool {
        let Some(fourcc) = layer.format() else {
            return false;
        };
        self.probe_cache
            .lookup(crtc_index, plane_id, fourcc, Modifier(layer.modifier()))
            == Verdict::Rejected
    }

    /// Record that a plane rejected a `(fourcc, modifier)` pair, so a lying
    /// `IN_FORMATS` costs one probe rather than a dropped edge re-probed every
    /// frame.
    pub fn record_probe_rejection(&mut self, crtc_index: u32, plane_id: u32, layer: &Layer) {
        if let Some(fourcc) = layer.format() {
            self.probe_cache.record(
                crtc_index,
                plane_id,
                fourcc,
                Modifier(layer.modifier()),
                false,
            );
        }
    }
}

/// Whether the caller or the scene has sent `layer` to the canvas, whatever
/// the planes could take.
const fn forced_composited(layer: &Layer) -> bool {
    layer.is_force_composited() || layer.is_transient_composited()
}

/// Whether `layer` on `plane_id` stacks consistently with everything already
/// in `assignment` (see [`stacking_consistent`](crate::stacking_consistent)).
///
/// A plane missing from `planes` cannot be ruled on and is not held against
/// the layer, which is what a plane without a zpos property gets as well.
fn fits_stacking(
    planes: &[&PlaneCapabilities],
    assignment: &PlaneAssignment,
    present: &[LayerRef<'_>],
    plane_id: u32,
    layer: &Layer,
) -> bool {
    let caps = |id: u32| planes.iter().find(|plane| plane.id == id).copied();
    let Some(plane) = caps(plane_id) else {
        return true;
    };
    let zpos = layer.property(PropTag::Zpos);
    assignment.entries().iter().all(|(other_plane, other_id)| {
        if *other_plane == plane_id {
            return true;
        }
        let (Some(other), Some(entry)) = (
            caps(*other_plane),
            present.iter().find(|entry| entry.id == *other_id),
        ) else {
            return true;
        };
        crate::stacking_consistent(plane, zpos, other, entry.layer.property(PropTag::Zpos))
    })
}

/// The capabilities of `plane_id` among `planes`.
fn caps_in<'a>(planes: &[&'a PlaneCapabilities], plane_id: u32) -> Option<&'a PlaneCapabilities> {
    planes.iter().find(|plane| plane.id == plane_id).copied()
}

/// Whether `plane_id` may join `assignment`: not a multirect virtual plane, or
/// one whose parent is already in it (see [`multirect_pairing_ok`]).
///
/// As with stacking, a plane missing from `planes` is not held against it.
fn fits_multirect(
    planes: &[&PlaneCapabilities],
    assignment: &PlaneAssignment,
    plane_id: u32,
) -> bool {
    caps_in(planes, plane_id).is_none_or(|plane| {
        crate::multirect_pairing_ok(plane.multirect_parent, |parent| {
            assignment.get(parent).is_some()
        })
    })
}

/// Whether no multirect virtual plane in `assignment` is without its parent.
fn multirect_complete(planes: &[&PlaneCapabilities], assignment: &PlaneAssignment) -> bool {
    assignment
        .entries()
        .iter()
        .all(|(plane_id, _)| fits_multirect(planes, assignment, *plane_id))
}

/// Whether every placement in `assignment` stacks consistently.
fn stacking_consistent_all(
    planes: &[&PlaneCapabilities],
    assignment: &PlaneAssignment,
    present: &[LayerRef<'_>],
) -> bool {
    assignment.entries().iter().all(|(plane_id, layer_id)| {
        present
            .iter()
            .find(|entry| entry.id == *layer_id)
            .is_none_or(|entry| fits_stacking(planes, assignment, present, *plane_id, entry.layer))
    })
}

/// Which side of the one composition canvas the placed layers must keep.
///
/// A placed layer that overlaps a composited one stays on the side of the
/// canvas its zpos asks for: `below` is the highest zpos that must stay under
/// the canvas, `above` the lowest that must cover it. Port of upstream's
/// `Allocator::CanvasBounds` (`07226e4`).
#[derive(Debug, Clone, Copy, Default)]
struct CanvasBounds {
    below: Option<u64>,
    above: Option<u64>,
}

impl CanvasBounds {
    /// Whether one canvas zpos satisfies every placed layer: not when one
    /// would have to sit on both sides.
    fn feasible(self) -> bool {
        match (self.below, self.above) {
            (Some(below), Some(above)) => below < above,
            _ => true,
        }
    }
}

/// The layers `assignment` places, with the pinned layers on planes of their
/// own, and the layers it leaves to the canvas.
fn placed_and_composited<'a>(
    assignment: &PlaneAssignment,
    layers: &[LayerRef<'a>],
) -> (Vec<&'a Layer>, Vec<&'a Layer>) {
    let mut placed = Vec::new();
    let mut composited = Vec::new();
    for entry in layers {
        let layer = entry.layer;
        if layer.is_composition_layer() || layer.is_externally_bound() {
            continue;
        }
        if assignment.get_plane_of(entry.id).is_some() {
            placed.push(layer);
        } else if layer.is_pinned() {
            if layer
                .assigned_plane()
                .is_some_and(|plane_id| assignment.get(plane_id).is_none())
            {
                placed.push(layer);
            }
        } else {
            composited.push(layer);
        }
    }
    (placed, composited)
}

/// The canvas bounds `assignment` imposes on the layers it leaves composited.
fn canvas_bounds(assignment: &PlaneAssignment, layers: &[LayerRef<'_>]) -> CanvasBounds {
    let (placed, composited) = placed_and_composited(assignment, layers);
    let mut bounds = CanvasBounds::default();
    for placed in &placed {
        let Some(zp) = placed.property(PropTag::Zpos) else {
            continue;
        };
        for composited in &composited {
            let Some(zc) = composited.property(PropTag::Zpos) else {
                continue;
            };
            if zc == zp || !layers_intersect(placed, composited) {
                continue;
            }
            if zc < zp {
                bounds.above = Some(bounds.above.map_or(zp, |above| above.min(zp)));
            } else {
                bounds.below = Some(bounds.below.map_or(zp, |below| below.max(zp)));
            }
        }
    }
    bounds
}

/// Move placed layers into the composition until the canvas bounds are
/// feasible: each round, the lowest keep-priority layer caught between them
/// that overlaps a composited one, with any multirect virtual plane riding on
/// its plane. `false` when nothing placed by the search is caught, which
/// leaves the pinned layers.
fn resolve_canvas_bounds(
    assignment: &mut PlaneAssignment,
    layers: &[LayerRef<'_>],
    planes: &[&PlaneCapabilities],
) -> bool {
    loop {
        let bounds = canvas_bounds(assignment, layers);
        let (Some(above), Some(below)) = (bounds.above, bounds.below) else {
            return true;
        };
        if below < above {
            return true;
        }
        let (_, composited) = placed_and_composited(assignment, layers);
        let mut victim: Option<(u32, i32)> = None;
        for (plane_id, layer_id) in assignment.entries() {
            let Some(entry) = layers.iter().find(|entry| entry.id == *layer_id) else {
                continue;
            };
            let Some(z) = entry.layer.property(PropTag::Zpos) else {
                continue;
            };
            let caught = z >= above
                && z <= below
                && composited.iter().any(|c| {
                    c.property(PropTag::Zpos)
                        .is_some_and(|zc| zc != z && layers_intersect(entry.layer, c))
                });
            if !caught {
                continue;
            }
            let keep = keep_priority(entry.layer);
            if victim.is_none_or(|(_, victim_keep)| keep < victim_keep) {
                victim = Some((*plane_id, keep));
            }
        }
        let Some((victim, _)) = victim else {
            return false;
        };
        assignment.remove(victim);
        let children: Vec<u32> = assignment
            .entries()
            .iter()
            .map(|(id, _)| *id)
            .filter(|id| caps_in(planes, *id).is_some_and(|c| c.multirect_parent == Some(victim)))
            .collect();
        for child in children {
            assignment.remove(child);
        }
    }
}
