# drmkit

A Rust port of [drm-cxx](https://github.com/jwinarske/drm-cxx) — a layered
Linux DRM/KMS display framework covering modesetting, plane allocation, scene
composition, and scanout.

[![ci](https://github.com/jwinarske/drmkit/actions/workflows/ci.yml/badge.svg)](https://github.com/jwinarske/drmkit/actions/workflows/ci.yml)
[![vkms](https://github.com/jwinarske/drmkit/actions/workflows/vkms.yml/badge.svg)](https://github.com/jwinarske/drmkit/actions/workflows/vkms.yml)

> **Status: Phase 0.** The workspace, toolchain pin, CI, and drift linters are
> in place, along with the two leaf crates. Nothing here is usable as a display
> library yet. See [`plan.md`](plan.md) for the full porting plan and
> [`docs/phase0-spikes.md`](docs/phase0-spikes.md) for the dependency gap list.

### Phase gates

**Phase 0 — met.** Workspace, pinned toolchain, cross fragments, all CI lanes
green, drift linters negative-tested, and the dependency gap list decided
([`docs/phase0-spikes.md`](docs/phase0-spikes.md)). The one item left open then
— a trivial modeset under vkms — is now met by Phase 1: `drmkit-core` and
`drmkit-dumb` allocate, map, and register framebuffers against the vkms card in
the lane on every push.

**Phase 1 — met, with one carry-over.**

| Item | State |
|---|---|
| `drmkit-core`, `-modeset`, `-dumb`, `-sync`, `-fmt` | done |
| Ported unit tests for these subsystems pass | done — 10 upstream files ported, 1 partial |
| Invariant 3 pinned (`PageFlip::dispatch`) | done |
| Invariant 6 pinned (`Buffer::map`) | done |
| Fuzz target seeded for `IN_FORMATS` | done — ~1.5M execs per CI run |
| Fuzz target seeded for EDID-adjacent parsers | **carried to phase 4** |

The EDID parser lands with `drmkit-display` in phase 4, so its fuzz target
cannot be seeded before the code it targets exists.

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
| `drmkit-core` | Device handles, property cache, atomic requests | 1 ✅ |
| `drmkit-modeset` | Mode selection, page-flip dispatch — **invariant 3** | 1 ✅ |
| `drmkit-dumb` | Dumb buffers, lifetime mmaps — **invariant 6** | 1 ✅ |

| `drmkit-planes` | Registry, layer model, matcher, allocator — **invariant 4** | 2 ✅ |

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
