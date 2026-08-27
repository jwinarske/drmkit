// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! Several scenes, one frame.
//!
//! Port of `src/scene/scene_set.{hpp,cpp}`.
//!
//! A [`LayerScene`] drives one CRTC. A [`SceneSet`] drives several and decides
//! how many atomic commits that costs — which is the whole question, because
//! one commit across every output is *atomic* and several are not, while one
//! commit means one output's refusal takes the rest down with it. See
//! [`NarrowPolicy`].
//!
//! # Mirrored layers
//!
//! [`add_layer`](SceneSet::add_layer) takes one source and a target per
//! scene, so the same content reaches several outputs — a presentation
//! mirrored to a projector, a status bar on every screen. Each target carries
//! its own geometry, since the outputs rarely share a resolution.

mod partition;
pub use partition::{NarrowPolicy, SlotState, partition_for_policy};

use std::rc::Rc;

use drmkit_scene::{DisplayParams, LayerBufferSource, LayerHandle, LayerScene};

mod shared;
use shared::SharedSource;

#[cfg(test)]
mod tests;

/// Where one mirrored layer lands on one scene.
#[derive(Debug, Clone, Copy)]
pub struct Target {
    /// Which scene in the set, by index.
    pub scene_index: usize,
    /// How to display it there.
    ///
    /// Per target, not per layer: two outputs rarely share a resolution, and
    /// mirroring at one geometry would letterbox or crop the other.
    pub display: DisplayParams,
    /// Force this copy through composition rather than onto a plane.
    pub force_composited: bool,
}

/// One layer across one or more scenes.
pub struct LayerSpec {
    /// The content. Shared across every target — that is what makes it a
    /// mirror rather than two layers that happen to look alike.
    pub source: Rc<std::cell::RefCell<dyn LayerBufferSource>>,
    /// Where it goes. At least one.
    pub targets: Vec<Target>,
}

impl std::fmt::Debug for LayerSpec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LayerSpec")
            .field("targets", &self.targets.len())
            .finish_non_exhaustive()
    }
}

/// A handle to a layer that may span several scenes.
///
/// Distinct from [`LayerHandle`], which names a layer within one scene: this
/// names the *set* of them a single [`add_layer`](SceneSet::add_layer)
/// created, so removing it removes every copy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SetLayerHandle {
    id: u32,
    generation: u32,
}

impl SetLayerHandle {
    /// Whether this handle ever named anything.
    ///
    /// A default handle does not. Checked rather than assumed because the
    /// default is what a caller gets from an uninitialised field, and
    /// removing with one must not remove whatever happens to be at index
    /// zero.
    #[must_use]
    pub const fn is_valid(self) -> bool {
        self.id != 0
    }
}

/// Why a set operation was refused.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[non_exhaustive]
pub enum SetError {
    /// A layer was added with no targets.
    ///
    /// Refused rather than accepted as a no-op: a caller that built an empty
    /// target list has a bug upstream of here, and silently creating a layer
    /// that appears nowhere hides it until someone asks why the screen is
    /// blank.
    #[error("a layer needs at least one target")]
    NoTargets,

    /// A target named a scene the set does not have.
    #[error("scene index {index} is out of range ({count} scenes)")]
    NoSuchScene {
        /// What was asked for.
        index: usize,
        /// How many the set holds, holes included.
        count: usize,
    },
}

/// One layer's copies, so removal can find them all.
struct SetLayer {
    generation: u32,
    /// Which scene each copy lives in, and its handle there.
    copies: Vec<(usize, LayerHandle)>,
}

/// Several scenes committed together.
pub struct SceneSet {
    /// `None` is a hole: a removed scene, kept so indices stay stable.
    scenes: Vec<Option<LayerScene>>,
    layers: Vec<Option<SetLayer>>,
    next_generation: u32,
}

impl std::fmt::Debug for SceneSet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SceneSet")
            .field("slots", &self.scenes.len())
            .field("engaged", &self.scenes.iter().flatten().count())
            .finish_non_exhaustive()
    }
}

impl SceneSet {
    /// A set over `scenes`, in the order given.
    ///
    /// The index is the caller's handle to an output for the set's lifetime,
    /// so the order is theirs to choose and is never rearranged.
    #[must_use]
    pub fn new(scenes: Vec<LayerScene>) -> Self {
        Self {
            scenes: scenes.into_iter().map(Some).collect(),
            layers: Vec::new(),
            next_generation: 1,
        }
    }

    /// How many slots the set has, holes included.
    ///
    /// Not how many scenes: a hole still occupies an index, because that is
    /// what keeps every other index meaning what it did.
    #[must_use]
    pub fn slot_count(&self) -> usize {
        self.scenes.len()
    }

    /// How many slots hold a scene.
    #[must_use]
    pub fn scene_count(&self) -> usize {
        self.scenes.iter().flatten().count()
    }

    /// The scene at `index`, if that slot holds one.
    #[must_use]
    pub fn scene(&self, index: usize) -> Option<&LayerScene> {
        self.scenes.get(index)?.as_ref()
    }

    /// The scene at `index`, mutably.
    pub fn scene_mut(&mut self, index: usize) -> Option<&mut LayerScene> {
        self.scenes.get_mut(index)?.as_mut()
    }

    /// Add a scene, reusing a hole if there is one.
    ///
    /// Reusing keeps the set from growing without bound as outputs come and
    /// go — a laptop docking and undocking all day would otherwise leave a
    /// slot behind each time.
    pub fn add_scene(&mut self, scene: LayerScene) -> usize {
        if let Some(index) = self.scenes.iter().position(Option::is_none) {
            self.scenes[index] = Some(scene);
            return index;
        }
        self.scenes.push(Some(scene));
        self.scenes.len() - 1
    }

    /// Remove the scene at `index`, leaving a hole.
    ///
    /// Out of range is a no-op, and so is a slot that is already a hole: both
    /// are what a caller reacting to an output that has gone away will do
    /// twice.
    pub fn remove_scene(&mut self, index: usize) {
        if let Some(slot) = self.scenes.get_mut(index) {
            *slot = None;
        }
    }

    /// Add one layer to every target scene.
    ///
    /// # Errors
    ///
    /// [`SetError::NoTargets`] for an empty target list, and
    /// [`SetError::NoSuchScene`] if a target names a slot the set does not
    /// have or that holds no scene. **Checked before anything is added**, so a
    /// spec naming three scenes and one bad index leaves the set as it was
    /// rather than half-populated.
    pub fn add_layer(&mut self, spec: &LayerSpec) -> Result<SetLayerHandle, SetError> {
        if spec.targets.is_empty() {
            return Err(SetError::NoTargets);
        }
        for target in &spec.targets {
            if self.scene(target.scene_index).is_none() {
                return Err(SetError::NoSuchScene {
                    index: target.scene_index,
                    count: self.scenes.len(),
                });
            }
        }

        let mut copies = Vec::with_capacity(spec.targets.len());
        for target in &spec.targets {
            let shared = SharedSource(Rc::clone(&spec.source));
            // Every index was checked above and nothing since then can have
            // removed a scene, so a missing slot here would be a bug in this
            // function rather than a caller error -- skipping it silently
            // would leave the layer on fewer outputs than asked for.
            let Some(scene) = self.scene_mut(target.scene_index) else {
                continue;
            };
            let handle = scene.add_layer(Box::new(shared));
            if let Some(layer) = scene.layer_mut(handle) {
                layer.set_display(target.display);
                layer.set_force_composited(target.force_composited);
            }
            copies.push((target.scene_index, handle));
        }

        let generation = self.next_generation;
        self.next_generation = self.next_generation.wrapping_add(1).max(1);
        self.layers.push(Some(SetLayer { generation, copies }));

        Ok(SetLayerHandle {
            // One-based: zero is the "never issued" value a default handle
            // carries, and it must not name the first layer.
            id: u32::try_from(self.layers.len()).unwrap_or(u32::MAX),
            generation,
        })
    }

    /// Remove every copy of a layer.
    ///
    /// A handle that was never issued, or was issued and already removed, is
    /// ignored. The generation is what makes the second case safe: a slot
    /// reused by a later layer does not answer to the old handle.
    pub fn remove_layer(&mut self, handle: SetLayerHandle) {
        if !handle.is_valid() {
            return;
        }
        let Some(index) = (handle.id as usize).checked_sub(1) else {
            return;
        };
        let Some(slot) = self.layers.get_mut(index) else {
            return;
        };
        let Some(layer) = slot.as_ref() else { return };
        if layer.generation != handle.generation {
            return;
        }

        let Some(taken) = slot.take() else { return };
        let copies = taken.copies;
        for (scene_index, layer_handle) in copies {
            if let Some(scene) = self.scene_mut(scene_index) {
                scene.remove_layer(layer_handle);
            }
        }
    }

    /// What each slot looks like this frame, for the partition.
    ///
    /// `wants_modeset` is the caller's to say: the scene does not know whether
    /// the mode has been set, since the commit path owns the `Modeset` and
    /// this type does not commit for the caller.
    #[must_use]
    pub fn slot_states(&self, wants_modeset: &[usize]) -> Vec<SlotState> {
        self.scenes
            .iter()
            .enumerate()
            .map(|(index, slot)| SlotState {
                is_hole: slot.is_none(),
                wants_modeset: wants_modeset.contains(&index),
            })
            .collect()
    }

    /// How this frame splits into commits.
    ///
    /// The groups are slot indices, in the order they should be committed.
    #[must_use]
    pub fn plan_commits(&self, wants_modeset: &[usize], policy: NarrowPolicy) -> Vec<Vec<usize>> {
        partition_for_policy(&self.slot_states(wants_modeset), policy)
    }
}
