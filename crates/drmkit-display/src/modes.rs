// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! What each connector is, and what modes it will take.
//!
//! Nothing here needs a rendering backend, a GBM device or DRM master, so it
//! can answer `--list-modes` before anything is brought up, and can answer it
//! again on a hotplug.

use drm::control::Device as _;
use drm::control::connector::{Interface, State};

use drmkit_core::{Device, Result};

use crate::profile::errno;
use drm::control::Mode;

/// Whether to make the kernel ask the display again.
///
/// Not a free choice. A probe is a round trip to the sink over DDC or the
/// DP aux channel: tens of milliseconds per connector, and on a sink
/// in standby it can wake the panel. The cached answer costs nothing and is
/// what the kernel last saw.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Probe {
    /// Ask the display. What a listing wants, and what a hotplug wants: the
    /// cache is exactly what has just gone stale.
    #[default]
    Force,
    /// Take the kernel's cached answer. What a loop wants, and anything
    /// running while a display might be asleep.
    Cached,
}

/// One connector and the modes it advertises.
#[derive(Debug, Clone, PartialEq)]
pub struct ConnectorModes {
    /// The connector's object id.
    pub connector_id: u32,
    /// What kind of connector it is.
    pub interface: Interface,
    /// Which one of that kind -- the `-N` in `HDMI-A-1`.
    pub interface_id: u32,
    /// Whether a display is attached.
    pub connected: bool,
    /// The modes, in the order the kernel lists them: preferred first on
    /// almost every driver, but that is a convention rather than a promise --
    /// [`select_preferred_mode`](drmkit_modeset::select_preferred_mode) reads
    /// the flag instead. [`ModeInfo`](drmkit_modeset::ModeInfo) is the
    /// extension trait that reads the rest.
    pub modes: Vec<Mode>,
}

impl ConnectorModes {
    /// The conventional name: `HDMI-A-1`, `eDP-1`, `DP-3`.
    ///
    /// The same string the kernel puts in `/sys/class/drm/card0-*` and
    /// `modetest` prints, so it can be matched against either.
    #[must_use]
    pub fn name(&self) -> String {
        format!("{}-{}", self.interface.as_str(), self.interface_id)
    }
}

/// Every connector on the device, with its modes.
///
/// In resource order, which is the order everything else on the device is
/// listed in.
///
/// A connector that disappears mid-enumeration is skipped rather than failing
/// the call: hotplug is asynchronous, and a listing that a monitor was
/// unplugged during should still describe the rest of the machine.
///
/// Writeback connectors are not here unless the caller has asked for them.
/// The kernel hides them from any client that has not set
/// `DRM_CLIENT_CAP_WRITEBACK_CONNECTORS`, and setting it is a decision about
/// the whole device rather than about one listing -- it changes what every
/// other enumeration on that descriptor returns too. Set it and they appear:
/// measured on amdgpu, `Writeback-1` shows up reporting nothing attached.
///
/// # Errors
///
/// [`CoreError::Io`] if the device's resources cannot be read at all.
pub fn query_connector_modes(device: &Device, probe: Probe) -> Result<Vec<ConnectorModes>> {
    let resources = device.resource_handles().map_err(|error| errno(&error))?;

    let force = probe == Probe::Force;
    let mut out = Vec::with_capacity(resources.connectors().len());
    for handle in resources.connectors() {
        let Ok(connector) = device.get_connector(*handle, force) else {
            continue;
        };
        out.push(ConnectorModes {
            connector_id: (*handle).into(),
            interface: connector.interface(),
            interface_id: connector.interface_id(),
            connected: connector.state() == State::Connected,
            modes: connector.modes().to_vec(),
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::ConnectorModes;
    use drm::control::connector::Interface;

    /// Every connector type as libdrm names it.
    ///
    /// Measured with `drmModeGetConnectorTypeName` on libdrm 2.4.x rather
    /// than copied from a header, because the point of the table is to agree
    /// with what the kernel and every other tool print. A name that has
    /// drifted looks right and matches nothing: no `/sys/class/drm/card0-*`
    /// directory, no line in `modetest`, no `--output` a user could pass.
    const LIBDRM: [(Interface, &str); 21] = [
        (Interface::Unknown, "Unknown"),
        (Interface::VGA, "VGA"),
        (Interface::DVII, "DVI-I"),
        (Interface::DVID, "DVI-D"),
        (Interface::DVIA, "DVI-A"),
        (Interface::Composite, "Composite"),
        (Interface::SVideo, "SVIDEO"),
        (Interface::LVDS, "LVDS"),
        (Interface::Component, "Component"),
        (Interface::NinePinDIN, "DIN"),
        (Interface::DisplayPort, "DP"),
        (Interface::HDMIA, "HDMI-A"),
        (Interface::HDMIB, "HDMI-B"),
        (Interface::TV, "TV"),
        (Interface::EmbeddedDisplayPort, "eDP"),
        (Interface::Virtual, "Virtual"),
        (Interface::DSI, "DSI"),
        (Interface::DPI, "DPI"),
        (Interface::Writeback, "Writeback"),
        (Interface::SPI, "SPI"),
        (Interface::USB, "USB"),
    ];

    fn connector(interface: Interface, interface_id: u32) -> ConnectorModes {
        ConnectorModes {
            connector_id: 1,
            interface,
            interface_id,
            connected: false,
            modes: Vec::new(),
        }
    }

    #[test]
    fn every_connector_type_is_named_the_way_libdrm_names_it() {
        for (interface, expected) in LIBDRM {
            assert_eq!(
                connector(interface, 1).name(),
                format!("{expected}-1"),
                "{interface:?}"
            );
        }
    }

    #[test]
    fn the_name_carries_the_instance() {
        // Two displays of the same kind differ only by this number, so a name
        // without it identifies both or neither.
        assert_eq!(connector(Interface::HDMIA, 1).name(), "HDMI-A-1");
        assert_eq!(connector(Interface::HDMIA, 2).name(), "HDMI-A-2");
        assert_eq!(connector(Interface::EmbeddedDisplayPort, 2).name(), "eDP-2");
    }

    #[test]
    fn a_type_nothing_knows_about_still_has_a_name() {
        // The kernel gains connector types; a listing that panicked or
        // produced an empty name on one it had not heard of would break on a
        // machine newer than the build.
        assert_eq!(
            connector(Interface::from(0xFFFF_u32), 1).name(),
            "Unknown-1"
        );
    }
}
