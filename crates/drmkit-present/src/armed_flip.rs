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
//! the one read right after the commit, the flip has landed.

use std::os::fd::AsFd;
use std::time::{Duration, Instant};

use drmkit_scene::LayerScene;

/// How long teardown waits before giving up on a CRTC that stopped counting.
const BOUND: Duration = Duration::from_millis(100);
/// How often it looks.
const STEP: Duration = Duration::from_millis(2);

/// The sequence to wait past, read right after a real commit succeeded.
///
/// `None` when the commit armed no flip, or the sequence could not be read --
/// a driver without vblank support has nothing to wait on.
pub(crate) fn armed_sequence(device: &impl AsFd, crtc_id: u32, arms_flip: bool) -> Option<u64> {
    arms_flip
        .then(|| drmkit_core::crtc_sequence(device, crtc_id).ok())
        .flatten()
}

/// Wait, bounded, for the flip armed at `armed` to land, then tell the scene.
///
/// A no-op when the scene has no flip outstanding, is suspended, or no
/// sequence was recorded. If the wait gives up, or the sequence cannot be
/// read, the scene is left believing the flip is outstanding, so its own
/// `Drop` still warns.
pub(crate) fn settle(scene: &mut LayerScene, device: &impl AsFd, crtc_id: u32, armed: Option<u64>) {
    let Some(armed) = armed else { return };
    if !scene.has_pending_flip() || scene.is_suspended() {
        return;
    }
    let deadline = Instant::now() + BOUND;
    loop {
        match drmkit_core::crtc_sequence(device, crtc_id) {
            Ok(now) if now > armed => {
                scene.flip_landed();
                return;
            }
            Ok(_) => {}
            Err(_) => return,
        }
        if Instant::now() >= deadline {
            return;
        }
        std::thread::sleep(STEP);
    }
}
