# libdisplay-info: 4-byte leak in `parse_data_block` on a CTA InfoFrame block

Ready to file at <https://gitlab.freedesktop.org/emersion/libdisplay-info/-/issues>.
Everything below was reproduced locally; the reproducer files sit beside this
one.

---

## Summary

`di_info_parse_edid()` leaks 4 bytes when the EDID carries a CTA-861 extension
block containing an **InfoFrame Data Block** (extended tag 32) with one
InfoFrame descriptor. The allocation is made in `parse_data_block` and is not
reachable from `di_info_destroy()`, so a caller that does everything right
still leaks.

An EDID arrives from whatever is on the other end of a display cable, so this
is attacker-influenced input in the sense that matters: a compositor probing a
connector that flaps, or a hostile sink, leaks 4 bytes per parse.

## Version

Reproduced against **libdisplay-info 0.3.0** (Fedora `libdisplay-info.so.3`,
build id `2a02663aca3f159a80d4247a6606f2f0e0bd85cf`), and independently in CI
against a 0.3.0 built from source on Ubuntu.

## Reproducer

`leak_repro.c` needs nothing but the library:

```sh
cc -g -fsanitize=address -o leak_repro leak_repro.c \
   $(pkg-config --cflags --libs libdisplay-info)

./leak_repro edid-libdisplay-info-leak-minimal.bin
```

Output:

```
parsing 256 bytes
parsed and destroyed cleanly

==4120141==ERROR: LeakSanitizer: detected memory leaks

Direct leak of 4 byte(s) in 1 object(s) allocated from:
    #0 calloc
    #1 parse_data_block.lto_priv.0 (libdisplay-info.so.3+0x3008)
    #2 di_info_parse_edid          (libdisplay-info.so.3+0x14e66)
    #3 main leak_repro.c:21

SUMMARY: AddressSanitizer: 4 byte(s) leaked in 1 allocation(s).
```

Note that `main` calls `di_info_destroy()` before returning — the leak survives
it.

Two inputs are attached:

| file | what it is |
|---|---|
| `edid-libdisplay-info-leak-minimal.bin` | 256 bytes, 17 of them non-zero — reduced by hand from the one below |
| `edid-libdisplay-info-leak.bin` | 257 bytes, the original artifact as the fuzzer found it |

## The input that triggers it

The minimal case is a valid 128-byte base block declaring one extension,
followed by a CTA-861 extension whose data block collection is exactly six
bytes:

```
ff 20 20 00 00 05
```

- `ff` — data block header: tag 7 (Extended Tag), length 31
- `20` — extended tag 32, InfoFrame Data Block
- `20` — InfoFrame Processing Descriptor header: 1 descriptor follows,
  processing-descriptor payload length 0
- `00 00 05` — the InfoFrame descriptor

Both checksums are valid and the block's declared length (31) sits inside the
collection, so this is not a truncation or a checksum case.

To build the file from scratch without the attachment:

```python
base = bytearray(128)
base[1:7] = b"\xff" * 6          # EDID magic 00 ff ff ff ff ff ff 00
base[18] = 1                     # version 1
base[126] = 1                    # one extension block
base[127] = (256 - sum(base[:127]) % 256) % 256

cta = bytearray(128)
cta[0], cta[1], cta[2] = 0x02, 3, 36          # CTA tag, revision, DTD offset
cta[4:10] = bytes([0xff, 0x20, 0x20, 0x00, 0x00, 0x05])
cta[127] = (256 - sum(cta[:127]) % 256) % 256

open("leak.bin", "wb").write(bytes(base + cta))
```

## What was ruled out

Each of these was tested against the same build; only the last leaks.

| variant | result |
|---|---|
| Two real-world EDIDs, one with a CTA block and HDR metadata | clean |
| Base block alone, no extension | clean |
| Base block with the extension count zeroed | clean |
| CTA extension with an empty data block collection | clean |
| CTA extension with a well-formed Video data block | clean |
| Extended blocks with tags 0, 5, 6, 13 in place of 32 | clean |
| InfoFrame block with 0 descriptors (`ff 20 00 ...`) | clean |
| InfoFrame block with 2 descriptors (`ff 20 40 ...`) | clean |
| Same block with the final `05` byte zeroed | clean |
| The block truncated to 3, 4 or 5 bytes | clean |
| **`ff 20 20 00 00 05`, one descriptor** | **leaks 4 bytes** |

Also checked and found not to matter: the bad extension-block checksum and the
stray trailing byte that the original 257-byte artifact happened to carry.
Correcting the checksum and dropping the byte leaves the leak intact, which is
why the minimal case has neither.

## How it was found

By a fuzz target in [drmkit](https://github.com/jwinarske/drmkit), a Rust
DRM/KMS library that uses libdisplay-info through the `libdisplay-info` crate
(0.3.0) to parse EDIDs. The Rust side holds the `Info` and drops it, which is
the `di_info_destroy()` call, so the binding is not implicated — and the C
reproducer above uses no Rust at all.

drmkit currently carries an LSan suppression naming this library so its fuzz
lane stays green, which we would like to delete.
