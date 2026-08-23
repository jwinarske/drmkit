// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! Parity port of `tests/unit/test_device.cpp`, `test_property_store.cpp`, and
//! `test_atomic_builder.cpp` from drm-cxx @ `dc2915b`.
//!
//! The upstream device and atomic suites skip themselves when no card is
//! present. Here the card-dependent cases are `#[ignore]` instead, so they run
//! under the vkms lane with `--include-ignored` and stay visible as skipped
//! rather than silently passing — the same reasoning as the vkms lane's own
//! health checks.

use super::*;
use std::os::fd::{AsRawFd as _, OwnedFd};

/// Serializes the card-dependent cases.
///
/// `Device::open` acquires DRM master, and master is held per open file
/// description: only one can have it at a time. Several of these cases open the
/// same card, so run in parallel they race for it -- one wins and the rest get
/// `EACCES`, which is indistinguishable from a real permission failure. The
/// mutex makes each case the only claimant while it runs.
static CARD_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Take the card lock, ignoring poisoning: a panicking case has already failed
/// and must not cascade into every other one.
fn card_guard() -> std::sync::MutexGuard<'static, ()> {
    CARD_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// The card the ignored cases use, matching the vkms lane's environment.
fn test_card() -> String {
    std::env::var("DRMKIT_TEST_CARD").unwrap_or_else(|_| "/dev/dri/card0".to_owned())
}

/// A `TEST_ONLY` commit requires DRM master. Under virtme-ng the suite runs as
/// root with no display server and always gets it; on a developer desktop a
/// compositor already holds it and every atomic call returns `EACCES`.
///
/// Rather than skip -- which would let the lane pass while testing nothing --
/// the lane sets `DRMKIT_REQUIRE_MASTER=1` and these cases then treat
/// `NotMaster` as a failure. Locally it is reported and tolerated, so
/// `--include-ignored` stays usable without a bare console.
fn check_master_dependent(outcome: Result<()>, what: &str) {
    let require = std::env::var_os("DRMKIT_REQUIRE_MASTER").is_some();
    match outcome {
        Ok(()) => {}
        Err(CoreError::NotMaster) if !require => {
            println!("note: {what} skipped -- another client holds DRM master");
        }
        Err(CoreError::NotMaster) => {
            panic!("{what}: not DRM master, but DRMKIT_REQUIRE_MASTER is set");
        }
        Err(e) => panic!("{what} failed: {e}"),
    }
}

fn open_dev_null() -> OwnedFd {
    std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/null")
        .expect("/dev/null")
        .into()
}

// --- test_device.cpp ---------------------------------------------------------

/// `DeviceTest.OpenInvalidPathReturnsError`
#[test]
fn open_invalid_path_returns_error() {
    let result = Device::open("/dev/dri/nonexistent_card_999");
    assert!(result.is_err());
}

/// `DeviceTest.OpenEmptyPathReturnsError`
#[test]
fn open_empty_path_returns_error() {
    assert!(Device::open("").is_err());
}

/// `DeviceTest.MoveConstructorTransfersFd`
///
/// The C++ case name is aspirational — its body only checks that opening a
/// non-DRM node fails, because it cannot rely on a card in CI. That check is
/// the valuable part and is kept; the actual move semantics are covered below,
/// where Rust can express them.
#[test]
fn open_non_drm_node_is_rejected() {
    let result = Device::open("/dev/null");
    assert!(
        matches!(result, Err(CoreError::NotADrmDevice { .. })),
        "/dev/null is a character device but not a DRM one"
    );
}

/// `DeviceTest.FromFdDoesNotCloseOnDestruction`
///
/// A borrowed device must not close the caller's descriptor — a seat session
/// holding a revocable libseat fd depends on it.
#[test]
fn borrowed_device_does_not_close_the_descriptor() {
    let owned = open_dev_null();
    let raw = owned.as_raw_fd();

    {
        // SAFETY: `owned` outlives this scope, so the borrowed descriptor stays
        // valid for the whole life of the device.
        let device = unsafe { Device::from_borrowed_fd(raw) };
        assert_eq!(device.raw_fd(), raw);
        assert!(!device.owns_fd());
    }

    // Still open: fstat succeeds only on a live descriptor.
    assert!(
        rustix::fs::fstat(&owned).is_ok(),
        "a borrowed device must not close the caller's descriptor"
    );
}

/// `DeviceTest.FromFdMoveDoesNotCloseBorrowedFd`
///
/// Moving preserves the non-owning semantics.
#[test]
fn moving_a_borrowed_device_keeps_it_borrowed() {
    let owned = open_dev_null();
    let raw = owned.as_raw_fd();

    {
        // SAFETY: as above.
        let source = unsafe { Device::from_borrowed_fd(raw) };
        let moved = source;
        assert_eq!(moved.raw_fd(), raw);
        assert!(!moved.owns_fd());
    }

    assert!(rustix::fs::fstat(&owned).is_ok());
}

/// An owning device closes on drop. The C++ has no counterpart: `from_fd` is
/// always non-owning there, because C++ cannot express the transfer in a type.
///
/// Probing whether one specific descriptor *number* is closed does not work:
/// the harness runs cases on parallel threads, and a number freed by this drop
/// can be handed straight to another case's `open`, making the check report
/// "still open". That is exactly how an earlier version of this test passed on
/// x86_64 and failed under qemu-riscv64, where the scheduling differs.
///
/// Measuring the descriptor table instead is race-tolerant in the same way the
/// `drmkit-sync` leak test is: a missing close grows the table once per
/// iteration, so the signal scales with the loop count while concurrent noise
/// stays a small constant.
#[test]
fn owned_device_closes_on_drop() {
    let open_and_drop = |times: usize| {
        for _ in 0..times {
            let device = Device::from_owned_fd(open_dev_null());
            assert!(device.owns_fd());
            drop(device);
        }
    };

    let count = || {
        std::fs::read_dir("/proc/self/fd")
            .expect("/proc/self/fd")
            .count()
    };

    open_and_drop(16);
    let after_few = count();

    open_and_drop(240);
    let after_many = count();

    let growth = after_many.saturating_sub(after_few);
    assert!(
        growth < 16,
        "240 further owning devices grew the fd table by {growth}; \
         a device that failed to close would grow it by 240"
    );
}

/// The mirror of the above: a borrowed device must *not* close, so the table
/// grows by one per iteration when the caller keeps the descriptors alive.
#[test]
fn borrowed_device_leaves_the_descriptor_to_the_caller() {
    let mut kept = Vec::new();
    for _ in 0..64 {
        let owned = open_dev_null();
        let raw = owned.as_raw_fd();
        // SAFETY: `owned` is pushed to `kept` below and outlives the device.
        let device = unsafe { Device::from_borrowed_fd(raw) };
        assert_eq!(device.raw_fd(), raw);
        drop(device);
        // Still valid because the device did not close it.
        assert!(rustix::fs::fstat(&owned).is_ok());
        kept.push(owned);
    }
    assert_eq!(kept.len(), 64);
}

// `DeviceTest.SetClientCapOnInvalidFdFails` has no Rust counterpart.
//
// The C++ passes the capability number 0xFFFFFFFF and expects rejection.
// `ClientCapability` is an enum here, so an out-of-range capability cannot be
// named -- the same reason the `fd = -1` cases upstream do not survive the
// port. Substituting a real-but-unlikely capability was tried and does not
// work either: vkms accepts `Stereo3D`, so the substitute tested nothing and
// only encoded a guess about driver support. What the case was really
// protecting -- that `set_client_cap` reports driver rejection rather than
// swallowing it -- is exercised by `open_card_and_enable_atomic_vkms`, which
// fails loudly if either capability is refused.

#[test]
#[ignore = "needs a DRM device; run under the vkms lane with --include-ignored"]
fn open_card_and_enable_atomic_vkms() {
    let _guard = card_guard();
    let device = Device::open(test_card()).expect("open test card");
    device
        .enable_universal_planes()
        .expect("universal planes must be available");
    device.enable_atomic().expect("atomic must be available");
    assert!(device.owns_fd());
}

// --- test_property_store.cpp -------------------------------------------------

/// `PropertyStoreTest.LookupOnEmptyStoreReturnsError`
#[test]
fn property_id_on_empty_store_is_an_error() {
    let store = PropertyStore::new();
    assert!(matches!(
        store.property_id(42, "type"),
        Err(CoreError::NoSuchObject { object_id: 42 })
    ));
}

/// `PropertyStoreTest.PropertyValueOnEmptyStoreReturnsError`
#[test]
fn property_value_on_empty_store_is_an_error() {
    let store = PropertyStore::new();
    assert!(store.property_value(42, "type").is_err());
    assert!(store.is_immutable(42, "type").is_err());
}

/// `PropertyStoreTest.PropertiesOnEmptyStoreReturnsNull`
#[test]
fn properties_on_empty_store_is_none() {
    let store = PropertyStore::new();
    assert!(store.properties(42).is_none());
    assert!(store.is_empty());
}

/// `PropertyStoreTest.ClearEmptiesStore`
#[test]
fn clear_empties_the_store() {
    let mut store = PropertyStore::new();
    store.clear();
    assert!(store.properties(42).is_none());
    assert_eq!(store.len(), 0);
}

/// `PropertyStoreTest.CacheWithInvalidFdReturnsError`
///
/// The C++ passes `fd = -1`. That does not compile here: caching takes a
/// `&Device`, so there is no invalid descriptor to pass. The equivalent
/// reachable failure is caching an object id the kernel does not know, which
/// the vkms case below covers.
#[test]
#[ignore = "needs a DRM device; run under the vkms lane with --include-ignored"]
fn cache_unknown_object_is_an_error_vkms() {
    let _guard = card_guard();
    let device = Device::open(test_card()).expect("open test card");
    let mut store = PropertyStore::new();
    assert!(
        store
            .cache_properties(&device, 0xdead_beef, ObjectType::Connector)
            .is_err(),
        "an object id the kernel does not know must fail"
    );
    assert!(store.is_empty(), "a failed cache must not leave an entry");
}

/// Caching a real plane must yield the properties the allocator depends on.
#[test]
#[ignore = "needs a DRM device; run under the vkms lane with --include-ignored"]
fn cache_real_plane_properties_vkms() {
    use drm::control::Device as _;

    let device = Device::open(test_card()).expect("open test card");
    device.enable_universal_planes().expect("universal planes");
    device.enable_atomic().expect("atomic");

    let planes = device.plane_handles().expect("plane handles");
    let plane = *planes.first().expect("vkms exposes at least one plane");
    let plane_id: u32 = plane.into();

    let mut store = PropertyStore::new();
    store
        .cache_properties(&device, plane_id, ObjectType::Plane)
        .expect("cache plane properties");

    let props = store.properties(plane_id).expect("cached");
    assert!(!props.is_empty());

    // Every KMS plane advertises "type", and it is immutable on every driver.
    let type_id = store.property_id(plane_id, "type").expect("type property");
    assert_ne!(type_id, 0, "property ids are non-zero by construction");
    assert!(
        store.is_immutable(plane_id, "type").expect("type"),
        "plane type is immutable, so a request builder must skip it"
    );

    assert!(store.property_id(plane_id, "NOT_A_REAL_PROPERTY").is_err());
    store.forget(plane_id);
    assert!(store.properties(plane_id).is_none());
}

// --- test_atomic_builder.cpp -------------------------------------------------

/// `AtomicRequestTest.ConstructAndDestroy`
///
/// The C++ constructs from a `Device` because it allocates a
/// `drmModeAtomicReq` and needs the fd. Here a request is a plain vector, so
/// construction needs no device and cannot fail — there is no `valid()` to
/// check.
#[test]
fn construct_is_infallible_and_empty() {
    let request = AtomicRequest::new();
    assert!(request.is_empty());
    assert_eq!(request.len(), 0);
    assert!(request.writes().is_empty());
}

/// `AtomicRequestTest.AddPropertyQueuesWithoutValidation`
///
/// Non-zero ids queue without being checked against real kernel objects;
/// libdrm behaves the same, and the check happens at commit time.
#[test]
fn add_property_queues_without_validating_against_the_kernel() {
    const SENTINEL_OBJECT: u32 = 0xdead_beef;
    const SENTINEL_PROP: u32 = 0xcafe_f00d;

    let mut request = AtomicRequest::new();
    request
        .add_property(SENTINEL_OBJECT, SENTINEL_PROP, 0)
        .expect("non-zero ids queue");

    assert_eq!(request.len(), 1);
    assert_eq!(
        request.writes()[0],
        PropertyWrite {
            object_id: SENTINEL_OBJECT,
            property_id: SENTINEL_PROP,
            value: 0,
        }
    );
}

/// libdrm rejects a zero id outright, and so does this.
///
/// The rejection is load-bearing rather than cosmetic: DRM resource handles are
/// `NonZeroU32`, so a zero id has no representation to build a request from.
/// Accepting one could only mean dropping it at build time — a commit going out
/// one property short while reporting success.
#[test]
fn add_property_rejects_zero_ids() {
    let mut request = AtomicRequest::new();
    assert!(request.add_property(0, 1, 0).is_err(), "zero object id");
    assert!(request.add_property(1, 0, 0).is_err(), "zero property id");
    assert!(request.add_property(0, 0, 0).is_err());
    assert!(request.is_empty(), "a rejected write must not be queued");
}

/// Order and duplicates are preserved: the kernel applies writes in order, so a
/// later write to the same property wins. Collapsing them would change what a
/// replayed trace means.
#[test]
fn writes_preserve_order_and_duplicates() {
    let mut request = AtomicRequest::new();
    request.add_property(1, 10, 100).unwrap();
    request.add_property(2, 20, 200).unwrap();
    request.add_property(1, 10, 999).unwrap(); // same target, later value

    let writes = request.writes();
    assert_eq!(writes.len(), 3);
    assert_eq!(writes[0].value, 100);
    assert_eq!(writes[1].object_id, 2);
    assert_eq!(writes[2].value, 999, "the later write must survive");
}

/// `clear` empties the queue while keeping the allocation for the next frame.
#[test]
fn clear_empties_the_queue() {
    let mut request = AtomicRequest::with_capacity(8);
    request.add_property(1, 2, 3).unwrap();
    assert_eq!(request.len(), 1);

    request.clear();
    assert!(request.is_empty());
    assert!(request.writes().is_empty());
}

/// `AtomicRequestTest.MoveConstruct` / `MoveAssign`
///
/// A move must carry the queued writes intact.
#[test]
fn move_transfers_queued_writes() {
    let mut source = AtomicRequest::new();
    source.add_property(1, 2, 3).unwrap();
    source.add_property(4, 5, 6).unwrap();

    let moved = source;
    assert_eq!(moved.len(), 2);
    assert_eq!(moved.writes()[1].object_id, 4);
}

/// `AtomicRequestTest.TestCommitWithEmptyRequest` — needs a card.
#[test]
#[ignore = "needs a DRM device; run under the vkms lane with --include-ignored"]
fn test_commit_with_empty_request_vkms() {
    let _guard = card_guard();
    let device = Device::open(test_card()).expect("open test card");
    device.enable_atomic().expect("atomic");

    let request = AtomicRequest::new();
    // An empty atomic request is valid: it changes nothing.
    check_master_dependent(
        request.test(&device, AtomicCommitFlags::empty()),
        "empty request must validate",
    );
}

/// A request naming objects the kernel does not know must be rejected at
/// commit time, not at insertion.
#[test]
#[ignore = "needs a DRM device; run under the vkms lane with --include-ignored"]
fn test_rejects_unknown_objects_vkms() {
    let _guard = card_guard();
    let device = Device::open(test_card()).expect("open test card");
    device.enable_atomic().expect("atomic");

    let mut request = AtomicRequest::new();
    request.add_property(0xdead_beef, 0xcafe_f00d, 0).unwrap();

    match request.test(&device, AtomicCommitFlags::empty()) {
        Err(CoreError::NotMaster) => {
            // Says nothing about the ids; the commit never reached validation.
            println!("note: skipped -- another client holds DRM master");
            assert!(
                std::env::var_os("DRMKIT_REQUIRE_MASTER").is_none(),
                "not DRM master, but DRMKIT_REQUIRE_MASTER is set"
            );
        }
        Err(_) => {}
        Ok(()) => panic!("the kernel must reject ids that name nothing"),
    }
}

/// `test` masks `PAGE_FLIP_EVENT`, which the kernel rejects with `EINVAL`
/// alongside `TEST_ONLY`. Callers forward commit flags verbatim, so the mask
/// has to live here.
#[test]
#[ignore = "needs a DRM device; run under the vkms lane with --include-ignored"]
fn test_masks_page_flip_event_vkms() {
    let _guard = card_guard();
    let device = Device::open(test_card()).expect("open test card");
    device.enable_atomic().expect("atomic");

    let request = AtomicRequest::new();
    check_master_dependent(
        request.test(&device, AtomicCommitFlags::PAGE_FLIP_EVENT),
        "PAGE_FLIP_EVENT must be masked out rather than rejected",
    );
}
