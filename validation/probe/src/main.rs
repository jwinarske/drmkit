// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! What a DRM device actually is, measured.
//!
//! Every hardware claim in this tree that turned out to be wrong was a claim
//! nothing measured: a comment saying vc4 carries `FB_DAMAGE_CLIPS`, a lane
//! running for months against one plane, a finding asserting a board gave a
//! threshold teeth when the number was flat. This writes down what a device
//! is, so those become data a run can diff rather than prose a reader has to
//! believe.
//!
//! The output is deterministic and free of object ids. Ids are assigned per
//! boot on some drivers, and a baseline that moved every reboot would be
//! ignored within a week. Everything is aggregated by capability instead:
//! *how many* planes carry a rotation mask, not which.

use std::collections::BTreeMap;

use drm::control::Device as _;
use drmkit_core::Device;

mod json;
use json::{Json, pretty};

fn main() -> std::process::ExitCode {
    let mut cards: Vec<String> = std::env::args().skip(1).collect();
    if cards.is_empty() {
        cards = default_cards();
    }

    let mut out = Json::object();
    out.raw("host", &host());
    let mut nodes = Json::array();
    for path in &cards {
        match probe(path) {
            Ok(node) => nodes.push(&node),
            Err(error) => {
                let mut failed = Json::object();
                failed.string("path", path);
                failed.string("error", &error);
                nodes.push(&failed.finish());
            }
        }
    }
    out.raw("cards", &nodes.finish());
    println!("{}", pretty(&out.finish()));
    std::process::ExitCode::SUCCESS
}

/// Every `/dev/dri/cardN`, in order.
fn default_cards() -> Vec<String> {
    let Ok(entries) = std::fs::read_dir("/dev/dri") else {
        return Vec::new();
    };
    let mut cards: Vec<String> = entries
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.starts_with("card"))
        .map(|name| format!("/dev/dri/{name}"))
        .collect();
    cards.sort();
    cards
}

/// Kernel release and machine, the two things that explain a capability
/// changing under you.
fn host() -> String {
    let read = |path: &str| {
        std::fs::read_to_string(path)
            .map(|value| value.trim().to_owned())
            .unwrap_or_default()
    };
    let mut host = Json::object();
    host.string("kernel", &read("/proc/sys/kernel/osrelease"));
    host.string("arch", std::env::consts::ARCH);
    host.string(
        "model",
        read("/sys/firmware/devicetree/base/model")
            .trim_end_matches('\0')
            .trim(),
    );
    host.finish()
}

fn probe(path: &str) -> Result<String, String> {
    let device = Device::open(path).map_err(|error| format!("{error}"))?;

    let mut card = Json::object();
    card.string("path", path);
    card.string("driver", &driver_name(&device));

    // Measured, not assumed: how many planes each capability combination
    // reveals. `DRM_CLIENT_CAP_ATOMIC` turns universal planes on by itself on
    // every driver tried, which means a caller that sets only atomic still
    // sees primaries -- and a test meaning to check that universal planes were
    // asked for cannot observe it.
    card.raw("plane_visibility", &plane_visibility(path));

    device
        .enable_universal_planes()
        .map_err(|error| format!("universal planes: {error}"))?;
    device
        .enable_atomic()
        .map_err(|error| format!("atomic: {error}"))?;

    card.raw("min_framebuffer", &min_framebuffer(&device));
    card.raw("planes", &planes(&device));
    card.raw("crtcs", &crtcs(&device));
    card.raw("connectors", &connectors(&device));
    Ok(card.finish())
}

fn driver_name(device: &Device) -> String {
    drm::Device::get_driver(device).map_or_else(
        |_| "unknown".to_owned(),
        |driver| driver.name().to_string_lossy().into_owned(),
    )
}

/// How many planes are visible under each client capability.
///
/// Three numbers rather than one because the difference between them is the
/// thing that surprises people.
fn plane_visibility(path: &str) -> String {
    let count = |atomic: bool, universal: bool| -> usize {
        let Ok(device) = Device::open(path) else {
            return 0;
        };
        if atomic && device.enable_atomic().is_err() {
            return 0;
        }
        if universal && device.enable_universal_planes().is_err() {
            return 0;
        }
        device.plane_handles().map(|p| p.len()).unwrap_or(0)
    };
    let mut out = Json::object();
    out.number("neither_cap", count(false, false));
    out.number("atomic_only", count(true, false));
    out.number("universal_only", count(false, true));
    out.number("both", count(true, true));
    out.finish()
}

/// The smallest framebuffer the driver will accept, per dimension.
///
/// vkms refuses anything below 10x10 and amdgpu accepts 1x1. A fixture sized
/// for one is silently skipped on the other, which is how a whole test file
/// stopped running on the CI card without anyone noticing.
fn min_framebuffer(device: &Device) -> String {
    let accepts = |width: u32, height: u32| {
        drmkit_dumb::Buffer::create(
            device,
            &drmkit_dumb::Config {
                width,
                height,
                fourcc: drmkit_fmt::fourcc::ARGB8888,
                ..drmkit_dumb::Config::default()
            },
        )
        .is_ok()
    };

    // Linear rather than binary: the range is tiny and a driver whose answer
    // is not monotonic would fool a binary search into a plausible wrong
    // number.
    let first = |vary_width: bool| {
        (1..=64).find(|n| {
            if vary_width {
                accepts(*n, 64)
            } else {
                accepts(64, *n)
            }
        })
    };

    let mut out = Json::object();
    match (first(true), first(false)) {
        (Some(w), Some(h)) => {
            out.number("min_width", w as usize);
            out.number("min_height", h as usize);
        }
        _ => out.string("error", "no size up to 64 was accepted"),
    }
    out.finish()
}

fn planes(device: &Device) -> String {
    let registry = match drmkit_planes::PlaneRegistry::probe(device) {
        Ok(registry) => registry,
        Err(error) => {
            let mut out = Json::object();
            out.string("error", &format!("{error}"));
            return out.finish();
        }
    };
    let all = registry.all();

    let count_of =
        |f: &dyn Fn(&drmkit_planes::PlaneCapabilities) -> bool| all.iter().filter(|p| f(p)).count();

    let mut out = Json::object();
    out.number("total", all.len());
    out.number(
        "primary",
        count_of(&|p| p.plane_type == drmkit_planes::PlaneType::Primary),
    );
    out.number(
        "cursor",
        count_of(&|p| p.plane_type == drmkit_planes::PlaneType::Cursor),
    );
    out.number(
        "overlay",
        count_of(&|p| p.plane_type == drmkit_planes::PlaneType::Overlay),
    );

    // The capabilities a test might gate on. Counts rather than booleans: a
    // driver where only some planes carry a property is the interesting case,
    // and amdgpu is one -- six of ten for damage clips.
    out.number("with_damage_clips", count_of(&|p| p.has_fb_damage_clips));
    out.number("with_blend_mode", count_of(&|p| p.has_pixel_blend_mode));
    out.number("with_alpha", count_of(&|p| p.has_per_plane_alpha));
    out.number("with_color_encoding", count_of(&|p| p.has_color_encoding));
    out.number("with_in_formats", count_of(&|p| p.has_format_modifiers));
    out.number("with_zpos", count_of(&|p| p.zpos_min.is_some()));
    out.number("with_rotation", count_of(&|p| p.supports_rotation));

    // Rotation masks histogram. vkms reports 0x3f, amdgpu 0xf and vc4 0x35 --
    // the last of which does 0 and 180 and both reflects but *not* 90 or 270,
    // which is the case the allocator gates on bits rather than on the
    // property's presence.
    let mut masks: BTreeMap<u64, usize> = BTreeMap::new();
    for plane in all {
        *masks.entry(plane.rotation_bits).or_default() += 1;
    }
    let mut histogram = Json::object();
    for (mask, count) in masks {
        histogram.number(&format!("{mask:#x}"), count);
    }
    out.raw("rotation_masks", &histogram.finish());

    // zpos ranges, likewise: a primary pinned at [0,0] beside overlays at
    // [1,17] is what makes a plane pool heterogeneous.
    let mut ranges: BTreeMap<String, usize> = BTreeMap::new();
    for plane in all {
        let key = match (plane.zpos_min, plane.zpos_max) {
            (Some(min), Some(max)) => format!("[{min},{max}]"),
            _ => "none".to_owned(),
        };
        *ranges.entry(key).or_default() += 1;
    }
    let mut zpos = Json::object();
    for (range, count) in ranges {
        zpos.number(&range, count);
    }
    out.raw("zpos_ranges", &zpos.finish());
    out.finish()
}

fn crtcs(device: &Device) -> String {
    let Ok(resources) = device.resource_handles() else {
        let mut out = Json::object();
        out.string("error", "resources unreadable");
        return out.finish();
    };

    let mut total = 0;
    let mut pipelines: BTreeMap<String, usize> = BTreeMap::new();
    let mut gamma_sizes: BTreeMap<u32, usize> = BTreeMap::new();
    for handle in resources.crtcs() {
        total += 1;
        let id: u32 = (*handle).into();
        let Ok(capabilities) = drmkit_display::CrtcCapabilities::probe(device, id) else {
            continue;
        };
        let shape = format!(
            "degamma={} ctm={} gamma={}",
            capabilities.has_degamma_lut, capabilities.has_ctm, capabilities.has_gamma_lut
        );
        *pipelines.entry(shape).or_default() += 1;
        if capabilities.has_gamma_lut {
            *gamma_sizes.entry(capabilities.gamma_lut_size).or_default() += 1;
        }
    }

    let mut out = Json::object();
    out.number("total", total);
    let mut shapes = Json::object();
    for (shape, count) in pipelines {
        shapes.number(&shape, count);
    }
    out.raw("color_pipelines", &shapes.finish());
    let mut sizes = Json::object();
    for (size, count) in gamma_sizes {
        sizes.number(&size.to_string(), count);
    }
    out.raw("gamma_lut_sizes", &sizes.finish());
    out.finish()
}

fn connectors(device: &Device) -> String {
    let Ok(listed) = drmkit_display::query_connector_modes(device, drmkit_display::Probe::Cached)
    else {
        let mut out = Json::object();
        out.string("error", "connectors unreadable");
        return out.finish();
    };

    let mut array = Json::array();
    for connector in &listed {
        let capabilities =
            drmkit_display::ConnectorCapabilities::probe(device, connector.connector_id).ok();

        let mut entry = Json::object();
        // The name, not the id: `HDMI-A-1` is stable across boots and matches
        // sysfs, and the id is not.
        entry.string("name", &connector.name());
        entry.bool("connected", connector.connected);
        entry.number("modes", connector.modes.len());
        if let Some(caps) = capabilities {
            entry.number(
                "colorspace_entries",
                drmkit_display::Colorspace::ALL
                    .into_iter()
                    .filter(|entry| caps.colorspace(*entry).is_some())
                    .count(),
            );
            entry.bool("hdr_output_metadata", caps.has_hdr_output_metadata);
            entry.bool("can_signal_hdr", caps.can_signal_hdr());
            match caps.max_bpc {
                Some(bpc) => entry.string("max_bpc", &format!("[{},{}]", bpc.min, bpc.max)),
                None => entry.string("max_bpc", "none"),
            }
        }
        array.push(&entry.finish());
    }
    array.finish()
}
