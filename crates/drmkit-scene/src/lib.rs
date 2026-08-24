// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! Scene composition for drmkit.
//!
//! Port of `src/scene/` from drm-cxx @ `dc2915b`. This crate is being built up
//! across phase 3; today it carries the contract surface every source and the
//! commit path are written against.
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
//! is enforced by the type. The *timing* half still needs the vkms pins, which
//! land with the commit path.

mod canvas;
mod commit;
mod display;
mod frame;
mod lower;
mod release;
mod report;
mod scene;
mod source;

pub use canvas::{CompositeRect, CompositeSrc, blend_into, clear_into, format_supported};
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
pub use source::{
    AcquiredBuffer, BindingModel, DamageRect, DmaBufDesc, LayerBufferSource, SourceError,
    SourceFormat,
};

#[cfg(test)]
mod tests;
