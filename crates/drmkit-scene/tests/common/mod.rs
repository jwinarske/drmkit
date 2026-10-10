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
    /// CRTCs a switch moved away from, still lit until teardown.
    retired_crtcs: Vec<u32>,
}

/// An output the fixture can drive: a connected connector, a CRTC that can
/// reach it, and the mode to light it with.
pub(crate) struct Output {
    connector_id: u32,
    crtc: drm::control::crtc::Handle,
    crtc_index: u32,
    mode: drmkit_core::Mode,
}

impl Output {
    /// The mode this output would be driven at, in pixels.
    pub(crate) fn mode_size(&self) -> (u32, u32) {
        (
            drmkit_modeset::ModeInfo::width(&self.mode),
            drmkit_modeset::ModeInfo::height(&self.mode),
        )
    }
}

/// The connector names `DRMKIT_TEST_CONNECTORS` asks for, in preference order.
///
/// A board can have connected outputs a test must not light -- a virtual one
/// that forwards to another machine, a panel something else owns -- so the
/// list is a whitelist when set, not a hint. Unset, every connected connector
/// is a candidate, in resource order.
fn wanted_connectors() -> Option<Vec<String>> {
    let list = std::env::var("DRMKIT_TEST_CONNECTORS").ok()?;
    Some(
        list.split(',')
            .map(|name| name.trim().to_owned())
            .filter(|name| !name.is_empty())
            .collect(),
    )
}

/// The first output not on `avoid_crtc` and not `avoid_connector`.
fn find_output(
    device: &Device,
    resources: &drm::control::ResourceHandles,
    avoid_crtc: Option<drm::control::crtc::Handle>,
    avoid_connector: Option<u32>,
) -> Option<Output> {
    let candidates: Vec<_> = resources
        .connectors()
        .iter()
        .filter_map(|handle| {
            let connector = device.get_connector(*handle, false).ok()?;
            let id = u32::from(*handle);
            (connector.state() == drm::control::connector::State::Connected
                && Some(id) != avoid_connector)
                .then_some(connector)
        })
        .collect();
    let name = |c: &drm::control::connector::Info| {
        format!("{}-{}", c.interface().as_str(), c.interface_id())
    };
    let ordered: Vec<_> = match wanted_connectors() {
        Some(wanted) => wanted
            .iter()
            .filter_map(|want| candidates.iter().find(|c| name(c) == *want))
            .collect(),
        None => candidates.iter().collect(),
    };
    ordered.into_iter().find_map(|connector| {
        let mode = *connector.modes().first()?;
        let crtc = connector.encoders().iter().find_map(|e| {
            let encoder = device.get_encoder(*e).ok()?;
            resources
                .filter_crtcs(encoder.possible_crtcs())
                .into_iter()
                .find(|crtc| Some(*crtc) != avoid_crtc)
        })?;
        let crtc_index = u32::try_from(resources.crtcs().iter().position(|c| *c == crtc)?).ok()?;
        Some(Output {
            connector_id: u32::from(connector.handle()),
            crtc,
            crtc_index,
            mode,
        })
    })
}

pub(crate) fn fixture(device: Device) -> Option<Fixture> {
    let resources = device.resource_handles().ok()?;
    let output = find_output(&device, &resources, None, None)?;
    let crtc_id = u32::from(output.crtc);

    let registry = PlaneRegistry::probe(&device).ok()?;
    let mut map = PlanePropertyMap::new();
    for plane in registry.for_crtc(output.crtc_index) {
        map.learn_plane(&device, plane.id).ok()?;
    }

    Some(Fixture {
        scene: LayerScene::new(crtc_id),
        device,
        registry,
        map,
        crtc_id,
        crtc_index: output.crtc_index,
        connector_id: output.connector_id,
        mode: output.mode,
        needs_modeset: true,
        retired_crtcs: Vec::new(),
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
        self.commit_within(None).map(|(report, _)| report)
    }

    /// Build and issue one real commit on a CRTC that lights at most `limit`
    /// planes at once, as some controllers do while advertising more (RK3566
    /// VOP2: three eligible, two usable).
    ///
    /// vkms has no such limit, so it is imposed: every allocator `TEST_ONLY`
    /// that would arm more is refused, and so is the real commit -- counted
    /// against what the kernel has now plus what the request writes. Returns
    /// the report and how many tests the limit refused.
    pub(crate) fn commit_limited(&mut self, limit: usize) -> Result<(CommitReport, usize), String> {
        self.commit_within(Some(limit))
    }

    fn commit_within(&mut self, limit: Option<usize>) -> Result<(CommitReport, usize), String> {
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

        let mut committer = PlaneLimit {
            inner: DeviceCommitter::new(
                &self.device,
                &self.map,
                &self.registry,
                self.crtc_index,
                AtomicCommitFlags::empty(),
                modeset.as_ref(),
            ),
            limit,
            extra: None,
            refused: 0,
        };
        let mut build = self
            .scene
            .build_frame(
                &self.registry,
                self.crtc_index,
                CommitKind::Real { arms_flip: false },
                &mut committer,
            )
            .map_err(|error| format!("building: {error}"))?;
        let refused = committer.refused;

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
        if let Some(limit) = limit {
            let armed = self.armed_after(&request);
            if armed > limit {
                self.scene.finalize_frame(build, KernelResult::Rejected);
                return Err(format!(
                    "committing: the frame arms {armed} planes on a CRTC that lights {limit}"
                ));
            }
        }
        match request.commit(&self.device, flags) {
            Ok(()) => {
                // Colorimetry is restated until the kernel has taken it once.
                // Without this it re-emits every frame and the minimal write
                // count is two higher than the diff decided.
                for plane_id in programmed {
                    self.map.note_color_committed(plane_id);
                }
                self.needs_modeset = false;
                Ok((self.scene.finalize_frame(build, KernelResult::Ok), refused))
            }
            Err(error) => {
                self.scene.finalize_frame(build, KernelResult::Rejected);
                Err(format!("committing: {error}"))
            }
        }
    }

    /// Planes armed on this CRTC once `request` applies on top of what the
    /// kernel has now.
    fn armed_after(&self, request: &AtomicRequest) -> usize {
        self.registry
            .for_crtc(self.crtc_index)
            .filter(|plane| {
                let written = |tag| {
                    let property = self.map.property_id(plane.id, tag)?;
                    request
                        .writes()
                        .iter()
                        .rev()
                        .find(|w| w.object_id == plane.id && w.property_id == property)
                        .map(|w| w.value)
                };
                let fb = written(drmkit_planes::PropTag::FbId)
                    .or_else(|| self.plane_property(plane.id, "FB_ID"))
                    .unwrap_or(0);
                let crtc = written(drmkit_planes::PropTag::CrtcId)
                    .or_else(|| self.plane_property(plane.id, "CRTC_ID"))
                    .unwrap_or(0);
                fb != 0 && crtc == u64::from(self.crtc_id)
            })
            .count()
    }

    /// Give the scene a canvas the size of the mode.
    pub(crate) fn enable_full_screen_composition(&mut self) -> Option<()> {
        let (w, h) = self.mode_size();
        self.scene.enable_composition(&self.device, w, h).ok()
    }

    /// Fill a layer's buffer with one opaque `xrgb` color.
    pub(crate) fn paint(&mut self, handle: LayerHandle, xrgb: u32) -> Option<()> {
        let layer = self.scene.layer_mut(handle)?;
        let mut mapping = layer.source_mut().map(drmkit_dumb::MapAccess::Write).ok()?;
        let stride = mapping.stride() as usize;
        let width = mapping.width() as usize;
        let height = mapping.height() as usize;
        // `height` rows, not every stride-sized chunk: a driver may allocate
        // past `stride * height` (msm pads 320x180 by 3072 bytes), and the
        // tail is shorter than a row.
        for row in mapping.pixels_mut().chunks_mut(stride).take(height) {
            for pixel in row[..width * 4].chunks_exact_mut(4) {
                pixel.copy_from_slice(&(0xFF00_0000 | xrgb).to_le_bytes());
            }
        }
        Some(())
    }

    /// Ask for a layer's zpos, leaving everything else about it alone.
    pub(crate) fn set_zpos(&mut self, handle: LayerHandle, zpos: u64) {
        let layer = self.scene.layer_mut(handle).expect("the layer");
        let mut display = *layer.display();
        display.zpos = Some(zpos);
        layer.set_display(display);
    }

    /// The color on screen at `(x, y)`, read back from the CRTC, alpha
    /// dropped; `None` where the CRTC cannot be read back (the SA8155P's
    /// planes hand out no readable framebuffer).
    pub(crate) fn pixel_at(&self, x: u32, y: u32) -> Option<u32> {
        let image = match drmkit_capture::snapshot(&self.device, self.crtc_id) {
            Ok(image) => image,
            Err(drmkit_capture::CaptureError::NothingReadable) => return None,
            Err(error) => panic!("snapshot: {error}"),
        };
        Some(image.pixels()[(y * image.width() + x) as usize] & 0x00FF_FFFF)
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

    /// A non-primary, non-cursor plane on this CRTC.
    ///
    /// What a pin case needs: pinning to the plane the allocator would have
    /// chosen anyway proves only that the allocator works.
    pub(crate) fn an_overlay_plane(&self) -> Option<u32> {
        self.registry
            .for_crtc(self.crtc_index)
            .find(|plane| plane.plane_type == drmkit_planes::PlaneType::Overlay)
            .map(|plane| plane.id)
    }

    /// What the kernel currently has on a plane's `FB_ID`.
    ///
    /// Read back from the device rather than from the report: the report says
    /// what the scene decided, and the point of asking is whether the decision
    /// reached hardware.
    pub(crate) fn plane_framebuffer(&self, plane_id: u32) -> Option<u32> {
        u32::try_from(self.plane_property(plane_id, "FB_ID")?).ok()
    }

    /// What the kernel currently has in one of a plane's properties.
    pub(crate) fn plane_property(&self, plane_id: u32, name: &str) -> Option<u64> {
        let handle = drm::control::plane::Handle::from(std::num::NonZeroU32::new(plane_id)?);
        let props = self.device.get_properties(handle).ok()?;
        let (handles, values) = props.as_props_and_values();
        for (property, value) in handles.iter().zip(values.iter()) {
            let Ok(info) = self.device.get_property(*property) else {
                continue;
            };
            if info.name().to_string_lossy() == name {
                return Some(*value);
            }
        }
        None
    }

    /// Planes the kernel has holding a framebuffer on `crtc_id`.
    pub(crate) fn lit_planes(&self, crtc_id: u32) -> Vec<u32> {
        let Ok(planes) = self.device.plane_handles() else {
            return Vec::new();
        };
        planes
            .iter()
            .filter_map(|handle| {
                let info = self.device.get_plane(*handle).ok()?;
                let on_crtc = info.crtc().map(u32::from) == Some(crtc_id);
                (info.framebuffer().is_some() && on_crtc).then(|| u32::from(*handle))
            })
            .collect()
    }

    /// A second connected output, on a CRTC other than this one.
    pub(crate) fn another_output(&self) -> Option<Output> {
        let resources = self.device.resource_handles().ok()?;
        let current = drm::control::crtc::Handle::from(std::num::NonZeroU32::new(self.crtc_id)?);
        find_output(
            &self.device,
            &resources,
            Some(current),
            Some(self.connector_id),
        )
    }

    /// A plane both this CRTC and `other` can use, preferring an overlay.
    ///
    /// The only plane a rebind can carry a baseline onto: a plane id from one
    /// pipe's exclusive set means nothing on the other.
    pub(crate) fn a_plane_shared_with(&self, other: &Output) -> Option<u32> {
        let shared: Vec<_> = self
            .registry
            .for_crtc(self.crtc_index)
            .filter(|plane| plane.compatible_with_crtc(other.crtc_index))
            .filter(|plane| plane.plane_type != drmkit_planes::PlaneType::Cursor)
            .collect();
        shared
            .iter()
            .find(|plane| plane.plane_type == drmkit_planes::PlaneType::Overlay)
            .or_else(|| shared.first())
            .map(|plane| plane.id)
    }

    /// Commit what a rebind left lit on the old CRTC.
    pub(crate) fn commit_detach(&mut self) -> Result<bool, String> {
        drmkit_scene::commit_detach(&self.device, &mut self.scene)
            .map_err(|error| format!("detaching: {error}"))
    }

    /// Drive `output` from the next commit on, leaving the old CRTC lit.
    ///
    /// What the scene does not do for the caller: learn the new pipe's planes
    /// and set its mode. The rebind itself is the test's to call.
    pub(crate) fn switch_to(&mut self, output: &Output) -> Option<()> {
        for plane in self.registry.for_crtc(output.crtc_index) {
            self.map.learn_plane(&self.device, plane.id).ok()?;
        }
        self.retired_crtcs.push(self.crtc_id);
        self.crtc_id = u32::from(output.crtc);
        self.crtc_index = output.crtc_index;
        self.connector_id = output.connector_id;
        self.mode = output.mode;
        self.needs_modeset = true;
        Some(())
    }

    /// The mode this fixture drives, in pixels.
    pub(crate) fn mode_size(&self) -> (u32, u32) {
        (
            drmkit_modeset::ModeInfo::width(&self.mode),
            drmkit_modeset::ModeInfo::height(&self.mode),
        )
    }

    /// The CRTC this fixture drives.
    pub(crate) const fn crtc_id(&self) -> u32 {
        self.crtc_id
    }

    /// Set the mode again with the next commit.
    ///
    /// What a caller does after a rebind: the scene has forgotten the output,
    /// and the mode has to be re-stated with the frame that reintroduces it.
    pub(crate) const fn force_modeset(&mut self) {
        self.needs_modeset = true;
    }

    pub(crate) fn teardown(&mut self) {
        for crtc_id in std::iter::once(self.crtc_id).chain(self.retired_crtcs.drain(..)) {
            let _ = self.device.set_crtc(
                drm::control::crtc::Handle::from(
                    std::num::NonZeroU32::new(crtc_id).expect("non-zero"),
                ),
                None,
                (0, 0),
                &[],
                None,
            );
        }
    }
}

#[allow(
    unused_imports,
    reason = "not every test file in this crate picks a CRTC"
)]
pub(crate) use drmkit_testkit::crtc::pick_crtc;

/// A committer for a CRTC that lights at most `limit` planes: any test that
/// would arm more is refused before it reaches the kernel. The canvas counts,
/// which is the whole point -- it is the plane that tips such a frame over.
struct PlaneLimit<'a> {
    inner: DeviceCommitter<'a>,
    limit: Option<usize>,
    extra: Option<u32>,
    refused: usize,
}

impl drmkit_planes::TestCommitter for PlaneLimit<'_> {
    fn test_assignment(
        &mut self,
        assignment: &[(u32, drmkit_planes::LayerRef<'_>)],
    ) -> Result<(), drmkit_planes::TestFailure> {
        let planes = assignment.len() + usize::from(self.extra.is_some());
        if self.limit.is_some_and(|limit| planes > limit) {
            self.refused += 1;
            return Err(drmkit_planes::TestFailure::Rejected);
        }
        self.inner.test_assignment(assignment)
    }

    fn set_extra_plane(&mut self, plane_id: u32, layer: drmkit_planes::Layer) {
        self.extra = Some(plane_id);
        self.inner.set_extra_plane(plane_id, layer);
    }

    fn clear_extra_plane(&mut self) {
        self.extra = None;
        self.inner.clear_extra_plane();
    }
}
