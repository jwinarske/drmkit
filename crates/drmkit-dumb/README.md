# drmkit-dumb

DRM dumb buffers: GPU-free, CPU-mappable scanout surfaces.

Dumb buffers are KMS's allocation path that needs no accelerator: any driver
implementing `DRM_CAP_DUMB_BUFFER` — every modern KMS driver — can hand back
a linear, CPU-mappable pixel buffer. They are the right tool for cursors,
client-side decorations, software-rendered UI, and test harnesses.

Part of [drmkit](https://github.com/jwinarske/drmkit), a Rust port of
[drm-cxx](https://github.com/jwinarske/drm-cxx). API documentation is on
[docs.rs](https://docs.rs/drmkit-dumb).

## License

MIT, matching drm-cxx.
