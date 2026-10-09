// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! Parity port of `tests/integration/test_scanout_backend_vkms.cpp` from
//! drm-cxx @ `4a0b64a`.
//!
//! End to end: discover the output, negotiate, allocate through a GBM
//! producer, build the single-layer scene, and commit. These modeset, so they
//! take DRM master and run one at a time.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::{Mutex, MutexGuard};

use drmkit_core::{AtomicCommitFlags, Device};
use drmkit_present::{BackendConfig, GbmScanoutProducer, RestorePolicy, ScanoutBackend, VrrPolicy};
use drmkit_scene::{
    AcquiredBuffer, BindingModel, DmaBufDesc, LayerBufferSource, SourceError, SourceFormat,
};
use drmkit_sync::SyncFence;

static CARD_LOCK: Mutex<()> = Mutex::new(());

fn card_guard() -> MutexGuard<'static, ()> {
    CARD_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn open_card() -> Option<Device> {
    let path = std::env::var("DRMKIT_TEST_CARD").unwrap_or_else(|_| "/dev/dri/card0".to_owned());
    drmkit_testkit::announce_card(&path);
    let device = match Device::open(&path) {
        Ok(device) => device,
        Err(error) => {
            assert!(
                std::env::var_os("DRMKIT_REQUIRE_MASTER").is_none(),
                "{path}: {error}, but DRMKIT_REQUIRE_MASTER is set"
            );
            drmkit_testkit::skipped(&format!("no DRM device at {path} ({error})"));
            return None;
        }
    };
    device.enable_universal_planes().ok()?;
    device.enable_atomic().ok()?;
    if device.set_master().is_err() {
        assert!(
            std::env::var_os("DRMKIT_REQUIRE_MASTER").is_none(),
            "not DRM master, but DRMKIT_REQUIRE_MASTER is set"
        );
        drmkit_testkit::skipped("another client holds DRM master");
        return None;
    }
    Some(device)
}

/// The backend finds the output, allocates through the producer, and puts a
/// full-screen layer on a plane.
#[test]
#[ignore = "needs a DRM device and DRM master"]
fn a_full_screen_layer_reaches_a_plane_vkms() {
    let _guard = card_guard();
    let Some(device) = open_card() else { return };

    let mut producer = GbmScanoutProducer::new(&device);
    let mut backend =
        match ScanoutBackend::create(&device, &mut producer, &BackendConfig::default()) {
            Ok(backend) => backend,
            Err(error) => {
                drmkit_testkit::skipped(&format!("no scanout output here ({error})"));
                return;
            }
        };

    assert_eq!(backend.profile().name, "vkms");
    assert_ne!(
        backend.target().primary_plane_id,
        0,
        "a target with no primary plane cannot scan anything out"
    );
    assert!(
        drmkit_modeset::ModeInfo::width(&backend.target().mode) > 0,
        "a zero-width mode is not something to scan out"
    );

    let report = backend
        .present(&device, AtomicCommitFlags::empty(), None)
        .expect("present the first frame");

    assert!(report.layers_total >= 1);
    assert!(
        report.layers_assigned >= 1,
        "the full-screen layer must land on a plane, not be composited"
    );
    assert!(!report.skipped_idle, "an unconditional present never skips");
}

/// An unchanged frame is suppressed; the first one never is.
///
/// The first frame commits whatever the caller says, because nothing is on
/// screen yet -- "unchanged" describes nothing, and suppressing it would leave
/// the scanout contents undefined.
#[test]
#[ignore = "needs a DRM device and DRM master"]
fn an_unchanged_frame_is_suppressed_but_never_the_first_vkms() {
    let _guard = card_guard();
    let Some(device) = open_card() else { return };

    let mut producer = GbmScanoutProducer::new(&device);
    let mut backend =
        match ScanoutBackend::create(&device, &mut producer, &BackendConfig::default()) {
            Ok(backend) => backend,
            Err(error) => {
                drmkit_testkit::skipped(&format!("no scanout output here ({error})"));
                return;
            }
        };

    let first = backend
        .present_if_changed(&device, false, AtomicCommitFlags::empty(), None)
        .expect("the first frame commits regardless");
    assert!(!first.skipped_idle, "the first frame is never suppressed");
    assert!(first.layers_assigned >= 1);

    let idle = backend
        .present_if_changed(&device, false, AtomicCommitFlags::empty(), None)
        .expect("a suppressed frame is not a failure");
    assert!(idle.skipped_idle);
    assert_eq!(
        idle.layers_total, 0,
        "there was no commit, so there is nothing to count"
    );
    assert!(idle.placements.is_empty());

    let changed = backend
        .present_if_changed(&device, true, AtomicCommitFlags::empty(), None)
        .expect("a changed frame commits again");
    assert!(!changed.skipped_idle);
    assert!(changed.layers_assigned >= 1);

    assert_eq!(backend.frames_committed(), 2);
    assert_eq!(backend.frames_skipped(), 1);
}

/// The caller's out-fence is filled in on a CRTC that advertises
/// `OUT_FENCE_PTR`.
///
/// Asserted on the second frame, not the first: the first brings the CRTC up
/// and the fence is for the steady-state flip, which is the case a producer
/// waits on instead of blocking until the flip event.
#[test]
#[ignore = "needs a DRM device and DRM master"]
fn a_commit_delivers_its_out_fence_vkms() {
    let _guard = card_guard();
    let Some(device) = open_card() else { return };

    let mut producer = GbmScanoutProducer::new(&device);
    let mut backend =
        match ScanoutBackend::create(&device, &mut producer, &BackendConfig::default()) {
            Ok(backend) => backend,
            Err(error) => {
                drmkit_testkit::skipped(&format!("no scanout output here ({error})"));
                return;
            }
        };

    let mut first_fence = None;
    backend
        .present(&device, AtomicCommitFlags::empty(), Some(&mut first_fence))
        .expect("the modeset frame");

    let mut flip_fence = None;
    backend
        .present(&device, AtomicCommitFlags::empty(), Some(&mut flip_fence))
        .expect("the steady-state frame");

    assert!(
        flip_fence.is_some(),
        "this CRTC advertises OUT_FENCE_PTR, so the caller must get the fence"
    );
}

/// `Auto` takes variable refresh from the driver profile, and the resulting
/// commit still lands.
///
/// Arming `VRR_ENABLED` is a mode change on most drivers, so the transition
/// needs `ALLOW_MODESET` even though the mode itself is already set. A backend
/// that did not add it would commit cleanly until the first toggle and then
/// fail with `EINVAL` -- which is why each toggle here is followed by a frame.
///
/// **What this cannot prove on vkms.** Its `VRR_ENABLED` accepts the change on
/// a plain commit, so dropping `ALLOW_MODESET` passes every assertion here --
/// verified by injecting exactly that. Catching it needs a controller with
/// real variable refresh. Recorded as P-30.
#[test]
#[ignore = "needs a DRM device and DRM master"]
fn variable_refresh_arms_from_the_profile_and_toggles_cleanly_vkms() {
    let _guard = card_guard();
    let Some(device) = open_card() else { return };

    let mut producer = GbmScanoutProducer::new(&device);
    let config = BackendConfig {
        vrr: VrrPolicy::Auto,
        ..BackendConfig::default()
    };
    let mut backend = match ScanoutBackend::create(&device, &mut producer, &config) {
        Ok(backend) => backend,
        Err(error) => {
            drmkit_testkit::skipped(&format!("no scanout output here ({error})"));
            return;
        }
    };

    assert_eq!(
        backend.vrr_capable(),
        backend.profile().vrr_capable,
        "what Auto decided from must be what the caller can read back"
    );

    backend
        .present(&device, AtomicCommitFlags::empty(), None)
        .expect("present with VRR as Auto chose it");

    backend.set_vrr(false);
    backend
        .present(&device, AtomicCommitFlags::empty(), None)
        .expect("present with VRR disarmed");

    backend.set_vrr(true);
    backend
        .present(&device, AtomicCommitFlags::empty(), None)
        .expect("present with VRR re-armed");
}

/// `SavedCrtc` puts back the CRTC that was there before, on teardown.
///
/// Observed through the legacy CRTC framebuffer, because that is what the
/// restore writes: the backend's own commits are atomic and never touch that
/// field, so this drives it with legacy modesets. Establish a known
/// framebuffer, build the backend (which snapshots it), disable the CRTC, then
/// drop the backend and see the original come back.
///
/// This pins the connector: injecting an empty list into the legacy `set_crtc`
/// fails it, which is the failure that found the bug. It does **not** pin the
/// scene-before-restore ordering -- removing that drop passes here, since vkms
/// does not refuse a modeset against a framebuffer still referenced. P-30.
#[test]
#[ignore = "needs a DRM device and DRM master"]
fn a_saved_crtc_is_put_back_on_teardown_vkms() {
    use drm::control::Device as _;

    let _guard = card_guard();
    let Some(device) = open_card() else { return };

    let resources = device.resource_handles().expect("resources");
    let Some((connector, crtc, mode)) = resources.connectors().iter().find_map(|handle| {
        let connector = device.get_connector(*handle, false).ok()?;
        if connector.state() != drm::control::connector::State::Connected {
            return None;
        }
        let mode = *connector.modes().first()?;
        let crtc = *resources.crtcs().first()?;
        Some((*handle, crtc, mode))
    }) else {
        drmkit_testkit::skipped("no connected connector with a mode");
        return;
    };

    // Stand in for the prior on-screen contents, the way fbcon would.
    let buffer = drmkit_dumb::Buffer::create(
        &device,
        &drmkit_dumb::Config {
            width: u32::from(mode.size().0),
            height: u32::from(mode.size().1),
            fourcc: drmkit_fmt::fourcc::XRGB8888,
            ..drmkit_dumb::Config::default()
        },
    )
    .expect("a stand-in framebuffer");
    let original = buffer.fb_id().expect("its fb id");

    device
        .set_crtc(
            crtc,
            drm::control::framebuffer::Handle::from(
                std::num::NonZeroU32::new(original).expect("non-zero"),
            )
            .into(),
            (0, 0),
            &[connector],
            Some(mode),
        )
        .expect("establish a known CRTC configuration");

    let restored = {
        let mut producer = GbmScanoutProducer::new(&device);
        let config = BackendConfig {
            restore: RestorePolicy::SavedCrtc,
            ..BackendConfig::default()
        };
        let backend = match ScanoutBackend::create(&device, &mut producer, &config) {
            Ok(backend) => backend,
            Err(error) => {
                drmkit_testkit::skipped(&format!("no scanout output here ({error})"));
                return;
            }
        };

        // Take the CRTC down, so a backend that restored nothing leaves it down.
        device
            .set_crtc(crtc, None, (0, 0), &[], None)
            .expect("disable the CRTC");
        assert_eq!(
            device.get_crtc(crtc).expect("read it back").framebuffer(),
            None,
            "the CRTC must actually be down before teardown, or this proves nothing"
        );

        drop(backend);
        device.get_crtc(crtc).expect("read it back").framebuffer()
    };

    assert_eq!(
        restored.map(u32::from),
        Some(original),
        "teardown must put back the framebuffer that was on screen at create"
    );

    let _ = device.set_crtc(crtc, None, (0, 0), &[], None);
}

/// Wraps a producer's sources so each records the CRTC's vblank sequence at
/// the moment it is dropped -- which is when its buffer and framebuffer go.
struct SeqOnDrop<'a> {
    inner: GbmScanoutProducer<'a>,
    fd: std::os::fd::RawFd,
    crtc_id: Rc<Cell<u32>>,
    seqs: Rc<RefCell<Vec<u64>>>,
}

impl drmkit_present::ScanoutProducer for SeqOnDrop<'_> {
    fn exportable_modifiers(&mut self, fourcc: u32) -> Vec<u64> {
        self.inner.exportable_modifiers(fourcc)
    }

    fn create_buffer(
        &mut self,
        width: u32,
        height: u32,
        fourcc: u32,
        allowed: &[u64],
    ) -> Result<Box<dyn LayerBufferSource>, drmkit_present::ProducerError> {
        let inner = self.inner.create_buffer(width, height, fourcc, allowed)?;
        Ok(Box::new(SeqSource {
            inner,
            fd: self.fd,
            crtc_id: Rc::clone(&self.crtc_id),
            seqs: Rc::clone(&self.seqs),
        }))
    }
}

struct SeqSource {
    inner: Box<dyn LayerBufferSource>,
    fd: std::os::fd::RawFd,
    crtc_id: Rc<Cell<u32>>,
    seqs: Rc<RefCell<Vec<u64>>>,
}

impl Drop for SeqSource {
    fn drop(&mut self) {
        // Runs before `inner` drops, so this is the sequence the buffer and
        // its framebuffer are torn down at.
        //
        // SAFETY: the test's device outlives the backend, and so this source.
        let fd = unsafe { std::os::fd::BorrowedFd::borrow_raw(self.fd) };
        if let Ok(seq) = drmkit_core::crtc_sequence(&fd, self.crtc_id.get()) {
            self.seqs.borrow_mut().push(seq);
        }
    }
}

impl LayerBufferSource for SeqSource {
    fn acquire(&mut self) -> Result<AcquiredBuffer, SourceError> {
        self.inner.acquire()
    }

    fn release(&mut self, acquired: AcquiredBuffer) {
        self.inner.release(acquired);
    }

    fn release_with_fence(&mut self, acquired: AcquiredBuffer, fence: Option<SyncFence>) {
        self.inner.release_with_fence(acquired, fence);
    }

    fn wants_release_fence(&self) -> bool {
        self.inner.wants_release_fence()
    }

    fn has_fresh_content(&self) -> bool {
        self.inner.has_fresh_content()
    }

    fn on_retired(&mut self) {
        self.inner.on_retired();
    }

    fn binding_model(&self) -> BindingModel {
        self.inner.binding_model()
    }

    fn format(&self) -> SourceFormat {
        self.inner.format()
    }

    fn map(
        &mut self,
        access: drmkit_dumb::MapAccess,
    ) -> Result<drmkit_dumb::Mapping<'_>, SourceError> {
        self.inner.map(access)
    }

    fn export_dma_buf(&mut self) -> Result<DmaBufDesc<'_>, SourceError> {
        self.inner.export_dma_buf()
    }

    fn bind_to_plane(&mut self, plane_id: u32) -> Result<(), SourceError> {
        self.inner.bind_to_plane(plane_id)
    }

    fn unbind_from_plane(&mut self, plane_id: u32) {
        self.inner.unbind_from_plane(plane_id);
    }

    fn on_session_paused(&mut self) {
        self.inner.on_session_paused();
    }

    fn on_session_resumed(&mut self, device: &Device) -> Result<(), SourceError> {
        self.inner.on_session_resumed(device)
    }
}

/// Dropping the backend with a flip still armed waits for it to land.
///
/// Port of `LayerSceneReleaseVkms.DestructorWaitsForArmedFlip` (`debd061`),
/// on the owner of the scene: the scene itself holds no device to wait on.
/// The caller neither dispatches the event nor calls `flip_landed`. The
/// layer's source must be torn down after the armed vblank, and the event
/// must still be queued, because the wait never reads the event queue.
///
/// Upstream reads the sequence when the scene hands each buffer back. The
/// port's scene hands nothing back at teardown -- it drops its sources, and
/// their buffers with them -- so the sequence is read as the source drops.
/// Reading it after the whole drop returns instead would prove nothing: the
/// framebuffer still on screen is removed on the way out, and the kernel's
/// disable commit for that waits out a vblank of its own.
#[test]
#[ignore = "needs a DRM device and DRM master"]
fn dropping_the_backend_waits_for_the_armed_flip_vkms() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::time::{Duration, Instant};

    use drmkit_modeset::{PageFlip, Timeout};

    let _guard = card_guard();
    let Some(device) = open_card() else { return };

    let crtc_id = Rc::new(Cell::new(0));
    let seqs = Rc::new(RefCell::new(Vec::new()));
    let mut producer = SeqOnDrop {
        inner: GbmScanoutProducer::new(&device),
        fd: device.raw_fd(),
        crtc_id: Rc::clone(&crtc_id),
        seqs: Rc::clone(&seqs),
    };
    let mut backend =
        match ScanoutBackend::create(&device, &mut producer, &BackendConfig::default()) {
            Ok(backend) => backend,
            Err(error) => {
                drmkit_testkit::skipped(&format!("no scanout output here ({error})"));
                return;
            }
        };
    crtc_id.set(backend.target().crtc.id);

    let mut flip = PageFlip::new(&device).expect("page flip");
    let flips = Arc::new(AtomicU32::new(0));
    let counted = Arc::clone(&flips);
    flip.set_handler(Box::new(move |_| {
        counted.fetch_add(1, Ordering::Relaxed);
    }));

    // The modeset, with no event, then the frame whose flip is left armed.
    backend
        .present(&device, AtomicCommitFlags::empty(), None)
        .expect("modeset frame");
    backend
        .present(
            &device,
            AtomicCommitFlags::PAGE_FLIP_EVENT | AtomicCommitFlags::NONBLOCK,
            None,
        )
        .expect("armed frame");
    let armed = drmkit_core::crtc_sequence(&device, crtc_id.get()).expect("read the sequence");
    seqs.borrow_mut().clear();

    let started = Instant::now();
    drop(backend); // no dispatch, no flip_landed
    let took = started.elapsed();

    let seqs = seqs.borrow();
    assert!(!seqs.is_empty(), "teardown should drop the layer's source");
    for seq in seqs.iter() {
        assert!(
            *seq > armed,
            "a buffer was torn down before the flip landed ({seq} <= {armed})"
        );
    }
    assert!(
        took < Duration::from_millis(150),
        "the wait is bounded, took {took:?}"
    );
    flip.dispatch(Timeout::Bounded(Duration::from_millis(500)))
        .expect("dispatch");
    assert_eq!(
        flips.load(Ordering::Relaxed),
        1,
        "the wait must leave the event queued for the caller"
    );
}
