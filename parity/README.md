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

## Which reference answered

The reference is the specification, so which revision of it answered has to be
known. `run.sh` checks `DRM_CXX` against the baseline recorded in
`INVARIANTS.md` and warns when the tree is behind it or has uncommitted
changes, because a drifted checkout produces a diff that looks like a parity
result and is not one.

That is not hypothetical. The corpus was for a while compared against a tree
six commits behind the documented baseline, missing among other things the
warm-start stability bonus that one of its own scenarios exists to test.
Nothing looked wrong: six scenarios diffed clean either way, which is
[P-27](../docs/parity-findings.md).

Set `DRMKIT_PARITY_ANY_REF=1` to silence it when comparing against a newer
upstream on purpose, which is how the next baseline gets adopted.

## Scenarios that are expected to diverge

A scenario whose header says so is a **reproduction**, not a regression: both
implementations are known to disagree and the upstream question is open. It is
kept in the corpus because it is what will show the fix landing, and `run.sh`
reports it dirty in the meantime.

Everything without such a header is expected to diff clean, and a dirty result
there is a real finding.

## Adding a scenario

Scenario grammar, and `#` comments. Both runners parse it independently; keep
them in step.

| Command | Meaning |
|---|---|
| `add NAME ZPOS DST_X DST_Y W H` | Add a layer. `W H` of `0 0` means the full display. |
| `del NAME` | Remove it. |
| `frame` | One real commit, and land its flip. |
| `move NAME X Y` | Move it, which defeats the FB-only fast path. |
| `zpos NAME Z` | Restack it. |
| `starve NAME` / `unstarve NAME` | Make `acquire` return `WouldBlock`, and stop. |
| `mint` | A `frame` whose out-fence is kept — already signalled once it lands. |
| `fence NAME` / `unfence NAME` | Give a layer's buffer that fence as its acquire fence, or take it away. |

Write down what the reference actually does in a comment at the top of the
scenario. The reference is the specification, and a scenario whose expected
answer lives only in someone's head is not a parity test.

## Oracles

The `*-oracle.cc` files are a second, smaller kind of parity test. Where a
scenario drives both implementations against a live vkms and diffs the
property writes, an oracle runs one *pure* function from drm-cxx and prints
what it returns, so a Rust test can be written against the reference's own
answers instead of against a reading of the specification.

They exist for the functions where a paper derivation is worth little: colour
maths (`tonemap-oracle.cc`), blending (`blend-oracle.cc`), EDID interpretation
(`edid-oracle.cc`), and the HDR metadata wire format (`hdr-oracle.cc`), where
CTA-861.3's green/blue/red primary ordering and the struct's tail padding are
both invisible on a screen and both change the bytes.

Each file carries its own build line in a comment at the top. They need
drm-cxx's sources but not a configured build; the `<drm-cxx/...>` include
prefix resolves to `$DRM_CXX/src`, so a symlink is enough:

```sh
DRM_CXX=/path/to/drm-cxx
mkdir -p shim && ln -sfn "$DRM_CXX/src" shim/drm-cxx
```

An oracle's output belongs in the Rust test as a literal table, with a comment
saying it was generated rather than derived. Do not regenerate it to make a
failing test pass — a changed vector means the two implementations have parted
company, which is the finding.

A baked table cannot notice the reference changing underneath it, though. The
Rust tests go on passing against numbers the reference has stopped producing,
and nothing says so. So the answers are recorded in `oracle-expected/` as well,
and

```sh
DRM_CXX=/path/to/drm-cxx parity/oracles.sh
```

rebuilds all four against that checkout and diffs. Run it when adopting a new
baseline — that is the moment the reference moves and the tables do not.

A difference is not a thing to regenerate away. It says the reference's maths
changed: work out whether the Rust tables should follow, and change both
deliberately or neither.

## The corpus

The plan names four required scenarios; all four are here, alongside three
that came out of defects found on hardware.

| Scenario | What it pins |
|---|---|
| `warm-start-stability` | A layer inserted into, or removed from, a settled stack must not move the survivors (§4.6). Insertion is the half that discriminates — see P-27. |
| `eagain-skip-accounting` | A starved source holds its plane and its last framebuffer, and the residual arithmetic balances. |
| `fast-path-classification` | Which frames may skip the `TEST_ONLY` commit, in both directions. |
| `fence-dup-independence` | One acquire fence arms frame after frame, because each acquire yields its own descriptor. |
| `plane-migration` | Ordering within one commit, and z-order on planes that cannot be reordered. |
| `composition-overflow` | Layers past the plane count reach the screen through the canvas. |
| `zorder-reorder` | A restack, and the upstream bug it found. |

A scenario that cannot fail is worth nothing, so each is checked by breaking
the behaviour it describes and confirming the diff catches it. For the fence
one that means handing over the fence instead of an import of it: the second
fenced frame then arms nothing, and the trace loses a line.
