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
use drmkit_scene::{BindingModel, DamageRect, LayerBufferSource, SourceError};

/// `Device::open` acquires DRM master, which is per open file description, so
/// card-dependent cases serialize rather than racing for it.
static CARD_LOCK: Mutex<()> = Mutex::new(());

fn card_guard() -> std::sync::MutexGuard<'static, ()> {
    CARD_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn open_card() -> Device {
    let path = std::env::var("DRMKIT_TEST_CARD").unwrap_or_else(|_| "/dev/dri/card0".to_owned());
    Device::open(path).expect("open test card")
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
    let device = open_card();
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
    let device = open_card();
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
    let device = open_card();
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
    let device = open_card();
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
    let device = open_card();
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
    let device = open_card();
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
    let device = open_card();
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
    let device = open_card();
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
    let device = open_card();
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
    let device = open_card();
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
    let device = open_card();
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
    let device = open_card();
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
    let device = open_card();
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
    let device = open_card();
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
