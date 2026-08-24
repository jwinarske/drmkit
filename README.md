# drmkit

A Rust port of [drm-cxx](https://github.com/jwinarske/drm-cxx) — a layered
Linux DRM/KMS display framework covering modesetting, plane allocation, scene
composition, and scanout.

[![ci](https://github.com/jwinarske/drmkit/actions/workflows/ci.yml/badge.svg)](https://github.com/jwinarske/drmkit/actions/workflows/ci.yml)
[![vkms](https://github.com/jwinarske/drmkit/actions/workflows/vkms.yml/badge.svg)](https://github.com/jwinarske/drmkit/actions/workflows/vkms.yml)

> **Status: phases 0–3 met, phase 4 started.** Twelve crates, 357 tests, and
> all six [`INVARIANTS.md`](INVARIANTS.md) contracts pinned against a real
> device. A scene can build a frame, allocate planes, commit it, composite
> what it cannot place, and hand buffers back on the right vblank.
>
> Not yet a display library you would ship: EDID, colour management, the
> cursor and capture paths, and the whole present spine are still ahead, and
> nothing has run on non-vkms hardware. See [`plan.md`](plan.md) for the full
> porting plan.

### Phase gates

**Phase 0 — met.** Workspace, pinned toolchain, cross fragments, all CI lanes
green, drift linters negative-tested, and the dependency gap list decided
([`docs/phase0-spikes.md`](docs/phase0-spikes.md)). The one item left open then
— a trivial modeset under vkms — is now met by Phase 1: `drmkit-core` and
`drmkit-dumb` allocate, map, and register framebuffers against the vkms card in
the lane on every push.

**Phase 1 — met.** Its one carry-over closed in phase 4.

| Item | State |
|---|---|
| `drmkit-core`, `-modeset`, `-dumb`, `-sync`, `-fmt` | done |
| Ported unit tests for these subsystems pass | done — 10 upstream files ported, 1 partial |
| Invariant 3 pinned (`PageFlip::dispatch`) | done |
| Invariant 6 pinned (`Buffer::map`) | done |
| Fuzz target seeded for `IN_FORMATS` | done — ~1.5M execs per CI run |
| Fuzz target seeded for EDID-adjacent parsers | done in phase 4 — `edid_blob`, once the parser it targets existed |

The EDID parser landed with `drmkit-display` in phase 4 and the target was
seeded then, closing phase 1's one carry-over.

**Phase 2 — met.**

| Item | State |
|---|---|
| `PlaneRegistry::from_capabilities` first | done — every allocator test runs on synthetic fixtures, no hardware |
| Matching, allocator, warm-start + stability bonus | done |
| Invariant 4 hash-classification table | done — an exhaustive `match`, so an unclassified property does not compile |
| Externally-bound exclusion hooks | done — `is_externally_bound` / `is_pinned` skip placement |
| `test_matching`, `test_plane_allocator`, `test_layer`, `test_output` ported | done |
| Proptest: cardinality-first | done — against a brute-force oracle |
| Proptest: stability bonus never reduces placements | done |

Every card-dependent test is `#[ignore]`d so a developer machine without vkms
stays green, and the lane runs them with `--include-ignored` under
`DRMKIT_REQUIRE_MASTER=1` — which turns "another client holds DRM master" from a
tolerated local condition into a lane failure, so a card test cannot pass in CI
by quietly doing nothing.

**Phase 3 — met.**

| Item | State |
|---|---|
| `LayerScene`: build/finalize split, EAGAIN flow control, commit report | done |
| Invariants 1, 2 and 5 pinned against a device | done — `release_invariants_vkms` |
| Invariant 4 pinned end to end | done — reachability *and* the steady-state write census |
| Real commit path: property map, modeset, emission | done |
| Composition fallback: blend, canvas, plane arming | done |
| `drmkit-scene-sources` tier 1: dumb, external, ring, pool, cache | done — `GbmBufferSource` waits on `drmkit-gbm` |
| T7 differential harness runs its first scenario | done — six scenarios, five diff-clean |

**Phase 4 — started.**

| Item | State |
|---|---|
| `drmkit-gbm` | done |
| `drmkit-display`: tone mapper and colour curves | done — diffed pixel-for-pixel against the reference |
| `drmkit-display`: EDID via libdisplay-info | done — behind the `edid` feature |
| Fuzz target for EDID | done — seeded, closing phase 1's carry-over |
| `allocator_torture` thresholds asserted on vkms | done — four of five cases discriminate there; one cannot ([P-14](docs/parity-findings.md)) |
| `plane_stress` measurement tool | done — `benches/plane-stress`, CSV per frame |
| `drmkit-display`: `DriverProfile` | done — capability probe, never the driver name |
| HDR cache, mode lists, connector/CRTC capabilities, `-input`, `-session`, `-cursor`, `-capture` | ahead |

### The differential harness

The [T7 harness](parity/README.md) runs identical scenario scripts through both
implementations against one vkms instance and diffs the property writes that
reach the kernel, captured at the ioctl boundary so neither side is
instrumented.

Building it found eleven defects in this port — a missing modeset path, a
composition fallback that blinked layers off screen, inverted z-order on
hardware that cannot reorder planes — most of them invisible to every counter
and test in the tree. Five scenarios now produce byte-identical traces; the
sixth is a reproduction for an open upstream question and is expected to
diverge. See [`docs/parity-findings.md`](docs/parity-findings.md).

**Upstream first.** drm-cxx is the origin of authority: anything this port
surfaces is [filed there](docs/upstream-findings.md) rather than fixed only
here, and the default for a bug is to reproduce the C++ behaviour so the two
agree byte for byte. Eight issues filed so far.

**Upstream baseline:** `jwinarske/drm-cxx` @ `dc2915b` (v2.0.1, PR #231).

## What this is

A **behavioral-parity port, not a redesign**. The C++ tree is the reference
implementation; its test suite, benchmark thresholds, and `INVARIANTS.md` are
the contract. Where Rust's type system can turn a documented hazard into a
compile error it does — but the timing contracts still get their vkms pins,
because ownership cannot express "two commits deep".

Two files keep the port honest as upstream moves:

- [`TEST_PARITY.md`](TEST_PARITY.md) — every upstream test file mapped to its
  Rust counterpart and status. A test appearing upstream without a row fails CI.
- [`INVARIANTS.md`](INVARIANTS.md) — the six externally-relied-upon contracts,
  numbered and titled identically to upstream. Divergence fails CI.

Both are enforced by `cargo xtask check`.

A third file records the flow in the other direction:
[`docs/upstream-findings.md`](docs/upstream-findings.md) tracks every defect and
improvement the port surfaces, the drm-cxx issue it was filed as, and what the
Rust side does in the meantime. drm-cxx is the origin of authority — a fix that
lived only here would make the two implementations diverge.

## Crates

| Crate | Purpose | Phase |
|---|---|---|
| `drmkit-log` | Diagnostic surface: level, installable sink, per-category channels | 0 ✅ |
| `drmkit-time` | Monotonic clock abstraction over `CLOCK_MONOTONIC` | 0 ✅ |
| `xtask` | TEST_PARITY / INVARIANTS drift linters | 0 ✅ |
| `drmkit-fmt` | Format modifiers, `IN_FORMATS` tables, bandwidth cost (`no_std`) | 1 ✅ |
| `drmkit-sync` | `sync_file` fences for explicit synchronization | 1 ✅ |
| `drmkit-core` | Device handles, property cache, atomic requests, property blobs | 1 ✅ |
| `drmkit-modeset` | Mode selection, page-flip dispatch — **invariant 3** | 1 ✅ |
| `drmkit-dumb` | Dumb buffers, lifetime mmaps — **invariant 6** | 1 ✅ |
| `drmkit-planes` | Registry, layer model, matcher, allocator — **invariant 4** | 2 ✅ |
| `drmkit-scene` | Scene, commit path, release ring, software composition — **invariants 1, 2, 5** | 3 ✅ |
| `drmkit-scene-sources` | Tier-1 sources: dumb, external dma-buf, ring, pool, cache | 3 ✅ |
| `drmkit-gbm` | GBM allocation and dma-buf export | 4 ✅ |
| `drmkit` | Umbrella re-export, feature-gated | — |

Plus [`parity/`](parity/README.md): the T7 differential harness — an
`LD_PRELOAD` trace shim, a scenario runner per implementation, and the corpus.

The remaining ~25 crates and the phase that lands each are listed in
[`plan.md`](plan.md) §5 and §8.

## Building

Requires Rust **1.94.1** exactly, pinned by `rust-toolchain.toml`; `rustup`
installs it automatically.

```sh
cargo build --workspace
cargo test  --workspace
```

## Drift linters

Both need a drm-cxx checkout, from `--ref` or `$DRMKIT_DRM_CXX`:

```sh
export DRMKIT_DRM_CXX=/path/to/drm-cxx
cargo xtask check        # parity + invariants
cargo xtask parity
cargo xtask invariants
```

They fail on a missing reference rather than passing vacuously — a linter that
silently skips is worse than no linter.

## Cross compilation

Per-target fragments are pulled in through Cargo's `include` config key:

```sh
cargo build --target aarch64-unknown-linux-gnu      # RK3588, RK3566, SA8155P, Orin
cargo build --target riscv64gc-unknown-linux-gnu    # StarFive VisionFive 2
```

## License

MIT, matching drm-cxx. See [`LICENSE`](LICENSE).
