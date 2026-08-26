# TEST_PARITY

Machine-checked mapping from every drm-cxx test file to its drmkit counterpart.

**Upstream baseline:** `jwinarske/drm-cxx` @ `dc2915b` (v2.0.1, PR #231, 2026-07-26)
**Coverage:** 97 test files (68 unit + 29 integration)

`cargo xtask parity --ref <drm-cxx>` fails when this table and the upstream
tree disagree in either direction: an upstream test with no row here, or a row
naming a file that no longer exists upstream. That is the point — drm-cxx keeps
moving during the port (104 commits in the three weeks before the plan was
written), and drift must surface as red CI rather than as silence.

## Status vocabulary

| Status | Meaning |
|---|---|
| `ported` | Every case in the upstream file has a named Rust counterpart. |
| `partial` | Some cases ported; the remainder are listed in the crate's tracking issue. |
| `pending` | Not started. Every row begins here. |
| `n/a` | Deliberately not ported — no Rust analogue exists. |

## Invariant pins

The `Inv.` column names the `INVARIANTS.md` contracts a file pins. A row
carrying an invariant cannot reach `ported` until the named Rust test exists
and runs in the vkms lane (plan §6, §13).

## Mapping

| C++ test | Rust test(s) | Status | Inv. |
|---|---|---|---|
| `tests/unit/test_acquire_fence.cpp` | `drmkit-sync` `tests.rs` + `drmkit-scene` `tests.rs` | ported | The fence half was already covered -- `import` dupping, a move carrying the descriptor, repeated import and drop leaking nothing. What is added is the composition the file is really about: an `AcquiredBuffer` carrying a fence through a move and a drop, once per layer per frame. `OwnedFd` makes the double-close half of the C++ hazard unrepresentable; the leak half is still reachable and is what the count measures |
| `tests/unit/test_atomic_builder.cpp` | `drmkit-core` `src/tests.rs` | ported |  |
| `tests/unit/test_buffer_mapping.cpp` | `drmkit-dumb` `src/tests.rs` | partial | 6 |
| `tests/unit/test_buffer_ring.cpp` | _drmkit-present_ (phase 6+) | pending |  |
| `tests/unit/test_canvas_format.cpp` | `drmkit-scene` `canvas.rs` | ported | All seven, plus the conversion the negotiation exists to feed: red/blue exchange, 565 packing against five reference values, and the field swap. The port had no format negotiation at all before this -- the canvas was `ARGB8888` unconditionally, so a single-plane controller without it could not composite |
| `tests/unit/test_capture_jpg.cpp` | _drmkit-capture_ (phase 6+) | pending |  |
| `tests/unit/test_capture_snapshot.cpp` | `drmkit-capture` `image.rs`, `png.rs`, `snapshot.rs` | ported | The four `Image` cases and both PNG cases, plus the blend arithmetic, the placement arithmetic (clipping, negative offsets, stride padding, scaling) and `RGB565`, none of which the C++ suite reaches. `snapshot` itself gets a device lane the C++ has none of |
| `tests/unit/test_color_pipeline_curves.cpp` | `drmkit-display` `curves.rs` | ported | All twenty-four: transfer functions, S31.32 sign-magnitude encoding, LUT quantization, the LUT and matrix builders, and the two spec anchor points that say the PQ curve is that curve rather than merely monotonic. Plus a check that the two gamut matrices multiply to the identity, which upstream tests one direction of |
| `tests/unit/test_commit_report.cpp` | `drmkit-scene` `report.rs` | ported | All three. The stale-handle case is stronger here than upstream's: a recycled slot yields a different `LayerId` because the generation is packed into it, so the miss is structural rather than a comparison that could be forgotten |
| `tests/unit/test_composite_canvas.cpp` | `drmkit-scene` `canvas.rs`, `tests/canvas_oracle.rs`, `tests/canvas_surface_vkms.rs`, `tests/composition_vkms.rs` | ported | Blend diffed against the reference's own output; surface and scene integration pinned against a device. Multi-canvas and the primary-anchor reservation are not ported |
| `tests/unit/test_connector_capabilities.cpp` | `drmkit-display` `capabilities.rs` | ported | All six, against a synthetic connector shaped like the amdgpu one this was measured on. Plus device cases upstream has none of: the probe is diffed against a raw read of the same property set, on vkms and on amdgpu |
| `tests/unit/test_csd_animator.cpp` | _drmkit-csd_ (phase 6+) | pending |  |
| `tests/unit/test_csd_overlay_reservation.cpp` | _drmkit-csd_ (phase 6+) | pending |  |
| `tests/unit/test_csd_presenter_composite.cpp` | _drmkit-csd_ (phase 6+) | pending |  |
| `tests/unit/test_csd_presenter_fb.cpp` | _drmkit-csd_ (phase 6+) | pending |  |
| `tests/unit/test_csd_presenter_plane.cpp` | _drmkit-csd_ (phase 6+) | pending |  |
| `tests/unit/test_csd_probe_presenter.cpp` | _drmkit-csd_ (phase 6+) | pending |  |
| `tests/unit/test_csd_renderer.cpp` | _drmkit-csd_ (phase 6+) | pending |  |
| `tests/unit/test_csd_shadow_cache.cpp` | _drmkit-csd_ (phase 6+) | pending |  |
| `tests/unit/test_csd_surface.cpp` | _drmkit-csd_ (phase 6+) | pending |  |
| `tests/unit/test_csd_theme.cpp` | _drmkit-csd_ (phase 6+) | pending |  |
| `tests/unit/test_cursor_cursor.cpp` | `drmkit-cursor` `cursor.rs`, `xcursor.rs` | ported | All eleven cases, plus the `.xcursor` parser the C++ delegates to libxcursor: every prefix of a valid file, a declared image larger than the file, an over-large table of contents, byte order, size-selection ties, and frame grouping by size. Fuzzed as `cursor_file` |
| `tests/unit/test_cursor_theme.cpp` | `drmkit-cursor` `theme.rs`, `alias.rs` | ported | All fifteen cases, plus an inherits cycle, a diamond, a missing parent, a key that merely starts with `Inherits`, search-path precedence, and that the alias table's order — not the caller's — decides which file a theme resolves to |
| `tests/unit/test_device.cpp` | `drmkit-core` `src/tests.rs` | ported |  |
| `tests/unit/test_display.cpp` | `drmkit-display` `edid.rs`, `profile.rs` | partial | EDID parsing and `DriverProfile` ported. EDID is diffed against the reference, with the fixture's checksum corrected (drm-cxx#241) and `hdr`/`wide_gamut` gating deliberately divergent. Mode lists, the HDR cache and connector/CRTC capabilities are not ported |
| `tests/unit/test_dma_buf_source_cache.cpp` | `drmkit-scene-sources` `cache.rs` | ported | Keyed reuse, re-import on geometry change, eviction, and no entry left behind by a failed import |
| `tests/unit/test_driver_profile.cpp` | `drmkit-display` `profile.rs` + `tests.rs` | ported | `decode_prime_caps` and `connector_type_self_refreshes`, plus a bit the reference does not set. `test_defaults` has no counterpart: `DriverProfile` has no `Default`, so a profile that was never probed cannot exist -- the conservative values it checks are `probe`'s fallbacks, covered by `an_unreported_capability_falls_back` |
| `tests/unit/test_dumb_buffer.cpp` | `drmkit-dumb` `src/tests.rs` | ported | 6 |
| `tests/unit/test_dumb_scanout_sink.cpp` | _drmkit-present_ (phase 6+) | pending |  |
| `tests/unit/test_egl_stream_builder.cpp` | _drmkit-scene-streams_ (phase 6+) | pending |  |
| `tests/unit/test_egl_stream_source.cpp` | _drmkit-scene-streams_ (phase 6+) | pending |  |
| `tests/unit/test_external_dma_buf_pool.cpp` | `drmkit-scene-sources` `pool.rs` | partial | Lazy import, key reuse, failed-import hold, generation reset with deferred retirement, session pause. Bounded LRU eviction is upstream's own follow-up and is not ported |
| `tests/unit/test_external_dma_buf_ring.cpp` | `drmkit-scene-sources` `ring.rs` + `presenter.rs` | ported | Per-slot import, idle-hold, release on supersede, fence forwarding, session pause |
| `tests/unit/test_external_dma_buf_source.cpp` | `drmkit-scene-sources` `external.rs` + `tests.rs` | ported | Eleven of fourteen. The three `Accepts*LayoutValidation` cases make their point by handing `create` a device built from fd -1 and checking the error is `bad_file_descriptor` rather than `invalid_argument`; a `&Device` here is a live DRM device by construction, so the same distinction is drawn from the other side -- a valid NV12, YUV420 or tiled layout must not come back `Invalid`. `RejectsBadDeviceFd` and `RejectsNonDrmDeviceFd` have no counterpart for the same reason |
| `tests/unit/test_format.cpp` | `drmkit-fmt` `src/tests.rs` | ported |  |
| `tests/unit/test_format_mod.cpp` | `drmkit-fmt` `src/tests.rs` | ported |  |
| `tests/unit/test_frame_economy.cpp` | _drmkit-present_ (phase 6+) | pending |  |
| `tests/unit/test_gbm.cpp` | `drmkit-gbm` | partial | Device open, allocation, geometry, dma-buf export, format refusal. `modifier` is unpinned on vkms (P-13) and surface allocation is not ported |
| `tests/unit/test_gbm_surface_source.cpp` | _drmkit-scene-gbm_ (phase 6+) | pending |  |
| `tests/unit/test_gl_compositor_math.cpp` | _drmkit-gl_ (phase 6+) | pending |  |
| `tests/unit/test_gst_appsink_source.cpp` | _drmkit-scene-gst_ (phase 6+) | pending |  |
| `tests/unit/test_hdr_metadata.cpp` | `drmkit-display` `hdr.rs` | ported | Twelve vectors diffed byte-for-byte against the reference's own serialiser via `parity/hdr-oracle.cc`, plus a NaN coordinate the C++ suite does not cover. `DefaultConstructedIsEmpty` has no counterpart: there is no empty blob to construct, which is what `Option` is for |
| `tests/unit/test_hdr_metadata_cache.cpp` | `drmkit-display` `hdr_cache.rs` | ported | All ten cases, plus session-loss and flush, which the C++ suite declares but does not exercise. The cache is generic over `HdrBlob` rather than reaching for the C++'s `synthesize_for_test` back door |
| `tests/unit/test_input.cpp` | `drmkit-input` `keyboard.rs`, `libinput_log.rs`, `seat.rs` | partial | The ten `KeyboardTest` cases, the log-routing ones, and `SeatTest` -- rewritten to assert what happens, since upstream's puts every assertion behind `has_value()` ([drm-cxx#248](https://github.com/jwinarske/drm-cxx/issues/248)). Plus a latching-modifier keymap and `lv(apostrophe)`, which upstream has no equivalent of ([drm-cxx#247](https://github.com/jwinarske/drm-cxx/issues/247)). Outstanding: the `PointerTest` accumulator and `EventDispatcherTest`, whose modules are not ported. The per-seat log sink is not ported -- see the crate docs |
| `tests/unit/test_key_repeater.cpp` | `drmkit-input` `repeat.rs` + `repeater.rs` | ported | All fifteen, plus a backlog cap that reports what it dropped and a zero delay that still repeats. `RejectsNullKeyboard` has no counterpart: the repeater does not hold a keyboard, it is told whether the key repeats |
| `tests/unit/test_layer.cpp` | `drmkit-planes` | ported | 4 |
| `tests/unit/test_log.cpp` | `drmkit-log` `src/tests.rs` | ported |  |
| `tests/unit/test_matching.cpp` | `drmkit-planes` | ported |  |
| `tests/unit/test_mode.cpp` | `drmkit-modeset` `src/tests.rs` | ported |  |
| `tests/unit/test_mode_list.cpp` | `drmkit-display` `modes.rs` | ported | Both cases, widened from the five connector types upstream checks to all twenty-one, against the names `drmModeGetConnectorTypeName` actually returns -- which is how the `S-Video` drift showed up ([drm-cxx#250](https://github.com/jwinarske/drm-cxx/issues/250)). Plus device cases upstream has none of: the listing diffed against `/sys/class/drm`, on vkms and on amdgpu |
| `tests/unit/test_negotiate.cpp` | _drmkit-present_ (phase 6+) | pending | Filed under `drmkit-scene` until 2026-08-25; it tests `drm::present::negotiate`, which lives in `src/present/`, so it belongs to the present spine and not to the minimal cut |
| `tests/unit/test_output.cpp` | `drmkit-planes` | ported |  |
| `tests/unit/test_output_signaling.cpp` | `drmkit-scene` `signaling.rs` | ported | All thirty-two. The gamut ranking is the enum's own `Ord` rather than a separate rank function, so a variant added in the wrong place fails a test instead of mis-ranking silently. Adobe RGB is checked to differ from BT.709 in green alone, which is what says the table was not copied from the wrong row |
| `tests/unit/test_page_flip.cpp` | `drmkit-modeset` `src/tests.rs` | ported | 3 |
| `tests/unit/test_plane_allocator.cpp` | `drmkit-planes` | ported |  |
| `tests/unit/test_playlist.cpp` | — (covers `examples/`, outside the ported `src/` surface) | n/a |  |
| `tests/unit/test_property_store.cpp` | `drmkit-core` `src/tests.rs` | ported |  |
| `tests/unit/test_scanout_target.cpp` | `drmkit-display` `scanout.rs` | ported | The `primary_plane_for_crtc` helper against a synthetic registry, plus a case upstream has none of: an overlay first in the list must not be taken for the primary. `discover` itself is device-bound and covered on vkms and amdgpu |
| `tests/unit/test_scene.cpp` | _drmkit-scene_ (phase 3) | pending |  |
| `tests/unit/test_scene_set.cpp` | _drmkit-scene-set_ (phase 6+) | pending |  |
| `tests/unit/test_select_connector.cpp` | `drmkit-display` `scanout.rs` | ported | All ten, plus two the reference cannot pass: a virtual display is rankable, and a real panel still outranks it. None of upstream's three rank arrays names `VIRTUAL`, so its examples find no display on vkms or in a VM -- [drm-cxx#253](https://github.com/jwinarske/drm-cxx/issues/253). Upstream keeps this in `examples/`; here it is public API, since "first connected" is what `ScanoutTarget::discover` would otherwise do |
| `tests/unit/test_stream_capability.cpp` | _drmkit-scene-streams_ (phase 6+) | pending |  |
| `tests/unit/test_sync_fence.cpp` | `drmkit-sync` `src/tests.rs` | ported |  |
| `tests/unit/test_test_patterns.cpp` | — (covers `examples/`, outside the ported `src/` surface) | n/a |  |
| `tests/unit/test_tone_mapper.cpp` | `drmkit-display` `tone_mapper.rs` | ported | Fifteen pixels diffed against the reference's own output, plus monotonicity, alpha passthrough, and black preservation |
| `tests/unit/test_v4l2_camera_source.cpp` | _drmkit-scene-v4l2_ (phase 6+) | pending |  |
| `tests/unit/test_v4l2_decoder_source.cpp` | _drmkit-scene-v4l2_ (phase 6+) | pending |  |
| `tests/unit/test_v4l2_plane_layout.cpp` | _drmkit-scene-v4l2_ (phase 6+) | pending |  |
| `tests/integration/test_capture_vkms.cpp` | `drmkit-capture` `tests/snapshot_vkms.rs` | ported | `RoundTripPrimaryPlane` -- the same four-quadrant pattern through a real modeset and back -- plus three the reference does not have: the snapshot's size, a PNG that decodes back, and what happens without the atomic cap |
| `tests/integration/test_crtc_color_pipeline_vkms.cpp` | `drmkit-display` `pipeline.rs` + `tests/pipeline_device.rs` | partial | Probe, absent-stage refusal, blob staging and replacement, and what an apply writes -- run on vkms, whose CRTC is gamma-only, and on amdgpu, whose CRTC has all three. `IdentityGammaApplyAndCommit`'s commit half is not ported: it needs DRM master, which these cases deliberately do not take |
| `tests/integration/test_cursor_renderer_legacy.cpp` | `drmkit-cursor` `tests/renderer_vkms.rs` | ported | The legacy path is forced by offering a registry with no cursor or overlay plane, so it is exercised on drivers that would otherwise never choose it. Covers the disable-probe, install-then-move, hiding, and the two refusals: rotation, which the call cannot express, and a caller who forbade the path |
| `tests/integration/test_cursor_source.cpp` | `drmkit-scene-sources` `cursor.rs` | ported | The sprite round trip, hotspot and dimensions, plus four the reference has no equivalent of: the buffer sized to the largest frame, a smaller frame not leaving the previous one's edges around it, damage reported only when the frame changes, and a static cursor never repainting. Fixtures are 16x16 rather than 4x4 because vkms refuses anything below 10x10 and the reference's silently skips there -- [drm-cxx#254](https://github.com/jwinarske/drm-cxx/issues/254) |
| `tests/integration/test_dma_buf_source_cache_vkms.cpp` | _drmkit-scene-sources_ (phase 3) | pending |  |
| `tests/integration/test_dumb_buffer_p010_vkms.cpp` | _drmkit-dumb_ (phase 1) | pending |  |
| `tests/integration/test_dumb_ring_source.cpp` | _drmkit-present_ (phase 6+) | pending |  |
| `tests/integration/test_dumb_scanout_sink_vkms.cpp` | _drmkit-present_ (phase 6+) | pending |  |
| `tests/integration/test_egl_streams_hw.cpp` | _drmkit-scene-streams_ (phase 6+) | pending |  |
| `tests/integration/test_explicit_sync_fence_gl.cpp` | _drmkit-gl_ (phase 6+) | pending |  |
| `tests/integration/test_external_dma_buf_pool_vkms.cpp` | _drmkit-scene-sources_ (phase 3) | pending |  |
| `tests/integration/test_external_dma_buf_ring_scene_vkms.cpp` | _drmkit-scene-sources_ (phase 3) | pending |  |
| `tests/integration/test_external_dma_buf_ring_vkms.cpp` | _drmkit-scene-sources_ (phase 3) | pending |  |
| `tests/integration/test_gbm_surface_source_vkms.cpp` | _drmkit-scene-gbm_ (phase 6+) | pending |  |
| `tests/integration/test_gl_compositor_readback.cpp` | _drmkit-gl_ (phase 6+) | pending |  |
| `tests/integration/test_gst_appsink_source_vkms.cpp` | _drmkit-scene-gst_ (phase 6+) | pending |  |
| `tests/integration/test_layer_scene_app_priority_vkms.cpp` | _drmkit-scene_ (phase 3) | pending |  |
| `tests/integration/test_layer_scene_census_vkms.cpp` | _drmkit-scene_ (phase 3) | pending | 4 |
| `tests/integration/test_layer_scene_composition_vkms.cpp` | _drmkit-scene_ (phase 3) | pending |  |
| `tests/integration/test_layer_scene_content_type_vkms.cpp` | _drmkit-scene_ (phase 3) | pending |  |
| `tests/integration/test_layer_scene_minimization_vkms.cpp` | _drmkit-scene_ (phase 3) | pending | 4 |
| `tests/integration/test_layer_scene_pin_vkms.cpp` | _drmkit-scene_ (phase 3) | pending |  |
| `tests/integration/test_layer_scene_rebind_vkms.cpp` | _drmkit-scene_ (phase 3) | pending |  |
| `tests/integration/test_layer_scene_release_vkms.cpp` | _drmkit-scene_ (phase 3) | pending | 1, 2, 5 |
| `tests/integration/test_scanout_backend_vkms.cpp` | _drmkit-present_ (phase 6+) | pending |  |
| `tests/integration/test_scene_set_vkms.cpp` | _drmkit-scene-set_ (phase 6+) | pending |  |
| `tests/integration/test_v4l2_camera_source_vivid.cpp` | _drmkit-scene-v4l2_ (phase 6+) | pending |  |
| `tests/integration/test_v4l2_decoder_source_vicodec.cpp` | _drmkit-scene-v4l2_ (phase 6+) | pending |  |
| `tests/integration/test_vgem_buffer.cpp` | _drmkit-dumb_ (phase 1) | pending |  |

## Summary

- `ported`: 14
- `partial`: 1
- `pending`: 80
- `n/a`: 2
