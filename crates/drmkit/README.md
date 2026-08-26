# drmkit

A layered Linux DRM/KMS display framework — modesetting, plane allocation,
scene composition, and scanout. A behavioral-parity Rust port of
[drm-cxx](https://github.com/jwinarske/drm-cxx).

> **This crate re-exports nothing yet.** The library is real — phases 0–4 of
> the porting plan are met and the member crates are usable — but taking
> `drmkit` gets you a version constant and nothing else. **Depend on the
> member crates you need directly** until this becomes their feature-gated
> re-export surface.

A scene can bring up a display, allocate planes for a layer stack, commit a
frame, composite what it cannot place, and release buffers on the right
vblank. EDID, colour management, cursor and capture are in. The present spine,
client-side decorations, and the GL, GBM surface and stream source tiers are
not.

See the [crate documentation](https://docs.rs/drmkit) for which member crate
does what, or <https://github.com/jwinarske/drmkit> for the porting plan.

Upstream baseline: `jwinarske/drm-cxx` @ `4a0b64a` (v2.0.1+6).

## License

MIT, matching drm-cxx.
