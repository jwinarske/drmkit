# drmkit-scene-gbm

A scene source backed by a GBM **surface**.

A surface is a swap chain: EGL or Vulkan renders into it, `eglSwapBuffers`
rotates it, and this source locks whichever buffer came to the front. That is
the shape a GL or Vulkan producer needs, where `drmkit-scene-sources`'s
`GbmBufferSource` hands out one allocation the caller renders into.

Part of [drmkit](https://github.com/jwinarske/drmkit), a Rust port of
[drm-cxx](https://github.com/jwinarske/drm-cxx). API documentation is on
[docs.rs](https://docs.rs/drmkit-scene-gbm).

## License

MIT, matching drm-cxx.
