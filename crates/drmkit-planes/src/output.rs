// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! One CRTC's worth of layers.
//!
//! Port of `src/planes/output.{hpp,cpp}`.

use crate::allocator::{LayerId, LayerRef};
use crate::layer::Layer;
use crate::prop::PropTag;

/// The layers destined for one CRTC, in presentation order.
///
/// # Deviation: the composition layer is owned here
///
/// The C++ `Output` takes the composition layer by reference and owns only the
/// rest, keeping a `std::vector<std::unique_ptr<Layer>>` alongside a
/// `std::vector<Layer*>` view it can reorder independently.
///
/// Here every layer is owned by the `Output` and the composition layer is
/// identified by [`LayerId`]. Holding a `&mut Layer` in the struct would infect
/// every caller with a lifetime parameter for no benefit, and owning it removes
/// the second vector — and with it the C++'s dead `rebuild_layer_ptrs`, which
/// would have silently undone [`sort_layers_by_zpos`](Self::sort_layers_by_zpos)
/// had anything called it (drm-cxx#237).
#[derive(Debug, Default)]
pub struct Output {
    crtc_id: u32,
    /// Owned layers in presentation order. Reordered by
    /// [`sort_layers_by_zpos`](Self::sort_layers_by_zpos).
    layers: Vec<(LayerId, Layer)>,
    next_id: u64,
    composition: Option<LayerId>,
}

impl Output {
    /// An output for `crtc_id` with no layers.
    #[must_use]
    pub const fn new(crtc_id: u32) -> Self {
        Self {
            crtc_id,
            layers: Vec::new(),
            next_id: 1,
            composition: None,
        }
    }

    /// The CRTC this output drives.
    #[must_use]
    pub const fn crtc_id(&self) -> u32 {
        self.crtc_id
    }

    /// Add an empty layer, returning its id.
    ///
    /// Ids are never reused for the life of the output, which is what lets the
    /// allocator key its warm-start state on them safely.
    pub fn add_layer(&mut self) -> LayerId {
        let id = LayerId(self.next_id);
        self.next_id += 1;
        self.layers.push((id, Layer::new()));
        id
    }

    /// Remove a layer. Returns whether it was there.
    ///
    /// If it was the composition layer, the output is left with none.
    pub fn remove_layer(&mut self, id: LayerId) -> bool {
        let Some(index) = self.layers.iter().position(|(known, _)| *known == id) else {
            return false;
        };
        self.layers.remove(index);
        if self.composition == Some(id) {
            self.composition = None;
        }
        true
    }

    /// Borrow a layer.
    #[must_use]
    pub fn layer(&self, id: LayerId) -> Option<&Layer> {
        self.layers
            .iter()
            .find(|(known, _)| *known == id)
            .map(|(_, layer)| layer)
    }

    /// Borrow a layer mutably.
    pub fn layer_mut(&mut self, id: LayerId) -> Option<&mut Layer> {
        self.layers
            .iter_mut()
            .find(|(known, _)| *known == id)
            .map(|(_, layer)| layer)
    }

    /// Every layer with its id, in presentation order.
    pub fn layers(&self) -> impl Iterator<Item = LayerRef<'_>> + '_ {
        self.layers
            .iter()
            .map(|(id, layer)| LayerRef { id: *id, layer })
    }

    /// Every layer, as the slice the allocator takes.
    #[must_use]
    pub fn layer_refs(&self) -> Vec<LayerRef<'_>> {
        self.layers().collect()
    }

    /// How many layers the output holds.
    #[must_use]
    pub fn len(&self) -> usize {
        self.layers.len()
    }

    /// Whether the output holds no layers.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.layers.is_empty()
    }

    /// Designate a layer as the composition pass's output.
    ///
    /// Clears the flag on any previous composition layer, so exactly one layer
    /// carries it. Returns whether `id` names a layer of this output.
    pub fn set_composition_layer(&mut self, id: LayerId) -> bool {
        if self.layer(id).is_none() {
            return false;
        }
        if let Some(previous) = self.composition
            && let Some(layer) = self.layer_mut(previous)
        {
            layer.set_composition_layer(false);
        }
        self.composition = Some(id);
        if let Some(layer) = self.layer_mut(id) {
            layer.set_composition_layer(true);
        }
        true
    }

    /// The composition layer's id, if one is designated.
    #[must_use]
    pub const fn composition_layer(&self) -> Option<LayerId> {
        self.composition
    }

    /// Whether any layer changed since the last
    /// [`mark_clean`](Self::mark_clean).
    #[must_use]
    pub fn any_layer_dirty(&self) -> bool {
        self.layers.iter().any(|(_, layer)| layer.is_dirty())
    }

    /// The layers that changed, in presentation order.
    pub fn changed_layers(&self) -> impl Iterator<Item = LayerRef<'_>> + '_ {
        self.layers().filter(|entry| entry.layer.is_dirty())
    }

    /// Clear every layer's dirty flag.
    pub fn mark_clean(&mut self) {
        for (_, layer) in &mut self.layers {
            layer.mark_clean();
        }
    }

    /// Reorder layers by ascending `zpos`, treating an unset `zpos` as 0.
    ///
    /// Stable, so layers sharing a `zpos` keep the order they were added in.
    pub fn sort_layers_by_zpos(&mut self) {
        self.layers
            .sort_by_key(|(_, layer)| layer.property(PropTag::Zpos).unwrap_or(0));
    }
}
