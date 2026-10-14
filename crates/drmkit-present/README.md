# drmkit-present

The present spine: getting rendered frames onto a display, repeatedly.

Where `drmkit_scene` answers "what goes on
which plane this frame", this crate answers the questions around it: which
buffer to draw into, how much of it needs redrawing, and whether the frame
is worth submitting at all.

Part of [drmkit](https://github.com/jwinarske/drmkit), a Rust port of
[drm-cxx](https://github.com/jwinarske/drm-cxx). API documentation is on
[docs.rs](https://docs.rs/drmkit-present).

## License

MIT, matching drm-cxx.
