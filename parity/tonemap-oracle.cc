// SPDX-License-Identifier: MIT
//
// Differential oracle for the tone mapper.
//
// Runs drm-cxx's own ToneMapper over a spread of pixels and prints what comes
// back, so crates/drmkit-display/src/tests.rs can be written against the
// reference's answers rather than against hand-derived ones. Colour maths has
// too many plausible readings — which matrix, absolute or relative scaling,
// where the curve is applied, how the LUT interpolates — for an expectation
// worked out on paper to be worth much.
//
// Needs no device; the mapper is pure.
//
//   g++ -std=c++17 -o tonemap-oracle tonemap-oracle.cc \
//       -I$DRM_CXX/src -I$DRM_CXX -I$BUILD -I$BUILD/src \
//       $(for d in $DRM_CXX/third_party/*/include; do echo -I$d; done) \
//       $(pkg-config --cflags --libs libdrm) -L$BUILD/src -ldrm-cxx

#include <drm-cxx/display/tone_mapper.hpp>

#include <cstdint>
#include <cstdio>

using drm::display::ToneMapCurve;
using drm::display::ToneMapper;

static std::uint64_t px(std::uint16_t r, std::uint16_t g, std::uint16_t b, std::uint16_t a) {
  return static_cast<std::uint64_t>(r) | (static_cast<std::uint64_t>(g) << 16) |
         (static_cast<std::uint64_t>(b) << 32) | (static_cast<std::uint64_t>(a) << 48);
}

static void run(const char* name, const ToneMapper& m, std::uint64_t in) {
  std::printf("%-34s %016llX -> %016llX\n", name, (unsigned long long)in,
              (unsigned long long)m(in));
}

int main() {
  const auto up = ToneMapper::bt709_to_bt2020_pq(100.0F);
  const auto up203 = ToneMapper::bt709_to_bt2020_pq(203.0F);
  const auto down = ToneMapper::bt2020_pq_to_bt709(100.0F, ToneMapCurve::Reinhard);
  const auto down_hable = ToneMapper::bt2020_pq_to_bt709(100.0F, ToneMapCurve::Hable);
  const auto down_none = ToneMapper::bt2020_pq_to_bt709(100.0F, ToneMapCurve::None);
  const auto hlg = ToneMapper::hlg_to_bt709(100.0F);

  run("up_black",        up,        px(0, 0, 0, 0xFFFF));
  run("up_white",        up,        px(0xFFFF, 0xFFFF, 0xFFFF, 0xFFFF));
  run("up_mid_grey",     up,        px(0x8000, 0x8000, 0x8000, 0xFFFF));
  run("up_red",          up,        px(0xFFFF, 0, 0, 0xFFFF));
  run("up_alpha_kept",   up,        px(0x4000, 0x4000, 0x4000, 0x1234));
  run("up203_mid_grey",  up203,     px(0x8000, 0x8000, 0x8000, 0xFFFF));

  run("down_black",      down,      px(0, 0, 0, 0xFFFF));
  run("down_white",      down,      px(0xFFFF, 0xFFFF, 0xFFFF, 0xFFFF));
  run("down_mid",        down,      px(0x8000, 0x8000, 0x8000, 0xFFFF));
  run("down_green",      down,      px(0, 0xC000, 0, 0xFFFF));
  run("down_hable_mid",  down_hable, px(0x8000, 0x8000, 0x8000, 0xFFFF));
  run("down_none_mid",   down_none, px(0x8000, 0x8000, 0x8000, 0xFFFF));

  run("hlg_black",       hlg,       px(0, 0, 0, 0xFFFF));
  run("hlg_half",        hlg,       px(0x8000, 0x8000, 0x8000, 0xFFFF));
  run("hlg_white",       hlg,       px(0xFFFF, 0xFFFF, 0xFFFF, 0xFFFF));
  return 0;
}
