// SPDX-License-Identifier: MIT
//
// Differential oracle for HDR static metadata.
//
// Runs drm-cxx's own `serialize_hdr_metadata` and `hdr_metadata_hash` over a
// spread of sources and prints the bytes and the hash, so the Rust can be
// diffed against the reference's output rather than against a reading of
// CTA-861.3. Two things here are easy to get wrong in a way no display would
// reveal: the green/blue/red reordering, and whether the struct's tail padding
// reaches the hash. Both are byte-visible and neither is visible on screen.
//
// Needs no device -- both functions are pure.
//
//   g++ -std=c++20 -o hdr-oracle hdr-oracle.cc $DRM_CXX/src/display/hdr_metadata.cpp \
//       -Ishim -I$DRM_CXX/src -I$DRM_CXX \
//       $(for d in $DRM_CXX/third_party/*/include; do echo -I$d; done) \
//       $(pkg-config --cflags --libs libdrm)
//
// where shim/drm-cxx is a symlink to $DRM_CXX/src, which is what the
// `<drm-cxx/...>` include prefix resolves to in a configured build.

#include <drm-cxx/display/hdr_metadata.hpp>

#include <cstdint>
#include <cstdio>

using drm::display::HdrSourceMetadata;
using drm::display::TransferFunction;

namespace {

void emit(const char* name, const HdrSourceMetadata& src) {
  const auto bytes = drm::display::serialize_hdr_metadata(src);
  std::printf("%s ", name);
  for (const auto b : bytes) {
    std::printf("%02x", b);
  }
  std::printf(" %016llx\n",
              static_cast<unsigned long long>(drm::display::hdr_metadata_hash(src)));
}

HdrSourceMetadata bt2020_pq() {
  HdrSourceMetadata src;
  src.eotf = TransferFunction::SmpteSt2084Pq;
  src.display_primaries.red = {0.708F, 0.292F};
  src.display_primaries.green = {0.170F, 0.797F};
  src.display_primaries.blue = {0.131F, 0.046F};
  src.display_primaries.white = {0.3127F, 0.3290F};
  src.display_primaries.has_primaries = true;
  src.display_primaries.has_default_white = true;
  src.max_display_mastering_luminance = 1000;
  src.min_display_mastering_luminance = 50;
  src.max_content_light_level = 1000;
  src.max_frame_average_light_level = 400;
  return src;
}

}  // namespace

int main() {
  emit("default", HdrSourceMetadata{});
  emit("bt2020_pq", bt2020_pq());

  // Every EOTF, so the enum mapping is pinned rather than assumed.
  for (const auto eotf : {TransferFunction::TraditionalGammaSdr,
                          TransferFunction::TraditionalGammaHdr,
                          TransferFunction::SmpteSt2084Pq, TransferFunction::Bt2100Hlg}) {
    HdrSourceMetadata src = bt2020_pq();
    src.eotf = eotf;
    char name[32];
    std::snprintf(name, sizeof(name), "eotf_%d", static_cast<int>(eotf));
    emit(name, src);
  }

  // The clamp edges, including a coordinate that lands exactly on the last
  // representable step and must not saturate.
  {
    HdrSourceMetadata src;
    src.display_primaries.red = {-0.1F, 5.0F};
    src.display_primaries.green = {0.0F, 1.31F};
    src.display_primaries.blue = {2.0F, 0.0F};
    emit("clamped", src);
  }

  // Luminance fields alone, to catch a field written at the wrong offset that
  // a full sample would mask.
  {
    HdrSourceMetadata src;
    src.max_display_mastering_luminance = 0x1122;
    src.min_display_mastering_luminance = 0x3344;
    src.max_content_light_level = 0x5566;
    src.max_frame_average_light_level = 0x7788;
    emit("luminance_only", src);
  }

  // One primary at a time: a swapped pair is invisible when all six differ
  // only in value, and obvious when five of them are zero.
  {
    const char* names[] = {"only_red", "only_green", "only_blue", "only_white"};
    for (int i = 0; i < 4; ++i) {
      HdrSourceMetadata src;
      const drm::display::Chromaticity c{0.25F, 0.75F};
      switch (i) {
        case 0: src.display_primaries.red = c; break;
        case 1: src.display_primaries.green = c; break;
        case 2: src.display_primaries.blue = c; break;
        default: src.display_primaries.white = c; break;
      }
      emit(names[i], src);
    }
  }

  return 0;
}
