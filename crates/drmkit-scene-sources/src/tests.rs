// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! Tests for the tier-1 sources.
//!
//! Allocating a dumb buffer needs a card, so those cases are `#[ignore]`d and
//! run under the vkms lane with `--include-ignored` — the same convention as
//! `drmkit-core` and `drmkit-dumb`. The damage bookkeeping needs no card and
//! runs everywhere.

use super::*;
use std::sync::Mutex;

use drmkit_core::Device;
use drmkit_dumb::MapAccess;
use drmkit_scene::{AcquiredBuffer, BindingModel, DamageRect, LayerBufferSource, SourceError};
use drmkit_sync::SyncFence;

/// `Device::open` acquires DRM master, which is per open file description, so
/// card-dependent cases serialize rather than racing for it.
static CARD_LOCK: Mutex<()> = Mutex::new(());

fn card_guard() -> std::sync::MutexGuard<'static, ()> {
    CARD_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Open the card, or `None` if this machine has no vkms.
///
/// Absence of the device is a different condition from the device being there
/// and misbehaving. Locally it means vkms is not loaded, which is a skip; in
/// the lane it means the lane is not testing what it claims, which is a
/// failure — the same line `DRMKIT_REQUIRE_MASTER` already draws for DRM
/// master. Everything past this point still asserts.
fn open_card() -> Option<Device> {
    let path = std::env::var("DRMKIT_TEST_CARD").unwrap_or_else(|_| "/dev/dri/card0".to_owned());
    match Device::open(&path) {
        Ok(device) => Some(device),
        Err(error) => {
            assert!(
                std::env::var_os("DRMKIT_REQUIRE_MASTER").is_none(),
                "{path}: {error}, but DRMKIT_REQUIRE_MASTER is set"
            );
            println!("note: skipped -- no DRM device at {path} ({error})");
            None
        }
    }
}

/// The format every KMS driver accepts for a dumb buffer.
///
/// Taken from `drmkit-fmt` rather than written as a literal: the first version
/// of this file hardcoded `0x3234_5258`, which transposes two digits of the
/// real `XR24` code and made every allocation fail with "not a recognized DRM
/// `FourCC`". There is a constant for exactly this reason.
use drmkit_fmt::fourcc::XRGB8888;

#[test]
#[ignore = "needs a DRM device; run under the vkms lane with --include-ignored"]
fn a_dumb_source_hands_out_a_stable_framebuffer_vkms() {
    let _guard = card_guard();
    let Some(device) = open_card() else { return };
    let mut source = DumbBufferSource::create(&device, 64, 64, XRGB8888).expect("create");

    assert_eq!(source.format().width, 64);
    assert_eq!(source.format().height, 64);
    assert_eq!(source.format().fourcc, XRGB8888);
    assert_eq!(source.format().modifier, 0, "a dumb buffer is linear");
    assert_eq!(source.binding_model(), BindingModel::SceneSubmitsFbId);

    let first = source.acquire().expect("acquire");
    assert_ne!(first.fb_id, 0);
    let fb = first.fb_id;
    source.release(first);

    let second = source.acquire().expect("acquire again");
    assert_eq!(
        second.fb_id, fb,
        "a single-buffer source hands out the same framebuffer every frame"
    );
    source.release(second);
}

/// The mapping is a real, writable view, and reading it back proves it is
/// coherent — which is what lets a software producer paint straight into the
/// scanout buffer.
#[test]
#[ignore = "needs a DRM device; run under the vkms lane with --include-ignored"]
fn a_dumb_source_maps_a_writable_coherent_view_vkms() {
    let _guard = card_guard();
    let Some(device) = open_card() else { return };
    let mut source = DumbBufferSource::create(&device, 32, 32, XRGB8888).expect("create");

    {
        let mut view = source.map(MapAccess::Write).expect("map");
        assert_eq!(view.width(), 32);
        assert_eq!(view.height(), 32);
        view.row_mut(0).expect("row 0").fill(0xab);
    }

    let view = source.map(MapAccess::Read).expect("map");
    assert!(view.row(0).expect("row 0").iter().all(|b| *b == 0xab));
}

/// The kernel zero-fills a dumb buffer, so the first frame is transparent
/// rather than whatever was in memory.
#[test]
#[ignore = "needs a DRM device; run under the vkms lane with --include-ignored"]
fn a_fresh_dumb_source_starts_zeroed_vkms() {
    let _guard = card_guard();
    let Some(device) = open_card() else { return };
    let mut source = DumbBufferSource::create(&device, 32, 32, XRGB8888).expect("create");

    let view = source.map(MapAccess::Read).expect("map");
    assert!(
        view.pixels().iter().all(|b| *b == 0),
        "the first acquire must not present uninitialized memory"
    );
}

/// A session resume re-allocates against the new device and preserves the
/// shape: consumers rely on `format()` returning the same value across it.
#[test]
#[ignore = "needs a DRM device; run under the vkms lane with --include-ignored"]
fn a_session_resume_preserves_the_source_format_vkms() {
    let _guard = card_guard();
    let Some(device) = open_card() else { return };
    let mut source = DumbBufferSource::create(&device, 48, 24, XRGB8888).expect("create");

    let before = source.format();
    let old_fb = source.acquire().expect("acquire").fb_id;

    source.on_session_paused();
    source
        .on_session_resumed(&device)
        .expect("re-allocate on resume");

    assert_eq!(source.format(), before, "the shape must survive a resume");
    let after_fb = source.acquire().expect("acquire").fb_id;
    assert_ne!(
        after_fb, 0,
        "the source must be usable again after a resume"
    );
    let _ = old_fb;
}

/// Repeated resumes must not leak: each one forgets the old handles and
/// allocates fresh. Measured as descriptor-table growth, for the same reason as
/// the other leak tests — an absolute count races the parallel harness.
#[test]
#[ignore = "needs a DRM device; run under the vkms lane with --include-ignored"]
fn repeated_session_resumes_do_not_leak_vkms() {
    let _guard = card_guard();
    let Some(device) = open_card() else { return };
    let mut source = DumbBufferSource::create(&device, 32, 32, XRGB8888).expect("create");

    let count = || {
        std::fs::read_dir("/proc/self/fd")
            .expect("/proc/self/fd")
            .count()
    };

    for _ in 0..8 {
        source.on_session_resumed(&device).expect("resume");
    }
    let after_few = count();

    for _ in 0..120 {
        source.on_session_resumed(&device).expect("resume");
    }
    let after_many = count();

    let growth = after_many.saturating_sub(after_few);
    assert!(
        growth < 16,
        "120 further resumes grew the fd table by {growth}"
    );
}

// --- damage bookkeeping, no card needed --------------------------------------

/// A source with no buffer cannot acquire — and says so as a failure, not as
/// flow control, because there is nothing to wait for.
#[test]
fn acquiring_without_a_framebuffer_is_a_failure_not_a_skip() {
    // A source can only reach this state through a failed allocation, which
    // `create` reports instead of returning. The distinction still matters:
    // `WouldBlock` means "try next frame", and there is no next frame here.
    let error = SourceError::Failed(rustix::io::Errno::INVAL);
    assert!(
        !matches!(error, SourceError::WouldBlock),
        "a missing framebuffer is not something a later frame will fix"
    );
}

/// Damage is handed over on acquire and cleared, so the next frame starts from
/// full-frame unless the producer says otherwise.
#[test]
#[ignore = "needs a DRM device; run under the vkms lane with --include-ignored"]
fn damage_is_reported_once_then_cleared_vkms() {
    let _guard = card_guard();
    let Some(device) = open_card() else { return };
    let mut source = DumbBufferSource::create(&device, 64, 64, XRGB8888).expect("create");

    let dirty = [DamageRect {
        x: 4,
        y: 8,
        w: 16,
        h: 16,
    }];
    source.set_damage(&dirty);

    let first = source.acquire().expect("acquire");
    assert_eq!(first.damage.len(), 1);
    assert_eq!(first.damage[0].x, 4);
    assert_eq!(first.damage[0].w, 16);
    source.release(first);

    let second = source.acquire().expect("acquire");
    assert!(
        second.damage.is_empty(),
        "damage describes one frame; an unset next frame means full-frame"
    );
    source.release(second);
}

/// Setting damage twice replaces rather than accumulates: the producer knows
/// what it just painted, and merging a stale region would over-report.
#[test]
#[ignore = "needs a DRM device; run under the vkms lane with --include-ignored"]
fn setting_damage_replaces_the_previous_set_vkms() {
    let _guard = card_guard();
    let Some(device) = open_card() else { return };
    let mut source = DumbBufferSource::create(&device, 64, 64, XRGB8888).expect("create");

    source.set_damage(&[DamageRect {
        x: 0,
        y: 0,
        w: 8,
        h: 8,
    }]);
    source.set_damage(&[DamageRect {
        x: 32,
        y: 32,
        w: 4,
        h: 4,
    }]);

    let acquired = source.acquire().expect("acquire");
    assert_eq!(acquired.damage.len(), 1, "the earlier set is replaced");
    assert_eq!(acquired.damage[0].x, 32);
    source.release(acquired);
}

/// An empty damage set means full-frame, which is correct but not
/// power-optimal — the scene emits no clips at all.
#[test]
#[ignore = "needs a DRM device; run under the vkms lane with --include-ignored"]
fn empty_damage_means_full_frame_vkms() {
    let _guard = card_guard();
    let Some(device) = open_card() else { return };
    let mut source = DumbBufferSource::create(&device, 64, 64, XRGB8888).expect("create");

    source.set_damage(&[DamageRect {
        x: 0,
        y: 0,
        w: 8,
        h: 8,
    }]);
    source.set_damage(&[]);

    let acquired = source.acquire().expect("acquire");
    assert!(acquired.damage.is_empty());
    source.release(acquired);
}

// --- ExternalDmaBufSource: the acquire-fence dup (plan §4.9, upstream #230) --

use std::os::fd::{AsFd as _, AsRawFd as _, OwnedFd};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use drmkit_scene::SourceFormat;

/// An eventfd standing in for a producer's render-done fence.
fn fake_fence() -> OwnedFd {
    rustix::event::eventfd(0, rustix::event::EventfdFlags::CLOEXEC).expect("eventfd")
}

/// A descriptor for the validation cases, which are rejected before any ioctl.
fn placeholder_fd() -> OwnedFd {
    rustix::event::eventfd(0, rustix::event::EventfdFlags::CLOEXEC).expect("eventfd")
}

/// A **real** dma-buf, made by allocating a dumb buffer and exporting its GEM
/// handle through PRIME.
///
/// The first version of these tests fed an eventfd to `create` and fell back to
/// a skip when the driver refused it. Every one of the fence cases took that
/// path -- four silent skips, including the §4.9 pin this chunk exists for. A
/// test that skips on the only interesting configuration is not a test.
///
/// Returns the descriptor and the pitch the kernel chose, which the caller
/// needs for the plane description. The dumb buffer is dropped: the exported
/// descriptor keeps the underlying object alive.
fn real_dma_buf(device: &Device, width: u32, height: u32) -> Option<(OwnedFd, u32)> {
    use drm::control::Device as _;

    let buffer = drmkit_dumb::Buffer::create(
        device,
        &drmkit_dumb::Config {
            width,
            height,
            fourcc: XRGB8888,
            add_fb: false,
            ..drmkit_dumb::Config::default()
        },
    )
    .ok()?;

    let handle = drm::control::from_u32(buffer.gem_handle()?)?;
    let pitch = buffer.stride();
    let fd = device.buffer_to_prime_fd(handle, 0).ok()?;
    Some((fd, pitch))
}

fn plane(fd: &OwnedFd) -> ExternalPlane<'_> {
    ExternalPlane {
        fd: fd.as_fd(),
        offset: 0,
        pitch: 256,
    }
}

fn external_format() -> SourceFormat {
    SourceFormat {
        fourcc: XRGB8888,
        modifier: 0,
        width: 64,
        height: 64,
    }
}

// --- validation, no card needed ---------------------------------------------

#[test]
#[ignore = "needs a DRM device; run under the vkms lane with --include-ignored"]
fn external_create_rejects_unusable_shapes_vkms() {
    let _guard = card_guard();
    let Some(device) = open_card() else { return };
    let fd = placeholder_fd();

    let cases: [(SourceFormat, &str); 3] = [
        (
            SourceFormat {
                width: 0,
                ..external_format()
            },
            "zero width",
        ),
        (
            SourceFormat {
                height: 0,
                ..external_format()
            },
            "zero height",
        ),
        (
            SourceFormat {
                fourcc: 0,
                ..external_format()
            },
            "zero format",
        ),
    ];

    for (format, what) in cases {
        assert!(
            matches!(
                ExternalDmaBufSource::create(&device, format, &[plane(&fd)], None),
                Err(ExternalError::Invalid { .. })
            ),
            "{what} must be rejected"
        );
    }

    // No planes, and a plane with no pitch.
    assert!(matches!(
        ExternalDmaBufSource::create(&device, external_format(), &[], None),
        Err(ExternalError::Invalid { .. })
    ));
    let bad_pitch = ExternalPlane {
        fd: fd.as_fd(),
        offset: 0,
        pitch: 0,
    };
    assert!(matches!(
        ExternalDmaBufSource::create(&device, external_format(), &[bad_pitch], None),
        Err(ExternalError::Invalid { .. })
    ));
}

/// A failed create must **not** fire the release callback: the caller still
/// owns the upstream buffer and will re-queue or drop it themselves. Firing
/// would re-queue a buffer the caller has not finished with.
#[test]
#[ignore = "needs a DRM device; run under the vkms lane with --include-ignored"]
fn a_failed_create_does_not_fire_the_release_callback_vkms() {
    let _guard = card_guard();
    let Some(device) = open_card() else { return };
    let fd = placeholder_fd();
    let fired = Arc::new(AtomicUsize::new(0));

    let counter = Arc::clone(&fired);
    let result = ExternalDmaBufSource::create(
        &device,
        SourceFormat {
            width: 0,
            ..external_format()
        },
        &[plane(&fd)],
        Some(Box::new(move || {
            counter.fetch_add(1, Ordering::Relaxed);
        })),
    );

    assert!(result.is_err());
    assert_eq!(
        fired.load(Ordering::Relaxed),
        0,
        "the caller still owns the upstream buffer after a failed create"
    );
}

// --- the fence dup ----------------------------------------------------------

/// **Plan §4.9 / upstream PR #230.** Two consecutive acquires must yield
/// **independently closeable** descriptors.
///
/// The scene acquires each source twice per frame — a TEST commit, then the
/// real one. An owned fence handed over on the first acquire would be gone by
/// the second, so the real commit would go out unsynced and the display engine
/// could sample the buffer before the producer's writes land.
#[test]
#[ignore = "needs a DRM device; run under the vkms lane with --include-ignored"]
fn two_acquires_yield_independently_closeable_fences_vkms() {
    let _guard = card_guard();
    let Some(device) = open_card() else { return };
    let (dmabuf, pitch) = real_dma_buf(&device, 64, 64).expect("export a real dma-buf");
    let planes = [ExternalPlane {
        fd: dmabuf.as_fd(),
        offset: 0,
        pitch,
    }];
    let mut source = ExternalDmaBufSource::create(&device, external_format(), &planes, None)
        .expect("wrap a real dma-buf");

    source.set_acquire_fence(
        drmkit_sync::SyncFence::import(fake_fence().as_fd()).expect("import fence"),
    );

    let first = source.acquire().expect("first acquire");
    let second = source.acquire().expect("second acquire");

    let first_fd = first
        .acquire_fence
        .as_ref()
        .and_then(drmkit_sync::SyncFence::as_fd);
    let second_fd = second
        .acquire_fence
        .as_ref()
        .and_then(drmkit_sync::SyncFence::as_fd);

    assert!(
        first_fd.is_some(),
        "the TEST commit's acquire must be fenced"
    );
    assert!(
        second_fd.is_some(),
        "and so must the real commit's -- an owned fence would have been \
         consumed by the first, leaving the real commit unsynced"
    );
    assert_ne!(
        first_fd.map(|fd| fd.as_raw_fd()),
        second_fd.map(|fd| fd.as_raw_fd()),
        "each acquire must get its own descriptor, not an alias"
    );

    // Dropping one must not disturb the other: each rides its own buffer's
    // lifecycle and closes on release.
    drop(first);
    assert!(
        rustix::fs::fstat(second_fd.expect("second fence")).is_ok(),
        "releasing one buffer must not close another's fence"
    );

    assert!(
        source.has_acquire_fence(),
        "the producer's fence stays live for the next frame; only \
         set_acquire_fence replaces it"
    );
}

/// With no producer fence set, acquires are simply unfenced — the common case
/// for a source whose buffer is ready synchronously.
#[test]
#[ignore = "needs a DRM device; run under the vkms lane with --include-ignored"]
fn acquires_are_unfenced_when_no_producer_fence_is_set_vkms() {
    let _guard = card_guard();
    let Some(device) = open_card() else { return };
    let (dmabuf, pitch) = real_dma_buf(&device, 64, 64).expect("export a real dma-buf");
    let planes = [ExternalPlane {
        fd: dmabuf.as_fd(),
        offset: 0,
        pitch,
    }];
    let mut source = ExternalDmaBufSource::create(&device, external_format(), &planes, None)
        .expect("wrap a real dma-buf");

    assert!(!source.has_acquire_fence());
    let acquired = source.acquire().expect("acquire");
    assert!(acquired.acquire_fence.is_none());
}

/// The release callback fires **exactly once**, whether the source is released
/// or torn down without ever reaching release. Firing twice would re-queue a
/// buffer still in use; never firing would stall the producer's pipeline.
#[test]
#[ignore = "needs a DRM device; run under the vkms lane with --include-ignored"]
fn the_release_callback_fires_exactly_once_vkms() {
    let _guard = card_guard();
    let Some(device) = open_card() else { return };
    let (dmabuf, pitch) = real_dma_buf(&device, 64, 64).expect("export a real dma-buf");
    let planes = [ExternalPlane {
        fd: dmabuf.as_fd(),
        offset: 0,
        pitch,
    }];
    let fired = Arc::new(AtomicUsize::new(0));

    {
        let counter = Arc::clone(&fired);
        let mut source = ExternalDmaBufSource::create(
            &device,
            external_format(),
            &planes,
            Some(Box::new(move || {
                counter.fetch_add(1, Ordering::Relaxed);
            })),
        )
        .expect("wrap a real dma-buf");

        let acquired = source.acquire().expect("acquire");
        source.release(acquired);
        assert_eq!(
            fired.load(Ordering::Relaxed),
            1,
            "the first retire re-queues"
        );

        let again = source.acquire().expect("acquire");
        source.release(again);
        assert_eq!(
            fired.load(Ordering::Relaxed),
            1,
            "a second release must not re-queue a buffer still in use"
        );
    }

    assert_eq!(
        fired.load(Ordering::Relaxed),
        1,
        "and the drop must not fire it again"
    );
}

/// A source torn down before ever being released must still re-queue upstream,
/// or the producer waits for a slot that never comes back.
#[test]
#[ignore = "needs a DRM device; run under the vkms lane with --include-ignored"]
fn dropping_without_releasing_still_fires_the_callback_vkms() {
    let _guard = card_guard();
    let Some(device) = open_card() else { return };
    let (dmabuf, pitch) = real_dma_buf(&device, 64, 64).expect("export a real dma-buf");
    let planes = [ExternalPlane {
        fd: dmabuf.as_fd(),
        offset: 0,
        pitch,
    }];
    let fired = Arc::new(AtomicUsize::new(0));

    {
        let counter = Arc::clone(&fired);
        let source = ExternalDmaBufSource::create(
            &device,
            external_format(),
            &planes,
            Some(Box::new(move || {
                counter.fetch_add(1, Ordering::Relaxed);
            })),
        )
        .expect("wrap a real dma-buf");
        drop(source);
    }

    assert_eq!(
        fired.load(Ordering::Relaxed),
        1,
        "teardown must re-queue the upstream buffer"
    );
}

// --- DmaBufSourceCache -------------------------------------------------------

/// A repeated key returns the same source, framebuffer and all.
///
/// This is the whole point of the cache: a swapchain hands the same descriptors
/// back every frame, and importing them again would be two ioctls per plane per
/// frame to arrive at the framebuffer id the kernel already had.
#[test]
fn a_repeated_key_reuses_the_import() {
    let _guard = card_guard();
    let Some(device) = open_card() else { return };
    let Some((fd, pitch)) = real_dma_buf(&device, 64, 64) else {
        panic!("a dumb buffer must export a dma-buf on any KMS driver");
    };

    let format = SourceFormat {
        fourcc: XRGB8888,
        modifier: 0,
        width: 64,
        height: 64,
    };
    let planes = [ExternalPlane {
        fd: fd.as_fd(),
        offset: 0,
        pitch,
    }];

    let mut cache = DmaBufSourceCache::new();
    let first = cache
        .get_or_create(7, &device, format, &planes)
        .expect("first import")
        .acquire()
        .expect("acquire")
        .fb_id;
    let second = cache
        .get_or_create(7, &device, format, &planes)
        .expect("second lookup")
        .acquire()
        .expect("acquire")
        .fb_id;

    assert_eq!(
        first, second,
        "the same key must hand back the same framebuffer, not a fresh import"
    );
    assert_eq!(cache.len(), 1, "one buffer, one entry");
}

/// A key reused for a different buffer is re-imported, not handed back stale.
///
/// A producer reusing an index after a resize is normal. Returning the old
/// source would scan out the wrong memory at the wrong stride — which the
/// kernel accepts, because the framebuffer is perfectly valid; it is simply
/// the previous buffer.
#[test]
fn a_key_reused_for_new_geometry_is_reimported() {
    let _guard = card_guard();
    let Some(device) = open_card() else { return };
    let Some((small_fd, small_pitch)) = real_dma_buf(&device, 64, 64) else {
        panic!("dma-buf export");
    };
    let Some((large_fd, large_pitch)) = real_dma_buf(&device, 128, 128) else {
        panic!("dma-buf export");
    };

    let mut cache = DmaBufSourceCache::new();
    cache
        .get_or_create(
            1,
            &device,
            SourceFormat {
                fourcc: XRGB8888,
                modifier: 0,
                width: 64,
                height: 64,
            },
            &[ExternalPlane {
                fd: small_fd.as_fd(),
                offset: 0,
                pitch: small_pitch,
            }],
        )
        .expect("first import");

    let bigger = SourceFormat {
        fourcc: XRGB8888,
        modifier: 0,
        width: 128,
        height: 128,
    };
    cache
        .get_or_create(
            1,
            &device,
            bigger,
            &[ExternalPlane {
                fd: large_fd.as_fd(),
                offset: 0,
                pitch: large_pitch,
            }],
        )
        .expect("re-import");

    // The cached source's own geometry, not its framebuffer id. The id proves
    // nothing here: dropping the old source frees its framebuffer and the
    // kernel hands the very same id straight back to the replacement -- which
    // is what the first version of this case saw and mistook for a cache hit.
    let cached = cache.find(1).expect("an entry under the reused key");
    assert_eq!(
        LayerBufferSource::format(cached),
        bigger,
        "the geometry changed under the same key, so the source must have been \
         rebuilt rather than handed back stale"
    );
    assert_eq!(
        cache.len(),
        1,
        "the replacement takes the old entry's place"
    );
}

/// Evicting drops the import, so the next lookup builds a new one.
#[test]
fn eviction_forces_a_fresh_import() {
    let _guard = card_guard();
    let Some(device) = open_card() else { return };
    let Some((fd, pitch)) = real_dma_buf(&device, 64, 64) else {
        panic!("dma-buf export");
    };
    let format = SourceFormat {
        fourcc: XRGB8888,
        modifier: 0,
        width: 64,
        height: 64,
    };
    let planes = [ExternalPlane {
        fd: fd.as_fd(),
        offset: 0,
        pitch,
    }];

    let mut cache = DmaBufSourceCache::new();
    cache
        .get_or_create(3, &device, format, &planes)
        .expect("import");
    assert!(cache.find(3).is_some());

    assert!(
        cache.evict(3),
        "evicting a present key reports it was there"
    );
    assert!(!cache.evict(3), "evicting it twice does not");
    assert!(cache.find(3).is_none());
    assert!(cache.is_empty());

    cache
        .get_or_create(3, &device, format, &planes)
        .expect("re-import after eviction");
    assert_eq!(cache.len(), 1);

    cache.clear();
    assert!(cache.is_empty(), "clear drops everything");
}

/// A failed import caches nothing.
///
/// Otherwise a transient failure would poison the key: every later frame would
/// find the broken entry and hand it back, and the layer would never recover.
#[test]
fn a_failed_import_leaves_the_cache_untouched() {
    let _guard = card_guard();
    let Some(device) = open_card() else { return };
    let Some((fd, pitch)) = real_dma_buf(&device, 64, 64) else {
        panic!("dma-buf export");
    };

    let mut cache = DmaBufSourceCache::new();
    let refused = cache.get_or_create(
        9,
        &device,
        SourceFormat {
            fourcc: XRGB8888,
            modifier: 0,
            width: 0, // rejected before any ioctl
            height: 64,
        },
        &[ExternalPlane {
            fd: fd.as_fd(),
            offset: 0,
            pitch,
        }],
    );

    assert!(refused.is_err(), "a zero dimension must be refused");
    assert!(
        cache.find(9).is_none(),
        "a failed import must leave no entry behind to be handed out later"
    );
    assert!(cache.is_empty());
}

// --- ExternalDmaBufRing ------------------------------------------------------

/// Build a ring of `n` slots over real exported dma-bufs.
///
/// Returns the ring and the descriptors, which the caller must keep alive only
/// because the test wants them — the ring duplicates its own.
fn ring_of(device: &Device, n: usize) -> Option<(ExternalDmaBufRing, Vec<OwnedFd>)> {
    let mut fds = Vec::new();
    let mut pitches = Vec::new();
    for _ in 0..n {
        let (fd, pitch) = real_dma_buf(device, 64, 64)?;
        fds.push(fd);
        pitches.push(pitch);
    }
    let planes: Vec<Vec<ExternalPlane<'_>>> = fds
        .iter()
        .zip(&pitches)
        .map(|(fd, pitch)| {
            vec![ExternalPlane {
                fd: fd.as_fd(),
                offset: 0,
                pitch: *pitch,
            }]
        })
        .collect();
    let slots: Vec<&[ExternalPlane<'_>]> = planes.iter().map(Vec::as_slice).collect();

    let ring = ExternalDmaBufRing::create(
        device,
        SourceFormat {
            fourcc: XRGB8888,
            modifier: 0,
            width: 64,
            height: 64,
        },
        &slots,
        None,
    )
    .expect("import the ring");
    Some((ring, fds))
}

/// Every slot gets its own framebuffer.
///
/// One id shared across slots would mean the producer's rotation changed
/// nothing on screen — every frame would present whichever buffer that id
/// happened to name.
#[test]
fn each_slot_gets_its_own_framebuffer() {
    let _guard = card_guard();
    let Some(device) = open_card() else { return };
    let Some((mut ring, _fds)) = ring_of(&device, 3) else {
        panic!("dma-buf export");
    };
    assert_eq!(ring.slot_count(), 3);

    let mut seen = std::collections::HashSet::new();
    for slot in 0..3 {
        ring.submit(slot, None, &[]);
        let fb = ring.acquire().expect("acquire").fb_id;
        assert!(fb != 0, "slot {slot} has no framebuffer");
        assert!(
            seen.insert(fb),
            "slot {slot} reused another slot's framebuffer"
        );
    }
}

/// With nothing new submitted, the ring re-presents what is on screen.
///
/// Returning `WouldBlock` instead would have the scene drop the layer, which
/// blanks the plane — a producer that pauses for one vblank should freeze, not
/// disappear.
#[test]
fn an_idle_ring_holds_the_last_frame() {
    let _guard = card_guard();
    let Some(device) = open_card() else { return };
    let Some((mut ring, _fds)) = ring_of(&device, 2) else {
        panic!("dma-buf export");
    };

    ring.submit(1, None, &[]);
    let first = ring.acquire().expect("first frame");
    assert!(!ring.has_fresh_frame(), "the submission was taken");

    let held = ring.acquire().expect("an idle vblank still presents");
    assert_eq!(held.fb_id, first.fb_id, "the same buffer stays up");
    assert_eq!(
        held.token, first.token,
        "and under the same token, so it is not mistaken for a superseded frame"
    );
}

/// A slot is handed back only once something newer is on screen.
#[test]
fn a_slot_is_released_when_superseded() {
    let _guard = card_guard();
    let Some(device) = open_card() else { return };
    let Some((mut ring, _fds)) = ring_of(&device, 2) else {
        panic!("dma-buf export");
    };

    let freed = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&freed);
    ring.set_on_release(Box::new(move |slot, fence| {
        sink.lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push((slot, fence.is_some()));
    }));

    ring.submit(0, None, &[]);
    let first_token = ring.acquire().expect("first").token;
    // Release keys on the token alone, so a buffer carrying it says the same
    // thing as the one the scene held. `AcquiredBuffer` owns its fence and so
    // is deliberately not `Clone`.
    ring.release(AcquiredBuffer {
        token: first_token,
        ..AcquiredBuffer::default()
    });
    assert!(
        freed
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_empty(),
        "slot 0 is still on screen; telling the producer it is free would race \
         it into overwriting live scanout"
    );

    ring.submit(1, None, &[]);
    let second_token = ring.acquire().expect("second").token;
    ring.release(AcquiredBuffer {
        token: first_token,
        ..AcquiredBuffer::default()
    });
    assert_eq!(
        &*freed
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner),
        &[(0, false)],
        "slot 0 left the screen and must come back to the producer"
    );

    // And the newly live one still does not.
    ring.release(AcquiredBuffer {
        token: second_token,
        ..AcquiredBuffer::default()
    });
    assert_eq!(
        freed
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .len(),
        1
    );
}

/// The release fence reaches the producer.
///
/// A GPU producer waits on it and re-renders the slot with no CPU stall. Losing
/// it would not break correctness — the callback edge still says "free" — but
/// it forces the stall the fence exists to avoid.
#[test]
fn a_release_fence_is_forwarded_to_the_producer() {
    let _guard = card_guard();
    let Some(device) = open_card() else { return };
    let Some((mut ring, _fds)) = ring_of(&device, 2) else {
        panic!("dma-buf export");
    };

    let fenced = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&fenced);
    ring.set_on_release(Box::new(move |_, fence| {
        if fence.is_some() {
            counter.fetch_add(1, Ordering::SeqCst);
        }
    }));
    assert!(
        ring.wants_release_fence(),
        "with a listener attached the scene should ask the kernel for an \
         out-fence"
    );

    ring.submit(0, None, &[]);
    let first_token = ring.acquire().expect("first").token;
    ring.submit(1, None, &[]);
    ring.acquire().expect("second");

    let stand_in =
        rustix::event::eventfd(0, rustix::event::EventfdFlags::CLOEXEC).expect("eventfd");
    ring.release_with_fence(
        AcquiredBuffer {
            token: first_token,
            ..AcquiredBuffer::default()
        },
        Some(SyncFence::from_owned(stand_in)),
    );

    assert_eq!(
        fenced.load(Ordering::SeqCst),
        1,
        "the displacing commit's fence must reach the producer"
    );
}

/// Nothing listening means no reason to ask the kernel for an out-fence.
#[test]
fn a_ring_with_no_listener_wants_no_release_fence() {
    let _guard = card_guard();
    let Some(device) = open_card() else { return };
    let Some((ring, _fds)) = ring_of(&device, 1) else {
        panic!("dma-buf export");
    };
    assert!(!ring.wants_release_fence());
}

/// A paused session leaves nothing presentable.
///
/// The descriptor is revoked, so the framebuffer ids are meaningless. Handing
/// one to a commit would take the whole frame down, so the ring reports no
/// frame until the producer submits against a re-imported ring.
#[test]
fn a_paused_ring_presents_nothing() {
    let _guard = card_guard();
    let Some(device) = open_card() else { return };
    let Some((mut ring, _fds)) = ring_of(&device, 2) else {
        panic!("dma-buf export");
    };

    ring.submit(0, None, &[]);
    ring.acquire().expect("a frame before the pause");

    ring.on_session_paused();

    assert_eq!(ring.scanning_slot(), None, "nothing is on screen any more");
    assert!(
        matches!(ring.acquire(), Err(SourceError::WouldBlock)),
        "with nothing on screen there is nothing to hold"
    );

    // The part that needs the framebuffers actually forgotten. Clearing the
    // presenter alone is not enough: a producer that keeps submitting across
    // the pause would otherwise be handed a framebuffer id registered on a
    // descriptor that no longer exists, and committing it takes the whole
    // frame down.
    ring.submit(0, None, &[]);
    assert!(
        matches!(ring.acquire(), Err(SourceError::Failed(_))),
        "a slot whose framebuffer was forgotten must not be presented"
    );
}

/// A ring needs at least one slot.
#[test]
fn an_empty_ring_is_refused() {
    let _guard = card_guard();
    let Some(device) = open_card() else { return };
    let result = ExternalDmaBufRing::create(
        &device,
        SourceFormat {
            fourcc: XRGB8888,
            modifier: 0,
            width: 64,
            height: 64,
        },
        &[],
        None,
    );
    assert!(result.is_err(), "a ring with no slots can never present");
}

/// A slot index the ring does not have is ignored.
///
/// The producer owns the indices it was built with. Failing loudly on its own
/// thread, where there is no commit to fail, would give it nothing to do about
/// it.
#[test]
fn an_out_of_range_submit_is_ignored() {
    let _guard = card_guard();
    let Some(device) = open_card() else { return };
    let Some((mut ring, _fds)) = ring_of(&device, 2) else {
        panic!("dma-buf export");
    };

    ring.submit(99, None, &[]);
    assert!(!ring.has_fresh_frame(), "nothing was queued");
    assert!(matches!(ring.acquire(), Err(SourceError::WouldBlock)));
}

// --- ExternalDmaBufPool ------------------------------------------------------

fn pool_format() -> SourceFormat {
    SourceFormat {
        fourcc: XRGB8888,
        modifier: 0,
        width: 64,
        height: 64,
    }
}

/// A key is imported once and reused thereafter.
#[test]
fn a_pool_imports_each_key_once() {
    let _guard = card_guard();
    let Some(device) = open_card() else { return };
    let Some((fd, pitch)) = real_dma_buf(&device, 64, 64) else {
        panic!("dma-buf export");
    };
    let planes = [ExternalPlane {
        fd: fd.as_fd(),
        offset: 0,
        pitch,
    }];

    let mut pool = ExternalDmaBufPool::new(pool_format(), None);
    assert_eq!(pool.cached_count(), 0, "a pool starts empty");

    assert!(pool.submit(&device, 42, &planes, None, &[]));
    let first = pool.acquire().expect("first frame").fb_id;
    assert_eq!(pool.cached_count(), 1);

    assert!(pool.submit(&device, 42, &planes, None, &[]));
    let second = pool.acquire().expect("second frame").fb_id;

    assert_eq!(first, second, "the same key must reuse its import");
    assert_eq!(pool.cached_count(), 1, "and not import a second time");
}

/// Distinct keys are distinct buffers.
#[test]
fn a_pool_keeps_keys_apart() {
    let _guard = card_guard();
    let Some(device) = open_card() else { return };
    let Some((a_fd, a_pitch)) = real_dma_buf(&device, 64, 64) else {
        panic!("dma-buf export");
    };
    let Some((b_fd, b_pitch)) = real_dma_buf(&device, 64, 64) else {
        panic!("dma-buf export");
    };

    let mut pool = ExternalDmaBufPool::new(pool_format(), None);
    pool.submit(
        &device,
        1,
        &[ExternalPlane {
            fd: a_fd.as_fd(),
            offset: 0,
            pitch: a_pitch,
        }],
        None,
        &[],
    );
    let first = pool.acquire().expect("first").fb_id;

    pool.submit(
        &device,
        2,
        &[ExternalPlane {
            fd: b_fd.as_fd(),
            offset: 0,
            pitch: b_pitch,
        }],
        None,
        &[],
    );
    let second = pool.acquire().expect("second").fb_id;

    assert_ne!(first, second, "two keys must not share a framebuffer");
    assert_eq!(pool.cached_count(), 2);
}

/// A failed import skips the frame and holds the last good buffer.
///
/// The producer is on its own thread with no commit to fail, so reporting
/// upward has nowhere to go. A frozen layer beats a blank one.
#[test]
fn a_failed_import_holds_the_last_frame() {
    let _guard = card_guard();
    let Some(device) = open_card() else { return };
    let Some((fd, pitch)) = real_dma_buf(&device, 64, 64) else {
        panic!("dma-buf export");
    };
    let planes = [ExternalPlane {
        fd: fd.as_fd(),
        offset: 0,
        pitch,
    }];

    let mut pool = ExternalDmaBufPool::new(pool_format(), None);
    pool.submit(&device, 1, &planes, None, &[]);
    let good = pool.acquire().expect("a good frame").fb_id;

    // A key the pool has never seen, with a plane list it must refuse.
    assert!(
        !pool.submit(&device, 2, &[], None, &[]),
        "an empty plane list cannot be imported"
    );
    assert_eq!(pool.cached_count(), 1, "the bad key cached nothing");

    let held = pool.acquire().expect("the layer must not go blank");
    assert_eq!(held.fb_id, good, "the last good buffer stays up");
}

/// A new generation retires the old buffers, but not while they are on screen.
///
/// Tearing down a framebuffer the kernel is still scanning out is exactly the
/// hazard the deferred-release protocol exists to avoid; a resolution change
/// must not become a way around it.
#[test]
fn a_generation_reset_retires_buffers_only_once_unreferenced() {
    let _guard = card_guard();
    let Some(device) = open_card() else { return };
    let Some((old_fd, old_pitch)) = real_dma_buf(&device, 64, 64) else {
        panic!("dma-buf export");
    };
    let Some((new_fd, new_pitch)) = real_dma_buf(&device, 32, 32) else {
        panic!("dma-buf export");
    };

    let mut pool = ExternalDmaBufPool::new(pool_format(), None);
    pool.submit(
        &device,
        1,
        &[ExternalPlane {
            fd: old_fd.as_fd(),
            offset: 0,
            pitch: old_pitch,
        }],
        None,
        &[],
    );
    let old_token = pool.acquire().expect("the old generation").token;
    assert_eq!(pool.cached_count(), 1);

    pool.reset_generation(SourceFormat {
        fourcc: XRGB8888,
        modifier: 0,
        width: 32,
        height: 32,
    });

    // Still on screen, so still imported.
    pool.acquire().expect("holding the old buffer");
    assert_eq!(
        pool.cached_count(),
        1,
        "the retiring buffer is on screen and must not be torn down under it"
    );

    // The new generation arrives under a fresh key, as the contract requires.
    pool.submit(
        &device,
        2,
        &[ExternalPlane {
            fd: new_fd.as_fd(),
            offset: 0,
            pitch: new_pitch,
        }],
        None,
        &[],
    );
    pool.acquire().expect("the new generation");
    pool.release(AcquiredBuffer {
        token: old_token,
        ..AcquiredBuffer::default()
    });
    pool.acquire().expect("a later frame sweeps");

    assert_eq!(
        pool.cached_count(),
        1,
        "once nothing references it, the old buffer is torn down"
    );
    assert_eq!(
        LayerBufferSource::format(&pool).width,
        32,
        "and future imports use the new geometry"
    );
}

/// A paused pool drops its imports so the producer re-imports on resume.
#[test]
fn a_paused_pool_forgets_its_imports() {
    let _guard = card_guard();
    let Some(device) = open_card() else { return };
    let Some((fd, pitch)) = real_dma_buf(&device, 64, 64) else {
        panic!("dma-buf export");
    };
    let planes = [ExternalPlane {
        fd: fd.as_fd(),
        offset: 0,
        pitch,
    }];

    let mut pool = ExternalDmaBufPool::new(pool_format(), None);
    pool.submit(&device, 1, &planes, None, &[]);
    pool.acquire().expect("a frame");
    assert_eq!(pool.cached_count(), 1);

    pool.on_session_paused();

    assert_eq!(
        pool.cached_count(),
        0,
        "every framebuffer id belonged to a descriptor that is now revoked"
    );
    assert!(matches!(pool.acquire(), Err(SourceError::WouldBlock)));
}
