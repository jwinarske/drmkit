# T7 parity findings

Divergences the differential harness has found between `drmkit` and the
reference, and what is being done about each. drm-cxx is the origin of
authority, so unless a row says otherwise the reference's behavior is the
specification and the port is what changes.

Findings that turn out to be *upstream* bugs go to `docs/upstream-findings.md`
and get filed as issues instead.

## Open

| # | Scenario | Divergence | Status |
|---|---|---|---|
| P-2 | warm-start-stability | The port never writes `COLOR_ENCODING` or `COLOR_RANGE`; the reference writes both on every plane it programs. | Open — the tags are absent from `PropTag`, so the port cannot express them. |
| P-3 | warm-start-stability | The port emits the modeset properties (`crtc.ACTIVE`, `crtc.MODE_ID`, `connector.CRTC_ID`) only in the apply commit. The reference includes them in the `TEST_ONLY` commit too. | Open — a test commit that binds planes to a CRTC that is not yet active is not testing the configuration that will actually be applied. |
| P-6 | — | The port has no equivalent of upstream's `zpos_is_fixed`: a plane whose advertised zpos range is degenerate (`min == max`) is not settable even though the kernel does not flag it `IMMUTABLE`, and writing it returns `EINVAL`. Upstream skips the write; the port would make it. | Open — vkms exposes no zpos property at all, so no scenario here can reach it. Wants a driver that does (vc4's primary is upstream's cited case). |

## Closed

| # | Divergence | Resolution |
|---|---|---|
| P-0 | The port had no modeset path at all — it never wrote `crtc.ACTIVE`, `crtc.MODE_ID` or `connector.CRTC_ID`, so it could not bring up a display on a system that did not already have one lit. | Fixed: `drmkit_scene::Modeset`, emitted on the first commit under `ALLOW_MODESET`, on `drmkit_core::PropertyBlob` for the mode. |
| P-1 | The port issued a `TEST_ONLY` commit on **every** frame where the reference issued one and then never again — 4 against 1 over a 4-frame scenario. | Fixed, in two parts. Nothing outside the allocator's own unit tests ever recorded the committed baseline the FB-only fast path diffs against, so `is_fb_only_frame` could never be true: `finalize_frame` now records it after a successful real commit. And removing a layer threw away the entire warm-start cache, forcing a full search; it now prunes just that layer, as upstream's `forget_layer` does. Pinned by `a_steady_frame_issues_no_test_commit` and `removing_a_layer_does_not_force_a_search`. |
| P-4 | The port wrote the whole property bag on every apply where the reference wrote only what changed: 46/43/35/35 properties against 46/4/5/3 over the scenario's four applies. Every apply now matches the reference exactly. | Fixed. The allocator's per-plane baseline now keeps the full property snapshot, not just a hash, and the apply path writes a property only when it differs from what the kernel took. `FB_ID` and `IN_FENCE_FD` are written unconditionally: `FB_ID` re-attachment is how KMS is told a new frame exists and is what schedules the page-flip event, so diffing it away on a single-buffered source leaves an empty request the kernel accepts and never sends an event for, wedging the flip; `IN_FENCE_FD` is a one-shot the kernel consumes every commit. Planes the kernel already has off are no longer re-disabled. The search's test commits still write everything — a throwaway request proposes a configuration the kernel has not got, so a diff against current state would be testing the wrong thing, and upstream splits into two functions for the same reason. |
| P-5 | The port force-disabled **cursor** planes on every commit. | Fixed: `PlaneRegistry::force_disable_candidates` excludes them, and both the test and apply paths go through it. Upstream carves out the same exception in `disable_unused_planes` — a cursor plane belongs to the cursor path, and clearing it here would erase the cursor on every frame the scene commits. |

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
