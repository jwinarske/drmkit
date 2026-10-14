# drmkit-time

drmkit's library-wide monotonic clock abstraction.

Subsystems that animate, pace, or schedule take a `&dyn Clock` so
callers can:

- sync animation with an external presentation clock — a compositor that
  already tracks vblank/present timestamps hands the same time point to
  drmkit and to its own renderers;
- freeze or step time in tests without sleeping (see `ManualClock`);
- consolidate on a single clock as other timing-aware subsystems
  (page-flip pacing, input debouncing, animation cycles) land.

Callers who do not care get `default_clock` and never construct a clock.

Part of [drmkit](https://github.com/jwinarske/drmkit), a Rust port of
[drm-cxx](https://github.com/jwinarske/drm-cxx). API documentation is on
[docs.rs](https://docs.rs/drmkit-time).

## License

MIT, matching drm-cxx.
