# drmkit-cursor

Cursor themes, and the cursors in them.

`Theme` indexes the `XCursor` themes installed on a machine and resolves a
cursor name -- following each theme's `Inherits=` chain and the alias table
that maps modern names onto whatever legacy name a theme happens to ship --
to a file on disk. It reads no pixels, which keeps it cheap and testable
without a display.

`Cursor` is the other half: it reads one of those files into frames,
hotspots and timing. A cursor file is untrusted input, so the parser bounds
every length against the bytes actually present before allocating anything
-- see the `xcursor` module for why that is not delegated to a crate.

Part of [drmkit](https://github.com/jwinarske/drmkit), a Rust port of
[drm-cxx](https://github.com/jwinarske/drm-cxx). API documentation is on
[docs.rs](https://docs.rs/drmkit-cursor).

## License

MIT, matching drm-cxx.
