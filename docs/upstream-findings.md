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
| [237](https://github.com/jwinarske/drm-cxx/issues/237) | `planes` | `Output::rebuild_layer_ptrs` is never called, and calling it would silently undo `sort_layers_by_zpos` — it restores the insertion order the sort deliberately broke | Latent; nothing calls it today | Not ported. The port owns its layers in one vector and sorts in place, so there is no second view to resync |
| [236](https://github.com/jwinarske/drm-cxx/issues/236) | `planes` | `split_independent_groups` returns groups in unspecified order, and cross-group placement ignores `keep_priority` — groups consume a *shared* plane pool, so order decides who wins contention | Determinism: latent (stable on libstdc++ in practice). Priority: live | Groups come back ordered by lowest member index — deterministic and following the caller's layer order. Deliberately **not** reordered by priority, pending the upstream decision |
| [241](https://github.com/jwinarske/drm-cxx/issues/241) | `display` | `ParseEdidTest.ValidBlobParses`'s fixture stores checksum `0x21` where EDID's sum-to-zero rule needs `0x3F`, so `parse_edid` refuses it and every assertion inside the test's `if (result)` has never run. Correcting it reveals a second defect the test was written to catch: `hdr` and `wide_gamut` are populated whenever the C accessor returns non-null, and libdisplay-info returns a zeroed struct rather than null, so both optionals are `Some` for every display | Live. The test asserts nothing today; the optionals mislead any caller that treats `Some` as "the display said something" | **Deliberate divergence.** The port gates both on the display actually advertising something — and excludes `traditional_sdr` from the HDR test, since libdisplay-info sets it for any base EDID and counting it would reproduce the same always-`Some` defect by another route. This matches what upstream's own test comment says was intended. Switches if upstream resolves it differently |
| [240](https://github.com/jwinarske/drm-cxx/issues/240) | `scene` | The composition canvas's plane is chosen by reservation, before the allocation, and its stacking comes from a `zpos` write. On a driver with no settable plane `zpos` that write never happens, so the canvas lands wherever the reservation put it — below the very layers it carries, which are by construction the topmost ones | Live, visible on zpos-less hardware; unreproducible on typical desktop parts | **Not yet resolved either way.** The port is not better here — it places the canvas one plane too low for the same reason — so this is a shared design gap. The `composition-overflow` T7 scenario is the reproduction and is expected to diff dirty until upstream decides |
| [239](https://github.com/jwinarske/drm-cxx/issues/239) | `planes` | A `zpos` restack with no change to the layer *set* is silently dropped on drivers whose planes have no `zpos` property: warm-start re-tests the cached assignment, the kernel accepts it because it is still *valid* — just no longer in zpos order — and keeps it. `sort_layers_by_zpos` orders a cold search correctly; the warm path never re-examines order | Live, visible — the model reorders and the screen does not. Only on drivers without settable plane `zpos` (vkms, Tegra Orin display, much embedded hardware); unreproducible on typical desktop parts | **Reproduced deliberately.** The two traces are byte-identical, so this is upstream's behavior rather than a port divergence, and holding parity keeps it that way. Covered by the `zorder-reorder` T7 scenario, which documents the shared behavior. Switches when upstream does |
| [235](https://github.com/jwinarske/drm-cxx/issues/235) | `planes` | The `property_hash` content/placement split is two names in an `if`; a new `PropTag` is classified by default with nothing telling the author a decision was made | Enhancement | Already stronger here. `PropTag::class` is an exhaustive `match`, so an unclassified variant does not compile (invariant 4) |
| [242](https://github.com/jwinarske/drm-cxx/issues/242) | `scene` | `choose_canvas_zpos` returns `max_explicit + 1` and writes it unchecked. Above the plane's advertised zpos range the kernel refuses the **whole** atomic commit, so the symptom is a blank screen rather than a mis-stacked canvas | Bug | Reproduced on a Raspberry Pi 5: vc4 overlays advertise `zpos [1,17]`, an 18-layer scene asked for 19, `EINVAL`. Isolated by removing each of the 18 objects from the request in turn. The port carried the same defect and now clamps a derived zpos to the plane's range — see P-15 |

## Conventions

- **Verify before filing.** Compile a demonstration, quote the exact
  `file:line`, show the observable consequence. Never file from reasoning alone.
- **Be honest about severity.** Say plainly when something is latent rather than
  live.
- **Title as** `subsystem: lowercase description`, matching the repo's existing
  issues.
- **Say what the port needs**, which is often nothing — that keeps the issue
  about the C++ rather than about drmkit.
