# drmkit-scene-set

Several scenes, one frame.

A `LayerScene` drives one CRTC. A `SceneSet` holds several, mirrors layers
across them, and plans how many atomic commits a frame across them costs —
which is the whole question, because one commit across every output is
*atomic* and several are not, while one commit means one output's refusal
takes the rest down with it. See `NarrowPolicy`. The set plans; making the
commits is the caller's.

Part of [drmkit](https://github.com/jwinarske/drmkit), a Rust port of
[drm-cxx](https://github.com/jwinarske/drm-cxx). API documentation is on
[docs.rs](https://docs.rs/drmkit-scene-set).

## License

MIT, matching drm-cxx.
