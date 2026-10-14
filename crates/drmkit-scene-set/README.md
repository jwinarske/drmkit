# drmkit-scene-set

Several scenes, one frame.

A `LayerScene` drives one CRTC. A `SceneSet` drives several and decides
how many atomic commits that costs — which is the whole question, because
one commit across every output is *atomic* and several are not, while one
commit means one output's refusal takes the rest down with it. See
`NarrowPolicy`.

Part of [drmkit](https://github.com/jwinarske/drmkit), a Rust port of
[drm-cxx](https://github.com/jwinarske/drm-cxx). API documentation is on
[docs.rs](https://docs.rs/drmkit-scene-set).

## License

MIT, matching drm-cxx.
