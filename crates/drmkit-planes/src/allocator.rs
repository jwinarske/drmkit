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
use crate::registry::{PlaneCapabilities, PlaneRegistry};
use crate::scoring::{
    ScoreContext, keep_priority, plane_statically_compatible, score_pair, split_independent_groups,
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
    passed: bool,
    hits: u32,
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

    /// Record a verdict, counting repeat failures so scoring can back off.
    pub fn record(&mut self, plane_id: u32, property_hash: u64, passed: bool) {
        let entry = self.entries.entry((plane_id, property_hash)).or_default();
        entry.passed = passed;
        entry.hits = entry.hits.saturating_add(1);
    }

    /// How many times this combination has been recorded.
    #[must_use]
    pub fn hit_count(&self, plane_id: u32, property_hash: u64) -> u32 {
        self.entries
            .get(&(plane_id, property_hash))
            .map_or(0, |entry| entry.hits)
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
    layer: LayerId,
    /// `property_hash` at commit time, which excludes `FB_ID` and
    /// `IN_FENCE_FD`. The fast path compares against this to prove geometry,
    /// format, and modifier are unchanged since the kernel accepted them.
    hash: u64,
}

/// Assigns layers to display planes.
#[derive(Debug)]
pub struct Allocator {
    previous: PlaneAssignment,
    previous_valid: bool,
    last_committed: HashMap<u32, LastCommitted>,
    failure_cache: TestCache,
    probe_cache: ModifierProbeCache,
    max_test_commits: usize,
    test_commits_this_frame: usize,
    matcher: BipartiteMatching,
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
            failure_cache: TestCache::new(),
            probe_cache: ModifierProbeCache::new(),
            max_test_commits: Self::DEFAULT_MAX_TEST_COMMITS,
            test_commits_this_frame: 0,
            matcher: BipartiteMatching::new(),
        }
    }

    /// Set the per-frame test-commit budget.
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
                layer: layer.id,
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
        self.last_committed.clear();
        for (plane_id, entry) in applied {
            self.record_committed(*plane_id, *entry);
        }
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
        self.last_committed
            .retain(|_, baseline| baseline.layer != layer);
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
                baseline.layer == *layer_id && baseline.hash == current.layer.property_hash()
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
        self.test_commits_this_frame = 0;

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

        // A layer present this frame that the previous frame did not place.
        //
        // Both cached paths below iterate the *previous* assignment to decide
        // what to emit, so neither can place a layer that is not already in it:
        // warm-start would succeed with the old set, the new layer would be
        // composited, and the cached assignment would never grow -- so the same
        // fate would hit every subsequent frame. Detecting it here forces a
        // full search, giving the new layer a real shot at a plane.
        let has_new_layer = self.previous_valid
            && placeable
                .iter()
                .any(|entry| self.previous.get_plane_of(entry.id).is_none());

        // --- invariant 4: the FB-only fast path ---------------------------
        if self.previous_valid && !has_new_layer && self.is_fb_only_frame(layers) {
            let assignment = self.previous.clone();
            let composited = Self::unplaced(&placeable, &assignment);
            return Ok(Allocation {
                assignment,
                composited,
                diagnostics: Diagnostics {
                    test_commits_issued: 0,
                    fb_delta_fast_path: true,
                },
            });
        }

        // --- warm start: re-validate the cached assignment ----------------
        if self.previous_valid
            && !has_new_layer
            && !self.previous.is_empty()
            && self.previous_still_applies(&placeable)
        {
            let pairs = Self::pairs_for(&self.previous, &placeable);
            if pairs.len() == self.previous.len() {
                self.test_commits_this_frame += 1;
                match committer.test_assignment(&pairs) {
                    Ok(()) => {
                        let assignment = self.previous.clone();
                        let composited = Self::unplaced(&placeable, &assignment);
                        return Ok(Allocation {
                            assignment,
                            composited,
                            diagnostics: Diagnostics {
                                test_commits_issued: self.test_commits_this_frame,
                                fb_delta_fast_path: false,
                            },
                        });
                    }
                    Err(TestFailure::NotMaster) => return Err(TestFailure::NotMaster),
                    Err(TestFailure::Rejected) => {
                        // Fall through to a full search.
                        self.previous_valid = false;
                    }
                }
            }
        }

        let assignment = self.full_search(&placeable, registry, crtc_index, committer)?;
        let composited = Self::unplaced(&placeable, &assignment);

        self.previous = assignment.clone();
        self.previous_valid = !assignment.is_empty();

        Ok(Allocation {
            assignment,
            composited,
            diagnostics: Diagnostics {
                test_commits_issued: self.test_commits_this_frame,
                fb_delta_fast_path: false,
            },
        })
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

    /// Full search: split into independent groups, place each from a shared
    /// plane pool.
    fn full_search<C: TestCommitter>(
        &mut self,
        placeable: &[LayerRef<'_>],
        registry: &PlaneRegistry,
        crtc_index: u32,
        committer: &mut C,
    ) -> Result<PlaneAssignment, TestFailure> {
        let mut available: Vec<&PlaneCapabilities> = registry.for_crtc(crtc_index).collect();
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
        if self.try_test(&assignment, &ordered, committer)? {
            return Ok(assignment);
        }

        // --- greedy from ranked candidates ---------------------------------
        assignment.clear();
        let mut candidates = self.rank_candidates(&ordered, planes, crtc_index);
        candidates.sort_by(|a, b| b.2.cmp(&a.2));

        let mut used_planes: Vec<u32> = Vec::new();
        let mut used_layers: Vec<LayerId> = Vec::new();
        for (plane_id, layer, _) in candidates {
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
            assignment.remove(plane_id);
            if assignment.is_empty() {
                break;
            }
            if self.try_test(&assignment, &ordered, committer)? {
                break;
            }
            if self.test_commits_this_frame >= self.max_test_commits {
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
                    .hit_count(plane.id, entry.layer.property_hash()),
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
