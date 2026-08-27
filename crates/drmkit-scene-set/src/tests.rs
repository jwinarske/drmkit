// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! Parity port of `tests/unit/test_scene_set.cpp` from drm-cxx @ `4a0b64a`.
//!
//! The partition is the part worth testing hardest: it decides how many
//! ioctls a multi-output frame costs and whether the outputs are atomic with
//! each other, and it is pure — no device, no kernel, no timing.

use crate::{LayerSpec, NarrowPolicy, SceneSet, SetError, SetLayerHandle, SlotState};
use crate::{Target, partition_for_policy};

fn engaged(wants_modeset: bool) -> SlotState {
    SlotState {
        is_hole: false,
        wants_modeset,
    }
}

const HOLE: SlotState = SlotState {
    is_hole: true,
    wants_modeset: false,
};

// --- partition ----------------------------------------------------------------

/// No slots, no commits.
#[test]
fn an_empty_set_commits_nothing() {
    for policy in [
        NarrowPolicy::Combined,
        NarrowPolicy::AutoOnModeset,
        NarrowPolicy::PerCrtc,
    ] {
        assert!(
            partition_for_policy(&[], policy).is_empty(),
            "{policy:?} invented a commit for a set with no scenes"
        );
    }
}

/// Slots that are all holes commit nothing either.
///
/// A hole is an index kept alive so the ones after it keep meaning what they
/// did. Committing for one would be an ioctl that programs nothing, and under
/// `PerCrtc` it would be one per removed output.
#[test]
fn a_set_of_only_holes_commits_nothing() {
    let slots = [HOLE, HOLE, HOLE];
    for policy in [
        NarrowPolicy::Combined,
        NarrowPolicy::AutoOnModeset,
        NarrowPolicy::PerCrtc,
    ] {
        assert!(
            partition_for_policy(&slots, policy).is_empty(),
            "{policy:?} committed for a hole"
        );
    }
}

/// `Combined` is always one group, holding every engaged slot.
#[test]
fn combined_is_one_group_of_every_engaged_slot() {
    let slots = [engaged(false), HOLE, engaged(true), engaged(false)];

    assert_eq!(
        partition_for_policy(&slots, NarrowPolicy::Combined),
        vec![vec![0, 2, 3]],
        "the modesetting slot goes in the same commit as the steady ones, \
         which is what Combined means and what makes the frame atomic"
    );
}

/// `PerCrtc` is one group per engaged slot, in index order.
#[test]
fn per_crtc_is_one_group_per_engaged_slot() {
    let slots = [engaged(false), HOLE, engaged(true)];

    assert_eq!(
        partition_for_policy(&slots, NarrowPolicy::PerCrtc),
        vec![vec![0], vec![2]],
        "one commit each, and the hole between them is not one of them"
    );
}

/// With every engaged slot steady, `AutoOnModeset` keeps them together.
///
/// The split has a cost — the outputs stop being atomic with each other — so
/// it is paid only when there is something to gain.
#[test]
fn auto_keeps_a_uniformly_steady_set_together() {
    let slots = [engaged(false), engaged(false), HOLE];

    assert_eq!(
        partition_for_policy(&slots, NarrowPolicy::AutoOnModeset),
        vec![vec![0, 1]]
    );
}

/// With every engaged slot modesetting, one group again.
///
/// Uniform is uniform in both directions: splitting here would give up
/// atomicity and buy nothing, since every output pays the modeset either way.
#[test]
fn auto_keeps_a_uniformly_modesetting_set_together() {
    let slots = [engaged(true), engaged(true)];

    assert_eq!(
        partition_for_policy(&slots, NarrowPolicy::AutoOnModeset),
        vec![vec![0, 1]]
    );
}

/// A mixed set splits, and the modeset goes first.
///
/// The order is the contract. A steady frame committed to an output that is
/// about to be reconfigured is wasted work at best, and at worst it is a
/// frame the modeset then blanks.
#[test]
fn auto_splits_a_mixed_set_with_the_modeset_first() {
    let slots = [engaged(false), engaged(true), engaged(false), engaged(true)];

    assert_eq!(
        partition_for_policy(&slots, NarrowPolicy::AutoOnModeset),
        vec![vec![1, 3], vec![0, 2]],
        "modesetting slots first, steady ones second, each in index order"
    );
}

/// A hole is never what makes a set look mixed.
///
/// `SlotState::default` is a hole with `wants_modeset` false, so a
/// classification that looked at the flag without checking the hole would
/// count every removed output as a steady one — and a set of one modesetting
/// output plus a hole would split into two commits, the second empty.
#[test]
fn holes_do_not_make_a_uniform_set_look_mixed() {
    let slots = [HOLE, engaged(true), HOLE];

    assert_eq!(
        partition_for_policy(&slots, NarrowPolicy::AutoOnModeset),
        vec![vec![1]],
        "one modesetting output and two holes is uniform, not mixed"
    );
}

// --- the set ------------------------------------------------------------------

fn empty_set() -> SceneSet {
    SceneSet::new(Vec::new())
}

fn source() -> std::rc::Rc<std::cell::RefCell<dyn drmkit_scene::LayerBufferSource>> {
    std::rc::Rc::new(std::cell::RefCell::new(Stub))
}

/// A source that never produces anything, for the cases that never commit.
struct Stub;

impl drmkit_scene::LayerBufferSource for Stub {
    fn acquire(&mut self) -> Result<drmkit_scene::AcquiredBuffer, drmkit_scene::SourceError> {
        Err(drmkit_scene::SourceError::WouldBlock)
    }

    fn release(&mut self, _acquired: drmkit_scene::AcquiredBuffer) {}

    fn binding_model(&self) -> drmkit_scene::BindingModel {
        drmkit_scene::BindingModel::SceneSubmitsFbId
    }

    fn format(&self) -> drmkit_scene::SourceFormat {
        drmkit_scene::SourceFormat {
            fourcc: drmkit_fmt::fourcc::XRGB8888,
            modifier: 0,
            width: 64,
            height: 64,
        }
    }
}

/// A set with no scenes is a set, not an error.
///
/// A compositor starting before any output is connected has one of these, and
/// refusing to construct it would make the caller special-case its own
/// startup.
#[test]
fn a_set_with_no_scenes_is_constructible() {
    let set = empty_set();
    assert_eq!(set.slot_count(), 0);
    assert_eq!(set.scene_count(), 0);
    assert!(
        set.plan_commits(&[], NarrowPolicy::AutoOnModeset)
            .is_empty()
    );
}

/// An index past the end answers nothing rather than panicking.
#[test]
fn an_index_out_of_range_answers_nothing() {
    let mut set = SceneSet::new(vec![drmkit_scene::LayerScene::new(1)]);
    assert!(set.scene(0).is_some());
    assert!(set.scene(1).is_none());
    assert!(set.scene(usize::MAX).is_none());
    assert!(set.scene_mut(99).is_none());
}

/// A layer with no targets is refused.
#[test]
fn a_layer_with_no_targets_is_refused() {
    let mut set = SceneSet::new(vec![drmkit_scene::LayerScene::new(1)]);

    let error = set
        .add_layer(&LayerSpec {
            source: source(),
            targets: Vec::new(),
        })
        .expect_err("a layer that appears nowhere is a caller bug, not a no-op");

    assert_eq!(error, SetError::NoTargets);
}

/// A target naming a scene the set does not have is refused.
#[test]
fn a_target_naming_no_scene_is_refused() {
    let mut set = empty_set();

    let error = set
        .add_layer(&LayerSpec {
            source: source(),
            targets: vec![Target {
                scene_index: 0,
                display: drmkit_scene::DisplayParams::default(),
                force_composited: false,
            }],
        })
        .expect_err("there is no scene 0");

    assert_eq!(error, SetError::NoSuchScene { index: 0, count: 0 });
}

/// A spec with one bad target adds nothing at all.
///
/// Checked before anything is added. Adding the good targets first and then
/// failing would leave the layer on some outputs and not others, with a
/// handle the caller never received to remove it by.
#[test]
fn a_spec_with_one_bad_target_leaves_the_set_untouched() {
    let mut set = SceneSet::new(vec![
        drmkit_scene::LayerScene::new(1),
        drmkit_scene::LayerScene::new(2),
    ]);

    let error = set
        .add_layer(&LayerSpec {
            source: source(),
            targets: vec![
                Target {
                    scene_index: 0,
                    display: drmkit_scene::DisplayParams::default(),
                    force_composited: false,
                },
                Target {
                    scene_index: 7,
                    display: drmkit_scene::DisplayParams::default(),
                    force_composited: false,
                },
            ],
        })
        .expect_err("scene 7 does not exist");

    assert_eq!(error, SetError::NoSuchScene { index: 7, count: 2 });
    assert_eq!(
        set.scene(0).map(drmkit_scene::LayerScene::len),
        Some(0),
        "the good target must not have been added before the bad one was found"
    );
}

/// A mirrored layer reaches every target scene, and removing it clears them
/// all.
#[test]
fn a_mirrored_layer_reaches_every_target_and_leaves_together() {
    let mut set = SceneSet::new(vec![
        drmkit_scene::LayerScene::new(1),
        drmkit_scene::LayerScene::new(2),
    ]);

    let handle = set
        .add_layer(&LayerSpec {
            source: source(),
            targets: vec![
                Target {
                    scene_index: 0,
                    display: drmkit_scene::DisplayParams::default(),
                    force_composited: false,
                },
                Target {
                    scene_index: 1,
                    display: drmkit_scene::DisplayParams::default(),
                    force_composited: true,
                },
            ],
        })
        .expect("both scenes exist");

    assert!(handle.is_valid());
    assert_eq!(set.scene(0).map(drmkit_scene::LayerScene::len), Some(1));
    assert_eq!(set.scene(1).map(drmkit_scene::LayerScene::len), Some(1));

    // The per-target flag reaches the layer it was meant for, and only it.
    let composited = |index: usize| {
        set.scene(index)
            .and_then(|scene| scene.handles().next())
            .and_then(|handle| set.scene(index)?.layer(handle))
            .map(drmkit_scene::SceneLayer::is_force_composited)
    };
    assert_eq!(composited(0), Some(false));
    assert_eq!(
        composited(1),
        Some(true),
        "targets carry their own flags; one setting it must not set the other"
    );

    set.remove_layer(handle);
    assert_eq!(
        set.scene(0).map(drmkit_scene::LayerScene::len),
        Some(0),
        "removing a mirrored layer removes every copy, or the outputs that \
         keep theirs go on scanning out content the caller has forgotten"
    );
    assert_eq!(set.scene(1).map(drmkit_scene::LayerScene::len), Some(0));
}

/// A handle that names nothing is ignored.
///
/// The default handle is what an uninitialised field carries, and it must not
/// remove whatever happens to be first.
#[test]
fn removing_with_a_stale_or_default_handle_is_a_no_op() {
    let mut set = SceneSet::new(vec![drmkit_scene::LayerScene::new(1)]);
    let handle = set
        .add_layer(&LayerSpec {
            source: source(),
            targets: vec![Target {
                scene_index: 0,
                display: drmkit_scene::DisplayParams::default(),
                force_composited: false,
            }],
        })
        .expect("scene 0 exists");

    set.remove_layer(SetLayerHandle::default());
    assert_eq!(
        set.scene(0).map(drmkit_scene::LayerScene::len),
        Some(1),
        "a default handle is not a licence to remove layer one"
    );

    set.remove_layer(handle);
    assert_eq!(set.scene(0).map(drmkit_scene::LayerScene::len), Some(0));

    // And again, now that it names a slot that has been emptied.
    set.remove_layer(handle);
}

/// Removing a scene leaves a hole, and the indices after it do not move.
///
/// The index is the caller's handle to an output. Compacting would have every
/// stored index silently start naming its neighbour.
#[test]
fn removing_a_scene_leaves_a_hole_rather_than_renumbering() {
    let mut set = SceneSet::new(vec![
        drmkit_scene::LayerScene::new(1),
        drmkit_scene::LayerScene::new(2),
        drmkit_scene::LayerScene::new(3),
    ]);

    set.remove_scene(1);

    assert_eq!(set.slot_count(), 3, "the slot stays");
    assert_eq!(set.scene_count(), 2, "but it holds nothing");
    assert!(set.scene(1).is_none());
    assert_eq!(
        set.scene(2).map(drmkit_scene::LayerScene::crtc_id),
        Some(3),
        "scene 2 is still scene 2"
    );
}

/// Removing out of range, or twice, is a no-op.
#[test]
fn removing_a_scene_that_is_not_there_is_a_no_op() {
    let mut set = SceneSet::new(vec![drmkit_scene::LayerScene::new(1)]);

    set.remove_scene(99);
    set.remove_scene(0);
    set.remove_scene(0);

    assert_eq!(set.scene_count(), 0);
    assert_eq!(set.slot_count(), 1);
}

/// A new scene fills a hole before growing the set.
///
/// An output that comes and goes would otherwise leave a slot behind each
/// time, and a laptop docking all day would accumulate them.
#[test]
fn a_new_scene_fills_a_hole_before_growing_the_set() {
    let mut set = SceneSet::new(vec![
        drmkit_scene::LayerScene::new(1),
        drmkit_scene::LayerScene::new(2),
    ]);
    set.remove_scene(0);

    let index = set.add_scene(drmkit_scene::LayerScene::new(3));

    assert_eq!(index, 0, "the hole was reused");
    assert_eq!(set.slot_count(), 2, "and the set did not grow");
    assert_eq!(set.scene(0).map(drmkit_scene::LayerScene::crtc_id), Some(3));
}

/// With no hole, a new scene goes on the end.
#[test]
fn a_new_scene_with_no_hole_to_fill_goes_on_the_end() {
    let mut set = SceneSet::new(vec![drmkit_scene::LayerScene::new(1)]);

    assert_eq!(set.add_scene(drmkit_scene::LayerScene::new(2)), 1);
    assert_eq!(set.slot_count(), 2);
}

/// The plan reflects the holes and the caller's modeset list.
#[test]
fn the_commit_plan_follows_the_set_and_what_the_caller_says_needs_a_modeset() {
    let mut set = SceneSet::new(vec![
        drmkit_scene::LayerScene::new(1),
        drmkit_scene::LayerScene::new(2),
        drmkit_scene::LayerScene::new(3),
    ]);
    set.remove_scene(1);

    assert_eq!(
        set.plan_commits(&[], NarrowPolicy::AutoOnModeset),
        vec![vec![0, 2]],
        "nothing is modesetting, so the two engaged outputs stay atomic"
    );
    assert_eq!(
        set.plan_commits(&[2], NarrowPolicy::AutoOnModeset),
        vec![vec![2], vec![0]],
        "one modesetting and one steady is mixed, and the modeset leads"
    );
    assert_eq!(
        set.plan_commits(&[1], NarrowPolicy::AutoOnModeset),
        vec![vec![0, 2]],
        "a modeset named on a hole changes nothing -- there is no output there"
    );
}
