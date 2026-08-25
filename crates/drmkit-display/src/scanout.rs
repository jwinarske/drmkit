// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! Finding something to put a picture on.
//!
//! A connected connector, a mode it accepts, a CRTC its encoders can drive,
//! and that CRTC's primary plane. Every consumer otherwise walks
//! connector to encoder to CRTC to plane by hand, and the walk has two
//! non-obvious steps in it -- so it lives here once.

use drm::control::Device as _;
use drm::control::connector::{Interface, State};
use drm::control::{Mode, connector, crtc};

use drmkit_core::{Device, Result};
use drmkit_fmt::FormatTable;
use drmkit_modeset::select_preferred_mode;
use drmkit_planes::{PlaneRegistry, PlaneType};

use crate::profile::errno;

/// Which display to prefer when more than one is attached.
///
/// "The first connected connector" is wrong on every multi-output machine: a
/// docked laptop with eDP and HDMI both live, or a board with DSI and HDMI,
/// lands on whichever the kernel enumerated first. These are the policies
/// worth having a name for, and [`discover_ranked`](ScanoutTarget::discover_ranked)
/// takes any order a caller composes.
pub mod rank {
    use drm::control::connector::Interface;

    /// Internal panels first, then cable-out, with VGA last.
    ///
    /// The right default for "give me the display the user means".
    pub const MAIN: &[Interface] = &[
        Interface::EmbeddedDisplayPort,
        Interface::LVDS,
        Interface::DSI,
        Interface::DPI,
        Interface::SPI,
        Interface::HDMIA,
        Interface::HDMIB,
        Interface::DisplayPort,
        Interface::DVID,
        Interface::DVII,
        Interface::DVIA,
        Interface::VGA,
        // Last, and present -- which the reference's equivalent is not. A
        // virtual display is a real output; it is only never the one you
        // would prefer if a physical panel were also attached. Leaving it out
        // means finding no display on vkms and in every VM guest, since
        // virtio-gpu enumerates as Virtual too. See
        // [drm-cxx#253](https://github.com/jwinarske/drm-cxx/issues/253).
        Interface::Virtual,
    ];

    /// Internal panels only.
    ///
    /// For something that must not grab an external monitor even when one is
    /// plugged in -- an instrument cluster on a docked dev board.
    pub const INTERNAL: &[Interface] = &[
        Interface::EmbeddedDisplayPort,
        Interface::LVDS,
        Interface::DSI,
        Interface::DPI,
        Interface::SPI,
    ];

    /// Cable-out connectors only, and the virtual one.
    ///
    /// For signage that should land on the external display even when an
    /// embedded panel is present. `Virtual` is here so the same policy works
    /// in a VM, where there is nothing else.
    pub const EXTERNAL: &[Interface] = &[
        Interface::HDMIA,
        Interface::HDMIB,
        Interface::DisplayPort,
        Interface::DVID,
        Interface::DVII,
        Interface::DVIA,
        Interface::VGA,
        Interface::Virtual,
    ];
}

/// The best of `candidates` by `ranks`, as an index.
///
/// Walks the ranks in order and returns the first candidate matching each.
/// A candidate whose type is not in `ranks` is skipped, and `None` means none
/// matched -- which is a real answer for [`rank::INTERNAL`] on a machine with
/// only external outputs.
///
/// Stable: two displays of the same kind resolve to the earlier candidate, so
/// the kernel's enumeration order is the tiebreak and the answer does not
/// move between runs.
#[must_use]
pub fn rank_pick(candidates: &[Interface], ranks: &[Interface]) -> Option<usize> {
    ranks
        .iter()
        .find_map(|wanted| candidates.iter().position(|kind| kind == wanted))
}

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
        Self::discover_ranked(device, rank::MAIN)
    }

    /// As [`ScanoutTarget::discover`], preferring displays in `ranks` order.
    ///
    /// The rank is a preference, not a filter. When nothing connected
    /// matches it, the first eligible connector is used anyway -- it is a
    /// display, and refusing to drive it leaves a machine looking headless
    /// with a picture on it. So [`rank::INTERNAL`] on a machine with only
    /// external outputs still returns one of them.
    ///
    /// A caller that wants the strict reading -- an instrument cluster that
    /// must show nothing rather than appear on the docking monitor -- ranks
    /// the connectors itself with [`rank_pick`], which returns `None` when
    /// none match.
    ///
    /// # Errors
    ///
    /// As [`ScanoutTarget::discover`].
    pub fn discover_ranked(
        device: &Device,
        ranks: &[Interface],
    ) -> Result<std::result::Result<Self, ScanoutError>> {
        device.enable_universal_planes()?;

        let resources = device.resource_handles().map_err(|error| errno(&error))?;

        // Connected *and* advertising a mode. A connected connector with no
        // modes is a sink that failed to give an EDID, and choosing it means
        // a modeset with nothing to set.
        let mut eligible = Vec::new();
        for handle in resources.connectors() {
            let Ok(info) = device.get_connector(*handle, true) else {
                continue;
            };
            if info.state() == State::Connected && !info.modes().is_empty() {
                eligible.push(info);
            }
        }
        if eligible.is_empty() {
            return Ok(Err(ScanoutError::NothingConnected));
        }

        let kinds: Vec<Interface> = eligible.iter().map(connector::Info::interface).collect();
        // Falling back to the first eligible one, which is what the kernel
        // enumerated first -- the behaviour a caller would have got with no
        // ranking at all.
        let chosen = rank_pick(&kinds, ranks).unwrap_or(0);
        let connector = &eligible[chosen];
        let connector_id: u32 = connector.handle().into();

        let Ok(mode) = select_preferred_mode(connector.modes()) else {
            return Ok(Err(ScanoutError::NothingConnected));
        };

        let Some(crtc) = crtc_for_connector(device, &resources, connector) else {
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
    use super::{rank, rank_pick};
    use drm::control::connector::Interface;
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

    /// The docked-laptop case: HDMI enumerated first, eDP second.
    #[test]
    fn an_internal_panel_wins_under_the_main_rank() {
        let candidates = [Interface::HDMIA, Interface::EmbeddedDisplayPort];
        assert_eq!(rank_pick(&candidates, rank::MAIN), Some(1));
    }

    #[test]
    fn cable_out_is_taken_when_there_is_no_panel() {
        let candidates = [Interface::VGA, Interface::HDMIA];
        assert_eq!(rank_pick(&candidates, rank::MAIN), Some(1), "HDMI over VGA");
    }

    #[test]
    fn the_internal_rank_ignores_external_connectors() {
        let candidates = [Interface::HDMIA, Interface::DSI, Interface::DisplayPort];
        assert_eq!(rank_pick(&candidates, rank::INTERNAL), Some(1));
    }

    #[test]
    fn the_internal_rank_finds_nothing_when_only_cables_are_attached() {
        // A real answer, not a failure: an instrument cluster that must not
        // grab the external monitor would rather have nothing.
        let candidates = [Interface::HDMIA, Interface::DisplayPort];
        assert_eq!(rank_pick(&candidates, rank::INTERNAL), None);
    }

    #[test]
    fn the_external_rank_ignores_embedded_panels() {
        let candidates = [Interface::EmbeddedDisplayPort, Interface::HDMIA];
        assert_eq!(rank_pick(&candidates, rank::EXTERNAL), Some(1));
    }

    #[test]
    fn a_tie_goes_to_the_earlier_connector() {
        // Two HDMI outputs. The kernel's enumeration order is the tiebreak,
        // so the same machine picks the same display every boot.
        let candidates = [Interface::HDMIA, Interface::HDMIA];
        assert_eq!(rank_pick(&candidates, rank::MAIN), Some(0));
    }

    #[test]
    fn nothing_to_choose_from_chooses_nothing() {
        assert_eq!(rank_pick(&[], rank::MAIN), None);
        assert_eq!(rank_pick(&[Interface::HDMIA], &[]), None);
    }

    #[test]
    fn a_connector_kind_the_rank_does_not_name_is_skipped() {
        let candidates = [Interface::Composite, Interface::TV];
        assert_eq!(rank_pick(&candidates, rank::MAIN), None);
    }

    #[test]
    fn a_caller_can_impose_its_own_order() {
        let candidates = [Interface::HDMIA, Interface::EmbeddedDisplayPort];
        let ranks = [Interface::HDMIA, Interface::EmbeddedDisplayPort];
        assert_eq!(rank_pick(&candidates, &ranks), Some(0));
    }

    #[test]
    fn a_virtual_display_is_still_a_display() {
        // vkms exposes exactly one connector and it is Virtual, as does
        // virtio-gpu in a VM. The reference's rank arrays name none of the
        // three, so every one of its examples reports no display on both --
        // see drm-cxx#253.
        assert_eq!(rank_pick(&[Interface::Virtual], rank::MAIN), Some(0));
        assert_eq!(rank_pick(&[Interface::Virtual], rank::EXTERNAL), Some(0));
    }

    #[test]
    fn a_real_panel_still_outranks_a_virtual_one() {
        // Virtual sits last, so a VM that also has a physical passthrough
        // panel does not land on the virtual one.
        let candidates = [Interface::Virtual, Interface::EmbeddedDisplayPort];
        assert_eq!(rank_pick(&candidates, rank::MAIN), Some(1));
    }
}
