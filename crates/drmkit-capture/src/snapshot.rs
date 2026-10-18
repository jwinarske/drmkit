// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! Read back what a CRTC is scanning out.

use std::num::NonZeroUsize;

use drm::control::Device as _;
use drmkit_core::{Device, ObjectType, PropertyStore};
use drmkit_fmt::fourcc;

use crate::CaptureError;
use crate::image::{Image, src_over};

/// A scanout format this can read back.
///
/// Deliberately short. Everything here is packed, linear and host-readable,
/// which is the entire class of buffer a CPU readback can honestly claim to
/// support -- see [`snapshot`] for what is skipped and why.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SourceFormat {
    /// 32-bit, premultiplied alpha.
    Argb8888,
    /// 32-bit, undefined top byte, treated as opaque.
    Xrgb8888,
    /// 16-bit 5:6:5, opaque. What a Linux console is on much embedded
    /// hardware, including a Raspberry Pi's.
    Rgb565,
}

impl SourceFormat {
    /// Bytes per pixel.
    const fn bytes(self) -> usize {
        match self {
            Self::Argb8888 | Self::Xrgb8888 => 4,
            Self::Rgb565 => 2,
        }
    }

    /// Read one pixel as premultiplied `0xAARRGGBB`.
    ///
    /// 5- and 6-bit channels are widened by replicating their high bits into
    /// the low ones, so full-scale stays full-scale: 0b11111 becomes 0xFF and
    /// not 0xF8. Shifting alone would darken every bright pixel by up to 3%,
    /// which is invisible in isolation and obvious in a gradient.
    fn read(self, chunk: &[u8]) -> u32 {
        match self {
            Self::Argb8888 => u32::from_ne_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]),
            // The top byte means nothing, so state that the plane is opaque:
            // a buffer that happens to hold zero there would otherwise vanish.
            Self::Xrgb8888 => {
                u32::from_ne_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]) | 0xff00_0000
            }
            Self::Rgb565 => {
                let v = u32::from(u16::from_ne_bytes([chunk[0], chunk[1]]));
                let r = (v >> 11) & 0x1f;
                let g = (v >> 5) & 0x3f;
                let b = v & 0x1f;
                let r = (r << 3) | (r >> 2);
                let g = (g << 2) | (g >> 4);
                let b = (b << 3) | (b >> 2);
                0xff00_0000 | (r << 16) | (g << 8) | b
            }
        }
    }
}

/// One plane's pixels and where they land, with no ownership of either.
///
/// The blit is separated from the mapping deliberately: everything that can be
/// got wrong about placement -- scaling, clipping, a plane hanging off the top
/// edge, `XRGB8888`'s undefined alpha byte -- is arithmetic, and arithmetic
/// that only runs behind an `mmap` of a real scanout buffer is arithmetic that
/// only gets tested on hardware that happens to be plugged in.
pub(crate) struct Blit<'a> {
    /// The framebuffer's bytes, `pitch` per row.
    pub bytes: &'a [u8],
    pub width: u32,
    pub height: u32,
    pub pitch: u32,
    /// How to read a pixel out of `bytes`.
    pub format: SourceFormat,
    /// Where it lands. `x` and `y` are signed: a plane may hang off an edge.
    pub crtc_x: i32,
    pub crtc_y: i32,
    /// Zero means "as large as the framebuffer", as the kernel reads it.
    pub crtc_w: u32,
    pub crtc_h: u32,
}

/// A plane's scanout buffer, mapped and ready to composite.
struct PlaneFrame {
    plane_id: u32,
    /// Framebuffer dimensions, which need not match where it lands.
    width: u32,
    height: u32,
    /// Framebuffer stride in bytes.
    pitch: u32,
    /// Where the image starts in the mapping: the framebuffer's
    /// `offsets[0]`, which need not be zero.
    offset: usize,
    format: SourceFormat,
    /// Where the plane sits on the CRTC. `x` and `y` are signed: a plane may
    /// hang off the top or left edge.
    crtc_x: i32,
    crtc_y: i32,
    crtc_w: u32,
    crtc_h: u32,
    zpos: i64,
    mapping: Mapping,
}

/// A read-only mapping of a scanout buffer, unmapped on drop.
struct Mapping {
    ptr: *mut core::ffi::c_void,
    len: usize,
}

impl Mapping {
    /// The mapped bytes.
    fn bytes(&self) -> &[u8] {
        // SAFETY: `ptr` and `len` come from a successful `mmap` that has not
        // been unmapped -- `Mapping` owns the mapping for its whole lifetime
        // and only `Drop` releases it. The mapping is `PROT_READ` of a
        // dma-buf, and the slice is handed out immutably, so nothing here
        // writes through it. The producer may write the buffer concurrently,
        // which makes the *contents* a torn frame rather than the *access*
        // unsound: every byte in range stays readable either way.
        unsafe { core::slice::from_raw_parts(self.ptr.cast::<u8>(), self.len) }
    }
}

impl Drop for Mapping {
    fn drop(&mut self) {
        // SAFETY: as `bytes` -- a live mapping of exactly this address and
        // length, released exactly once because `Mapping` is not `Copy` and
        // this is its only unmap.
        unsafe {
            let _ = rustix::mm::munmap(self.ptr, self.len);
        }
    }
}

/// Read back the current plane composition of `crtc_id` into an image sized to
/// its active mode.
///
/// Planes are composited bottom-up by `zpos`, `SRC_OVER`, as the display
/// controller would for planes that are neither cropped nor scaled: the whole
/// framebuffer is sampled, ignoring `SRC_*`, and a plane whose `CRTC_W`/`CRTC_H`
/// differ from its framebuffer is scaled bilinearly, bit for bit as the
/// reference's `Blend2D` does it -- which need not be what the display
/// controller's own scaler does.
///
/// **The atomic client cap must be enabled on `device` first.** Plane
/// placement is read from `CRTC_X`/`CRTC_Y`/`CRTC_W`/`CRTC_H`, which the
/// kernel hides from clients that have not set `DRM_CLIENT_CAP_ATOMIC` -- so
/// without it every plane is skipped for want of geometry and the call fails
/// as though the CRTC were blank. That is a property of the kernel's property
/// filtering, not of this function, and it is the same filtering that makes
/// `modetest -p` under-report `IN_FENCE_FD`.
///
/// What it will not read, skipping the plane with a warning rather than
/// failing the capture:
///
/// - **Non-linear modifiers** -- tiled, AFBC, DCC. Untiling them on the CPU is
///   a per-vendor job and reading them correctly is not guessable.
/// - **Anything but `ARGB8888`, `XRGB8888` and `RGB565`.** YUV in particular: a video
///   overlay is usually both tiled and YUV, so covering it means GPU-assisted
///   readback.
/// - **Buffers that cannot be mapped**, which includes VRAM with no
///   host-visible mapping.
///
/// # Errors
///
/// - [`CaptureError::NoMode`] if the CRTC is not driving a mode -- there is no
///   size to capture at.
/// - [`CaptureError::NothingReadable`] if no plane on the CRTC could be read.
///   This is the "you forgot the atomic cap" symptom as well as the genuinely
///   blank one; the warnings say which.
/// - [`CaptureError::Io`] if the CRTC -- an unknown `crtc_id` included -- or
///   the plane list cannot be queried.
pub fn snapshot(device: &Device, crtc_id: u32) -> Result<Image, CaptureError> {
    // Id 0 names no object, and the kernel says so with ENOENT, as the
    // reference reports it; the errno is kept rather than turned into text.
    let handle = core::num::NonZeroU32::new(crtc_id).ok_or_else(|| {
        CaptureError::Io(std::io::Error::from_raw_os_error(
            rustix::io::Errno::NOENT.raw_os_error(),
        ))
    })?;
    let crtc = device
        .get_crtc(drm::control::crtc::Handle::from(handle))
        .map_err(CaptureError::Io)?;

    let Some(mode) = crtc.mode() else {
        drmkit_log::log_warn!("capture: crtc {crtc_id} is not driving a mode; nothing to snapshot");
        return Err(CaptureError::NoMode);
    };
    let (out_w, out_h) = (u32::from(mode.size().0), u32::from(mode.size().1));

    let Collected {
        mut frames,
        enumerated,
        unbound,
    } = collect_planes(device, crtc_id)?;
    if frames.is_empty() {
        // The counts are what tell the causes apart, as in the reference: no
        // planes enumerated at all is a client without universal planes, and
        // every plane unbound is a CRTC that is really blank.
        drmkit_log::log_warn!(
            "capture: crtc {crtc_id} has {enumerated} enumerated planes, {unbound} unbound to \
             this crtc, none readable -- snapshot aborted (plane geometry needs \
             DRM_CLIENT_CAP_ATOMIC on the fd)"
        );
        return Err(CaptureError::NothingReadable);
    }

    // Bottom-up, and stably: two planes at the same zpos keep the order the
    // kernel enumerated them in, which is the same tie-break the allocator
    // uses.
    frames.sort_by_key(|frame| frame.zpos);

    let mut image = Image::new(out_w, out_h);
    for frame in &frames {
        compose(&mut image, frame);
    }
    Ok(image)
}

/// What [`collect_planes`] found.
struct Collected {
    /// Every readable plane bound to the CRTC.
    frames: Vec<PlaneFrame>,
    /// Planes the kernel listed to this client.
    enumerated: usize,
    /// Of those, the ones on another CRTC or showing nothing.
    unbound: usize,
}

/// Map every readable plane bound to `crtc_id`.
fn collect_planes(device: &Device, crtc_id: u32) -> Result<Collected, CaptureError> {
    let handles = device.plane_handles().map_err(CaptureError::Io)?;

    let enumerated = handles.len();
    let mut unbound = 0;
    let mut frames = Vec::new();
    for handle in handles {
        let plane_id: u32 = handle.into();
        let Ok(info) = device.get_plane(handle) else {
            continue;
        };
        let (Some(fb), true) = (
            info.framebuffer(),
            info.crtc().map(u32::from) == Some(crtc_id),
        ) else {
            unbound += 1;
            continue;
        };
        match map_plane(device, plane_id, fb) {
            Ok(frame) => frames.push(frame),
            Err(reason) => {
                drmkit_log::log_warn!("capture: skipping plane {plane_id} -- {reason}");
            }
        }
    }
    Ok(Collected {
        frames,
        enumerated,
        unbound,
    })
}

/// Everything that can make one plane unreadable, as a message rather than an
/// error: a plane this cannot read is skipped, and the capture continues.
type Skipped = String;

/// Read one plane's framebuffer and geometry, and map its pixels.
fn map_plane(
    device: &Device,
    plane_id: u32,
    fb: drm::control::framebuffer::Handle,
) -> Result<PlaneFrame, Skipped> {
    let fb = device
        .get_planar_framebuffer(fb)
        .map_err(|e| format!("framebuffer query failed: {e}"))?;
    // GETFB2 opened a GEM handle on this descriptor for each buffer object,
    // and each one holds the object alive until it is closed. The mapping
    // below keeps its own reference through the dma-buf, so they close on
    // every way out of here.
    let _handles = FramebufferHandles::new(device, &fb.buffers());

    let modifier = fb.modifier().map_or(0, u64::from);
    // `None` is DRM_FORMAT_MOD_INVALID, which means "unstated", and an
    // unstated modifier on a scanout buffer is linear in practice.
    let linear = fb.modifier().is_none() || modifier == 0;
    if !linear {
        return Err(format!("non-linear modifier {modifier:#018x}"));
    }

    let format = match fb.pixel_format() as u32 {
        fourcc::ARGB8888 => SourceFormat::Argb8888,
        fourcc::XRGB8888 => SourceFormat::Xrgb8888,
        fourcc::RGB565 => SourceFormat::Rgb565,
        other => {
            return Err(format!(
                "unsupported format {}",
                drmkit_fmt::format_name(other)
            ));
        }
    };

    let (width, height) = fb.size();
    let pitch = fb.pitches()[0];
    let Some(buffer) = fb.buffers().first().copied().flatten() else {
        return Err("framebuffer reports no buffer object".to_owned());
    };

    let mut store = PropertyStore::new();
    store
        .cache_properties(device, plane_id, ObjectType::Plane)
        .map_err(|e| format!("property cache failed: {e}"))?;

    let geometry = |name: &str| {
        store
            .property_value(plane_id, name)
            .map_err(|_| format!("{name} unavailable -- is the atomic client cap enabled?"))
    };
    // CRTC_X and CRTC_Y are signed int32 carried in a u64 slot, so they come
    // back sign-extended as a huge number rather than a negative one. Two
    // casts, through the width they were written at.
    #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
    let crtc_x = geometry("CRTC_X")? as u32 as i32;
    #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
    let crtc_y = geometry("CRTC_Y")? as u32 as i32;
    #[allow(clippy::cast_possible_truncation)]
    let crtc_w = geometry("CRTC_W")? as u32;
    #[allow(clippy::cast_possible_truncation)]
    let crtc_h = geometry("CRTC_H")? as u32;
    #[allow(clippy::cast_possible_wrap)]
    let zpos = store
        .property_value(plane_id, "zpos")
        .map_or(0, |z| z as i64);

    // The image starts at the framebuffer's offset into the buffer object,
    // so the mapping runs from the object's start to the image's end.
    let offset = fb.offsets()[0] as usize;
    let len = offset + pitch as usize * height as usize;
    let mapping = map_buffer(device, buffer, len)?;

    Ok(PlaneFrame {
        plane_id,
        width,
        height,
        pitch,
        offset,
        format,
        crtc_x,
        crtc_y,
        crtc_w,
        crtc_h,
        zpos,
        mapping,
    })
}

/// The GEM handles a framebuffer query opened, closed on drop.
///
/// Closed once each: planes of one object share its handle.
struct FramebufferHandles<'a> {
    device: &'a Device,
    handles: Vec<drm::buffer::Handle>,
}

impl<'a> FramebufferHandles<'a> {
    fn new(device: &'a Device, buffers: &[Option<drm::buffer::Handle>]) -> Self {
        let mut handles: Vec<_> = buffers.iter().flatten().copied().collect();
        handles.sort_unstable_by_key(|h| u32::from(*h));
        handles.dedup();
        Self { device, handles }
    }
}

impl Drop for FramebufferHandles<'_> {
    fn drop(&mut self) {
        for handle in self.handles.drain(..) {
            let _ = self.device.close_buffer(handle);
        }
    }
}

/// Export a GEM handle as a dma-buf, close-on-exec as the reference exports
/// it, so a child another thread spawns mid-capture does not inherit the
/// descriptor and keep the scanout buffer alive.
pub(crate) fn export(
    device: &Device,
    buffer: drm::buffer::Handle,
) -> Result<std::os::fd::OwnedFd, Skipped> {
    device
        .buffer_to_prime_fd(buffer, rustix::fs::OFlags::CLOEXEC.bits())
        .map_err(|e| format!("dma-buf export failed: {e}"))
}

/// Export a GEM handle as a dma-buf and map it read-only.
fn map_buffer(
    device: &Device,
    buffer: drm::buffer::Handle,
    len: usize,
) -> Result<Mapping, Skipped> {
    let Some(len) = NonZeroUsize::new(len) else {
        return Err("framebuffer has zero size".to_owned());
    };
    let dmabuf = export(device, buffer)?;

    // SAFETY: `mmap` with a null hint asks the kernel to choose the address,
    // which is the only requirement the call has of the caller. The length is
    // the framebuffer's own pitch times height, so the range is backed. The
    // returned pointer is handed straight to `Mapping`, which owns it and is
    // the only thing that unmaps it.
    let ptr = unsafe {
        rustix::mm::mmap(
            core::ptr::null_mut(),
            len.get(),
            rustix::mm::ProtFlags::READ,
            rustix::mm::MapFlags::SHARED,
            &dmabuf,
            0,
        )
    }
    .map_err(|e| format!("dma-buf mmap failed: {e}"))?;

    Ok(Mapping {
        ptr,
        len: len.get(),
    })
}

/// Blend one plane into the output.
fn compose(image: &mut Image, frame: &PlaneFrame) {
    blit(
        image,
        &Blit {
            bytes: frame
                .mapping
                .bytes()
                .get(frame.offset..)
                .unwrap_or_default(),
            width: frame.width,
            height: frame.height,
            pitch: frame.pitch,
            format: frame.format,
            crtc_x: frame.crtc_x,
            crtc_y: frame.crtc_y,
            crtc_w: frame.crtc_w,
            crtc_h: frame.crtc_h,
        },
    );
    drmkit_log::log_debug!(
        "capture: composed plane {} ({}x{} -> {}x{} at {},{}) zpos {}",
        frame.plane_id,
        frame.width,
        frame.height,
        frame.crtc_w,
        frame.crtc_h,
        frame.crtc_x,
        frame.crtc_y,
        frame.zpos
    );
}

/// Blend one plane's pixels into the output.
///
/// An unscaled plane -- which is what a screenshot of an ordinary desktop is
/// made of -- is copied pixel for pixel. A scaled one is resampled the way the
/// C++ reference's `Blend2D` `blit_image` does it, bilinear filtering included,
/// so that the two agree to the bit; see [`Axis`] for what that takes.
///
/// With one deliberate exception: a scaled `XRGB8888` plane is opaque here,
/// as an unscaled one is in both. `Blend2D`'s JIT bilinear fetch skips the
/// alpha fill its other `XRGB32` fetches do, so the reference interpolates the
/// undefined top byte into its output alpha (drm-cxx#376); its own portable
/// pipeline does not. Only the output's alpha differs, planes stacked above included: no
/// color channel of `SRC_OVER` reads the destination's alpha.
///
/// Anything landing outside the output is dropped rather than wrapped, which
/// matters because a plane may legitimately start at a negative coordinate.
pub(crate) fn blit(image: &mut Image, src: &Blit<'_>) {
    // A zero destination size means "as large as the framebuffer", which is
    // what the kernel does with an unset CRTC_W/CRTC_H.
    let dst_w = if src.crtc_w == 0 {
        src.width
    } else {
        src.crtc_w
    };
    let dst_h = if src.crtc_h == 0 {
        src.height
    } else {
        src.crtc_h
    };
    if dst_w == 0 || dst_h == 0 || src.width == 0 || src.height == 0 {
        return;
    }

    if dst_w == src.width && dst_h == src.height {
        blit_unscaled(image, src);
    } else {
        blit_scaled(image, src, dst_w, dst_h);
    }
}

/// The destination coordinates `start..start + len` that land on an output
/// `size` across, each paired with its offset from `start`.
///
/// `start` is signed, because a plane may begin above or left of the output.
fn visible(start: i32, len: u32, size: u32) -> impl Iterator<Item = (u32, u32)> {
    (0..len).filter_map(move |offset| {
        let at = u32::try_from(start.checked_add_unsigned(offset)?).ok()?;
        (at < size).then_some((offset, at))
    })
}

/// A 1:1 plane: every framebuffer pixel lands on exactly one output pixel.
fn blit_unscaled(image: &mut Image, src: &Blit<'_>) {
    let out_w = image.width();
    let out_h = image.height();
    let bpp = src.format.bytes();

    for (src_y, y) in visible(src.crtc_y, src.height, out_h) {
        let row = src_y as usize * src.pitch as usize;
        for (src_x, x) in visible(src.crtc_x, src.width, out_w) {
            let at = row + src_x as usize * bpp;
            let Some(chunk) = src.bytes.get(at..at + bpp) else {
                // A framebuffer whose stride and height disagree with its
                // mapping is a driver bug, not a reason to abandon the frame.
                continue;
            };
            let pixel = src.format.read(chunk);
            let index = y as usize * out_w as usize + x as usize;
            let pixels = image.pixels_mut();
            pixels[index] = src_over(pixels[index], pixel);
        }
    }
}

/// One axis of `Blend2D`'s bilinear affine pattern fetch, which is what its
/// `blit_image` into a rectangle of a different size runs.
///
/// Matching it to the bit means matching its arithmetic rather than bilinear
/// filtering in general:
///
/// - The inverse transform is computed in `f64` the way `Blend2D` inverts the
///   scale matrix -- through the determinant, so the reciprocal scale is
///   `sy / (sx * sy)` and not `1 / sx` -- then floored into 32.32 fixed point.
/// - Pixel centers sit at `.5` on both sides: the start is
///   `-origin / scale + 0.5 / scale - 0.5`, and each output pixel steps
///   `1 / scale` from there, in fixed point, from output coordinate zero.
/// - The weight is the top eight bits of the fraction: the far tap gets `w`
///   of 256 and the near one `256 - w`.
/// - Past the source edge, `blit_image` reflects rather than pads. Bilinear
///   only ever overreaches by one tap, where the two agree, but the index
///   arithmetic is kept as `Blend2D` has it so that they agree everywhere.
struct Axis {
    /// The source position of output coordinate zero, 32.32.
    start: i64,
    /// The source distance between adjacent output pixels, 32.32.
    step: i64,
    /// The source length, in pixels.
    len: u32,
}

/// One output coordinate's two source taps and the far tap's weight.
#[derive(Clone, Copy)]
struct Taps {
    near: usize,
    far: usize,
    weight: u32,
}

impl Axis {
    /// `inverse` is the reciprocal scale as `Blend2D` computes it.
    fn new(len: u32, inverse: f64, origin: i32) -> Self {
        const ONE: f64 = 4_294_967_296.0;
        // The reflect period is twice the length, and `%` is C's `fmod`.
        let period = f64::from(len) * 2.0;
        let translate = -(f64::from(origin) * inverse);
        let start = ((translate + 0.5 * inverse) - 0.5) % period;
        // Saturating, where `Blend2D` would zero the transform instead; both
        // take a plane millions of pixels across to reach.
        #[allow(clippy::cast_possible_truncation)]
        Self {
            start: (start * ONE).floor() as i64,
            step: (inverse * ONE).floor() as i64,
            len,
        }
    }

    /// The taps for output coordinate `at`.
    fn taps(&self, at: u32) -> Taps {
        let position = i128::from(self.start) + i128::from(at) * i128::from(self.step);
        let len = i128::from(self.len);
        // Into [-len, len), where a negative index reflects to `-index - 1`.
        let mut whole = (position >> 32).rem_euclid(len * 2);
        if whole >= len {
            whole -= len * 2;
        }
        // In [0, len) either way, and `len` came from a `u32`.
        let reflect = |index: i128| usize::try_from(if index < 0 { -index - 1 } else { index });
        Taps {
            near: reflect(whole).unwrap_or(0),
            far: reflect((whole + 1).min(len - 1)).unwrap_or(0),
            // Masked to eight bits, so neither truncates nor loses a sign.
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            weight: ((position >> 24) & 0xff) as u32,
        }
    }
}

/// Mix two premultiplied pixels channel by channel, `far` weighted `weight`
/// of 256, truncating: `Blend2D`'s `(a * (256 - w) + b * w) >> 8`.
const fn lerp(near: u32, far: u32, weight: u32) -> u32 {
    let mut out = 0;
    let mut shift = 0;
    while shift < 32 {
        let a = (near >> shift) & 0xff;
        let b = (far >> shift) & 0xff;
        out |= ((a * (256 - weight) + b * weight) >> 8) << shift;
        shift += 8;
    }
    out
}

/// A plane whose destination size differs from its framebuffer's.
fn blit_scaled(image: &mut Image, src: &Blit<'_>, dst_w: u32, dst_h: u32) {
    let out_w = image.width();
    let out_h = image.height();
    let bpp = src.format.bytes();

    let scale_x = f64::from(dst_w) / f64::from(src.width);
    let scale_y = f64::from(dst_h) / f64::from(src.height);
    let det = scale_x * scale_y;
    let x_axis = Axis::new(src.width, scale_y / det, src.crtc_x);
    let y_axis = Axis::new(src.height, scale_x / det, src.crtc_y);

    // The taps depend only on the output column, so work them out once rather
    // than once per row.
    let columns: Vec<(u32, Taps)> = visible(src.crtc_x, dst_w, out_w)
        .map(|(_, x)| (x, x_axis.taps(x)))
        .collect();

    // A framebuffer whose stride and height disagree with its mapping is a
    // driver bug, not a reason to abandon the frame: a pixel with a tap past
    // the end is skipped.
    let read = |row: usize, column: usize| {
        let at = row * src.pitch as usize + column * bpp;
        src.bytes
            .get(at..at + bpp)
            .map(|chunk| src.format.read(chunk))
    };

    for (_, y) in visible(src.crtc_y, dst_h, out_h) {
        let rows = y_axis.taps(y);
        for &(x, cols) in &columns {
            let (Some(top_near), Some(top_far), Some(bottom_near), Some(bottom_far)) = (
                read(rows.near, cols.near),
                read(rows.near, cols.far),
                read(rows.far, cols.near),
                read(rows.far, cols.far),
            ) else {
                continue;
            };
            // Down each column first, then across: the order `Blend2D`
            // truncates in.
            let pixel = lerp(
                lerp(top_near, bottom_near, rows.weight),
                lerp(top_far, bottom_far, rows.weight),
                cols.weight,
            );
            let index = y as usize * out_w as usize + x as usize;
            let pixels = image.pixels_mut();
            pixels[index] = src_over(pixels[index], pixel);
        }
    }
}
