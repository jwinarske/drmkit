// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! The dumb-buffer handle.
//!
//! Port of `src/dumb/buffer.{hpp,cpp}`.

use std::os::fd::{AsFd as _, BorrowedFd, RawFd};
use std::ptr::NonNull;

use drm::buffer::DrmFourcc;
use drm::control::{Device as ControlDevice, FbCmd2Flags, framebuffer};
use drmkit_core::Device;
use drmkit_fmt::fourcc;

use crate::error::{DumbError, Result};
use crate::mapping::{MapAccess, Mapping};

/// Past every shipping camera and display maximum. Rejecting beyond it keeps
/// the row and byte-count computations from wrapping on hostile or quirky
/// dimensions fed in from a driver-controlled V4L2 format echo.
const MAX_IMAGE_DIM: u32 = 16384;

/// What the kernel reported for a `CREATE_DUMB` allocation.
///
/// Kept rather than using the `drm` crate's `DumbBuffer`, which stores the
/// kernel's `size` and `pitch` in private fields with no accessors — and whose
/// mapping helper re-`mmap`s per call, which invariant 6 forbids. `drm-ffi` is
/// still the Smithay binding (DR-2); this reaches one layer down, not around.
#[derive(Debug, Clone, Copy)]
struct Allocation {
    handle: u32,
    /// The kernel's own byte count, which may exceed `pitch * height` when the
    /// driver pads. Mapping less than this would under-map the object.
    size: u64,
}

/// A `mmap`'d region owned for a buffer's lifetime.
///
/// Held rather than re-established per frame — see [`Mapping`] and invariant 6.
#[derive(Debug)]
struct Mmap {
    ptr: NonNull<u8>,
    len: usize,
}

// SAFETY: the region is owned exclusively by the `Buffer` holding it, and the
// kernel's dumb-buffer mapping is coherent across threads. Nothing here is
// thread-affine.
unsafe impl Send for Mmap {}
// SAFETY: as above; `&Mmap` hands out no interior mutability of its own.
unsafe impl Sync for Mmap {}

impl Mmap {
    fn as_slice(&self) -> &[u8] {
        // SAFETY: `ptr` is a live mapping of `len` bytes established in
        // `map_region` and unmapped only in `Drop`.
        unsafe { std::slice::from_raw_parts(self.ptr.as_ptr(), self.len) }
    }

    fn as_mut_slice(&mut self) -> &mut [u8] {
        // SAFETY: as above; `&mut self` guarantees exclusive access.
        unsafe { std::slice::from_raw_parts_mut(self.ptr.as_ptr(), self.len) }
    }
}

impl Drop for Mmap {
    fn drop(&mut self) {
        // SAFETY: unmapping exactly the region established in `map_region`.
        // munmap is descriptor-independent, so this stays correct even after
        // the originating DRM descriptor has been closed -- which is what
        // makes `Buffer::forget` able to tear the mapping down.
        unsafe {
            let _ = rustix::mm::munmap(self.ptr.as_ptr().cast(), self.len);
        }
    }
}

/// How a dumb buffer should be allocated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Config {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// DRM `FourCC`.
    pub fourcc: u32,
    /// Bits per pixel the kernel uses to size the allocation. Must match the
    /// format.
    pub bpp: u32,
    /// Whether to register a KMS framebuffer after allocating.
    ///
    /// Set false for the legacy `drmModeSetCursor` path, which consumes the
    /// raw GEM handle and needs no framebuffer id.
    pub add_fb: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            width: 0,
            height: 0,
            fourcc: 0,
            bpp: 32,
            add_fb: true,
        }
    }
}

/// Geometry of a recognized semi-planar layout.
struct PlanarGeometry {
    /// Bits per sample the kernel uses to size the pitch.
    bpp: u32,
    /// Extra rows for the chroma plane, as a fraction of the image height.
    extra_rows_num: u64,
    extra_rows_den: u64,
}

/// The semi-planar 4:2:0 formats [`Buffer::create_planar`] handles.
const fn planar_geometry(fourcc_code: u32) -> Option<PlanarGeometry> {
    match fourcc_code {
        // 8-bit semi-planar 4:2:0. A Y row and a UV row are both `stride`
        // bytes wide (Cb/Cr interleaved at half horizontal resolution cover
        // the same byte count); the UV plane is height/2 rows.
        fourcc::NV12 | fourcc::NV21 => Some(PlanarGeometry {
            bpp: 8,
            extra_rows_num: 1,
            extra_rows_den: 2,
        }),
        // 16-bit semi-planar 4:2:0. Same row arrangement, u16 samples. The
        // bpp difference is what sizes the pitch; the plane ratio is the same.
        fourcc::P010 | fourcc::P012 | fourcc::P016 => Some(PlanarGeometry {
            bpp: 16,
            extra_rows_num: 1,
            extra_rows_den: 2,
        }),
        _ => None,
    }
}

/// An owned dumb buffer, its optional KMS framebuffer, and its CPU mapping.
///
/// Dumb buffers are KMS's GPU-free allocation path: any driver implementing
/// `DRM_CAP_DUMB_BUFFER` — every modern KMS driver — can allocate a
/// CPU-mappable linear pixel buffer with no accelerator involved. They are the
/// right tool for cursors, client-side decorations, software-rendered UI, and
/// test harnesses.
///
/// # Deviation: the mapping is established once, not per `map()`
///
/// The `drm` crate's `map_dumb_buffer` performs an `mmap` on every call and its
/// guard `munmap`s on drop. That is the opposite of what **invariant 6**
/// requires — one mapping held for the buffer's lifetime, and a free `map()`
/// per frame. So the allocation and destruction ioctls come from the `drm`
/// crate (DR-2), while the mapping is established once here at creation and
/// held. See [`Mapping`] for why the cost contract matters.
#[derive(Debug)]
pub struct Buffer {
    /// `None` once [`Buffer::forget`] has run or the buffer is empty.
    fd: Option<RawFd>,
    alloc: Option<Allocation>,
    fb: Option<framebuffer::Handle>,
    mapping: Option<Mmap>,
    width: u32,
    height: u32,
    stride: u32,
}

impl Default for Buffer {
    fn default() -> Self {
        Self::new()
    }
}

impl Buffer {
    /// An empty buffer holding no resources.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            fd: None,
            alloc: None,
            fb: None,
            mapping: None,
            width: 0,
            height: 0,
            stride: 0,
        }
    }

    /// Allocate a single-plane dumb buffer.
    ///
    /// # Errors
    ///
    /// - [`DumbError::InvalidConfig`] for zero dimensions, a zero format, a
    ///   zero bpp, or dimensions past 16384.
    /// - [`DumbError::Io`] if an ioctl or the `mmap` fails.
    pub fn create(device: &Device, config: &Config) -> Result<Self> {
        if config.width == 0 || config.height == 0 {
            return Err(DumbError::InvalidConfig {
                reason: "width and height must be non-zero",
            });
        }
        if config.fourcc == 0 {
            return Err(DumbError::InvalidConfig {
                reason: "format must be non-zero",
            });
        }
        if config.bpp == 0 {
            return Err(DumbError::InvalidConfig {
                reason: "bits per pixel must be non-zero",
            });
        }
        if config.width > MAX_IMAGE_DIM || config.height > MAX_IMAGE_DIM {
            return Err(DumbError::InvalidConfig {
                reason: "dimensions exceed 16384",
            });
        }

        let fourcc_typed =
            DrmFourcc::try_from(config.fourcc).map_err(|_| DumbError::InvalidConfig {
                reason: "format is not a recognized DRM FourCC",
            })?;

        let created = drm_ffi::mode::dumbbuffer::create(
            device.as_fd(),
            config.width,
            config.height,
            config.bpp,
            0,
        )
        .map_err(|e| io_to_errno(&e))?;

        // Anything failing past this point must release the GEM allocation, or
        // the buffer leaks until the descriptor closes.
        let mut buffer = Self {
            fd: Some(device.raw_fd()),
            width: config.width,
            height: config.height,
            stride: created.pitch,
            alloc: Some(Allocation {
                handle: created.handle,
                size: created.size,
            }),
            fb: None,
            mapping: None,
        };

        buffer.mapping = Some(buffer.map_region(device)?);

        if config.add_fb {
            let planes = PlaneLayout::single(&buffer, fourcc_typed);
            let fb = device
                .add_planar_framebuffer(&planes, FbCmd2Flags::empty())
                .map_err(|e| io_to_errno(&e))?;
            buffer.fb = Some(fb);
        }

        Ok(buffer)
    }

    /// Allocate a dumb buffer for a recognized semi-planar YUV format —
    /// `NV12`, `NV21`, `P010`, `P012`, or `P016`.
    ///
    /// The kernel allocates one linear region; the framebuffer points the Y and
    /// interleaved-UV planes at distinct byte offsets within it. Pass the
    /// **image** dimensions: the over-allocation for the half-height chroma
    /// plane and the 8- versus 16-bit sample width are derived internally.
    ///
    /// `height` should be even. The chroma half-height uses integer division,
    /// so an odd height rounds down.
    ///
    /// # Errors
    ///
    /// - [`DumbError::UnsupportedFormat`] for a format outside the recognized
    ///   set — fall back to [`Buffer::create`] for single-plane formats.
    /// - [`DumbError::InvalidConfig`] for zero or out-of-range dimensions.
    /// - [`DumbError::TooLarge`] if the row count or chroma offset would not
    ///   fit the kernel's 32-bit fields.
    /// - [`DumbError::Io`] if an ioctl or the `mmap` fails.
    pub fn create_planar(
        device: &Device,
        fourcc_code: u32,
        width: u32,
        height: u32,
    ) -> Result<Self> {
        if width == 0 || height == 0 || width > MAX_IMAGE_DIM || height > MAX_IMAGE_DIM {
            return Err(DumbError::InvalidConfig {
                reason: "dimensions must be non-zero and at most 16384",
            });
        }
        let geometry = planar_geometry(fourcc_code).ok_or(DumbError::UnsupportedFormat {
            fourcc: fourcc_code,
        })?;
        let fourcc_typed =
            DrmFourcc::try_from(fourcc_code).map_err(|_| DumbError::UnsupportedFormat {
                fourcc: fourcc_code,
            })?;

        // Over-allocate so the chroma plane fits the same linear region. The
        // kernel sees one tall buffer; the framebuffer points the chroma plane
        // at the second part via a per-plane offset. All of this runs in 64-bit
        // so a hostile geometry cannot wrap, and the kernel's height field is
        // 32-bit, so a result that would truncate is rejected.
        let extra_rows = u64::from(height)
            .checked_mul(geometry.extra_rows_num)
            .ok_or(DumbError::TooLarge {
                what: "chroma row count",
            })?
            / geometry.extra_rows_den;
        let total_rows = u64::from(height)
            .checked_add(extra_rows)
            .and_then(|rows| u32::try_from(rows).ok())
            .ok_or(DumbError::TooLarge {
                what: "total row count",
            })?;

        let created =
            drm_ffi::mode::dumbbuffer::create(device.as_fd(), width, total_rows, geometry.bpp, 0)
                .map_err(|e| io_to_errno(&e))?;

        let stride = created.pitch;
        let mut buffer = Self {
            fd: Some(device.raw_fd()),
            // The image dimensions, not the over-allocated row count: a
            // consumer reading `height()` sees the visible image, matching how
            // the framebuffer is framed.
            width,
            height,
            stride,
            alloc: Some(Allocation {
                handle: created.handle,
                size: created.size,
            }),
            fb: None,
            mapping: None,
        };

        buffer.mapping = Some(buffer.map_region(device)?);

        // The chroma offset is the Y plane's footprint, computed against the
        // kernel-reported stride so any alignment padding the driver added is
        // honored -- amdgpu DC's 256-byte rule lands here, and using the
        // kernel stride means not having to know about it.
        let y_size = u64::from(stride) * u64::from(height);
        let chroma_offset = u32::try_from(y_size).map_err(|_| DumbError::TooLarge {
            what: "chroma plane offset",
        })?;

        let planes = PlaneLayout::semi_planar(&buffer, fourcc_typed, chroma_offset);
        let fb = device
            .add_planar_framebuffer(&planes, FbCmd2Flags::empty())
            .map_err(|e| io_to_errno(&e))?;
        buffer.fb = Some(fb);

        Ok(buffer)
    }

    /// Establish the lifetime mapping for the freshly allocated GEM object.
    fn map_region(&self, device: &Device) -> Result<Mmap> {
        let alloc = self.alloc.ok_or(DumbError::InvalidConfig {
            reason: "buffer has no allocation to map",
        })?;
        let len = usize::try_from(alloc.size).map_err(|_| DumbError::TooLarge {
            what: "allocation size does not fit a usize",
        })?;

        let offset = drm_ffi::mode::dumbbuffer::map(device.as_fd(), alloc.handle, 0, 0)
            .map_err(|e| io_to_errno(&e))?
            .offset;

        // SAFETY: `offset` is the mmap offset the kernel just reported for this
        // GEM handle, `len` is the size the kernel reported at creation, and
        // the descriptor is the one the handle belongs to. The returned region
        // is owned solely by this `Mmap` and unmapped exactly once, in `Drop`.
        let ptr = unsafe {
            rustix::mm::mmap(
                std::ptr::null_mut(),
                len,
                rustix::mm::ProtFlags::READ | rustix::mm::ProtFlags::WRITE,
                rustix::mm::MapFlags::SHARED,
                device.as_fd(),
                offset,
            )
        }?;

        let ptr = NonNull::new(ptr.cast::<u8>()).ok_or(DumbError::Io(rustix::io::Errno::NOMEM))?;
        Ok(Mmap { ptr, len })
    }

    /// Whether the buffer holds no allocation.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.alloc.is_none()
    }

    /// Width in pixels.
    #[must_use]
    pub const fn width(&self) -> u32 {
        self.width
    }

    /// Height in pixels — the image height, not the over-allocated row count of
    /// a planar buffer.
    #[must_use]
    pub const fn height(&self) -> u32 {
        self.height
    }

    /// Bytes per row, as the kernel reported it.
    #[must_use]
    pub const fn stride(&self) -> u32 {
        self.stride
    }

    /// Total mapped bytes.
    #[must_use]
    pub fn size_bytes(&self) -> usize {
        self.mapping.as_ref().map_or(0, |m| m.len)
    }

    /// The GEM handle on the originating descriptor.
    ///
    /// Legacy cursor paths consume this directly; atomic paths use
    /// [`fb_id`](Self::fb_id).
    #[must_use]
    pub fn gem_handle(&self) -> Option<u32> {
        self.alloc.map(|a| a.handle)
    }

    /// The KMS framebuffer id, if one was registered.
    #[must_use]
    pub fn fb_id(&self) -> Option<u32> {
        self.fb.map(Into::into)
    }

    /// The pixel storage, read only.
    #[must_use]
    pub fn data(&self) -> &[u8] {
        self.mapping.as_ref().map_or(&[], Mmap::as_slice)
    }

    /// The pixel storage, mutable.
    #[must_use]
    pub fn data_mut(&mut self) -> &mut [u8] {
        self.mapping.as_mut().map_or(&mut [], Mmap::as_mut_slice)
    }

    /// Acquire a scoped CPU view.
    ///
    /// Free in both directions — see [`Mapping`] and invariant 6. The access
    /// mode is recorded on the view but ignored here; it exists so the same
    /// scoped painting code works against dumb and GBM buffers.
    ///
    /// Yields an empty view when the buffer is empty.
    pub fn map(&mut self, access: MapAccess) -> Mapping<'_> {
        let (stride, width, height) = (self.stride, self.width, self.height);
        let pixels = self.data_mut();
        Mapping::new(pixels, stride, width, height, access)
    }

    /// Abandon the GEM handle and framebuffer id **without issuing ioctls**.
    ///
    /// For when the originating descriptor is known to be dead — libseat's
    /// session-resume replaced the device. The CPU mapping is still torn down,
    /// because `munmap` is descriptor-independent, so the caller is left with a
    /// valid empty buffer. The kernel reclaims the orphaned handles when the
    /// descriptor closes.
    pub fn forget(&mut self) {
        self.mapping = None; // munmap runs; it needs no descriptor
        self.alloc = None;
        self.fb = None;
        self.fd = None;
        self.width = 0;
        self.height = 0;
        self.stride = 0;
    }
}

impl Drop for Buffer {
    fn drop(&mut self) {
        let (Some(fd), Some(alloc)) = (self.fd, self.alloc.take()) else {
            // Already forgotten, or never allocated. The mapping still drops.
            return;
        };

        // SAFETY: `fd` is the descriptor this buffer allocated against. The
        // borrow lives only for these two ioctls, and `forget` is the
        // documented escape hatch for when the descriptor is known dead.
        let borrowed = unsafe { BorrowedFd::borrow_raw(fd) };
        let device = RawDevice { fd: borrowed };

        if let Some(fb) = self.fb.take() {
            // Order matters: the framebuffer references the GEM object, so it
            // goes first. Errors are unactionable in a destructor.
            let _ = device.destroy_framebuffer(fb);
        }

        // Unmap before releasing the GEM object.
        self.mapping = None;

        let _ = drm_ffi::mode::dumbbuffer::destroy(borrowed, alloc.handle);
    }
}

/// Minimal device adapter for the two teardown ioctls, so `Drop` need not hold
/// a `&Device` it cannot borrow.
struct RawDevice<'a> {
    fd: BorrowedFd<'a>,
}

impl std::os::fd::AsFd for RawDevice<'_> {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.fd
    }
}

impl drm::Device for RawDevice<'_> {}
impl drm::control::Device for RawDevice<'_> {}

/// Plane layout handed to `add_planar_framebuffer`.
struct PlaneLayout {
    width: u32,
    height: u32,
    fourcc: DrmFourcc,
    handles: [Option<drm::buffer::Handle>; 4],
    pitches: [u32; 4],
    offsets: [u32; 4],
}

impl PlaneLayout {
    fn single(buffer: &Buffer, fourcc: DrmFourcc) -> Self {
        let handle = buffer.alloc.and_then(|a| drm::control::from_u32(a.handle));
        Self {
            width: buffer.width,
            height: buffer.height,
            // Already validated by the caller. Taking the typed value rather
            // than re-converting removes the only place a bad FourCC could
            // have silently become a different format on the framebuffer.
            fourcc,
            handles: [handle, None, None, None],
            pitches: [buffer.stride, 0, 0, 0],
            offsets: [0; 4],
        }
    }

    /// Both planes name the same GEM object; the chroma plane is distinguished
    /// only by its byte offset.
    fn semi_planar(buffer: &Buffer, fourcc: DrmFourcc, chroma_offset: u32) -> Self {
        let handle = buffer.alloc.and_then(|a| drm::control::from_u32(a.handle));
        Self {
            width: buffer.width,
            height: buffer.height,
            fourcc,
            handles: [handle, handle, None, None],
            // A Y row and a UV row are both `stride` bytes wide for semi-planar
            // 4:2:0.
            pitches: [buffer.stride, buffer.stride, 0, 0],
            offsets: [0, chroma_offset, 0, 0],
        }
    }
}

impl drm::buffer::PlanarBuffer for PlaneLayout {
    fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    fn format(&self) -> DrmFourcc {
        self.fourcc
    }

    fn modifier(&self) -> Option<drm::buffer::DrmModifier> {
        // Dumb buffers are linear by construction, and passing no modifier is
        // what keeps `add_planar_framebuffer` from setting the MODIFIERS flag.
        None
    }

    fn pitches(&self) -> [u32; 4] {
        self.pitches
    }

    fn handles(&self) -> [Option<drm::buffer::Handle>; 4] {
        self.handles
    }

    fn offsets(&self) -> [u32; 4] {
        self.offsets
    }
}

/// The `drm` crate returns `io::Result`; drmkit classifies on `Errno`.
fn io_to_errno(error: &std::io::Error) -> DumbError {
    rustix::io::Errno::from_io_error(error)
        .map_or(DumbError::Io(rustix::io::Errno::IO), DumbError::Io)
}
