# drmkit-log

drmkit's library-wide diagnostic surface. Two knobs, matching drm-cxx's:

1. `set_log_level` — process-wide threshold. Records at or below the
   active level are emitted; the rest are dropped *before* formatting cost
   is paid. A call below the threshold reduces to one relaxed atomic load
   and a branch.

2. `set_log_sink` — process-wide redirection hook. The default sink
   writes to stderr (errors, warnings) or stdout (info, debug) prefixed
   with `[drm:LEVEL] `. Consumers can forward to `tracing`, journald,
   syslog, or an in-memory test buffer. `reset_log_sink` restores the
   default — the Rust spelling of the C++ `set_log_sink(nullptr)` path.

Part of [drmkit](https://github.com/jwinarske/drmkit), a Rust port of
[drm-cxx](https://github.com/jwinarske/drm-cxx). API documentation is on
[docs.rs](https://docs.rs/drmkit-log).

## License

MIT, matching drm-cxx.
