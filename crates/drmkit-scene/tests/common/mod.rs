// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! Shared rig for drmkit-scene's device cases.
//!
//! A scene bound to a connected output, committing for real. Three test files
//! drive it, which is why it lives here rather than in the first one that
//! needed it.

#![allow(dead_code, reason = "each test file uses a different part of the rig")]

use std::sync::{Mutex, MutexGuard};

use drm::control::Device as ControlDevice;
use drmkit_core::{AtomicCommitFlags, AtomicRequest, Device};
use drmkit_fmt::fourcc;
use drmkit_planes::PlaneRegistry;
use drmkit_scene::{
    CommitKind, CommitReport, DeviceCommitter, DisplayParams, KernelResult, LayerHandle,
    LayerScene, Modeset, PlanePropertyMap, Rect, arm_acquire_fences, emit_frame_damaged,
};

static CARD_LOCK: Mutex<()> = Mutex::new(());

pub(crate) fn card_guard() -> MutexGuard<'static, ()> {
    CARD_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

pub(crate) fn open_card() -> Option<Device> {
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

pub(crate) const LAYER_W: u32 = 64;
pub(crate) const LAYER_H: u32 = 64;

/// A scene on a connected output, committing for real.
pub(crate) struct Fixture {
    pub(crate) device: Device,
    registry: PlaneRegistry,
    map: PlanePropertyMap,
    pub(crate) scene: LayerScene,
    crtc_id: u32,
    crtc_index: u32,
    connector_id: u32,
    mode: drmkit_core::Mode,
    needs_modeset: bool,
}

pub(crate) fn fixture(device: Device) -> Option<Fixture> {
    let resources = device.resource_handles().ok()?;
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

    Some(Fixture {
        scene: LayerScene::new(crtc_id),
        device,
        registry,
        map,
        crtc_id,
        crtc_index,
        connector_id,
        mode,
        needs_modeset: true,
    })
}

impl Fixture {
    /// Add a dumb-backed layer at `(x, y)`.
    pub(crate) fn add_layer(&mut self, x: i32, y: i32, w: u32, h: u32) -> Option<LayerHandle> {
        let source =
            drmkit_scene_sources::DumbBufferSource::create(&self.device, w, h, fourcc::XRGB8888)
                .ok()?;
        let handle = self.scene.add_layer(Box::new(source));
        self.scene.layer_mut(handle)?.set_display(DisplayParams {
            src_rect: Rect { x: 0, y: 0, w, h },
            dst_rect: Rect { x, y, w, h },
            ..DisplayParams::default()
        });
        Some(handle)
    }

    /// Move a layer, leaving everything else about it alone.
    pub(crate) fn move_layer(&mut self, handle: LayerHandle, x: i32, y: i32) {
        let layer = self.scene.layer_mut(handle).expect("the layer is there");
        let mut display = *layer.display();
        display.dst_rect = Rect {
            x,
            y,
            w: display.dst_rect.w,
            h: display.dst_rect.h,
        };
        layer.set_display(display);
    }

    /// Build and issue one real commit.
    pub(crate) fn commit(&mut self) -> Result<CommitReport, String> {
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

        let mut request = AtomicRequest::with_capacity(64);
        let (_, _damage_blobs) = emit_frame_damaged(
            &mut request,
            &self.map,
            &mut build,
            modeset.as_ref(),
            Some(&self.device),
        )
        .map_err(|error| format!("emitting: {error}"))?;
        arm_acquire_fences(&mut build, &mut request, &self.map, true)
            .map_err(|error| format!("fences: {error}"))?;

        let programmed: Vec<u32> = build.plan().iter().map(|entry| entry.plane_id).collect();
        match request.commit(&self.device, flags) {
            Ok(()) => {
                // Colorimetry is restated until the kernel has taken it once.
                // Without this it re-emits every frame and the minimal write
                // count is two higher than the diff decided.
                for plane_id in programmed {
                    self.map.note_color_committed(plane_id);
                }
                self.needs_modeset = false;
                Ok(self.scene.finalize_frame(build, KernelResult::Ok))
            }
            Err(error) => {
                self.scene.finalize_frame(build, KernelResult::Rejected);
                Err(format!("committing: {error}"))
            }
        }
    }

    /// How many planes on this CRTC the allocator may place a layer on.
    ///
    /// Measured rather than assumed: it is the number a pressure case has to
    /// exceed, and it differs by an order of magnitude between vkms and a
    /// real `SoC`.
    pub(crate) fn eligible_planes(&self) -> usize {
        self.registry
            .force_disable_candidates(self.crtc_index)
            .count()
    }

    pub(crate) fn teardown(&mut self) {
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

#[allow(
    unused_imports,
    reason = "not every test file in this crate picks a CRTC"
)]
pub(crate) use drmkit_testkit::crtc::pick_crtc;
