# T7 parity findings

Divergences the differential harness has found between `drmkit` and the
reference, and what is being done about each. drm-cxx is the origin of
authority, so unless a row says otherwise the reference's behavior is the
specification and the port is what changes.

Findings that turn out to be *upstream* bugs go to `docs/upstream-findings.md`
and get filed as issues instead.

## Status

Three scenarios in the corpus, all **byte-identical** on both sides:

| Scenario | What it covers |
|---|---|
| `warm-start-stability` | The stability bonus: a layer removed from the middle of the stack must not make the survivors move. |
| `fast-path-classification` | Which frames may skip the `TEST_ONLY` commit — in both directions, including that the fast path returns after an invalidation rather than being lost for good. |
| `eagain-skip-accounting` | A source with no frame ready: the layer holds its plane and its last framebuffer, and the report's identity still balances. |
| `plane-migration` | A changing layer set that forces every overlay onto a different plane: the ordering inside one commit, and z-order on planes that cannot be reordered. |
| `zorder-reorder` | Restacking with the layer set unchanged. Both sides drop the restack on a driver with no plane `zpos` — see [drm-cxx#239](https://github.com/jwinarske/drm-cxx/issues/239); the scenario holds that shared behavior. |

## Open

| # | Scenario | Divergence | Status |
|---|---|---|---|
| P-13 | — | `GbmBuffer::modifier` is unpinned on vkms: its buffers really are linear, so an implementation that ignored the driver and returned zero passes every assertion — verified by injecting exactly that. | Open — needs a driver that reports a non-linear layout. A GPU or vc4 would; vkms cannot. |
| P-6 | — | The port has no equivalent of upstream's `zpos_is_fixed`: a plane whose advertised zpos range is degenerate (`min == max`) is not settable even though the kernel does not flag it `IMMUTABLE`, and writing it returns `EINVAL`. Upstream skips the write; the port would make it. | Open — vkms exposes no zpos property at all, so no scenario here can reach it. Wants a driver that does (vc4's primary is upstream's cited case). |

## Closed

| # | Divergence | Resolution |
|---|---|---|
| P-0 | The port had no modeset path at all — it never wrote `crtc.ACTIVE`, `crtc.MODE_ID` or `connector.CRTC_ID`, so it could not bring up a display on a system that did not already have one lit. | Fixed: `drmkit_scene::Modeset`, emitted on the first commit under `ALLOW_MODESET`, on `drmkit_core::PropertyBlob` for the mode. |
| P-1 | The port issued a `TEST_ONLY` commit on **every** frame where the reference issued one and then never again — 4 against 1 over a 4-frame scenario. | Fixed, in two parts. Nothing outside the allocator's own unit tests ever recorded the committed baseline the FB-only fast path diffs against, so `is_fb_only_frame` could never be true: `finalize_frame` now records it after a successful real commit. And removing a layer threw away the entire warm-start cache, forcing a full search; it now prunes just that layer, as upstream's `forget_layer` does. Pinned by `a_steady_frame_issues_no_test_commit` and `removing_a_layer_does_not_force_a_search`. |
| P-4 | The port wrote the whole property bag on every apply where the reference wrote only what changed: 46/43/35/35 properties against 46/4/5/3 over the scenario's four applies. Every apply now matches the reference exactly. | Fixed. The allocator's per-plane baseline now keeps the full property snapshot, not just a hash, and the apply path writes a property only when it differs from what the kernel took. `FB_ID` and `IN_FENCE_FD` are written unconditionally: `FB_ID` re-attachment is how KMS is told a new frame exists and is what schedules the page-flip event, so diffing it away on a single-buffered source leaves an empty request the kernel accepts and never sends an event for, wedging the flip; `IN_FENCE_FD` is a one-shot the kernel consumes every commit. Planes the kernel already has off are no longer re-disabled. The search's test commits still write everything — a throwaway request proposes a configuration the kernel has not got, so a diff against current state would be testing the wrong thing, and upstream splits into two functions for the same reason. |
| P-2 | The port never wrote `COLOR_ENCODING` or `COLOR_RANGE`. | Fixed. Not by adding `PropTag` entries — upstream's tag list is the same sixteen, and these are written by a separate scene-level pass, because they are **sticky across clients**: whatever the last compositor left is what this one inherits, and the wrong YCbCr matrix or range tints every YUV layer. The enum integers are looked up by name per plane via the new `PropertyStore::enum_value`, since the value a driver assigns to a name is its own business and a hardcoded one silently selects a different variant elsewhere. Written once per plane and then diffed away, matching upstream's `sticky_props_` cache — the cache updates only after a commit the kernel accepted. |
| P-3 | The port emitted the modeset properties only in the apply commit; the reference includes them in the `TEST_ONLY` commits too. | Fixed, and it was worse than first recorded. This is not "the test does not match the apply": while the CRTC is inactive the kernel rejects **every** plane bound to it with `EINVAL`, so the whole search would find nothing placeable and composite the entire scene on the first frame. Upstream carries a `test_preparer_` hook for exactly this and says so. `DeviceCommitter` now takes an optional `Modeset` and prepends it to each test request. Invisible here for the same reason as P-0: fbcon keeps the CRTC lit. |
| P-7 | A layer whose source returned `WouldBlock` was dropped from the frame entirely, so its plane was **disabled**: a source that hiccups for one frame blinked its layer off screen. The reference re-attaches the framebuffer the layer already had. | Fixed. The scene remembers each layer's last framebuffer and re-attaches it when the source has nothing ready, so the layer goes on showing what it put up. A layer starved before it ever produced anything is still absent — there is nothing to hold. |
| P-8 | Holding the plane made the report double-count: the starved layer was in the assignment *and* counted as skipped, so `total = assigned + composited + unassigned + skipped` stopped holding and a compositor watching those counters for dropped frames would see phantom ones. | Fixed by excluding starved layers from `layers_assigned` — they are skipped, not assigned, because nothing new reached the screen. This is the EAGAIN accounting drift the plan lists as a named risk; it appeared the moment P-7 was fixed, which is roughly when it was predicted to. |
| P-9 | Layers were assigned to planes in the order the scene held them rather than bottom-up by zpos. On a card whose planes expose a settable `zpos` that is only a preference. On one whose planes do not — vkms, and plenty of embedded display controllers — the plane's index **is** its stacking order, so a backing store could land on a higher-numbered plane than the overlays it belongs behind and cover them. | Fixed: the search now sorts by zpos before placing, as upstream's `sort_layers_by_zpos` does. It cannot cost a placement — the matcher maximizes cardinality first and uses score only for tie-breaks, so reordering the input changes which valid assignment wins, never whether one is found. |
| P-10 | The scene never CPU-waited on an acquire fence when the assigned plane has no `IN_FENCE_FD`, so on such a plane a fenced buffer was committed with nothing having waited. | Fixed by moving the write out of lowering into `arm_acquire_fences`, a pass that runs **after** allocation — which is where upstream puts it, and why. Until a layer has a plane there is nothing to ask about `IN_FENCE_FD`, so the fence cannot simply be lowered into the property bag with everything else. The pass is driven from the acquisitions, where the `SyncFence` itself lives, so there is no raw descriptor to rebuild a borrow from and no `unsafe`. A failed wait is reported, not fatal: a producer that never signals should not wedge the display on one bad frame. `real` gates the wait alone — a `TEST_ONLY` never scans out. Also fixes `in_fences_armed` and `in_fence_cpu_waits`, which had sat on `CommitReport` for several commits with nothing setting either. |
| P-11 | A buffer's `acquire_fence` was carried through the whole acquire path and then dropped at lowering: nothing ever wrote `IN_FENCE_FD`. An asynchronous producer's half-rendered frame went straight to the screen, with the kernel never told to wait. | Fixed: `LoweringInput` carries the descriptor and `lower_layer` writes it. Borrowed for the commit only — the kernel does not take ownership of `IN_FENCE_FD`, so the fence rides the buffer's lifecycle. Pinned by `an_acquire_fence_reaches_the_in_fence_property` and its negative. Found while building the out-fence support the fence scenario needs, not by a scenario: every source in the tree renders synchronously and leaves the field `None`, so no trace could show it and it would first have appeared as intermittent tearing on the GPU-backed sources. |
| P-5 | The port force-disabled **cursor** planes on every commit. | Fixed: `PlaneRegistry::force_disable_candidates` excludes them, and both the test and apply paths go through it. Upstream carves out the same exception in `disable_unused_planes` — a cursor plane belongs to the cursor path, and clearing it here would erase the cursor on every frame the scene commits. |

### What P-9 looked like from inside the process

Nothing. Every layer was assigned, the counts balanced, the kernel accepted
the commit, and the screen was wrong. No counter in `CommitReport` distinguishes
a correct stacking from an inverted one, because the report records *that* a
layer got a plane, not what that plane sits above.

It is the clearest case so far for diffing against a reference rather than
asserting against expectations: nobody writes the assertion for a property they
have not realised is load-bearing, and on hardware with settable zpos this one
is not.

### A bug P-4's own fix introduced, and the harness caught

Skipping already-off planes made the frame after a layer removal stop
disabling the plane that layer had been using — it would have gone on scanning
out the departed layer's last framebuffer.

The cause was in P-1's fix. `forget_layer` erased the allocator's baseline
entry for the plane, and that entry is the only record that the kernel still
has the plane lit; without it the plane looks already-off and nothing disables
it. Upstream nulls the layer pointer and keeps the entry. That was read here as
a defence against pointer reuse, which generation-tagged `LayerId`s already
rule out — but it does two jobs, and only one of them was ported. The entry is
now kept with its identity cleared, which forces a full property write for the
next layer to land there *and* keeps the plane visible to the disable pass.

Worth noting the sequence: the harness caught this within minutes of the change
that introduced it, in a frame no unit test was looking at. It is now pinned by
`the_plane_a_removed_layer_held_is_disabled`.

### Why P-1 survived a green unit suite

The allocator's fast-path tests call `record_committed` themselves to set up
the baseline, then assert that `is_fb_only_frame` returns true. That proves the
predicate works. It says nothing about whether anything ever satisfies it — and
nothing did. The test supplied the very state whose absence was the bug.

The regression tests count test commits from *outside* the allocator, driving
`build_frame`/`finalize_frame` the way a caller does, which is the only vantage
point from which the gap is visible.

### Why P-0 survived a green vkms lane

Every existing vkms test passed without a modeset, on a host where the CRTC was
already active. Most desktops run a DRM fbdev client that re-enables the CRTC as
soon as the previous master drops it — a `drmModeSetCrtc(…, 0, …)` returns
success, reports `mode_valid=0` on that fd, and a fresh open immediately reports
`mode_valid=1` again. So the ambient state that masked the bug could not be
turned off long enough to expose it.

The tests also never issued a real apply commit: they built a frame, let the
allocator's `TEST_ONLY` commits go to the kernel, and then supplied the finalize
answer themselves. That is the right shape for pinning release invariants, but
it means no test exercised the apply path, and the apply path was where the
modeset was missing. The parity runner is the first code in the tree that
commits a frame for real.

## A note on §4.6 and vkms

The plan words the warm-start scenario as "compositor inserts/re-splits a
backing-store layer while overlays stay put; assert zero overlay
reprogramming". That cannot hold on vkms: its planes expose **no `zpos`
property at all** (only an immutable `type`), so stacking order is fixed by
plane index, and adding a layer below the overlays forces every one of them to
shift up. The reference does exactly that, correctly.

`warm-start-stability.scenario` removes a layer from the middle of the stack
instead. It exercises the same stability bonus — the survivors must not compact
into the hole — without a constraint that dominates the answer. The plan's
wording should be amended to match.
