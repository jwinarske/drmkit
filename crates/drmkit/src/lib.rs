// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! Umbrella crate for **drmkit**, a layered Linux DRM/KMS display framework —
//! modesetting, plane allocation, scene composition, and scanout.
//!
//! drmkit is a behavioral-parity Rust port of
//! [drm-cxx](https://github.com/jwinarske/drm-cxx).
//!
//! # Status
//!
//! **This crate is still a placeholder: it re-exports nothing.** The library
//! is real and the member crates below are usable, but taking `drmkit` gets
//! you a version constant and nothing else. Depend on the member crates you
//! need directly until this becomes their feature-gated re-export surface.
//!
//! Phases 0–4 of the porting plan are met and phase 5, the `v0.1.0-minimal`
//! ship gate, is under way. A scene can bring up a display, allocate planes
//! for a layer stack, commit a frame, composite what it cannot place, and
//! release buffers on the right vblank; EDID, colour management, cursor and
//! capture are in.
//!
//! Not yet here: the present spine, client-side decorations, and the GL, GBM
//! surface and stream source tiers. 47 of the 97 upstream test files are
//! unported and every one of them waits on a crate that does not exist yet.
//!
//! # The crates
//!
//! Roughly bottom-up. A consumer driving a display needs `-core`, `-modeset`
//! and one of the buffer crates; everything above that is optional.
//!
//! | Crate | What it is for |
//! |---|---|
//! | [`drmkit-core`](https://docs.rs/drmkit-core) | Device handles, cached properties, atomic requests. Everything else sits on this. |
//! | [`drmkit-modeset`](https://docs.rs/drmkit-modeset) | Choosing a mode, and dispatching page-flip events. |
//! | [`drmkit-dumb`](https://docs.rs/drmkit-dumb) | GPU-free CPU-mappable scanout buffers. |
//! | [`drmkit-gbm`](https://docs.rs/drmkit-gbm) | GBM allocation, for when the GPU renders. |
//! | [`drmkit-fmt`](https://docs.rs/drmkit-fmt) | Formats and modifiers: what a plane will actually scan out. |
//! | [`drmkit-planes`](https://docs.rs/drmkit-planes) | The plane registry and the allocator that assigns layers to planes. |
//! | [`drmkit-scene`](https://docs.rs/drmkit-scene) | A layer stack, lowered to a frame and committed; composition for what will not fit. |
//! | [`drmkit-scene-sources`](https://docs.rs/drmkit-scene-sources) | Where a layer's pixels come from: dumb buffers, imported DMA-BUFs. |
//! | [`drmkit-display`](https://docs.rs/drmkit-display) | EDID, driver capabilities, colour pipelines and tone mapping. |
//! | [`drmkit-cursor`](https://docs.rs/drmkit-cursor) | Cursor themes, and getting one onto a plane. |
//! | [`drmkit-capture`](https://docs.rs/drmkit-capture) | Reading back what a CRTC is scanning out. |
//! | [`drmkit-sync`](https://docs.rs/drmkit-sync) | `sync_file` fences, for explicit synchronisation. |
//! | [`drmkit-session`](https://docs.rs/drmkit-session) | Holding devices across a VT switch. |
//! | [`drmkit-input`](https://docs.rs/drmkit-input) | libinput devices and xkb key resolution. |
//! | [`drmkit-log`](https://docs.rs/drmkit-log) | Diagnostics: a process-wide level and an installable sink. |
//! | [`drmkit-time`](https://docs.rs/drmkit-time) | A swappable presentation clock, so pacing is testable. |
//!
//! # The contracts
//!
//! Six timing and lifetime rules are load-bearing across the crates, and each
//! is stated once in `INVARIANTS.md` and cross-referenced from the items that
//! carry it. They are the parts a caller can get wrong without any type
//! complaining — releasing a buffer a commit ago, tearing down while a flip is
//! outstanding — so the doc comment on the item that owns each one is worth
//! reading before using it.
//!
//! Follow <https://github.com/jwinarske/drmkit> for the porting plan, the
//! per-phase scope, and what each divergence from the reference turned out to
//! be.
//!
// The crate exports nothing yet by design; the lint would otherwise fire on an
// empty root.
#![allow(clippy::empty_docs)]

/// The upstream drm-cxx revision this port targets.
///
/// Kept as a constant so a consumer — and the port's own tooling — can tell at
/// a glance which C++ baseline a given drmkit build claims parity with.
pub const UPSTREAM_BASELINE: &str = "4a0b64a";

#[cfg(test)]
mod tests {
    #[test]
    fn baseline_is_recorded() {
        assert_eq!(super::UPSTREAM_BASELINE, "4a0b64a");
    }
}
