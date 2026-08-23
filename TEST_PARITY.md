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
| `tests/unit/test_acquire_fence.cpp` | _drmkit-scene-sources_ (phase 3) | pending |  |
| `tests/unit/test_atomic_builder.cpp` | `drmkit-core` `src/tests.rs` | ported |  |
| `tests/unit/test_buffer_mapping.cpp` | `drmkit-dumb` `src/tests.rs` | partial | 6 |
| `tests/unit/test_buffer_ring.cpp` | _drmkit-present_ (phase 6+) | pending |  |
| `tests/unit/test_canvas_format.cpp` | _drmkit-scene_ (phase 3) | pending |  |
| `tests/unit/test_capture_jpg.cpp` | _drmkit-capture_ (phase 6+) | pending |  |
| `tests/unit/test_capture_snapshot.cpp` | _drmkit-capture_ (phase 4) | pending |  |
| `tests/unit/test_color_pipeline_curves.cpp` | _drmkit-display_ (phase 4) | pending |  |
| `tests/unit/test_commit_report.cpp` | _drmkit-scene_ (phase 3) | pending |  |
| `tests/unit/test_composite_canvas.cpp` | _drmkit-scene_ (phase 3) | pending |  |
| `tests/unit/test_connector_capabilities.cpp` | _drmkit-display_ (phase 4) | pending |  |
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
| `tests/unit/test_cursor_cursor.cpp` | _drmkit-cursor_ (phase 4) | pending |  |
| `tests/unit/test_cursor_theme.cpp` | _drmkit-cursor_ (phase 4) | pending |  |
| `tests/unit/test_device.cpp` | `drmkit-core` `src/tests.rs` | ported |  |
| `tests/unit/test_display.cpp` | _drmkit-display_ (phase 4) | pending |  |
| `tests/unit/test_dma_buf_source_cache.cpp` | _drmkit-scene-sources_ (phase 3) | pending |  |
| `tests/unit/test_driver_profile.cpp` | _drmkit-display_ (phase 4) | pending |  |
| `tests/unit/test_dumb_buffer.cpp` | `drmkit-dumb` `src/tests.rs` | ported | 6 |
| `tests/unit/test_dumb_scanout_sink.cpp` | _drmkit-present_ (phase 6+) | pending |  |
| `tests/unit/test_egl_stream_builder.cpp` | _drmkit-scene-streams_ (phase 6+) | pending |  |
| `tests/unit/test_egl_stream_source.cpp` | _drmkit-scene-streams_ (phase 6+) | pending |  |
| `tests/unit/test_external_dma_buf_pool.cpp` | _drmkit-scene-sources_ (phase 3) | pending |  |
| `tests/unit/test_external_dma_buf_ring.cpp` | _drmkit-scene-sources_ (phase 3) | pending |  |
| `tests/unit/test_external_dma_buf_source.cpp` | _drmkit-scene-sources_ (phase 3) | pending |  |
| `tests/unit/test_format.cpp` | `drmkit-fmt` `src/tests.rs` | ported |  |
| `tests/unit/test_format_mod.cpp` | `drmkit-fmt` `src/tests.rs` | ported |  |
| `tests/unit/test_frame_economy.cpp` | _drmkit-present_ (phase 6+) | pending |  |
| `tests/unit/test_gbm.cpp` | _drmkit-gbm_ (phase 4) | pending |  |
| `tests/unit/test_gbm_surface_source.cpp` | _drmkit-scene-gbm_ (phase 6+) | pending |  |
| `tests/unit/test_gl_compositor_math.cpp` | _drmkit-gl_ (phase 6+) | pending |  |
| `tests/unit/test_gst_appsink_source.cpp` | _drmkit-scene-gst_ (phase 6+) | pending |  |
| `tests/unit/test_hdr_metadata.cpp` | _drmkit-display_ (phase 4) | pending |  |
| `tests/unit/test_hdr_metadata_cache.cpp` | _drmkit-display_ (phase 4) | pending |  |
| `tests/unit/test_input.cpp` | _drmkit-input_ (phase 4) | pending |  |
| `tests/unit/test_key_repeater.cpp` | _drmkit-input_ (phase 4) | pending |  |
| `tests/unit/test_layer.cpp` | _drmkit-planes_ (phase 2) | pending | 4 |
| `tests/unit/test_log.cpp` | `drmkit-log` `src/tests.rs` | ported |  |
| `tests/unit/test_matching.cpp` | _drmkit-planes_ (phase 2) | pending |  |
| `tests/unit/test_mode.cpp` | `drmkit-modeset` `src/tests.rs` | ported |  |
| `tests/unit/test_mode_list.cpp` | _drmkit-modeset_ (phase 1) | pending |  |
| `tests/unit/test_negotiate.cpp` | _drmkit-scene_ (phase 3) | pending |  |
| `tests/unit/test_output.cpp` | _drmkit-planes_ (phase 2) | pending |  |
| `tests/unit/test_output_signaling.cpp` | _drmkit-scene_ (phase 3) | pending |  |
| `tests/unit/test_page_flip.cpp` | `drmkit-modeset` `src/tests.rs` | ported | 3 |
| `tests/unit/test_plane_allocator.cpp` | _drmkit-planes_ (phase 2) | pending |  |
| `tests/unit/test_playlist.cpp` | — (covers `examples/`, outside the ported `src/` surface) | n/a |  |
| `tests/unit/test_property_store.cpp` | `drmkit-core` `src/tests.rs` | ported |  |
| `tests/unit/test_scanout_target.cpp` | _drmkit-present_ (phase 6+) | pending |  |
| `tests/unit/test_scene.cpp` | _drmkit-scene_ (phase 3) | pending |  |
| `tests/unit/test_scene_set.cpp` | _drmkit-scene-set_ (phase 6+) | pending |  |
| `tests/unit/test_select_connector.cpp` | _drmkit-display_ (phase 4) | pending |  |
| `tests/unit/test_stream_capability.cpp` | _drmkit-scene-streams_ (phase 6+) | pending |  |
| `tests/unit/test_sync_fence.cpp` | `drmkit-sync` `src/tests.rs` | ported |  |
| `tests/unit/test_test_patterns.cpp` | — (covers `examples/`, outside the ported `src/` surface) | n/a |  |
| `tests/unit/test_tone_mapper.cpp` | _drmkit-display_ (phase 4) | pending |  |
| `tests/unit/test_v4l2_camera_source.cpp` | _drmkit-scene-v4l2_ (phase 6+) | pending |  |
| `tests/unit/test_v4l2_decoder_source.cpp` | _drmkit-scene-v4l2_ (phase 6+) | pending |  |
| `tests/unit/test_v4l2_plane_layout.cpp` | _drmkit-scene-v4l2_ (phase 6+) | pending |  |
| `tests/integration/test_capture_vkms.cpp` | _drmkit-capture_ (phase 4) | pending |  |
| `tests/integration/test_crtc_color_pipeline_vkms.cpp` | _drmkit-display_ (phase 4) | pending |  |
| `tests/integration/test_cursor_renderer_legacy.cpp` | _drmkit-cursor_ (phase 4) | pending |  |
| `tests/integration/test_cursor_source.cpp` | _drmkit-cursor_ (phase 4) | pending |  |
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

- `ported`: 10
- `partial`: 1
- `pending`: 84
- `n/a`: 2
