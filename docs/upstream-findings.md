# Upstream findings

drm-cxx is the **origin of authority** for this port. Anything the port surfaces
— a defect, a latent hazard, a design improvement — is filed as an issue on
[`jwinarske/drm-cxx`](https://github.com/jwinarske/drm-cxx/issues) rather than
fixed only here.

A fix that lives only in Rust makes the two implementations diverge and leaves
the C++ consumers carrying the bug. Filing upstream keeps the authority where it
belongs, and this table records what the Rust side does in the meantime.

The default for a **bug** is to reproduce the C++ behavior, so the two agree
byte for byte, and switch once upstream decides. The default for a
**hazard the type system removes** is to note it and move on — there is nothing
to reproduce.

## Filed

| # | Subsystem | Finding | Severity | Rust side |
|---|---|---|---|---|
| [232](https://github.com/jwinarske/drm-cxx/issues/232) | `fmt` | The `#ifndef` fallback for `DRM_FORMAT_MOD_ARM_16X16_BLOCK_U_INTERLEAVED` aliases `AFBC(16x16)`, so `classify()` would report 16x16 AFBC as `Tiling` | Latent — only on a header predating the ARM type-field encoding | Fixed. `ARM_16X16_BLOCK_U_INTERLEAVED` is a named constant carrying the type field, pinned by `afbc_16x16_is_not_the_interleaved_layout` |
| [233](https://github.com/jwinarske/drm-cxx/issues/233) | `planes` | `compatible_with_crtc` shifts by an unbounded index; UB for `crtc_index >= 32` | Latent — no shipping device has 32 CRTCs, but it is reachable from public API | Fixed. Returns `false` past the mask width, pinned by `compatible_with_crtc_rejects_out_of_range_indices` |
| [234](https://github.com/jwinarske/drm-cxx/issues/234) | `core` | `format_bpp`/`format_name` omit packed 4:2:2 YUV and `AYUV`; a `YUYV` layer is costed at 2× its real bandwidth | Live, minor — biases allocator scoring against capture layers | **Reproduced deliberately.** `format_bpp` returns 0 for these so the two implementations agree; pinned by `format_bpp_packed_8bit_yuv_matches_cxx_zero`, which says it is a parity choice. Switches when upstream does |
| [235](https://github.com/jwinarske/drm-cxx/issues/235) | `planes` | The `property_hash` content/placement split is two names in an `if`; a new `PropTag` is classified by default with nothing telling the author a decision was made | Enhancement | Already stronger here. `PropTag::class` is an exhaustive `match`, so an unclassified variant does not compile (invariant 4) |

## Conventions

- **Verify before filing.** Compile a demonstration, quote the exact
  `file:line`, show the observable consequence. Never file from reasoning alone.
- **Be honest about severity.** Say plainly when something is latent rather than
  live.
- **Title as** `subsystem: lowercase description`, matching the repo's existing
  issues.
- **Say what the port needs**, which is often nothing — that keeps the issue
  about the C++ rather than about drmkit.
