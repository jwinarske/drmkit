# drmkit-sync

`sync_file` fences for explicit synchronization.

A `SyncFence` wraps a kernel `sync_file` descriptor — the objects that
flow through a plane's `IN_FENCE_FD` property and come back out of a commit
through `OUT_FENCE_PTR`. The type owns its descriptor and closes it on drop.

Part of [drmkit](https://github.com/jwinarske/drmkit), a Rust port of
[drm-cxx](https://github.com/jwinarske/drm-cxx). API documentation is on
[docs.rs](https://docs.rs/drmkit-sync).

## License

MIT, matching drm-cxx.
