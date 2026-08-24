// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! What a DRM node can do, probed once.
//!
//! Consolidates the "can this device…" questions a presenter has to answer
//! before it commits anything.
//!
//! # Capabilities, never the name
//!
//! [`DriverProfile::name`] is informational. Branching on it is how a display
//! stack accumulates a table of driver names and a bug for every driver not in
//! it — including every future one. Every decision here is gated on a
//! capability the kernel reports, which is what the rest of drmkit already
//! does.

use drmkit_core::Device;

/// Whether a connected panel might hold its image without a flip.
///
/// Telemetry only, and deliberately not authoritative: PSR has no portable KMS
/// property, so the best available signal is the connector type. Nothing gates
/// behaviour on this — a frame-economy decision that skipped a commit because
/// it guessed the panel would self-refresh, and guessed wrong, freezes the
/// display.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PanelSelfRefresh {
    /// No connected connector, or the set could not be read.
    #[default]
    Unknown,
    /// Every connected connector is external, so none can self-refresh.
    None,
    /// A connected eDP or DSI panel, which *may* support it.
    Possible,
}

/// What `DRM_CAP_PRIME`'s bitmask says.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PrimeCaps {
    /// The device can import a dma-buf.
    pub can_import: bool,
    /// The device can export one.
    pub can_export: bool,
}

/// `DRM_PRIME_CAP_IMPORT`.
const PRIME_CAP_IMPORT: u64 = 0x1;
/// `DRM_PRIME_CAP_EXPORT`.
const PRIME_CAP_EXPORT: u64 = 0x2;

/// Decode the `DRM_CAP_PRIME` bitmask.
///
/// A pure helper so the bit positions can be tested without a device — the
/// kind of thing that is silently wrong on hardware nobody has.
#[must_use]
pub const fn decode_prime_caps(cap: u64) -> PrimeCaps {
    PrimeCaps {
        can_import: cap & PRIME_CAP_IMPORT != 0,
        can_export: cap & PRIME_CAP_EXPORT != 0,
    }
}

/// `DRM_MODE_CONNECTOR_eDP`.
const CONNECTOR_EDP: u32 = 14;
/// `DRM_MODE_CONNECTOR_DSI`.
const CONNECTOR_DSI: u32 = 16;

/// Whether a connector type can hold its image without a flip.
///
/// Internal panels only. Anything on a cable has to be fed.
#[must_use]
pub const fn connector_type_self_refreshes(connector_type: u32) -> bool {
    matches!(connector_type, CONNECTOR_EDP | CONNECTOR_DSI)
}

/// A device's capabilities.
///
/// One field per `DRM_CAP_*` the presenter consults, rather than a bitflags
/// type: these are independent questions with independent answers, and a
/// reader checking one against the kernel's documentation should find the
/// kernel's name for it.
#[derive(Debug, Clone, PartialEq, Eq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "the shape is the kernel's capability list, not a design choice"
)]
pub struct DriverProfile {
    /// The driver's name. Informational — see the module docs.
    pub name: String,
    /// `AddFB2` accepts explicit modifiers.
    pub addfb2_modifiers: bool,
    /// Asynchronous page flip is supported.
    pub async_page_flip: bool,
    /// Vblank timestamps are `CLOCK_MONOTONIC`.
    pub timestamp_monotonic: bool,
    /// dma-bufs can be imported.
    pub prime_import: bool,
    /// dma-bufs can be exported.
    pub prime_export: bool,
    /// Maximum cursor width. 64 where the driver reports nothing, which is the
    /// figure every KMS driver has historically accepted.
    pub cursor_width: u64,
    /// Maximum cursor height, defaulted the same way.
    pub cursor_height: u64,
    /// At least one plane advertises `FB_DAMAGE_CLIPS`.
    pub fb_damage_clips: bool,
    /// At least one CRTC exposes `VRR_ENABLED`.
    pub vrr_capable: bool,
    /// Whether a connected panel might self-refresh. Telemetry only.
    pub psr: PanelSelfRefresh,
}

impl DriverProfile {
    /// Probe `device`.
    ///
    /// Capabilities the driver does not report are taken as absent rather than
    /// as an error: `DRM_CAP_*` is additive, and a kernel too old to know a cap
    /// answers the same way as one whose hardware lacks it.
    ///
    /// # Errors
    ///
    /// [`drmkit_core::CoreError`] if the driver name or the resource list
    /// cannot be read at all — which means this is not a working KMS node.
    pub fn probe(device: &Device) -> Result<Self, drmkit_core::CoreError> {
        use drm::control::Device as _;

        let driver = drm::Device::get_driver(device).map_err(|e| errno(&e))?;
        let cap = |c: drm::DriverCapability| drm::Device::get_driver_capability(device, c).ok();

        let prime = decode_prime_caps(cap(drm::DriverCapability::Prime).unwrap_or(0));

        let resources = device.resource_handles().map_err(|e| errno(&e))?;

        // Per-object capabilities are found by enumeration, not by asking the
        // device: they are properties of planes and CRTCs, and a device may
        // have some that carry them and some that do not.
        let mut fb_damage_clips = false;
        if let Ok(planes) = device.plane_handles() {
            for handle in planes {
                if object_has_property(device, handle.into(), ObjectType::Plane, "FB_DAMAGE_CLIPS")
                {
                    fb_damage_clips = true;
                    break;
                }
            }
        }

        let mut vrr_capable = false;
        for crtc in resources.crtcs() {
            if object_has_property(device, (*crtc).into(), ObjectType::Crtc, "VRR_ENABLED") {
                vrr_capable = true;
                break;
            }
        }

        // Unknown unless a connector is actually connected: an unplugged eDP
        // says nothing about what is on screen, because nothing is.
        let mut psr = PanelSelfRefresh::Unknown;
        for handle in resources.connectors() {
            let Ok(info) = device.get_connector(*handle, false) else {
                continue;
            };
            if info.state() != drm::control::connector::State::Connected {
                continue;
            }
            let internal = connector_type_self_refreshes(connector_type_code(info.interface()));
            psr = if internal {
                PanelSelfRefresh::Possible
            } else if psr == PanelSelfRefresh::Unknown {
                PanelSelfRefresh::None
            } else {
                psr
            };
            if internal {
                break;
            }
        }

        Ok(Self {
            name: driver.name().to_string_lossy().into_owned(),
            addfb2_modifiers: cap(drm::DriverCapability::AddFB2Modifiers).unwrap_or(0) != 0,
            async_page_flip: cap(drm::DriverCapability::ASyncPageFlip).unwrap_or(0) != 0,
            timestamp_monotonic: cap(drm::DriverCapability::MonotonicTimestamp).unwrap_or(0) != 0,
            prime_import: prime.can_import,
            prime_export: prime.can_export,
            cursor_width: nonzero_or(cap(drm::DriverCapability::CursorWidth), 64),
            cursor_height: nonzero_or(cap(drm::DriverCapability::CursorHeight), 64),
            fb_damage_clips,
            vrr_capable,
            psr,
        })
    }
}

use drmkit_core::{CoreError, ObjectType, PropertyStore};

/// An io error as the crate's own error type.
///
/// `CoreError::from_io` is private to drmkit-core, and widening it just for
/// this would export an implementation detail; the errno is what carries the
/// information either way.
fn errno(error: &std::io::Error) -> CoreError {
    CoreError::Io(rustix::io::Errno::from_io_error(error).unwrap_or(rustix::io::Errno::IO))
}

/// A reported capability, or the fallback when the driver said nothing.
///
/// Zero counts as "said nothing": a driver reporting a maximum cursor of zero
/// would mean no cursor at all, which no KMS driver means.
pub(crate) const fn nonzero_or(reported: Option<u64>, fallback: u64) -> u64 {
    match reported {
        Some(v) if v != 0 => v,
        _ => fallback,
    }
}

/// Whether one KMS object carries a named property.
fn object_has_property(device: &Device, id: u32, kind: ObjectType, name: &str) -> bool {
    let mut store = PropertyStore::new();
    store.cache_properties(device, id, kind).is_ok() && store.property_id(id, name).is_ok()
}

/// The numeric connector type behind the `drm` crate's enum.
fn connector_type_code(interface: drm::control::connector::Interface) -> u32 {
    use drm::control::connector::Interface;
    match interface {
        Interface::EmbeddedDisplayPort => CONNECTOR_EDP,
        Interface::DSI => CONNECTOR_DSI,
        _ => 0,
    }
}
