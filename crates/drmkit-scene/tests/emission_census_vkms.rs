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
use drmkit_scene::DamageRect;
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
    /// Reported on the next acquire, the way a producer that repainted part of
    /// its buffer would say so.
    next_damage: Rc<RefCell<Vec<DamageRect>>>,
    /// Handed to the next acquired buffer, the way an asynchronous producer
    /// would stamp its render fence on the frame it just submitted.
    next_fence: Rc<RefCell<Option<SyncFence>>>,
}

impl LayerBufferSource for DumbSource {
    fn acquire(&mut self) -> Result<AcquiredBuffer, SourceError> {
        let mut acquired = self.inner.acquire()?;
        *self.live.borrow_mut() += 1;
        acquired.acquire_fence = self.next_fence.borrow_mut().take();
        acquired.damage = std::mem::take(&mut *self.next_damage.borrow_mut());
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
    damage_slot: Rc<RefCell<Vec<DamageRect>>>,
    handle: drmkit_scene::LayerHandle,
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
    let damage_slot: Rc<RefCell<Vec<DamageRect>>> = Rc::new(RefCell::new(Vec::new()));
    let mut scene = LayerScene::new(crtc_id);
    let source = drmkit_scene_sources::DumbBufferSource::create(&device, 64, 64, fourcc::XRGB8888)
        .expect("dumb buffer source");
    let handle = scene.add_layer(Box::new(DumbSource {
        inner: source,
        live: Rc::clone(&live),
        next_fence: Rc::clone(&fence_slot),
        next_damage: Rc::clone(&damage_slot),
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
        damage_slot,
        handle,
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

/// Run `frames` frames, returning the reports.
fn run(
    fx: &mut Fixture,
    frames: usize,
    mut before_each: impl FnMut(&mut Fixture, usize),
) -> Vec<drmkit_scene::CommitReport> {
    (0..frames)
        .map(|i| {
            before_each(fx, i);
            let build = build_one(fx);
            fx.scene.finalize_frame(build, KernelResult::Ok)
        })
        .collect()
}

/// A widget-scale dirty region every frame.
///
/// Damage is *content*, not placement: the geometry never changes, so the
/// FB-only fast path holds after the cold frame and the whole run costs one
/// `TEST_ONLY`. A scene that treated damage as a placement change would search
/// every frame.
///
/// The clip count is driver-gated, and asserted exactly for whichever
/// capability the driver has rather than skipped: a driver with
/// `FB_DAMAGE_CLIPS` must arm one per damaged frame, and one without must arm
/// none and fall back to full-frame. Both are correct; reporting neither would
/// be the bug.
///
/// Which branch runs where, counted per plane rather than taken on trust:
/// amdgpu 6 of 10, vkms 0 of 10, vc4 0 of 56, rp1-dsi 0 of 1. So the
/// arms-a-clip branch runs on amdgpu alone of the hardware here -- the
/// reference's equivalent names vc4 as capable, which it is not
/// ([drm-cxx#255](https://github.com/jwinarske/drm-cxx/issues/255)).
#[test]
#[ignore = "needs a DRM device"]
fn a_damaged_frame_stays_on_the_fast_path_vkms() {
    const FRAMES: usize = 8;

    let _guard = card_guard();
    let Some(mut fx) = fixture() else { return };
    let handle = fx.handle;

    let reports = run(&mut fx, FRAMES, |fx, i| {
        // Moving slightly so the region stays non-empty and non-identical --
        // a degenerate clip is dropped, which would make this vacuous.
        *fx.damage_slot.borrow_mut() = vec![DamageRect {
            x: 10 + i32::try_from(i).expect("small"),
            y: 10,
            w: 64,
            h: 64,
        }];
        let _ = handle;
    });

    let test_commits: usize = reports.iter().map(|r| r.test_commits_issued).sum();
    assert_eq!(
        test_commits, 1,
        "damage is content, not placement -- one cold search and no more"
    );
    let fast = reports.iter().filter(|r| r.fb_delta_fast_path).count();
    assert_eq!(
        fast,
        FRAMES - 1,
        "every frame after the cold one keeps the FB-only fast path"
    );

    let profile = drmkit_display::DriverProfile::probe(&fx.device).expect("driver profile");
    let damaged: usize = reports.iter().map(|r| r.damaged_layers).sum();
    if profile.fb_damage_clips {
        assert_eq!(
            damaged, FRAMES,
            "a damage-capable driver must arm one clip per damaged frame"
        );
    } else {
        assert_eq!(
            damaged, 0,
            "a driver without FB_DAMAGE_CLIPS falls back to full-frame"
        );
    }
    println!(
        "note: {} damage clips over {FRAMES} frames (driver {} FB_DAMAGE_CLIPS)",
        damaged,
        if profile.fb_damage_clips {
            "has"
        } else {
            "lacks"
        }
    );
}

/// A layer translated every few frames.
///
/// Each move changes `CRTC_X`, which is placement: the fast path is defeated
/// and the frame re-validates with one `TEST_ONLY`. The frames between are
/// back on the fast path, which is what says the invalidation is per-change
/// and not sticky.
#[test]
#[ignore = "needs a DRM device"]
fn a_moving_layer_pays_one_test_commit_per_move_vkms() {
    const FRAMES: usize = 9;
    const EVERY: usize = 3;

    let _guard = card_guard();
    let Some(mut fx) = fixture() else { return };
    let handle = fx.handle;

    let reports = run(&mut fx, FRAMES, |fx, i| {
        if i % EVERY == 0 {
            let layer = fx.scene.layer_mut(handle).expect("live");
            let mut display = *layer.display();
            display.dst_rect.x = i32::try_from(i).expect("small") * 4;
            layer.set_display(display);
        }
    });

    let moves = FRAMES.div_ceil(EVERY);
    let test_commits: usize = reports.iter().map(|r| r.test_commits_issued).sum();
    assert_eq!(
        test_commits, moves,
        "one search per move and none between: {test_commits} over {FRAMES} frames"
    );

    // The frames that did not move stayed on the fast path.
    let fast = reports.iter().filter(|r| r.fb_delta_fast_path).count();
    assert_eq!(
        fast,
        FRAMES - moves,
        "a frame that moved nothing must not re-validate"
    );
}

/// Several overlapping layers, steady.
///
/// Overlap is what makes the allocator's job non-trivial -- disjoint layers
/// are placed group by group -- and the point of the case is that it still
/// settles: one cold search, then nothing.
#[test]
#[ignore = "needs a DRM device"]
fn overlapping_layers_settle_to_no_search_vkms() {
    const FRAMES: usize = 6;

    let _guard = card_guard();
    let Some(mut fx) = fixture() else { return };

    // Three more layers on top of the fixture's one, all overlapping it.
    for i in 0..3 {
        let source =
            drmkit_scene_sources::DumbBufferSource::create(&fx.device, 64, 64, fourcc::XRGB8888)
                .expect("dumb source");
        let handle = fx.scene.add_layer(Box::new(DumbSource {
            inner: source,
            live: Rc::clone(&fx.live),
            next_fence: Rc::new(RefCell::new(None)),
            next_damage: Rc::new(RefCell::new(Vec::new())),
        }));
        fx.scene
            .layer_mut(handle)
            .expect("live")
            .set_display(DisplayParams {
                src_rect: Rect {
                    x: 0,
                    y: 0,
                    w: 64,
                    h: 64,
                },
                dst_rect: Rect {
                    x: i * 8,
                    y: i * 8,
                    w: 64,
                    h: 64,
                },
                ..DisplayParams::default()
            });
    }

    let reports = run(&mut fx, FRAMES, |_, _| {});

    let total = reports.last().expect("frames").layers_total;
    assert_eq!(total, 4, "all four layers were in the frame");
    for report in &reports {
        assert!(
            report.accounting_balances(),
            "the layer tally stopped adding up: {report:?}"
        );
    }

    let test_commits: usize = reports.iter().map(|r| r.test_commits_issued).sum();
    assert_eq!(
        test_commits, 1,
        "an unchanging overlapping scene searches once: {test_commits}"
    );
}
