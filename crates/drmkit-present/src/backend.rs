// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! The whole scanout path behind one object.
//!
//! Port of `src/present/scanout_backend.{hpp,cpp}`.

use std::os::fd::OwnedFd;

use drm::control::Device as ControlDevice;
use drmkit_core::{
    AtomicCommitFlags, AtomicRequest, Device, Mode, ObjectType, PropertyStore,
    commit_with_out_fence,
};
use drmkit_display::{DriverProfile, ScanoutTarget};
use drmkit_modeset::ModeInfo as _;
use drmkit_planes::PlaneRegistry;
use drmkit_scene::{
    CommitKind, CommitReport, DeviceCommitter, DisplayParams, KernelResult, LayerHandle,
    LayerScene, Modeset, PlanePropertyMap, arm_acquire_fences, emit_frame,
};
use drmkit_sync::SyncFence;

use crate::{FrameAction, FrameEconomy, PresentError, ScanoutProducer, negotiate};

/// When to ask for variable refresh.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum VrrPolicy {
    /// Never arm `VRR_ENABLED`.
    #[default]
    Off,
    /// Arm it when the driver profile reports the CRTC capable.
    Auto,
    /// Arm it regardless -- a no-op on a CRTC that does not expose it.
    On,
}

/// What to leave on screen when the backend goes away.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RestorePolicy {
    /// Leave the last committed frame up.
    #[default]
    None,
    /// Put back the CRTC configuration captured at
    /// [`create`](ScanoutBackend::create).
    ///
    /// What a display board wants on teardown: the console comes back rather
    /// than the screen freezing on whatever was committed last.
    SavedCrtc,
}

/// How to build a scanout backend.
#[derive(Debug, Clone, Copy)]
pub struct Config {
    /// The `FourCC` to allocate and scan out.
    pub fourcc: u32,
    /// The rotation the layer will be presented with, which the modifier
    /// negotiation filters against -- a tiled layout a plane cannot rotate is
    /// not a candidate for a rotated layer.
    pub rotation: drmkit_fmt::Rotation,
    /// Whether to ask for variable refresh.
    pub vrr: VrrPolicy,
    /// What to leave on screen at teardown.
    pub restore: RestorePolicy,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            fourcc: drmkit_fmt::fourcc::XRGB8888,
            rotation: drmkit_fmt::Rotation::Rotate0,
            vrr: VrrPolicy::Off,
            restore: RestorePolicy::None,
        }
    }
}

/// The CRTC configuration as it stood before the backend touched it.
#[derive(Debug, Clone, Copy)]
struct SavedCrtc {
    handle: drm::control::crtc::Handle,
    framebuffer: Option<drm::control::framebuffer::Handle>,
    position: (u32, u32),
    mode: Option<Mode>,
}

/// One output, one layer, and everything between a producer and the screen.
///
/// Discovers the output, reads the driver's profile, negotiates a modifier
/// against what the CRTC's planes can scan out, asks the producer for a buffer
/// in one of them, and drives a single-layer scene over it. What is left for
/// the caller is rendering and pacing.
///
/// The backend does **not** dispatch the page-flip event, for the reason
/// `DumbScanoutSink` does not: the scene deliberately has no `commit()` of its
/// own, so the caller drives its own `PageFlip` and calls
/// [`flip_landed`](Self::flip_landed).
pub struct ScanoutBackend {
    target: ScanoutTarget,
    profile: DriverProfile,
    modifiers: Vec<u64>,
    scene: LayerScene,
    layer: LayerHandle,
    registry: PlaneRegistry,
    map: PlanePropertyMap,
    economy: FrameEconomy,
    crtc_index: u32,
    /// Resolved once at create; `None` on a CRTC that exposes no `VRR_ENABLED`.
    vrr_property: Option<u32>,
    /// What the caller last asked for, and what was last written -- a change
    /// between them is what needs `ALLOW_MODESET`.
    vrr_wanted: bool,
    vrr_armed: bool,
    /// Resolved once at create; `None` on a CRTC with no `OUT_FENCE_PTR`.
    out_fence_property: Option<u32>,
    needs_modeset: bool,
    restore: RestorePolicy,
    saved: Option<SavedCrtc>,
    /// The descriptor the restore is issued on. Held raw because `Drop` has no
    /// device argument, and the restore is a legacy ioctl either way.
    device_fd: std::os::fd::RawFd,
}

impl std::fmt::Debug for ScanoutBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ScanoutBackend")
            .field("driver", &self.profile.name)
            .field("crtc_id", &self.target.crtc.id)
            .field("modifiers", &self.modifiers.len())
            .field("vrr", &self.vrr_armed)
            .finish_non_exhaustive()
    }
}

impl ScanoutBackend {
    /// Discover an output and build everything needed to present on it.
    ///
    /// # Errors
    ///
    /// [`PresentError::Bind`] if no output can be found, the planes cannot be
    /// probed, or the producer cannot allocate.
    pub fn create(
        device: &Device,
        producer: &mut dyn ScanoutProducer,
        config: &Config,
    ) -> Result<Self, PresentError> {
        let target = ScanoutTarget::discover(device)
            .map_err(|error| PresentError::Bind(format!("discovering an output: {error}")))?
            .map_err(|error| PresentError::Bind(format!("discovering an output: {error}")))?;
        let profile = DriverProfile::probe(device)
            .map_err(|error| PresentError::Bind(format!("probing the driver: {error}")))?;

        let crtc_id = target.crtc.id;
        let crtc_index = target.crtc.index;
        let (width, height) = (target.mode.width(), target.mode.height());

        let registry = PlaneRegistry::probe(device)
            .map_err(|error| PresentError::Bind(format!("probing planes: {error}")))?;
        let mut map = PlanePropertyMap::new();
        for plane in registry.for_crtc(crtc_index) {
            map.learn_plane(device, plane.id)
                .map_err(|error| PresentError::Bind(format!("plane {}: {error}", plane.id)))?;
        }

        // Negotiate against the union across every non-cursor plane on this
        // CRTC, not the primary's IN_FORMATS alone -- see
        // `PlaneRegistry::candidate_modifiers` for the split-SoC case that
        // makes the difference.
        let plane_modifiers = registry.candidate_modifiers(crtc_index, config.fourcc);
        let producer_modifiers = producer.exportable_modifiers(config.fourcc);
        let modifiers = if plane_modifiers.is_empty() {
            // No plane advertises IN_FORMATS. That is "this driver does not
            // say", not "nothing works", so the producer is left unconstrained
            // and the commit is what finds out.
            Vec::new()
        } else {
            let producer: Vec<drmkit_fmt::Modifier> = producer_modifiers
                .iter()
                .copied()
                .map(drmkit_fmt::Modifier)
                .collect();
            let planes: Vec<drmkit_fmt::Modifier> = plane_modifiers
                .iter()
                .copied()
                .map(drmkit_fmt::Modifier)
                .collect();
            negotiate(&producer, &planes, config.rotation)
                .into_iter()
                .map(|modifier| modifier.0)
                .collect()
        };

        let source = producer
            .create_buffer(width, height, config.fourcc, &modifiers)
            .map_err(|error| PresentError::Bind(format!("allocating: {error}")))?;

        let mut scene = LayerScene::new(crtc_id);
        let layer = scene.add_layer(source);
        scene
            .layer_mut(layer)
            .ok_or_else(|| PresentError::Bind("the layer vanished".to_owned()))?
            .set_display(DisplayParams {
                src_rect: drmkit_planes::Rect {
                    x: 0,
                    y: 0,
                    w: width,
                    h: height,
                },
                dst_rect: drmkit_planes::Rect {
                    x: 0,
                    y: 0,
                    w: width,
                    h: height,
                },
                ..DisplayParams::default()
            });

        let mut store = PropertyStore::new();
        let (vrr_property, out_fence_property) = if store
            .cache_properties(device, crtc_id, ObjectType::Crtc)
            .is_ok()
        {
            (
                store.property_id(crtc_id, "VRR_ENABLED").ok(),
                store.property_id(crtc_id, "OUT_FENCE_PTR").ok(),
            )
        } else {
            (None, None)
        };

        let vrr_wanted = match config.vrr {
            VrrPolicy::Off => false,
            VrrPolicy::On => true,
            VrrPolicy::Auto => profile.vrr_capable,
        };

        let saved = (config.restore == RestorePolicy::SavedCrtc)
            .then(|| Self::snapshot_crtc(device, crtc_id))
            .flatten();

        Ok(Self {
            target,
            profile,
            modifiers,
            scene,
            layer,
            registry,
            map,
            economy: FrameEconomy::new(),
            crtc_index,
            vrr_property,
            vrr_wanted,
            vrr_armed: false,
            out_fence_property,
            needs_modeset: true,
            restore: config.restore,
            saved,
            device_fd: device.raw_fd(),
        })
    }

    /// Capture the CRTC as it stands, for [`RestorePolicy::SavedCrtc`].
    ///
    /// Read through the legacy CRTC ioctl rather than atomic properties
    /// because that is what the restore writes back through: an atomic commit
    /// does not update the legacy framebuffer field, so a snapshot taken any
    /// other way would not describe what `set_crtc` can put back.
    fn snapshot_crtc(device: &Device, crtc_id: u32) -> Option<SavedCrtc> {
        let handle = drm::control::crtc::Handle::from(std::num::NonZeroU32::new(crtc_id)?);
        let info = device.get_crtc(handle).ok()?;
        Some(SavedCrtc {
            handle,
            framebuffer: info.framebuffer(),
            position: info.position(),
            mode: info.mode(),
        })
    }

    /// Present one frame.
    ///
    /// `out_fence` receives this commit's `OUT_FENCE` where the CRTC exposes
    /// `OUT_FENCE_PTR` — it signals once the frame is on screen, which is what
    /// a producer waits on rather than blocking the CPU until the flip event.
    ///
    /// # Errors
    ///
    /// [`PresentError::Build`] if the scene cannot build the frame,
    /// [`PresentError::Commit`] if the kernel refuses it.
    pub fn present(
        &mut self,
        device: &Device,
        flags: AtomicCommitFlags,
        out_fence: Option<&mut Option<OwnedFd>>,
    ) -> Result<CommitReport, PresentError> {
        // An unconditional present is a full commit by definition, and it
        // still has to be counted -- `frames_committed` reports every frame
        // that reached the kernel, not only the gated ones. Running it through
        // the economy is what keeps one counter authoritative for both paths.
        self.economy.force_full();
        let _ = self.economy.decide(true, false);
        self.commit(device, flags, out_fence)
    }

    /// Present only if the content changed, suppressing an idle frame.
    ///
    /// The **first** frame always commits, whatever `content_changed` says:
    /// nothing is on screen yet, so "unchanged" describes nothing, and a
    /// suppressed first frame leaves the scanout contents undefined.
    ///
    /// A suppressed frame returns a report with
    /// [`skipped_idle`](CommitReport::skipped_idle) set and every other field
    /// zero — there was no commit to count.
    ///
    /// # Errors
    ///
    /// As [`present`](Self::present).
    pub fn present_if_changed(
        &mut self,
        device: &Device,
        content_changed: bool,
        flags: AtomicCommitFlags,
        out_fence: Option<&mut Option<OwnedFd>>,
    ) -> Result<CommitReport, PresentError> {
        // `decide` keeps the counters itself, so this is the only place they
        // move on the gated path.
        match self.economy.decide(content_changed, false) {
            FrameAction::Skip => Ok(CommitReport::suppressed()),
            FrameAction::CommitFull | FrameAction::CommitDamaged => {
                self.commit(device, flags, out_fence)
            }
        }
    }

    /// Build, emit and commit one frame.
    fn commit(
        &mut self,
        device: &Device,
        flags: AtomicCommitFlags,
        out_fence: Option<&mut Option<OwnedFd>>,
    ) -> Result<CommitReport, PresentError> {
        // A local, not a field: it holds a `PropertyBlob` borrowed from the
        // device, and storing one would put that lifetime on this type and
        // everything holding it.
        let modeset = if self.needs_modeset {
            Some(
                Modeset::learn(
                    device,
                    self.target.crtc.id,
                    self.target.connector_id,
                    &self.target.mode,
                )
                .map_err(|error| PresentError::Bind(format!("learning the mode: {error}")))?,
            )
        } else {
            None
        };

        // Enabling variable refresh is a mode change on most drivers, so the
        // transition needs ALLOW_MODESET even when the mode itself is already
        // set. Disabling it is not, but the same flag is harmless there and
        // the alternative is a branch that gets the direction wrong.
        let vrr_changing = self.vrr_property.is_some() && self.vrr_wanted != self.vrr_armed;

        let mut flags = flags;
        if modeset.is_some() || vrr_changing {
            flags |= AtomicCommitFlags::ALLOW_MODESET;
        }

        let mut committer = DeviceCommitter::new(
            device,
            &self.map,
            &self.registry,
            self.crtc_index,
            AtomicCommitFlags::empty(),
            modeset.as_ref(),
        );

        // A flip is armed only if this commit actually asked for the event.
        // Saying otherwise leaves the scene waiting for a completion that will
        // never arrive: it holds every acquisition open, and teardown then
        // trips invariant 5 over a flip that was never in flight.
        let arms_flip = flags.contains(AtomicCommitFlags::PAGE_FLIP_EVENT);
        let mut build = self.scene.build_frame(
            &self.registry,
            self.crtc_index,
            CommitKind::Real { arms_flip },
            &mut committer,
        )?;

        let crtc_id = self.target.crtc.id;
        let vrr = (self.vrr_property, self.vrr_wanted);
        // The fence is armed if the caller asked for it, or if a source did:
        // a producer that opted into release fences needs one whether or not
        // this particular caller wants to see it.
        let wanted_internally = self.scene.wants_release_fence();
        let out_fence_property = (out_fence.is_some() || wanted_internally)
            .then_some(self.out_fence_property)
            .flatten();

        // Taken out of `build` before the closure: `arm_acquire_fences` needs
        // it mutably and `emit_frame` needs it immutably, and one closure
        // cannot hold both. The plan is one entry per placed layer, so the
        // copy is small and per-frame.
        let plan = build.plan().to_vec();
        let disables = build.disables().to_vec();

        let emit = |request: &mut AtomicRequest, slot: u64| -> Result<(), drmkit_core::CoreError> {
            emit_frame(request, &self.map, &plan, &disables, modeset.as_ref())?;
            arm_acquire_fences(&mut build, request, &self.map, true)?;
            if let (Some(property), wanted) = vrr {
                request.add_property(crtc_id, property, u64::from(wanted))?;
            }
            if let Some(property) = out_fence_property {
                request.add_property(crtc_id, property, slot)?;
            }
            Ok(())
        };

        let programmed: Vec<u32> = plan.iter().map(|entry| entry.plane_id).collect();
        let outcome = commit_with_out_fence(device, flags, emit);

        let fence = match outcome {
            Ok(fence) => fence,
            Err(error) => {
                // The scene still has to be told, or its acquisitions leak and
                // the next frame builds on a lie.
                self.scene.finalize_frame(build, KernelResult::Rejected);
                return Err(PresentError::Commit(error));
            }
        };
        // Imported before it is handed over, because both halves may want it:
        // the sources get a duplicate each, and the caller keeps the original.
        let release_fence = fence
            .as_ref()
            .and_then(|fd| SyncFence::import(std::os::fd::AsFd::as_fd(fd)).ok());
        if let Some(slot) = out_fence {
            *slot = fence;
        }

        for plane_id in programmed {
            self.map.note_color_committed(plane_id);
        }
        self.needs_modeset = false;
        self.vrr_armed = self.vrr_wanted;
        Ok(self
            .scene
            .finalize_frame_with_fence(build, KernelResult::Ok, release_fence.as_ref()))
    }

    /// Ask for variable refresh, or stop asking, from the next frame.
    ///
    /// A no-op on a CRTC that exposes no `VRR_ENABLED`.
    pub const fn set_vrr(&mut self, enable: bool) {
        self.vrr_wanted = enable;
    }

    /// Whether the discovered output can do variable refresh.
    #[must_use]
    pub const fn vrr_capable(&self) -> bool {
        self.profile.vrr_capable
    }

    /// Make the next frame a full commit, whatever the economy would decide.
    pub const fn force_full_present(&mut self) {
        self.economy.force_full();
    }

    /// How many frames reached the kernel.
    #[must_use]
    pub const fn frames_committed(&self) -> u64 {
        self.economy.committed()
    }

    /// How many frames were suppressed as idle.
    #[must_use]
    pub const fn frames_skipped(&self) -> u64 {
        self.economy.skipped()
    }

    /// The output this backend found.
    #[must_use]
    pub const fn target(&self) -> &ScanoutTarget {
        &self.target
    }

    /// The driver's capabilities, as probed.
    #[must_use]
    pub const fn profile(&self) -> &DriverProfile {
        &self.profile
    }

    /// The negotiated modifiers the producer was allowed to allocate in.
    ///
    /// Empty when no plane advertised `IN_FORMATS`, which is *unconstrained*
    /// rather than *nothing was acceptable*.
    #[must_use]
    pub fn modifiers(&self) -> &[u64] {
        &self.modifiers
    }

    /// The scene, for a caller adding layers of its own.
    #[must_use]
    pub const fn scene(&self) -> &LayerScene {
        &self.scene
    }

    /// The scene, mutably.
    pub const fn scene_mut(&mut self) -> &mut LayerScene {
        &mut self.scene
    }

    /// The layer the producer's buffer is presented on.
    #[must_use]
    pub const fn layer(&self) -> LayerHandle {
        self.layer
    }

    /// The discovered connector, as a handle, for the teardown restore.
    fn restore_connector(&self) -> Vec<drm::control::connector::Handle> {
        std::num::NonZeroU32::new(self.target.connector_id)
            .map(drm::control::connector::Handle::from)
            .into_iter()
            .collect()
    }

    /// Tell the scene the flip it armed has landed.
    ///
    /// Until this is called the scene believes a flip is outstanding, which is
    /// what keeps teardown from releasing a buffer the display engine is still
    /// reading — invariant 5.
    pub fn flip_landed(&mut self) {
        self.scene.flip_landed();
    }
}

impl Drop for ScanoutBackend {
    fn drop(&mut self) {
        if self.restore != RestorePolicy::SavedCrtc {
            return;
        }
        let Some(saved) = self.saved else { return };

        // Tear the scene down first, so the restore below reprograms a
        // pipeline that no longer references this backend's framebuffers.
        // Rust drops fields after this body runs, which is the wrong order, so
        // the scene is swapped out and dropped here explicitly.
        drop(std::mem::replace(
            &mut self.scene,
            LayerScene::new(self.target.crtc.id),
        ));

        // A legacy modeset, because that is what the snapshot describes: the
        // atomic commits this backend issues never touched the legacy
        // framebuffer field, so putting back what was read from it is the only
        // thing that restores what was on screen.
        //
        // SAFETY: the descriptor belongs to the device this backend was built
        // against, which outlives it -- the borrow is only for this ioctl.
        let borrowed = unsafe { std::os::fd::BorrowedFd::borrow_raw(self.device_fd) };
        let device = RawDevice { fd: borrowed };
        // No saved framebuffer means the CRTC was dark before this backend took
        // it over, so put it back dark -- no connector, no mode -- rather than
        // lighting up a framebuffer that never existed. With one, the connector
        // has to come along: a legacy modeset naming a framebuffer and no
        // connector is refused, which is a restore that silently does nothing.
        let (connectors, mode) = if saved.framebuffer.is_some() {
            (self.restore_connector(), saved.mode)
        } else {
            (Vec::new(), None)
        };
        // Errors are unactionable in a destructor, and a failed restore leaves
        // the last frame up -- which is what RestorePolicy::None would have
        // done anyway.
        let _ = device.set_crtc(
            saved.handle,
            saved.framebuffer,
            saved.position,
            &connectors,
            mode,
        );
    }
}

/// A bare descriptor wrapper, so the destructor can issue one ioctl without
/// owning a `Device`.
struct RawDevice<'a> {
    fd: std::os::fd::BorrowedFd<'a>,
}

impl std::os::fd::AsFd for RawDevice<'_> {
    fn as_fd(&self) -> std::os::fd::BorrowedFd<'_> {
        self.fd
    }
}

impl drm::Device for RawDevice<'_> {}
impl ControlDevice for RawDevice<'_> {}
