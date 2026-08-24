//! T7 differential parity harness -- Rust (port) scenario runner.
//!
//! Reads the same scenario script as `parity/runner.cc` and drives
//! [`LayerScene`] through it. Like the C++ runner it prints nothing of
//! interest; the artifact is the property-write trace `parity/trace-shim.c`
//! captures from the ioctls this makes, which must diff clean against the
//! reference's.
//!
//! Build with `--cfg rustix_use_libc` or the shim sees nothing: rustix's
//! default Linux backend issues syscalls directly, so `LD_PRELOAD` never gets
//! a look at them. `parity/run.sh` sets it.

use std::cell::Cell;
use std::collections::HashMap;
use std::env;
use std::fs;
use std::process::ExitCode;
use std::rc::Rc;
use std::time::Duration;

use drm::control::Device as _;
use drmkit_core::{AtomicCommitFlags, AtomicRequest, Device};
use drmkit_modeset::{PageFlip, Timeout};
use drmkit_planes::{PlaneCapabilities, PlaneRegistry, PlaneType};
use drmkit_scene::{
    AcquiredBuffer, CommitKind, DeviceCommitter, KernelResult, LayerBufferSource, LayerHandle,
    LayerScene, Modeset, PlanePropertyMap, SourceError, SourceFormat, emit_frame,
};

/// A [`DumbBufferSource`] that can be told to starve.
///
/// `WouldBlock` from `acquire` is the documented "no frame yet" signal: the
/// layer is skipped for this frame and counted, rather than the frame failing.
/// Live sources -- V4L2 before its first sample, an appsink mid-preroll -- do
/// this routinely, so the accounting has to be right or a compositor cannot
/// tell a starved layer from a dropped one.
struct StarvableSource {
    inner: drmkit_scene_sources::DumbBufferSource,
    starved: Rc<Cell<bool>>,
}

impl LayerBufferSource for StarvableSource {
    fn acquire(&mut self) -> Result<AcquiredBuffer, SourceError> {
        if self.starved.get() {
            return Err(SourceError::WouldBlock);
        }
        self.inner.acquire()
    }

    fn release(&mut self, acquired: AcquiredBuffer) {
        self.inner.release(acquired);
    }

    fn format(&self) -> SourceFormat {
        self.inner.format()
    }
}

/// The device, the scene, and everything needed to commit a frame.
struct Runner<'a> {
    device: &'a Device,
    registry: PlaneRegistry,
    map: PlanePropertyMap,
    scene: LayerScene,
    flip: PageFlip,
    modeset: Modeset<'a>,
    crtc_index: u32,
    /// Cleared after the first apply commit: the mode only needs setting once,
    /// and `ALLOW_MODESET` on every frame would let a mistake that silently
    /// re-modesets go unnoticed -- it is meant to be a full-frame stall.
    first_commit: bool,
    handles: HashMap<String, LayerHandle>,
    starve_flags: HashMap<String, Rc<Cell<bool>>>,
    width: u32,
    height: u32,
}

/// The card to drive.
///
/// Honors `DRMKIT_TEST_CARD` like the vkms test suite does, so the lane and a
/// developer both point the two runners at the same node -- comparing traces
/// from two different cards would be meaningless.
fn open_card() -> Result<Device, String> {
    let path = env::var("DRMKIT_TEST_CARD").unwrap_or_else(|_| "/dev/dri/card0".to_owned());
    Device::open(&path).map_err(|e| format!("{path}: {e}"))
}

impl<'a> Runner<'a> {
    fn new(device: &'a Device) -> Result<Self, String> {
        device
            .enable_universal_planes()
            .map_err(|e| format!("universal planes: {e}"))?;
        device.enable_atomic().map_err(|e| format!("atomic: {e}"))?;

        let resources = device
            .resource_handles()
            .map_err(|e| format!("resources: {e}"))?;
        let crtcs = resources.crtcs();
        let crtc = *crtcs.first().ok_or("no crtc")?;
        let crtc_id: u32 = crtc.into();

        // Pick the first connector that is actually plugged in and offers a
        // mode. On vkms that is the writeback-less virtual connector.
        let mut chosen = None;
        for handle in resources.connectors() {
            let info = device
                .get_connector(*handle, false)
                .map_err(|e| format!("connector: {e}"))?;
            if info.state() == drm::control::connector::State::Connected
                && let Some(mode) = info.modes().first()
            {
                chosen = Some((u32::from(*handle), *mode));
                break;
            }
        }
        let (connector_id, mode) = chosen.ok_or("no connected connector with a mode")?;

        let mut capabilities = Vec::new();
        for handle in device.plane_handles().map_err(|e| format!("planes: {e}"))? {
            let info = device
                .get_plane(handle)
                .map_err(|e| format!("plane info: {e}"))?;
            let mut mask = 0u32;
            for allowed in resources.filter_crtcs(info.possible_crtcs()) {
                if let Some(index) = crtcs.iter().position(|c| *c == allowed)
                    && index < u32::BITS as usize
                {
                    mask |= 1 << index;
                }
            }
            // Read the real plane type rather than calling everything an
            // overlay. The reference excludes the cursor plane from the
            // allocator's candidates, so mislabeling it puts a plane in the
            // trace that the other side never touches -- a diff that says
            // nothing about either implementation.
            let plane_id: u32 = handle.into();
            let mut store = drmkit_core::PropertyStore::new();
            store
                .cache_properties(device, plane_id, drmkit_core::ObjectType::Plane)
                .map_err(|e| format!("plane properties: {e}"))?;
            let plane_type = match store.property_value(plane_id, "type") {
                Ok(0) => PlaneType::Overlay,
                Ok(1) => PlaneType::Primary,
                Ok(2) => PlaneType::Cursor,
                Ok(other) => return Err(format!("unknown plane type {other}")),
                Err(e) => return Err(format!("plane type: {e}")),
            };

            capabilities.push(PlaneCapabilities {
                id: plane_id,
                possible_crtcs: mask,
                plane_type,
                formats: vec![drmkit_fmt::fourcc::ARGB8888],
                supports_scaling: true,
                ..PlaneCapabilities::default()
            });
        }
        let registry = PlaneRegistry::from_capabilities(capabilities);

        let mut map = PlanePropertyMap::new();
        map.learn_all(device, &registry)
            .map_err(|e| format!("learn planes: {e}"))?;

        let crtc_index = 0;
        if registry
            .force_disable_candidates(crtc_index)
            .next()
            .is_none()
        {
            return Err("crtc has no compatible planes".into());
        }

        let modeset = Modeset::learn(device, crtc_id, connector_id, &mode)
            .map_err(|e| format!("modeset: {e}"))?;
        let flip = PageFlip::new(device).map_err(|e| format!("page flip: {e}"))?;

        Ok(Self {
            registry,
            map,
            scene: LayerScene::new(crtc_id),
            flip,
            modeset,
            crtc_index,
            first_commit: true,
            handles: HashMap::new(),
            starve_flags: HashMap::new(),
            width: u32::from(mode.size().0),
            height: u32::from(mode.size().1),
            device,
        })
    }

    fn add(&mut self, name: &str, zpos: i32, x: i32, y: i32, w: u32, h: u32) -> Result<(), String> {
        let (w, h) = if w == 0 && h == 0 {
            (self.width, self.height)
        } else {
            (w, h)
        };
        let source = drmkit_scene_sources::DumbBufferSource::create(
            self.device,
            w,
            h,
            drmkit_fmt::fourcc::ARGB8888,
        )
        .map_err(|e| format!("dumb source: {e}"))?;
        let starved = Rc::new(Cell::new(false));
        let handle = self.scene.add_layer(Box::new(StarvableSource {
            inner: source,
            starved: Rc::clone(&starved),
        }));
        self.starve_flags.insert(name.to_owned(), starved);
        self.scene
            .layer_mut(handle)
            .ok_or("layer vanished")?
            .set_display(drmkit_scene::DisplayParams {
                src_rect: drmkit_planes::Rect { x: 0, y: 0, w, h },
                dst_rect: drmkit_planes::Rect { x, y, w, h },
                zpos: Some(u64::from(zpos.unsigned_abs())),
                ..drmkit_scene::DisplayParams::default()
            });
        self.handles.insert(name.to_owned(), handle);
        Ok(())
    }

    /// A geometry change, which must defeat the FB-only fast path: the kernel
    /// accepted the old rectangle, not this one.
    fn move_layer(&mut self, name: &str, x: i32, y: i32) -> Result<(), String> {
        let handle = *self.handles.get(name).ok_or("move: unknown layer")?;
        let layer = self.scene.layer_mut(handle).ok_or("move: stale handle")?;
        let mut display = *layer.display();
        display.dst_rect.x = x;
        display.dst_rect.y = y;
        layer.set_display(display);
        Ok(())
    }

    fn set_starved(&mut self, name: &str, starved: bool) -> Result<(), String> {
        self.starve_flags
            .get(name)
            .ok_or("starve: unknown layer")?
            .set(starved);
        Ok(())
    }

    fn del(&mut self, name: &str) -> Result<(), String> {
        let handle = self.handles.remove(name).ok_or("del: unknown layer")?;
        self.scene.remove_layer(handle);
        Ok(())
    }

    fn frame(&mut self) -> Result<(), String> {
        // The search's test commits carry the modeset too, until the CRTC is
        // up: a plane bound to an inactive CRTC is rejected, so without it the
        // very first frame would find nothing placeable.
        let modeset = self.first_commit.then_some(&self.modeset);
        let test_flags = if self.first_commit {
            AtomicCommitFlags::ALLOW_MODESET
        } else {
            AtomicCommitFlags::empty()
        };
        let mut committer = DeviceCommitter::new(
            self.device,
            &self.map,
            &self.registry,
            self.crtc_index,
            test_flags,
            modeset,
        );
        let build = self
            .scene
            .build_frame(
                &self.registry,
                self.crtc_index,
                CommitKind::Real { arms_flip: true },
                &mut committer,
            )
            .map_err(|e| format!("build: {e}"))?;

        let mut request = AtomicRequest::with_capacity(64);
        let modeset = self.first_commit.then_some(&self.modeset);
        emit_frame(
            &mut request,
            &self.map,
            build.plan(),
            build.disables(),
            modeset,
        )
        .map_err(|e| format!("emit: {e}"))?;

        let mut flags = AtomicCommitFlags::PAGE_FLIP_EVENT;
        if self.first_commit {
            flags |= AtomicCommitFlags::ALLOW_MODESET;
        }

        let programmed: Vec<u32> = build.plan().iter().map(|entry| entry.plane_id).collect();
        let result = match request.commit(self.device, flags) {
            Ok(()) => KernelResult::Ok,
            Err(e) => return Err(format!("commit: {e}")),
        };
        self.first_commit = false;
        // Only now: a commit the kernel refused carried nothing.
        for plane_id in programmed {
            self.map.note_color_committed(plane_id);
        }

        // finalize_frame is what arms the flip, so it has to run before the
        // event is landed -- landing first leaves the scene believing a flip
        // is still outstanding, and invariant 5's teardown tripwire fires.
        self.scene.finalize_frame(build, result);
        self.flip
            .dispatch(Timeout::Bounded(Duration::from_secs(1)))
            .map_err(|e| format!("dispatch: {e}"))?;
        self.scene.flip_landed();
        Ok(())
    }
}

fn run() -> Result<(), String> {
    let path = env::args().nth(1).ok_or("usage: runner <scenario>")?;
    let script = fs::read_to_string(&path).map_err(|e| format!("{path}: {e}"))?;
    let device = open_card()?;
    let mut runner = Runner::new(&device)?;

    for line in script.lines() {
        let line = line.split('#').next().unwrap_or("");
        let mut parts = line.split_whitespace();
        let Some(cmd) = parts.next() else { continue };
        match cmd {
            "frame" => runner.frame()?,
            "starve" | "unstarve" => {
                let name = parts.next().ok_or("starve: missing name")?;
                runner.set_starved(name, cmd == "starve")?;
            }
            "move" => {
                let args: Vec<&str> = parts.collect();
                let [name, x, y] = args.as_slice() else {
                    return Err("move: bad args".into());
                };
                let signed = |s: &str| {
                    s.parse::<i32>()
                        .map_err(|_| format!("move: bad number {s}"))
                };
                runner.move_layer(name, signed(x)?, signed(y)?)?;
            }
            "del" => {
                let name = parts.next().ok_or("del: missing name")?;
                runner.del(name)?;
            }
            "add" => {
                let args: Vec<&str> = parts.collect();
                let [name, zpos, x, y, w, h] = args.as_slice() else {
                    return Err("add: bad args".into());
                };
                let signed = |s: &str| s.parse::<i32>().map_err(|_| format!("add: bad number {s}"));
                let size = |s: &str| s.parse::<u32>().map_err(|_| format!("add: bad size {s}"));
                runner.add(
                    name,
                    signed(zpos)?,
                    signed(x)?,
                    signed(y)?,
                    size(w)?,
                    size(h)?,
                )?;
            }
            other => return Err(format!("unknown command: {other}")),
        }
    }
    runner.scene.drain();
    Ok(())
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("parity-runner: {e}");
            ExitCode::FAILURE
        }
    }
}
