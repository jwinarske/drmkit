# Changelog

Every crate in the workspace shares one version and is released together, so
this file covers all of them. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow
[Semantic Versioning](https://semver.org/) as Cargo reads it: before 1.0, a
minor version may break the API and a patch version does not.

## [Unreleased]

### Fixed

- `drmkit-display`: `DriverProfile::vrr_capable` is true only when a
  connected sink reports `vrr_capable`. It was true whenever a CRTC had
  `VRR_ENABLED`, which the DRM core gives every CRTC of an atomic driver, so
  `VrrPolicy::Auto` armed variable refresh on sinks that cannot do it. The
  reference has the same bug, filed as drm-cxx#371 (P-76).
- `drmkit-display`: `ToneMapper` interpolates its tables as the reference
  does, taking the difference of two entries in `f32` before widening it.
  Widening first left some 16-bit pixels one step higher than the
  reference's (P-77).
- `drmkit-display`: a `ToneMapper` reports zero for the parameter its
  direction does not use, as the reference does: `target_max_nits` on a
  mapper up to PQ, `sdr_white_nits` on one down to SDR. It reported 100
  (P-78).
- `drmkit-display`: `curves::bt1886_oetf` returns NaN for NaN, as the
  reference does, rather than 0.0 (P-79).
- `drmkit-display`: the hotplug monitor reads the kernel's `CONNECTOR=` hint
  as digits only, as the reference does, so a leading `+` means
  re-enumerate rather than a connector id (P-80).
- `drmkit-capture`: `snapshot` closes the GEM handles its framebuffer
  queries open. Each capture left one open per plane, pinning every buffer
  it had read until the descriptor closed. The reference has the same bug,
  filed as drm-cxx#373 (P-83).
- `drmkit-cursor`: a cursor near the top or left edge commits. A negative
  plane origin was written as a 32-bit two's complement, which the kernel
  range-checks as a 64-bit signed value and refuses, so with a buffer of the
  driver's size every commit within about half a buffer of either edge
  failed with `EINVAL`. It is now sign-extended, as the reference does
  (P-84).
- `drmkit-scene`: a layer partly off the top or left edge is placed on a
  plane. Its negative position was written as a 32-bit two's complement,
  which the kernel range-checks as a 64-bit signed value and refuses, so the
  layer was left unassigned and did not appear. It is now sign-extended, as
  the reference does (P-85).
- `drmkit-capture`: `snapshot` reads a plane from where its framebuffer
  starts. It read from byte 0 of the buffer object and ignored the
  framebuffer's offset, so a framebuffer starting partway in came out shifted
  and short of its last rows. The reference has the same bug, filed as
  drm-cxx#374 (P-86).
- `drmkit-capture`: `snapshot` exports each plane's buffer close-on-exec, as
  the reference does, so a child spawned on another thread during a capture
  does not inherit the descriptor (P-87).

## [0.1.2] - 2026-10-15

### Added

- `drmkit-scene`: `LayerBufferSource::with_mapping`, a provided method that
  runs a closure over a CPU view of the pixels. The scene's composition reads
  through it; its default maps with `map`, so existing sources need nothing.
- `drmkit-scene`: `LayerScene::on_session_paused` and `on_session_resumed`,
  as the reference has them. A pause hands every held buffer back and pauses
  each source and the canvas; a resume rebuilds the allocator's view of the
  output, resumes each source on the new device, makes the canvas again, and
  lifts any suspension. Every caller had to walk the layers itself (P-74).

### Deprecated

- `drmkit-scene`: `OutputSignalling` and `derive_output_signalling` are now
  `OutputSignaling` and `derive_output_signaling`, in the US spelling the
  rest of the API uses. The old names remain as deprecated aliases until
  0.2.

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
- `drmkit-scene-gbm`: the producer's acquire fence covers one frame, as the
  reference hands it over: the acquire that takes it clears it. Every later
  frame got a copy, so the display engine waited on a render long finished,
  and a frame the producer had not fenced was never told apart. A
  framebuffer the kernel refuses reports the kernel's errno rather than
  `EINVAL` (P-67).
- `drmkit-present`: `FrameEconomy::default()` is the same as `new()`; its
  first decision commits in full. The derived default started as though a
  frame had gone out already, so an unchanged first frame was skipped
  (P-67).
- `drmkit-scene-set`: a mirrored layer's plane binding reaches its source,
  as the reference forwards it, and a removed layer's slot is reused, so a
  set whose layers come and go no longer grows by one slot each time
  (P-68).
- `drmkit-present`: `DumbRingSource` forgets its repaint history when the
  session pauses, along with its buffers. The first paint after a resume
  could be asked for a partial repaint of a freshly allocated, blank buffer,
  leaving the rest of it blank on screen; every slot now starts over with a
  full repaint (P-69).
- `drmkit-csd`: seven small departures from the reference, each now as the
  reference has it (P-70):
  - `ShadowCache::blit_into` and `blit_cross_fade` composite source-over
    onto the destination rather than overwriting it;
  - `blit_cross_fade` refuses two keys of different sizes, and its endpoints
    are exactly `blit_into`;
  - `theme_id` hashes each 32-bit field most significant byte first, so it is
    the reference's hash;
  - `Color::from_hex` refuses a sign, so `#+1+2+3` is no longer a color;
  - a noise amplitude past one is full strength;
  - `Surface::export` is close-on-exec and reports a refusal as the new
    `SurfaceError::Export`, with the kernel's errno, where it was `Empty`;
  - `decoration_geometry` no longer overflows, and panics, on a shadow
    extent or title bar past `i32::MAX / 2`.
- `drmkit-input`: `Keyboard::process_key` treats a repeat as a read, as the
  reference does, rather than feeding xkb another press of a key it holds;
  and keymap names with no options take the user's `XKB_DEFAULT_OPTIONS`,
  which an empty options string had been overriding (P-71).
- `drmkit-session`: a seat that cannot be opened is logged as an error, as
  the reference logs it, where it was debug; and a dispatch that fails
  before the seat activates is `SessionError::NoBackend`, the "no seat" a
  caller falls back on, rather than `SessionError::Seat` (P-71).
- `drmkit-scene-sources`: a GBM source that cannot re-allocate on resume
  reports the errno, as the reference does, rather than `Unsupported`
  (P-71).
- `drmkit-present`: `DumbRingSource::new` refuses a zero width, height or
  slot count as the new `PaintError::Zero`, as the reference refuses it with
  `EINVAL`, rather than failing at the first paint (P-72).
- `drmkit-scene-sources`: a dma-buf import tears down on the device file it
  was made on, through a descriptor of its own, rather than through the
  caller's descriptor number. After a session loss the next open can take
  that number, and clearing a `DmaBufSourceCache` -- documented for session
  loss -- removed the new file's framebuffer (P-73).
- `drmkit-input`: `InputError::Seat` says libinput could not enumerate the
  seat through udev, which is what it reports. It said "no such seat, or no
  permission for it", but libinput accepts an unknown seat name and finds no
  devices on it.
- `drmkit-scene-set`: a mirrored layer that falls to composition, or is
  forced there, is blended from the shared source, as the reference blends
  it. It was dropped from every frame, since a shared source could not be
  mapped (P-68).
- `drmkit-dumb`, `drmkit-scene-gbm`: a dumb buffer, and a GBM surface
  source's framebuffers, are torn down on the device file they were made on,
  through a descriptor of their own, as dma-buf imports are since P-73. A
  buffer dropped after a session loss destroyed the buffer of whatever file
  reused its descriptor number (P-75).

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
  (`hotplug`), driver and connector capabilities, color pipelines and tone
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

[Unreleased]: https://github.com/jwinarske/drmkit/compare/v0.1.2...HEAD
[0.1.2]: https://github.com/jwinarske/drmkit/compare/v0.1.1...v0.1.2
[0.1.1]: https://github.com/jwinarske/drmkit/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/jwinarske/drmkit/releases/tag/v0.1.0
