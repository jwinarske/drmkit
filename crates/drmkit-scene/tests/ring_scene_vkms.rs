// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! A rotating DMA-BUF ring driven through a scene, against a real device.
//!
//! Parity port of `tests/integration/test_external_dma_buf_ring_scene_vkms.cpp`
//! from drm-cxx @ `4a0b64a`. Every case here is `#[ignore]`d and runs under the
//! vkms lane with `--include-ignored`.
//!
//! What these add over the ring's own device cases is the **scene**: the ring
//! is a layer source, the commits are real, and the release edge is driven by
//! the kernel rather than by a test calling `release`. A ring can be correct on
//! its own and still be wired to a scene that never retires its buffers.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use drm::control::Device as ControlDevice;
use drmkit_core::{AtomicCommitFlags, Device};
use drmkit_fmt::fourcc;
use drmkit_planes::PlaneRegistry;
use drmkit_scene::{
    AcquiredBuffer, BindingModel, CommitKind, CommitReport, DeviceCommitter, DisplayParams,
    KernelResult, LayerBufferSource, LayerScene, Modeset, PlanePropertyMap, Rect, SourceError,
    SourceFormat, arm_acquire_fences, emit_frame,
};
use drmkit_scene_sources::{ExternalDmaBufRing, ExternalPlane};
use drmkit_sync::SyncFence;

mod common;

/// DRM master is per open file description, so these serialize.
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
    device.enable_universal_planes().expect("universal planes");
    device.enable_atomic().expect("atomic");
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

/// A dumb buffer exported as a dma-buf: a real importable descriptor, which is
/// what separates this from a fixture.
fn exported_buffer(
    device: &Device,
    width: u32,
    height: u32,
) -> Option<(std::os::fd::OwnedFd, u32)> {
    let buffer = drmkit_dumb::Buffer::create(
        device,
        &drmkit_dumb::Config {
            width,
            height,
            fourcc: fourcc::XRGB8888,
            // No framebuffer of its own: the ring registers one over the
            // imported descriptor, and a second registration on the same GEM
            // object would be a framebuffer nothing ever commits.
            add_fb: false,
            ..drmkit_dumb::Config::default()
        },
    )
    .ok()?;
    let handle = drm::control::from_u32(buffer.gem_handle()?)?;
    let stride = buffer.stride();
    let fd = device.buffer_to_prime_fd(handle, 0).ok()?;
    Some((fd, stride))
}

/// The ring, shared between the scene that owns the layer and the case that
/// submits into it.
///
/// The scene takes its sources by value for `'static`, and there is no way to
/// reach back in and get this one out again -- so it is shared, the same
/// arrangement `DumbScanoutSink` uses for its dumb ring. `RefCell` because the
/// commit path is single-threaded by contract; the release callback is the only
/// thing that crosses a thread, and it is `Send` on its own.
#[derive(Clone)]
struct SharedRing(Rc<RefCell<ExternalDmaBufRing>>);

impl LayerBufferSource for SharedRing {
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
}

/// The CRTC's `OUT_FENCE_PTR` property id, if it has one.
fn crtc_out_fence_property(device: &Device, crtc_id: u32) -> Option<u32> {
    let mut store = drmkit_core::PropertyStore::new();
    store
        .cache_properties(device, crtc_id, drmkit_core::ObjectType::Crtc)
        .ok()?;
    store.property_id(crtc_id, "OUT_FENCE_PTR").ok()
}

/// Everything a case needs to commit frames for real.
struct Harness {
    device: Device,
    registry: PlaneRegistry,
    map: PlanePropertyMap,
    scene: LayerScene,
    crtc_id: u32,
    crtc_index: u32,
    connector_id: u32,
    mode: drmkit_core::Mode,
    ring: Rc<RefCell<ExternalDmaBufRing>>,
    needs_modeset: bool,
    /// Kept alive: the ring's imports reference these descriptors.
    _fds: Vec<std::os::fd::OwnedFd>,
}

const WIDTH: u32 = 64;
const HEIGHT: u32 = 64;
const SLOTS: usize = 3;

/// Build a scene whose one layer is a ring, bound to a connected output.
///
/// `on_release` is installed before the ring reaches the scene, because
/// `wants_release_fence` is read from it: a listener attached afterwards would
/// arrive too late for the scene to have asked the kernel for an out-fence.
fn harness(device: Device, on_release: Option<drmkit_scene_sources::OnRelease>) -> Option<Harness> {
    let resources = device.resource_handles().ok()?;

    // A connected connector, its CRTC, and a mode -- a real commit needs all
    // three, unlike the build-only cases in `release_invariants_vkms`.
    let (connector_id, crtc, mode) = resources.connectors().iter().find_map(|handle| {
        let connector = device.get_connector(*handle, false).ok()?;
        if connector.state() != drm::control::connector::State::Connected {
            return None;
        }
        let mode = *connector.modes().first()?;
        let encoder = connector
            .encoders()
            .iter()
            .find_map(|e| device.get_encoder(*e).ok())?;
        let crtc = resources
            .filter_crtcs(encoder.possible_crtcs())
            .first()
            .copied()?;
        Some((u32::from(*handle), crtc, mode))
    })?;
    let crtc_index = u32::try_from(resources.crtcs().iter().position(|c| *c == crtc)?).ok()?;
    let crtc_id = u32::from(crtc);

    let registry = PlaneRegistry::probe(&device).ok()?;
    let mut map = PlanePropertyMap::new();
    for plane in registry.for_crtc(crtc_index) {
        map.learn_plane(&device, plane.id).ok()?;
    }

    let mut fds = Vec::new();
    let mut pitches = Vec::new();
    for _ in 0..SLOTS {
        let (fd, pitch) = exported_buffer(&device, WIDTH, HEIGHT)?;
        fds.push(fd);
        pitches.push(pitch);
    }
    let planes: Vec<Vec<ExternalPlane<'_>>> = fds
        .iter()
        .zip(&pitches)
        .map(|(fd, pitch)| {
            vec![ExternalPlane {
                fd: std::os::fd::AsFd::as_fd(fd),
                offset: 0,
                pitch: *pitch,
            }]
        })
        .collect();
    let slots: Vec<&[ExternalPlane<'_>]> = planes.iter().map(Vec::as_slice).collect();

    let mut ring = ExternalDmaBufRing::create(
        &device,
        SourceFormat {
            fourcc: fourcc::XRGB8888,
            modifier: 0,
            width: WIDTH,
            height: HEIGHT,
        },
        &slots,
        None,
    )
    .ok()?;
    if let Some(callback) = on_release {
        ring.set_on_release(callback);
    }

    let ring = Rc::new(RefCell::new(ring));
    let mut scene = LayerScene::new(crtc_id);
    let layer = scene.add_layer(Box::new(SharedRing(Rc::clone(&ring))));
    // The handle is used only to place the layer; later cases that add a
    // second one keep theirs.
    scene.layer_mut(layer)?.set_display(DisplayParams {
        src_rect: Rect {
            x: 0,
            y: 0,
            w: WIDTH,
            h: HEIGHT,
        },
        dst_rect: Rect {
            x: 0,
            y: 0,
            w: WIDTH,
            h: HEIGHT,
        },
        ..DisplayParams::default()
    });

    Some(Harness {
        device,
        registry,
        map,
        scene,
        crtc_id,
        crtc_index,
        connector_id,
        mode,
        ring,
        needs_modeset: true,
        _fds: fds,
    })
}

impl Harness {
    /// Build and issue one real commit.
    fn commit(&mut self) -> Result<CommitReport, String> {
        let modeset = if self.needs_modeset {
            Some(
                Modeset::learn(&self.device, self.crtc_id, self.connector_id, &self.mode)
                    .map_err(|error| format!("learning the mode: {error}"))?,
            )
        } else {
            None
        };
        let mut flags = AtomicCommitFlags::empty();
        if modeset.is_some() {
            flags |= AtomicCommitFlags::ALLOW_MODESET;
        }

        let mut committer = DeviceCommitter::new(
            &self.device,
            &self.map,
            &self.registry,
            self.crtc_index,
            AtomicCommitFlags::empty(),
            modeset.as_ref(),
        );
        let mut build = self
            .scene
            .build_frame(
                &self.registry,
                self.crtc_index,
                CommitKind::Real { arms_flip: false },
                &mut committer,
            )
            .map_err(|error| format!("building: {error}"))?;

        // Arm OUT_FENCE_PTR only when a source is listening, which is the
        // question `wants_release_fence` exists to answer: with nobody
        // listening the fence is a descriptor per commit that nothing closes.
        let out_fence_property = self
            .scene
            .wants_release_fence()
            .then(|| crtc_out_fence_property(&self.device, self.crtc_id))
            .flatten();

        let plan = build.plan().to_vec();
        let disables = build.disables().to_vec();
        let (map, crtc_id) = (&self.map, self.crtc_id);
        let modeset_ref = modeset.as_ref();

        let outcome = drmkit_core::commit_with_out_fence(&self.device, flags, |request, slot| {
            emit_frame(request, map, &plan, &disables, modeset_ref)?;
            arm_acquire_fences(&mut build, request, map, true)?;
            if let Some(property) = out_fence_property {
                request.add_property(crtc_id, property, slot)?;
            }
            Ok(())
        });

        match outcome {
            Ok(fence) => {
                self.needs_modeset = false;
                let fence =
                    fence.and_then(|fd| SyncFence::import(std::os::fd::AsFd::as_fd(&fd)).ok());
                Ok(self
                    .scene
                    .finalize_frame_with_fence(build, KernelResult::Ok, fence.as_ref()))
            }
            Err(error) => {
                self.scene.finalize_frame(build, KernelResult::Rejected);
                Err(format!("committing: {error}"))
            }
        }
    }

    /// Submit into `slot` through the shared ring.
    fn submit(&self, slot: usize) {
        self.ring.borrow().submit(slot, None, &[]);
    }

    /// Put the CRTC back down.
    fn teardown(&mut self) {
        let _ = self.device.set_crtc(
            drm::control::crtc::Handle::from(
                std::num::NonZeroU32::new(self.crtc_id).expect("non-zero"),
            ),
            None,
            (0, 0),
            &[],
            None,
        );
    }
}

/// Buffers retire as the ring rotates, and a release carries the displacing
/// commit's `OUT_FENCE`.
///
/// The fence is the point. A source that wants it gets the fence of the commit
/// that took its buffer *off* screen, so a GPU producer can wait on that
/// GPU-side rather than blocking the CPU until a later release edge. The
/// mechanism is pinned host-side with the kernel's answer as a parameter; what
/// this adds is that the kernel really returns one and it really reaches the
/// source.
#[test]
#[ignore = "needs a DRM device and DRM master"]
fn a_displaced_buffer_is_released_carrying_the_commits_out_fence_vkms() {
    let _guard = card_guard();
    let Some(device) = open_card() else { return };

    let releases = Arc::new(AtomicU32::new(0));
    let fenced = Arc::new(AtomicU32::new(0));
    let (r, f) = (Arc::clone(&releases), Arc::clone(&fenced));

    let Some(mut h) = harness(
        device,
        Some(Box::new(move |_slot, fence| {
            r.fetch_add(1, Ordering::Relaxed);
            if fence.is_some_and(|fence| fence.as_fd().is_some()) {
                f.fetch_add(1, Ordering::Relaxed);
            }
        })),
    ) else {
        drmkit_testkit::skipped("no connected output with an importable ring");
        return;
    };

    // Enough frames for the deferred-release ring to work through its
    // generations and start retiring.
    for frame in 0..8 {
        h.submit(frame % SLOTS);
        h.commit()
            .unwrap_or_else(|error| panic!("frame {frame}: {error}"));
    }

    assert!(
        releases.load(Ordering::Relaxed) > 0,
        "eight frames through a {SLOTS}-slot ring must retire something, or \
         the producer never gets a buffer back"
    );
    assert!(
        fenced.load(Ordering::Relaxed) > 0,
        "at least one release must carry the displacing commit's OUT_FENCE, \
         which is the whole reason a source opts into wanting it"
    );

    h.teardown();
}
