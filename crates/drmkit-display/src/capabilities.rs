// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! What a connector and a CRTC can be asked to do.
//!
//! Both are property caches, and both exist for the same reason: the integer
//! a driver assigns to a named enum entry is the driver's business, so the
//! only way to write `Colorspace = BT2020_RGB` is to have asked that
//! connector what it calls it. Doing that per frame is an ioctl per property;
//! doing it once is this.

use drmkit_core::{Device, Result};
use drmkit_core::{ObjectType, PropertyStore};

/// A connector's `Colorspace` entries, by the kernel's names.
///
/// What the sink is told the pixels mean. Writing the wrong one is not an
/// error the kernel can catch -- the picture just comes out in the wrong
/// gamut.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub enum Colorspace {
    /// `Default`
    Default,
    /// `SMPTE_170M_YCC`
    Smpte170mYcc,
    /// `BT709_YCC`
    Bt709Ycc,
    /// `XVYCC_601`
    XvYcc601,
    /// `XVYCC_709`
    XvYcc709,
    /// `SYCC_601`
    SYcc601,
    /// `opYCC_601`
    OpYcc601,
    /// `opRGB`
    OpRgb,
    /// `BT2020_CYCC`
    Bt2020Cycc,
    /// `BT2020_RGB`
    Bt2020Rgb,
    /// `BT2020_YCC`
    Bt2020Ycc,
    /// `DCI-P3_RGB_D65`
    DciP3RgbD65,
    /// `DCI-P3_RGB_Theater`
    DciP3RgbTheater,
    /// `RGB_Wide_Gamut_Fixed_Point`
    RgbWideFixed,
    /// `RGB_Wide_Gamut_Floating_Point`
    RgbWideFloat,
}

impl Colorspace {
    /// Every variant, in the order the cache stores them.
    pub const ALL: [Self; 15] = [
        Self::Default,
        Self::Smpte170mYcc,
        Self::Bt709Ycc,
        Self::XvYcc601,
        Self::XvYcc709,
        Self::SYcc601,
        Self::OpYcc601,
        Self::OpRgb,
        Self::Bt2020Cycc,
        Self::Bt2020Rgb,
        Self::Bt2020Ycc,
        Self::DciP3RgbD65,
        Self::DciP3RgbTheater,
        Self::RgbWideFixed,
        Self::RgbWideFloat,
    ];

    /// The name the kernel gives this entry.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Default => "Default",
            Self::Smpte170mYcc => "SMPTE_170M_YCC",
            Self::Bt709Ycc => "BT709_YCC",
            Self::XvYcc601 => "XVYCC_601",
            Self::XvYcc709 => "XVYCC_709",
            Self::SYcc601 => "SYCC_601",
            Self::OpYcc601 => "opYCC_601",
            Self::OpRgb => "opRGB",
            Self::Bt2020Cycc => "BT2020_CYCC",
            Self::Bt2020Rgb => "BT2020_RGB",
            Self::Bt2020Ycc => "BT2020_YCC",
            Self::DciP3RgbD65 => "DCI-P3_RGB_D65",
            Self::DciP3RgbTheater => "DCI-P3_RGB_Theater",
            Self::RgbWideFixed => "RGB_Wide_Gamut_Fixed_Point",
            Self::RgbWideFloat => "RGB_Wide_Gamut_Floating_Point",
        }
    }

    const fn index(self) -> usize {
        self as usize
    }
}

/// A connector's `Broadcast RGB` entries.
///
/// The RGB quantization range, which is a separate question from the YCbCr
/// `COLOR_RANGE` on a plane. Getting it wrong is the classic washed-out or
/// crushed-black HDMI picture.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub enum BroadcastRgb {
    /// `Automatic`
    Automatic,
    /// `Full`
    Full,
    /// `Limited 16:235`
    Limited,
}

impl BroadcastRgb {
    /// Every variant, in the order the cache stores them.
    pub const ALL: [Self; 3] = [Self::Automatic, Self::Full, Self::Limited];

    /// The name the kernel gives this entry.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Automatic => "Automatic",
            Self::Full => "Full",
            Self::Limited => "Limited 16:235",
        }
    }

    const fn index(self) -> usize {
        self as usize
    }
}

/// The `max bpc` range, and where it currently sits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MaxBpc {
    /// The lowest the driver will negotiate. Usually 6 or 8.
    pub min: u64,
    /// The highest. Usually 8, 10, 12 or 16.
    pub max: u64,
    /// What the property reads now.
    pub current: u64,
}

/// What one connector's colour properties can be set to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectorCapabilities {
    /// The connector this describes.
    pub connector_id: u32,
    /// The driver's integer for each `Colorspace` entry it exposes.
    ///
    /// Absent entries are the common case, not a defect: the kernel reports
    /// only what the hardware supports. Measured on amdgpu (kernel 7.1.9), a
    /// DP connector carries five of the fifteen -- `Default`,
    /// `BT709_YCC`, `opRGB`, `BT2020_RGB`, `BT2020_YCC` -- and none of the
    /// DCI-P3 or xvYCC ones.
    colorspace: [Option<u64>; Colorspace::ALL.len()],
    /// As `colorspace`, for `Broadcast RGB`.
    broadcast_rgb: [Option<u64>; BroadcastRgb::ALL.len()],
    /// The `max bpc` range, if the connector has one.
    pub max_bpc: Option<MaxBpc>,
    /// Whether `HDR_OUTPUT_METADATA` is exposed.
    pub has_hdr_output_metadata: bool,
}

impl ConnectorCapabilities {
    /// A connector with none of these properties.
    ///
    /// The starting point for a synthetic one -- a fixture, a test, a
    /// capability set read from somewhere other than a live device.
    #[must_use]
    pub const fn new(connector_id: u32) -> Self {
        Self {
            connector_id,
            colorspace: [None; Colorspace::ALL.len()],
            broadcast_rgb: [None; BroadcastRgb::ALL.len()],
            max_bpc: None,
            has_hdr_output_metadata: false,
        }
    }

    /// Give a `Colorspace` entry the driver integer `value`.
    #[must_use]
    pub const fn with_colorspace(mut self, entry: Colorspace, value: u64) -> Self {
        self.colorspace[entry.index()] = Some(value);
        self
    }

    /// Give a `Broadcast RGB` entry the driver integer `value`.
    #[must_use]
    pub const fn with_broadcast_rgb(mut self, entry: BroadcastRgb, value: u64) -> Self {
        self.broadcast_rgb[entry.index()] = Some(value);
        self
    }

    /// Give the connector a `max bpc` range.
    #[must_use]
    pub const fn with_max_bpc(mut self, max_bpc: MaxBpc) -> Self {
        self.max_bpc = Some(max_bpc);
        self
    }

    /// Say whether the connector exposes `HDR_OUTPUT_METADATA`.
    #[must_use]
    pub const fn with_hdr_output_metadata(mut self, present: bool) -> Self {
        self.has_hdr_output_metadata = present;
        self
    }

    /// Read a connector's colour properties.
    ///
    /// A property the connector does not have is simply absent. The only
    /// error is not being able to read the property set at all.
    ///
    /// # Errors
    ///
    /// [`CoreError::Io`](drmkit_core::CoreError::Io) if the connector's
    /// properties cannot be read.
    pub fn probe(device: &Device, connector_id: u32) -> Result<Self> {
        let mut properties = PropertyStore::new();
        properties.cache_properties(device, connector_id, ObjectType::Connector)?;
        Ok(Self::from_properties(device, &properties, connector_id))
    }

    /// As [`ConnectorCapabilities::probe`], from a cache the caller already
    /// filled in.
    #[must_use]
    pub fn from_properties(device: &Device, properties: &PropertyStore, connector_id: u32) -> Self {
        let mut colorspace = [None; Colorspace::ALL.len()];
        for entry in Colorspace::ALL {
            colorspace[entry.index()] = properties
                .enum_value(device, connector_id, "Colorspace", entry.name())
                .ok();
        }
        let mut broadcast_rgb = [None; BroadcastRgb::ALL.len()];
        for entry in BroadcastRgb::ALL {
            broadcast_rgb[entry.index()] = properties
                .enum_value(device, connector_id, "Broadcast RGB", entry.name())
                .ok();
        }

        let max_bpc = properties
            .range(connector_id, "max bpc")
            .ok()
            .flatten()
            .and_then(|(min, max)| {
                Some(MaxBpc {
                    min,
                    max,
                    current: properties.property_value(connector_id, "max bpc").ok()?,
                })
            });

        Self {
            connector_id,
            colorspace,
            broadcast_rgb,
            max_bpc,
            has_hdr_output_metadata: properties
                .property_id(connector_id, "HDR_OUTPUT_METADATA")
                .is_ok(),
        }
    }

    /// The driver's integer for a `Colorspace` entry, if it has one.
    #[must_use]
    pub const fn colorspace(&self, entry: Colorspace) -> Option<u64> {
        self.colorspace[entry.index()]
    }

    /// The driver's integer for a `Broadcast RGB` entry, if it has one.
    #[must_use]
    pub const fn broadcast_rgb(&self, entry: BroadcastRgb) -> Option<u64> {
        self.broadcast_rgb[entry.index()]
    }

    /// Whether the connector exposes `Colorspace` at all.
    #[must_use]
    pub fn has_colorspace(&self) -> bool {
        self.colorspace.iter().any(Option::is_some)
    }

    /// Whether the connector exposes `Broadcast RGB` at all.
    #[must_use]
    pub fn has_broadcast_rgb(&self) -> bool {
        self.broadcast_rgb.iter().any(Option::is_some)
    }

    /// Whether this connector could carry an HDR signal.
    ///
    /// Both halves are needed. `HDR_OUTPUT_METADATA` alone at eight bits per
    /// component gives a sink that accepts the metadata and displays banded
    /// SDR; ten bits without the metadata is a wider SDR signal. A caller
    /// that checks only one ships a picture that looks wrong for reasons no
    /// error reports.
    #[must_use]
    pub fn can_signal_hdr(&self) -> bool {
        self.has_hdr_output_metadata && self.max_bpc.is_some_and(|bpc| bpc.max >= 10)
    }
}

/// What one CRTC's colour pipeline can be asked to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CrtcCapabilities {
    /// The CRTC this describes.
    pub crtc_id: u32,
    /// Rows the driver expects in a `DEGAMMA_LUT` blob. Zero if absent.
    pub degamma_lut_size: u32,
    /// Rows the driver expects in a `GAMMA_LUT` blob. Zero if absent.
    pub gamma_lut_size: u32,
    /// Whether `DEGAMMA_LUT` is exposed.
    pub has_degamma_lut: bool,
    /// Whether `CTM` is exposed.
    pub has_ctm: bool,
    /// Whether `GAMMA_LUT` is exposed.
    pub has_gamma_lut: bool,
}

impl CrtcCapabilities {
    /// Read a CRTC's colour-pipeline properties.
    ///
    /// # Errors
    ///
    /// As [`ConnectorCapabilities::probe`].
    pub fn probe(device: &Device, crtc_id: u32) -> Result<Self> {
        let mut properties = PropertyStore::new();
        properties.cache_properties(device, crtc_id, ObjectType::Crtc)?;
        Ok(Self::from_properties(&properties, crtc_id))
    }

    /// As [`CrtcCapabilities::probe`], from a cache the caller already filled
    /// in.
    #[must_use]
    pub fn from_properties(properties: &PropertyStore, crtc_id: u32) -> Self {
        // The size properties are what say how long a blob the driver wants.
        // Their presence is what says the stage exists at all: a driver that
        // exposes DEGAMMA_LUT without DEGAMMA_LUT_SIZE has told the caller
        // nothing it can act on, since a LUT of unknown length cannot be
        // built.
        let size = |name: &str| {
            properties
                .property_value(crtc_id, name)
                .ok()
                .and_then(|value| u32::try_from(value).ok())
                .unwrap_or(0)
        };
        let has = |name: &str| properties.property_id(crtc_id, name).is_ok();

        Self {
            crtc_id,
            degamma_lut_size: size("DEGAMMA_LUT_SIZE"),
            gamma_lut_size: size("GAMMA_LUT_SIZE"),
            has_degamma_lut: has("DEGAMMA_LUT"),
            has_ctm: has("CTM"),
            has_gamma_lut: has("GAMMA_LUT"),
        }
    }

    /// Whether the CRTC has the whole degamma, matrix, gamma pipeline.
    ///
    /// False on a driver exposing only part of it -- vkms has `GAMMA_LUT` and
    /// nothing else -- even though the stages it does have are usable.
    #[must_use]
    pub const fn has_full_pipeline(&self) -> bool {
        self.has_degamma_lut && self.has_ctm && self.has_gamma_lut
    }
}

#[cfg(test)]
mod tests {
    use super::{BroadcastRgb, Colorspace, ConnectorCapabilities, CrtcCapabilities, MaxBpc};

    /// A connector shaped like the amdgpu one this was measured against.
    fn amdgpu() -> ConnectorCapabilities {
        ConnectorCapabilities::new(383)
            .with_colorspace(Colorspace::Default, 0)
            .with_colorspace(Colorspace::Bt709Ycc, 1)
            .with_colorspace(Colorspace::OpRgb, 2)
            .with_colorspace(Colorspace::Bt2020Rgb, 3)
            .with_colorspace(Colorspace::Bt2020Ycc, 4)
            .with_max_bpc(MaxBpc {
                min: 8,
                max: 16,
                current: 16,
            })
            .with_hdr_output_metadata(true)
    }

    #[test]
    fn an_entry_the_driver_does_not_have_is_absent_rather_than_zero() {
        // Zero is a real enum value -- amdgpu's `Default` -- so a cache that
        // answered zero for a missing entry would silently write Default
        // wherever DCI-P3 was asked for.
        let connector = amdgpu();
        assert_eq!(connector.colorspace(Colorspace::Default), Some(0));
        assert_eq!(connector.colorspace(Colorspace::DciP3RgbD65), None);
    }

    #[test]
    fn every_entry_is_stored_under_its_own_name() {
        // The index is derived from the discriminant, so a variant inserted
        // in the middle of the enum would shift everything after it.
        let mut connector = ConnectorCapabilities::new(1);
        for (value, entry) in Colorspace::ALL.into_iter().enumerate() {
            connector = connector.with_colorspace(entry, value as u64);
        }
        for (value, entry) in Colorspace::ALL.into_iter().enumerate() {
            assert_eq!(
                connector.colorspace(entry),
                Some(value as u64),
                "{}",
                entry.name()
            );
        }
    }

    #[test]
    fn broadcast_rgb_entries_are_stored_the_same_way() {
        let mut connector = ConnectorCapabilities::new(1);
        for (value, entry) in BroadcastRgb::ALL.into_iter().enumerate() {
            connector = connector.with_broadcast_rgb(entry, 10 + value as u64);
        }
        for (value, entry) in BroadcastRgb::ALL.into_iter().enumerate() {
            assert_eq!(connector.broadcast_rgb(entry), Some(10 + value as u64));
        }
        assert!(connector.has_broadcast_rgb());
    }

    #[test]
    fn hdr_needs_the_metadata_and_the_bit_depth() {
        assert!(amdgpu().can_signal_hdr());

        // Metadata at eight bits is a sink that accepts the blob and shows
        // banded SDR.
        assert!(
            !ConnectorCapabilities::new(1)
                .with_hdr_output_metadata(true)
                .with_max_bpc(MaxBpc {
                    min: 6,
                    max: 8,
                    current: 8,
                })
                .can_signal_hdr()
        );

        // Ten bits without the blob is a wider SDR signal.
        assert!(
            !ConnectorCapabilities::new(1)
                .with_max_bpc(MaxBpc {
                    min: 8,
                    max: 10,
                    current: 8,
                })
                .can_signal_hdr()
        );

        // And a connector with no `max bpc` at all says nothing about depth,
        // which is not the same as saying ten.
        assert!(
            !ConnectorCapabilities::new(1)
                .with_hdr_output_metadata(true)
                .can_signal_hdr()
        );
    }

    #[test]
    fn the_current_bpc_is_what_is_set_not_what_is_possible() {
        // A connector that could do sixteen but is sitting at eight can still
        // signal HDR -- it is the ceiling that decides, and the current value
        // is what a caller would have to change.
        let connector = ConnectorCapabilities::new(1)
            .with_hdr_output_metadata(true)
            .with_max_bpc(MaxBpc {
                min: 8,
                max: 16,
                current: 8,
            });
        assert!(connector.can_signal_hdr());
        assert_eq!(connector.max_bpc.map(|bpc| bpc.current), Some(8));
    }

    #[test]
    fn a_connector_with_nothing_on_it_says_so() {
        let connector = ConnectorCapabilities::new(7);
        assert!(!connector.has_colorspace());
        assert!(!connector.has_broadcast_rgb());
        assert!(!connector.can_signal_hdr());
        assert_eq!(connector.connector_id, 7);
    }

    #[test]
    fn a_partial_colour_pipeline_is_not_a_full_one() {
        // vkms exposes GAMMA_LUT and nothing else. The stage is usable; the
        // pipeline is not, and a caller programming a CTM into it gets EINVAL
        // on the whole commit.
        let vkms = CrtcCapabilities {
            crtc_id: 34,
            gamma_lut_size: 256,
            has_gamma_lut: true,
            ..CrtcCapabilities::default()
        };
        assert!(!vkms.has_full_pipeline());

        let amdgpu = CrtcCapabilities {
            crtc_id: 369,
            degamma_lut_size: 4096,
            gamma_lut_size: 4096,
            has_degamma_lut: true,
            has_ctm: true,
            has_gamma_lut: true,
        };
        assert!(amdgpu.has_full_pipeline());
    }
}
