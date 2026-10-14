# drmkit-core

Core DRM/KMS device access for drmkit.

Three things live here:

- `Device` — an open DRM node, owning or borrowing its descriptor.
- `PropertyStore` — a per-object property cache, so building a frame's
  atomic request is hash-map lookups rather than an ioctl per property.
- `AtomicRequest` — accumulated property writes, testable and committable.

Part of [drmkit](https://github.com/jwinarske/drmkit), a Rust port of
[drm-cxx](https://github.com/jwinarske/drm-cxx). API documentation is on
[docs.rs](https://docs.rs/drmkit-core).

## License

MIT, matching drm-cxx.
