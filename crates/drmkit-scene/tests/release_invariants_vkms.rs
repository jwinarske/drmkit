// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! The three release invariants, against a real device.
//!
//! Ports `test_layer_scene_release_vkms.cpp`. Every case here is `#[ignore]`d
//! and runs under the vkms lane with `--include-ignored`.
//!
//! The mechanism behind each is already pinned host-side, in `drmkit-scene`'s
//! unit tests, where the kernel's answer is a parameter. What these add is that
//! the mechanism is reached the same way through a **real** commit — a
//! distinction that matters, because a port can have a correct release ring
//! wired to the wrong place.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Mutex;

mod common;

use drmkit_core::{AtomicCommitFlags, Device};
use drmkit_fmt::fourcc;
use drmkit_planes::PlaneRegistry;
use drmkit_scene::{
    AcquiredBuffer, CommitKind, DeviceCommitter, DisplayParams, KernelResult, LayerBufferSource,
    LayerScene, PlanePropertyMap, Rect, SourceError, SourceFormat,
};

/// DRM master is per open file description, so card-dependent cases serialize.
static CARD_LOCK: Mutex<()> = Mutex::new(());

fn card_guard() -> std::sync::MutexGuard<'static, ()> {
    CARD_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn open_card() -> Option<Device> {
    let path = std::env::var("DRMKIT_TEST_CARD").unwrap_or_else(|_| "/dev/dri/card0".to_owned());
    // Absence of the device is a different condition from the device being
    // there and misbehaving: locally it means vkms is not loaded, which is a
    // skip; in the lane it means the lane is not testing what it claims.
    let device = match Device::open(&path) {
        Ok(device) => device,
        Err(error) => {
            assert!(
                std::env::var_os("DRMKIT_REQUIRE_MASTER").is_none(),
                "{path}: {error}, but DRMKIT_REQUIRE_MASTER is set"
            );
            println!("note: skipped -- no DRM device at {path} ({error})");
            return None;
        }
    };
    device.enable_universal_planes().expect("universal planes");
    device.enable_atomic().expect("atomic");
    Some(device)
}

/// Whether this process holds DRM master, which every real commit needs.
///
/// Under virtme-ng the suite is root with no display server and always has it;
/// on a desktop a compositor holds it. The lane sets `DRMKIT_REQUIRE_MASTER`,
/// which turns "not master" from a tolerated local condition into a failure --
/// so a case here cannot pass in CI by quietly doing nothing.
fn require_master(device: &Device) -> bool {
    if device.set_master().is_ok() {
        return true;
    }
    let required = std::env::var_os("DRMKIT_REQUIRE_MASTER").is_some();
    assert!(
        !required,
        "not DRM master, but DRMKIT_REQUIRE_MASTER is set"
    );
    println!("note: skipped -- another client holds DRM master");
    false
}

/// What a source did.
#[derive(Debug, Default)]
struct SourceLog {
    released: Vec<u32>,
}

/// A source over a real dumb buffer, so its framebuffer id is one the kernel
/// will accept.
struct RealSource {
    inner: drmkit_scene_sources::DumbBufferSource,
    log: Rc<RefCell<SourceLog>>,
}

impl LayerBufferSource for RealSource {
    fn acquire(&mut self) -> Result<AcquiredBuffer, SourceError> {
        self.inner.acquire()
    }

    fn release(&mut self, acquired: AcquiredBuffer) {
        self.log.borrow_mut().released.push(acquired.fb_id);
        self.inner.release(acquired);
    }

    fn format(&self) -> SourceFormat {
        self.inner.format()
    }
}

/// Everything a case needs: a device, a registry, a property map, and a scene
/// with one full-screen layer.
///
/// # Only one reason to skip
///
/// `harness` returns `None` **only** when another client holds DRM master,
/// which is a real condition on a developer desktop and a failure in the lane
/// (see `require_master`). Everything else asserts.
///
/// An earlier version used `?` throughout, so a missing CRTC, an unreadable
/// plane, or a failed allocation would silently return `None` and every case
/// would pass having done nothing. That is the same trap that made four of the
/// `drmkit-scene-sources` fence tests vacuous, and it is worth not repeating.
struct Harness {
    device: Device,
    registry: PlaneRegistry,
    map: PlanePropertyMap,
    scene: LayerScene,
    log: Rc<RefCell<SourceLog>>,
    crtc_index: u32,
    /// The one layer, for a case that needs to remove it.
    handle: drmkit_scene::LayerHandle,
}

fn harness() -> Option<Harness> {
    use drm::control::Device as _;

    let device = open_card()?;
    if !require_master(&device) {
        return None;
    }

    let resources = device
        .resource_handles()
        .expect("a modeset card must report its resources");
    let (crtc_id, crtc_index) = common::pick_crtc(&device, &resources);

    // Build the registry from the device's real planes.
    //
    // `possible_crtcs` comes back as an opaque filter rather than a mask, so
    // resolve it through `filter_crtcs` and set the bit for each allowed CRTC's
    // index -- which is what drmkit's own mask is indexed by.
    let all_crtcs = resources.crtcs();
    let mut capabilities = Vec::new();
    for handle in device
        .plane_handles()
        .expect("a universal-planes card must report its planes")
    {
        let info = device.get_plane(handle).expect("plane info");
        let plane_id: u32 = handle.into();

        let mut mask = 0u32;
        for allowed in resources.filter_crtcs(info.possible_crtcs()) {
            if let Some(index) = all_crtcs.iter().position(|c| *c == allowed)
                && index < u32::BITS as usize
            {
                mask |= 1 << index;
            }
        }

        capabilities.push(drmkit_planes::PlaneCapabilities {
            id: plane_id,
            possible_crtcs: mask,
            plane_type: drmkit_planes::PlaneType::Overlay,
            formats: vec![fourcc::XRGB8888],
            supports_scaling: true,
            ..drmkit_planes::PlaneCapabilities::default()
        });
    }
    let registry = PlaneRegistry::from_capabilities(capabilities);

    let mut map = PlanePropertyMap::new();
    map.learn_all(&device, &registry)
        .expect("every advertised plane must expose readable properties");

    assert!(
        registry
            .force_disable_candidates(crtc_index)
            .next()
            .is_some(),
        "CRTC {crtc_index} has no compatible planes; the lane is not testing \
         what it claims to"
    );

    let log = Rc::new(RefCell::new(SourceLog::default()));
    let mut scene = LayerScene::new(crtc_id);
    let source =
        drmkit_scene_sources::DumbBufferSource::create(&device, 64, 64, fourcc::XRGB8888).ok()?;
    let handle = scene.add_layer(Box::new(RealSource {
        inner: source,
        log: Rc::clone(&log),
    }));
    scene
        .layer_mut(handle)
        .expect("layer")
        .set_display(DisplayParams {
            dst_rect: Rect {
                x: 0,
                y: 0,
                w: 64,
                h: 64,
            },
            ..DisplayParams::default()
        });

    Some(Harness {
        device,
        registry,
        map,
        scene,
        log,
        crtc_index,
        handle,
    })
}

/// Build a frame, run the allocator against the real device, and finalize with
/// the given kernel answer — without issuing the real commit, so a case can
/// choose the outcome.
fn cycle(h: &mut Harness, result: KernelResult) -> drmkit_scene::CommitReport {
    let mut committer = DeviceCommitter::new(
        &h.device,
        &h.map,
        &h.registry,
        h.crtc_index,
        AtomicCommitFlags::empty(),
        None,
    );
    let build = h
        .scene
        .build_frame(
            &h.registry,
            h.crtc_index,
            CommitKind::Real { arms_flip: false },
            &mut committer,
        )
        .expect("build");
    h.scene.finalize_frame(build, result)
}

// --- invariant 1 -------------------------------------------------------------

/// **Invariant 1.** A buffer is released only after the flip that stops
/// scanning it out completes — two commits deep, never at ioctl return.
#[test]
#[ignore = "needs a DRM device; run under the vkms lane with --include-ignored"]
fn defers_release_two_commits_deep_vkms() {
    let _guard = card_guard();
    let Some(mut h) = harness() else { return };

    cycle(&mut h, KernelResult::Ok);
    assert!(
        h.log.borrow().released.is_empty(),
        "frame 1's buffer is on screen; releasing at ioctl return is the tear"
    );

    cycle(&mut h, KernelResult::Ok);
    assert!(
        h.log.borrow().released.is_empty(),
        "frame 1 only leaves the screen when frame 2's flip completes, which \
         the next commit observes"
    );

    cycle(&mut h, KernelResult::Ok);
    assert_eq!(
        h.log.borrow().released.len(),
        1,
        "two commits deep: frame 1 retires now"
    );

    h.scene.drain();
}

// --- invariant 2 -------------------------------------------------------------

/// **Invariant 2.** A rejected commit releases that frame's acquisitions
/// immediately, and a non-`EACCES` failure does not suspend the scene.
#[test]
#[ignore = "needs a DRM device; run under the vkms lane with --include-ignored"]
fn commit_failure_releases_acquisitions_vkms() {
    let _guard = card_guard();
    let Some(mut h) = harness() else { return };

    cycle(&mut h, KernelResult::Ok);
    let before = h.log.borrow().released.len();

    cycle(&mut h, KernelResult::Rejected);
    assert_eq!(
        h.log.borrow().released.len(),
        before + 1,
        "the rejected frame's buffer comes straight back -- a producer ring \
         reuses it immediately"
    );
    assert!(
        !h.scene.is_suspended(),
        "a non-EACCES failure must not suspend the scene"
    );

    // And the scene keeps running.
    cycle(&mut h, KernelResult::Ok);
    h.scene.drain();
}

/// Lost DRM master is the one failure that suspends.
#[test]
#[ignore = "needs a DRM device; run under the vkms lane with --include-ignored"]
fn lost_master_suspends_the_scene_vkms() {
    let _guard = card_guard();
    let Some(mut h) = harness() else { return };

    cycle(&mut h, KernelResult::NotMaster);
    assert!(h.scene.is_suspended());

    h.scene.resume();
    cycle(&mut h, KernelResult::Ok);
    h.scene.drain();
}

// --- invariant 5 -------------------------------------------------------------

/// **Invariant 5.** Draining hands every held buffer back without waiting on
/// the kernel, and an armed flip is still the caller's to land.
#[test]
#[ignore = "needs a DRM device; run under the vkms lane with --include-ignored"]
fn drain_lands_pending_flip_before_teardown_vkms() {
    let _guard = card_guard();
    let Some(mut h) = harness() else { return };

    // Two frames, so both generations are populated.
    cycle(&mut h, KernelResult::Ok);
    cycle(&mut h, KernelResult::Ok);
    assert!(h.log.borrow().released.is_empty(), "both still in flight");

    h.scene.drain();
    assert_eq!(
        h.log.borrow().released.len(),
        2,
        "draining returns every held buffer, so nothing is stranded at teardown"
    );

    // A scene with nothing armed drops cleanly; the tripwire only fires when a
    // flip is outstanding, which is what makes it a useful signal.
    assert!(!h.scene.has_pending_flip());
}

/// A real commit that arms a flip leaves the tripwire set until the event is
/// dispatched — the hazard the invariant describes.
#[test]
#[ignore = "needs a DRM device; run under the vkms lane with --include-ignored"]
fn an_armed_flip_keeps_teardown_unsafe_until_landed_vkms() {
    let _guard = card_guard();
    let Some(mut h) = harness() else { return };

    let mut committer = DeviceCommitter::new(
        &h.device,
        &h.map,
        &h.registry,
        h.crtc_index,
        AtomicCommitFlags::empty(),
        None,
    );
    let build = h
        .scene
        .build_frame(
            &h.registry,
            h.crtc_index,
            CommitKind::Real { arms_flip: true },
            &mut committer,
        )
        .expect("build");
    h.scene.finalize_frame(build, KernelResult::Ok);

    assert!(
        h.scene.has_pending_flip(),
        "the flip references a framebuffer teardown would destroy"
    );

    h.scene.drain();
    assert!(
        h.scene.has_pending_flip(),
        "draining returns buffers; it does not wait on the kernel"
    );

    h.scene.flip_landed();
    assert!(!h.scene.has_pending_flip());
}

/// A `TEST_ONLY` commit releases its acquisitions at once.
///
/// Nothing scans out from a test commit, so there is no in-flight flip to gate
/// the release on. Holding the buffers across one would stall the source's
/// ring for no reason -- visible on a pull-mode source, a V4L2 capture say, as
/// an `EAGAIN` loop where the producer has nothing free to fill.
///
/// The allocator issues these during a search, so a hold here would cost one
/// buffer per candidate assignment tried.
#[test]
#[ignore = "needs a DRM device; run under the vkms lane with --include-ignored"]
fn test_commits_release_immediately_vkms() {
    let _guard = card_guard();
    let Some(mut h) = harness() else { return };

    let before = h.log.borrow().released.len();

    let mut committer = DeviceCommitter::new(
        &h.device,
        &h.map,
        &h.registry,
        h.crtc_index,
        AtomicCommitFlags::empty(),
        None,
    );
    let build = h
        .scene
        .build_frame(&h.registry, h.crtc_index, CommitKind::Test, &mut committer)
        .expect("build");
    h.scene.finalize_frame(build, KernelResult::Ok);

    let released = h.log.borrow().released.len() - before;
    assert_eq!(
        released, 1,
        "a test commit acquired a buffer and did not give it back"
    );

    // And again, to show it is not a one-off that happens to drain on the
    // second: a search issues several of these back to back.
    let before = h.log.borrow().released.len();
    let mut committer = DeviceCommitter::new(
        &h.device,
        &h.map,
        &h.registry,
        h.crtc_index,
        AtomicCommitFlags::empty(),
        None,
    );
    let build = h
        .scene
        .build_frame(&h.registry, h.crtc_index, CommitKind::Test, &mut committer)
        .expect("build");
    h.scene.finalize_frame(build, KernelResult::Ok);
    assert_eq!(h.log.borrow().released.len() - before, 1);
}

/// Removing a layer whose buffer is still being scanned out defers retiring
/// its source.
///
/// The kernel is reading that framebuffer until the next flip lands. Dropping
/// the source -- and with it the buffer -- at `remove_layer` would free memory
/// the display engine is mid-scanout of, which is a tear at best and a fault
/// at worst.
#[test]
#[ignore = "needs a DRM device; run under the vkms lane with --include-ignored"]
fn removing_a_layer_defers_retiring_its_source_vkms() {
    let _guard = card_guard();
    let Some(mut h) = harness() else { return };

    // One real commit, so there is a buffer in flight to defer against.
    let report = cycle(&mut h, KernelResult::Ok);
    assert!(report.layers_assigned > 0, "the layer reached a plane");

    let handle = h.handle;
    assert!(h.scene.remove_layer(handle), "the layer was there");
    assert!(
        h.scene.layer(handle).is_none(),
        "and is gone from the scene immediately"
    );

    // The source has not been given its buffer back yet: the flip that stops
    // scanning it out has not landed.
    let released_at_removal = h.log.borrow().released.len();

    // Land it and run the frame that follows, which is where the deferred
    // retirement happens.
    h.scene.flip_landed();
    cycle(&mut h, KernelResult::Ok);
    h.scene.drain();

    assert!(
        h.log.borrow().released.len() > released_at_removal,
        "the source was never given its buffer back, so it was dropped while \
         the kernel still had it or leaked"
    );
}
