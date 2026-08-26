#!/usr/bin/env bash
# Rebuild the four oracles against $DRM_CXX and diff what they say against
# what is recorded in parity/oracle-expected/.
#
#   DRM_CXX=/path/to/drm-cxx parity/oracles.sh
#
# An oracle runs one pure function from drm-cxx and prints its answers. Those
# answers are baked into Rust tests as literal tables, which is what makes the
# tests parity tests rather than a reading of the specification -- but a baked
# table cannot notice the reference changing underneath it.
#
# So the answers are recorded here too, and this diffs them. A difference means
# the reference's maths moved: check whether the Rust tables need to follow,
# and do not regenerate either one to make a red thing green.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
: "${DRM_CXX:?set DRM_CXX to a drm-cxx checkout}"
out="${ORACLE_OUT:-${TMPDIR:-/tmp}/drmkit-oracles}"
mkdir -p "$out/shim"
ln -sfn "$DRM_CXX/src" "$out/shim/drm-cxx"

build="$out/build"
[[ -d "$build" ]] || meson setup "$build" "$DRM_CXX" -Dexamples=false >/dev/null
ninja -C "$build" >/dev/null

includes=(-I"$DRM_CXX/src" -I"$DRM_CXX" -I"$build" -I"$build/src")
for d in "$DRM_CXX"/third_party/*/include; do includes+=(-I"$d"); done

rc=0
for name in tonemap blend hdr edid; do
  g++ -std=c++17 -o "$out/$name-oracle" "$root/parity/$name-oracle.cc" \
    "${includes[@]}" $(pkg-config --cflags libdrm) \
    -L"$build/src" -ldrm-cxx $(pkg-config --libs libdrm) -Wl,-rpath,"$build/src"
  "$out/$name-oracle" > "$out/$name.now" 2>&1
  if diff -u "$root/parity/oracle-expected/$name.txt" "$out/$name.now"; then
    printf '  same   %s\n' "$name"
  else
    printf '  MOVED  %s -- the reference no longer answers what is recorded\n' "$name"
    rc=1
  fi
done
exit "$rc"
