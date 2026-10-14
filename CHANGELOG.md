# Changelog

Every crate in the workspace shares one version and is released together, so
this file covers all of them. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow
[Semantic Versioning](https://semver.org/) as Cargo reads it: before 1.0, a
minor version may break the API and a patch version does not.

## [Unreleased]

### Fixed

- `drmkit-scene`: a source's damage is bounded before it reaches the kernel,
  as the reference does. More than 16 rectangles repaints the whole frame,
  each rectangle is clipped to the buffer, and one left empty is dropped. A
  clip outside the buffer could otherwise make the kernel refuse the whole
  commit (P-53).
- `drmkit-scene`: the first fast-path frame the kernel refuses is logged as a
  warning, once, as the reference does. The latch was set but nothing logged
  (P-54).
- `drmkit-core`: `Device::open` reports a node the caller may not open as
  `CoreError::Io(EACCES)`, the errno as the reference returns it, rather than
  `CoreError::NotMaster`, whose "not DRM master" misdiagnosed a permissions
  problem.

## [0.1.1] - 2026-10-14

### Fixed

- `drmkit-session`: libseat's own diagnostics (why a seat could not be
  opened) now arrive through `drmkit-log`, tagged `[libseat]`, at error
  level, as the reference routes them. They went to the `log` crate at debug
  level, which `drmkit-log` does not read. The `libseat-log` feature no
  longer does anything beyond `libseat`, and is kept for 0.1.x.

## [0.1.0] - 2026-10-14

The first release: a behavioral-parity port of
[drm-cxx](https://github.com/jwinarske/drm-cxx) v5.0.0 (`2b260d0`), as
twenty-one crates.

### Added

- `drmkit-core`, `-modeset`, `-dumb`, `-fmt`, `-sync`, `-log`, `-time`: device
  handles and cached properties, atomic requests, mode selection and page-flip
  dispatch, dumb buffers, format modifiers and `IN_FORMATS`, `sync_file`
  fences, diagnostics, and a swappable presentation clock.
- `drmkit-planes`: the plane registry and the allocator that assigns layers
  to planes, with reading planes off a device behind `device`.
- `drmkit-scene`, `-scene-sources`, `-scene-set`, `-scene-gbm`: a layer stack
  lowered to a frame and committed, composition for what does not fit, the
  sources a layer's pixels come from, several scenes committed together, and
  layers fed by a GBM surface.
- `drmkit-present`: the scanout spine — buffer rings, modifier negotiation,
  frame pacing, and a backend that drives them.
- `drmkit-display`: EDID through libdisplay-info (`edid`), the hotplug watch
  (`hotplug`), driver and connector capabilities, colour pipelines and tone
  mapping.
- `drmkit-cursor`, `-capture`, `-csd`: cursor themes and cursor planes,
  readback of what a CRTC is scanning out, and client-side decorations.
- `drmkit-gbm`: GBM allocation, with libgbm opened at run time.
- `drmkit-session`, `-input`: holding devices across a VT switch through
  libseat, and libinput devices with xkb key resolution.
- `drmkit`: every member crate re-exported as a module behind a feature of the
  same name, with the system libraries as features of their own.

What the port leaves out, and why, is recorded per test file in
[`TEST_PARITY.md`](TEST_PARITY.md) and per divergence in
[`docs/parity-findings.md`](docs/parity-findings.md).

[Unreleased]: https://github.com/jwinarske/drmkit/compare/v0.1.1...HEAD
[0.1.1]: https://github.com/jwinarske/drmkit/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/jwinarske/drmkit/releases/tag/v0.1.0
