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
/// different threads, which a combined atomic commit across them does not
/// allow.
pub(crate) struct SharedSource(pub(crate) Rc<RefCell<dyn LayerBufferSource>>);

impl LayerBufferSource for SharedSource {
    /// Acquire from the shared producer.
    ///
    /// Every mirrored copy calls the inner source's `acquire`. A
    /// static-buffer source returns the same buffer each time, which is the
    /// point: one render, several outputs. A ring or swap-chain source hands
    /// each copy a different buffer, or none, so it does not suit mirroring.
    /// The release side is what a caller has to think about — the source
    /// hears one release per copy.
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

    // `map` is not forwarded, though the reference forwards it. A mapping
    // borrows the source for as long as it lives, and the source is behind a
    // `RefCell` the caller also holds: the borrow guard cannot outlive this
    // call, and handing the mapping out without it would let the caller, or
    // another copy, re-allocate or map the buffer while the view is live. So
    // a shared source cannot be composited; its layer needs a plane.

    /// Not forwarded. One copy retiring says nothing about the others, which
    /// may still be scanning the same buffer out; letting it go to the
    /// producer would have it overwritten mid-scanout on those outputs.
    fn on_retired(&mut self) {}

    fn binding_model(&self) -> BindingModel {
        self.0.borrow().binding_model()
    }

    fn format(&self) -> SourceFormat {
        LayerBufferSource::format(&*self.0.borrow())
    }

    /// Forwarded, as the reference forwards it: a driver-owned producer is
    /// bound per plane, and each copy lands on a plane of its own.
    fn bind_to_plane(&mut self, plane_id: u32) -> Result<(), SourceError> {
        self.0.borrow_mut().bind_to_plane(plane_id)
    }

    fn unbind_from_plane(&mut self, plane_id: u32) {
        self.0.borrow_mut().unbind_from_plane(plane_id);
    }

    fn on_session_paused(&mut self) {
        self.0.borrow_mut().on_session_paused();
    }

    fn on_session_resumed(&mut self, device: &Device) -> Result<(), SourceError> {
        self.0.borrow_mut().on_session_resumed(device)
    }
}
