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

/// More planes than any format has.
///
/// The array the handles go into is fixed at four, so a fifth would write past
/// it if the count were not checked -- and four is the kernel's own limit, so
/// nothing legitimate is being turned away.
#[test]
#[ignore = "needs a DRM device; run under the vkms lane with --include-ignored"]
fn external_create_rejects_more_planes_than_a_format_can_have_vkms() {
    let _guard = card_guard();
    let Some(device) = open_card() else { return };
    let fd = placeholder_fd();
    let five: Vec<ExternalPlane<'_>> = (0..5).map(|_| plane(&fd)).collect();

    assert!(matches!(
        ExternalDmaBufSource::create(&device, external_format(), &five, None),
        Err(ExternalError::Invalid { .. })
    ));

    // Four is accepted by the shape check -- it may still fail later on the
    // import, which is a different answer and the point of the distinction.
    let four: Vec<ExternalPlane<'_>> = (0..4).map(|_| plane(&fd)).collect();
    assert!(
        !matches!(
            ExternalDmaBufSource::create(&device, external_format(), &four, None),
            Err(ExternalError::Invalid { .. })
        ),
        "four planes is a shape the kernel allows"
    );
}

/// Multi-plane layouts pass validation.
///
/// The reference makes this point by handing `create` a device built from fd
/// -1 and asserting the error is `bad_file_descriptor` rather than
/// `invalid_argument` -- the layout was accepted, and the failure came from
/// the device. Here a `&Device` is a live DRM device by construction, so the
/// same distinction is drawn from the other side: whatever these do, they must
/// not come back `Invalid`, because that would mean the shape was rejected.
#[test]
#[ignore = "needs a DRM device; run under the vkms lane with --include-ignored"]
fn external_create_accepts_multi_plane_layouts_vkms() {
    const W: u32 = 64;
    const H: u32 = 64;

    let _guard = card_guard();
    let Some(device) = open_card() else { return };
    let fd = placeholder_fd();

    let at = |offset: u32, pitch: u32| ExternalPlane {
        fd: fd.as_fd(),
        offset,
        pitch,
    };

    // NV12: an 8bpp Y plane and an interleaved 8bpp chroma plane after it.
    let nv12 = [at(0, W), at(W * H, W)];
    // YUV420: Y, then two half-width chroma planes.
    let yuv420 = [
        at(0, W),
        at(W * H, W / 2),
        at(W * H + (W / 2) * (H / 2), W / 2),
    ];
    // A tiled single-plane layout -- the modifier must not make the shape
    // check reject it.
    let tiled = [at(0, W * 4)];

    let cases: [(&str, u32, u64, &[ExternalPlane<'_>]); 3] = [
        ("NV12", drmkit_fmt::fourcc::NV12, 0, &nv12),
        ("YUV420", drmkit_fmt::fourcc::YUV420, 0, &yuv420),
        (
            "tiled XRGB8888",
            XRGB8888,
            // An arbitrary vendor modifier: what matters is that it is not
            // LINEAR and does not change the shape check's answer.
            0x0100_0000_0000_0002,
            &tiled,
        ),
    ];

    for (what, fourcc, modifier, planes) in cases {
        let format = SourceFormat {
            fourcc,
            modifier,
            width: W,
            height: H,
        };
        assert!(
            !matches!(
                ExternalDmaBufSource::create(&device, format, planes, None),
                Err(ExternalError::Invalid { .. })
            ),
            "{what} is a valid layout and must not be rejected as malformed"
        );
    }
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
/// An owned fence handed over on one acquire would be gone by the next, so a
/// later commit would go out unsynced and the display engine could sample the
/// buffer before the producer's writes land. Nothing reports it: the commit
/// succeeds, the counters balance, and the frame tears under load.
///
/// This used to say the scene acquires each source twice per frame, once for
/// the `TEST_ONLY` commit and once for the real one. Measured through the T7
/// runner, it does not: six frames over two layers produce twelve acquires,
/// exactly one per source per frame, including the frame that searches. The
/// test commit is built by `DeviceCommitter` from the allocator's `Layer`
/// property bags, which never carry a fence, so `IN_FENCE_FD` reaches the
/// apply only — and drm-cxx agrees byte for byte, so it is settled behaviour
/// rather than a divergence.
///
/// The requirement is unchanged; only the reason is. One fence has to arm
/// frame after frame, which is what `fence-dup-independence.scenario` pins at
/// the trace level.
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

// --- GbmBufferSource ---------------------------------------------------------

/// A GBM allocation becomes a framebuffer the scene can submit.
///
/// The path this covers is the one that carries the modifier: the buffer goes
/// to KMS as a DMA-BUF import, so a tiled or compressed allocation arrives at
/// `add_planar_framebuffer` described as what it is. On vkms everything is
/// linear, so what is pinned here is that the export-import round trip works
/// at all and reports the shape it was asked for.
#[cfg(feature = "gbm")]
#[test]
#[ignore = "needs a DRM device; run under the vkms lane with --include-ignored"]
fn a_gbm_allocation_becomes_a_submittable_framebuffer_vkms() {
    let _guard = card_guard();
    let Some(device) = open_card() else { return };

    let mut source = match crate::GbmBufferSource::create(&device, 64, 64, XRGB8888, &[0]) {
        Ok(source) => source,
        Err(error) => {
            println!("note: skipped -- no GBM allocation on this card ({error})");
            return;
        }
    };

    let format = LayerBufferSource::format(&source);
    assert_eq!(format.width, 64);
    assert_eq!(format.height, 64);
    assert_eq!(format.fourcc, XRGB8888);

    let fb_id = source.fb_id().expect("the import registered a framebuffer");
    assert_ne!(fb_id, 0, "a zero fb_id is what the scene skips writing");

    let acquired = source.acquire().expect("acquire");
    assert_eq!(
        acquired.fb_id, fb_id,
        "a single-buffer source hands out the framebuffer it registered"
    );
    source.release(acquired);
}

/// The source reports the modifier the allocation actually has.
///
/// Not the one that was requested: a driver with no constrained entry point
/// falls back to an unconstrained allocation, and a framebuffer registered
/// with a modifier the buffer does not have is one the kernel either refuses
/// or -- worse -- accepts and scans out as garbage.
#[cfg(feature = "gbm")]
#[test]
#[ignore = "needs a DRM device; run under the vkms lane with --include-ignored"]
fn the_reported_modifier_is_the_one_the_buffer_has_vkms() {
    let _guard = card_guard();
    let Some(device) = open_card() else { return };

    let source = match crate::GbmBufferSource::create(&device, 64, 64, XRGB8888, &[0]) {
        Ok(source) => source,
        Err(error) => {
            println!("note: skipped -- no GBM allocation on this card ({error})");
            return;
        }
    };

    assert_eq!(
        LayerBufferSource::format(&source).modifier,
        source.buffer().modifier(),
        "the framebuffer describes the layout the allocation reports"
    );
}

/// Composition reaches this source through its DMA-BUF, not a CPU map.
///
/// A GBM buffer may be tiled or device-local, so `map` is correctly
/// unsupported -- and a source that only said that would be uncompositable,
/// blanking its layer whenever the allocator could not place it. The export is
/// what keeps it rescuable.
#[cfg(feature = "gbm")]
#[test]
#[ignore = "needs a DRM device; run under the vkms lane with --include-ignored"]
fn a_gbm_source_is_compositable_through_its_dma_buf_vkms() {
    let _guard = card_guard();
    let Some(device) = open_card() else { return };

    let mut source = match crate::GbmBufferSource::create(&device, 64, 64, XRGB8888, &[0]) {
        Ok(source) => source,
        Err(error) => {
            println!("note: skipped -- no GBM allocation on this card ({error})");
            return;
        }
    };

    assert!(
        matches!(
            source.map(drmkit_dumb::MapAccess::Read),
            Err(SourceError::Unsupported)
        ),
        "a GBM allocation has no meaningful CPU view to hand back"
    );
    assert!(
        source.export_dma_buf().is_ok(),
        "so the DMA-BUF export is the only thing keeping the layer compositable"
    );
}

// --- composability of imported sources ---------------------------------------

/// An imported buffer is compositable through its descriptors.
///
/// The trait's own documentation names this case as the reason
/// `export_dma_buf` exists -- "a source whose pixels never reach CPU memory
/// ... can still be composited by importing its DMA-BUF". This source is that
/// case. Without the export it is uncompositable, and its layer blanks
/// whenever the allocator cannot find it a plane.
#[test]
#[ignore = "needs a DRM device; run under the vkms lane with --include-ignored"]
fn an_imported_buffer_lends_its_descriptors_back_vkms() {
    let _guard = card_guard();
    let Some(device) = open_card() else { return };
    let (dma_buf, pitch) = real_dma_buf(&device, 64, 64).expect("export a real dma-buf");

    let format = SourceFormat {
        fourcc: XRGB8888,
        modifier: 0,
        width: 64,
        height: 64,
    };
    let mut source = ExternalDmaBufSource::create(
        &device,
        format,
        &[ExternalPlane {
            fd: std::os::fd::AsFd::as_fd(&dma_buf),
            offset: 0,
            pitch,
        }],
        None,
    )
    .expect("import");

    assert!(
        matches!(
            source.map(drmkit_dumb::MapAccess::Read),
            Err(SourceError::Unsupported)
        ),
        "a foreign buffer exposes no CPU mapping"
    );

    let exported = source
        .export_dma_buf()
        .expect("so it must lend descriptors");
    assert_eq!(exported.fds.len(), 1);
    assert_eq!(exported.pitches, vec![pitch]);
    assert_eq!(exported.offsets, vec![0]);
    assert_eq!(
        exported.format, format,
        "the descriptor describes the same buffer the framebuffer does"
    );
}

/// A ring describes the slot the last acquire handed out, not slot zero.
///
/// A ring rotates, so "the currently-acquired buffer" is a different slot each
/// frame. Answering with a fixed slot would describe the wrong pixels -- and
/// on a two-slot ring it would be right half the time, which is worse than
/// being wrong every time.
#[test]
#[ignore = "needs a DRM device; run under the vkms lane with --include-ignored"]
fn a_ring_describes_the_slot_it_just_handed_out_vkms() {
    let _guard = card_guard();
    let Some(device) = open_card() else { return };
    let (first_fd, first_pitch) = real_dma_buf(&device, 64, 64).expect("export a real dma-buf");
    let (second_fd, second_pitch) = real_dma_buf(&device, 64, 64).expect("export a real dma-buf");

    let format = SourceFormat {
        fourcc: XRGB8888,
        modifier: 0,
        width: 64,
        height: 64,
    };
    let plane_a = [ExternalPlane {
        fd: std::os::fd::AsFd::as_fd(&first_fd),
        offset: 0,
        pitch: first_pitch,
    }];
    let plane_b = [ExternalPlane {
        fd: std::os::fd::AsFd::as_fd(&second_fd),
        offset: 0,
        pitch: second_pitch,
    }];
    let mut ring = ExternalDmaBufRing::create(&device, format, &[&plane_a, &plane_b], None)
        .expect("build a two-slot ring");

    assert!(
        matches!(ring.export_dma_buf(), Err(SourceError::Unsupported)),
        "before the first acquire there is no acquired buffer to describe"
    );

    // The slots are distinguished by descriptor, not by shape: a ring holds one
    // format across every slot, so the strides are equal by construction and
    // only the import's own duplicated descriptor tells them apart.
    let mut seen = Vec::new();
    for slot in 0..2 {
        ring.submit(slot, None, &[]);
        let acquired = ring.acquire().expect("acquire");
        let exported = ring.export_dma_buf().expect("describe the acquired slot");
        assert_eq!(
            exported.pitches,
            vec![first_pitch],
            "both slots share a shape"
        );
        seen.push(std::os::fd::AsRawFd::as_raw_fd(&exported.fds[0]));
        ring.release(acquired);
    }

    assert_ne!(
        seen[0], seen[1],
        "two acquires described the same slot, so the ring answers with a fixed one"
    );
    assert_eq!(second_pitch, first_pitch, "the fixture allocated two alike");
}

// --- ExternalDmaBufRing, against a card ---------------------------------------
//
// Parity port of `tests/integration/test_external_dma_buf_ring_vkms.cpp`.

/// A ring with a fence deadline, plus the descriptors keeping its slots alive.
#[cfg(test)]
fn deadline_ring_of(
    device: &Device,
    slots: usize,
    deadline: Option<std::time::Duration>,
) -> Option<(ExternalDmaBufRing, Vec<OwnedFd>)> {
    let mut fds = Vec::new();
    let mut pitches = Vec::new();
    for _ in 0..slots {
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
        deadline,
    )
    .expect("import the ring");
    Some((ring, fds))
}

/// A pipe standing in for a fence, signalled or not.
///
/// A `sync_file` is waited on with `poll`, and so is a pipe: the read end is
/// unreadable until something is written, which is exactly an unsignalled
/// fence, and writing a byte signals it. Upstream uses the same trick, because
/// producing a genuinely unsignalled GPU fence needs a GPU.
///
/// The writer comes back so the caller can signal a fence it made unsignalled
/// -- and because dropping it would close the pipe, which `poll` reports as
/// readable and the wait would read as "signalled".
#[cfg(test)]
fn pipe_fence(signalled: bool) -> Option<(SyncFence, std::io::PipeWriter)> {
    let (read, write) = std::io::pipe().ok()?;
    if signalled {
        std::io::Write::write_all(&mut { &write }, b"x").ok()?;
    }
    let fence = SyncFence::import(std::os::fd::AsFd::as_fd(&read)).ok()?;
    Some((fence, write))
}

/// Slots rotate, a release fires once for the slot that left, and an idle
/// acquire re-presents the last frame without releasing it.
#[test]
#[ignore = "needs a DRM device; run under the vkms lane with --include-ignored"]
fn a_ring_rotates_its_slots_and_holds_the_last_when_idle_vkms() {
    let _guard = card_guard();
    let Some(device) = open_card() else { return };
    let Some((mut ring, _fds)) = ring_of(&device, 2) else {
        println!("note: skipped -- no importable dma-buf on this card");
        return;
    };

    // Arc, not Rc: the release callback is `Send` by contract, because a
    // producer may hand buffers back from its own thread.
    let released: std::sync::Arc<std::sync::Mutex<Vec<usize>>> = std::sync::Arc::default();
    let recorder = std::sync::Arc::clone(&released);
    ring.set_on_release(Box::new(move |slot, fence| {
        assert!(
            fence.is_none(),
            "the callback edge carries no fence until the scene wires OUT_FENCE"
        );
        recorder.lock().expect("record the release").push(slot);
    }));

    assert!(
        matches!(ring.acquire(), Err(SourceError::WouldBlock)),
        "a ring nothing has been submitted to has no frame, which is flow \
         control rather than failure"
    );

    ring.submit(0, None, &[]);
    let first = ring.acquire().expect("slot 0");
    let fb0 = first.fb_id;
    assert_ne!(fb0, 0);

    ring.submit(1, None, &[]);
    let second = ring.acquire().expect("slot 1");
    let fb1 = second.fb_id;
    assert_ne!(fb1, 0);
    assert_ne!(
        fb1, fb0,
        "two slots sharing a framebuffer id would make the rotation change \
         nothing on screen"
    );

    ring.release(first);
    assert_eq!(
        *released.lock().expect("read the releases"),
        vec![0],
        "releasing the displaced frame returns exactly its slot to the producer"
    );

    // Nothing new submitted: the ring re-presents what is up rather than
    // reporting no frame, and holding does not release it.
    let held = ring.acquire().expect("hold the last frame");
    assert_eq!(held.fb_id, fb1);
    assert!(held.acquire_fence.is_none());
    ring.release(held);
    assert_eq!(
        released.lock().expect("read the releases").len(),
        1,
        "releasing a held frame must not hand its slot back -- it is still on screen"
    );

    ring.submit(0, None, &[]);
    let third = ring.acquire().expect("slot 0 again");
    assert_eq!(
        third.fb_id, fb0,
        "the ring came back round to the first slot"
    );

    ring.release(second);
    assert_eq!(*released.lock().expect("read the releases"), vec![0, 1]);
    ring.release(third);
}

/// Damage rides with the frame it was submitted for, and only that frame.
///
/// A held frame reports none: nothing changed since it went up, so repeating
/// the previous frame's damage would make the driver repaint a region that is
/// already correct.
#[test]
#[ignore = "needs a DRM device; run under the vkms lane with --include-ignored"]
fn damage_rides_with_its_own_frame_and_no_other_vkms() {
    let _guard = card_guard();
    let Some(device) = open_card() else { return };
    let Some((mut ring, _fds)) = ring_of(&device, 2) else {
        println!("note: skipped -- no importable dma-buf on this card");
        return;
    };

    let two = [
        DamageRect {
            x: 1,
            y: 2,
            w: 3,
            h: 4,
        },
        DamageRect {
            x: 10,
            y: 20,
            w: 30,
            h: 40,
        },
    ];
    ring.submit(0, None, &two);
    let first = ring.acquire().expect("slot 0");
    assert_eq!(first.damage, two, "both rects, in the order submitted");

    let held = ring.acquire().expect("hold");
    assert!(
        held.damage.is_empty(),
        "a held frame changed nothing, so it damages nothing"
    );
    ring.release(held);

    let one = [DamageRect {
        x: 5,
        y: 6,
        w: 7,
        h: 8,
    }];
    ring.submit(1, None, &one);
    let second = ring.acquire().expect("slot 1");
    assert_eq!(
        second.damage, one,
        "this frame's damage, not the last one's"
    );

    ring.release(first);
    ring.release(second);

    ring.submit(0, None, &[]);
    let third = ring.acquire().expect("slot 0");
    assert!(
        third.damage.is_empty(),
        "submitting no damage means no damage, not the previous set"
    );
    ring.release(third);
}

/// With a deadline configured, a frame submitted without a fence still
/// advances, and no fence is ever handed to the kernel.
///
/// The deadline turns the producer's fence into a CPU pre-wait. A ring that
/// also passed it on as `IN_FENCE_FD` would make the kernel wait a second time
/// on a fence already known to have signalled.
#[test]
#[ignore = "needs a DRM device; run under the vkms lane with --include-ignored"]
fn a_deadline_ring_advances_on_a_frame_with_no_fence_vkms() {
    let _guard = card_guard();
    let Some(device) = open_card() else { return };
    let Some((mut ring, _fds)) =
        deadline_ring_of(&device, 2, Some(std::time::Duration::from_millis(5)))
    else {
        println!("note: skipped -- no importable dma-buf on this card");
        return;
    };

    ring.submit(0, None, &[]);
    let first = ring.acquire().expect("slot 0");
    let fb0 = first.fb_id;
    assert_ne!(fb0, 0);
    assert!(
        first.acquire_fence.is_none(),
        "under a deadline the fence is pre-waited here, never wired to KMS"
    );

    ring.submit(1, None, &[]);
    let second = ring.acquire().expect("slot 1");
    assert_ne!(second.fb_id, fb0, "the ring advanced");
    assert!(second.acquire_fence.is_none());

    ring.release(first);
    ring.release(second);
}

/// A fence that misses its deadline holds the last good frame, and the ring
/// recovers when a signalled one arrives.
///
/// Holding is the point: advancing to a slot whose producer has not finished
/// writing puts a half-rendered frame on screen. Missing the deadline is a
/// late frame, which is a dropped frame -- not a torn one.
#[test]
#[ignore = "needs a DRM device; run under the vkms lane with --include-ignored"]
fn a_missed_fence_deadline_holds_the_last_frame_then_recovers_vkms() {
    let _guard = card_guard();
    let Some(device) = open_card() else { return };
    let Some((mut ring, _fds)) =
        deadline_ring_of(&device, 2, Some(std::time::Duration::from_millis(30)))
    else {
        println!("note: skipped -- no importable dma-buf on this card");
        return;
    };

    ring.submit(0, None, &[]);
    let first = ring.acquire().expect("slot 0");
    let fb0 = first.fb_id;
    assert_ne!(fb0, 0);

    let Some((unsignalled, _writer)) = pipe_fence(false) else {
        println!("note: skipped -- no pipe to stand in for a fence");
        return;
    };
    ring.submit(1, Some(unsignalled), &[]);

    let held = ring.acquire().expect("a deadline miss is not an error");
    assert_eq!(
        held.fb_id, fb0,
        "a missed deadline holds the last good slot rather than advancing to \
         one the producer has not finished"
    );
    assert!(
        held.acquire_fence.is_none(),
        "the fence was pre-waited, so it is never handed to the kernel"
    );
    ring.release(held);

    // Resubmit the same slot behind a fence that has already signalled.
    let (signalled, _writer) = pipe_fence(true).expect("a signalled fence");
    ring.submit(1, Some(signalled), &[]);

    let recovered = ring.acquire().expect("recover");
    assert_ne!(
        recovered.fb_id, fb0,
        "with the fence signalled the ring advances to the frame it held back"
    );
    ring.release(first);
    ring.release(recovered);
}

/// A dropped frame's damage is unioned into the next one, and only the next.
///
/// The dropped frame's pixels were never scanned out, so the region it
/// declared is still stale on screen. If the next frame reported only its own
/// damage, the driver would repaint that region and leave the dropped one
/// showing the frame before it. Carrying it forever would be the other error:
/// it would over-report every frame from here on.
#[test]
#[ignore = "needs a DRM device; run under the vkms lane with --include-ignored"]
fn a_dropped_frames_damage_is_carried_into_the_next_one_only_vkms() {
    let _guard = card_guard();
    let Some(device) = open_card() else { return };
    let Some((mut ring, _fds)) =
        deadline_ring_of(&device, 2, Some(std::time::Duration::from_millis(30)))
    else {
        println!("note: skipped -- no importable dma-buf on this card");
        return;
    };

    ring.submit(0, None, &[]);
    let first = ring.acquire().expect("slot 0");

    let dropped = [
        DamageRect {
            x: 1,
            y: 1,
            w: 2,
            h: 2,
        },
        DamageRect {
            x: 3,
            y: 3,
            w: 4,
            h: 4,
        },
    ];
    let Some((unsignalled, _writer)) = pipe_fence(false) else {
        println!("note: skipped -- no pipe to stand in for a fence");
        return;
    };
    ring.submit(1, Some(unsignalled), &dropped);

    let held = ring.acquire().expect("the deadline miss holds");
    assert!(
        held.damage.is_empty(),
        "a held frame changed nothing since it went up"
    );
    ring.release(held);

    let arriving = [DamageRect {
        x: 5,
        y: 5,
        w: 6,
        h: 6,
    }];
    let (signalled, _writer) = pipe_fence(true).expect("a signalled fence");
    ring.submit(1, Some(signalled), &arriving);

    let advanced = ring.acquire().expect("advance");
    assert_eq!(
        advanced.damage,
        [dropped[0], dropped[1], arriving[0]],
        "the dropped frame's regions are still stale on screen, so they have \
         to be repainted alongside this frame's"
    );

    // One frame only: the carry was consumed above.
    ring.submit(0, None, &arriving);
    let plain = ring.acquire().expect("the frame after");
    assert_eq!(
        plain.damage, arriving,
        "carrying past one frame would over-report every frame from here on"
    );

    ring.release(first);
    ring.release(advanced);
    ring.release(plain);
}

/// Dropping a whole-frame update forces the next frame to whole-frame.
///
/// Empty damage means *everything changed*. There is no rect list that says
/// that, so a dropped whole-frame update cannot be unioned into the next
/// frame's rects -- carrying nothing would silently downgrade it to "only what
/// the next frame touched", leaving the rest of the screen stale.
#[test]
#[ignore = "needs a DRM device; run under the vkms lane with --include-ignored"]
fn a_dropped_whole_frame_forces_the_next_to_whole_frame_vkms() {
    let _guard = card_guard();
    let Some(device) = open_card() else { return };
    let Some((mut ring, _fds)) =
        deadline_ring_of(&device, 2, Some(std::time::Duration::from_millis(30)))
    else {
        println!("note: skipped -- no importable dma-buf on this card");
        return;
    };

    ring.submit(0, None, &[]);
    let first = ring.acquire().expect("slot 0");

    // Whole-frame, and dropped.
    let Some((unsignalled, _writer)) = pipe_fence(false) else {
        println!("note: skipped -- no pipe to stand in for a fence");
        return;
    };
    ring.submit(1, Some(unsignalled), &[]);
    let held = ring.acquire().expect("the deadline miss holds");
    ring.release(held);

    // The next frame declares a small region; the drop must override it.
    let (signalled, _writer) = pipe_fence(true).expect("a signalled fence");
    ring.submit(
        1,
        Some(signalled),
        &[DamageRect {
            x: 5,
            y: 5,
            w: 6,
            h: 6,
        }],
    );

    let advanced = ring.acquire().expect("advance");
    assert!(
        advanced.damage.is_empty(),
        "a dropped whole-frame update cannot be expressed as rects, so the \
         next frame must repaint everything"
    );

    ring.release(first);
    ring.release(advanced);
}

// --- ExternalDmaBufPool, against a card ---------------------------------------
//
// Parity port of `tests/integration/test_external_dma_buf_pool_vkms.cpp`.

/// One imported dma-buf, ready to submit under any key.
#[cfg(test)]
fn pool_planes(device: &Device) -> Option<(OwnedFd, u32)> {
    real_dma_buf(device, 64, 64)
}

/// A submitted key advances the pool; with nothing new, it holds what is up.
#[test]
#[ignore = "needs a DRM device; run under the vkms lane with --include-ignored"]
fn a_pool_advances_on_a_new_key_and_holds_when_idle_vkms() {
    let _guard = card_guard();
    let Some(device) = open_card() else { return };
    let Some((a_fd, a_pitch)) = pool_planes(&device) else {
        println!("note: skipped -- no importable dma-buf on this card");
        return;
    };
    let Some((b_fd, b_pitch)) = pool_planes(&device) else {
        return;
    };
    let a = [ExternalPlane {
        fd: a_fd.as_fd(),
        offset: 0,
        pitch: a_pitch,
    }];
    let b = [ExternalPlane {
        fd: b_fd.as_fd(),
        offset: 0,
        pitch: b_pitch,
    }];

    let mut pool = ExternalDmaBufPool::new(pool_format(), None);
    assert!(pool.submit(&device, 0xA, &a, None, &[]));
    let first = pool.acquire().expect("key A").fb_id;
    assert_ne!(first, 0);
    assert!(
        !pool.has_fresh_content(),
        "the frame was taken, so there is nothing fresh waiting behind it"
    );

    let held = pool.acquire().expect("hold");
    assert_eq!(
        held.fb_id, first,
        "with nothing new, the last frame stays up"
    );

    assert!(pool.submit(&device, 0xB, &b, None, &[]));
    assert!(
        pool.has_fresh_content(),
        "a submitted frame is what makes the scene bother committing"
    );
    let second = pool.acquire().expect("key B").fb_id;
    assert_ne!(second, 0);
    assert_ne!(second, first, "a different key is a different buffer");
}

/// Releasing a displaced buffer hands its key back; releasing the one still on
/// screen does not.
///
/// The producer reuses a buffer as soon as it hears about it. Firing for the
/// buffer still being scanned out would have it overwritten mid-frame.
#[test]
#[ignore = "needs a DRM device; run under the vkms lane with --include-ignored"]
fn a_pool_releases_the_displaced_key_and_not_the_live_one_vkms() {
    let _guard = card_guard();
    let Some(device) = open_card() else { return };
    let Some((a_fd, a_pitch)) = pool_planes(&device) else {
        println!("note: skipped -- no importable dma-buf on this card");
        return;
    };
    let Some((b_fd, b_pitch)) = pool_planes(&device) else {
        return;
    };
    let a = [ExternalPlane {
        fd: a_fd.as_fd(),
        offset: 0,
        pitch: a_pitch,
    }];
    let b = [ExternalPlane {
        fd: b_fd.as_fd(),
        offset: 0,
        pitch: b_pitch,
    }];

    let released: std::sync::Arc<std::sync::Mutex<Vec<usize>>> = std::sync::Arc::default();
    let recorder = std::sync::Arc::clone(&released);
    let mut pool = ExternalDmaBufPool::new(pool_format(), None);
    pool.set_on_release(Box::new(move |key, _fence| {
        recorder.lock().expect("record").push(key);
    }));
    assert!(
        pool.wants_release_fence(),
        "with a listener attached the scene is worth asking for an out-fence"
    );

    assert!(pool.submit(&device, 0xA, &a, None, &[]));
    let first = pool.acquire().expect("key A");
    assert!(pool.submit(&device, 0xB, &b, None, &[]));
    let second = pool.acquire().expect("key B");

    pool.release_with_fence(first, None);
    assert_eq!(
        *released.lock().expect("read"),
        vec![0xA],
        "A was displaced by B, so A is the producer's again"
    );

    pool.release_with_fence(second, None);
    assert_eq!(
        released.lock().expect("read").len(),
        1,
        "B is still on screen; handing it back would let the producer \
         overwrite what is being scanned out"
    );
}

/// Beyond the cap, the least recently used idle import is dropped.
///
/// Without this the pool grows for as long as the producer keeps minting keys,
/// and each entry holds a GEM handle, a framebuffer and a descriptor until the
/// pool itself drops.
#[test]
#[ignore = "needs a DRM device; run under the vkms lane with --include-ignored"]
fn a_pool_evicts_its_least_recently_used_idle_import_vkms() {
    let _guard = card_guard();
    let Some(device) = open_card() else { return };
    let Some((fd, pitch)) = pool_planes(&device) else {
        println!("note: skipped -- no importable dma-buf on this card");
        return;
    };
    let planes = [ExternalPlane {
        fd: fd.as_fd(),
        offset: 0,
        pitch,
    }];

    let mut pool = ExternalDmaBufPool::new(pool_format(), None);
    pool.set_max_pool(2);

    assert!(pool.submit(&device, 0xA, &planes, None, &[]));
    let first = pool.acquire().expect("key A");
    assert!(pool.submit(&device, 0xB, &planes, None, &[]));
    let second = pool.acquire().expect("key B");

    // A is now idle and unreferenced: displaced from the screen and released.
    pool.release_with_fence(first, None);
    assert_eq!(pool.cached_count(), 2);

    assert!(pool.submit(&device, 0xC, &planes, None, &[]));
    assert_eq!(
        pool.cached_count(),
        2,
        "the third key must displace the stalest one, not push the cache over \
         its cap"
    );
    let third = pool.acquire().expect("key C");
    pool.release_with_fence(second, None);
    pool.release_with_fence(third, None);
}

/// An import the kernel is still scanning out is not torn down to make room.
///
/// It is marked instead, and goes on the next acquire that finds it
/// unreferenced. Destroying a framebuffer mid-scanout is how a display tears.
#[test]
#[ignore = "needs a DRM device; run under the vkms lane with --include-ignored"]
fn eviction_waits_for_a_buffer_that_is_still_in_flight_vkms() {
    let _guard = card_guard();
    let Some(device) = open_card() else { return };
    let Some((fd, pitch)) = pool_planes(&device) else {
        println!("note: skipped -- no importable dma-buf on this card");
        return;
    };
    let planes = [ExternalPlane {
        fd: fd.as_fd(),
        offset: 0,
        pitch,
    }];

    let mut pool = ExternalDmaBufPool::new(pool_format(), None);
    pool.set_max_pool(1);

    assert!(pool.submit(&device, 0xA, &planes, None, &[]));
    let first = pool.acquire().expect("key A");

    assert!(pool.submit(&device, 0xB, &planes, None, &[]));
    let second = pool.acquire().expect("key B");
    assert_eq!(
        pool.cached_count(),
        2,
        "over the cap, but A's commit has not retired -- tearing it down here \
         would destroy a framebuffer the kernel is reading"
    );

    pool.release_with_fence(first, None);
    let held = pool.acquire().expect("hold B, and sweep");
    assert_eq!(
        pool.cached_count(),
        1,
        "with A's token retired the deferred eviction goes through"
    );

    pool.release_with_fence(held, None);
    pool.release_with_fence(second, None);
}

/// A new generation retires the old buffers, once nothing references them.
#[test]
#[ignore = "needs a DRM device; run under the vkms lane with --include-ignored"]
fn a_new_generation_retires_the_previous_buffers_vkms() {
    let _guard = card_guard();
    let Some(device) = open_card() else { return };
    let Some((fd, pitch)) = pool_planes(&device) else {
        println!("note: skipped -- no importable dma-buf on this card");
        return;
    };
    let Some((small_fd, small_pitch)) = real_dma_buf(&device, 32, 32) else {
        return;
    };
    let planes = [ExternalPlane {
        fd: fd.as_fd(),
        offset: 0,
        pitch,
    }];
    let small = [ExternalPlane {
        fd: small_fd.as_fd(),
        offset: 0,
        pitch: small_pitch,
    }];

    let mut pool = ExternalDmaBufPool::new(pool_format(), None);
    assert!(pool.submit(&device, 0xA, &planes, None, &[]));
    let first = pool.acquire().expect("key A");
    assert_eq!(pool.cached_count(), 1);

    let next = SourceFormat {
        fourcc: XRGB8888,
        modifier: 0,
        width: 32,
        height: 32,
    };
    pool.reset_generation(next);
    assert_eq!(
        LayerBufferSource::format(&pool),
        next,
        "future submits import at the new geometry"
    );
    assert_eq!(
        pool.cached_count(),
        1,
        "A is marked, not destroyed: the kernel is still scanning it out"
    );

    // The new generation must arrive under a fresh key.
    assert!(pool.submit(&device, 0xB, &small, None, &[]));
    let second = pool.acquire().expect("key B");
    assert_eq!(
        pool.cached_count(),
        2,
        "A's teardown is still deferred past its commit"
    );

    pool.release_with_fence(first, None);
    let held = pool.acquire().expect("hold, and sweep");
    assert_eq!(
        pool.cached_count(),
        1,
        "with A retired only the new generation is left"
    );

    pool.release_with_fence(held, None);
    pool.release_with_fence(second, None);
}

/// A submit the pool cannot import is skipped, and the last good frame stays.
///
/// Reporting the failure would give the producer thread nothing useful to do
/// about it -- there is no commit there to fail. A frozen layer beats a blank
/// one, so the frame is dropped and what is on screen stays.
#[test]
#[ignore = "needs a DRM device; run under the vkms lane with --include-ignored"]
fn a_submit_the_pool_cannot_import_holds_the_last_frame_vkms() {
    let _guard = card_guard();
    let Some(device) = open_card() else { return };
    let Some((fd, pitch)) = pool_planes(&device) else {
        println!("note: skipped -- no importable dma-buf on this card");
        return;
    };
    let planes = [ExternalPlane {
        fd: fd.as_fd(),
        offset: 0,
        pitch,
    }];

    let mut pool = ExternalDmaBufPool::new(pool_format(), None);
    assert!(pool.submit(&device, 0xA, &planes, None, &[]));
    let good = pool.acquire().expect("key A");
    let fb_a = good.fb_id;

    // A descriptor that is not a dma-buf at all: the PRIME import refuses it.
    let (not_a_buffer, _writer) = std::io::pipe().expect("a pipe is not a dma-buf");
    let bad = [ExternalPlane {
        fd: std::os::fd::AsFd::as_fd(&not_a_buffer),
        offset: 0,
        pitch,
    }];
    assert!(
        !pool.submit(&device, 0xBAD, &bad, None, &[]),
        "an import that failed must say so to a caller that wants to count"
    );
    assert_eq!(
        pool.cached_count(),
        1,
        "nothing was imported, so nothing is cached"
    );
    assert!(
        !pool.has_fresh_content(),
        "and it never reached the presenter, so there is no frame to commit"
    );

    let held = pool.acquire().expect("holding is not an error");
    assert_eq!(
        held.fb_id, fb_a,
        "the last good frame stays up rather than the layer going blank"
    );

    pool.release_with_fence(held, None);
    pool.release_with_fence(good, None);
}

// --- DmaBufSourceCache, against a card ----------------------------------------
//
// Parity port of `tests/integration/test_dma_buf_source_cache_vkms.cpp`.

/// A key hits, a changed shape under the same key replaces it, and evicting
/// or clearing empties the cache.
///
/// The staleness rule is the one worth having on a card. A producer that
/// reallocates at a new resolution keeps its buffer keys -- a V4L2 index, a
/// slot number -- so a cache that only ever hit would hand back a framebuffer
/// describing the previous geometry, and the kernel would scan out the new
/// buffer through the old shape.
#[test]
#[ignore = "needs a DRM device; run under the vkms lane with --include-ignored"]
fn a_cached_key_hits_until_its_shape_changes_vkms() {
    let _guard = card_guard();
    let Some(device) = open_card() else { return };
    let Some((fd, pitch)) = real_dma_buf(&device, 64, 64) else {
        println!("note: skipped -- no importable dma-buf on this card");
        return;
    };
    let planes = [ExternalPlane {
        fd: fd.as_fd(),
        offset: 0,
        pitch,
    }];

    let mut cache = DmaBufSourceCache::new();
    assert!(cache.is_empty(), "a cache starts empty");

    let first = cache
        .get_or_create(7, &device, pool_format(), &planes)
        .expect("import")
        .fb_id();
    assert_eq!(cache.len(), 1);
    assert!(cache.find(7).is_some());

    let again = cache
        .get_or_create(7, &device, pool_format(), &planes)
        .expect("hit")
        .fb_id();
    assert_eq!(first, again, "the same key and shape must reuse the import");
    assert_eq!(cache.len(), 1, "and not import a second time");

    // Same key, new shape: the entry is stale and must be replaced.
    let smaller = SourceFormat {
        fourcc: XRGB8888,
        modifier: 0,
        width: 32,
        height: 32,
    };
    let replaced = cache
        .get_or_create(7, &device, smaller, &planes)
        .expect("re-import at the new shape");
    assert_eq!(
        LayerBufferSource::format(replaced),
        smaller,
        "a hit here would describe the new buffer with the old geometry"
    );
    assert_eq!(cache.len(), 1, "replaced, not accumulated");

    assert!(cache.evict(7));
    assert!(cache.is_empty());
    assert!(cache.find(7).is_none());

    cache.clear();
    assert!(cache.is_empty(), "clearing an empty cache is not an error");
}

/// An explicit modifier reaches the kernel, and so does its absence.
///
/// Which path each takes is `drmkit_core::framebuffer_modifier`'s call: the
/// `INVALID` sentinel always goes as "no modifier", and `LINEAR` is declared
/// exactly when the driver has `DRM_CAP_ADDFB2_MODIFIERS` -- upstream's rule
/// since `be147be`, after LCDIF-class drivers refused the declared form even
/// for linear. Both have to produce a framebuffer, and a cache that conflated
/// them would send one down the other's path.
#[test]
#[ignore = "needs a DRM device; run under the vkms lane with --include-ignored"]
fn an_explicit_modifier_and_its_absence_both_import_vkms() {
    let _guard = card_guard();
    let Some(device) = open_card() else { return };
    let Some((fd, pitch)) = real_dma_buf(&device, 64, 64) else {
        println!("note: skipped -- no importable dma-buf on this card");
        return;
    };
    let planes = [ExternalPlane {
        fd: fd.as_fd(),
        offset: 0,
        pitch,
    }];

    let mut cache = DmaBufSourceCache::new();

    // LINEAR: declared where the driver takes modifiers, otherwise not.
    let implicit = cache
        .get_or_create(1, &device, pool_format(), &planes)
        .expect("import with no declared modifier");
    let implicit_fb = implicit.acquire().expect("acquire").fb_id;
    assert_ne!(implicit_fb, 0);

    // The INVALID sentinel means "the producer is not saying", and takes the
    // same default path rather than being declared as a real layout.
    let unstated = SourceFormat {
        fourcc: XRGB8888,
        modifier: drmkit_fmt::Modifier::INVALID.0,
        width: 64,
        height: 64,
    };
    let sentinel = cache
        .get_or_create(2, &device, unstated, &planes)
        .expect("import with the INVALID sentinel");
    let sentinel_fb = sentinel.acquire().expect("acquire").fb_id;
    assert_ne!(
        sentinel_fb, 0,
        "declaring INVALID to the kernel as a layout would be refused"
    );

    assert_ne!(
        implicit_fb, sentinel_fb,
        "two keys are two imports, whatever they say about modifiers"
    );
    assert_eq!(cache.len(), 2);

    // GETFB2 is Linux 5.7; on an older kernel there is nothing to read back.
    let declared = |fb_id: u32| {
        let handle = drm::control::framebuffer::Handle::from(
            std::num::NonZeroU32::new(fb_id).expect("non-zero"),
        );
        drm::control::Device::get_planar_framebuffer(&device, handle)
            .ok()
            .map(|info| info.modifier().map(u64::from))
    };
    match declared(implicit_fb) {
        None => println!("note: no GETFB2 on this kernel; the readback is skipped"),
        Some(modifier) if drmkit_core::supports_framebuffer_modifiers(&device) => {
            assert_eq!(modifier, Some(0), "a LINEAR import is LINEAR");
        }
        Some(_) => {}
    }
}

/// The pool lends its acquired import's descriptors, like the other two.
///
/// This was the one of the three that could not, and the reason was
/// structural rather than an oversight: its imports live behind a `Mutex`,
/// and `DmaBufDesc` borrows what it describes, so a borrow taken inside the
/// guard cannot outlive it. Holding the acquired import as an `Arc` outside
/// the lock is what makes the borrow come from the pool. Recorded as P-29
/// while it was open.
#[test]
#[ignore = "needs a DRM device; run under the vkms lane with --include-ignored"]
fn a_pool_lends_its_acquired_descriptors_vkms() {
    let _guard = card_guard();
    let Some(device) = open_card() else { return };
    let Some((fd, pitch)) = pool_planes(&device) else {
        println!("note: skipped -- no importable dma-buf on this card");
        return;
    };
    let planes = [ExternalPlane {
        fd: fd.as_fd(),
        offset: 0,
        pitch,
    }];

    let mut pool = ExternalDmaBufPool::new(pool_format(), None);

    assert!(
        matches!(pool.export_dma_buf(), Err(SourceError::Unsupported)),
        "nothing has been acquired, so there is nothing to describe"
    );

    assert!(pool.submit(&device, 0xA, &planes, None, &[]));
    let acquired = pool.acquire().expect("key A");

    let exported = pool
        .export_dma_buf()
        .expect("the pool has to lend, like the others");
    assert_eq!(exported.fds.len(), 1);
    assert_eq!(exported.pitches, vec![pitch]);
    assert_eq!(exported.format, pool_format());

    assert!(
        matches!(
            pool.map(drmkit_dumb::MapAccess::Read),
            Err(SourceError::Unsupported)
        ),
        "and the CPU map is still correctly unsupported -- the export is what \
         keeps the layer compositable, not a mapping"
    );

    pool.release(acquired);
}

/// A session pause drops what the pool was lending.
///
/// The descriptors belong to a device that is going away. Continuing to
/// describe them would have composition read from a buffer whose backing is
/// gone.
#[test]
#[ignore = "needs a DRM device; run under the vkms lane with --include-ignored"]
fn a_paused_pool_lends_nothing_vkms() {
    let _guard = card_guard();
    let Some(device) = open_card() else { return };
    let Some((fd, pitch)) = pool_planes(&device) else {
        println!("note: skipped -- no importable dma-buf on this card");
        return;
    };
    let planes = [ExternalPlane {
        fd: fd.as_fd(),
        offset: 0,
        pitch,
    }];

    let mut pool = ExternalDmaBufPool::new(pool_format(), None);
    assert!(pool.submit(&device, 0xA, &planes, None, &[]));
    let acquired = pool.acquire().expect("key A");
    assert!(pool.export_dma_buf().is_ok());
    pool.release(acquired);

    pool.on_session_paused();

    assert!(
        matches!(pool.export_dma_buf(), Err(SourceError::Unsupported)),
        "the descriptors belong to a device that is going away"
    );
    assert_eq!(pool.cached_count(), 0, "and the imports went with it");
}
