# drmkit-input

Keyboard, pointer and touch input.

Two halves that are useful apart. `Keyboard` compiles an xkb keymap and
turns evdev keycodes into keysyms and text; the libinput side reads the
devices those codes come from. A caller with its own event source -- a
remote protocol, a test harness, a replay -- wants the first without the
second, so they are separate and each is behind its own feature.

Part of [drmkit](https://github.com/jwinarske/drmkit), a Rust port of
[drm-cxx](https://github.com/jwinarske/drm-cxx). API documentation is on
[docs.rs](https://docs.rs/drmkit-input).

## License

MIT, matching drm-cxx.
