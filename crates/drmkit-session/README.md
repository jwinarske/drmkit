# drmkit-session

Holding devices across a VT switch.

A compositor that owns the display cannot simply open `/dev/dri/card0` and
keep it: switching to another virtual terminal has to take the display away
and give it back. A seat -- logind, seatd, or a bare-root fallback -- is
what arbitrates that, handing out descriptors it can revoke.

What arrives from the seat is two events. *Paused*: stop touching every
descriptor immediately, because the ioctls will start failing. *Resumed*:
here are fresh descriptors, and every per-descriptor kernel object you had
-- framebuffers, property blobs, client caps, GEM handles -- is gone.
`drmkit_cursor::Paused` is the shape that pairs with this on the other
side.

Part of [drmkit](https://github.com/jwinarske/drmkit), a Rust port of
[drm-cxx](https://github.com/jwinarske/drm-cxx). API documentation is on
[docs.rs](https://docs.rs/drmkit-session).

## License

MIT, matching drm-cxx.
