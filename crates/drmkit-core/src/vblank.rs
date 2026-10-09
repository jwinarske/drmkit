// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! The CRTC's vblank counter, as `drmCrtcGetSequence` reads it.
//!
//! The `drm` crate binds `DRM_IOCTL_WAIT_VBLANK`, whose sequence is 32 bits,
//! but not `DRM_IOCTL_CRTC_GET_SEQUENCE`, which is what upstream reads. The
//! wider counter is the one to compare against: a 32-bit one wraps in a little
//! over two years at 60 Hz, and a comparison across the wrap goes backwards.

use std::os::fd::AsFd;

use drm_ffi::drm_sys::drm_crtc_get_sequence;

use crate::error::{CoreError, Result};

/// `DRM_IOCTL_CRTC_GET_SEQUENCE`, i.e. `DRM_IOWR(0x3b, struct
/// drm_crtc_get_sequence)` with `DRM_IOCTL_BASE == 'd'`.
const CRTC_GET_SEQUENCE: rustix::ioctl::Opcode =
    rustix::ioctl::opcode::read_write::<drm_crtc_get_sequence>(b'd', 0x3b);

/// The CRTC's current vblank sequence.
///
/// Port of `drmCrtcGetSequence` without the timestamp. The count moves once per
/// vblank while the CRTC is active, so a flip committed when it read `n` has
/// landed once it reads more than `n`.
///
/// # Errors
///
/// [`CoreError::Io`] if the ioctl fails: `EINVAL` for a CRTC id the device
/// does not have, `EOPNOTSUPP` on a driver without vblank support.
pub fn crtc_sequence(device: &impl AsFd, crtc_id: u32) -> Result<u64> {
    let mut data = drm_crtc_get_sequence {
        crtc_id,
        ..Default::default()
    };
    // SAFETY: CRTC_GET_SEQUENCE is DRM_IOWR(0x3b, struct drm_crtc_get_sequence)
    // and `drm_crtc_get_sequence` is the bindgen definition of exactly that
    // struct, so the opcode and the value type agree. The descriptor is
    // borrowed for the duration of the call.
    unsafe {
        rustix::ioctl::ioctl(
            device.as_fd(),
            rustix::ioctl::Updater::<CRTC_GET_SEQUENCE, drm_crtc_get_sequence>::new(&mut data),
        )
    }
    .map_err(CoreError::from_errno)?;
    Ok(data.sequence)
}
