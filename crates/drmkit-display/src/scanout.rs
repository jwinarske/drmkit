// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! Finding something to put a picture on.
//!
//! A connected connector, a mode it accepts, a CRTC its encoders can drive,
//! and that CRTC's primary plane. Every consumer otherwise walks
//! connector to encoder to CRTC to plane by hand, and the walk has two
//! non-obvious steps in it -- so it lives here once.

use drm::control::Device as _;
use drm::control::connector::State;
use drm::control::{Mode, connector, crtc};

use drmkit_core::{Device, Result};
use drmkit_fmt::FormatTable;
use drmkit_modeset::select_preferred_mode;
use drmkit_planes::{PlaneRegistry, PlaneType};

use crate::profile::errno;

/// A CRTC, and where it sits in the resource list.
///
/// Both halves are needed and they are not interchangeable: the id addresses
/// the object in an atomic request, and the index is the bit position that
/// encoder `possible_crtcs` and plane masks are expressed in. Mixing them up
/// gives an allocator that offers planes to the wrong CRTC.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CrtcChoice {
    /// The CRTC's object id.
    pub id: u32,
    /// Its index in the resource list.
    pub index: u32,
}

/// Somewhere to scan out.
#[derive(Debug, Clone)]
pub struct ScanoutTarget {
    /// The connector with a display on it.
    pub connector_id: u32,
    /// The CRTC that will drive it.
    pub crtc: CrtcChoice,
    /// That CRTC's primary plane.
    pub primary_plane_id: u32,
    /// The mode to set.
    pub mode: Mode,
    /// What the primary plane will scan out.
    ///
    /// Synthesized as `LINEAR`-only when the driver exposes no `IN_FORMATS`,
    /// so this is never empty on a plane that works -- see
    /// [`PlaneRegistry::from_capabilities`].
    pub primary_formats: FormatTable,
}

/// Why no output could be found.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum ScanoutError {
    /// Nothing is plugged in, or nothing plugged in advertises a mode.
    #[error("no connected connector with any mode")]
    NothingConnected,

    /// A display is attached but no CRTC can drive it.
    ///
    /// Every CRTC its encoders allow is already taken by another output --
    /// the ordinary case on hardware with more connectors than pipes.
    #[error("connector {connector_id} has a display but no CRTC free to drive it")]
    NoCrtc {
        /// The connector that could not be routed.
        connector_id: u32,
    },

    /// A CRTC was found but has no primary plane.
    ///
    /// Almost always means `DRM_CLIENT_CAP_UNIVERSAL_PLANES` was never set,
    /// which hides primary and cursor planes from the client.
    #[error("CRTC {crtc_id} has no primary plane: was universal planes enabled?")]
    NoPrimaryPlane {
        /// The CRTC that had none.
        crtc_id: u32,
    },
}

impl ScanoutTarget {
    /// Find the first output with a display on it.
    ///
    /// Enables universal planes, without which the kernel hides primary and
    /// cursor planes and this could only ever fail. Most callers will have
    /// set it already without meaning to: measured on amdgpu, a client with
    /// neither capability sees 2 planes and one that has gone atomic sees all
    /// 10, because the kernel turns universal planes on with atomic. It is
    /// asked for here anyway, so a caller that has not gone atomic yet gets
    /// an answer rather than a confusing `NoPrimaryPlane`.
    ///
    /// # Errors
    ///
    /// [`ScanoutError`] for each way there is nothing to drive, or
    /// [`CoreError`](drmkit_core::CoreError) if the device cannot be read.
    pub fn discover(device: &Device) -> Result<std::result::Result<Self, ScanoutError>> {
        device.enable_universal_planes()?;

        let resources = device.resource_handles().map_err(|error| errno(&error))?;

        // The first connector with a display *and* a mode. A connected
        // connector with no modes is a sink that failed to give an EDID, and
        // choosing it means a modeset with nothing to set.
        let mut connected = None;
        for handle in resources.connectors() {
            let Ok(info) = device.get_connector(*handle, true) else {
                continue;
            };
            if info.state() == State::Connected && !info.modes().is_empty() {
                connected = Some(info);
                break;
            }
        }
        let Some(connector) = connected else {
            return Ok(Err(ScanoutError::NothingConnected));
        };
        let connector_id: u32 = connector.handle().into();

        let Ok(mode) = select_preferred_mode(connector.modes()) else {
            return Ok(Err(ScanoutError::NothingConnected));
        };

        let Some(crtc) = crtc_for_connector(device, &resources, &connector) else {
            return Ok(Err(ScanoutError::NoCrtc { connector_id }));
        };

        let registry = PlaneRegistry::probe(device)?;
        let Some(primary) = primary_plane_for_crtc(&registry, crtc.index) else {
            return Ok(Err(ScanoutError::NoPrimaryPlane { crtc_id: crtc.id }));
        };

        Ok(Ok(Self {
            connector_id,
            crtc,
            primary_plane_id: primary.id,
            mode,
            primary_formats: primary.format_table.clone(),
        }))
    }
}

/// The primary plane that can feed `crtc_index`, if there is one.
///
/// Taken over a registry rather than a device so it can be reasoned about,
/// and tested, without one. `None` means either that the CRTC has no plane
/// wired to it, or -- far more often -- that the client never set
/// `DRM_CLIENT_CAP_UNIVERSAL_PLANES` and the kernel is hiding every primary.
#[must_use]
pub fn primary_plane_for_crtc(
    registry: &PlaneRegistry,
    crtc_index: u32,
) -> Option<&drmkit_planes::PlaneCapabilities> {
    registry
        .for_crtc(crtc_index)
        .find(|plane| plane.plane_type == PlaneType::Primary)
}

/// A CRTC that can drive `connector`.
///
/// Prefers the one already bound to it through its active encoder, which is
/// the warm case: the display is lit and taking over the pipe it is already
/// on avoids a mode change the user would see. Falls back to the first any of
/// its possible encoders allows, which is the cold case -- nothing bound yet,
/// and the first atomic commit wires it up.
#[must_use]
pub fn crtc_for_connector(
    device: &Device,
    resources: &drm::control::ResourceHandles,
    connector: &connector::Info,
) -> Option<CrtcChoice> {
    let index_of = |crtc: crtc::Handle| {
        resources
            .crtcs()
            .iter()
            .position(|candidate| *candidate == crtc)
            .and_then(|index| u32::try_from(index).ok())
    };

    let bound = connector
        .current_encoder()
        .and_then(|encoder| device.get_encoder(encoder).ok())
        .and_then(|encoder| encoder.crtc())
        .and_then(|crtc| Some((crtc, index_of(crtc)?)));
    if let Some((crtc, index)) = bound {
        return Some(CrtcChoice {
            id: crtc.into(),
            index,
        });
    }

    for handle in connector.encoders() {
        let Ok(encoder) = device.get_encoder(*handle) else {
            continue;
        };
        let allowed = resources.filter_crtcs(encoder.possible_crtcs());
        for crtc in allowed {
            if let Some(index) = index_of(crtc) {
                return Some(CrtcChoice {
                    id: crtc.into(),
                    index,
                });
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::primary_plane_for_crtc;
    use drmkit_planes::{PlaneCapabilities, PlaneRegistry, PlaneType};

    fn plane(id: u32, plane_type: PlaneType, crtcs: u32) -> PlaneCapabilities {
        PlaneCapabilities {
            id,
            plane_type,
            possible_crtcs: crtcs,
            formats: vec![drmkit_fmt::fourcc::ARGB8888],
            ..PlaneCapabilities::default()
        }
    }

    fn registry() -> PlaneRegistry {
        PlaneRegistry::from_capabilities(vec![
            plane(10, PlaneType::Overlay, 0b01),
            plane(11, PlaneType::Primary, 0b01),
            plane(12, PlaneType::Cursor, 0b01),
            plane(13, PlaneType::Primary, 0b10),
        ])
    }

    #[test]
    fn each_crtc_gets_its_own_primary() {
        let registry = registry();
        assert_eq!(
            primary_plane_for_crtc(&registry, 0).map(|plane| plane.id),
            Some(11)
        );
        assert_eq!(
            primary_plane_for_crtc(&registry, 1).map(|plane| plane.id),
            Some(13)
        );
    }

    #[test]
    fn an_overlay_is_never_mistaken_for_a_primary() {
        // Plane 10 comes first and can feed CRTC 0. Taking the first
        // compatible plane rather than the first *primary* would put the
        // backing store on an overlay, which on hardware that cannot reorder
        // planes puts it above the layers it belongs behind.
        let registry = registry();
        assert_ne!(
            primary_plane_for_crtc(&registry, 0).map(|plane| plane.id),
            Some(10)
        );
    }

    #[test]
    fn a_crtc_with_nothing_wired_to_it_has_no_primary() {
        assert!(primary_plane_for_crtc(&registry(), 2).is_none());
    }

    #[test]
    fn a_registry_with_no_primary_at_all_yields_none() {
        // What a client that never set universal planes sees: the kernel
        // hands out overlays only, and every CRTC looks unusable.
        let registry = PlaneRegistry::from_capabilities(vec![
            plane(10, PlaneType::Overlay, 0b11),
            plane(11, PlaneType::Overlay, 0b11),
        ]);
        assert!(primary_plane_for_crtc(&registry, 0).is_none());
        assert!(primary_plane_for_crtc(&registry, 1).is_none());
    }
}
