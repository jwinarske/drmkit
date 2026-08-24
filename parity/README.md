# T7 differential parity harness

Runs one scenario script through both implementations against the same vkms
instance and diffs the resulting property-write traces.

```
DRM_CXX=/path/to/drm-cxx parity/run.sh parity/scenarios/warm-start-stability.scenario
```

## Why the trace is captured at the ioctl boundary

`trace-shim.c` is an `LD_PRELOAD` shim that intercepts `ioctl` and decodes
`DRM_IOCTL_MODE_ATOMIC`. Neither implementation is instrumented, which matters
for two reasons: the drm-cxx tree stays pristine as the origin of authority,
and both traces are produced by the *same* code reading the *same* struct. A
differ fed by each implementation's own request builder would be comparing the
harness against itself as much as the implementations.

It also captures what the kernel answered, not just what was asked. A rejected
commit is a behavior the two must agree on and it is invisible in the writes.

## The rustix trap

`drm-ffi` calls through `rustix`, whose default Linux backend issues syscalls
directly. `LD_PRELOAD` cannot see a raw syscall, so a normally-built Rust runner
produces an **empty** trace — and an empty trace diffs clean against nothing at
all if you are not counting lines. `run.sh` builds with `--cfg rustix_use_libc`
to route rustix through libc. If a Rust trace ever comes back at zero lines,
that is the cause, and the diff it produced was meaningless.

## Normalization

Raw KMS object ids, framebuffer ids and blob ids are assigned by the kernel and
differ between runs, so a trace that quoted them would diff dirty for reasons
unrelated to behavior. The shim rewrites them:

| Raw | Trace |
|---|---|
| object id | `crtc[0]`, `plane[3]`, `connector[1]` — resolved from the device |
| `FB_ID`, `MODE_ID` | `#0`, `#1`, … — numbered in order of first appearance |
| `CRTC_ID` value | the target object's name |
| a zero `FB_ID` / `CRTC_ID` | `0` — the disable sentinel, not an allocation |

Numbering buffers by first appearance is deliberate: what a trace asserts is
that the same *distinct* buffer comes back on the same frame, not which handle
the kernel happened to hand out.

## Scenarios that are expected to diverge

A scenario whose header says so is a **reproduction**, not a regression: both
implementations are known to disagree and the upstream question is open. It is
kept in the corpus because it is what will show the fix landing, and `run.sh`
reports it dirty in the meantime.

Everything without such a header is expected to diff clean, and a dirty result
there is a real finding.

## Adding a scenario

Scenario grammar is four commands — `add NAME ZPOS DST_X DST_Y W H`, `del NAME`,
`frame`, and `#` comments. Both runners parse it independently; keep them in
step. `W H` of `0 0` means the full display.

Write down what the reference actually does in a comment at the top of the
scenario. The reference is the specification, and a scenario whose expected
answer lives only in someone's head is not a parity test.
