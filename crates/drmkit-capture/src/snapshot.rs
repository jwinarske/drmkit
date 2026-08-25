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
/// Planes are composited bottom-up by `zpos`, `SRC_OVER`, exactly as the
/// display controller would.
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
/// - **Anything but `ARGB8888` and `XRGB8888`.** YUV in particular: a video
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
/// - [`CaptureError::Io`] if the kernel refuses to enumerate resources.
pub fn snapshot(device: &Device, crtc_id: u32) -> Result<Image, CaptureError> {
    let crtc = device
        .get_crtc(drm::control::crtc::Handle::from(
            core::num::NonZeroU32::new(crtc_id).ok_or(CaptureError::NoMode)?,
        ))
        .map_err(|e| CaptureError::Io(std::io::Error::other(e.to_string())))?;

    let Some(mode) = crtc.mode() else {
        drmkit_log::log_warn!("capture: crtc {crtc_id} is not driving a mode; nothing to snapshot");
        return Err(CaptureError::NoMode);
    };
    let (out_w, out_h) = (u32::from(mode.size().0), u32::from(mode.size().1));

    let mut frames = collect_planes(device, crtc_id)?;
    if frames.is_empty() {
        drmkit_log::log_warn!(
            "capture: crtc {crtc_id} has no readable plane -- if the atomic client cap is not \
             enabled, the kernel hides the geometry properties and every plane looks empty"
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

/// Map every readable plane bound to `crtc_id`.
fn collect_planes(device: &Device, crtc_id: u32) -> Result<Vec<PlaneFrame>, CaptureError> {
    let handles = device
        .plane_handles()
        .map_err(|e| CaptureError::Io(std::io::Error::other(e.to_string())))?;

    let mut frames = Vec::new();
    for handle in handles {
        let plane_id: u32 = handle.into();
        let Ok(info) = device.get_plane(handle) else {
            continue;
        };
        if info.crtc().map(u32::from) != Some(crtc_id) {
            continue;
        }
        let Some(fb) = info.framebuffer() else {
            continue;
        };
        match map_plane(device, plane_id, fb) {
            Ok(frame) => frames.push(frame),
            Err(reason) => {
                drmkit_log::log_warn!("capture: skipping plane {plane_id} -- {reason}");
            }
        }
    }
    Ok(frames)
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

    let len = pitch as usize * height as usize;
    let mapping = map_buffer(device, buffer, len)?;

    Ok(PlaneFrame {
        plane_id,
        width,
        height,
        pitch,
        format,
        crtc_x,
        crtc_y,
        crtc_w,
        crtc_h,
        zpos,
        mapping,
    })
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
    let dmabuf = device
        .buffer_to_prime_fd(buffer, 0)
        .map_err(|e| format!("dma-buf export failed: {e}"))?;

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
            bytes: frame.mapping.bytes(),
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
/// Scaling is nearest-neighbour. The C++ reference resamples through `Blend2D`,
/// so a *scaled* plane will not match it pixel for pixel; an unscaled one --
/// which is what a screenshot of an ordinary desktop is made of -- matches
/// exactly. Pinning a readback path to one particular resampler is not worth
/// the coupling, and upstream is not expected to keep that resampler.
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

    let out_w = image.width();
    let out_h = image.height();

    for dy in 0..dst_h {
        // Signed, because a plane may start above the top edge.
        let Some(y) = src.crtc_y.checked_add_unsigned(dy) else {
            continue;
        };
        if y < 0 || y.cast_unsigned() >= out_h {
            continue;
        }
        // `dy < dst_h`, so the quotient is strictly below `height` and fits;
        // the fallback clamps to the last row rather than wrapping to the
        // first, which would be a visible tear rather than a missing one.
        let src_y = u32::try_from(u64::from(dy) * u64::from(src.height) / u64::from(dst_h))
            .unwrap_or(src.height.saturating_sub(1));
        let row = src_y as usize * src.pitch as usize;

        for dx in 0..dst_w {
            let Some(x) = src.crtc_x.checked_add_unsigned(dx) else {
                continue;
            };
            if x < 0 || x.cast_unsigned() >= out_w {
                continue;
            }
            let src_x = u32::try_from(u64::from(dx) * u64::from(src.width) / u64::from(dst_w))
                .unwrap_or(src.width.saturating_sub(1));
            let bpp = src.format.bytes();
            let at = row + src_x as usize * bpp;
            let Some(chunk) = src.bytes.get(at..at + bpp) else {
                // A framebuffer whose stride and height disagree with its
                // mapping is a driver bug, not a reason to abandon the frame.
                continue;
            };
            let pixel = src.format.read(chunk);
            let index = y.cast_unsigned() as usize * out_w as usize + x.cast_unsigned() as usize;
            let pixels = image.pixels_mut();
            pixels[index] = src_over(pixels[index], pixel);
        }
    }
}
