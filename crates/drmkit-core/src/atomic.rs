// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! Atomic modeset requests.
//!
//! Port of `src/modeset/atomic.{hpp,cpp}`.

use std::os::fd::{FromRawFd as _, OwnedFd};
use std::sync::LazyLock;
use std::sync::atomic::{AtomicBool, Ordering};

use drm::control::{AtomicCommitFlags, Device as ControlDevice, atomic::AtomicModeReq};

use crate::device::Device;
use crate::error::{CoreError, Result};

/// One accumulated `(object, property, value)` write.
///
/// This is the DR-9 seam. The C++ exposes `AtomicRequest::native_handle()`
/// returning a raw `drmModeAtomicReq*`, because NVIDIA's
/// `eglStreamConsumerAcquireAttribEXT` takes one through
/// `EGL_DRM_ATOMIC_REQUEST_NV` and submits the commit itself. That pointer
/// cannot exist here: `drm-ffi` builds the kernel struct from flat slices at
/// commit time and never materializes a libdrm request object.
///
/// Exporting the writes instead lets `drmkit-scene-streams` replay them into a
/// libdrm request it owns, so libdrm stays out of `drmkit-core` and out of
/// GPU-less builds. The same ordered view is what T7's property-write trace
/// differ consumes, so the seam has a consumer in the minimal cut rather than
/// lying dormant until the streams tier lands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PropertyWrite {
    /// The DRM object being programmed.
    pub object_id: u32,
    /// The property id on that object.
    pub property_id: u32,
    /// The value to write.
    pub value: u64,
}

/// An atomic modeset request under construction.
///
/// # Deviation: no libdrm request object
///
/// The C++ allocates a `drmModeAtomicReq` up front and appends to it, so its
/// `valid()` reports whether that allocation succeeded. Here the request is a
/// plain vector of [`PropertyWrite`]s and the `drm` crate's [`AtomicModeReq`]
/// is built at commit time. There is no allocation to fail, so there is no
/// `valid()` — construction is infallible.
#[derive(Debug, Clone, Default)]
pub struct AtomicRequest {
    writes: Vec<PropertyWrite>,
}

impl AtomicRequest {
    /// An empty request.
    #[must_use]
    pub const fn new() -> Self {
        Self { writes: Vec::new() }
    }

    /// An empty request with room for `capacity` writes.
    #[must_use]
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            writes: Vec::with_capacity(capacity),
        }
    }

    /// Append a property write.
    ///
    /// Order is preserved and duplicates are kept: the kernel applies writes in
    /// order, so a later write to the same property wins, and collapsing them
    /// here would change what a replayed trace means.
    ///
    /// # Errors
    ///
    /// [`CoreError::Io`] with `EINVAL` if either id is zero. libdrm's
    /// `drmModeAtomicAddProperty` rejects a zero id the same way, and rejecting
    /// at insertion is what keeps the commit path total: DRM resource handles
    /// are non-zero by construction, so a zero id has no representation to
    /// build a request from and could only be dropped silently — a commit going
    /// out one property short, which is far worse than an error here.
    ///
    /// Note that non-zero ids are **not** checked against real kernel objects.
    /// libdrm does not either; that check happens at commit time.
    pub fn add_property(&mut self, object_id: u32, property_id: u32, value: u64) -> Result<()> {
        if object_id == 0 || property_id == 0 {
            return Err(CoreError::Io(rustix::io::Errno::INVAL));
        }
        self.writes.push(PropertyWrite {
            object_id,
            property_id,
            value,
        });
        Ok(())
    }

    /// The accumulated writes, in submission order.
    ///
    /// See [`PropertyWrite`] for why this exists rather than a raw request
    /// pointer.
    #[must_use]
    pub fn writes(&self) -> &[PropertyWrite] {
        &self.writes
    }

    /// How many writes are queued.
    #[must_use]
    pub fn len(&self) -> usize {
        self.writes.len()
    }

    /// Whether nothing is queued.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.writes.is_empty()
    }

    /// Drop every queued write, keeping the allocation for the next frame.
    pub fn clear(&mut self) {
        self.writes.clear();
    }

    /// Build the `drm` crate request from the accumulated writes.
    ///
    /// Fallible rather than skipping: `RawResourceHandle` is a `NonZeroU32`, so
    /// a zero id has no representation. Skipping one would submit a commit
    /// missing a property and report success. [`AtomicRequest::add_property`]
    /// already rejects zero ids, so this is unreachable in practice — but the
    /// failure mode it guards against is silent corruption, which is worth a
    /// branch.
    fn build(&self) -> Result<AtomicModeReq> {
        let mut req = AtomicModeReq::new();
        for write in &self.writes {
            let (Some(object), Some(property)) = (
                drm::control::RawResourceHandle::new(write.object_id),
                drm::control::RawResourceHandle::new(write.property_id),
            ) else {
                return Err(CoreError::Io(rustix::io::Errno::INVAL));
            };
            req.add_raw_property(object, property.into(), write.value);
        }
        Ok(req)
    }

    /// Validate the request without applying it (`DRM_MODE_ATOMIC_TEST_ONLY`).
    ///
    /// `PAGE_FLIP_EVENT` is masked out of `flags`. The kernel rejects
    /// `TEST_ONLY | PAGE_FLIP_EVENT` with `EINVAL` — a test applies nothing to
    /// hardware, so there is no flip to queue an event on — and callers
    /// routinely forward the same flags they would pass to
    /// [`commit`](Self::commit). The allocator replays caller flags for its
    /// internal test commits, so masking here beats making every caller
    /// remember.
    ///
    /// # Errors
    ///
    /// [`CoreError::NotMaster`] if the caller lost DRM master, otherwise
    /// [`CoreError::Io`] with the kernel's rejection reason.
    pub fn test(&self, device: &Device, flags: AtomicCommitFlags) -> Result<()> {
        let test_flags =
            (flags - AtomicCommitFlags::PAGE_FLIP_EVENT) | AtomicCommitFlags::TEST_ONLY;
        let result = device
            .atomic_commit(test_flags, self.build()?)
            .map_err(|e| CoreError::from_io(&e));
        self.trace(device, &result, "test");
        result
    }

    /// Apply the request.
    ///
    /// # Errors
    ///
    /// [`CoreError::NotMaster`] if the caller is not DRM master — the one
    /// failure a scene suspends on (invariant 2) — otherwise [`CoreError::Io`].
    pub fn commit(&self, device: &Device, flags: AtomicCommitFlags) -> Result<()> {
        let result = device
            .atomic_commit(flags, self.build()?)
            .map_err(|e| CoreError::from_io(&e));
        self.trace(device, &result, "commit");
        result
    }

    /// Log every write this request carries, property ids resolved to names.
    ///
    /// A rejected atomic request says only that the kernel disliked
    /// *something*, and nothing in the error names which property. No-op
    /// unless `DRM_ATOMIC_DEBUG` or `DRM_ALLOC_DEBUG` is set -- upstream's
    /// switches, so one variable works on both implementations -- and then
    /// routed through [`log_channel!`](drmkit_log::log_channel), since the
    /// variable is already the opt-in. `why` labels the dump.
    ///
    /// [`test`](Self::test) and [`commit`](Self::commit) call it themselves on
    /// a rejection, and on the first request accepted after one: a rejected
    /// request beside an accepted one is what shows the difference, where a
    /// rejection alone is read as whatever looks suspicious. Upstream wires
    /// the same two dumps into its allocator and scene; here every request
    /// goes through these two methods, so wiring them once covers the
    /// allocator, the present backend and the cursor alike.
    pub fn dump(&self, device: &Device, why: &str) {
        if !*ATOMIC_DEBUG {
            return;
        }
        let name_of = |property_id: u32| {
            let handle = drm::control::property::Handle::from(
                drm::control::RawResourceHandle::new(property_id)?,
            );
            let info = device.get_property(handle).ok()?;
            Some(info.name().to_string_lossy().into_owned())
        };
        for line in self.dump_lines(why, name_of) {
            drmkit_log::log_channel!(drmkit_log::LogLevel::Debug, "{line}");
        }
    }

    /// The dump's text, with names supplied by `name_of`.
    pub(crate) fn dump_lines(
        &self,
        why: &str,
        name_of: impl Fn(u32) -> Option<String>,
    ) -> Vec<String> {
        let count = self.writes.len();
        let mut lines = Vec::with_capacity(count + 1);
        lines.push(format!(
            "[atomic] {why}: {count} propert{} in this request",
            if count == 1 { "y" } else { "ies" }
        ));
        for write in &self.writes {
            let name = name_of(write.property_id).unwrap_or_else(|| "?".to_owned());
            lines.push(format!(
                "[atomic]   obj={:<4} {name:<24} = {}",
                write.object_id, write.value
            ));
        }
        lines
    }

    /// Dump a rejection, and the first acceptance that follows one.
    fn trace(&self, device: &Device, result: &Result<()>, what: &str) {
        if !*ATOMIC_DEBUG {
            return;
        }
        match result {
            Ok(()) => {
                if SAW_REJECTION.swap(false, Ordering::Relaxed) {
                    self.dump(
                        device,
                        &format!("{what} accepted (first after a rejection)"),
                    );
                }
            }
            // Losing master is not a property problem; the dump would only
            // bury the one that is.
            Err(CoreError::NotMaster) => {}
            Err(error) => {
                SAW_REJECTION.store(true, Ordering::Relaxed);
                self.dump(device, &format!("{what} rejected ({error})"));
            }
        }
    }
}

/// Whether request dumps are on: `DRM_ATOMIC_DEBUG`, or `DRM_ALLOC_DEBUG`,
/// which turns on both so the allocator's trace and the requests arrive
/// together. Read once.
static ATOMIC_DEBUG: LazyLock<bool> = LazyLock::new(|| {
    std::env::var_os("DRM_ATOMIC_DEBUG").is_some() || std::env::var_os("DRM_ALLOC_DEBUG").is_some()
});

/// Set by a rejected request, cleared by the next accepted one, so that one
/// can be dumped alongside it.
static SAW_REJECTION: AtomicBool = AtomicBool::new(false);

/// Commit `build`'s request and collect the CRTC's out-fence.
///
/// `OUT_FENCE_PTR` is unlike every other atomic property: its value is not
/// data, it is the address of a signed 32-bit slot the **kernel writes into**.
/// Getting a fence back therefore means handing the kernel a pointer that has
/// to stay put from the moment the property is added until the ioctl returns.
///
/// The closure is why this is a function rather than a property write the
/// caller makes itself. The slot lives on this frame's stack for exactly the
/// commit, `build` receives only the value to write, and the fence is taken
/// afterwards — so there is no arrangement of the safe API that lets the
/// pointer outlive what it points at, or that lets a caller commit a request
/// carrying a stale one.
///
/// Returns `None` when the commit succeeded but the kernel left the slot
/// alone: a `TEST_ONLY` never produces a fence, and neither does a driver
/// without `OUT_FENCE_PTR`.
///
/// # Errors
///
/// As [`AtomicRequest::commit`], plus whatever `build` returns.
pub fn commit_with_out_fence<F>(
    device: &Device,
    flags: AtomicCommitFlags,
    build: F,
) -> Result<Option<OwnedFd>>
where
    F: FnOnce(&mut AtomicRequest, u64) -> Result<()>,
{
    // -1 is the kernel's "nothing here" value, and stays that way if the
    // driver has no OUT_FENCE_PTR or the commit was a test.
    let mut slot: i32 = -1;
    let mut request = AtomicRequest::new();
    build(&mut request, (&raw mut slot) as u64)?;
    request.commit(device, flags)?;

    if slot < 0 {
        return Ok(None);
    }
    // SAFETY: the kernel returned a fresh sync_file descriptor in `slot` and
    // nothing else holds it, so this is the sole owner.
    Ok(Some(unsafe { OwnedFd::from_raw_fd(slot) }))
}
