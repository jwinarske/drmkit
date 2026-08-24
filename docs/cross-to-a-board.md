# Cross-compiling to a Raspberry Pi, from a host with no target libc

`.cargo/cross-aarch64.toml` names the linker and a qemu runner and points at
`/usr/aarch64-linux-gnu` for libraries. On a Debian host with
`crossbuild-essential-arm64` that path holds a full target libc and cross builds
link first time.

On Fedora it does not. `gcc-aarch64-linux-gnu` installs the compiler and leaves
`/usr/aarch64-linux-gnu/sys-root` empty, so compilation succeeds and linking
fails on everything at once:

```text
aarch64-linux-gnu-ld: cannot find -lgcc_s
aarch64-linux-gnu-ld: cannot find -lc
aarch64-linux-gnu-ld: cannot find crtn.o
```

That reads like a broken toolchain and is not one — the compiler is fine and
there is nothing for it to link against.

There is a fix that installs nothing on either machine: take the sysroot from
the board. The Pi is already running the exact glibc, gcc runtime and kernel
headers the binary will meet, which makes it a better sysroot than a packaged
one and a worse thing to forget you are depending on. Twenty-seven megabytes,
once.

## Borrow the sysroot

```sh
SYSROOT=$HOME/.cache/pi-sysroot
PI=joel@raspberrypi          # whichever board

mkdir -p "$SYSROOT/usr/lib/aarch64-linux-gnu" "$SYSROOT/usr/lib/gcc"

# The linking essentials: start files, the stub archives glibc still ships for
# -lpthread and friends, the loader, and libgcc.
rsync -a \
  --include='*.o' --include='*.a' \
  --include='libc.so*' --include='libm.so*' --include='libmvec.so*' \
  --include='libdl.so*' --include='libpthread.so*' --include='librt.so*' \
  --include='libutil.so*' --include='libanl.so*' --include='libgcc_s.so*' \
  --include='ld-linux-aarch64*' \
  --exclude='*' \
  "$PI:/usr/lib/aarch64-linux-gnu/" "$SYSROOT/usr/lib/aarch64-linux-gnu/"

rsync -a "$PI:/usr/lib/gcc/aarch64-linux-gnu" "$SYSROOT/usr/lib/gcc/"

# Two symlinks, both load-bearing. Debian's linker scripts name absolute paths
# under /lib, and the dynamic loader is looked for at /lib/ld-linux-aarch64.so.1
# *inside* the sysroot -- not where it actually sits, one directory down.
ln -sfn usr/lib "$SYSROOT/lib"
ln -sfn aarch64-linux-gnu/ld-linux-aarch64.so.1 \
        "$SYSROOT/usr/lib/ld-linux-aarch64.so.1"
```

Include `*.o` rather than `crt*.o`: the glob is case-sensitive and `Scrt1.o`,
the start file for a position-independent executable, is what Rust actually
asks for. Missing it costs a build and a puzzled minute.

## Build against it

```sh
export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=aarch64-linux-gnu-gcc
export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_RUSTFLAGS="-Dwarnings \
-C link-arg=--sysroot=$SYSROOT \
-C link-arg=-B$SYSROOT/usr/lib/aarch64-linux-gnu \
-C link-arg=-L$SYSROOT/usr/lib/aarch64-linux-gnu \
-C link-arg=-L$SYSROOT/usr/lib/gcc/aarch64-linux-gnu/14"

cargo test -p drmkit-core --target aarch64-unknown-linux-gnu --no-run
```

`-Dwarnings` is repeated from the fragment on purpose. A
`CARGO_TARGET_..._RUSTFLAGS` variable *replaces* the target's `rustflags` rather
than adding to it, so leaving it out quietly turns the cross lane's warnings
back into warnings — which is the sort of thing that is noticed a month later by
someone wondering why a lint only fires on x86.

All four link arguments earn their place, and dropping any one of them fails
differently:

| argument | without it |
|---|---|
| `--sysroot` | linker scripts resolve `/lib/...` against the host |
| `-B` | `crt1.o` and friends are not found |
| `-L` multiarch | `-lc`, `-lm` are not found |
| `-L` gcc | `-lgcc_s` is not found |

The `14` is gcc's major version on the board. Check it rather than assume:
`ls "$SYSROOT/usr/lib/gcc/aarch64-linux-gnu/"`.

The environment variables are deliberate rather than a fragment edit. They
override the committed `cross-aarch64.toml` for one shell without making the
workspace depend on a path that exists on one machine.

## Run it on the board

`cargo test --no-run` prints the binary it built. Copy that across and run it
there — no cargo on the Pi, and none needed.

```sh
BIN=$(ls -t target/aarch64-unknown-linux-gnu/debug/deps/mytest-* | grep -v '\.d$' | head -1)
scp "$BIN" "$PI:/tmp/t" && ssh "$PI" 'chmod +x /tmp/t && /tmp/t --test-threads=1'
```

`--test-threads=1` matters for anything taking DRM master: two tests racing for
it will report the second as unable to open a card, which reads as a device
problem and is a scheduling one.

## Two things that bite

**Which card.** A Pi 4 and a Pi 5 both put the render device and the display
controllers on separate DRM nodes, and a Pi 5 has two display controllers. Code
that takes the first openable `/dev/dri/cardN` gets whichever the directory
lists first, which on a Pi 5 is `rp1-dsi` and not `vc4`. If a test can be
pointed at a card, point it: a suite that passes on the controller that works
says nothing about the one beside it.

**No linking against GPU libraries.** Nothing above installs Vulkan, EGL, GBM or
libdrm for the target, and nothing needs to. `ash::Entry::load` opens the
loader at runtime and drm-rs issues ioctls through `rustix`, so neither reaches
the linker. A crate that links `libgbm` — `drmkit-gbm` — is the exception, and
wants that library added to the sysroot the same way.

## It has been run

`drmkit-core` cross-built this way and copied to a Raspberry Pi 5 gives 17
passing and 6 ignored; with `DRMKIT_REQUIRE_MASTER=1` and
`DRMKIT_TEST_CARD` naming a card it gives 23 passing, on both `vc4` and
`rp1-dsi`. That is the device suite running against a real display controller
rather than vkms, which is what the whole exercise is for.

## Why not `cross`

It works and pulls a container per target. This is for a workstation that
already has the cross compiler and wants the target's own libraries rather than
a distribution's guess at them — which for a board running a vendor kernel is
the difference between a binary that runs and one that almost does.
