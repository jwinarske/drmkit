// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! Capture whatever a CRTC is scanning out and write it to a PNG.
//!
//! ```text
//! screenshot [CARD] [OUT.png]
//! ```
//!
//! Defaults to `/dev/dri/card0` and `screenshot.png`. Captures the first CRTC
//! that is driving a mode. Needs to be DRM master, which in practice means no
//! compositor is running -- or that you are the compositor.

use drm::control::Device as _;
use drmkit_capture::{snapshot, write_png};
use drmkit_core::Device;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let card = args.next().unwrap_or_else(|| "/dev/dri/card0".to_owned());
    let out = args.next().unwrap_or_else(|| "screenshot.png".to_owned());

    let device = Device::open(&card)?;
    device.enable_universal_planes()?;
    // Without this the kernel hides plane geometry and every plane looks
    // empty -- see `snapshot`.
    device.enable_atomic()?;
    device
        .set_master()
        .map_err(|e| format!("{card}: not DRM master ({e}); is a compositor running?"))?;

    let resources = device.resource_handles()?;
    let (crtc_id, mode) = resources
        .crtcs()
        .iter()
        .find_map(|handle| {
            let info = device.get_crtc(*handle).ok()?;
            info.mode().map(|mode| (u32::from(*handle), mode))
        })
        .ok_or("no CRTC is driving a mode")?;

    let (w, h) = mode.size();
    println!("capturing crtc {crtc_id} at {w}x{h}");

    let image = snapshot(&device, crtc_id)?;
    write_png(&image, &out)?;
    println!("wrote {out} ({}x{})", image.width(), image.height());
    Ok(())
}
