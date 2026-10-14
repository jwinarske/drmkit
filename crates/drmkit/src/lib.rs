// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! Umbrella crate for **drmkit**, a layered Linux DRM/KMS display framework —
//! modesetting, plane allocation, scene composition, and scanout.
//!
//! drmkit is a behavioral-parity Rust port of
//! [drm-cxx](https://github.com/jwinarske/drm-cxx).
//!
//! # Using it
//!
//! Every member crate is a module here, behind a feature of the same name:
//! `drmkit::scene` is `drmkit-scene`, `drmkit::core` is `drmkit-core`. The
//! default features are the path from a card to a committed scene —
//! `core`, `modeset`, `dumb`, `fmt`, `planes` and `scene` — and `full` adds
//! every other crate.
//!
//! ```toml
//! [dependencies]
//! drmkit = { version = "0.1", features = ["cursor", "capture"] }
//! ```
//!
//! None of those links a system library. The parts that do are features of
//! their own, so a build machine is never asked for one nobody wanted:
//!
//! | Feature | Turns on | Links |
//! |---|---|---|
//! | `edid` | EDID parsing in `display` | libdisplay-info |
//! | `hotplug` | the hotplug watch in `display` | libudev |
//! | `libinput` | input devices in `input` | libinput, libudev |
//! | `xkb` | keymaps and key resolution in `input` | xkbcommon |
//! | `libseat` | the seat backend in `session` | libseat |
//!
//! A member crate's other features — JPEG in `drmkit-capture`, the `tracing`
//! sink in `drmkit-log`, the device half of `drmkit-present` — are turned on
//! by naming that crate alongside this one; Cargo unifies the two.
//!
//! # Status
//!
//! Phases 0–4 of the porting plan are met. Of phase 5, the `v0.1.0-minimal`
//! ship gate, what remains is the smoke run on RK3588 and `VisionFive` 2
//! boards; 0.1.0 is released ahead of it. A scene can bring up a display,
//! allocate planes for a layer stack, commit a frame, composite what it
//! cannot place, and release buffers on the right vblank; EDID, colour
//! management, cursor and capture are in. So are the present spine,
//! cross-output orchestration, GBM surface sources and client-side
//! decorations.
//!
//! Not yet here: the GL compositor, EGL streams, V4L2 and `GStreamer` sources.
//! Of the 108 test files drm-cxx v5.0.0 has, 75 are ported, 14 partial and 3
//! not applicable; the 16 still pending each wait on hardware or a library
//! this tree does not have — a GPU render node, the `vgem`, `vivid` or
//! `vicodec` modules, or NVIDIA silicon. `TEST_PARITY.md` records which, per
//! file.
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
//! | [`drmkit-scene-gbm`](https://docs.rs/drmkit-scene-gbm) | A layer fed by a GBM surface — the swap chain a GL or Vulkan producer renders into. |
//! | [`drmkit-scene-set`](https://docs.rs/drmkit-scene-set) | Several scenes committed together, and how many ioctls that costs. |
//! | [`drmkit-present`](https://docs.rs/drmkit-present) | The scanout spine: buffer rings, modifier negotiation, frame pacing, and a backend that drives them. |
//! | [`drmkit-csd`](https://docs.rs/drmkit-csd) | Client-side decorations: title bars, shadows, and getting them onto a plane. |
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
#![cfg_attr(docsrs, feature(doc_cfg))]

macro_rules! member {
    ($feature:literal, $krate:ident as $name:ident) => {
        #[cfg(feature = $feature)]
        #[cfg_attr(docsrs, doc(cfg(feature = $feature)))]
        #[doc(inline)]
        pub use $krate as $name;
    };
}

member!("capture", drmkit_capture as capture);
member!("core", drmkit_core as core);
member!("csd", drmkit_csd as csd);
member!("cursor", drmkit_cursor as cursor);
member!("display", drmkit_display as display);
member!("dumb", drmkit_dumb as dumb);
member!("fmt", drmkit_fmt as fmt);
member!("gbm", drmkit_gbm as gbm);
member!("input", drmkit_input as input);
member!("log", drmkit_log as log);
member!("modeset", drmkit_modeset as modeset);
member!("planes", drmkit_planes as planes);
member!("present", drmkit_present as present);
member!("scene", drmkit_scene as scene);
member!("scene-gbm", drmkit_scene_gbm as scene_gbm);
member!("scene-set", drmkit_scene_set as scene_set);
member!("scene-sources", drmkit_scene_sources as scene_sources);
member!("session", drmkit_session as session);
member!("sync", drmkit_sync as sync);
member!("time", drmkit_time as time);

/// The upstream drm-cxx revision this port targets.
///
/// Kept as a constant so a consumer — and the port's own tooling — can tell at
/// a glance which C++ baseline a given drmkit build claims parity with.
pub const UPSTREAM_BASELINE: &str = "2b260d0";

#[cfg(test)]
mod tests {
    #[test]
    fn baseline_is_recorded() {
        assert_eq!(super::UPSTREAM_BASELINE, "2b260d0");
    }
}
