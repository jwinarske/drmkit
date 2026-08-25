# 4-byte leak in parse_data_block on a CTA InfoFrame block

Draft for <https://gitlab.freedesktop.org/emersion/libdisplay-info/-/issues>.
Files referenced are next to this one.

---

`di_info_parse_edid()` leaks 4 bytes if the EDID has a CTA-861 extension with
an InfoFrame Data Block in it. `di_info_destroy()` doesn't free it.

Seen on 0.3.0 (Fedora's `libdisplay-info.so.3`), also on a 0.3.0 built from
source on Ubuntu.

Reproducer is attached (`leak_repro.c`, `edid-libdisplay-info-leak-minimal.bin`):

```sh
cc -g -fsanitize=address -o leak_repro leak_repro.c \
   $(pkg-config --cflags --libs libdisplay-info)
./leak_repro edid-libdisplay-info-leak-minimal.bin
```

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

The EDID is a plain base block plus a CTA extension whose whole data block
collection is six bytes:

```
ff 20 20 00 00 05
```

ie. Extended Tag block, extended tag 32 (InfoFrame Data Block), processing
descriptor header saying one descriptor follows with a zero-length payload,
then the descriptor. Both checksums are fine and the block fits inside the
collection, so it isn't a truncation or checksum thing.

If you'd rather build it than download it:

```python
base = bytearray(128)
base[1:7] = b"\xff" * 6
base[18] = 1
base[126] = 1
base[127] = (256 - sum(base[:127]) % 256) % 256

cta = bytearray(128)
cta[0], cta[1], cta[2] = 0x02, 3, 36
cta[4:10] = bytes([0xff, 0x20, 0x20, 0x00, 0x00, 0x05])
cta[127] = (256 - sum(cta[:127]) % 256) % 256

open("leak.bin", "wb").write(bytes(base + cta))
```

It's fairly specific. Zero that last `05`, or say 0 descriptors instead of 1,
or 2, or cut the block shorter, and it doesn't leak. Extended tags 0/5/6/13 in
place of 32 don't leak. A couple of real monitor EDIDs (one with a CTA block
and HDR metadata) don't leak either. So it looks like one path through the
InfoFrame handling that allocates and then bails.

Haven't dug into the source, so no patch, sorry.

Found by a fuzzer in a project of mine that parses EDIDs through this library.
The C reproducer above doesn't involve any of it.
