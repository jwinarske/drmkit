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
| P-14 | — | The `rapid churn` allocator threshold (average under 1.5 test commits per frame) cannot fail on any hardware available. vkms's planes are interchangeable, so a cold search lands first try at exactly one test commit; vc4's 17-candidate CRTC does the same. Measured on a Raspberry Pi 5 with the warm-start cache **disabled**: `rapid churn averaged 1.00 test commits/frame over 60 frames` — identical to the cached number. | Open, and the condition is narrower than first recorded. **The earlier note claiming vc4 gives this case teeth was wrong** and is corrected here by measurement. The scenario moves `planes + 1` layers without changing the layer set, so the assignment is stable and a greedy first try succeeds whatever the plane count. The average can only exceed 1 where the *kernel* refuses a combination the matcher believes is placeable, forcing a second `TEST_ONLY` — which is the Radxa ZERO 3's shape (3 eligible, 2 simultaneously usable), not a question of how many planes there are. The neighbouring `slow drift` case does discriminate on vc4: with the cache disabled it peaks at 2 test commits per frame against a threshold of 1, and fails as it should. |

| P-15 | — | The composition canvas asks to sit one slot above the highest layer, and that number was written to the plane without checking what the plane accepts. On vc4 the overlays advertise `zpos [1, 17]`; an 18-layer scene asked for 19 and the kernel refused **the entire frame** with `EINVAL` — all sixteen correctly programmed layers with it. | **Fixed.** `PropertyStore` now caches the inclusive range of unsigned range properties, `PlanePropertyMap` learns each plane's zpos range, and `emit_layer` clamps zpos — and only zpos — before the baseline diff, so the diff compares what the kernel will be told. Upstream has the same defect: `choose_canvas_zpos` returns `max_explicit + 1` guarded against `INT32_MAX` and nothing else. Filed as [drm-cxx#242](https://github.com/jwinarske/drm-cxx/issues/242). Invisible on vkms, which exposes no zpos property at all. |
| P-16 | — | Every on-device rig took `crtcs().first()`. On vkms that is the only CRTC and the one with the display on it; on a Pi 5 it is an inactive CRTC with no connector, advertising 34 candidate planes against the connected HDMI's 18. Three of the four hardware failures were this, not the library. | **Fixed.** A shared `tests/common::pick_crtc` prefers a CRTC a connected connector is routed to, falls back to the first — so the vkms lane, where nothing reports `Connected`, is unchanged — and honours `DRMKIT_TEST_CRTC`. The rigs now carry the chosen index instead of hardcoding 0, which they were also doing for `force_disable_candidates` and every `build_frame`. |
| P-17 | — | Spatially disjoint layers are placed one group at a time at one test commit each, so a scene of 17 disjoint layers exhausts the default budget of 16 and composites its last layer — with a free, compatible plane sitting unused. `layers_composited` reported that identically to a genuine plane shortage. | **Reported rather than changed.** The budget is doing its job; hiding it was the defect. `CommitReport::budget_exhausted` now distinguishes "raise the budget" from "this device is out of planes". vkms has far fewer planes than the budget, so no lane there can reach it. |
| P-18 | — | The composition canvas is armed on a plane no `TEST_ONLY` ever included. The allocator validates the layer assignment; `compose_unassigned` then picks a free plane for the canvas and `build_plan` adds it. On hardware that lights fewer planes than it advertises the assembled frame is refused, and a refused frame puts *nothing* on screen. | **Fixed.** Reproduced on a Radxa ZERO 3 (rockchip VOP2, RK3566): 3 planes eligible on the CRTC, **2 simultaneously usable** — measured with raw ioctls at 64x64 and 640x480, with and without `zpos` writes, always 2. The fix is in two parts. `TestCommitter` grows `set_extra_plane`, so a committer can arm one more plane in every test; `DeviceCommitter` keeps it out of the disable pass and emits it. The scene then runs a second allocation pass on frames that turn out to composite, holding a plane back for the canvas *and* arming it in each test — so the allocator stops when the kernel accepts the whole frame rather than the frame minus a plane, and drops a layer into the composition instead of proposing something uncommittable. Pinned by `a_frame_never_arms_more_planes_than_the_kernel_will_take_vkms`, which imposes the constraint with a committer that refuses more than two planes, so it reproduces without that board. Both ways of reintroducing the bug — not telling the committer, or skipping the second pass — make it arm 3 planes on a budget of 2. Costs one extra search on compositing frames, so such a frame can spend up to twice the per-search test-commit budget; starving the second search would leave it unable to validate, which is the one thing it is for. Filed as [drm-cxx#244](https://github.com/jwinarske/drm-cxx/issues/244); upstream has the same structure. |

| P-19 | — | A connector reporting no display attached cannot be produced on vkms: every connector it exposes, its writeback one included, reports `Connected`. So the "a disconnected connector is still listed, with no modes" case cannot fail there — verified by injecting a listing that skips disconnected connectors, which the vkms lane passes and the amdgpu run fails. | **Covered off-lane.** amdgpu's writeback connector reports nothing attached once `DRM_CLIENT_CAP_WRITEBACK_CONNECTORS` is set, and a Raspberry Pi 5 has a genuinely unplugged `HDMI-A-2` — the real case rather than a stand-in. Both run the assertion for real; the vkms lane still cannot. The case is grounded in `/sys/class/drm` rather than in the query's own answer, so it cannot pass by agreeing with itself. |

| P-20 | — | The vkms lane has been loading the module with its defaults, and on the kernels these runners ship that means overlay and cursor planes are off. Twelve device rigs reported `1 candidate plane(s)`: plane migration, z-order across planes and the composition fallback were all running against a CRTC with nothing to migrate to, and passing. | **Fixed.** The lane asks for `enable_cursor=1 enable_overlay=1 enable_writeback=1`, falling back to a plain load so a kernel that drops a parameter still boots. `DRMKIT_MIN_PLANES` is what stops it going quiet again: the lane sets it to 2, and a device with fewer fails rather than skips. Locally vkms already defaults every parameter on (kernel 7.1.9), which is why this was invisible from here. |

| P-21 | — | `CrtcColorPipeline`'s `NoPipeline` branch is unreachable on either lane card: vkms exposes `GAMMA_LUT` and amdgpu all three stages, so a CRTC with no colour pipeline at all never occurs. The device cases panicked rather than handling it, which is how it surfaced. | **Covered off-lane.** vc4 on a Raspberry Pi 5 exposes no `DEGAMMA_LUT`, `CTM` or `GAMMA_LUT` on any of its four CRTCs. The cases now assert that *every* CRTC is classified as having none — a real assertion rather than a skip — and misclassifying it fails all five there while passing on both lane cards. **The corpus says this is not a vc4 quirk.** Of 4,129 CRTCs in drmdb, 848 have no colour pipeline at all — the second most common shape — and three partial shapes sit between that and the full stack: 208 `ctm + gamma`, 198 `gamma` alone, 22 `ctm` alone. Every branch of the per-stage refusal is reachable on shipped hardware, which is what makes the design of `CrtcColorPipeline` — each stage refusing on its own rather than the pipeline being refused whole — load-bearing rather than defensive. |
| P-22 | — | Rotation capability differs by more than presence, and one driver cannot show it. Measured across three: vkms advertises `0x3f` (four rotations and both reflects), amdgpu `0xf` (four rotations, no reflect), vc4 `0x35` — **0/180 and both reflects, no 90 or 270**. | Recorded, not a defect. vc4 is the hardware the `rotation_bits` contract exists for: a plane there exposes `rotation` and would silently drop a 90-degree request, which is why the allocator gates on the bits and not on the property. Nothing in either lane would have shown it. |

## Closed

| # | Divergence | Resolution |
|---|---|---|
| P-0 | The port had no modeset path at all — it never wrote `crtc.ACTIVE`, `crtc.MODE_ID` or `connector.CRTC_ID`, so it could not bring up a display on a system that did not already have one lit. | Fixed: `drmkit_scene::Modeset`, emitted on the first commit under `ALLOW_MODESET`, on `drmkit_core::PropertyBlob` for the mode. |
| P-1 | The port issued a `TEST_ONLY` commit on **every** frame where the reference issued one and then never again — 4 against 1 over a 4-frame scenario. | Fixed, in two parts. Nothing outside the allocator's own unit tests ever recorded the committed baseline the FB-only fast path diffs against, so `is_fb_only_frame` could never be true: `finalize_frame` now records it after a successful real commit. And removing a layer threw away the entire warm-start cache, forcing a full search; it now prunes just that layer, as upstream's `forget_layer` does. Pinned by `a_steady_frame_issues_no_test_commit` and `removing_a_layer_does_not_force_a_search`. |
| P-4 | The port wrote the whole property bag on every apply where the reference wrote only what changed: 46/43/35/35 properties against 46/4/5/3 over the scenario's four applies. Every apply now matches the reference exactly. | Fixed. The allocator's per-plane baseline now keeps the full property snapshot, not just a hash, and the apply path writes a property only when it differs from what the kernel took. `FB_ID` and `IN_FENCE_FD` are written unconditionally: `FB_ID` re-attachment is how KMS is told a new frame exists and is what schedules the page-flip event, so diffing it away on a single-buffered source leaves an empty request the kernel accepts and never sends an event for, wedging the flip; `IN_FENCE_FD` is a one-shot the kernel consumes every commit. Planes the kernel already has off are no longer re-disabled. The search's test commits still write everything — a throwaway request proposes a configuration the kernel has not got, so a diff against current state would be testing the wrong thing, and upstream splits into two functions for the same reason. |
| P-2 | The port never wrote `COLOR_ENCODING` or `COLOR_RANGE`. | Fixed. Not by adding `PropTag` entries — upstream's tag list is the same sixteen, and these are written by a separate scene-level pass, because they are **sticky across clients**: whatever the last compositor left is what this one inherits, and the wrong YCbCr matrix or range tints every YUV layer. The enum integers are looked up by name per plane via the new `PropertyStore::enum_value`, since the value a driver assigns to a name is its own business and a hardcoded one silently selects a different variant elsewhere. Written once per plane and then diffed away, matching upstream's `sticky_props_` cache — the cache updates only after a commit the kernel accepted. |
| P-3 | The port emitted the modeset properties only in the apply commit; the reference includes them in the `TEST_ONLY` commits too. | Fixed, and it was worse than first recorded. This is not "the test does not match the apply": while the CRTC is inactive the kernel rejects **every** plane bound to it with `EINVAL`, so the whole search would find nothing placeable and composite the entire scene on the first frame. Upstream carries a `test_preparer_` hook for exactly this and says so. `DeviceCommitter` now takes an optional `Modeset` and prepends it to each test request. Invisible here for the same reason as P-0: fbcon keeps the CRTC lit. |
| P-6 | The port has no equivalent of upstream's `zpos_is_fixed`: a plane whose advertised zpos range is degenerate (`min == max`) is not settable even though the kernel does not flag it `IMMUTABLE`, and writing it returns `EINVAL` — which refuses the whole frame. Upstream skips the write; the port would make it. | **Closed: the hardware is real, and the port is already right.** Not reproducible on the cited hardware — vc4 on a Pi 5 reports `zpos [0,0] flags=immutable range`, degenerate *and* flagged, which `PlanePropertyMap` skips — so this sat open pending a device. The drmdb corpus has two: `sun4i-drm` (Allwinner) advertises `zpos` with `min == max == 0` and leaves it mutable, on 2 of 11,134 planes across 1,195 devices. The port needs no guard, because `clamp_zpos` — added for P-15, and unrelated at the time — already bends a derived zpos into the plane's own range, and a degenerate range admits exactly one value. The clamp subsumes the guard and is strictly wider: upstream's skips only the degenerate case, ours also covers the out-of-range one that P-15 was. Pinned by `a_degenerate_zpos_clamps_to_the_only_value_it_will_take`, which fails if the clamp is ever narrowed to treat a degenerate range as a no-op. |
| P-23 | A cursor plane is tested for `ARGB8888` by `FourCC` equality, so a plane that carries alpha in a different channel order is rejected. The cursor then takes an overlay the compositor wanted, or drops to `drmModeSetCursor` — losing atomic coalescing with the frame, and rotation with it. | **Fixed, and found without the hardware.** Tegra's cursor plane advertises `RGBA8888` (`RA24`) and nothing else. Replaying the drmdb corpus through `select_plane` found it: 4 CRTCs across 2 boards, and the only cursor planes in 1,195 devices that decline `ARGB8888` — 3,407 of 3,578 CRTCs get a dedicated cursor plane, and these were in the difference. `CURSOR_FORMATS` now lists the four 32-bit formats that carry alpha, best first, and the renderer permutes the shadow pixel into whichever the plane took — a permutation, so no precision is lost and no channel invented. `XRGB8888` is deliberately absent: a cursor without alpha is a rectangle, and a test pins that widening the list did not let one through. The reference has the same defect in `plane_supports_argb8888`; filed as [drm-cxx#256](https://github.com/jwinarske/drm-cxx/issues/256). The cursor path was re-run on two drivers after the change — vkms locally and vc4 on a Raspberry Pi 5 — since both take the `ARGB8888` fast path and the risk of widening the list is regressing the case that already worked. 19 device cases and 110 unit cases pass on each. Not amdgpu: the device suites default to `/dev/dri/card0`, which is vkms on that machine, and card1's amdgpu is held by a desktop session. That mislabelling is what `cargo xtask coverage` was built to make impossible to repeat. |
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

## A note on how properties were counted

Two findings above were first written from a `modetest -p` dump and were wrong,
in the same way and for the same reason.

The kernel hides atomic-only properties from clients that have not set
`DRM_CLIENT_CAP_ATOMIC` — `IN_FENCE_FD` and `OUT_FENCE_PTR` among them.
`modetest -p` does not set that cap, so it reports those properties as absent
on hardware that fully supports them. Measured on the same vc4 device, same
boot, minutes apart:

| | planes with `IN_FENCE_FD` | CRTCs with `OUT_FENCE_PTR` |
|---|---|---|
| client sets the atomic cap | 56 | 4 |
| client does not | 0 | 0 |

An absent property is therefore never evidence of a missing driver feature
unless the tool that looked for it was an atomic client. Probe with the cap
set, or read the driver source; do not infer capability from a `modetest`
dump.
