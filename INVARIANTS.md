# Relied-upon invariants

drmkit's copy of the drm-cxx contract file. Headings are numbered and titled
**identically to upstream** — `cargo xtask invariants` fails CI when they
diverge, because an invariant added, removed, renumbered, or retitled upstream
is an API-level event for the port and must not land quietly.

The bodies deliberately differ: each names the Rust source location that owns
the contract and the Rust test that pins it. Where Rust's type system converts
a latent runtime hazard into a compile error, that is noted — but the timing
contracts still need their vkms pins, because ownership cannot express "two
commits deep".

**Upstream baseline:** `jwinarske/drm-cxx` @ `dc2915b` (v2.0.1, PR #231).

Tests suffixed `_vkms` need the VKMS virtual driver (or any modeset card via
`DRMKIT_TEST_CARD`). Unlike the C++ CI, which self-skips them on hosted
runners, drmkit runs them under virtme-ng on every PR — they are
merge-blocking from Phase 0 onward (plan §9, §10).

> **Status:** no invariant is pinned yet. Phase 0 carries only `drmkit-log`
> and `drmkit-time`, neither of which owns a contract. Each entry below names
> the crate and phase that will land its pin; `cargo xtask parity` tracks the
> test files those pins live in.

---

## 1. Deferred buffer release (release after the flip, not after the ioctl)

A buffer acquired from a `LayerBufferSource` is released back to the source only
**after the page-flip that stops scanning it out has completed** — two commits
deep, not when `commit()`'s ioctl returns. Releasing at ioctl-return would let a
producer (V4L2 capture, a GBM ring) overwrite a buffer the display engine is
still scanning out, tearing on hardware whose producer driver attaches no
reservation fence.

Rust ownership makes a premature release a *move* error rather than a latent
tear: the scene holds each `AcquiredBuffer` in a two-deep queue and cannot hand
it back while it is still owned. The **timing** contract is not expressible in
the type system, so the vkms pin carries it.

- **Will be defined:** `drmkit-scene`, `LayerBufferSource::release` /
  `release_with_fence` (phase 3).
- **Will be pinned by:** `defers_release_two_commits_deep_vkms`.

## 2. A failed commit releases that frame's acquisitions before returning

When a real `commit()` is rejected by the kernel, `finalize_frame` releases the
frame's held acquisitions **immediately** (no flip is in flight to gate their
release on) and returns the error. A non-`EACCES` failure does **not** put the
scene into suspended mode. Self-healing producer rings rely on this to reuse
their buffers after a rejected commit; the dropped frame heals on the next.

- **Will be defined:** `drmkit-scene`, `finalize_frame` failure branch
  (phase 3).
- **Will be pinned by:** `commit_failure_releases_acquisitions_vkms`.

## 3. `PageFlip::dispatch` never returns `errc::interrupted`

`dispatch()` retries `EINTR` internally, so a signal landing mid-wait (SIGCHLD,
interval timers, profilers) never abandons a flip still in flight. For a bounded
`timeout_ms > 0`, the remaining budget is recomputed from a steady clock across
retries, so a signal storm cannot stretch the wait past `timeout_ms`. A foreign
source's own fd reads may still observe `EINTR` — that remains the caller's.

The Rust hazard is specific and worth stating: `rustix` event-loop reads return
`Errno::INTR`, and a bare `?` on that read would leak `Interrupted` straight
through the contract. The retry-with-budget loop must swallow it. Budget
recomputation uses `drmkit-time`'s `Clock`, so the storm test drives a
`ManualClock` rather than sleeping.

- **Will be defined:** `drmkit-modeset`, `PageFlip::dispatch` (phase 1).
- **Will be pinned by:** `dispatch_infinite_retries_until_ready`,
  `dispatch_bounded_timeout_honors_budget_under_storm`.

## 4. FB-only fast path: the real commit's `atomic_check` is the arbiter

When only content changed on already-placed layers (FB_ID / damage / fence, with
geometry, format, and modifier byte-identical to the last accepted commit —
detected via `property_hash`, which excludes FB_ID / IN_FENCE_FD), the allocator
reuses the cached plane assignment and **skips the redundant `TEST_ONLY`**. The
real commit's own `atomic_check` stays the sole arbiter: a rejection invalidates
the cached allocation and re-searches on the next frame. The failure mode is a
dropped-then-healed frame, never corruption. `CommitReport.fb_delta_fast_path`
is set and `test_commits_issued` is 0 on such a frame.

Corollary consumers depend on: **`property_hash` isolates content from
placement** — every placement/format property (src/dst rects, rotation, alpha,
zpos, COLOR_*, pixel format, modifier) moves the hash; FB_ID and IN_FENCE_FD do
not. A property misclassified as content would let a real change skip the test.

The C++ can only document the classification. The Rust port carries it as data
with an **exhaustive `match` over the property enum**, so adding a variant
without classifying it fails to compile rather than silently defaulting to
"content" — the one place the port is structurally stronger than the original.

- **Will be defined:** `drmkit-planes`, `is_fb_only_frame` and the
  classification table (phase 2); the reject-invalidates-cache net in
  `drmkit-scene::finalize_frame` (phase 3).
- **Will be pinned by:** `property_hash_isolates_fb_only_content_from_placement`;
  `steady_state_writes_only_fb_id_per_assigned_layer_vkms`;
  `fb_only_fast_path_retests_on_placement_change_vkms`;
  `static_steady_state_vkms`.

## 5. Teardown does not wait on the kernel — drain the last flip first

Destroying a `LayerScene` releases its buffers and framebuffers **without
waiting on the kernel**. If the last real commit armed `DRM_MODE_PAGE_FLIP_EVENT`
and that event has not been dispatched, the flip still references a buffer the
destructor tears down (the RmFB-on-in-flight-FB hazard). Land it first: call
`LayerScene::drain(pf, timeout)` — a no-op when nothing is armed — or dispatch
the event yourself, before the scene goes out of scope.

`Drop` must stay non-blocking; `drain` stays an explicit method and never moves
into `Drop`. The port adds observability the C++ side cannot cheaply provide: a
`tracing::warn!` (and `debug_assert!`) in `Drop` when an armed flip is still
outstanding, so the hazard is loud in development instead of silent until it
tears.

- **Will be defined:** `drmkit-scene`, `LayerScene::drain` and the `Drop` impl
  (phase 3).
- **Will be pinned by:** `drain_lands_pending_flip_before_teardown_vkms`.

## 6. `dumb::Buffer::map` is a zero-cost view of a lifetime mmap

A dumb buffer is mmap'd once at `create()` and the mapping is held for the
buffer's full lifetime and is cache-coherent by kernel construction. `map()` is a
thin wrapper around `data()` / `stride()`; the returned `BufferMapping` guard's
destructor is a **no-op**. Consumers may therefore `map()` per frame with no
syscall cost and write pixels directly — the access mode is recorded on the guard
but ignored at the dumb-buffer level.

In Rust the `Buffer` owns the mapping and `map()` returns a borrow-guarded
`&mut [u8]` view, so the no-op-destructor property is structural rather than
documented. The lifetime-mmap decision is load-bearing for `DumbScanoutSink`
pacing — keep it.

- **Will be defined:** `drmkit-dumb`, `Buffer::map` / `Buffer::data` (phase 1).
- **Will be pinned by:** `default_is_empty_and_drop_is_noop`;
  `dumb_buffer_create_and_ownership`.
