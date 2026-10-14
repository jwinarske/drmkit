# drmkit-planes

Display-plane capabilities, the layer model, and the plane allocator.

Three pieces, bottom-up:

- `PlaneCapabilities` and `PlaneRegistry` — what each plane on a device
  can do: which CRTCs it feeds, which formats and modifiers it scans out,
  whether it rotates, where its zpos may sit. Built from a device behind
  the `device` feature, or from fixtures a caller supplies, which is what
  lets the allocator be tested against hardware nobody has.
- `Layer` — one thing to put on screen, as a bag of KMS properties plus
  the hints that inform placement.
- `Allocator` — which layer goes on which plane.

Part of [drmkit](https://github.com/jwinarske/drmkit), a Rust port of
[drm-cxx](https://github.com/jwinarske/drm-cxx). API documentation is on
[docs.rs](https://docs.rs/drmkit-planes).

## License

MIT, matching drm-cxx.
