# drmkit-gbm

GBM buffer allocation.

A thin layer over libgbm, in the shape the rest of drmkit expects: errors
as a typed enum, formats as drmkit `FourCC`s, and a buffer that hands out
the descriptor a scanout path needs.

GBM is how a GPU-rendered surface becomes something KMS can scan out. Where
`drmkit-dumb` allocates plain linear memory the CPU writes,
this asks the driver for memory its renderer can target — which on real
hardware means tiled or compressed layouts, and a modifier saying which.

On a driver with no render engine the `drm` backend falls back to dumb
allocation, so this still works against vkms: linear, one plane, and a
dma-buf that exports. That is what makes it testable without a GPU.

Part of [drmkit](https://github.com/jwinarske/drmkit), a Rust port of
[drm-cxx](https://github.com/jwinarske/drm-cxx). API documentation is on
[docs.rs](https://docs.rs/drmkit-gbm).

## License

MIT, matching drm-cxx.
