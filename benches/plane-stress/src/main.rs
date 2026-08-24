//! `plane_stress` — what the allocator does under load, as numbers.
//!
//! Drives a scene through a configurable workload and writes one CSV row per
//! frame. Where [`allocator_torture`] asserts thresholds and fails, this only
//! measures: it is the answer to "what does the allocator actually do under
//! load", not a gate.
//!
//! ```text
//! plane-stress [--device PATH] [--layers N] [--size WxH]
//!              [--churn none|move|alpha|toggle] [--churn-rate N]
//!              [--frames N] [--csv PATH]
//! ```
//!
//! Columns: `frame, build_us, test_commits, assigned, composited, unassigned,
//! skipped, fast_path`. Upstream also reports property and framebuffer counts,
//! composition buckets and damage clips; those are omitted rather than emitted
//! as zeros, because a column that is always zero reads as a measurement
//! rather than an absence.
//!
//! [`allocator_torture`]: ../../crates/drmkit-scene/tests/allocator_torture_vkms.rs

use std::fmt::Write as _;
use std::time::Instant;

use drmkit_core::{AtomicCommitFlags, Device, ObjectType, PropertyStore};
use drmkit_fmt::fourcc;
use drmkit_planes::{PlaneCapabilities, PlaneRegistry, PlaneType};
use drmkit_scene::{
    AcquiredBuffer, CommitKind, DeviceCommitter, DisplayParams, KernelResult, LayerBufferSource,
    LayerHandle, LayerScene, PlanePropertyMap, Rect, SourceError, SourceFormat,
};

/// How the workload mutates the scene each churn tick.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Churn {
    /// Nothing changes: the steady-state floor.
    None,
    /// Every layer's destination rectangle moves.
    Move,
    /// Every layer's plane alpha changes.
    Alpha,
    /// A layer is removed and re-added, so the layer set itself changes.
    Toggle,
}

struct Config {
    device: String,
    layers: usize,
    width: u32,
    height: u32,
    churn: Churn,
    churn_rate: usize,
    frames: usize,
    csv: Option<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            device: "/dev/dri/card0".to_owned(),
            layers: 4,
            width: 256,
            height: 256,
            churn: Churn::Move,
            churn_rate: 1,
            frames: 300,
            csv: None,
        }
    }
}

fn parse_args() -> Result<Config, String> {
    let mut cfg = Config::default();
    let mut args = std::env::args().skip(1);
    while let Some(flag) = args.next() {
        let mut value = || args.next().ok_or_else(|| format!("{flag} needs a value"));
        match flag.as_str() {
            "--device" => cfg.device = value()?,
            "--layers" => cfg.layers = value()?.parse().map_err(|_| "bad --layers")?,
            "--frames" => cfg.frames = value()?.parse().map_err(|_| "bad --frames")?,
            "--churn-rate" => {
                cfg.churn_rate = value()?.parse().map_err(|_| "bad --churn-rate")?;
                if cfg.churn_rate == 0 {
                    return Err("--churn-rate must be at least 1".into());
                }
            }
            "--csv" => cfg.csv = Some(value()?),
            "--size" => {
                let v = value()?;
                let (w, h) = v.split_once('x').ok_or("--size wants WxH")?;
                cfg.width = w.parse().map_err(|_| "bad --size width")?;
                cfg.height = h.parse().map_err(|_| "bad --size height")?;
            }
            "--churn" => {
                cfg.churn = match value()?.as_str() {
                    "none" => Churn::None,
                    "move" => Churn::Move,
                    "alpha" => Churn::Alpha,
                    "toggle" => Churn::Toggle,
                    other => return Err(format!("unknown churn mode {other}")),
                }
            }
            "--help" | "-h" => {
                println!("{}", include_str!("usage.txt"));
                std::process::exit(0);
            }
            other => return Err(format!("unknown flag {other}")),
        }
    }
    Ok(cfg)
}

struct Source {
    inner: drmkit_scene_sources::DumbBufferSource,
}

impl LayerBufferSource for Source {
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

fn build_registry(device: &Device) -> Result<(PlaneRegistry, u32), String> {
    use drm::control::Device as _;

    let resources = device
        .resource_handles()
        .map_err(|e| format!("resources: {e}"))?;
    let crtcs = resources.crtcs();
    let crtc_id: u32 = (*crtcs.first().ok_or("no CRTC")?).into();

    let mut capabilities = Vec::new();
    for handle in device.plane_handles().map_err(|e| format!("planes: {e}"))? {
        let info = device
            .get_plane(handle)
            .map_err(|e| format!("plane info: {e}"))?;
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
            .cache_properties(device, plane_id, ObjectType::Plane)
            .map_err(|e| format!("plane properties: {e}"))?;
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
    Ok((PlaneRegistry::from_capabilities(capabilities), crtc_id))
}

fn run(cfg: &Config) -> Result<String, String> {
    let device = Device::open(&cfg.device).map_err(|e| format!("{}: {e}", cfg.device))?;
    device
        .enable_universal_planes()
        .map_err(|e| format!("universal planes: {e}"))?;
    device.enable_atomic().map_err(|e| format!("atomic: {e}"))?;
    if device.set_master().is_err() {
        return Err("another client holds DRM master".into());
    }

    let (registry, crtc_id) = build_registry(&device)?;
    let planes = registry.force_disable_candidates(0).count();
    let mut map = PlanePropertyMap::new();
    map.learn_all(&device, &registry)
        .map_err(|e| format!("learn planes: {e}"))?;

    let mut scene = LayerScene::new(crtc_id);
    scene
        .enable_composition(&device, cfg.width, cfg.height)
        .map_err(|e| format!("composition canvas: {e}"))?;

    let mut handles: Vec<LayerHandle> = Vec::new();
    let mut add = |scene: &mut LayerScene, i: usize| -> Result<LayerHandle, String> {
        let source = drmkit_scene_sources::DumbBufferSource::create(
            &device,
            cfg.width,
            cfg.height,
            fourcc::ARGB8888,
        )
        .map_err(|e| format!("dumb source: {e}"))?;
        let handle = scene.add_layer(Box::new(Source { inner: source }));
        scene
            .layer_mut(handle)
            .ok_or("layer vanished")?
            .set_display(DisplayParams {
                dst_rect: Rect {
                    x: 0,
                    y: 0,
                    w: cfg.width,
                    h: cfg.height,
                },
                zpos: Some(u64::try_from(i).unwrap_or(0)),
                ..DisplayParams::default()
            });
        Ok(handle)
    };
    for i in 0..cfg.layers {
        handles.push(add(&mut scene, i)?);
    }

    let mut csv = String::from(
        "frame,build_us,test_commits,assigned,composited,unassigned,skipped,fast_path\n",
    );
    let mut totals = (0usize, 0usize, 0u64); // test commits, fast-path frames, µs

    for frame in 0..cfg.frames {
        if cfg.churn != Churn::None && frame > 0 && frame % cfg.churn_rate == 0 {
            churn(&mut scene, &mut handles, cfg, frame, &mut add)?;
        }

        let mut committer = DeviceCommitter::new(
            &device,
            &map,
            &registry,
            0,
            AtomicCommitFlags::empty(),
            None,
        );
        let started = Instant::now();
        let build = scene
            .build_frame(
                &registry,
                0,
                CommitKind::Real { arms_flip: false },
                &mut committer,
            )
            .map_err(|e| format!("frame {frame}: {e}"))?;
        let build_us = started.elapsed().as_micros();
        let report = scene.finalize_frame(build, KernelResult::Ok);

        totals.0 += report.test_commits_issued;
        totals.1 += usize::from(report.fb_delta_fast_path);
        totals.2 += u64::try_from(build_us).unwrap_or(u64::MAX);

        let _ = writeln!(
            csv,
            "{frame},{build_us},{},{},{},{},{},{}",
            report.test_commits_issued,
            report.layers_assigned,
            report.layers_composited,
            report.layers_unassigned,
            report.layers_skipped_no_frame,
            u8::from(report.fb_delta_fast_path),
        );
    }
    scene.drain();

    let frames = cfg.frames.max(1);
    // Counts, not measurements: a frame tally that exceeded f64's exact range
    // would mean a run of 2^53 frames, so the conversion is lossless in every
    // reachable case and saturating is the honest fallback for the rest.
    let as_f64 = |v: usize| f64::from(u32::try_from(v).unwrap_or(u32::MAX));
    eprintln!(
        "plane-stress: {} layers on {planes} planes, churn {:?} every {} frame(s)",
        cfg.layers, cfg.churn, cfg.churn_rate
    );
    eprintln!(
        "  {:.2} test commits/frame, {:.0}% fast-path, {:.1} us/frame to build",
        as_f64(totals.0) / as_f64(frames),
        (as_f64(totals.1) / as_f64(frames)) * 100.0,
        as_f64(usize::try_from(totals.2).unwrap_or(usize::MAX)) / as_f64(frames),
    );
    Ok(csv)
}

fn churn(
    scene: &mut LayerScene,
    handles: &mut Vec<LayerHandle>,
    cfg: &Config,
    frame: usize,
    add: &mut impl FnMut(&mut LayerScene, usize) -> Result<LayerHandle, String>,
) -> Result<(), String> {
    match cfg.churn {
        Churn::None => {}
        Churn::Move => {
            for (i, handle) in handles.iter().enumerate() {
                if let Some(layer) = scene.layer_mut(*handle) {
                    let mut display = *layer.display();
                    display.dst_rect.x = i32::try_from((frame + i * 8) % 128).unwrap_or(0);
                    layer.set_display(display);
                }
            }
        }
        Churn::Alpha => {
            for handle in handles.iter() {
                if let Some(layer) = scene.layer_mut(*handle) {
                    let mut display = *layer.display();
                    display.alpha = Some(u16::try_from((frame * 977) % 0xFFFF).unwrap_or(0xFFFF));
                    layer.set_display(display);
                }
            }
        }
        Churn::Toggle => {
            // Removing and re-adding changes the layer *set*, which is what
            // defeats a warm start rather than merely invalidating a frame.
            if let Some(handle) = handles.pop() {
                scene.remove_layer(handle);
            }
            handles.push(add(scene, handles.len())?);
        }
    }
    Ok(())
}

fn main() -> std::process::ExitCode {
    let cfg = match parse_args() {
        Ok(cfg) => cfg,
        Err(error) => {
            eprintln!("plane-stress: {error}");
            return std::process::ExitCode::FAILURE;
        }
    };
    match run(&cfg) {
        Ok(csv) => {
            match &cfg.csv {
                Some(path) => {
                    if let Err(error) = std::fs::write(path, csv) {
                        eprintln!("plane-stress: {path}: {error}");
                        return std::process::ExitCode::FAILURE;
                    }
                }
                None => print!("{csv}"),
            }
            std::process::ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("plane-stress: {error}");
            std::process::ExitCode::FAILURE
        }
    }
}
