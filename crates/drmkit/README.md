# drmkit

A layered Linux DRM/KMS display framework — modesetting, plane allocation,
scene composition, and scanout. A behavioral-parity Rust port of
[drm-cxx](https://github.com/jwinarske/drm-cxx).

Each member crate is a module here behind a feature of the same name —
`drmkit::scene` is `drmkit-scene`. The defaults (`core`, `modeset`, `dumb`,
`fmt`, `planes`, `scene`) take a card to a committed scene, and `full` adds
the rest:

```toml
[dependencies]
drmkit = { version = "0.1", features = ["cursor", "capture"] }
```

None of those links a system library. The parts that do are features of
their own: `edid` (libdisplay-info), `hotplug` (libudev), `libinput`
(libinput, libudev), `xkb` (xkbcommon) and `libseat` (libseat).

A scene can bring up a display, allocate planes for a layer stack, commit a
frame, composite what it cannot place, and release buffers on the right
vblank. EDID, color management, cursor and capture are in, as are the present
spine, cross-output orchestration, GBM surface sources and client-side
decorations. The GL compositor, the GL and Vulkan scanout producers, EGL
streams, the V4L2, `GStreamer` and nvbufsurface sources, and the Vulkan
display surface are not.

See the [crate documentation](https://docs.rs/drmkit) for which member crate
does what, or <https://github.com/jwinarske/drmkit> for the porting plan.

Upstream baseline: `jwinarske/drm-cxx` @ `5e7047b` (v5.0.0+).

## License

MIT, matching drm-cxx.
