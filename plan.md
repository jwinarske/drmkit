# drmkit — drm-cxx → Rust Porting Plan

**Rev 2.0 — 2026-08-22**
**Target source:** `jwinarske/drm-cxx` @ `main` (`4a0b64a`, tag v2.0.1 plus 6, PR #238 merged)
**Snapshot:** 42,831 LOC under `src/` across 18 subsystems · 68 unit tests · 29 integration tests · 3 benchmarks · `INVARIANTS.md` (6 pinned contracts)
**Toolchain target:** Rust **1.94.1** (MSRV, pinned), **edition 2024**
**Supersedes:** `drmkit-porting-plan.md` rev 2026-07-04 (@ `9fe80df`, ~38.9K LOC). This revision carries the full v2.0.0/v2.0.1 surface and re-bases the toolchain policy on 1.94.
**Working name:** `drmkit` (unchanged; `drm-rs` collides with Smithay).

---

## 1. Executive summary

drm-cxx v2.0 is no longer "a KMS library with an allocator" — it is a layered display framework with three distinct spines:

1. **Scene spine** (`scene/`, 18.2K LOC): `LayerScene` + `SceneSet` cross-CRTC orchestration, 13 buffer-source implementations, EAGAIN flow control, EGL Streams with driver-owned plane binding, composition fallback.
2. **Present spine** (`present/`, 2.4K LOC): `ScanoutBackend` + `ScanoutProducer` seam (GBM/GL/VK producers), `BufferRing` with buffer-age, `FrameEconomy` idle suppression, explicit sync (`IN_FENCE_FD`/`OUT_FENCE_PTR`), `DumbScanoutSink` for GPU-less targets.
3. **Allocator spine** (`planes/` + `fmt/`, 4.6K LOC): bipartite matching with warm-start caching and (new in v2.0.1) a stability bonus, `FormatTable` as the single format/modifier eligibility gate, FB-only fast path.

The port remains a **behavioral-parity port, not a redesign**. The C++ tree is the reference implementation; its test suite, benchmark thresholds, and — new since the last plan — `INVARIANTS.md` are the contract. Every one of the six pinned invariants (§6) must have a named Rust test before the subsystem that owns it is declared done.

Success criteria, in order:

1. Rust `Allocator` + `LayerScene` pass the ported allocator/scene unit suite against synthetic `PlaneRegistry::from_capabilities` fixtures — no hardware.
2. Rust `plane_stress` / `allocator_torture` reproduce the C++ perf assertions (`avg test_commits < 1.5` post-warm; `test_commits == 1` steady-state; FB-only fast path frames report `test_commits_issued == 0`) on vkms and at least one real device.
3. The six `INVARIANTS.md` contracts hold under the differential parity harness (T7) — identical scenario scripts through both implementations on the same vkms instance, diffing property-write traces and writeback CRCs.

**Recommended target: v0.1.0-minimal first** (~3.5 months single-engineer; the surface grew ~10% since the last estimate), deferring EGL Streams, V4L2, GStreamer, nvbuf, SceneSet, `present/`, and CSD tiers to follow-on releases. Full parity ≈ 7 months. The plan covers the full surface so nothing gets designed into a corner.

---

## 2. Why Rust 1.94.1 — what the MSRV buys this codebase specifically

Rust 1.94.0 shipped 2026-03-05; 1.94.1 is the point release and the pinned MSRV. Edition **2024** (stable since 1.85) is the workspace edition. The choice is not arbitrary — 1.94 stabilizes several things that map directly onto drm-cxx surface:

| 1.94 feature | Where it lands in the port |
|---|---|
| **29 RISC-V target features stabilized, incl. large parts of RVA22U64 / RVA23U64 profiles** | First-class `riscv64gc-unknown-linux-gnu` CI lane with profile-accurate `-Ctarget-feature` for the VisionFive 2 (JH7110 is RVA20-ish; the stabilization matters for forward targets and for building the feature-detection story now instead of retrofitting). |
| **AArch64 NEON fp16 intrinsics (stable, excl. `f16`-type-dependent)** | `drmkit-display`'s `ToneMapper` LUT inner loop. The C++ side documents the tone mapper as a correctness fallback, not real-time; the Rust port can keep the scalar LUT as the portable path and gate a NEON fp16 SIMD lane behind runtime feature detection on RK3588/SA8155P without nightly. x86 AVX-512 FP16 is stable too but is irrelevant to the target matrix — do not spend effort there. |
| **`<[T]>::array_windows`** | EDID/CTA-861 block iteration in `drmkit-display`, `IN_FORMATS` blob walking in `drmkit-fmt` (fixed-stride record scans become `&[u8; N]` windows — the C++ side had a misaligned-read bug in exactly this parser; const-length windows make the Rust version structurally immune). |
| **`LazyLock::get` / `LazyCell::get`** | The `egl_runtime`-style process singletons (dlopen registries, VADisplay refcount pattern). `get()` lets teardown/diagnostic paths ask "was this ever initialized" without forcing the dlopen — the C++ `std::call_once` registries can't express that. |
| **`Peekable::next_if_map`** | Property-blob and mode-list parsers; minor but real ergonomics in `drmkit-core`. |
| **Cargo `include` config key + TOML 1.1 manifests** | The cross-compilation story: per-target `.cargo/config.toml` fragments (aarch64 GNU, AGL SDK, riscv64) shared across drmkit and sibling ports via `include`, instead of the copy-paste config drift the emb manifests currently paper over. |
| **Closure-capture behavior change (1.94 caveat)** | One-time risk, not a benefit: 1.94 changed precise-capture details around patterns. Irrelevant for greenfield code, but pin CI to 1.94.1 exactly (`rust-toolchain.toml`) so contributor toolchains can't drift and reintroduce pre-1.94 capture assumptions. |

MSRV policy: `rust-version = "1.94.1"` in every crate manifest, enforced by `cargo-msrv verify` in CI. No nightly features anywhere, including tests and benches. Clippy pinned to the 1.94.1 component.

---

## 3. Current-state survey (what is being ported)

Per-subsystem LOC at `dc2915b`, with crate disposition:

| Subsystem | LOC | Rust crate(s) | v0.1.0-minimal |
|---|---:|---|:---:|
| `core/` (Device, Resources, PropertyStore, AtomicRequest incl. `native_handle()`) | 1,341 | `drmkit-core` | ✅ |
| `modeset/` (Mode, ModeList, PageFlip: epoll fd, EINTR-safe budgeted dispatch) | 890 | `drmkit-modeset` | ✅ |
| `dumb/` (dumb buffers, lifetime-mmap `BufferMapping`) | 501 | `drmkit-dumb` | ✅ |
| `sync/` (SyncFence) | 145 | `drmkit-sync` | ✅ |
| `time/` | 78 | `drmkit-time` | ✅ |
| `fmt/` (FormatTable, Modifier, classify/describe, BandwidthClass, rotation_compatible, SAND base-modifier plane match) | 1,148 | `drmkit-fmt` | ✅ |
| `planes/` (Registry + `from_capabilities`, Layer, Output, Allocator, matching, rotation caps, externally-bound exclusion) | 3,418 | `drmkit-planes` | ✅ |
| `display/` (EDID, hotplug, DriverProfile, HdrMetadataCache, color pipeline, ToneMapper) | 3,218 | `drmkit-display` | ✅ |
| `input/` (libinput, xkbcommon, KeyRepeater, log-sink routing) | 1,726 | `drmkit-input` | ✅ |
| `gbm/` | 635 | `drmkit-gbm` | ✅ |
| `session/` (libseat, `TakeDeviceOpts`) | 604 | `drmkit-session` | ✅ (feature) |
| `scene/` — core (LayerScene, canvas, negotiate, commit report, `build_frame_into`/`finalize_frame` split, EAGAIN flow control, stream-pin plumbing) | ~7,500 | `drmkit-scene` | ✅ |
| `scene/` — sources tier 1: Dumb, ExternalDmaBuf (+Ring +Pool, modifier passthrough, per-acquire fence dup), DmaBufSourceCache, GbmBuffer | ~3,500 | `drmkit-scene-sources` | ✅ |
| `scene/` — GbmSurfaceSource, GlCompositor (+zink guard) | ~2,200 | `drmkit-scene-gbm`, `drmkit-gl` | ⬜ |
| `scene/` — EGL Streams (builder, source, capability, egl_runtime, mixing probe, NVIDIA quirks) | ~2,600 | `drmkit-scene-streams` | ⬜ |
| `scene/` — V4L2 camera + decoder (+plane_layout), GstAppsink, nvbufsurface, playlist | ~2,800 | `drmkit-scene-v4l2`, `-gst`, `-nvbuf` | ⬜ |
| `scene/` — SceneSet, partition planner, SharedLayerBufferSource fan-out | ~1,500 | `drmkit-scene-set` | ⬜ |
| `present/` (ScanoutBackend, negotiate, BufferRing, FrameEconomy, Gbm/Gl/Vk producers, DumbScanoutSink + DumbRingSource) | 2,429 | `drmkit-present`, `-present-gl`, `-present-vk` | ⬜ |
| `csd/` (Tier 0 PlanePresenter + Composite/Fb presenters, probe_presenter, OverlayReservation, Renderer, ShadowCache cross-fade, WindowAnim, decoration_geometry) | 4,145 | `drmkit-csd` | ⬜ |
| `cursor/` (per-rotation HW probe, ping-pong atomic buffers, accepted-size probe, theme resolve) | 2,699 | `drmkit-cursor` | ✅ |
| `capture/` (PNG + JPEG/NV12 encoders) | 888 | `drmkit-capture` | ✅ (PNG only) |
| `vulkan/` (VK_KHR_display enum) | 332 | `drmkit-vulkan` | ⬜ |
| top-level (`log.hpp` sink, `buffer_mapping.hpp`) | ~200 | `drmkit-log` (facade over `log`/`tracing`) | ✅ |

**Build-system note that changes the Rust mapping:** the C++ tree dropped its C++23 floor to a **C++17 default with vendored polyfills** (tl-expected, tcb-span, fmt) and a dual-std CI lane. Every polyfill dependency evaporates in Rust: `tl::expected` → `Result`, `tcb::span` → slices, `fmt` → `core::fmt`. The dual-std lane has no Rust analogue and is simply deleted from the CI matrix — one of the few places the port is *less* work than the original.

---

## 4. Delta since the 2026-07-04 plan (`9fe80df` → `dc2915b`, 104 commits)

Everything below is new surface the previous plan did not cover, or covered pre-v2.0-shape. Each item names the Rust destination.

### 4.1 v2.0.0 breaking changes (define the API the port targets)
- `PlaneCapabilities::supports_format_modifier` **removed**; `fmt::FormatTable` (built per-plane in `PlaneRegistry`) is now the sole eligibility gate. The Rust API ports the v2 shape only — there is no legacy path to carry. `FormatTable::supports(fourcc, Modifier)` takes a typed modifier; `bandwidth_class(u64)` takes raw.
- `CompositeCanvas::drm_fourcc()` is now a virtual member (canvas reports its actual allocated format). Rust: trait method on `CompositionTarget`, no static.
- `Allocator::apply(bool test_only = false)`, `Seat::take_device(TakeDeviceOpts)` — default-argument signatures become builder-lite option structs (`#[derive(Default)]` + struct-update syntax), the established pattern from the prior plan.

### 4.2 `drm::present` — entire new spine
`ScanoutBackend`, the `ScanoutProducer` seam, `BufferRing` with buffer-age, `ScanoutTarget` discovery, modifier negotiation across all planes, `FrameEconomy` idle suppression + `FB_DAMAGE_CLIPS` (driver-dependent no-op tolerated), VRR from `DriverProfile`, async flip, page-flip epoll integration, explicit sync fences surfaced in `CommitReport`, `RestorePolicy`, and `DumbScanoutSink` (vsync-paced dumb ring, RGB565 scanout, damage-aware present).

Rust mapping: `drmkit-present` is core-only and **never link-binds** GL/EGL/Vulkan — the C++ side's dlopen discipline maps to `libloading` behind `drmkit-present-gl` / `drmkit-present-vk` feature crates. `ScanoutProducer` is a trait; producers are separate crates so a GPU-less build (`DumbScanoutSink` only) pulls zero graphics deps. `ash` (which loads `libvulkan` at runtime by default) satisfies the "headers present, runtime absent" requirement for VK natively.

### 4.3 EGL Streams tier — port-hostile, quarantine it
The full stream stack: `StreamCapability`/`probe_stream_capability`, `EglStreamBuilder` (sole public constructor), internal `EglStreamSource` with `BindingModel::DriverOwnsBinding`, `ensure_stream_layer_pins` / `arm_stream_layer_planes` in the commit path, allocator exclusion via `Layer::set_externally_bound`, empirical `probe_stream_mixing()` with sticky verdict, `Exclusive`-mixing constraint via `transient_composited_`, NVIDIA quirks (`EGL_DRM_MASTER_FD_EXT`, deferred producer-surface creation, `EGL_NV_output_drm_atomic` first-commit handoff through `AtomicRequest::native_handle()`, `flip_event_data()`), and the `egl_runtime` shared-dlopen registry.

Rust mapping: everything stays inside `drmkit-scene-streams`, but the **hooks it needs must land in `drmkit-scene`/`drmkit-planes` in v0.1.0-minimal**: `BindingModel` on the source trait, `set_externally_bound` on `Layer`, the pin bookkeeping slots, and `AtomicRequest::native_handle()` (an `unsafe fn` returning the raw req pointer — one of the few deliberate raw-pointer leaks in the public API, documented as the NVIDIA-handoff seam). Deferring the hooks would force a scene rewrite later; they are cheap to carry as dormant fields with `test_scene`-level coverage.

### 4.4 SceneSet cross-CRTC orchestration
`SceneSet` (one ioctl per frame across N scenes), `add_layer(SceneSetLayerSpec)` fan-out via `SharedLayerBufferSource`, hole-preserving `add_scene`/`remove_scene`, `NarrowPolicy` (`AutoOnModeset`/`Combined`/`PerCrtc`) with the pure `partition_for_policy` planner, `would_request_modeset()` const peek, zero-engaged commit skip, and the `build_frame_into`/`finalize_frame` split.

Rust mapping: the **split lands in `drmkit-scene` from day one** — `LayerScene::commit` is already a thin orchestrator over it in C++, so porting the split shape first costs nothing and unblocks `drmkit-scene-set` later. `partition_for_policy` is a pure function over slot-state vectors: port it early with proptest coverage (it's exactly the kind of enumerable-state function property testing eats). `SharedLayerBufferSource`'s "one acquire/release pair per participating scene per frame" contract maps to `Arc<dyn LayerBufferSource>` with the documented per-frame-ring exclusion carried verbatim into the rustdoc.

### 4.5 CSD tier — now three presenters
Since the prior plan, `CompositePresenter` and `FramebufferPresenter` joined `PlanePresenter`, plus `probe_presenter` startup selection. `compute_writes` remains a pure slots+surfaces→property-writes function — the unit-test seam carries over directly. `OverlayReservation` idempotent-release semantics (shared-plane Komeda vs partitioned amdgpu/Intel/DCSS/VOP) are documented behavior, not code paths — port the doc comment with the code. `WindowAnim` and `ShadowCache::blit_cross_fade` are value-type/pure-function ports (trivial). All of `drmkit-csd` stays post-minimal.

### 4.6 Allocator: warm-start stability bonus (v2.0.1)
`Allocator::score_pair` now strongly prefers keeping a layer on the plane it held last frame; the matcher maximizes cardinality first, score orders tie-breaks only, so the bonus can never reduce placements or override validity. This is a **semantic contract, not an implementation detail** — the flicker-on-layer-set-change regression it fixes is observable. The Rust matcher must reproduce it, and the T7 differential harness must include the reproducing scenario (compositor inserts/re-splits a backing-store layer while overlays stay put; assert zero overlay reprogramming in the property-write trace).

### 4.7 fmt: SAND base-modifier plane match (PR #231)
Broadcom SAND accepted on a plane advertising the base modifier — a parameterized-modifier family match, not equality. `drmkit-fmt`'s `FormatTable::supports` needs family-aware matching for vendor parameterized modifiers (SAND col-height, AFBC block-size variants). The prior plan's AFBC classifier work already anticipated this; SAND makes it two vendors, which justifies a general `ModifierFamily` abstraction rather than two special cases.

### 4.8 Log routing
libinput/libseat diagnostics route into `drm::log` (tagged), drm-cxx's own output goes through the sink, and the `DRM_ALLOC_DEBUG`/`DRM_EXT_DMABUF_DEBUG` channels are redirectable with env-gated thresholds *not* vetoed by the global level (GStreamer-style per-category). Rust mapping: `tracing` with per-target filters reproduces the per-category-threshold semantics natively (`RUST_LOG=drmkit_planes::alloc=trace` ≡ `DRM_ALLOC_DEBUG`). `drmkit-log` is a thin facade so no-std-ish leaf crates depend on `log` only; the libinput/libseat C-callback capture lives in `drmkit-input`/`drmkit-session` and forwards into `tracing` events. `SeatOptions::log_handler` override maps to an optional boxed callback in the options struct.

### 4.9 Acquire-fence dup (PR #230)
`ExternalDmaBufSource::acquire()` dups the acquire fence per acquire. In Rust this is the natural shape — `OwnedFd` per `AcquiredBuffer`, `try_clone()` at acquire — and the C++ fix is evidence the ownership model the prior plan chose (fences as `OwnedFd`, never raw) was right. Pin it with a test anyway: two consecutive acquires yield independently closeable fds.

---

## 5. Workspace and crate map

```
drmkit/
├── Cargo.toml                  # workspace, edition 2024, rust-version 1.94.1
├── rust-toolchain.toml         # 1.94.1 pinned
├── .cargo/config.toml          # include = [...] per-target fragments (1.94 Cargo)
├── crates/
│   ├── drmkit-sys-shim/        # only if a binding gap forces it; empty by intent
│   ├── drmkit-log/
│   ├── drmkit-time/
│   ├── drmkit-core/            # over smithay `drm` + `drm-ffi`
│   ├── drmkit-modeset/
│   ├── drmkit-dumb/
│   ├── drmkit-sync/
│   ├── drmkit-fmt/             # no_std + alloc capable; ModifierFamily here
│   ├── drmkit-planes/          # allocator, matching, registry; no_std-adjacent core
│   ├── drmkit-display/         # smithay `libdisplay-info` crate for EDID
│   ├── drmkit-input/           # `input` (libinput-rs) + `xkbcommon`
│   ├── drmkit-session/         # libseat via `libseat` crate or minimal sys
│   ├── drmkit-gbm/             # `gbm` crate (Smithay)
│   ├── drmkit-scene/
│   ├── drmkit-scene-sources/
│   ├── drmkit-scene-gbm/       # post-minimal
│   ├── drmkit-gl/              # post-minimal, libloading
│   ├── drmkit-scene-streams/   # post-minimal, libloading
│   ├── drmkit-scene-v4l2/     # post-minimal
│   ├── drmkit-scene-gst/       # post-minimal, gstreamer-rs
│   ├── drmkit-scene-nvbuf/    # post-minimal
│   ├── drmkit-scene-set/      # post-minimal
│   ├── drmkit-present/        # post-minimal
│   ├── drmkit-present-gl/     # post-minimal
│   ├── drmkit-present-vk/     # post-minimal, ash
│   ├── drmkit-csd/            # post-minimal
│   ├── drmkit-cursor/
│   ├── drmkit-capture/        # png; jpeg feature post-minimal
│   ├── drmkit-vulkan/         # post-minimal
│   └── drmkit/                # umbrella re-export, feature-gated
├── xtask/                      # TEST_PARITY lint, vkms runner, trace differ
├── tests-integration/          # T5 vkms/vgem suites
├── benches/                    # plane_stress, allocator_torture, tone_mapper
└── parity/                     # T7 scenario scripts + C++ runner shim
```

Foreign-crate policy (unchanged in spirit from rev 1, re-verified against the v2 surface):

| Need | Crate | Note |
|---|---|---|
| DRM ioctls, atomic req | `drm` + `drm-ffi` (Smithay) | Covers `core/`/`modeset/` floor; drmkit-core wraps, never re-binds. Gaps (e.g. writeback CRC, lease ioctls if needed later) go through `drm-ffi` PRs upstream first, local `drmkit-sys-shim` only as a stopgap. |
| EDID/DisplayID | `libdisplay-info` (Smithay bindings) | Decision carried from rev 1 (reuse over rebind). |
| GBM | `gbm` (Smithay) | |
| libinput | `input` | Log-callback capture needs the raw context; verify the crate exposes `libinput_log_set_handler` — if not, small sys extension. |
| xkbcommon | `xkbcommon` | |
| libseat | `libseat` crate if maintained; else minimal sys module in `drmkit-session` | Verify at Phase 0; the crate ecosystem here is thin. |
| Vulkan | `ash` | Runtime-loaded by default — matches dlopen discipline. |
| EGL/GLES | `libloading` + hand-rolled entry-point sets | khronos-egl exists but link-binds by default; the streams tier's exact entry-point control (dual bootstrap/`eglGetProcAddress` resolution under one init) is easier to reproduce directly, mirroring `egl_runtime`. |
| GStreamer | `gstreamer`/`gstreamer-app`/`gstreamer-video` | Post-minimal. |
| PNG/JPEG | `png`; `jpeg-encoder` or `mozjpeg` (feature) | |
| Property tests / fuzz | `proptest`, `cargo-fuzz` | |
| Error types | `thiserror` per crate; no `anyhow` in library code | `errc`-shaped: each crate's error enum carries the same discriminants the C++ `std::error_code` paths document (EAGAIN-as-flow-control must survive as a typed variant, not an io::Error squint). |

---

## 6. INVARIANTS.md — the contract file (new since rev 1)

The C++ tree now ships `INVARIANTS.md`: six externally-relied-upon contracts, each with its defining source location and pinning test. This is the single most useful artifact for the port and becomes the spine of the Rust test plan. The port maintains its own `INVARIANTS.md` with identical numbering; the xtask parity linter (§9) fails CI if the two files' invariant lists diverge.

| # | Invariant | Rust port consequence |
|---|---|---|
| 1 | **Deferred buffer release** — release only after the page-flip that stops scanout completes (two commits deep), never at ioctl return. | The scene's held-acquisition bookkeeping is a two-deep queue of `AcquiredBuffer`s; Rust ownership makes premature release a *move* error rather than a latent tear, but the timing contract still needs the vkms pin (`DefersReleaseTwoCommitsDeep`). |
| 2 | **Failed commit releases that frame's acquisitions immediately**; non-EACCES failure does not suspend the scene. | `finalize_frame` error branch; pin with `CommitFailureReleasesAcquisitions`. Self-healing producer rings depend on it. |
| 3 | **`PageFlip::dispatch` never returns `errc::interrupted`** — internal EINTR retry with steady-clock budget recomputation. | `rustix` event-loop reads return `Errno::INTR`; the retry-with-budget loop is drmkit-modeset code, pinned by the two EINTR-storm tests. Do not let a `?` on the raw read leak `Interrupted`. |
| 4 | **FB-only fast path: real commit's atomic_check is the arbiter** — `property_hash` isolates content (FB_ID, IN_FENCE_FD) from placement; fast-path frames skip TEST_ONLY, rejection invalidates the cache, failure mode is dropped-then-healed, never corruption. | The hash-classification table is ported as data with an exhaustive test: every property enum variant is asserted content-or-placement, so adding a variant without classifying it is a compile-or-test failure (C++ can only document this; Rust can `match` it exhaustively). |
| 5 | **Teardown does not wait on the kernel** — caller drains the last flip (`LayerScene::drain`) before drop; RmFB-on-in-flight-FB hazard. | `Drop` must stay non-blocking. `drain(&mut self, pf, timeout)` is an explicit method; consider a `debug_assert!`/`tracing::warn!` in `Drop` when an armed flip is outstanding — observability the C++ side can't cheaply add. |
| 6 | **`dumb::Buffer::map` is a zero-cost view of a lifetime mmap.** | `Buffer` owns the mapping; `map()` returns a borrow-guarded `&mut [u8]` view. The lifetime-mmap decision is load-bearing for `DumbScanoutSink` pacing — keep it. |

---

## 7. API translation rules (delta-relevant additions to rev 1)

Rev 1's rules stand (pImpl → plain struct + `Impl`-free layout; `std::error_code` → per-crate `thiserror` enums; `unique_ptr<Base>` → `Box<dyn Trait>`; option-struct defaults → `Default` + struct-update). Additions forced by the v2 surface:

- **`BindingModel` / EAGAIN flow control**: `LayerBufferSource::acquire` returns `Result<AcquiredBuffer, SourceError>` where `SourceError::WouldBlock` is the EAGAIN variant. `binding_model()` is a provided trait method defaulting to `SceneOwnsBinding` — v1-source parity ("none override it") for free.
- **Driver-owned dlopen registries**: `static RUNTIME: LazyLock<Option<EglRuntime>>` with `LazyLock::get` (1.94) available for was-it-loaded diagnostics. `Option` inside, because dlopen failure is a valid resolved state (Mesa-without-EGL must be deterministic `Unsupported`, per the streams capability contract).
- **`CommitReport`**: plain `#[non_exhaustive]` struct; new counters (`layers_skipped_no_frame`, `fb_delta_fast_path`, fence counters) landed additively in C++ and `#[non_exhaustive]` keeps that additive freedom.
- **Per-category log gates**: `tracing` target conventions documented in `drmkit-log`; the "channel-by-name is not vetoed by the default level" semantics fall out of `EnvFilter` directives natively.
- **`decoration_geometry` / `compute_writes` / `partition_for_policy` / `box_downscale_rotated`**: the C++ tree's deliberate pure-function seams port as free functions with `#[cfg(test)]`-free doctests — these are the highest-value early ports because they validate the translation rules with zero unsafe and zero fds.

Unsafe policy (unchanged): confined to sys-boundary modules; every `unsafe` block carries a `// SAFETY:` justification; `clippy::undocumented_unsafe_blocks = "deny"` workspace-wide; `unsafe_op_in_unsafe_fn = "deny"` (edition 2024 default, kept explicit). Miri runs the pure-logic crates (`fmt`, `planes` matching, csd geometry, partition planner) — the fd-bound crates are sanitizer territory (T3), not Miri.

---

## 8. Phasing

Phase gates are merge-blocking; each phase ends with its tests in CI green, including the virtme-ng vkms lane from Phase 0 onward.

| Phase | Scope | Exit gate |
|---|---|---|
| **0** (2 wk) | Workspace, toolchain pin, CI skeleton incl. virtme-ng vkms boot lane, xtask TEST_PARITY linter, `drmkit-log`, `drmkit-time`, foreign-crate verification spikes (libseat crate viability, `input` log-handler exposure, `drm-ffi` gap list). | vkms lane green on a trivial modeset; gap list decided. |
| **1** (3 wk) | `drmkit-core`, `-modeset` (incl. Invariant 3), `-dumb` (Invariant 6), `-sync`, `-fmt` (ModifierFamily incl. SAND/AFBC). | Ported unit tests for these subsystems pass; fuzz targets for IN_FORMATS + EDID-adjacent parsers seeded. |
| **2** (4 wk) | `drmkit-planes`: registry (`from_capabilities` first — it unblocks everything), matching, allocator with warm-start + stability bonus + FB-only fast path (Invariant 4 hash-classification table), externally-bound exclusion hooks. | `test_matching`, `test_plane_allocator`, `test_layer` ports green on synthetic fixtures; proptest invariants (cardinality-first, stability-bonus-never-reduces-placements). |
| **3** (4 wk) | `drmkit-scene` core: LayerScene with `build_frame_into`/`finalize_frame` split, EAGAIN flow control, commit report, canvas/negotiate, dormant stream-pin hooks; `drmkit-scene-sources` tier 1 (fence-dup pin). Invariants 1, 2, 5 vkms pins. | Scene unit suite + the three release-invariant vkms tests green; T7 harness runs its first scenario. |
| **4** (3 wk) | `drmkit-display` (EDID via libdisplay-info, DriverProfile, HDR cache, ToneMapper scalar + NEON fp16 lane), `drmkit-input`, `-session`, `-gbm`, `-cursor`, `-capture` (PNG). | Full minimal-cut unit suite; benchmarks ported (`plane_stress`, `allocator_torture`) and thresholds asserted on vkms. |
| **5** (2 wk) | v0.1.0-minimal hardening: T7 scenario corpus (incl. §4.6 stability scenario), HIL smoke on RK3588 + VisionFive 2, docs, `INVARIANTS.md` (Rust). | **v0.1.0-minimal ship.** |
| **6+** | Post-minimal tiers in dependency order: present spine → scene-gbm/gl → scene-set → v4l2/gst → csd → streams → vulkan/nvbuf. Streams last: highest quirk density, needs NVIDIA HIL (Jetson Orin Nano). | Per-tier: ported tests + tier-specific parity scenarios. Full parity ≈ month 7. |

---

## 9. Test plan (tiers carried from rev 1, counts and additions updated)

- **T0 unit** — port of the 68-file unit suite against synthetic fixtures. `PlaneRegistry::from_capabilities` is the linchpin and lands first in Phase 2.
- **T1 property** — proptest on: matcher (cardinality maximal; stability bonus never reduces cardinality; tie-break determinism), `property_hash` classification (placement-mutations move it, content-mutations don't), `partition_for_policy` (pure slot-state function), `ModifierFamily` matching, `box_downscale_rotated` (energy conservation bounds).
- **T2 fuzz** — untrusted parsers: IN_FORMATS blob (the misaligned-read bug's home), EDID/CTA HDR blocks, V4L2 format negotiation (post-minimal), cursor Xcursor images.
- **T3 Miri + sanitizers** — Miri on pure crates; ASan/TSan test runs for fd-bound crates (TSan interesting only where the C++ used atomics: external ring core, pool, seat — the Rust equivalents should reduce to `Mutex`/channel and mostly retire the question).
- **T4 doctests** — every public item; the pure-function seams get executable examples.
- **T5 integration (virtme-ng vkms/vgem)** — the 29 integration tests, incl. all `_vkms` invariant pins, merge-blocking on every PR from Phase 0. This remains the structural improvement over the C++ CI, which self-skips these on hosted runners.
- **T6 benchmarks** — `plane_stress`, `allocator_torture`, `tone_mapper_bench` with hard threshold assertions; the C++ thresholds are the floor. Criterion for measurement, plus an xtask that replays the C++ `docs/BENCHMARKS.md` methodology so numbers are comparable (`avg test_commits < 1.5` warm; steady-state `== 1`; amdgpu-N/A carve-out honored).
- **T7 differential parity** — scenario scripts driven through both implementations on the same vkms instance; diff property-write traces + writeback CRCs. New required scenarios: warm-start stability (§4.6), EAGAIN skip accounting (`layers_skipped_no_frame` equality), FB-only fast-path frame classification, fence-dup independence.
- **T8 HIL** — self-hosted: RK3588, RK3566, VisionFive 2 (riscv64 native execution), amdgpu desktop, Jetson Orin Nano (streams tier), SA8155P as available. Minimal-cut gate needs RK3588 + VisionFive 2 only.

**TEST_PARITY.md + xtask linter**: machine-checked table mapping every C++ test file → Rust test(s) → status. A new test file appearing upstream without a table entry fails the port's CI, so upstream drift surfaces as red instead of silence. The same xtask diffs the two `INVARIANTS.md` files.

---

## 10. CI matrix

| Lane | Toolchain/target | Notes |
|---|---|---|
| lint | 1.94.1: fmt, clippy (deny undocumented unsafe), cargo-msrv verify, cargo-deny | |
| test x86_64 | 1.94.1 host | T0/T1/T4 |
| vkms | 1.94.1 + virtme-ng pinned kernel (KVM, ~2–3 s boot) | T5, merge-blocking |
| miri | nightly-for-miri only (tool exception, not code) | pure crates |
| fuzz smoke | 1.94.1 + cargo-fuzz short runs | corpus cached |
| cross aarch64 | `aarch64-unknown-linux-gnu`, Cargo config `include` fragment | build + qemu-user T0 |
| cross riscv64 | `riscv64gc-unknown-linux-gnu` + RVA-profile `target-feature` set (1.94 stabilization) | build + qemu-user T0 |
| bench | self-hosted, T6 thresholds | scheduled + release-gating |
| HIL | self-hosted board pool | T8, scheduled |

---

## 11. Decision records

- **DR-1 (carried):** `drmkit` naming; `drm-rs` is taken.
- **DR-2 (carried):** reuse Smithay ecosystem (`drm`, `drm-ffi`, `gbm`, `libdisplay-info`) over rebinding; gaps go upstream first.
- **DR-3 (new):** MSRV = 1.94.1 exactly, pinned via `rust-toolchain.toml`; edition 2024. Rationale in §2. Library-consumer MSRV pressure is nil at this stage (no external consumers yet); revisit the pin only when drmkit itself acquires downstream crates.
- **DR-4 (new):** stream-tier *hooks* (BindingModel, externally-bound, pin slots, `native_handle`) ship in v0.1.0-minimal as dormant surface; stream *implementation* ships last. Avoids a scene rewrite at month 6.
- **DR-5 (new):** EGL via `libloading` + hand-rolled entry points mirroring `egl_runtime`, not khronos-egl. Exact resolution-order and never-link discipline outweigh binding reuse here.
- **DR-6 (new):** `SourceError::WouldBlock` as a typed variant is the EAGAIN contract; never `io::ErrorKind::WouldBlock` in the public scene API.
- **DR-7 (new):** ToneMapper SIMD is aarch64 NEON fp16 only (1.94 stable), runtime-detected; no x86 SIMD lane (target matrix doesn't justify it).
- **DR-8 (new):** the C++17-polyfill/dual-std machinery is explicitly out of scope — no Rust analogue exists or is needed.
- **DR-9 (2026-08-22, phase 0 spike B-1):** `AtomicRequest` exposes its accumulated `(object, property, value)` writes through a safe ordered iterator; `drmkit-scene-streams` replays them into a libdrm `drmModeAtomicReq` for the `EGL_DRM_ATOMIC_REQUEST_NV` handoff. Supersedes DR-4's `unsafe fn native_handle() -> *mut drmModeAtomicReq`, which cannot be built on `drm-ffi` (it materializes no libdrm request object). Keeps libdrm out of `drmkit-core` and out of GPU-less builds; the same view feeds T7's property-write trace differ, so it ships live in v0.1.0-minimal rather than dormant.

---

## 12. Risk register

| Risk | L | I | Mitigation |
|---|---|---|---|
| `drm-ffi` gaps for v2 surface (writeback CRC for T7, fence counters, damage clips) | M | M | Phase 0 gap-list spike; upstream PRs early; `drmkit-sys-shim` stopgap with removal tracked. |
| libseat / libinput-log-handler crate coverage thin | M | L | Phase 0 verification spike; fallback is a ~200-line sys module — bounded. |
| Warm-start + stability-bonus + FB-fast-path interaction subtly diverges | M | H | T7 property-write trace diffing is specifically designed for this; §4.6 scenario is a required corpus member; proptest invariants on the matcher. |
| EAGAIN accounting drift (skip vs drop residual math) | M | M | `layers_skipped_no_frame` equality asserted in T7; unit tests on the residual subtraction. |
| Streams tier NVIDIA quirks unreproducible without HW | H | M (post-minimal) | Jetson Orin Nano HIL before the tier starts; quirk behaviors captured as recorded-interaction tests where possible. |
| 1.94 closure-capture change bites a contributor on an older local toolchain | L | L | `rust-toolchain.toml` pin; cargo-msrv in CI. |
| Upstream drm-cxx keeps moving during the port (104 commits in 3 weeks recently) | H | M | TEST_PARITY + INVARIANTS xtask linters turn drift into red CI; re-baseline cadence: monthly snapshot review, plan-doc rev bump on breaking upstream changes. |
| Benchmark threshold parity on vkms vs real HW | M | M | Thresholds asserted on vkms in CI; HIL confirms on RK3588; amdgpu N/A carve-out carried verbatim. |

---

## 13. Definition of done

**v0.1.0-minimal:** every ✅-row crate published in-workspace; T0–T6 green in CI incl. merge-blocking vkms; Invariants 1–6 pinned by named Rust tests; T7 harness with ≥ 6 scenarios (incl. warm-start stability) diff-clean; benchmarks meet C++ thresholds on vkms; HIL smoke on RK3588 + VisionFive 2; Rust `INVARIANTS.md` and `TEST_PARITY.md` lint-clean.

**v1.0 (full parity):** all 18 subsystems ported; all 68 unit + 29 integration tests mapped and green; streams tier validated on Jetson HIL; T7 corpus covers every tier; no `drmkit-sys-shim` residue (all bindings upstream); public API rustdoc-complete with the six invariants cross-referenced from the items that carry them.
