// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! **Invariant 4**, against a real device: what a steady frame actually writes.
//!
//! The other half of invariant 4 — that a steady frame issues no `TEST_ONLY`
//! commit — is pinned host-side in `scene_flow.rs`, where the kernel's answer
//! is a parameter. This is the half that needs a device, because it is about
//! the property ids a real card hands out and the properties it really
//! exposes.
//!
//! The census is the point. "Only the framebuffer changed" is a claim about a
//! count, and a count is exactly what a host test with a synthetic property map
//! cannot make honestly: it would be counting the tags the test itself put in.
//!
//! Every case is `#[ignore]`d and runs under the vkms lane with
//! `--include-ignored`.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Mutex;

mod common;

use drmkit_core::{AtomicCommitFlags, AtomicRequest, Device, ObjectType, PropertyStore};
use drmkit_fmt::fourcc;
use drmkit_planes::{PlaneCapabilities, PlaneRegistry, PlaneType};
use drmkit_scene::{
    AcquiredBuffer, CommitKind, DeviceCommitter, DisplayParams, KernelResult, LayerBufferSource,
    LayerScene, PlanePropertyMap, Rect, SourceError, SourceFormat, arm_acquire_fences, emit_frame,
};
use drmkit_sync::SyncFence;

/// DRM master is per open file description, so card-dependent cases serialize.
static CARD_LOCK: Mutex<()> = Mutex::new(());

fn card_guard() -> std::sync::MutexGuard<'static, ()> {
    CARD_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// A source over a real dumb buffer, so the kernel can accept the commit.
struct DumbSource {
    inner: drmkit_scene_sources::DumbBufferSource,
    live: Rc<RefCell<usize>>,
    /// Handed to the next acquired buffer, the way an asynchronous producer
    /// would stamp its render fence on the frame it just submitted.
    next_fence: Rc<RefCell<Option<SyncFence>>>,
}

impl LayerBufferSource for DumbSource {
    fn acquire(&mut self) -> Result<AcquiredBuffer, SourceError> {
        let mut acquired = self.inner.acquire()?;
        *self.live.borrow_mut() += 1;
        acquired.acquire_fence = self.next_fence.borrow_mut().take();
        Ok(acquired)
    }

    fn release(&mut self, acquired: AcquiredBuffer) {
        *self.live.borrow_mut() -= 1;
        self.inner.release(acquired);
    }

    fn format(&self) -> SourceFormat {
        self.inner.format()
    }
}

struct Fixture {
    crtc_index: u32,
    device: Device,
    next_acquire_fence: Option<SyncFence>,
    fence_slot: Rc<RefCell<Option<SyncFence>>>,
    registry: PlaneRegistry,
    map: PlanePropertyMap,
    scene: LayerScene,
    live: Rc<RefCell<usize>>,
}

/// Returns `None` only when another client holds DRM master.
///
/// Everything else asserts. A fixture that returned `None` for a missing CRTC
/// or an unreadable plane would let every case here pass having done nothing,
/// which is the failure mode this whole file exists to catch elsewhere.
/// Open the card, or `None` if this machine has no vkms.
///
/// Absence of the device is a different condition from the device being there
/// and misbehaving. Locally it means "vkms is not loaded", which is a skip; in
/// the lane it means the lane is not testing what it claims, which is a
/// failure — the same line `DRMKIT_REQUIRE_MASTER` already draws for DRM
/// master. Everything past this point still asserts.
fn open_card_or_skip() -> Option<Device> {
    let path = std::env::var("DRMKIT_TEST_CARD").unwrap_or_else(|_| "/dev/dri/card0".to_owned());
    match Device::open(&path) {
        Ok(device) => Some(device),
        Err(error) => {
            assert!(
                std::env::var_os("DRMKIT_REQUIRE_MASTER").is_none(),
                "{path}: {error}, but DRMKIT_REQUIRE_MASTER is set"
            );
            println!("note: skipped -- no DRM device at {path} ({error})");
            None
        }
    }
}

fn fixture() -> Option<Fixture> {
    use drm::control::Device as _;

    let device = open_card_or_skip()?;
    device.enable_universal_planes().expect("universal planes");
    device.enable_atomic().expect("atomic");

    if device.set_master().is_err() {
        assert!(
            std::env::var_os("DRMKIT_REQUIRE_MASTER").is_none(),
            "not DRM master, but DRMKIT_REQUIRE_MASTER is set"
        );
        println!("note: skipped -- another client holds DRM master");
        return None;
    }

    let resources = device.resource_handles().expect("resources");
    let crtcs = resources.crtcs();
    let (crtc_id, crtc_index) = common::pick_crtc(&device, &resources);

    let mut capabilities = Vec::new();
    for handle in device.plane_handles().expect("planes") {
        let info = device.get_plane(handle).expect("plane info");
        let plane_id: u32 = handle.into();
        let mut mask = 0u32;
        for allowed in resources.filter_crtcs(info.possible_crtcs()) {
            if let Some(index) = crtcs.iter().position(|c| *c == allowed)
                && index < u32::BITS as usize
            {
                mask |= 1 << index;
            }
        }
        let mut store = PropertyStore::new();
        store
            .cache_properties(&device, plane_id, ObjectType::Plane)
            .expect("plane properties");
        let plane_type = match store.property_value(plane_id, "type") {
            Ok(1) => PlaneType::Primary,
            Ok(2) => PlaneType::Cursor,
            _ => PlaneType::Overlay,
        };
        capabilities.push(PlaneCapabilities {
            id: plane_id,
            possible_crtcs: mask,
            plane_type,
            formats: vec![fourcc::XRGB8888],
            supports_scaling: true,
            ..PlaneCapabilities::default()
        });
    }
    let registry = PlaneRegistry::from_capabilities(capabilities);

    let mut map = PlanePropertyMap::new();
    map.learn_all(&device, &registry).expect("learn planes");

    let live = Rc::new(RefCell::new(0));
    let fence_slot = Rc::new(RefCell::new(None));
    let mut scene = LayerScene::new(crtc_id);
    let source = drmkit_scene_sources::DumbBufferSource::create(&device, 64, 64, fourcc::XRGB8888)
        .expect("dumb buffer source");
    let handle = scene.add_layer(Box::new(DumbSource {
        inner: source,
        live: Rc::clone(&live),
        next_fence: Rc::clone(&fence_slot),
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

    Some(Fixture {
        crtc_index,
        device,
        next_acquire_fence: None,
        fence_slot,
        registry,
        map,
        scene,
        live,
    })
}

/// Build one frame and return the request it would commit.
///
/// The kernel's answer is supplied rather than issued: the census is about what
/// the request contains, and a case that also applied it would be testing the
/// device's tolerance rather than the emission.
/// Build one frame, handing the source whatever fence the case staged.
fn build_one(fx: &mut Fixture) -> drmkit_scene::FrameBuild {
    *fx.fence_slot.borrow_mut() = fx.next_acquire_fence.take();
    let mut committer = DeviceCommitter::new(
        &fx.device,
        &fx.map,
        &fx.registry,
        fx.crtc_index,
        AtomicCommitFlags::empty(),
        None,
    );
    fx.scene
        .build_frame(
            &fx.registry,
            fx.crtc_index,
            CommitKind::Real { arms_flip: false },
            &mut committer,
        )
        .expect("build")
}

fn frame(fx: &mut Fixture) -> AtomicRequest {
    let mut committer = DeviceCommitter::new(
        &fx.device,
        &fx.map,
        &fx.registry,
        fx.crtc_index,
        AtomicCommitFlags::empty(),
        None,
    );
    let build = fx
        .scene
        .build_frame(
            &fx.registry,
            fx.crtc_index,
            CommitKind::Real { arms_flip: false },
            &mut committer,
        )
        .expect("build");

    let mut request = AtomicRequest::with_capacity(64);
    emit_frame(&mut request, &fx.map, build.plan(), build.disables(), None).expect("emit");

    for entry in build.plan() {
        fx.map.note_color_committed(entry.plane_id);
    }
    fx.scene.finalize_frame(build, KernelResult::Ok);
    request
}

/// A steady frame writes one property per plane: the framebuffer.
///
/// **Invariant 4.** Everything else the kernel already has, and restating it
/// costs a property write the kernel must walk on every plane on every frame.
#[test]
#[ignore = "needs a DRM device"]
fn a_steady_frame_writes_only_the_framebuffer_vkms() {
    let _guard = card_guard();
    let Some(mut fx) = fixture() else { return };

    let first = frame(&mut fx);
    assert!(
        first.writes().len() > 5,
        "the first frame configures the plane from scratch and must write more \
         than a framebuffer; it wrote {}",
        first.writes().len()
    );

    let steady = frame(&mut fx);

    let mut store = PropertyStore::new();
    let planes: Vec<u32> = steady.writes().iter().map(|w| w.object_id).collect();
    for plane_id in &planes {
        store
            .cache_properties(&fx.device, *plane_id, ObjectType::Plane)
            .expect("plane properties");
    }
    let names: Vec<String> = steady
        .writes()
        .iter()
        .map(|write| {
            store
                .properties(write.object_id)
                .and_then(|props| props.iter().find(|p| p.id == write.property_id))
                .map_or_else(|| "unknown".to_owned(), |p| p.name.clone())
        })
        .collect();

    assert!(
        names.iter().all(|name| name == "FB_ID"),
        "a steady frame must write nothing but FB_ID; it wrote {names:?}"
    );
    assert!(
        !names.is_empty(),
        "a steady frame must still re-attach FB_ID"
    );

    fx.scene.drain();
    assert_eq!(*fx.live.borrow(), 0, "every buffer must come back");
}

/// Colorimetry is written once and then left alone.
///
/// `COLOR_ENCODING` and `COLOR_RANGE` are sticky across clients, so they have
/// to be written once to displace whatever the previous compositor left. They
/// must not be written again: an unchanged value restated every frame is the
/// per-frame traffic the rest of the emit path works to avoid.
#[test]
#[ignore = "needs a DRM device"]
fn colorimetry_is_written_once_not_every_frame_vkms() {
    let _guard = card_guard();
    let Some(mut fx) = fixture() else { return };

    let first = frame(&mut fx);
    let mut store = PropertyStore::new();
    for write in first.writes() {
        if store.properties(write.object_id).is_none() {
            store
                .cache_properties(&fx.device, write.object_id, ObjectType::Plane)
                .expect("plane properties");
        }
    }
    let name_of = |store: &PropertyStore, object_id: u32, property_id: u32| {
        store
            .properties(object_id)
            .and_then(|props| props.iter().find(|p| p.id == property_id))
            .map_or_else(|| "unknown".to_owned(), |p| p.name.clone())
    };
    let color_writes = |request: &AtomicRequest, store: &PropertyStore| {
        request
            .writes()
            .iter()
            .filter(|w| {
                let name = name_of(store, w.object_id, w.property_id);
                name == "COLOR_ENCODING" || name == "COLOR_RANGE"
            })
            .count()
    };

    let first_color = color_writes(&first, &store);
    if first_color == 0 {
        // vkms grew these properties over time; on a kernel without them there
        // is nothing to assert, and saying so beats passing silently.
        eprintln!("skipped: no plane on this card exposes COLOR_ENCODING/COLOR_RANGE");
        fx.scene.drain();
        return;
    }

    let steady = frame(&mut fx);
    assert_eq!(
        color_writes(&steady, &store),
        0,
        "colorimetry is sticky and was already committed; restating it every \
         frame is {first_color} wasted writes per frame"
    );

    fx.scene.drain();
}

/// A fenced buffer arrives at its plane with its fence, and the report says so.
///
/// The fence here is a stand-in descriptor, not a `sync_file` the kernel would
/// accept, and this case deliberately never commits the request it builds. The
/// claim under test is what the scene *constructs* — that a buffer carrying an
/// acquire fence causes `IN_FENCE_FD` to be written on the plane that buffer
/// landed on, and that the commit report says so. Whether the kernel then
/// honours a valid fence is the kernel's business, and a real one is awkward to
/// come by: `CONFIG_SW_SYNC` is a debug option and off on most kernels, leaving
/// only an `OUT_FENCE` round-trip.
///
/// It still needs a device. The property id comes from a real plane, and
/// whether a plane exposes `IN_FENCE_FD` at all is the branch the whole arming
/// pass turns on.
///
/// `in_fences_armed` and `in_fence_cpu_waits` sat on `CommitReport` for several
/// commits with nothing setting either, so the report advertised fence
/// accounting and always answered "no source produced a fence". Asserting the
/// counters, not just the property write, is what keeps that from recurring.
#[test]
#[ignore = "needs a DRM device"]
fn a_fenced_buffer_arms_its_plane_vkms() {
    let _guard = card_guard();
    let Some(mut fx) = fixture() else { return };

    // Any descriptor will do: `SyncFence::import` dups and does not validate,
    // and nothing commits this request.
    let stand_in =
        rustix::event::eventfd(0, rustix::event::EventfdFlags::CLOEXEC).expect("eventfd");
    fx.next_acquire_fence = Some(SyncFence::from_owned(stand_in));

    let mut build = build_one(&mut fx);
    let mut request = AtomicRequest::with_capacity(64);
    emit_frame(&mut request, &fx.map, build.plan(), build.disables(), None).expect("emit");
    arm_acquire_fences(&mut build, &mut request, &fx.map, true).expect("arm");

    let mut store = PropertyStore::new();
    for write in request.writes() {
        if store.properties(write.object_id).is_none() {
            store
                .cache_properties(&fx.device, write.object_id, ObjectType::Plane)
                .expect("plane properties");
        }
    }
    let fence_writes = request
        .writes()
        .iter()
        .filter(|write| {
            store
                .properties(write.object_id)
                .and_then(|props| props.iter().find(|p| p.id == write.property_id))
                .is_some_and(|p| p.name == "IN_FENCE_FD")
        })
        .count();

    let report = build.report().clone();
    for entry in build.plan() {
        fx.map.note_color_committed(entry.plane_id);
    }
    fx.scene.finalize_frame(build, KernelResult::Ok);

    assert_eq!(
        fence_writes, 1,
        "the fenced layer's plane must be handed IN_FENCE_FD"
    );
    assert_eq!(
        report.in_fences_armed, 1,
        "and the report must say so; the counter was dead for several commits"
    );
    assert_eq!(
        report.in_fence_cpu_waits, 0,
        "every vkms plane advertises IN_FENCE_FD, so nothing should CPU-wait"
    );

    fx.scene.drain();
}
