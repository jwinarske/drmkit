# drmkit-csd

Client-side window decorations.

Title bars, shadows, rounded corners and the buttons on them, drawn by the
client rather than by a compositor, which is what makes them client-side.

Everything here is CPU work over a pixel buffer. Nothing in this crate
touches a DRM device: a decoration is pixels, and where those pixels go is
the presenter's business.

Part of [drmkit](https://github.com/jwinarske/drmkit), a Rust port of
[drm-cxx](https://github.com/jwinarske/drm-cxx). API documentation is on
[docs.rs](https://docs.rs/drmkit-csd).

## License

MIT, matching drm-cxx.
