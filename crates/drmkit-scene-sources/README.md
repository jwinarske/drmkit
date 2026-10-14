# drmkit-scene-sources

Tier-1 buffer sources for drmkit scenes: dumb buffers, GBM buffers and
imported DMA-BUFs. They need no EGL and no external media stack, and every
one implements `LayerBufferSource`, so a scene consumes them without knowing
which it has.

Part of [drmkit](https://github.com/jwinarske/drmkit), a Rust port of
[drm-cxx](https://github.com/jwinarske/drm-cxx). API documentation is on
[docs.rs](https://docs.rs/drmkit-scene-sources).

## License

MIT, matching drm-cxx.
