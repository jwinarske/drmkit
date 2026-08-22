# drmkit

A layered Linux DRM/KMS display framework — modesetting, plane allocation,
scene composition, and scanout. A behavioral-parity Rust port of
[drm-cxx](https://github.com/jwinarske/drm-cxx).

> **This crate is a placeholder and exports nothing yet.** The port is at
> Phase 0: workspace, pinned toolchain, CI, and drift linters are in place,
> along with two leaf crates. Nothing here is usable as a display library.
> **Do not depend on this crate yet.**

Follow <https://github.com/jwinarske/drmkit> for progress. Once the member
crates publish, this becomes their feature-gated re-export surface.

Upstream baseline: `jwinarske/drm-cxx` @ `dc2915b` (v2.0.1).

## License

MIT, matching drm-cxx.
