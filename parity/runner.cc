// SPDX-License-Identifier: MIT
//
// T7 differential parity harness -- C++ (reference) scenario runner.
//
// Reads a scenario script and drives drm::scene::LayerScene through it. It
// prints nothing of interest itself; the artifact is the property-write trace
// that parity/trace-shim.c captures from the ioctls this makes. The Rust
// runner in parity/runner/ consumes the same script and must produce a trace
// that diffs clean against this one.
//
// Deliberately built out of tree against a pristine drm-cxx checkout: the C++
// side is the origin of authority, so the harness adapts to it rather than the
// other way round.

#include <drm-cxx/core/device.hpp>
#include <drm-cxx/modeset/page_flip.hpp>
#include <drm-cxx/scene/dumb_buffer_source.hpp>
#include <drm-cxx/scene/layer_desc.hpp>
#include <drm-cxx/scene/layer_scene.hpp>

#include <drm_fourcc.h>
#include <xf86drm.h>
#include <xf86drmMode.h>

#include <cstdint>
#include <cstdio>
#include <cstring>
#include <fcntl.h>
#include <fstream>
#include <map>
#include <sstream>
#include <string>
#include <unistd.h>
#include <vector>

using drm::Device;
using drm::scene::DumbBufferSource;
using drm::scene::LayerDesc;
using drm::scene::LayerHandle;
using drm::scene::LayerScene;

namespace {

/// A DumbBufferSource that can be told to starve.
///
/// EAGAIN from acquire() is the documented "no frame yet" signal: the layer is
/// skipped for this frame and counted, rather than the frame failing. Live
/// sources (V4L2 before the first sample, a GStreamer appsink mid-preroll) do
/// this routinely, so the accounting has to be right or a compositor cannot
/// tell a starved layer from a dropped one.
class StarvableSource : public drm::scene::LayerBufferSource {
 public:
  explicit StarvableSource(std::unique_ptr<drm::scene::DumbBufferSource> inner)
      : inner_(std::move(inner)) {}

  void set_starved(bool starved) noexcept { starved_ = starved; }

  drm::expected<drm::scene::AcquiredBuffer, std::error_code> acquire() override {
    if (starved_) {
      return drm::unexpected<std::error_code>(
          std::make_error_code(std::errc::resource_unavailable_try_again));
    }
    return inner_->acquire();
  }

  void release(drm::scene::AcquiredBuffer buf) noexcept override {
    inner_->release(std::move(buf));
  }

  [[nodiscard]] drm::scene::BindingModel binding_model() const noexcept override {
    return inner_->binding_model();
  }

  [[nodiscard]] drm::scene::SourceFormat format() const noexcept override {
    return inner_->format();
  }

 private:
  std::unique_ptr<drm::scene::DumbBufferSource> inner_;
  bool starved_{false};
};

int fail(const char* what) {
  std::fprintf(stderr, "parity-runner: %s\n", what);
  return 1;
}

std::string find_vkms() {
  for (int i = 0; i < 16; i++) {
    char path[32];
    std::snprintf(path, sizeof path, "/dev/dri/card%d", i);
    const int fd = ::open(path, O_RDWR | O_CLOEXEC);
    if (fd < 0) continue;
    drmVersionPtr v = drmGetVersion(fd);
    const bool ok = v && v->name && std::strcmp(v->name, "vkms") == 0;
    if (v) drmFreeVersion(v);
    ::close(fd);
    if (ok) return path;
  }
  return {};
}

struct Active {
  std::uint32_t crtc_id{0}, connector_id{0};
  drmModeModeInfo mode{};
};

bool pick_crtc(int fd, Active& out) {
  auto* res = drmModeGetResources(fd);
  if (!res) return false;
  bool found = false;
  for (int i = 0; i < res->count_connectors && !found; i++) {
    auto* conn = drmModeGetConnector(fd, res->connectors[i]);
    if (!conn) continue;
    if (conn->connection == DRM_MODE_CONNECTED && conn->count_modes > 0) {
      auto* enc = conn->encoder_id ? drmModeGetEncoder(fd, conn->encoder_id) : nullptr;
      if (enc) {
        if (enc->crtc_id) {
          out.crtc_id = enc->crtc_id;
          out.connector_id = conn->connector_id;
          out.mode = conn->modes[0];
          found = true;
        }
        drmModeFreeEncoder(enc);
      }
      if (!found && res->count_crtcs > 0) {
        out.crtc_id = res->crtcs[0];
        out.connector_id = conn->connector_id;
        out.mode = conn->modes[0];
        found = true;
      }
    }
    drmModeFreeConnector(conn);
  }
  drmModeFreeResources(res);
  return found;
}

}  // namespace

int main(int argc, char** argv) {
  if (argc != 2) return fail("usage: runner <scenario>");

  std::ifstream script(argv[1]);
  if (!script) return fail("cannot open scenario");

  const std::string node = find_vkms();
  if (node.empty()) return fail("no vkms node");

  auto dev_r = Device::open(node);
  if (!dev_r) return fail("Device::open");
  Device dev = std::move(*dev_r);
  if (!dev.enable_universal_planes()) return fail("universal planes");
  if (!dev.enable_atomic()) return fail("atomic");

  Active active;
  if (!pick_crtc(dev.fd(), active)) return fail("no usable crtc");

  LayerScene::Config cfg;
  cfg.crtc_id = active.crtc_id;
  cfg.connector_id = active.connector_id;
  cfg.mode = active.mode;
  auto scene_r = LayerScene::create(dev, cfg);
  if (!scene_r) return fail("LayerScene::create");
  auto& scene = **scene_r;

  drm::PageFlip flip(dev);
  std::map<std::string, LayerHandle> handles;

  std::string line;
  while (std::getline(script, line)) {
    const auto hash = line.find('#');
    if (hash != std::string::npos) line.erase(hash);
    std::istringstream in(line);
    std::string cmd;
    if (!(in >> cmd)) continue;

    if (cmd == "frame") {
      // A real commit with an event, then land it. Committing without
      // draining would leave the flip armed and the next commit would race
      // it -- and, per invariant 5, would make teardown unsafe.
      auto r = scene.commit(DRM_MODE_PAGE_FLIP_EVENT, &flip);
      if (!r) return fail("commit");
      if (!scene.drain(flip, 1000)) return fail("drain");
      continue;
    }

    if (cmd == "starve" || cmd == "unstarve") {
      std::string name;
      if (!(in >> name)) return fail("starve: missing name");
      auto it = handles.find(name);
      if (it == handles.end()) return fail("starve: unknown layer");
      auto* layer = scene.get_layer(it->second);
      if (layer == nullptr) return fail("starve: stale handle");
      auto* src = dynamic_cast<StarvableSource*>(&layer->source());
      if (src == nullptr) return fail("starve: not a starvable source");
      src->set_starved(cmd == "starve");
      continue;
    }

    if (cmd == "zpos") {
      // Reorder the stack without changing anything else about the layers.
      std::string name;
      int z = 0;
      if (!(in >> name >> z)) return fail("zpos: bad args");
      auto it = handles.find(name);
      if (it == handles.end()) return fail("zpos: unknown layer");
      auto* layer = scene.get_layer(it->second);
      if (layer == nullptr) return fail("zpos: stale handle");
      layer->set_zpos(z);
      continue;
    }

    if (cmd == "move") {
      // A geometry change, which must defeat the FB-only fast path: the
      // kernel accepted the old rectangle, not this one.
      std::string name;
      int x = 0, y = 0;
      if (!(in >> name >> x >> y)) return fail("move: bad args");
      auto it = handles.find(name);
      if (it == handles.end()) return fail("move: unknown layer");
      auto* layer = scene.get_layer(it->second);
      if (layer == nullptr) return fail("move: stale handle");
      const auto r = layer->display().dst_rect;
      layer->set_dst_rect(drm::scene::Rect{x, y, r.w, r.h});
      continue;
    }

    if (cmd == "del") {
      std::string name;
      if (!(in >> name)) return fail("del: missing name");
      auto it = handles.find(name);
      if (it == handles.end()) return fail("del: unknown layer");
      scene.remove_layer(it->second);
      handles.erase(it);
      continue;
    }

    if (cmd == "add") {
      std::string name;
      int zpos = 0, x = 0, y = 0;
      std::uint32_t w = 0, h = 0;
      if (!(in >> name >> zpos >> x >> y >> w >> h)) return fail("add: bad args");
      if (w == 0 && h == 0) {
        w = active.mode.hdisplay;
        h = active.mode.vdisplay;
      }
      auto src = DumbBufferSource::create(dev, w, h, DRM_FORMAT_ARGB8888);
      if (!src) return fail("DumbBufferSource::create");

      LayerDesc layer;
      layer.source = std::make_unique<StarvableSource>(std::move(*src));
      layer.display.src_rect = drm::scene::Rect{0, 0, w, h};
      layer.display.dst_rect = drm::scene::Rect{x, y, w, h};
      layer.display.zpos = zpos;
      auto added = scene.add_layer(std::move(layer));
      if (!added) return fail("add_layer");
      handles.emplace(name, *added);
      continue;
    }

    return fail("unknown command");
  }

  // Teardown while a flip could still be outstanding is exactly what
  // invariant 5 forbids; every `frame` already drained, so this is a
  // belt-and-braces landing for a scenario that ends mid-flight.
  (void)scene.drain(flip, 1000);
  return 0;
}
