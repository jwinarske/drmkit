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
  build="$out/drm-cxx-build"
  [[ -d "$build" ]] || meson setup "$build" "$DRM_CXX" -Dexamples=false >/dev/null
  ninja -C "$build" >/dev/null
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
