// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! The CRTC colour pipeline against a real device.
//!
//! `#[ignore]`d and run in the device lanes with `--include-ignored`. Blobs
//! are created and destroyed but nothing is committed: a commit would need
//! DRM master, and taking it would blank a running display.

use std::sync::Mutex;

use drm::control::Device as _;
use drmkit_core::Device;
use drmkit_display::{ColorLut, CrtcColorPipeline, PipelineError, Stage};

static CARD_LOCK: Mutex<()> = Mutex::new(());

fn card_guard() -> std::sync::MutexGuard<'static, ()> {
    CARD_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
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
    device.enable_universal_planes().expect("universal planes");
    device.enable_atomic().expect("atomic");
    Some(device)
}

/// The first CRTC that has any colour pipeline at all.
///
/// `None` is a real answer, not a missing device: vc4 exposes no
/// `DEGAMMA_LUT`, `CTM` or `GAMMA_LUT` on any of its four CRTCs. Callers use
/// [`no_pipeline_anywhere`] to say so and to check the refusal is the right
/// one, which is the only place `PipelineError::NoPipeline` is reachable --
/// vkms has gamma, amdgpu has all three.
fn any_pipeline(device: &Device) -> Option<u32> {
    let resources = device.resource_handles().expect("resources");
    resources
        .crtcs()
        .iter()
        .map(|handle| u32::from(*handle))
        .find(|id| CrtcColorPipeline::new(device, *id).is_ok())
}

/// Confirm every CRTC really has no pipeline, and say so.
///
/// Returns true when the caller should stop. Not a bare skip: a card with no
/// colour pipeline still has something to assert -- that every CRTC is
/// classified as having none, rather than one of them failing to be read.
fn no_pipeline_anywhere(device: &Device) -> bool {
    let resources = device.resource_handles().expect("resources");
    for handle in resources.crtcs() {
        let id = u32::from(*handle);
        match CrtcColorPipeline::new(device, id) {
            Err(PipelineError::NoPipeline) => {}
            Err(error) => panic!("crtc {id}: {error}"),
            Ok(_) => return false,
        }
    }
    println!(
        "note: no CRTC on this card has a colour pipeline, and every one of \
         them says so"
    );
    true
}

#[test]
#[ignore = "needs a DRM device"]
fn a_stage_the_crtc_has_takes_a_blob() {
    let _guard = card_guard();
    let Some(device) = open_card() else { return };
    let Some(crtc_id) = any_pipeline(&device) else {
        assert!(no_pipeline_anywhere(&device));
        drmkit_testkit::skipped("no CRTC on this device has a colour pipeline");
        return;
    };

    let mut pipeline = CrtcColorPipeline::new(&device, crtc_id).expect("pipeline");
    let capabilities = *pipeline.capabilities();
    println!(
        "crtc {crtc_id}: degamma {} ctm {} gamma {}",
        capabilities.has_degamma_lut, capabilities.has_ctm, capabilities.has_gamma_lut
    );

    // Nothing staged yet: an apply now would write nothing, and the kernel
    // keeps whatever the last client left.
    for stage in [Stage::Degamma, Stage::Ctm, Stage::Gamma] {
        assert_eq!(pipeline.blob_id(stage), None);
    }

    pipeline.set_identity().expect("identity");

    for (present, stage) in [
        (capabilities.has_degamma_lut, Stage::Degamma),
        (capabilities.has_ctm, Stage::Ctm),
        (capabilities.has_gamma_lut, Stage::Gamma),
    ] {
        assert_eq!(
            pipeline.blob_id(stage).is_some(),
            present,
            "{}",
            stage.property()
        );
    }
}

#[test]
#[ignore = "needs a DRM device"]
fn a_stage_the_crtc_lacks_refuses_rather_than_committing_nothing() {
    // vkms has gamma and neither of the others. Silently doing nothing there
    // would leave a caller believing it had programmed a degamma curve.
    let _guard = card_guard();
    let Some(device) = open_card() else { return };
    let Some(crtc_id) = any_pipeline(&device) else {
        assert!(no_pipeline_anywhere(&device));
        drmkit_testkit::skipped("no CRTC on this device has a colour pipeline");
        return;
    };

    let mut pipeline = CrtcColorPipeline::new(&device, crtc_id).expect("pipeline");
    let capabilities = *pipeline.capabilities();

    if !capabilities.has_degamma_lut {
        assert!(matches!(
            pipeline.set_pq_to_linear(),
            Err(PipelineError::NoSuchStage(Stage::Degamma))
        ));
    }
    if !capabilities.has_ctm {
        assert!(matches!(
            pipeline.set_bt2020_to_bt709(),
            Err(PipelineError::NoSuchStage(Stage::Ctm))
        ));
    }
    if !capabilities.has_gamma_lut {
        assert!(matches!(
            pipeline.set_linear_to_pq(),
            Err(PipelineError::NoSuchStage(Stage::Gamma))
        ));
    }
    if capabilities.has_full_pipeline() {
        println!("note: this CRTC has every stage, so nothing was refused");
    }
}

#[test]
#[ignore = "needs a DRM device"]
fn a_lut_of_the_wrong_length_is_refused_before_the_kernel_sees_it() {
    // The kernel rejects a wrong-length blob by failing the whole atomic
    // commit -- every plane in the frame with it. Catching it here turns a
    // blank screen into an error naming the stage.
    let _guard = card_guard();
    let Some(device) = open_card() else { return };
    let Some(crtc_id) = any_pipeline(&device) else {
        assert!(no_pipeline_anywhere(&device));
        drmkit_testkit::skipped("no CRTC on this device has a colour pipeline");
        return;
    };

    let mut pipeline = CrtcColorPipeline::new(&device, crtc_id).expect("pipeline");
    let capabilities = *pipeline.capabilities();
    let Some(stage) = [
        (capabilities.has_degamma_lut, Stage::Degamma),
        (capabilities.has_gamma_lut, Stage::Gamma),
    ]
    .into_iter()
    .find_map(|(present, stage)| present.then_some(stage)) else {
        panic!("no LUT stage on crtc {crtc_id}");
    };

    let expected = match stage {
        Stage::Degamma => capabilities.degamma_lut_size,
        _ => capabilities.gamma_lut_size,
    };
    let short = vec![ColorLut::default(); expected as usize - 1];
    assert!(matches!(
        pipeline.set_custom_lut(stage, &short),
        Err(PipelineError::WrongLutSize { .. })
    ));

    let right = vec![ColorLut::default(); expected as usize];
    pipeline
        .set_custom_lut(stage, &right)
        .expect("the right length");
    assert!(pipeline.blob_id(stage).is_some());
}

#[test]
#[ignore = "needs a DRM device"]
fn restaging_a_stage_replaces_its_blob() {
    // Every restage that kept the old blob would be a leak, and a compositor
    // that re-programs the pipeline on every mode change runs the kernel out
    // of blob ids eventually.
    let _guard = card_guard();
    let Some(device) = open_card() else { return };
    let Some(crtc_id) = any_pipeline(&device) else {
        assert!(no_pipeline_anywhere(&device));
        drmkit_testkit::skipped("no CRTC on this device has a colour pipeline");
        return;
    };

    let mut pipeline = CrtcColorPipeline::new(&device, crtc_id).expect("pipeline");
    if !pipeline.capabilities().has_gamma_lut {
        drmkit_testkit::skipped("this CRTC has no gamma stage");
        return;
    }

    pipeline.set_identity().expect("identity");
    let first = pipeline.blob_id(Stage::Gamma).expect("staged");
    pipeline.set_linear_to_pq().expect("pq");
    let second = pipeline.blob_id(Stage::Gamma).expect("staged");

    assert_ne!(first, second, "the same blob id came back for a new curve");
}

#[test]
#[ignore = "needs a DRM device"]
fn an_apply_writes_only_what_was_staged() {
    let _guard = card_guard();
    let Some(device) = open_card() else { return };
    let Some(crtc_id) = any_pipeline(&device) else {
        assert!(no_pipeline_anywhere(&device));
        drmkit_testkit::skipped("no CRTC on this device has a colour pipeline");
        return;
    };

    let mut pipeline = CrtcColorPipeline::new(&device, crtc_id).expect("pipeline");
    let mut request = drmkit_core::AtomicRequest::new();
    pipeline.apply(&mut request).expect("apply");
    assert_eq!(
        request.len(),
        0,
        "nothing was staged, so nothing should be written"
    );

    pipeline.set_identity().expect("identity");
    let mut request = drmkit_core::AtomicRequest::new();
    pipeline.apply(&mut request).expect("apply");

    let capabilities = *pipeline.capabilities();
    let expected = usize::from(capabilities.has_degamma_lut)
        + usize::from(capabilities.has_ctm)
        + usize::from(capabilities.has_gamma_lut);
    assert_eq!(request.len(), expected);
}
