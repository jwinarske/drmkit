// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! One source, several scenes.

use std::cell::RefCell;
use std::rc::Rc;

use drmkit_core::Device;
use drmkit_scene::{AcquiredBuffer, BindingModel, LayerBufferSource, SourceError, SourceFormat};
use drmkit_sync::SyncFence;

/// A source shared by every scene a mirrored layer reaches.
///
/// A scene owns its sources for `'static`, so the same one cannot be given to
/// two scenes directly. Sharing is what makes mirroring a mirror: two copies
/// of the source would be two independent producers drawing the same thing
/// twice, at twice the cost and with no guarantee they stay in step.
///
/// `RefCell` rather than a lock because the commit path is single-threaded by
/// contract — a lock here would suggest the scenes could be committed from
/// different threads, which the atomic ioctl they share does not allow.
pub(crate) struct SharedSource(pub(crate) Rc<RefCell<dyn LayerBufferSource>>);

impl LayerBufferSource for SharedSource {
    /// Acquire from the shared producer.
    ///
    /// Every mirrored copy acquires independently and gets the same buffer,
    /// which is the point: one render, several outputs. The release side is
    /// what a caller has to think about — the source hears one release per
    /// copy, and a ring sized for one output will starve driving two.
    fn acquire(&mut self) -> Result<AcquiredBuffer, SourceError> {
        self.0.borrow_mut().acquire()
    }

    fn release(&mut self, acquired: AcquiredBuffer) {
        self.0.borrow_mut().release(acquired);
    }

    fn release_with_fence(&mut self, acquired: AcquiredBuffer, release_fence: Option<SyncFence>) {
        self.0
            .borrow_mut()
            .release_with_fence(acquired, release_fence);
    }

    fn wants_release_fence(&self) -> bool {
        self.0.borrow().wants_release_fence()
    }

    fn has_fresh_content(&self) -> bool {
        self.0.borrow().has_fresh_content()
    }

    fn binding_model(&self) -> BindingModel {
        self.0.borrow().binding_model()
    }

    fn format(&self) -> SourceFormat {
        LayerBufferSource::format(&*self.0.borrow())
    }

    fn on_session_paused(&mut self) {
        self.0.borrow_mut().on_session_paused();
    }

    fn on_session_resumed(&mut self, device: &Device) -> Result<(), SourceError> {
        self.0.borrow_mut().on_session_resumed(device)
    }
}
