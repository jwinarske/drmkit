# drmkit-scene

Scene composition for drmkit.

A `LayerScene` owns one CRTC's stack of layers and the machinery that
turns them into a frame: which plane each layer lands on, what to composite
when there are more layers than planes, and when a buffer may go back to
whoever produced it.

Part of [drmkit](https://github.com/jwinarske/drmkit), a Rust port of
[drm-cxx](https://github.com/jwinarske/drm-cxx). API documentation is on
[docs.rs](https://docs.rs/drmkit-scene).

## License

MIT, matching drm-cxx.
