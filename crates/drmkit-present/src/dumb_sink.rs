// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! Putting a finished CPU frame on screen, repeatedly.

use std::cell::RefCell;
use std::rc::Rc;

use drmkit_core::{AtomicCommitFlags, AtomicRequest, Device, Mode};
use drmkit_modeset::ModeInfo as _;
use drmkit_planes::PlaneRegistry;
use drmkit_scene::{
    AcquiredBuffer, BindingModel, CommitKind, CommitReport, DamageRect, DeviceCommitter,
    DisplayParams, KernelResult, LayerBufferSource, LayerHandle, LayerScene, Modeset,
    PlanePropertyMap, SourceError, SourceFormat, arm_acquire_fences, emit_frame_damaged,
};

use crate::{DumbRingSource, PaintError, Rect};

/// What went wrong presenting a frame.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum PresentError {
    /// The source buffer is shorter than `height * stride` says it is.
    #[error("a {height}-row frame at stride {stride} needs {needed} bytes, got {got}")]
    ShortFrame {
        /// Rows the sink expects.
        height: u32,
        /// Bytes per row the caller declared.
        stride: u32,
        /// Bytes that implies.
        needed: usize,
        /// Bytes actually supplied.
        got: usize,
    },

    /// Every ring slot is busy. Retry once a flip has completed.
    #[error("every ring slot is busy")]
    WouldBlock,

    /// Painting the frame into a slot failed.
    #[error("painting: {0}")]
    Paint(PaintError),

    /// The scene could not build this frame.
    #[error("building the frame: {0}")]
    Build(#[from] drmkit_scene::SceneError),

    /// A property write or the commit itself was refused.
    #[error("committing: {0}")]
    Commit(#[from] drmkit_core::CoreError),

    /// Setting the scene up against this output failed.
    #[error("binding the output: {0}")]
    Bind(String),
}

impl From<PaintError> for PresentError {
    fn from(error: PaintError) -> Self {
        match error {
            PaintError::WouldBlock => Self::WouldBlock,
            other => Self::Paint(other),
        }
    }
}

/// The ring, shared between the scene that owns the layer and the sink that
/// paints into it.
///
/// The reference keeps a raw `DumbRingSource*` alongside the scene that owns
/// it. There is no borrow that can express that here -- the scene owns the
/// source and the sink outlives neither -- so the ring is shared and the layer
/// holds a handle to it. `RefCell` rather than a lock because the whole spine
/// is single-threaded by contract, and a lock would suggest otherwise.
#[derive(Clone)]
struct SharedRing(Rc<RefCell<DumbRingSource>>);

impl LayerBufferSource for SharedRing {
    fn acquire(&mut self) -> Result<AcquiredBuffer, SourceError> {
        self.0.borrow_mut().acquire()
    }

    fn release(&mut self, acquired: AcquiredBuffer) {
        self.0.borrow_mut().release(acquired);
    }

    fn binding_model(&self) -> BindingModel {
        self.0.borrow().binding_model()
    }

    fn format(&self) -> SourceFormat {
        self.0.borrow().format()
    }

    fn on_session_paused(&mut self) {
        self.0.borrow_mut().on_session_paused();
    }

    fn on_session_resumed(&mut self, device: &Device) -> Result<(), SourceError> {
        self.0.borrow_mut().on_session_resumed(device)
    }
}

/// How a sink is configured.
#[derive(Debug, Clone, Copy)]
pub struct Config {
    /// The format to scan out, or `None` to negotiate one the plane takes.
    pub format: Option<u32>,
    /// Ring depth. Three is what the buffer-age path wants: one scanning, one
    /// in flight, one free to paint into.
    pub buffers: usize,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            format: None,
            buffers: 3,
        }
    }
}

/// A software present path: hand it a finished CPU frame, it puts it on screen.
///
/// For callers that render a whole frame on the CPU — a software rasteriser, a
/// plotting library, an instrument panel — and want none of GL, Vulkan or GBM.
/// It bundles a single-layer scene over a [`DumbRingSource`], so it inherits
/// the plane allocation, damage and release discipline rather than re-rolling
/// a modeset and page-flip loop.
///
/// # What it does not do
///
/// It does not dispatch the page-flip event. The reference's `present()` takes
/// a `PageFlip*` and the caller dispatches it; here the split is the same, and
/// visible: [`present`](Self::present) commits, the caller dispatches its own
/// [`PageFlip`](drmkit_modeset::PageFlip), and then calls
/// [`flip_landed`](Self::flip_landed).
///
/// That is one more call than the C++ needs, and it is the honest shape. A
/// present loop has other descriptors to poll — input, a timer, a socket — and
/// a sink that blocked on vblank inside `present` would own the loop rather
/// than serve it.
pub struct DumbScanoutSink {
    scene: LayerScene,
    ring: Rc<RefCell<DumbRingSource>>,
    registry: PlaneRegistry,
    map: PlanePropertyMap,
    crtc_id: u32,
    crtc_index: u32,
    connector_id: u32,
    mode: Mode,
    width: u32,
    height: u32,
    format: u32,
    /// Cleared after the first successful commit: the mode is set once, and
    /// `ALLOW_MODESET` on every frame would hide a mistake that silently
    /// re-modesets — which is a full-frame stall.
    needs_modeset: bool,
    layer: LayerHandle,
}

/// How many bytes a `height`-row frame at `stride` occupies.
///
/// `None` when that product does not fit a `usize`, which only a 32-bit host
/// can reach. Returning `None` rather than a wrapped product matters: the
/// wrapped value would be small, the caller's length check would pass, and
/// the row copy would then index past the frame it was handed.
pub(crate) fn frame_span(height: u32, stride: u32) -> Option<usize> {
    usize::try_from(height)
        .ok()?
        .checked_mul(usize::try_from(stride).ok()?)
}

impl DumbScanoutSink {
    /// Build a sink over an output that has already been chosen.
    ///
    /// `crtc_index` is the CRTC's position in the device's resource list, which
    /// is what the plane registry indexes by — not the same number as
    /// `crtc_id`, and mixing them up gives a registry view of the wrong pipe.
    ///
    /// # Errors
    ///
    /// [`PresentError::Bind`] if the planes cannot be probed or the layer
    /// cannot be added; [`PresentError::Paint`] if the format has no known
    /// bits-per-pixel.
    pub fn create(
        device: &Device,
        crtc_id: u32,
        crtc_index: u32,
        connector_id: u32,
        mode: &Mode,
        config: &Config,
    ) -> Result<Self, PresentError> {
        let format = config.format.unwrap_or_else(|| {
            // Fall back to XRGB8888 when the planes cannot be queried: it is
            // what nearly every controller takes, and the commit will say so
            // if this one does not.
            crate::negotiate_scanout_format(device, crtc_id, &[])
                .unwrap_or(drmkit_fmt::fourcc::XRGB8888)
        });
        let (width, height) = (mode.width(), mode.height());
        let buffers = if config.buffers == 0 {
            3
        } else {
            config.buffers
        };

        let ring = Rc::new(RefCell::new(DumbRingSource::new(
            width, height, format, buffers,
        )?));

        let registry = PlaneRegistry::probe(device)
            .map_err(|error| PresentError::Bind(format!("probing planes: {error}")))?;
        let mut map = PlanePropertyMap::new();
        for plane in registry.for_crtc(crtc_index) {
            map.learn_plane(device, plane.id)
                .map_err(|error| PresentError::Bind(format!("plane {}: {error}", plane.id)))?;
        }

        let mut scene = LayerScene::new(crtc_id);
        let layer = scene.add_layer(Box::new(SharedRing(Rc::clone(&ring))));
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

        Ok(Self {
            scene,
            ring,
            registry,
            map,
            crtc_id,
            crtc_index,
            connector_id,
            mode: *mode,
            width,
            height,
            format,
            needs_modeset: true,
            layer,
        })
    }

    /// Copy a finished CPU frame in and commit one flip.
    ///
    /// `src` is the whole frame in [`format`](Self::format) at `src_stride`
    /// bytes per row. `damage` is what changed since the previous present, in
    /// destination pixels; empty means the whole frame.
    ///
    /// The frame is always copied in full regardless of `damage`, because a
    /// reused ring slot holds an older frame and copying only the damage would
    /// leave the rest of it stale. `damage` is what gets *reported*, which the
    /// ring unions across buffer age into `FB_DAMAGE_CLIPS`.
    ///
    /// Under-reporting damage leaves stale pixels on screen. It is a hint to
    /// the driver, not a description of the copy.
    ///
    /// # Errors
    ///
    /// [`PresentError::WouldBlock`] when every ring slot is busy, which is flow
    /// control rather than failure. [`PresentError::ShortFrame`] when `src` is
    /// smaller than `height * src_stride`. Otherwise whatever the build or
    /// commit refused.
    pub fn present(
        &mut self,
        device: &Device,
        src: &[u8],
        src_stride: u32,
        damage: &[DamageRect],
        flags: AtomicCommitFlags,
    ) -> Result<CommitReport, PresentError> {
        let needed = frame_span(self.height, src_stride);
        if needed.is_none_or(|needed| src.len() < needed) {
            return Err(PresentError::ShortFrame {
                height: self.height,
                stride: src_stride,
                needed: needed.unwrap_or(usize::MAX),
                got: src.len(),
            });
        }

        let (width, height) = (self.width, self.height);
        self.ring.borrow_mut().paint(device, |mapping, _repaint| {
            for y in 0..height {
                let Some(row) = mapping.row_mut(y) else {
                    break;
                };
                let from = y as usize * src_stride as usize;
                let bytes = row.len().min(src_stride as usize);
                row[..bytes].copy_from_slice(&src[from..from + bytes]);
            }
            if damage.is_empty() {
                vec![Rect {
                    x: 0,
                    y: 0,
                    width,
                    height,
                }]
            } else {
                damage
                    .iter()
                    .map(|d| Rect {
                        x: d.x,
                        y: d.y,
                        width: d.w,
                        height: d.h,
                    })
                    .collect()
            }
        })?;

        self.commit(device, flags)
    }

    /// Build, emit and commit one frame from what the ring is holding.
    fn commit(
        &mut self,
        device: &Device,
        flags: AtomicCommitFlags,
    ) -> Result<CommitReport, PresentError> {
        // The modeset is a local, not a field: it holds a `PropertyBlob`
        // borrowed from the device, and storing one would put that lifetime on
        // this type and into everything holding it. It is needed once.
        let modeset = if self.needs_modeset {
            Some(
                Modeset::learn(device, self.crtc_id, self.connector_id, &self.mode)
                    .map_err(|error| PresentError::Bind(format!("learning the mode: {error}")))?,
            )
        } else {
            None
        };

        let mut flags = flags;
        if modeset.is_some() {
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

        let mut request = AtomicRequest::with_capacity(64);
        // The blobs are bound to a local that outlives the commit: the
        // request holds only their ids, and dropping them first would leave it
        // pointing at kernel objects that no longer exist.
        let (_, _damage_blobs) = emit_frame_damaged(
            &mut request,
            &self.map,
            &mut build,
            modeset.as_ref(),
            Some(device),
        )?;
        arm_acquire_fences(&mut build, &mut request, &self.map, true)?;

        let programmed: Vec<u32> = build.plan().iter().map(|entry| entry.plane_id).collect();
        let result = match request.commit(device, flags) {
            Ok(()) => KernelResult::Ok,
            Err(error) => {
                // The scene still has to be told, or its acquisitions leak and
                // the next frame builds on a lie.
                self.scene.finalize_frame(build, KernelResult::Rejected);
                return Err(PresentError::Commit(error));
            }
        };
        for plane_id in programmed {
            self.map.note_color_committed(plane_id);
        }
        self.needs_modeset = false;
        Ok(self.scene.finalize_frame(build, result))
    }

    /// Tell the scene the flip it armed has landed.
    ///
    /// Call it after dispatching the [`PageFlip`](drmkit_modeset::PageFlip)
    /// the commit's event was routed to. Until it is called the scene believes
    /// a flip is outstanding, which is what keeps teardown from releasing a
    /// buffer the display engine is still reading — invariant 5.
    pub fn flip_landed(&mut self) {
        self.scene.flip_landed();
    }

    /// The frame size this sink scans out.
    #[must_use]
    pub const fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// The `FourCC` a caller must render in — the requested format, or the
    /// negotiated one.
    #[must_use]
    pub const fn format(&self) -> u32 {
        self.format
    }

    /// The layer the frame is presented on, for a caller that wants to change
    /// its placement.
    #[must_use]
    pub const fn layer(&self) -> LayerHandle {
        self.layer
    }

    /// The scene underneath, for teardown and advanced wiring.
    #[must_use]
    pub const fn scene(&self) -> &LayerScene {
        &self.scene
    }

    /// As [`scene`](Self::scene).
    pub const fn scene_mut(&mut self) -> &mut LayerScene {
        &mut self.scene
    }
}
