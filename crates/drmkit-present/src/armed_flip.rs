// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! Teardown's bounded wait for the flip the last commit armed.
//!
//! Port of `wait_for_armed_flip` from `src/scene/layer_scene.cpp`
//! (`debd061`). Upstream's scene owns the device, so its destructor waits.
//! Here the scene has no device and its `Drop` stays non-blocking
//! (invariant 5); the wait lives with the two types that own both the scene
//! and the descriptor it commits on, [`ScanoutBackend`](crate::ScanoutBackend)
//! and [`DumbScanoutSink`](crate::DumbScanoutSink).
//!
//! It never reads the event queue. Most callers dispatch the flip event
//! themselves, and a teardown that consumed it would take it from them.
//! Instead it watches the CRTC's vblank sequence: once that has moved past
//! the one read right after the commit, the flip has landed. It also polls
//! the queue, without reading it: a driver that completes the flip inside the
//! commit (the SA8155P) has moved the sequence before it is read, and does
//! not move it again until the next commit, so the wait would always run to
//! its bound. The kernel queues the event only once the flip has landed
//! (drm-cxx `42287bd`). A CRTC with no vblank counter at all leaves the
//! queued event as the only thing to watch, and the wait watches it alone.

use std::os::fd::AsFd;
use std::time::{Duration, Instant};

use drmkit_scene::LayerScene;

/// How long teardown waits before giving up on a CRTC that stopped counting.
const BOUND: Duration = Duration::from_millis(100);
/// How often it looks.
const STEP: Duration = Duration::from_millis(2);

/// What teardown waits on, read right after a real commit that armed a flip.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ArmedFlip {
    /// The CRTC's vblank sequence then: the flip has landed once it moves
    /// past this.
    Sequence(u64),
    /// The CRTC has no vblank counter to read. A `panel-mipi-dbi` panel is
    /// one: its pipe has no vblank interrupt, and the helpers send the flip
    /// event themselves once the update is done. The queued event is then
    /// the only sign the flip landed, and the wait watches for it alone.
    /// Treating an unreadable counter as nothing to wait on tore the
    /// buffers down under the flip and tripped the scene's invariant-5
    /// tripwire (P-50).
    Unsequenced,
}

/// What to wait on, read right after a real commit succeeded.
///
/// `None` when the commit armed no flip.
pub(crate) fn armed_sequence(
    device: &impl AsFd,
    crtc_id: u32,
    arms_flip: bool,
) -> Option<ArmedFlip> {
    arms_flip.then(|| {
        drmkit_core::crtc_sequence(device, crtc_id)
            .map_or(ArmedFlip::Unsequenced, ArmedFlip::Sequence)
    })
}

/// Wait, bounded, for the flip armed at `armed` to land, then tell the scene.
///
/// A no-op when the scene has no flip outstanding, is suspended, or the last
/// commit armed nothing. If the wait gives up, the scene is left believing
/// the flip is outstanding, so its own `Drop` still warns.
pub(crate) fn settle(
    scene: &mut LayerScene,
    device: &impl AsFd,
    crtc_id: u32,
    armed: Option<ArmedFlip>,
) {
    let Some(armed) = armed else { return };
    if !scene.has_pending_flip() || scene.is_suspended() {
        return;
    }
    let deadline = Instant::now() + BOUND;
    loop {
        // A counter that stops reading partway is no worse than one that
        // never read: the event still says when the flip landed.
        if let ArmedFlip::Sequence(armed) = armed
            && drmkit_core::crtc_sequence(device, crtc_id).is_ok_and(|now| now > armed)
        {
            scene.flip_landed();
            return;
        }
        if drmkit_core::event_pending(device) {
            scene.flip_landed();
            return;
        }
        if Instant::now() >= deadline {
            return;
        }
        std::thread::sleep(STEP);
    }
}
