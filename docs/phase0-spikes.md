# Phase 0 verification spikes — foreign-crate gap list

Phase 0's exit gate is "vkms lane green on a trivial modeset; **gap list
decided**" (plan §8). This is the gap list.

Every finding below was verified against the published crate sources, not
inferred from documentation. Versions are those resolvable on 2026-08-22.

| Crate | Version | Verdict |
|---|---|---|
| `drm` / `drm-ffi` (Smithay) | 0.15.0 / 0.9.1 | Adopt. Two gaps, neither blocking the minimal cut. |
| `input` / `input-sys` | 0.10.0 / 1.19.0 | Adopt, with a ~20-line shim. See A-1. |
| `libseat` / `libseat-sys` | 0.2.4 / 0.2.0 | Adopt. Maintained; the plan's "thin ecosystem" risk did not materialize. |
| `xkbcommon` | 0.9.0 | Adopt. |
| `gbm` (Smithay) | 0.18.0 | Adopt. Oldest of the set (2024-12); watch, do not replace. |
| `libdisplay-info` | 0.3.0 | Adopt (DR-2). |
| `ash` | 0.38 | Adopt. Runtime-loads `libvulkan`, matching the dlopen discipline. |
| `rustix` | 1.1.4 | Adopt. Already in use by `drmkit-time`. |

---

## A. libinput log routing

### A-1 — `libinput_log_set_handler` is deliberately unavailable · **confirmed gap, bounded**

Plan §12 rates this risk M/L with a "fallback is a ~200-line sys module"
mitigation. The reality is narrower and worth recording precisely.

`input` 0.10.0 exposes no logging API at all. `input-sys` 1.19.0 does not
either — and not by omission. Its `build.rs` line 88 reads:

```rust
.blocklist_function("libinput_log_set_handler")
```

with `libinput_log_set_priority` and `libinput_log_priority` left allowlisted
and generated. The reason is the callback signature:

```c
void (*libinput_log_handler)(struct libinput *, enum libinput_log_priority,
                             const char *format, va_list args);
```

bindgen cannot express `va_list` portably, so the one function that takes such
a callback is cut and the rest of the family is kept.

**Consequence for the port.** Plan §4.8 requires libinput diagnostics to route
into the drmkit sink. That needs three things, only the first of which is
missing:

1. an `extern "C"` declaration of `libinput_log_set_handler` — roughly ten
   lines in `drmkit-input`, against a symbol already linked by `input-sys`;
2. `libc::vsnprintf` to render the `va_list` into a buffer inside the trampoline
   — this is the actual work, and the reason the shim cannot be pure-`input-sys`;
3. `libinput_log_set_priority`, which `input-sys` already provides.

**Decision.** ~20 lines in `drmkit-input`, not a 200-line sys module and not a
fork of `input-sys`. It is `unsafe` at the `va_list` boundary and gets a
`// SAFETY:` block plus an ASan run (T3). Do not send this upstream to
`input-sys`: the blocklist is correct for a bindgen crate, and the fix belongs
on the consumer side.

---

## B. `drm-ffi` / `drm` gaps

### B-1 — no libdrm `drmModeAtomicReq` object exists to hand to NVIDIA · **confirmed gap, affects DR-4**

`drm_ffi::mode::atomic_commit` (`src/mode.rs:758`) takes four flat slices and
builds the kernel `drm_mode_atomic` struct on the stack at call time:

```rust
pub fn atomic_commit(fd, flags, objs: &mut [u32], prop_counts: &mut [u32],
                     props: &mut [u32], values: &mut [u64]) -> io::Result<()>
```

There is no persistent libdrm-side request object anywhere in the stack —
`drm::control::atomic::AtomicModeReq` is a pure-Rust accumulator of those
slices, not a `drmModeAtomicReq`.

`EGL_NV_output_drm_atomic` requires a `drmModeAtomicReq*` for the first-commit
handoff (plan §4.3). So `AtomicRequest::native_handle()` — the deliberate
raw-pointer leak DR-4 says must ship dormant in v0.1.0-minimal — **has nothing
to return**. The hook as specified cannot be built on this stack.

**Decision.** DR-4 stands, but its *shape* changes and this must be settled
before `drmkit-core` freezes its atomic API in Phase 1, not at month 6. Three
options, to be chosen at the start of Phase 1:

- **(a)** `drmkit-scene-streams` links libdrm directly and owns a parallel
  `drmModeAtomicReq` for the handoff commit only. Contained, duplicative, and
  keeps `drmkit-core` clean. Current preference.
- **(b)** Upstream a `drmModeAtomicReq`-backed request type into `drm-ffi`.
  Correct long-term, slow, and a hard sell for a single consumer's NVIDIA path.
- **(c)** `drmkit-sys-shim` builds the libdrm request. The stopgap DR-2 wants
  to avoid, and the one v1.0's definition of done forbids leaving behind.

Whichever wins, the *dormant surface* Phase 1 must carry is a seam that can
express (a) — an escape hatch on the request type, not literally
`native_handle() -> *mut drmModeAtomicReq`.

### B-2 — no writeback capture or CRC path · **confirmed gap, T7 dependency**

`drm-0.15.0` knows writeback only as a connector *type*
(`control/connector.rs`); there is no writeback-job or framebuffer-capture
helper. `drm-ffi-0.9.1` has no CRC ioctl and no `crtc_get_sequence` /
`crtc_queue_sequence`.

T7's differential parity harness is specified as "diff property-write traces
**+ writeback CRCs**" (plan §9). Half of that has no binding.

**Decision.** Not blocking before Phase 5, but it is on the critical path for
the v0.1.0-minimal gate, which requires ≥6 diff-clean T7 scenarios. Two
mitigations, pursued in parallel starting Phase 3:

- property-write trace diffing alone covers the §4.6 warm-start stability
  scenario, the EAGAIN accounting scenario, and the FB-only fast-path
  classification scenario — three of the six — with no new bindings at all;
- the writeback path gets an upstream `drm-ffi` PR early (DR-2 order), with
  `drmkit-sys-shim` as the tracked stopgap if it has not landed by Phase 5.

### B-3 — atomic commits cannot carry a caller cookie · **investigated, not a gap**

`drm_ffi::mode::atomic_commit` zeroes `drm_mode_atomic::user_data` via
`..Default::default()`, and `drm::control::PageFlipEvent` exposes only
`{frame, duration, crtc}`. That looks like it would break flip-to-commit
correlation, which Invariants 1 and 5 depend on.

It does not. drm-cxx does not use a per-commit cookie either: `page_flip.cpp`
passes the `PageFlip*` itself as `user_data` and reads the CRTC from the v2
handler's `crtc_id` argument (`page_flip.cpp:43-50`). With at most one commit
in flight per CRTC — which is drm-cxx's model and stays drmkit's — CRTC
identity is exactly as much correlation as the C++ has.

`PageFlipEvent` supplies `crtc`, `frame`, and a timestamp. Parity, no gap.

### B-4 — `IN_FENCE_FD`, `OUT_FENCE_PTR`, `FB_DAMAGE_CLIPS` are absent · **not a gap**

None of these appear as named constants in either crate. They do not need to:
DRM properties are resolved by name against the driver's property list at
runtime, which is what `drmkit-core`'s `PropertyStore` does and what the C++
`PropertyStore` already does. Absence from the binding is expected, not
missing.

---

## C. Decisions carried into Phase 1

1. **Settle B-1's option (a)/(b)/(c) before `drmkit-core`'s atomic request type
   is written.** It is the one finding here that changes an API the minimal cut
   ships.
2. `drmkit-input` owns a ~20-line `libinput_log_set_handler` shim with
   `vsnprintf` (A-1); no `input-sys` fork, no upstream PR.
3. Open the `drm-ffi` writeback/CRC conversation upstream during Phase 3, so
   Phase 5 is not the first time it is raised (B-2).
4. `gbm` 0.18.0 is the least recently updated dependency in the set. Not a
   problem today; revisit if `drmkit-gbm` (Phase 4) finds it lacking rather
   than assuming a fork.

---

## D. Security note carried out of Phase 0

**EDID strings reach the log, and EDID is attacker-influenced.** Monitor names
and serial strings come off the wire from whatever display is plugged in; a
hostile or malformed EDID can carry newlines or ANSI escape sequences. The
built-in `drmkit-log` sink writes records verbatim to a terminal, so such a
string can forge log lines or move the cursor. drm-cxx has the same property.

**Decision.** Sanitize where the untrusted string *enters* a record, not in the
sink — once a record is one formatted string, the sink cannot tell a monitor
name from a driver name. `drmkit-display` (phase 4) owns escaping for EDID.
Recorded in `drmkit-log`'s crate documentation so the phase 4 author inherits
the constraint rather than rediscovering it.
