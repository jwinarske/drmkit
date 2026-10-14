# drmkit-display

Display characterisation and colour handling.

What a connected display can do, and what has to happen to pixels before
they reach it. Two halves that mostly do not talk to each other: finding the connected
output and the CRTC and primary plane that drive it, its modes, EDID and
capabilities; and the colour side, from HDR metadata to the CRTC's colour
pipeline.

Part of [drmkit](https://github.com/jwinarske/drmkit), a Rust port of
[drm-cxx](https://github.com/jwinarske/drm-cxx). API documentation is on
[docs.rs](https://docs.rs/drmkit-display).

## License

MIT, matching drm-cxx.
