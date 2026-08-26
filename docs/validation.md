# Validating on hardware

Every hardware claim in this tree that turned out to be wrong was a claim
nothing measured.

- A test comment said vc4 exposes `FB_DAMAGE_CLIPS`. It exposes it on none of
  its 56 planes, and neither mainline nor the vendor kernel ever attached it
  ([drm-cxx#255](https://github.com/jwinarske/drm-cxx/issues/255)).
- The vkms lane ran for months against **one** plane, because the module
  defaults its overlays off on the runner's kernel. Twelve device rigs were
  exercising plane migration on a CRTC with nowhere to migrate to
  ([P-20](parity-findings.md)).
- A finding said vc4's 17-plane CRTC gave the rapid-churn threshold teeth. The
  number is 1.00 there, and 1.00 with the warm-start cache disabled
  ([P-14](parity-findings.md)).
- `supports_scaling` reads true for every plane on every driver tried, so the
  matcher edge that consumes it has never excluded anything
  ([drm-cxx#252](https://github.com/jwinarske/drm-cxx/issues/252)).
- A cursor fixture sized 4x4 skipped silently on vkms, whose minimum
  framebuffer is 10x10
  ([drm-cxx#254](https://github.com/jwinarske/drm-cxx/issues/254)).

Each was found by accident. Each could have been found by a run.

## What a run does

```sh
cargo xtask validate                      # the whole pool
cargo xtask validate --device=rpi5-vc4    # one device
cargo xtask validate --require=rpi5-vc4,radxa-zero3-rk3566
cargo xtask validate --update-baselines   # accept what changed
```

For each device in `validation/devices.toml` it cross-builds `drmkit-probe`,
copies it over SSH, runs it, and diffs the profile against
`validation/baselines/<id>.json`.

Three outcomes, and the distinction between the last two is the point:

| outcome | meaning |
|---|---|
| `ok` | reached, and it is what the baseline says it is |
| `DRIFTED` | reached, and something about the hardware changed |
| `unreached` | not reached. **Never a pass.** |

A device that is off, off the network, or missing a sysroot is *unreached*. It
does not silently count as fine, and `--require` turns unreached into a
failure for the devices a release depends on. A pool where every board must be
up is a pool nobody runs, so the default is to report the gap and carry on.

## What the profile records

Aggregated by capability, never by object id — ids are assigned per boot on
some drivers, and a baseline that moved every reboot would be ignored within a
week. So it records *how many* planes carry a rotation mask, not which.

- **host** — kernel release, arch, device-tree model. The two things that
  explain a capability changing under you.
- **plane_visibility** — planes visible with neither client capability, atomic
  only, universal only, both. Atomic turns universal planes on by itself on
  every driver tried, which is why a test meaning to check that universal
  planes were requested cannot observe it on an atomic client.
- **min_framebuffer** — the smallest `AddFB2` the driver accepts, per
  dimension. vkms refuses below 10x10; amdgpu and vc4 accept 1x1.
- **planes** — totals by type, and a count per capability rather than a
  boolean, because capabilities are not all-or-nothing: amdgpu attaches damage
  clips to six of its ten planes. Plus histograms of rotation mask and zpos
  range, which is what makes a plane pool heterogeneous or not.
- **crtcs** — colour-pipeline shapes and gamma LUT sizes.
- **connectors** — by name, not id: `HDMI-A-1` is stable across boots and
  matches sysfs. Connection state, mode count, colorspace entries, `max bpc`,
  whether HDR can be signalled.

## Adding a device

Add a `[[device]]` block to `validation/devices.toml`, borrow its sysroot
(see [cross-to-a-board.md](cross-to-a-board.md) — each board needs its own,
since a Pi's glibc is not a Radxa's), and run
`cargo xtask validate --device=<id>`. The first run records the baseline;
read it before committing it, because it is the claim every later run is
checked against.

Say in the `notes` field what the device uniquely exercises. A pool with a gap
should be visible in the inventory and not only in hindsight.

## When a baseline changes

Drift is not automatically wrong — a vendor kernel bump that adds a property
is real. But it changes what the suite covers, so it is a reviewed change:
re-run with `--update-baselines` and say in the commit **what changed on the
hardware and why**, the same way a parity finding is recorded.

## The corpus, where the lab has no board

The lab has the devices we bought. [drmdb](https://drmdb.emersion.fr) has the
devices people own -- around 1,200 `drm_info` dumps across 50 drivers,
refreshed weekly -- and `validation/drmdb` replays every one of them through
drmkit's own capability logic.

```
cargo run -p drmkit-drmdb -- drmdb-data
```

The `drmdb` workflow does this on a schedule and on pull requests that touch
the logic the rules cover. It caches the snapshot by ISO year-week, so a
failing scan can be re-run against the same corpus rather than the next one.

The distinction that matters is between a **rule** and a **survey**.

A rule is an assumption this tree acts on. It gates: hardware that breaks one
is a defect here, not an observation about that hardware. The reference's
equivalent scan is intelligence-only; this one fails the job.

That gating is verified rather than asserted: a dump with `SRC_W` stripped
from one atomic plane makes the rule fail and the scanner exit non-zero, and
an empty corpus directory exits non-zero rather than reporting that every rule
passed with nothing to check.

A survey is a distribution, printed and never gated -- but printed *whole*.
An early version truncated each survey at fourteen rows, and the two planes
that closed [P-6](parity-findings.md) were in the tail: every visible row
agreed, so the histogram read as unanimous when it was not. A survey now
summarises what it dropped.

P-6 is what this is for. A guard upstream carries, that no board here could
reproduce, sitting open for want of hardware. The corpus has it: two Allwinner
boards, 2 planes out of 11,134. It turned out the port was already right --
for a reason that had nothing to do with the guard -- but that is a thing
worth knowing rather than assuming, and no lane, lab or otherwise, was going
to say so.

## Which assertions actually ran

A device case that cannot reach what it is named for returns early, and the
harness reports it as `ok`. That is right — the case is about hardware this
board does not have — but it means a green run carries no information about
how much of it ran. There are **114 such bail-outs across 118 device cases**,
so on any board an unknown fraction of a passing run asserted nothing.

`drmkit_testkit::skipped` prints a line naming the case and the reason, taking
the case's name from the harness's own thread so it cannot drift out of sync
with the function it labels. Pointing the colour-pipeline suite at a card that
does not exist shows what the count was hiding:

```
DRMKIT-SKIP  a_stage_the_crtc_has_takes_a_blob             no DRM device could be opened
DRMKIT-SKIP  an_apply_writes_only_what_was_staged          no DRM device could be opened
...
test result: ok. 5 passed; 0 failed
```

Five passed, nothing asserted. On amdgpu the same suite reports no skips at
all, which is the distinction the count alone cannot make.

It does not change what the harness reports: a skip is still not a failure,
because a board without a gamma stage is not a board with a bug.

What *is* a failure is that changing without anyone noticing. `cargo xtask
coverage` records, per device, which cases asserted and which bailed and why:

```sh
cargo test --workspace --all-features -- --ignored --nocapture 2>&1 \
  | cargo xtask coverage --device=localhost-amdgpu --record
```

and thereafter checks a run against it, failing when a case that asserted on
that device no longer does — or has stopped being reported at all, which is
what a renamed or deleted case looks like and is coverage lost just the same.

It reads a run on stdin rather than running the suites itself. How tests reach
a device differs — locally, over ssh to a board, inside the vkms lane — and
none of that changes what a coverage regression means.

The amdgpu baseline is the worked example: **118 cases, 4 of them skipping**.

```
a_layer_past_the_budget_is_composited_and_says_so_vkms
    9 candidate plane(s) does not exceed the budget of 16
an_ordinary_drm_change_is_read_and_discarded
    writing /sys/class/drm/card0/uevent needs root (try sudo)
a_property_that_merely_looks_like_hotplug_is_not_one
    writing /sys/class/drm/card0/uevent needs root (try sudo)
the_hotspot_is_left_alone_until_something_is_drawn_vkms
    no HOTSPOT_X (expected: not a virtualized driver)
```

The first is [P-17](parity-findings.md) stated as a number: amdgpu has 9
planes and the budget is 16, so that case has never asserted anything here.
Two need root. One wants a virtualized driver. None is a defect, and all four
were previously indistinguishable from a case that ran.

## What the pool covers, and what it does not

One device cannot answer whether an assertion runs anywhere. `cargo xtask
coverage --audit` reads every recorded baseline and answers it:

```
2 device(s): localhost-amdgpu (118 cases), rpi5-vc4 (97 cases)

Asserted on one device and skipped on the others -- lose it and these stop running:
  a_layer_past_the_budget_is_composited_and_says_so_vkms   only on rpi5-vc4
  a_stage_the_crtc_has_takes_a_blob                        only on localhost-amdgpu
  ... and the other three colour-pipeline cases
```

That is the split doing its job. vc4 has 17 planes against a budget of 16 and
no colour pipeline at all; amdgpu has 9 planes and all three colour stages.
Each board covers exactly what the other cannot, and a green run on either
alone would look complete.

The audit fails on the cases that assert **nowhere**:

```
  a_property_that_merely_looks_like_hotplug_is_not_one
      localhost-amdgpu: writing /sys/class/drm/card0/uevent needs root
  an_ordinary_drm_change_is_read_and_discarded
      localhost-amdgpu: writing /sys/class/drm/card0/uevent needs root
  the_hotspot_is_left_alone_until_something_is_drawn_vkms
      localhost-amdgpu: no HOTSPOT_X (expected: not a virtualized driver)
      rpi5-vc4:         no HOTSPOT_X (expected: not a virtualized driver)
```

Three assertions that have never checked anything. Two want a run as root; the
third wants a virtualized driver, which vkms has and neither of these boards
does. This is the shape [P-13](parity-findings.md) and P-14 already had —
found by accident, months apart. Here it is a command.

It distinguishes *skipped elsewhere* from *not run elsewhere*. A crate that
does not cross-build for a target never reports there, and counting that as a
coverage risk would bury the real ones.

## What this does not do yet

All fifteen device suites report their skips, and `cargo xtask coverage`
gates on them. Two things are still open.

The vkms lane records its coverage and will gate on it, but has no committed
baseline yet. The step adapts: with no baseline it records one and attaches it
to the run, and with one it checks. So the first green run after this produces
the file to commit, and gating starts the moment it lands — no flag day.

That lane matters twice over. It is where [P-20](parity-findings.md) hid,
twelve rigs exercising plane migration on a CRTC with nowhere to migrate to,
green throughout. And vkms is the only virtualized driver in the pool, so it
is the only thing that can run the `HOTSPOT_X` case at all — one of the three
the audit currently reports as asserting nowhere.

Its baseline will cover a larger set than the boards': the lane runs
`--include-ignored`, so unit cases are recorded alongside device ones. That is
harmless — a unit case never skips — but it inflates the audit's *unmeasured*
count, which counts cases one device ran and another never attempted.

And baselines exist for the devices reachable today. The rest of the pool
needs an entry in `validation/devices.toml` and a first recorded run each.
