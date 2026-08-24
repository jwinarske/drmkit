// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! The composition fallback, end to end against a device.
//!
//! When a scene has more layers than the CRTC has usable planes, the ones the
//! allocator cannot place are blended into a canvas and that canvas goes on a
//! plane of its own. Without the fallback they simply do not reach the screen.
//!
//! What needs a device here is the plane arithmetic: how many planes the CRTC
//! really has, which of them the allocator claims, and whether one is left for
//! the canvas. The blend itself is pinned host-side against drm-cxx's own
//! output in `canvas_oracle.rs`.

use std::sync::Mutex;

use drmkit_core::{AtomicCommitFlags, AtomicRequest, Device, ObjectType, PropertyStore};
use drmkit_fmt::fourcc;
use drmkit_planes::{PlaneCapabilities, PlaneRegistry, PlaneType};
use drmkit_scene::{
    AcquiredBuffer, CommitKind, DeviceCommitter, DisplayParams, KernelResult, LayerBufferSource,
    LayerScene, PlanePropertyMap, Rect, SourceError, SourceFormat, emit_frame,
};

static CARD_LOCK: Mutex<()> = Mutex::new(());

mod common;

fn card_guard() -> std::sync::MutexGuard<'static, ()> {
    CARD_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

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

/// A dumb-buffer source that can be CPU-mapped, so composition can read it.
struct MappableSource {
    inner: drmkit_scene_sources::DumbBufferSource,
}

impl LayerBufferSource for MappableSource {
    fn acquire(&mut self) -> Result<AcquiredBuffer, SourceError> {
        self.inner.acquire()
    }

    fn release(&mut self, acquired: AcquiredBuffer) {
        self.inner.release(acquired);
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
}

/// A source whose pixels the CPU cannot read.
///
/// Stands in for a V4L2 capture buffer or GPU-only surface: the allocator can
/// place it, but if it cannot, composition has no way to rescue it.
struct OpaqueSource {
    inner: drmkit_scene_sources::DumbBufferSource,
}

impl LayerBufferSource for OpaqueSource {
    fn acquire(&mut self) -> Result<AcquiredBuffer, SourceError> {
        self.inner.acquire()
    }

    fn release(&mut self, acquired: AcquiredBuffer) {
        self.inner.release(acquired);
    }

    fn format(&self) -> SourceFormat {
        self.inner.format()
    }
}

struct Fixture {
    crtc_index: u32,
    device: Device,
    registry: PlaneRegistry,
    map: PlanePropertyMap,
    scene: LayerScene,
    planes: usize,
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
            formats: vec![fourcc::ARGB8888],
            supports_scaling: true,
            ..PlaneCapabilities::default()
        });
    }
    let registry = PlaneRegistry::from_capabilities(capabilities);
    // However many the device offers. vkms exposes a different number
    // depending on kernel version and module parameters -- ten here, fewer in
    // the lane -- so every case below is written against this count rather
    // than a number that happened to be true on one machine.
    let planes = registry.force_disable_candidates(crtc_index).count();
    assert!(
        planes > 0,
        "a CRTC with no usable plane cannot display anything"
    );
    println!("note: {planes} candidate plane(s) on this CRTC");

    let mut map = PlanePropertyMap::new();
    map.learn_all(&device, &registry).expect("learn planes");

    let mut scene = LayerScene::new(crtc_id);
    scene
        .enable_composition(&device, 128, 64)
        .expect("allocate the canvas");

    Some(Fixture {
        crtc_index,
        device,
        registry,
        map,
        scene,
        planes,
    })
}

/// Add `count` mappable layers, each a distinct colour block.
fn fill(fx: &mut Fixture, count: usize) {
    for i in 0..count {
        let source =
            drmkit_scene_sources::DumbBufferSource::create(&fx.device, 32, 32, fourcc::ARGB8888)
                .expect("dumb source");
        let handle = fx
            .scene
            .add_layer(Box::new(MappableSource { inner: source }));
        fx.scene
            .layer_mut(handle)
            .expect("layer")
            .set_display(DisplayParams {
                dst_rect: Rect {
                    x: 0,
                    y: 0,
                    w: 32,
                    h: 32,
                },
                zpos: Some(i as u64),
                ..DisplayParams::default()
            });
    }
}

fn build(fx: &mut Fixture) -> drmkit_scene::FrameBuild {
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

/// With a plane for every layer, nothing composites.
///
/// The negative case matters as much as the positive one: a composition path
/// that engages when it is not needed spends a full-screen CPU blend every
/// frame to display what the hardware would have scanned out for free.
#[test]
#[ignore = "needs a DRM device"]
fn a_scene_that_fits_composites_nothing_vkms() {
    let _guard = card_guard();
    let Some(mut fx) = fixture() else { return };

    // Exactly as many layers as there are planes: the search can place all of
    // them, so no reservation is taken and no canvas is armed.
    let planes = fx.planes;
    fill(&mut fx, planes);
    let build = build(&mut fx);
    let report = build.report().clone();
    let planned = build.plan().len();
    fx.scene.finalize_frame(build, KernelResult::Ok);

    assert_eq!(report.layers_composited, 0, "nothing needed rescuing");
    assert_eq!(report.layers_assigned, planes);
    assert_eq!(planned, planes, "one plane per layer, no canvas");
    assert!(report.accounting_balances());

    fx.scene.drain();
}

/// More layers than planes: the overflow reaches the screen via the canvas.
///
/// It is also the accounting that matters. A rescued layer *did* reach
/// hardware, so reporting it unassigned would tell a compositor a frame was
/// dropped when it was not — and the identity has to keep holding either way.
#[test]
#[ignore = "needs a DRM device"]
fn layers_beyond_the_plane_count_are_composited_vkms() {
    let _guard = card_guard();
    let Some(mut fx) = fixture() else { return };

    // Two more layers than there are planes to put them on.
    let planes = fx.planes;
    let layers = planes + 2;
    fill(&mut fx, layers);

    let build = build(&mut fx);
    let report = build.report().clone();
    let planes_used: Vec<u32> = build.plan().iter().map(|entry| entry.plane_id).collect();
    fx.scene.finalize_frame(build, KernelResult::Ok);

    assert!(
        report.layers_composited > 0,
        "{layers} layers on {} planes must overflow into the canvas; report was {report:?}",
        fx.planes
    );
    assert_eq!(
        report.layers_unassigned, 0,
        "every overflowing layer was rescued, so none is unassigned"
    );
    assert!(
        report.accounting_balances(),
        "total {} != assigned {} + composited {} + unassigned {} + skipped {}",
        report.layers_total,
        report.layers_assigned,
        report.layers_composited,
        report.layers_unassigned,
        report.layers_skipped_no_frame
    );

    let unique: std::collections::HashSet<u32> = planes_used.iter().copied().collect();
    assert_eq!(
        unique.len(),
        planes_used.len(),
        "the canvas took a plane something else was already using: {planes_used:?}"
    );

    fx.scene.drain();
}

/// The canvas plane is not disabled by the same frame that arms it.
///
/// `emit_frame` clears every candidate plane the frame did not use. The canvas
/// plane *is* used — just not by a layer — so it has to come out of that pass,
/// or the request both clears it and writes to it.
///
/// This has to run two frames. On the first the canvas plane has no committed
/// baseline, so the already-off check keeps it out of the disable list for a
/// completely different reason and the case passes without exercising
/// anything. It is only once the plane has been committed with a framebuffer
/// that leaving it in the pass is visible — which is exactly the frame a real
/// compositor spends all its time in.
#[test]
#[ignore = "needs a DRM device"]
fn the_canvas_plane_is_not_also_disabled_vkms() {
    let _guard = card_guard();
    let Some(mut fx) = fixture() else { return };

    let planes = fx.planes;
    fill(&mut fx, planes + 2);

    // Frame one: arms the canvas, and gives its plane a baseline.
    let first = build(&mut fx);
    let canvas_plane = *first
        .plan()
        .iter()
        .map(|entry| entry.plane_id)
        .collect::<Vec<_>>()
        .last()
        .expect("something was planned");
    fx.scene.finalize_frame(first, KernelResult::Ok);

    // Frame two: the plane is now known-on, so nothing but the filter keeps it
    // out of the disable pass.
    let second = build(&mut fx);
    let armed: Vec<u32> = second.plan().iter().map(|entry| entry.plane_id).collect();
    let disabled: Vec<u32> = second.disables().to_vec();
    let composited = second.report().layers_composited;
    fx.scene.finalize_frame(second, KernelResult::Ok);

    assert!(composited > 0, "the second frame must still be compositing");
    assert!(
        armed.contains(&canvas_plane),
        "the canvas moved planes between frames; this case is no longer testing what it says"
    );
    for plane in &armed {
        assert!(
            !disabled.contains(plane),
            "plane {plane} is both armed and disabled in the same request"
        );
    }

    fx.scene.drain();
}

/// A source the CPU cannot read stays dropped; one that can is rescued.
///
/// Composition reads a layer's pixels to blend them. A layer that cannot be
/// mapped has no route in, so it stays off the screen — and must be reported
/// unassigned, not composited, because `composited` means "reached hardware".
///
/// The counts are arranged so both outcomes are certain. With `planes + 2` of
/// each kind and only `planes` placeable, at least two mappable and at least
/// two unmappable layers are dropped whatever order the search picks. An
/// earlier version added only two unmappable layers, and those happened to be
/// the only two dropped — so no canvas was armed, and the case proved nothing
/// about the split it was named for.
#[test]
#[ignore = "needs a DRM device"]
fn an_unmappable_source_is_not_rescued_vkms() {
    let _guard = card_guard();
    let Some(mut fx) = fixture() else { return };

    let planes = fx.planes;
    fill(&mut fx, planes + 2);
    for _ in 0..planes + 2 {
        let source =
            drmkit_scene_sources::DumbBufferSource::create(&fx.device, 32, 32, fourcc::ARGB8888)
                .expect("dumb source");
        let handle = fx.scene.add_layer(Box::new(OpaqueSource { inner: source }));
        fx.scene
            .layer_mut(handle)
            .expect("layer")
            .set_display(DisplayParams {
                dst_rect: Rect {
                    x: 0,
                    y: 0,
                    w: 32,
                    h: 32,
                },
                zpos: Some(64),
                ..DisplayParams::default()
            });
    }

    let build = build(&mut fx);
    let report = build.report().clone();
    fx.scene.finalize_frame(build, KernelResult::Ok);

    assert!(
        report.accounting_balances(),
        "the identity has to hold when a rescue fails too: {report:?}"
    );
    // The identity alone cannot tell a rescued layer from a lost one — both
    // keep the total right. Only the split does.
    assert!(
        report.layers_composited >= 2,
        "at least two mappable layers were dropped and must have been \
         rescued: {report:?}"
    );
    assert!(
        report.layers_unassigned >= 2,
        "at least two unmappable layers were dropped and cannot have been \
         rescued; counting them composited would report them on screen: \
         {report:?}"
    );

    fx.scene.drain();
}

/// A composed frame really commits.
///
/// Everything above inspects what the scene built. This one hands it to the
/// kernel, because the canvas's properties go into the same atomic request as
/// every placed layer — so if the canvas plane cannot take it, the whole frame
/// is rejected, not just the composited part.
#[test]
#[ignore = "needs a DRM device"]
fn a_composed_frame_is_accepted_by_the_kernel_vkms() {
    let _guard = card_guard();
    let Some(mut fx) = fixture() else { return };

    let planes = fx.planes;
    fill(&mut fx, planes + 2);
    let build = build(&mut fx);
    let mut request = AtomicRequest::with_capacity(96);
    emit_frame(&mut request, &fx.map, build.plan(), build.disables(), None).expect("emit");

    let verdict = request.test(&fx.device, AtomicCommitFlags::empty());
    let composited = build.report().layers_composited;
    fx.scene.finalize_frame(build, KernelResult::Ok);

    assert!(
        verdict.is_ok(),
        "the kernel refused a frame carrying {composited} composited layers: {verdict:?}"
    );

    fx.scene.drain();
}
