# drmkit

A Rust port of [drm-cxx](https://github.com/jwinarske/drm-cxx) — a layered
Linux DRM/KMS display framework covering modesetting, plane allocation, scene
composition, and scanout.

> **Status: Phase 0.** The workspace, toolchain pin, CI skeleton, and drift
> linters are in place, along with the two leaf crates. Nothing here is usable
> as a display library yet. See [`plan.md`](plan.md) for the full porting plan
> and [`docs/phase0-spikes.md`](docs/phase0-spikes.md) for the dependency gap
> list.

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

## Crates

| Crate | Purpose | Phase |
|---|---|---|
| `drmkit-log` | Diagnostic surface: level, installable sink, per-category channels | 0 ✅ |
| `drmkit-time` | Monotonic clock abstraction over `CLOCK_MONOTONIC` | 0 ✅ |
| `xtask` | TEST_PARITY / INVARIANTS drift linters | 0 ✅ |

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
