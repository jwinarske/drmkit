// SPDX-License-Identifier: MIT
//
// Differential oracle for EDID parsing.
//
// Runs drm-cxx's own parse_edid over the fixture from its unit suite and
// prints every field, so crates/drmkit-display/src/tests.rs can assert against
// the reference's reading rather than mine. EDID is a format where a field
// offset off by one still parses, still checksums, and yields a plausible
// wrong answer -- the worst kind to derive by hand.
//
// Needs no device.

#include <drm-cxx/display/edid.hpp>

#include <cstdint>
#include <cstdio>

int main() {
  static constexpr std::uint8_t edid[] = {
      0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x00,  //
      0x10, 0xAC, 0x00, 0x40, 0x00, 0x00, 0x00, 0x00,  //
      0x01, 0x11, 0x01, 0x03, 0x80, 0x34, 0x20, 0x78,  //
      0xEA, 0xEE, 0x95, 0xA3, 0x54, 0x4C, 0x99, 0x26,  //
      0x0F, 0x50, 0x54, 0xA5, 0x4B, 0x00, 0x71, 0x4F,  //
      0x81, 0x80, 0xA9, 0xC0, 0xD1, 0xC0, 0x01, 0x01,  //
      0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x02, 0x3A,  //
      0x80, 0x18, 0x71, 0x38, 0x2D, 0x40, 0x58, 0x2C,  //
      0x45, 0x00, 0x09, 0x25, 0x21, 0x00, 0x00, 0x1E,  //
      0x00, 0x00, 0x00, 0xFF, 0x00, 0x46, 0x4F, 0x4F,  //
      0x42, 0x41, 0x52, 0x0A, 0x20, 0x20, 0x20, 0x20,  //
      0x20, 0x20, 0x00, 0x00, 0x00, 0xFC, 0x00, 0x44,  //
      0x45, 0x4C, 0x4C, 0x0A, 0x20, 0x20, 0x20, 0x20,  //
      0x20, 0x20, 0x20, 0x20, 0x00, 0x00, 0x00, 0xFD,  //
      0x00, 0x38, 0x4C, 0x1E, 0x51, 0x11, 0x00, 0x0A,  //
      0x20, 0x20, 0x20, 0x20, 0x20, 0x20, 0x00, 0x3F,  //
      // ^^ checksum. drm-cxx's own fixture stores 0x21, which does not sum to
      // zero, so libdisplay-info refuses the blob and every assertion in its
      // test sits inside an if(result) that never runs. See drm-cxx#241.
  };

  auto r = drm::display::parse_edid(edid);
  if (!r) {
    std::printf("parse FAILED\n");
    return 1;
  }
  const auto& i = *r;
  std::printf("name        %s\n", i.name.c_str());
  std::printf("make        %s\n", i.make.c_str());
  std::printf("model       %s\n", i.model.c_str());
  std::printf("serial      %s\n", i.serial ? i.serial->c_str() : "(none)");
  std::printf("size_mm     %d x %d\n", i.width_mm, i.height_mm);
  if (i.colorimetry) {
    const auto& c = *i.colorimetry;
    std::printf("primaries   has=%d white=%d r=%.6f,%.6f g=%.6f,%.6f b=%.6f,%.6f w=%.6f,%.6f\n",
                c.has_primaries, c.has_default_white, c.red.x, c.red.y, c.green.x, c.green.y,
                c.blue.x, c.blue.y, c.white.x, c.white.y);
  } else {
    std::printf("primaries   (none)\n");
  }
  std::printf("hdr         %s\n", i.hdr ? "present" : "(none)");
  std::printf("wide_gamut  %s\n", i.wide_gamut ? "present" : "(none)");
  return 0;
}
