// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! The renderer against a real device.
//!
//! The pieces are tested on their own in the crate; what needs hardware is
//! that they fit together — a plane is chosen, a size the kernel accepts is
//! settled on, buffers are allocated with framebuffers registered, and a frame
//! reaches them.

use drm::control::Device as _;
use drmkit_core::{AtomicRequest, Device};
use drmkit_cursor::{Cursor, PlanePath, Renderer, RendererConfig, Rotation};
use drmkit_planes::{PlaneCapabilities, PlaneRegistry, PlaneType};

static CARD_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn card_guard() -> std::sync::MutexGuard<'static, ()> {
    CARD_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// A device holding DRM master, plus a registry built from its real planes.
fn fixture() -> Option<(Device, PlaneRegistry, u32, u32)> {
    let path = std::env::var("DRMKIT_TEST_CARD").unwrap_or_else(|_| "/dev/dri/card0".to_owned());
    let Ok(device) = Device::open(&path) else {
        println!("note: skipped -- {path} did not open");
        return None;
    };
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
    // Prefer a CRTC something is connected to; the first is often not it.
    let index = resources
        .connectors()
        .iter()
        .find_map(|handle| {
            let connector = device.get_connector(*handle, false).ok()?;
            if connector.state() != drm::control::connector::State::Connected {
                return None;
            }
            let encoder = device.get_encoder(connector.current_encoder()?).ok()?;
            let crtc = encoder.crtc()?;
            crtcs.iter().position(|candidate| *candidate == crtc)
        })
        .unwrap_or(0);
    let crtc_id: u32 = (*crtcs.get(index)?).into();

    let mut capabilities = Vec::new();
    for handle in device.plane_handles().expect("planes") {
        let info = device.get_plane(handle).expect("plane info");
        let mut mask = 0u32;
        for allowed in resources.filter_crtcs(info.possible_crtcs()) {
            if let Some(bit) = crtcs.iter().position(|c| *c == allowed) {
                mask |= 1 << bit;
            }
        }
        let mut store = drmkit_core::PropertyStore::new();
        let plane_id: u32 = handle.into();
        store
            .cache_properties(&device, plane_id, drmkit_core::ObjectType::Plane)
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
            formats: info.formats().to_vec(),
            ..PlaneCapabilities::default()
        });
    }

    let registry = PlaneRegistry::from_capabilities(capabilities);
    Some((
        device,
        registry,
        crtc_id,
        u32::try_from(index).expect("small"),
    ))
}

/// A 16x16 opaque square with its hotspot in the middle.
fn square() -> Cursor {
    Cursor::from_argb(&vec![0xffff_00ff; 16 * 16], 16, 16, 8, 8).expect("cursor")
}

fn config(crtc_id: u32, crtc_index: u32) -> RendererConfig {
    RendererConfig {
        crtc_id,
        crtc_index,
        ..RendererConfig::default()
    }
}

#[test]
#[ignore = "needs a DRM device and DRM master"]
fn a_renderer_settles_on_a_plane_and_a_size_vkms() {
    let _guard = card_guard();
    let Some((device, registry, crtc_id, crtc_index)) = fixture() else {
        return;
    };
    let renderer = match Renderer::create(&device, &registry, &config(crtc_id, crtc_index)) {
        Ok(renderer) => renderer,
        Err(error) => {
            println!("note: skipped -- no usable cursor path here: {error}");
            return;
        }
    };

    let plane = renderer.plane();
    let (w, h) = renderer.size();
    println!(
        "note: plane {} via {:?}, buffer {w}x{h}",
        plane.plane_id, plane.path
    );

    assert_ne!(plane.plane_id, 0, "an atomic path names a plane");
    assert!(
        matches!(
            plane.path,
            PlanePath::AtomicCursor | PlanePath::AtomicOverlay
        ),
        "legacy is refused rather than half-supported: {:?}",
        plane.path
    );
    assert_eq!(w, h, "a cursor buffer is square");
    assert!(w >= 64, "never below the floor: {w}");
}

#[test]
#[ignore = "needs a DRM device and DRM master"]
fn a_cursor_is_drawn_and_staged_vkms() {
    let _guard = card_guard();
    let Some((device, registry, crtc_id, crtc_index)) = fixture() else {
        return;
    };
    let Ok(mut renderer) = Renderer::create(&device, &registry, &config(crtc_id, crtc_index))
    else {
        println!("note: skipped -- no usable cursor path here");
        return;
    };

    renderer.set_cursor(square()).expect("set cursor");
    renderer.move_to(100, 200);

    let mut request = AtomicRequest::new();
    let written = renderer.stage_into(&mut request).expect("stage");
    assert!(written >= 10, "wrote only {written} properties");

    // The plane is armed at the position the hotspot implies -- and the
    // hotspot is in *buffer* coordinates, not the frame's. A 16x16 sprite
    // centred in a 64x64 cursor buffer starts 24 pixels in, so a hotspot 8
    // pixels into the sprite is 32 into the buffer, and a pointer at
    // (100, 200) puts the buffer's corner at (68, 168).
    //
    // Getting this wrong by using the frame's hotspot gives (92, 192): off by
    // exactly the centring offset, which is invisible until the cursor is
    // near a screen edge or someone clicks.
    let plane_id = renderer.plane().plane_id;
    let store = {
        let mut store = drmkit_core::PropertyStore::new();
        store
            .cache_properties(&device, plane_id, drmkit_core::ObjectType::Plane)
            .expect("props");
        store
    };
    let value = |name: &str| {
        let id = store.property_id(plane_id, name).expect(name);
        request
            .writes()
            .iter()
            .find(|w| w.object_id == plane_id && w.property_id == id)
            .map(|w| w.value)
    };
    let (buffer_w, _) = renderer.size();
    let centring = i64::from(buffer_w - 16) / 2;
    let expected_x = 100 - (8 + centring);
    let expected_y = 200 - (8 + centring);
    assert_eq!(value("CRTC_X"), Some(expected_x.cast_unsigned()));
    assert_eq!(value("CRTC_Y"), Some(expected_y.cast_unsigned()));
    assert_ne!(value("FB_ID"), Some(0), "a visible cursor names a buffer");
    assert_eq!(value("CRTC_ID"), Some(u64::from(crtc_id)));
}

#[test]
#[ignore = "needs a DRM device and DRM master"]
fn hiding_disarms_the_plane_vkms() {
    // There is no property for "invisible": a cursor hides by letting go of
    // the plane. A zero-alpha buffer would still cost scanout bandwidth.
    let _guard = card_guard();
    let Some((device, registry, crtc_id, crtc_index)) = fixture() else {
        return;
    };
    let Ok(mut renderer) = Renderer::create(&device, &registry, &config(crtc_id, crtc_index))
    else {
        println!("note: skipped -- no usable cursor path here");
        return;
    };
    renderer.set_cursor(square()).expect("set cursor");
    renderer.set_visible(false);

    let mut request = AtomicRequest::new();
    renderer.stage_into(&mut request).expect("stage");

    let plane_id = renderer.plane().plane_id;
    let mut store = drmkit_core::PropertyStore::new();
    store
        .cache_properties(&device, plane_id, drmkit_core::ObjectType::Plane)
        .expect("props");
    for name in ["FB_ID", "CRTC_ID"] {
        let id = store.property_id(plane_id, name).expect(name);
        let value = request
            .writes()
            .iter()
            .find(|w| w.object_id == plane_id && w.property_id == id)
            .map(|w| w.value);
        assert_eq!(value, Some(0), "{name} should be cleared while hidden");
    }
}

#[test]
#[ignore = "needs a DRM device and DRM master"]
fn an_animation_redraws_only_when_the_frame_changes_vkms() {
    // A cursor ticking at 60Hz against a 100ms animation redraws on three
    // frames in eighteen. Redrawing every tick would burn a map, a compose and
    // a buffer flip each time for nothing.
    let _guard = card_guard();
    let Some((device, registry, crtc_id, crtc_index)) = fixture() else {
        return;
    };
    let Ok(mut renderer) = Renderer::create(&device, &registry, &config(crtc_id, crtc_index))
    else {
        println!("note: skipped -- no usable cursor path here");
        return;
    };

    // Two frames, 100ms each.
    let frames = vec![
        drmkit_cursor::Frame {
            pixels: vec![0xffff_0000; 64],
            width: 8,
            height: 8,
            xhot: 0,
            yhot: 0,
            delay: std::time::Duration::from_millis(100),
        },
        drmkit_cursor::Frame {
            pixels: vec![0xff00_ff00; 64],
            width: 8,
            height: 8,
            xhot: 0,
            yhot: 0,
            delay: std::time::Duration::from_millis(100),
        },
    ];
    let animated = Cursor::from_frames(frames).expect("animated cursor");
    renderer.set_cursor(animated).expect("set cursor");

    let mut redraws = 0;
    for step in 0..12u64 {
        // ~60Hz through one 200ms cycle.
        if renderer
            .tick(std::time::Duration::from_millis(step * 16))
            .expect("tick")
        {
            redraws += 1;
        }
    }
    assert_eq!(
        redraws, 1,
        "one frame boundary is crossed in 192ms, so one redraw"
    );
}

#[test]
#[ignore = "needs a DRM device and DRM master"]
fn a_static_cursor_never_redraws_vkms() {
    let _guard = card_guard();
    let Some((device, registry, crtc_id, crtc_index)) = fixture() else {
        return;
    };
    let Ok(mut renderer) = Renderer::create(&device, &registry, &config(crtc_id, crtc_index))
    else {
        println!("note: skipped -- no usable cursor path here");
        return;
    };
    renderer.set_cursor(square()).expect("set cursor");
    for step in 0..8u64 {
        assert!(
            !renderer
                .tick(std::time::Duration::from_millis(step * 500))
                .expect("tick"),
            "a static cursor has nothing to advance to"
        );
    }
}

#[test]
#[ignore = "needs a DRM device and DRM master"]
fn rotation_is_asked_of_the_hardware_only_when_it_can_do_it_vkms() {
    let _guard = card_guard();
    let Some((device, registry, crtc_id, crtc_index)) = fixture() else {
        return;
    };
    let Ok(mut renderer) = Renderer::create(&device, &registry, &config(crtc_id, crtc_index))
    else {
        println!("note: skipped -- no usable cursor path here");
        return;
    };
    renderer.set_cursor(square()).expect("set cursor");
    renderer.set_rotation(Rotation::Quarter).expect("rotate");

    let plane_id = renderer.plane().plane_id;
    let mut store = drmkit_core::PropertyStore::new();
    store
        .cache_properties(&device, plane_id, drmkit_core::ObjectType::Plane)
        .expect("props");
    let Ok(id) = store.property_id(plane_id, "rotation") else {
        println!("note: this plane has no rotation property, so the turn is in software");
        return;
    };

    let mut request = AtomicRequest::new();
    renderer.stage_into(&mut request).expect("stage");
    let value = request
        .writes()
        .iter()
        .find(|w| w.object_id == plane_id && w.property_id == id)
        .map(|w| w.value)
        .expect("rotation written");

    // The registry this fixture builds carries no rotation bits, so the plane
    // is treated as unable to turn: the pixels were turned in software and the
    // hardware must be asked for none.
    assert_eq!(
        value,
        Rotation::None.drm_mask(),
        "asking the hardware to turn pixels that are already turned turns them twice"
    );
}
