// SPDX-License-Identifier: MIT
//
// Differential oracle for the software compositor's blend arithmetic.
//
// Runs drm-cxx's own CompositeCanvas::blend_into over a spread of inputs and
// prints what comes back, so crates/drmkit-scene/tests/canvas_oracle.rs can be
// written against the reference's answers rather than against hand-derived
// ones. Rounding, the premultiplied convention and the nearest-neighbour
// mapping each have several plausible readings, and a wrong reading usually
// produces a wrong expectation and a matching wrong implementation.
//
// Unlike the scenario runners this needs no device -- the blend is pure.
//
//   g++ -std=c++17 -o blend-oracle blend-oracle.cc \
//       -I$DRM_CXX/src -I$DRM_CXX -I$BUILD -I$BUILD/src \
//       $(for d in $DRM_CXX/third_party/*/include; do echo -I$d; done) \
//       $(pkg-config --cflags --libs libdrm) -L$BUILD/src -ldrm-cxx
#include <drm-cxx/scene/composite_canvas.hpp>
#include <cstdint>
#include <cstdio>
#include <cstring>
#include <vector>

using drm::scene::CompositeCanvas;
using drm::scene::CompositeRect;
using drm::scene::CompositeSrc;

static constexpr std::uint32_t ARGB = 0x34325241; // AR24
static constexpr std::uint32_t XRGB = 0x34325258; // XR24

static std::vector<std::uint8_t> solid(std::uint32_t w, std::uint32_t h, std::uint32_t v) {
  std::vector<std::uint8_t> b(static_cast<std::size_t>(w) * h * 4);
  for (std::size_t i = 0; i < b.size(); i += 4) std::memcpy(b.data() + i, &v, 4);
  return b;
}

static std::uint32_t at(const std::vector<std::uint8_t>& b, std::uint32_t stride,
                        std::uint32_t x, std::uint32_t y) {
  std::uint32_t v = 0;
  std::memcpy(&v, b.data() + (y * stride + x * 4), 4);
  return v;
}

static void run(const char* name, std::uint32_t dw, std::uint32_t dh, std::uint32_t dst_fill,
                std::uint32_t sw, std::uint32_t sh, std::uint32_t src_fill, std::uint32_t fourcc,
                std::uint16_t alpha, CompositeRect srect, CompositeRect drect) {
  auto dst = solid(dw, dh, dst_fill);
  auto pix = solid(sw, sh, src_fill);
  CompositeSrc s{};
  s.pixels = drm::span<const std::uint8_t>(pix.data(), pix.size());
  s.src_stride_bytes = sw * 4;
  s.src_width = sw;
  s.src_height = sh;
  s.drm_fourcc = fourcc;
  s.plane_alpha = alpha;
  CompositeCanvas::blend_into(drm::span<std::uint8_t>(dst.data(), dst.size()), dw * 4, dw, dh,
                              s, srect, drect);
  std::printf("%-34s", name);
  for (std::uint32_t y = 0; y < dh; y++)
    for (std::uint32_t x = 0; x < dw; x++) std::printf(" %08X", at(dst, dw * 4, x, y));
  std::printf("\n");
}

int main() {
  run("opaque_over_opaque",      2,1,0xFF0000FF, 2,1,0xFFFF0000, ARGB,0xFFFF,{0,0,2,1},{0,0,2,1});
  run("transparent_over_opaque", 2,1,0xFF0000FF, 2,1,0x00000000, ARGB,0xFFFF,{0,0,2,1},{0,0,2,1});
  run("half_alpha_over_opaque",  2,1,0xFF0000FF, 2,1,0x80800000, ARGB,0xFFFF,{0,0,2,1},{0,0,2,1});
  run("alpha_33_over_opaque",    2,1,0xFF0000FF, 2,1,0x33330000, ARGB,0xFFFF,{0,0,2,1},{0,0,2,1});
  run("alpha_cc_over_grey",      2,1,0xFF808080, 2,1,0xCC00CC00, ARGB,0xFFFF,{0,0,2,1},{0,0,2,1});
  run("xrgb_padding_ignored",    2,1,0xFF0000FF, 2,1,0x00FF0000, XRGB,0xFFFF,{0,0,2,1},{0,0,2,1});
  run("plane_alpha_8000",        2,1,0x00000000, 2,1,0xFFFFFFFF, ARGB,0x8000,{0,0,2,1},{0,0,2,1});
  run("plane_alpha_ffff",        2,1,0x00000000, 2,1,0xFFABCDEF, ARGB,0xFFFF,{0,0,2,1},{0,0,2,1});
  run("plane_alpha_4000_on_half",2,1,0x00000000, 2,1,0x80402010, ARGB,0x4000,{0,0,2,1},{0,0,2,1});
  run("negative_dst_x",          4,1,0x00000000, 4,1,0xFFFF0000, ARGB,0xFFFF,{0,0,4,1},{-2,0,4,1});
  run("offscreen_dst",           2,1,0xFF0000FF, 2,1,0xFFFF0000, ARGB,0xFFFF,{0,0,2,1},{100,0,2,1});
  run("scale_up_2_to_4",         4,1,0x00000000, 2,1,0xFF112233, ARGB,0xFFFF,{0,0,2,1},{0,0,4,1});
  run("scale_down_4_to_2",       2,1,0x00000000, 4,1,0xFF445566, ARGB,0xFFFF,{0,0,4,1},{0,0,2,1});
  run("src_rect_offset",         2,1,0x00000000, 4,1,0xFF778899, ARGB,0xFFFF,{2,0,2,1},{0,0,2,1});
  return 0;
}
