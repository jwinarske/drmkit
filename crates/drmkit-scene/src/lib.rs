// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! Scene composition for drmkit.
//!
//! Port of `src/scene/` from drm-cxx @ `dc2915b`.
//!
//! A [`LayerScene`] owns one CRTC's stack of layers and the machinery that
//! turns them into a frame: which plane each layer lands on, what to composite
//! when there are more layers than planes, and when a buffer may go back to
//! whoever produced it.
//!
//! # Driving a frame
//!
//! ```text
//! let mut scene = LayerScene::new(crtc_id);
//! let handle = scene.add_layer(Box::new(source));
//! scene.layer_mut(handle)?.set_display(DisplayParams { .. });
//!
//! // per frame
//! let build = scene.build_frame(&registry, crtc_index, kind, &mut committer)?;
//! emit_frame(&mut request, &map, build.plan(), build.disables(), modeset)?;
//! arm_acquire_fences(&mut build, &mut request, &map, true)?;
//! let result = request.commit(&device, flags);
//! let report = scene.finalize_frame(build, result.into());
//! // ... and when the page-flip event arrives
//! scene.flip_landed();
//! ```
//!
//! The split between `build_frame` and `finalize_frame` is the part worth
//! understanding. `build_frame` acquires a buffer from every source, runs the
//! allocator — which may issue `TEST_ONLY` commits through the committer to
//! ask the kernel whether an arrangement is legal — and hands back a
//! [`FrameBuild`] describing what to program. It does not commit: the caller
//! owns the request, so it can add a modeset, arm fences, or refuse the frame.
//! `finalize_frame` then takes the kernel's answer and settles the
//! bookkeeping, which is what arms the flip and what decides which buffers are
//! now safe to release.
//!
//! A `FrameBuild` dropped without `finalize_frame` leaks its acquisitions, and
//! says so in a debug assertion rather than silently.
//!
//! # A source with nothing ready
//!
//! [`SourceError::WouldBlock`] from a source is not an error. It is the
//! documented "no frame yet" signal — a V4L2 device before its first sample,
//! an appsink mid-preroll — and the layer is *skipped* for that frame and
//! counted in the report, keeping the plane it had and the framebuffer already
//! on it. A frame does not fail because one producer was late, and a starved
//! layer does not blink off screen.
//!
//! # The release contract
//!
//! Two of the six pinned invariants live here, and both are about *when* a
//! buffer goes back to its source:
//!
//! - **Invariant 1** — a buffer is released only after the page flip that stops
//!   scanning it out completes, two commits deep, never when the ioctl returns.
//!   Releasing early lets a producer overwrite a buffer the display engine is
//!   still reading, which tears on hardware whose producer driver attaches no
//!   reservation fence.
//! - **Invariant 2** — a *failed* commit releases that frame's acquisitions
//!   immediately, because no flip is in flight to gate them on, and a
//!   non-`EACCES` failure does not suspend the scene. Self-healing producer
//!   rings depend on being able to reuse their buffers after a rejection.
//!
//! [`AcquiredBuffer`] is move-only, so handing a buffer back early is a move
//! and using it afterwards does not compile — the ownership half of invariant 1
//! is enforced by the type. The timing half cannot be, and is pinned against a
//! real device in `tests/release_invariants_vkms.rs`.

mod canvas;
mod commit;
mod display;
mod frame;
mod lower;
mod release;
mod report;
mod scene;
mod signaling;
mod source;

pub use canvas::{
    CompositeCanvas, CompositeRect, CompositeSrc, blend_into, canvas_format_for_plane,
    canvas_output_bpp, clear_into, format_supported,
};
pub use commit::{
    DeviceCommitter, FenceAction, Modeset, PlanePropertyMap, arm_acquire_fences, classify,
    emit_disable, emit_frame, emit_layer, fence_action,
};
pub use display::{DisplayParams, Rect, to_16_16};
pub use frame::{AcquireTally, CommitKind, FrameLifecycle, FrameOutcome, KernelResult};
pub use lower::{LoweringInput, lower_layer};
pub use release::{Acquisition, ReleaseQueue, ReleaseReason, Released, ScenePendingFlip};
pub use report::{CommitReport, LayerPlacement, Placement};
pub use scene::{FrameBuild, LayerHandle, LayerScene, PlanePlan, SceneError, SceneLayer};
pub use signaling::{ColorPrimaries, OutputSignalling, derive_output_signalling, widest_gamut};
pub use source::{
    AcquiredBuffer, BindingModel, DamageRect, DmaBufDesc, LayerBufferSource, SourceError,
    SourceFormat,
};

#[cfg(test)]
mod tests;
