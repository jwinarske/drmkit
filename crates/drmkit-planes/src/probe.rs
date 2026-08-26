// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! Reading a device's planes and what each of them can do.

use drm::control::Device as _;

use drmkit_core::{Device, ObjectType, PropertyStore, Result};
use drmkit_fmt::FormatTable;

use crate::registry::{
    BlendModeValues, ColorEncodingValues, ColorPipeline, ColorRangeValues, PlaneCapabilities,
    PlaneRegistry, PlaneType,
};

/// The kernel's `DRM_PLANE_TYPE_*` values, from `drm_mode.h`.
const PLANE_TYPE_OVERLAY: u64 = 0;
const PLANE_TYPE_PRIMARY: u64 = 1;
const PLANE_TYPE_CURSOR: u64 = 2;

impl PlaneRegistry {
    /// Read every plane on the device.
    ///
    /// Needs `DRM_CLIENT_CAP_UNIVERSAL_PLANES` -- without it the kernel hides
    /// primary and cursor planes and this returns overlays only, which reads
    /// as a device with no way to put anything on screen. Call
    /// [`Device::enable_universal_planes`] first.
    ///
    /// A plane that cannot be read is skipped rather than failing the whole
    /// enumeration: one unreadable plane should not make a device unusable.
    ///
    /// # Errors
    ///
    /// [`CoreError::Io`](drmkit_core::CoreError::Io) if the plane list itself
    /// cannot be read.
    pub fn probe(device: &Device) -> Result<Self> {
        let handles = device
            .plane_handles()
            .map_err(|error| drmkit_core::CoreError::from(io_errno(&error)))?;
        // The plane's `possible_crtcs` is a mask over the CRTC *resource
        // order*, so the resource list is what turns it back into one. The
        // `drm` crate hands the mask out only as an opaque filter it will
        // apply for you, so this rebuilds the bits from what it selects.
        let resources = device
            .resource_handles()
            .map_err(|error| drmkit_core::CoreError::from(io_errno(&error)))?;

        let mut capabilities = Vec::with_capacity(handles.len());
        for handle in handles {
            let Ok(plane) = device.get_plane(handle) else {
                continue;
            };
            let id: u32 = handle.into();
            let mut properties = PropertyStore::new();
            if properties
                .cache_properties(device, id, ObjectType::Plane)
                .is_err()
            {
                continue;
            }
            capabilities.push(probe_plane(device, &resources, &properties, id, &plane));
        }
        Ok(Self::from_capabilities(capabilities))
    }
}

fn probe_plane(
    device: &Device,
    resources: &drm::control::ResourceHandles,
    properties: &PropertyStore,
    id: u32,
    plane: &drm::control::plane::Info,
) -> PlaneCapabilities {
    let value = |name: &str| properties.property_value(id, name).ok();
    let present = |name: &str| properties.property_id(id, name).is_ok();
    let enum_value =
        |name: &str, variant: &str| properties.enum_value(device, id, name, variant).ok();

    let (zpos_min, zpos_max) = match properties.range(id, "zpos") {
        // A settable range: the bounds are what the plane will take.
        Ok(Some((min, max))) => (Some(min), Some(max)),
        // Present but not an unsigned range -- immutable, or signed. Its
        // current value is the only stacking position it has, which is a
        // range of one rather than no constraint at all.
        Ok(None) => {
            let fixed = value("zpos");
            (fixed, fixed)
        }
        Err(_) => (None, None),
    };

    let format_table = in_formats(device, properties, id);
    let has_format_modifiers = !format_table.is_empty();

    PlaneCapabilities {
        id,
        possible_crtcs: crtc_mask(resources, plane.possible_crtcs()),
        plane_type: match value("type") {
            Some(PLANE_TYPE_PRIMARY) => PlaneType::Primary,
            Some(PLANE_TYPE_CURSOR) => PlaneType::Cursor,
            // Overlay is the kernel's zero, and also the safe answer for a
            // plane whose type could not be read: treating an unknown plane
            // as primary would have the scene put a backing store on it.
            Some(PLANE_TYPE_OVERLAY | _) | None => PlaneType::Overlay,
        },
        formats: plane.formats().to_vec(),
        format_table,
        has_format_modifiers,
        zpos_min,
        zpos_max,
        rotation_bits: properties.bitmask_bits(device, id, "rotation").unwrap_or(0),
        supports_rotation: present("rotation"),
        // Every atomic plane has SRC_W, and KMS exposes no capability that
        // says whether the hardware behind it can actually resize. The
        // TEST_ONLY commit is what finds out, so this starts optimistic and
        // the search corrects it. See the field's documentation.
        supports_scaling: present("SRC_W"),
        has_pixel_blend_mode: present("pixel blend mode"),
        has_fb_damage_clips: present("FB_DAMAGE_CLIPS"),
        has_per_plane_alpha: present("alpha"),
        blend_modes: BlendModeValues {
            premultiplied: enum_value("pixel blend mode", "Pre-multiplied"),
            coverage: enum_value("pixel blend mode", "Coverage"),
            none: enum_value("pixel blend mode", "None"),
        },
        has_color_encoding: present("COLOR_ENCODING"),
        color_encodings: ColorEncodingValues {
            bt601: enum_value("COLOR_ENCODING", "ITU-R BT.601 YCbCr"),
            bt709: enum_value("COLOR_ENCODING", "ITU-R BT.709 YCbCr"),
            bt2020: enum_value("COLOR_ENCODING", "ITU-R BT.2020 YCbCr"),
        },
        has_color_range: present("COLOR_RANGE"),
        color_ranges: ColorRangeValues {
            limited: enum_value("COLOR_RANGE", "YCbCr limited range"),
            full: enum_value("COLOR_RANGE", "YCbCr full range"),
        },
        color_pipeline: ColorPipeline {
            has_degamma_lut: present("PLANE_DEGAMMA_LUT"),
            has_ctm: present("PLANE_CTM"),
            has_hdr_mult: present("PLANE_HDR_MULT"),
            degamma_lut_size: value("PLANE_DEGAMMA_LUT_SIZE")
                .and_then(|size| u32::try_from(size).ok())
                .unwrap_or(0),
        },
        cursor_max_w: 0,
        cursor_max_h: 0,
        modifier_classes: Vec::new(),
    }
}

/// The plane's `IN_FORMATS` table, or an empty one.
///
/// Empty is not "no formats": it means the driver exposes no `IN_FORMATS`
/// blob, and [`PlaneRegistry::from_capabilities`] then synthesizes a
/// `LINEAR`-only table from the bare format list.
fn in_formats(device: &Device, properties: &PropertyStore, id: u32) -> FormatTable {
    let Ok(blob_id) = properties.property_value(id, "IN_FORMATS") else {
        return FormatTable::default();
    };
    device
        .property_blob(blob_id)
        .map(|blob| FormatTable::from_blob(&blob))
        .unwrap_or_default()
}

fn io_errno(error: &std::io::Error) -> rustix::io::Errno {
    rustix::io::Errno::from_io_error(error).unwrap_or(rustix::io::Errno::IO)
}

/// The plane's `possible_crtcs`, as a mask over the resource CRTC order.
fn crtc_mask(
    resources: &drm::control::ResourceHandles,
    filter: drm::control::CrtcListFilter,
) -> u32 {
    let selected = resources.filter_crtcs(filter);
    resources
        .crtcs()
        .iter()
        .enumerate()
        .filter(|(_, handle)| selected.contains(handle))
        .fold(0_u32, |mask, (index, _)| {
            // A device with more than 32 CRTCs cannot be expressed in the
            // kernel's own mask either, so anything past that is not reachable.
            mask | (1_u32
                .checked_shl(u32::try_from(index).unwrap_or(u32::MAX))
                .unwrap_or(0))
        })
}
