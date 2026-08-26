#!/usr/bin/env bash
# Run one T7 scenario through both implementations and diff the traces.
#
#   DRM_CXX=/path/to/drm-cxx parity/run.sh parity/scenarios/warm-start-stability.scenario
#
# Needs a vkms node and DRM master. Set DRMKIT_TEST_CARD to point both runners
# at the same card; comparing traces from two different cards is meaningless.
set -euo pipefail

scenario="${1:?usage: run.sh <scenario>}"
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
out="${PARITY_OUT:-${TMPDIR:-/tmp}/drmkit-parity}"
mkdir -p "$out"

cc -shared -fPIC -O2 -o "$out/trace-shim.so" "$root/parity/trace-shim.c" -ldl

# rustix issues syscalls directly on Linux unless told otherwise, and LD_PRELOAD
# cannot see a raw syscall. Without this the Rust trace comes back empty and the
# diff looks clean for the worst possible reason.
RUSTFLAGS="--cfg rustix_use_libc" cargo build --quiet -p drmkit-parity-runner
DRMKIT_TRACE="$out/rust.trace" LD_PRELOAD="$out/trace-shim.so" \
  "$root/target/debug/drmkit-parity-runner" "$scenario"

if [[ -n "${DRM_CXX:-}" ]]; then
  # The reference is the specification, so which revision of it answered has
  # to be known. A checkout that has drifted from the documented baseline
  # produces a diff that looks like a parity result and is not one -- the
  # corpus was for a while being compared against a tree six commits behind,
  # missing the very stability bonus one of its scenarios exists to test.
  #
  # A warning rather than a refusal: comparing against a newer upstream on
  # purpose is how the next baseline gets adopted. DRMKIT_PARITY_ANY_REF
  # silences it for that case.
  baseline="$(grep -oE 'drm-cxx` @ `[0-9a-f]+' "$root/INVARIANTS.md" | head -1 | grep -oE '[0-9a-f]+$')"
  if [[ -n "$baseline" && -z "${DRMKIT_PARITY_ANY_REF:-}" ]]; then
    if ! git -C "$DRM_CXX" merge-base --is-ancestor "$baseline" HEAD 2>/dev/null; then
      echo "warning: $DRM_CXX is not at or past the documented baseline $baseline." >&2
      echo "         Its HEAD is $(git -C "$DRM_CXX" rev-parse --short HEAD 2>/dev/null || echo unknown)." >&2
      echo "         Any diff below compares against a different specification." >&2
    fi
    dirty="$(git -C "$DRM_CXX" status --porcelain 2>/dev/null | grep -c '^ M' || true)"
    if [[ "${dirty:-0}" -gt 0 ]]; then
      echo "warning: $DRM_CXX has $dirty modified file(s) -- the reference is not pristine." >&2
    fi
  fi

  build="$out/drm-cxx-build"
  [[ -d "$build" ]] || meson setup "$build" "$DRM_CXX" -Dexamples=false >/dev/null
  # A build that fails leaves the previous runner in place, or none at all, and
  # either way the diff that follows is meaningless. The README warns about an
  # empty Rust trace for the same reason; this is the other half.
  if ! ninja -C "$build" >/dev/null; then
    echo "error: the reference did not build; there is nothing to compare against." >&2
    exit 1
  fi
  includes=(-I"$DRM_CXX/src" -I"$DRM_CXX" -I"$build" -I"$build/src")
  for d in "$DRM_CXX"/third_party/*/include; do includes+=(-I"$d"); done
  g++ -std=c++17 -O1 -o "$out/runner-cxx" "$root/parity/runner.cc" \
    "${includes[@]}" $(pkg-config --cflags libdrm) \
    -L"$build/src" -ldrm-cxx $(pkg-config --libs libdrm) -Wl,-rpath,"$build/src"
  DRMKIT_TRACE="$out/cxx.trace" LD_PRELOAD="$out/trace-shim.so" \
    "$out/runner-cxx" "$scenario"
  diff -u "$out/cxx.trace" "$out/rust.trace"
else
  echo "DRM_CXX unset: captured the Rust trace only, nothing to diff against." >&2
  echo "  $out/rust.trace" >&2
fi
