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
- `drmkit-display`: `DriverProfile::probe` turns on universal planes and
  atomic before scanning, as the reference does, so `fb_damage_clips` no
  longer reads false on a descriptor that has not gone atomic yet; and a
  node whose resource list cannot be read probes to defaults rather than
  failing. `ScanoutTarget` discovery no longer fails when a driver refuses
  universal planes (P-55).
- `drmkit-planes`: `Allocation::composited` reports transient-composited
  layers, which were neither placed nor reported, and no longer reports an
  unplaced composition layer, as the reference marks them (P-56).
- `drmkit-cursor`: an animated cursor with many sizes loads, as it does in
  the reference. The 256-frame cap counted every size in the file, so
  Adwaita's busy cursors (six sizes of sixty frames) were refused as too
  large; the size is now chosen first and only its frames count (P-57).
- `drmkit-capture`: JPEG output is 4:2:0 at every quality, as the
  reference's libjpeg writes it; from quality 90 up it was 4:4:4. A
  dimension past 65,500 pixels is refused rather than cropped or sheared
  and reported as written (P-58).
- `drmkit-cursor`: `Renderer::set_rotation` refuses a rotation on the legacy
  path with `Unsupported`, as `create` does and as the reference does, rather
  than pre-rotating the pixels in software (P-59).
- `drmkit-sync`: converting a `SyncError` to `io::Error` gives the
  reference's errno -- `EBADF` for an empty fence or an empty merge,
  `ETIMEDOUT` for a timeout -- where it gave errno-less kinds. An empty
  fence's `ErrorKind` is no longer `InvalidInput` (P-60).
- `drmkit-capture`: the warning for a capture with nothing readable gives
  the enumerated and unbound plane counts, as the reference's does, so a
  missing client cap can be told from a blank CRTC (P-61).
- `drmkit-scene-sources`: a dma-buf import closes its GEM handles when it
  goes, and when it fails, as the reference does. They were never closed,
  so every buffer a producer had handed over stayed pinned until the device
  was closed (P-62).
- `drmkit-scene-sources`: an external dma-buf source and an external ring
  re-import their buffers on a session resume, as the reference does. After
  a VT switch the source's layer failed every commit and the ring's went
  blank. The GBM source no longer tears its old import down through the dead
  descriptor when it re-allocates (P-63).
- `drmkit-session`: a device that cannot be reopened on resume no longer
  costs the others. It is reported as the new `SessionEvent::DeviceLost`
  and the rest resume, as the reference reports it through
  `resume_failed_cb`; `dispatch` returned the first failure and left every
  later device closed (P-64).
- `drmkit-scene-sources`: `DumbBufferSource` allocates for its format, as
  the reference does: a packed format at its own depth, so `RGB565` gets a
  16-bit stride, and the semi-planar 4:2:0 formats as two planes. Every
  format was sized at 32 bits a pixel in one plane, so `RGB565` scanned out
  sheared and `NV12` could not be created. A format with no known depth is
  refused as `InvalidConfig`, and a resume that cannot re-allocate reports
  the kernel's errno rather than `ENOMEM` (P-65).
- `drmkit-present`: when nothing is negotiated -- no plane advertises
  `IN_FORMATS`, or none takes a layout the producer can make --
  `ScanoutBackend::create` allows the producer `LINEAR`, as the reference
  does and as `ScanoutProducer` documented. It passed an empty set, which a
  producer reads as no constraint, so a tiled or compressed buffer no plane
  takes could reach the first commit (P-66).

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
