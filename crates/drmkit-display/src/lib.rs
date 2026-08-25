// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! Display characterisation and colour handling.
//!
//! What a connected display can do, and what has to happen to pixels before
//! they reach it.

mod capabilities;

#[cfg(feature = "hotplug")]
mod hotplug;
mod pipeline;
mod scanout;

pub mod color;
pub mod curves;
#[cfg(feature = "edid")]
pub mod edid;
pub mod hdr;
pub mod hdr_cache;
mod modes;
mod profile;
mod tone_mapper;

#[cfg(test)]
mod hdr_tests;
#[cfg(test)]
mod tests;

pub use capabilities::{BroadcastRgb, Colorspace, ConnectorCapabilities, CrtcCapabilities, MaxBpc};
pub use curves::{ColorCtm, ColorLut};
pub use hdr::{
    HdrBlob, HdrMetadataBlob, HdrSourceMetadata, TransferFunction, hdr_metadata_hash,
    serialize_hdr_metadata,
};
pub use hdr_cache::HdrMetadataCache;
#[cfg(feature = "hotplug")]
pub use hotplug::{HotplugEvent, HotplugMonitor, Source};
pub use modes::{ConnectorModes, Probe, query_connector_modes};
pub use pipeline::{CrtcColorPipeline, PipelineError, Stage};
pub use profile::{
    DriverProfile, PanelSelfRefresh, PrimeCaps, connector_type_self_refreshes, decode_prime_caps,
};
pub use scanout::{
    CrtcChoice, ScanoutError, ScanoutTarget, crtc_for_connector, primary_plane_for_crtc,
};
pub use tone_mapper::{Direction, LUT_SIZE, ToneMapCurve, ToneMapper};
