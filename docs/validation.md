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

## What this does not do yet

It does not yet record which assertions actually executed on which device.
That is the other half of the problem — a branch asserted on every board and
executed on none reads as coverage — and it is the next piece.
