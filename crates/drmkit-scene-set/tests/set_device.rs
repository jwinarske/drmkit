// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! Parity port of `tests/integration/test_scene_set_vkms.cpp` from drm-cxx @
//! `4a0b64a`.
//!
//! # Two outputs, or nothing to say
//!
//! Every case upstream is about what happens *across* CRTCs, and it skips
//! below two connected outputs — which is what a default vkms gives. So do
//! these. What is reachable on one output is the part that is not about
//! crossing anything: that a set of one plans one commit and drives it, and
//! that a shared source reaches a real scene.
//!
//! A vkms with more outputs is configurable (`modprobe vkms` with a configfs
//! setup on recent kernels), which is what would make the rest of this file
//! run without a second physical display.

use std::sync::{Mutex, MutexGuard};

use drm::control::Device as ControlDevice;
use drmkit_core::{AtomicCommitFlags, AtomicRequest, Device};
use drmkit_fmt::fourcc;
use drmkit_planes::PlaneRegistry;
use drmkit_scene::{
    CommitKind, DeviceCommitter, DisplayParams, KernelResult, LayerScene, Modeset,
    PlanePropertyMap, Rect, arm_acquire_fences, emit_frame,
};
use drmkit_scene_set::{LayerSpec, NarrowPolicy, SceneSet, Target};

static CARD_LOCK: Mutex<()> = Mutex::new(());

fn card_guard() -> MutexGuard<'static, ()> {
    CARD_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// A connected output: its CRTC, that CRTC's index, its connector, and a mode.
struct Output {
    crtc_id: u32,
    crtc_index: u32,
    connector_id: u32,
    mode: drmkit_core::Mode,
}

fn open_card() -> Option<Device> {
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
    device.enable_universal_planes().ok()?;
    device.enable_atomic().ok()?;
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

/// Every connected output, each on a CRTC of its own.
///
/// One CRTC per output is what a set is for, so an output that can only share
/// a CRTC already taken is left out rather than double-booked.
fn outputs(device: &Device) -> Vec<Output> {
    let Ok(resources) = device.resource_handles() else {
        return Vec::new();
    };
    let all_crtcs = resources.crtcs();
    let mut taken: Vec<u32> = Vec::new();
    let mut found = Vec::new();

    for handle in resources.connectors() {
        let Ok(connector) = device.get_connector(*handle, false) else {
            continue;
        };
        if connector.state() != drm::control::connector::State::Connected {
            continue;
        }
        let Some(mode) = connector.modes().first().copied() else {
            continue;
        };
        let candidate = connector
            .encoders()
            .iter()
            .filter_map(|encoder| device.get_encoder(*encoder).ok())
            .flat_map(|encoder| resources.filter_crtcs(encoder.possible_crtcs()))
            .find(|crtc| !taken.contains(&u32::from(*crtc)));
        let Some(crtc) = candidate else { continue };
        let Some(index) = all_crtcs.iter().position(|c| *c == crtc) else {
            continue;
        };

        taken.push(u32::from(crtc));
        found.push(Output {
            crtc_id: u32::from(crtc),
            crtc_index: u32::try_from(index).unwrap_or(0),
            connector_id: u32::from(*handle),
            mode,
        });
    }
    found
}

/// Commit one group of slots as a single atomic request.
///
/// This is what a `NarrowPolicy` group *means*: every slot in it reaches the
/// kernel in one ioctl, so they land together or not at all.
fn commit_group(
    device: &Device,
    set: &mut SceneSet,
    group: &[usize],
    outputs: &[Output],
    registry: &PlaneRegistry,
    map: &PlanePropertyMap,
) -> Result<Vec<drmkit_scene::CommitReport>, String> {
    let mut request = AtomicRequest::with_capacity(64);
    let mut builds = Vec::new();

    // Every slot's `Modeset` has to outlive the commit, not the loop
    // iteration that made it. It holds a `PropertyBlob` destroyed on drop,
    // and the request carries only the blob's id -- so a modeset dropped at
    // the end of its iteration leaves the request naming a blob the kernel
    // has already freed, and the commit is refused with EINVAL. Collected
    // first for exactly that reason.
    let mut modesets = Vec::with_capacity(group.len());
    for index in group {
        let output = &outputs[*index];
        modesets.push(
            Modeset::learn(device, output.crtc_id, output.connector_id, &output.mode)
                .map_err(|error| format!("learning the mode: {error}"))?,
        );
    }

    for (slot, index) in group.iter().enumerate() {
        let output = &outputs[*index];
        let modeset = &modesets[slot];

        let mut committer = DeviceCommitter::new(
            device,
            map,
            registry,
            output.crtc_index,
            AtomicCommitFlags::empty(),
            Some(modeset),
        );
        let scene = set.scene_mut(*index).ok_or("the slot holds no scene")?;
        let mut build = scene
            .build_frame(
                registry,
                output.crtc_index,
                CommitKind::Real { arms_flip: false },
                &mut committer,
            )
            .map_err(|error| format!("building slot {index}: {error}"))?;

        emit_frame(&mut request, map, &mut build, Some(modeset))
            .map_err(|error| format!("emitting slot {index}: {error}"))?;
        arm_acquire_fences(&mut build, &mut request, map, true)
            .map_err(|error| format!("fences: {error}"))?;
        builds.push((*index, build));
    }

    // One ioctl for the whole group, which is the point of the grouping.
    let result = request.commit(device, AtomicCommitFlags::ALLOW_MODESET);
    let answer = if result.is_ok() {
        KernelResult::Ok
    } else {
        KernelResult::Rejected
    };

    let mut reports = Vec::new();
    for (index, build) in builds {
        let scene = set.scene_mut(index).ok_or("the slot vanished mid-commit")?;
        reports.push(scene.finalize_frame(build, answer));
    }
    result.map_err(|error| format!("committing: {error}"))?;
    Ok(reports)
}

/// A set over every connected output plans and commits.
///
/// With one output this is a set of one — which still exercises the shared
/// source through a real scene and a real commit, and is the only part of
/// this file a single-CRTC card can reach.
#[test]
#[ignore = "needs a DRM device and DRM master"]
fn a_set_over_the_connected_outputs_commits_each_group_vkms() {
    let _guard = card_guard();
    let Some(device) = open_card() else { return };
    let outs = outputs(&device);
    if outs.is_empty() {
        drmkit_testkit::skipped("no connected output");
        return;
    }
    println!(
        "note: {} connected output(s) on their own CRTCs",
        outs.len()
    );

    let registry = PlaneRegistry::probe(&device).expect("probe planes");
    let mut map = PlanePropertyMap::new();
    for output in &outs {
        for plane in registry.for_crtc(output.crtc_index) {
            map.learn_plane(&device, plane.id)
                .expect("plane properties");
        }
    }

    let mut set = SceneSet::new(outs.iter().map(|o| LayerScene::new(o.crtc_id)).collect());
    let source: std::rc::Rc<std::cell::RefCell<dyn drmkit_scene::LayerBufferSource>> =
        std::rc::Rc::new(std::cell::RefCell::new(
            drmkit_scene_sources::DumbBufferSource::create(&device, 64, 64, fourcc::XRGB8888)
                .expect("a dumb source"),
        ));

    // One source, one target per output: a mirror where there is more than
    // one, and an ordinary layer where there is not.
    set.add_layer(&LayerSpec {
        source,
        targets: outs
            .iter()
            .enumerate()
            .map(|(index, _)| Target {
                scene_index: index,
                display: DisplayParams {
                    src_rect: Rect {
                        x: 0,
                        y: 0,
                        w: 64,
                        h: 64,
                    },
                    dst_rect: Rect {
                        x: 0,
                        y: 0,
                        w: 64,
                        h: 64,
                    },
                    ..DisplayParams::default()
                },
                force_composited: false,
            })
            .collect(),
    })
    .expect("every target names a scene that exists");

    // Every output is modesetting on its first frame, which is uniform -- so
    // `AutoOnModeset` keeps them in one commit and they stay atomic.
    let modesetting: Vec<usize> = (0..outs.len()).collect();
    let groups = set.plan_commits(&modesetting, NarrowPolicy::AutoOnModeset);
    assert_eq!(
        groups,
        vec![modesetting.clone()],
        "a uniformly modesetting set must not split"
    );

    for group in &groups {
        let reports = commit_group(&device, &mut set, group, &outs, &registry, &map)
            .unwrap_or_else(|error| panic!("group {group:?}: {error}"));
        assert_eq!(
            reports.len(),
            group.len(),
            "one report per slot in the group"
        );
        for report in &reports {
            assert_eq!(report.layers_total, 1, "each scene has the mirrored layer");
        }
    }

    for output in &outs {
        let _ = device.set_crtc(
            drm::control::crtc::Handle::from(
                std::num::NonZeroU32::new(output.crtc_id).expect("non-zero"),
            ),
            None,
            (0, 0),
            &[],
            None,
        );
    }
}

/// A mixed set splits into two commits, modeset first.
///
/// Needs two outputs to be mixed at all: with one, every set is uniform and
/// the split can never happen. The partition itself is pinned host-side —
/// what this adds is that both commits actually land on hardware.
#[test]
#[ignore = "needs a DRM device, DRM master, and two connected outputs"]
fn a_mixed_set_splits_into_two_commits_that_both_land_vkms() {
    let _guard = card_guard();
    let Some(device) = open_card() else { return };
    let outs = outputs(&device);
    if outs.len() < 2 {
        drmkit_testkit::skipped(&format!(
            "{} connected output(s) on distinct CRTCs; a mixed set needs 2",
            outs.len()
        ));
        return;
    }

    let registry = PlaneRegistry::probe(&device).expect("probe planes");
    let mut map = PlanePropertyMap::new();
    for output in &outs {
        for plane in registry.for_crtc(output.crtc_index) {
            map.learn_plane(&device, plane.id)
                .expect("plane properties");
        }
    }

    let mut set = SceneSet::new(outs.iter().map(|o| LayerScene::new(o.crtc_id)).collect());
    for (index, _) in outs.iter().enumerate() {
        let source: std::rc::Rc<std::cell::RefCell<dyn drmkit_scene::LayerBufferSource>> =
            std::rc::Rc::new(std::cell::RefCell::new(
                drmkit_scene_sources::DumbBufferSource::create(&device, 64, 64, fourcc::XRGB8888)
                    .expect("a dumb source"),
            ));
        set.add_layer(&LayerSpec {
            source,
            targets: vec![Target {
                scene_index: index,
                display: DisplayParams {
                    src_rect: Rect {
                        x: 0,
                        y: 0,
                        w: 64,
                        h: 64,
                    },
                    dst_rect: Rect {
                        x: 0,
                        y: 0,
                        w: 64,
                        h: 64,
                    },
                    ..DisplayParams::default()
                },
                force_composited: false,
            }],
        })
        .expect("scene exists");
    }

    // Only the first output is modesetting: mixed, so it splits.
    let groups = set.plan_commits(&[0], NarrowPolicy::AutoOnModeset);
    assert_eq!(groups.len(), 2, "mixed sets split");
    assert_eq!(groups[0], vec![0], "and the modeset leads");

    for group in &groups {
        let reports = commit_group(&device, &mut set, group, &outs, &registry, &map)
            .unwrap_or_else(|error| panic!("group {group:?}: {error}"));
        assert_eq!(reports.len(), group.len());
    }

    for output in &outs {
        let _ = device.set_crtc(
            drm::control::crtc::Handle::from(
                std::num::NonZeroU32::new(output.crtc_id).expect("non-zero"),
            ),
            None,
            (0, 0),
            &[],
            None,
        );
    }
}
