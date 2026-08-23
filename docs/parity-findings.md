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
| P-1 | warm-start-stability | The port issues a `TEST_ONLY` commit on **every** frame; the reference issues one on the first frame and then never again, including across a layer removal. 4 tests vs 1 over a 4-frame scenario. | Open — the warm allocation is not being reused. This is an extra ioctl per frame and means the FB-only fast path is not being taken, which is invariant 4's whole subject. |
| P-2 | warm-start-stability | The port never writes `COLOR_ENCODING` or `COLOR_RANGE`; the reference writes both on every plane it programs. | Open — the tags are absent from `PropTag`, so the port cannot express them. |
| P-3 | warm-start-stability | The port emits the modeset properties (`crtc.ACTIVE`, `crtc.MODE_ID`, `connector.CRTC_ID`) only in the apply commit. The reference includes them in the `TEST_ONLY` commit too. | Open — a test commit that binds planes to a CRTC that is not yet active is not testing the configuration that will actually be applied. |

## Closed

| # | Divergence | Resolution |
|---|---|---|
| P-0 | The port had no modeset path at all — it never wrote `crtc.ACTIVE`, `crtc.MODE_ID` or `connector.CRTC_ID`, so it could not bring up a display on a system that did not already have one lit. | Fixed: `drmkit_scene::Modeset`, emitted on the first commit under `ALLOW_MODESET`, on `drmkit_core::PropertyBlob` for the mode. |

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
