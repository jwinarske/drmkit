# drmkit-csd

Client-side window decorations.

Title bars, shadows, rounded corners and the buttons on them, drawn by the
client rather than by a compositor, which is what makes them client-side.

Drawing is CPU work over a pixel buffer. The one part that touches a DRM
device is `Surface`, which allocates, maps and exports the dumb buffer a
decoration is drawn into. The presenters only compute the property writes
that put those pixels on screen; committing them is the caller's business.

Part of [drmkit](https://github.com/jwinarske/drmkit), a Rust port of
[drm-cxx](https://github.com/jwinarske/drm-cxx). API documentation is on
[docs.rs](https://docs.rs/drmkit-csd).

## License

MIT, matching drm-cxx.
