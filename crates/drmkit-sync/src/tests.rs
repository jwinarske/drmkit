// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! Parity port of `tests/unit/test_sync_fence.cpp` from drm-cxx @ `dc2915b`.
//!
//! Like the C++, these use an `eventfd` as a stand-in for a real `sync_file`:
//! `wait` polls for `POLLIN`, and a written-to eventfd is readable, so the
//! signaled and unsignaled cases are both reachable without a GPU.
//!
//! All six upstream cases are covered, so this file is `ported`.
//!
//! `merge` is exercised by neither implementation. `SYNC_IOC_MERGE` is a real
//! `sync_file` ioctl that an eventfd rejects with `ENOTTY`, so a host test
//! could only assert the error path — which would pin the stub, not the merge.
//! It needs fences a commit actually produced, so it belongs in the vkms lane
//! alongside the explicit-sync work in phase 6+. Noting the gap here because
//! "ported" refers to parity with upstream coverage, not to merge being
//! tested.

use super::*;
use std::os::fd::AsRawFd as _;

use rustix::event::{EventfdFlags, eventfd};

/// An unsignaled eventfd, the C++ fixture's stand-in for a pending fence.
fn pending_fence_fd() -> OwnedFd {
    eventfd(0, EventfdFlags::CLOEXEC).expect("eventfd")
}

/// An eventfd with a count already written: readable, so `poll` reports it
/// immediately, standing in for a signaled fence.
fn signaled_fence_fd() -> OwnedFd {
    eventfd(1, EventfdFlags::CLOEXEC).expect("eventfd")
}

// --- parity with test_sync_fence.cpp ----------------------------------------

/// `SyncFenceTest.ImportInvalidFdReturnsError`
///
/// The C++ passes `-1` and expects `bad_file_descriptor`. That call does not
/// compile here: [`SyncFence::import`] takes a [`BorrowedFd`], so an invalid
/// descriptor is rejected by the type system rather than at runtime. What
/// remains testable is the same underlying contract — an empty fence is not
/// usable — which the operations below enforce.
#[test]
fn empty_fence_is_invalid_and_unusable() {
    let fence = SyncFence::new();
    assert!(!fence.is_valid());
    assert!(fence.as_fd().is_none());
    assert!(matches!(fence.wait(Duration::ZERO), Err(SyncError::Empty)));

    let mut fence = SyncFence::new();
    assert!(matches!(
        fence.merge(SyncFence::new()),
        Err(SyncError::Empty)
    ));
}

/// `SyncFenceTest.ImportValidFdSucceeds` — including the C++ comment's point
/// that the import dups, so the original can be closed immediately.
#[test]
fn import_dups_so_the_original_can_close() {
    let original = pending_fence_fd();
    let fence = SyncFence::import(original.as_fd()).expect("import");
    assert!(fence.is_valid());

    let duped_raw = fence.as_fd().expect("fd").as_raw_fd();
    assert_ne!(
        duped_raw,
        original.as_raw_fd(),
        "import must duplicate, not alias"
    );

    // Close the caller's copy; the fence must remain usable.
    drop(original);
    assert!(fence.is_valid());
    assert!(matches!(
        fence.wait(Duration::from_millis(1)),
        Err(SyncError::TimedOut)
    ));
}

/// `SyncFenceTest.MoveConstructor` / `SyncFenceTest.MoveAssignment`
///
/// In C++ a moved-from fence has `fd() == -1` and the move must not close the
/// descriptor twice. In Rust a move is not observable — the source is gone —
/// so the checkable property is that the descriptor survives the move and is
/// closed exactly once. `into_owned_fd` makes that observable.
#[test]
fn move_transfers_the_descriptor_intact() {
    let source = SyncFence::import(pending_fence_fd().as_fd()).expect("import");
    let raw = source.as_fd().expect("fd").as_raw_fd();

    let moved = source; // move
    assert!(moved.is_valid());
    assert_eq!(moved.as_fd().expect("fd").as_raw_fd(), raw);

    let owned = moved.into_owned_fd().expect("descriptor released");
    assert_eq!(owned.as_raw_fd(), raw);
}

/// `SyncFenceTest.WaitOnSignaledEventFdSucceeds`
#[test]
fn wait_on_signaled_fence_succeeds() {
    let fence = SyncFence::import(signaled_fence_fd().as_fd()).expect("import");
    fence
        .wait(Duration::from_secs(1))
        .expect("a signaled fence must not time out");
}

/// `SyncFenceTest.WaitTimesOut`
#[test]
fn wait_times_out_on_pending_fence() {
    let fence = SyncFence::import(pending_fence_fd().as_fd()).expect("import");
    assert!(matches!(
        fence.wait(Duration::from_millis(10)),
        Err(SyncError::TimedOut)
    ));
}

/// A zero timeout is a poll, not an infinite wait — the distinction matters
/// for a per-frame fence check that must not block the commit path.
#[test]
fn zero_timeout_polls_without_blocking() {
    let signaled = SyncFence::import(signaled_fence_fd().as_fd()).expect("import");
    signaled.wait(Duration::ZERO).expect("already readable");

    let pending = SyncFence::import(pending_fence_fd().as_fd()).expect("import");
    assert!(matches!(
        pending.wait(Duration::ZERO),
        Err(SyncError::TimedOut)
    ));
}

// --- fd lifecycle (the risk the C++ acquire-fence test targets) -------------

/// The whole point of owning the descriptor: no leak across many imports. The
/// C++ `test_acquire_fence.cpp` watches `/proc/self/fd` for the same reason.
///
/// `/proc/self/fd` is process-global and the test harness runs cases on
/// parallel threads, so an absolute before/after count is racy — a concurrent
/// case opening one eventfd makes it fail. Comparing the count after a small
/// number of iterations against the count after a large one makes the leak
/// signal proportional to the iteration count while the concurrent noise stays
/// a small constant: a real leak shows a difference of ~240, unrelated test
/// activity a handful at most.
#[test]
fn repeated_import_and_drop_does_not_leak() {
    let base = pending_fence_fd();

    let import_and_drop = |times: usize| {
        for _ in 0..times {
            let fence = SyncFence::import(base.as_fd()).expect("import");
            assert!(fence.is_valid());
            drop(fence);
        }
    };

    import_and_drop(16);
    let after_few = open_fd_count();

    import_and_drop(240);
    let after_many = open_fd_count();

    let growth = after_many.saturating_sub(after_few);
    assert!(
        growth < 16,
        "240 further import/drop cycles grew the fd table by {growth}; \
         a per-import leak would grow it by 240"
    );
}

/// `into_owned_fd` transfers ownership out; the fence is empty afterwards and
/// the descriptor stays open until the caller drops it.
#[test]
fn into_owned_fd_transfers_ownership() {
    let fence = SyncFence::import(pending_fence_fd().as_fd()).expect("import");
    let raw = fence.as_fd().expect("fd").as_raw_fd();

    let owned = fence.into_owned_fd().expect("some");
    assert_eq!(owned.as_raw_fd(), raw);

    // Still open: a second fence can be made from it.
    let again = SyncFence::import(owned.as_fd()).expect("still a live descriptor");
    assert!(again.is_valid());
}

/// `from_owned` adopts without duplicating — the `OUT_FENCE_PTR` path.
#[test]
fn from_owned_adopts_without_duplicating() {
    let fd = pending_fence_fd();
    let raw = fd.as_raw_fd();

    let fence = SyncFence::from_owned(fd);
    assert_eq!(
        fence.as_fd().expect("fd").as_raw_fd(),
        raw,
        "adopted, not duplicated"
    );
}

/// Count this process's open descriptors.
fn open_fd_count() -> usize {
    std::fs::read_dir("/proc/self/fd")
        .expect("/proc/self/fd")
        .count()
}
