// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! Display characterisation and colour handling.
//!
//! What a connected display can do, and what has to happen to pixels before
//! they reach it. Two halves that mostly do not talk to each other.
//!
//! # Finding the display
//!
//! - [`ScanoutTarget`] — the connected output and the CRTC and primary plane
//!   that drive it, which is the pair everything downstream needs and the one
//!   thing `crtcs().first()` reliably gets wrong on real hardware.
//! - [`query_connector_modes`] — what each connector offers, with
//!   [`Probe`] deciding whether to ask the sink or trust the cache.
//! - [`ConnectorCapabilities`] and [`CrtcCapabilities`] — the properties this
//!   pipe actually exposes, so a caller writes what the driver has rather than
//!   what the spec allows.
//! - [`HotplugMonitor`] — the kernel's own uevent stream, behind the `hotplug`
//!   feature.
//! - [`DriverProfile`] — device capabilities, probed and never inferred from
//!   the driver's name.
//!
//! # Getting the pixels right
//!
//! - [`ToneMapper`] — SDR to HDR and back, over a [`ToneMapCurve`]. Colour
//!   maths has too many plausible readings for a paper derivation to be worth
//!   much, so its answers are diffed against the reference's own, vector by
//!   vector.
//! - [`CrtcColorPipeline`] — the CRTC's degamma, CTM and gamma stages. Each
//!   refuses on its own rather than the pipeline refusing whole, because
//!   partial pipelines are the common case: of 4,129 CRTCs surveyed, 848 have
//!   no colour stage at all and three more shapes sit between that and the
//!   full three.
//! - [`HdrSourceMetadata`] and [`HdrMetadataCache`] — the CTA-861.3 blob a
//!   sink needs to switch into HDR, and the cache that stops it being rewritten
//!   every frame. Its green/blue/red primary ordering and tail padding are both
//!   invisible on screen and both change the bytes, so this is diffed against
//!   the reference too, over twelve vectors.
//! - [`edid`] — what the sink says about itself, behind the `edid` feature.
//!
//! # Features
//!
//! `edid` is off by default: `libdisplay-info-sys` links a system library, and
//! the cross lanes have no sysroot for it. Everything else here — the tone
//! mapper and colour curves that downstream crates want — builds without it.

mod capabilities;

#[cfg(feature = "hotplug")]
mod hotplug;
mod pipeline;
pub mod scanout;

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
    CrtcChoice, ScanoutError, ScanoutTarget, crtc_for_connector, primary_plane_for_crtc, rank,
    rank_pick,
};
pub use tone_mapper::{Direction, LUT_SIZE, ToneMapCurve, ToneMapper};
