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
  | cargo xtask coverage --device=rpi5-vc4 --record
```

and thereafter checks a run against it, failing when a case that asserted on
that device no longer does — or has stopped being reported at all, which is
what a renamed or deleted case looks like and is coverage lost just the same.

It reads a run on stdin rather than running the suites itself. How tests reach
a device differs — locally, over ssh to a board, inside the vkms lane — and
none of that changes what a coverage regression means.

The vc4 baseline is the worked example: **97 cases, 6 of them skipping**. Five
are the colour-pipeline cases, which vc4 cannot run because it has no
`DEGAMMA_LUT`, `CTM` or `GAMMA_LUT` on any CRTC ([P-21](parity-findings.md)).
The sixth wants `HOTSPOT_X`, which only a virtualized guest driver has.

## A baseline records the device, not the afternoon

`--record` refuses a run whose skips are about the machine rather than the
hardware:

```
refusing to record: some cases skipped for reasons that are about this run,
not this device

  43 case(s): "holds DRM master"
      stop the compositor, or run from a console: the card is in use
  2 case(s): "needs root"
      re-run as root; these cases write to sysfs
```

This exists because the mistake was made. The device suites default to
`/dev/dri/card0`; on the workstation here that is **vkms**, and amdgpu is
card1. A whole session's worth of "verified on amdgpu" was verified on vkms,
and the run that finally pointed at card1 skipped 43 of 118 cases because the
desktop holds DRM master on the card driving the display.

Recording that would have been worse than not measuring. Each of those 43
would have become an expectation: the case reports `ok`, skips forever, and
the check agrees that it should. A baseline may only contain skips a device
*cannot* do anything about, which is what makes `--audit` below trustworthy.

There is no override flag, on purpose. The remedy is to run the suite where it
can assert — from a console for a card a compositor owns, as root for the
cases that write to sysfs, or in the vkms lane, which is root with no display
server and holds master by construction.

## What the pool covers, and what it does not

One device cannot answer whether an assertion runs anywhere. `cargo xtask
coverage --audit` reads every recorded baseline and answers it:

```
2 device(s): ci-vkms (804 cases), rpi5-vc4 (97 cases)

Asserted on one device and skipped on the others -- lose it and these stop running:
  a_layer_past_the_budget_is_composited_and_says_so_vkms   only on rpi5-vc4
  a_stage_the_crtc_has_takes_a_blob                        only on ci-vkms
  ... and the other four colour-pipeline cases

error: assertions that run on no device in the pool

  the_hotspot_is_left_alone_until_something_is_drawn_vkms
      ci-vkms:  no HOTSPOT_X (expected: not a virtualized driver)
      rpi5-vc4: no HOTSPOT_X (expected: not a virtualized driver)
```

Two useful answers. The six single-device cases are the split doing its job:
vc4 has 17 planes against a budget of 16 and no colour pipeline at all, vkms
has 9 planes and a gamma stage. Each covers what the other cannot, and a green
run on either alone looks complete.

The one that asserts nowhere is a real gap. `HOTSPOT_X` is a property of
virtualized *guest* drivers — virtio-gpu, qxl, vmwgfx — and vkms is a virtual
display, not a guest driver. The case is named `..._vkms` and vkms cannot run
it. Either the pool needs a guest VM, or the case's name is claiming a home it
does not have.

That is the shape [P-13](parity-findings.md) and P-14 already had, found by
accident months apart. Here it is a command.

It distinguishes *skipped elsewhere* from *not run elsewhere*. A crate that
does not cross-build for a target never reports there, and counting that as a
coverage risk would bury the real ones.

## Whether the guards themselves have teeth

Every lane here exists because something was once silently not checked. That
makes the lanes worth the same suspicion: a guard that cannot fail is
indistinguishable from one that passes.

Each was tested by breaking what it guards and confirming it goes red.

| Guard | Verdict |
|---|---|
| `cargo xtask parity` / `invariants` | Teeth. Dropping a mapped test file, or retitling an invariant, names the divergence and exits 1. |
| `cargo xtask lanes` | Teeth. Removing a path the scanner reads prints the line to paste back. |
| `cargo xtask coverage` | Teeth, after two parse bugs were fixed — see below. |
| `DRMKIT_REQUIRE_MASTER` | Teeth. Pointed at a card a compositor holds, the cases fail instead of skipping. |
| `DRMKIT_MIN_PLANES` | **Was measuring the wrong thing.** It counted planes on the *device*; what collapses is candidates on a *CRTC*. Fixed. |
| cross lanes | Real. The qemu runner resolves and 1,336 cases execute under it, rather than a build that resembles a test run. |
| miri lane | Real — 85 cases. **`drmkit-planes` was missing**, though the unsafe policy names it. Added. |
| fuzz lane | Real, seeded, 60s per target. **The corpus was not cumulative** despite saying so. Fixed. |
| oracle tables | Match the reference at tip. **Nothing re-ran them when the baseline moved**; `parity/oracles.sh` now does. |
| `cargo-deny` | Teeth. A wildcard dependency fails `bans` and exits 2. |
| rustdoc | **Was not run at all.** `RUSTFLAGS` carries `-Dwarnings`; `RUSTDOCFLAGS` is separate and was unset, so six broken intra-doc links and an unclosed HTML tag sat unnoticed. Fixed, and now a lane. |
| asan lane | Teeth. A one-past-the-end read is caught as `heap-buffer-overflow` with the frame named. 389 cases, ~9s. `drmkit-input` runs with leak detection off — `input` leaks its boxed `libinput_interface` per context ([Smithay/input.rs#99](https://github.com/Smithay/input.rs/issues/99)) — and overflow detection was confirmed still live under that setting. |
| allocator torture thresholds | Three pin a mechanism; `rapid_churn` pins none — see [P-14](parity-findings.md). |

Two of those were only visible from outside. `DRMKIT_MIN_PLANES` looked correct
on vkms because vkms has one CRTC with every plane on it, so the device-wide
count and the per-CRTC count are the same number; they come apart on 859 of the
1001 atomic devices in the corpus. And the coverage checker reported a passing
test as coverage lost, because a log line landed mid-report and the parser was
anchored at the start of the line.

The method that found most of them: disable the mechanism a check claims to
guard, and see whether the check notices. It is cheap, and it is the only thing
that separates a green run from a green run that means something.

## What this does not do yet

All fifteen device suites report their skips, and `cargo xtask coverage`
gates on them. Two things are still open.

The vkms lane records its coverage and will gate on it, but has no committed
baseline yet. The step adapts: with no baseline it records one and attaches it
to the run, and with one it checks. So the first green run after this produces
the file to commit, and gating starts the moment it lands — no flag day.

New cases are reported, not failed — a baseline goes stale the moment anyone
adds a test, and failing for that would train people to ignore it. Refreshing
is mechanical: the lane uploads its own run as `ci-vkms-coverage`, so

```sh
gh run download <run-id> --name ci-vkms-coverage --dir /tmp/vk
cargo xtask coverage --device=ci-vkms --record < /tmp/vk/vkms-run.txt
```

re-records from exactly the run whose numbers you are looking at, rather than
from a workstation whose card is something else entirely.

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
