# drmkit-capture

Read back what a CRTC is scanning out, and write it to a file.

`snapshot` composites the planes bound to a CRTC into an `Image` the
size of its active mode, reading each plane's buffer back through a
dma-buf. `write_png` puts one on disk.

This is a readback path, not a rendering one: it reads buffers that already
exist and blends them on the CPU exactly as the display controller would.
What it cannot read -- tiled buffers, YUV, anything not host-mappable -- it
skips with a warning rather than failing the capture, because a screenshot
missing its video overlay is more use than no screenshot.

Part of [drmkit](https://github.com/jwinarske/drmkit), a Rust port of
[drm-cxx](https://github.com/jwinarske/drm-cxx). API documentation is on
[docs.rs](https://docs.rs/drmkit-capture).

## License

MIT, matching drm-cxx.
