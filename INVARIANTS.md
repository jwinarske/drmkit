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

**Upstream baseline:** `jwinarske/drm-cxx` @ `4a0b64a` (v2.0.1+6, PR #238).

The per-crate `Port of ... @ dc2915b` notes are provenance, not this: they
record the revision each crate was ported from, which does not change when the
baseline moves. `parity/run.sh` reads the hash above to check that whatever
`DRM_CXX` points at is at or past it.

Tests suffixed `_vkms` need the VKMS virtual driver (or any modeset card via
`DRMKIT_TEST_CARD`). Unlike the C++ CI, which self-skips them on hosted
runners, drmkit runs them under virtme-ng on every PR — they are
merge-blocking from Phase 0 onward (plan §9, §10).

> **Status.** All six are pinned end to end.
>
> Each entry says which of its pins are host tests and which run against a
> device.
>
> `cargo xtask invariants` checks that every test named below actually exists,
> so this file cannot drift from the tree the way it did before that check was
> added.

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

- **Defined:** `drmkit-scene`, `LayerBufferSource::release` /
  `release_with_fence`. The commit path that enforces the timing is still to
  come in phase 3.
- **Ownership half pinned by:** a compile-fail doctest on `AcquiredBuffer`.
  Releasing a buffer and then reading it does not compile, so the
  use-after-release this invariant exists to prevent cannot be written. Verified
  by removing the move from the snippet and watching rustdoc report "compiled
  successfully, but it's marked compile_fail".
- **Rotation pinned by:** `release_is_deferred_two_commits_deep` and
  `the_ring_holds_exactly_two_generations`, against `ReleaseQueue` directly —
  no device, no source, no kernel. Verified by breaking the ring to release one
  deep and watching seven cases fail.
- **End-to-end pinned by:** `defers_release_two_commits_deep_vkms`, which
  drives the same ring through a real device: the allocator's `TEST_ONLY`
  commits go to the kernel, and only the finalize answer is supplied.

## 2. A failed commit releases that frame's acquisitions before returning

When a real `commit()` is rejected by the kernel, `finalize_frame` releases the
frame's held acquisitions **immediately** (no flip is in flight to gate their
release on) and returns the error. A non-`EACCES` failure does **not** put the
scene into suspended mode. Self-healing producer rings rely on this to reuse
their buffers after a rejected commit; the dropped frame heals on the next.

- **Defined:** `drmkit-scene`, `ReleaseQueue::commit_failed` and
  `FrameLifecycle::finalize`.
- **Immediate-release pinned by:**
  `a_failed_commit_releases_its_own_acquisitions_at_once` and
  `a_failed_commit_leaves_the_in_flight_generations_alone`. The second matters
  as much as the first: a rejection displaced nothing, so the buffers already on
  screen must stay held and the rotation must not skip a generation.
- **Suspend-on-`EACCES`-only pinned by:**
  `an_ordinary_rejection_releases_but_does_not_suspend` and
  `lost_master_suspends_the_scene`. `FrameLifecycle::finalize` takes the
  kernel's answer as a parameter, so both are host tests. Verified by making
  every failure suspend, which fails the first.
- **End-to-end pinned by:** `commit_failure_releases_acquisitions_vkms` and
  `lost_master_suspends_the_scene_vkms`.

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

- **Defined:** `drmkit-modeset`, `PageFlip::dispatch` and the
  `remaining_budget` helper it delegates the budget to.
- **Pinned by:** `dispatch_infinite_retries_until_ready`,
  `dispatch_bounded_timeout_honors_budget_under_storm`, and the
  `remaining_budget_tests` module.

  The two storm cases drive a real `SIGALRM` cadence at the waiting thread with
  `pthread_kill`, not `setitimer`: the C++ approach posts the signal to the
  process, and under Rust's parallel test harness another case's thread absorbs
  it, leaving the wait uninterrupted. That was verified, not assumed — with
  `setitimer`, both cases passed against a `dispatch` deliberately broken to
  propagate `EINTR`.

  A bad **budget** is a different failure: `dispatch` hangs rather than
  answering wrongly, and no signal-driven test reports a hang reliably. So the
  budget arithmetic is also pinned as a pure function, deterministically, by
  `remaining_budget_tests` — which fails in milliseconds with exact values where
  the storm case could only stall.

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

- **Defined:** `drmkit-planes`, `PropTag::class` and `Allocator::is_fb_only_frame`;
  the reject-invalidates-cache net in `drmkit-scene`'s `FrameLifecycle::finalize`.
- **Classification pinned by:** `only_fb_id_and_in_fence_fd_are_content`, which
  hardcodes the expected set. `property_hash_isolates_content_from_placement`
  derives its expectations from `PropTag::class`, so it proves the hash *honors*
  the classification but cannot prove the classification is right — verified by
  misclassifying `CRTC_W` and watching it stay green while the first failed.
- **Fast path pinned by:** `fb_only_frame_skips_the_test_commit`,
  `a_placement_change_defeats_the_fast_path`, and
  `a_vanished_layer_defeats_the_fast_path`.
- **Reachability pinned by:** `a_steady_frame_issues_no_test_commit` and
  `removing_a_layer_does_not_force_a_search`, which count test commits from
  outside the allocator. The three above set up the committed baseline
  themselves, so they prove the predicate works and cannot show whether
  anything ever satisfies it — for a while nothing did, and the fast path was
  unreachable in production while they stayed green.
- **Steady-state census pinned by:**
  `a_steady_frame_writes_only_the_framebuffer_vkms` and
  `colorimetry_is_written_once_not_every_frame_vkms`. Skipping the test commit
  is only half the saving; a frame that then restates every property has moved
  the cost rather than removed it. The census needs a device because it is a
  claim about a count, and a host test with a synthetic property map would be
  counting the tags it had itself installed.
- **Emission pinned end to end by:** the T7 `warm-start-stability` scenario,
  whose trace is byte-identical to the reference's across all five commits.
- **Reject-invalidates-cache pinned by:**
  `a_rejected_fast_path_frame_invalidates_the_cached_allocation` and
  `an_ordinary_rejection_does_not_invalidate_the_cache`. The second matters as
  much: a rejection *after* a real test says nothing new about the cache, so
  invalidating then would discard a good assignment every time the kernel
  refused for an unrelated reason.
- **End-to-end will be pinned by:**
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

- **Partly defined:** `drmkit-scene`, `ReleaseQueue::drain` and
  `ScenePendingFlip`. `drain` returns every held buffer without waiting on the
  kernel; `ScenePendingFlip` is the tripwire the scene's `Drop` will consult to
  warn when a flip is still armed — the observability the C++ cannot cheaply
  add.
- **Pinned so far by:** `draining_returns_every_held_buffer_oldest_first` and
  `pending_flip_tracks_whether_teardown_is_safe`.
- **End-to-end pinned by:** `drain_lands_pending_flip_before_teardown_vkms`
  and `an_armed_flip_keeps_teardown_unsafe_until_landed_vkms`. The second is
  the hazard itself: draining returns buffers but does **not** wait on the
  kernel, so the flip stays outstanding and teardown stays unsafe until the
  event is dispatched.

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

- **Defined:** `drmkit-dumb`, `Buffer::map` and the `Mapping` view.
- **Pinned by:** `mapping_has_no_destructor`, plus the card-backed
  `create_allocates_maps_and_registers_a_framebuffer_vkms`.

  The no-op-destructor half is machine-checked rather than described:
  `mapping_has_no_destructor` asserts `!needs_drop::<Mapping>()`, so adding a
  `Drop` impl — or a field carrying one — fails immediately instead of quietly
  making `map()` cost something per frame. Verified by adding one.

  Four of the five upstream `BufferMapping` cases test the C++ guard's unmap
  bookkeeping: destructor calls the unmapper once, moves transfer that
  responsibility, move-assignment destroys the target's, self-move is
  harmless. None survive the port, because the guard carries no unmapper —
  there is nothing to unmap for a dumb buffer. `TEST_PARITY.md` records
  `test_buffer_mapping.cpp` as `partial` for that reason; the contract those
  cases protected is what `mapping_has_no_destructor` asserts directly.

  Note that the `drm` crate's own `map_dumb_buffer` does the opposite —
  `mmap` per call, `munmap` on drop — so `drmkit-dumb` establishes the mapping
  once at creation and holds it, going through `drm-ffi` for the allocation
  ioctls.
