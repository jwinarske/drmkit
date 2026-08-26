// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! What to tell the connector about the frame it is being handed.
//!
//! The producer side describes each layer's container colorimetry and
//! transfer function. The connector wants one `Colorspace` enum value and,
//! for HDR, one `HDR_OUTPUT_METADATA` blob describing the mastering display.
//! This is the bridge, and the whole of it is arithmetic over layer
//! descriptions -- no device, so it can be reasoned about without one.

use drmkit_display::color::{Chromaticity, ColorimetryInfo};
use drmkit_display::{Colorspace, ConnectorCapabilities, HdrSourceMetadata, TransferFunction};

use crate::display::DisplayParams;

/// The container colorimetry a producer says its layer is in.
///
/// The friendly producer-side description, distinct from
/// [`Colorspace`], which is what the driver advertises on a connector. The
/// scene picks the connector enum that best matches the widest-gamut layer at
/// commit time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub enum ColorPrimaries {
    /// BT.709 / sRGB. The SDR default.
    #[default]
    Bt709,
    /// Adobe RGB (1998).
    AdobeRgb,
    /// DCI-P3 D65, display-referred consumer P3.
    DciP3,
    /// BT.2020 / Rec.2100. HDR and wide-gamut SDR.
    Bt2020,
}

impl ColorPrimaries {
    /// Widest last, so the derived `Ord` ranks gamut directly.
    ///
    /// Deliberate: a separate rank function and a variant order that
    /// disagreed would be a silent mis-ranking, and there is nothing to
    /// notice it. Adding a variant in the wrong place breaks a test here
    /// rather than a picture on a display.
    #[must_use]
    pub const fn is_wider_than(self, other: Self) -> bool {
        (self as u8) > (other as u8)
    }

    /// Canonical CIE 1931 chromaticities, ready to seed mastering-display
    /// primaries.
    ///
    /// All four are D65-white.
    #[must_use]
    pub const fn colorimetry(self) -> ColorimetryInfo {
        let (red, green, blue) = match self {
            // ITU-R BT.709-6 section 3.2.
            Self::Bt709 => ((0.640, 0.330), (0.300, 0.600), (0.150, 0.060)),
            // ITU-R BT.2020-2 section 2.3.
            Self::Bt2020 => ((0.708, 0.292), (0.170, 0.797), (0.131, 0.046)),
            // SMPTE EG 432-1.
            Self::DciP3 => ((0.680, 0.320), (0.265, 0.690), (0.150, 0.060)),
            // Adobe RGB (1998) section 4.3.4.1. Red and blue are BT.709's;
            // only green is wider, which is the whole of the difference.
            Self::AdobeRgb => ((0.640, 0.330), (0.210, 0.710), (0.150, 0.060)),
        };
        ColorimetryInfo {
            red: Chromaticity { x: red.0, y: red.1 },
            green: Chromaticity {
                x: green.0,
                y: green.1,
            },
            blue: Chromaticity {
                x: blue.0,
                y: blue.1,
            },
            white: Chromaticity {
                x: 0.3127,
                y: 0.3290,
            },
            has_primaries: true,
            has_default_white: true,
        }
    }

    /// The connector enum that best carries this.
    #[must_use]
    pub const fn colorspace(self) -> Colorspace {
        match self {
            // Not `Bt709Ycc`: that would force YCC encoding, which is wrong
            // for an RGB source. `Default` lets the driver use its standard
            // SDR signalling, which is what an sRGB desktop wants.
            Self::Bt709 => Colorspace::Default,
            // The RGB variant, for the desktop and signage path. YCC content
            // says so through the plane's `COLOR_ENCODING` instead.
            Self::Bt2020 => Colorspace::Bt2020Rgb,
            Self::DciP3 => Colorspace::DciP3RgbD65,
            Self::AdobeRgb => Colorspace::OpRgb,
        }
    }
}

/// What the scene wants declared on the connector for one frame.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct OutputSignalling {
    /// The `Colorspace` to write, or `None` to leave the property alone.
    ///
    /// `None` when no layer said what it was in. Writing a guess would
    /// override whatever the last client left, which on a connector is
    /// sticky.
    pub colorspace: Option<Colorspace>,
    /// The metadata to publish, or `None` for no HDR signalling.
    pub hdr_metadata: Option<HdrSourceMetadata>,
    /// Whether HDR was called for and could not be carried.
    ///
    /// The layers asked for it and the connector cannot: no
    /// `HDR_OUTPUT_METADATA`, or a `max bpc` ceiling below 10. The metadata
    /// is dropped rather than sent, and this says so -- a caller that wants
    /// to tone-map itself, or tell the user, has no other way to find out.
    pub hdr_downgraded: bool,
}

/// The widest-gamut primaries any layer names.
///
/// `None` when no layer names any, which the caller reads as "leave the
/// connector alone" rather than as BT.709.
#[must_use]
pub fn widest_gamut<'a>(
    layers: impl IntoIterator<Item = &'a DisplayParams>,
) -> Option<ColorPrimaries> {
    layers
        .into_iter()
        .filter_map(|layer| layer.color_primaries)
        .max()
}

/// Whether a transfer function is one that needs HDR signalling.
///
/// `TraditionalGammaHdr` is not: it is HDR content over a BT.1886 path, and
/// publishing PQ metadata for it would tell the sink to apply a curve the
/// content is not in.
const fn is_hdr(eotf: TransferFunction) -> bool {
    matches!(
        eotf,
        TransferFunction::SmpteSt2084Pq | TransferFunction::Bt2100Hlg
    )
}

/// Derive what the connector should signal for this frame's layers.
///
/// - The widest-gamut layer decides the `Colorspace`. No layer naming one
///   leaves it unset.
/// - Any layer in PQ or HLG produces metadata; PQ wins when both are present,
///   being the more common HDR10 path.
/// - The mastering primaries come from the widest-gamut layer, defaulting to
///   BT.2020 when an HDR layer names none -- an HDR layer that did not say is
///   far more likely to be BT.2020 than BT.709.
/// - Luminance, `MaxCLL` and `MaxFALL` stay zero. A caller with real
///   mastering data sets the whole struct itself; inventing values here would
///   have the sink tone-map to numbers nobody measured.
///
/// `capabilities` gates the HDR half. Passing `None` skips the check, for a
/// caller that already gated up front.
#[must_use]
pub fn derive_output_signalling(
    layers: &[DisplayParams],
    capabilities: Option<&ConnectorCapabilities>,
) -> OutputSignalling {
    let gamut = widest_gamut(layers);

    let eotf = layers
        .iter()
        .filter_map(|layer| layer.source_eotf)
        .filter(|eotf| is_hdr(*eotf))
        // PQ over HLG, and `SmpteSt2084Pq` sorts before `Bt2100Hlg` in the
        // enum, so this is `min` rather than `max`.
        .min_by_key(|eotf| match eotf {
            TransferFunction::SmpteSt2084Pq => 0_u8,
            _ => 1,
        });

    let mut signalling = OutputSignalling {
        colorspace: gamut.map(ColorPrimaries::colorspace),
        hdr_metadata: eotf.map(|eotf| HdrSourceMetadata {
            eotf,
            display_primaries: gamut.unwrap_or(ColorPrimaries::Bt2020).colorimetry(),
            max_display_mastering_luminance: 0,
            min_display_mastering_luminance: 0,
            max_content_light_level: 0,
            max_frame_average_light_level: 0,
        }),
        hdr_downgraded: false,
    };

    if signalling.hdr_metadata.is_some() && capabilities.is_some_and(|caps| !caps.can_signal_hdr())
    {
        // Eight bits per component with a PQ flag is a sink that accepts the
        // metadata and shows banded SDR. Dropping the signalling is the
        // honest answer, and the flag is how the caller learns it happened.
        signalling.hdr_metadata = None;
        signalling.hdr_downgraded = true;
    }

    signalling
}

#[cfg(test)]
mod tests {
    use super::{ColorPrimaries, derive_output_signalling, widest_gamut};
    use crate::display::DisplayParams;
    use drmkit_display::{Colorspace, ConnectorCapabilities, MaxBpc, TransferFunction};

    /// A layer that says nothing about colour.
    fn plain() -> DisplayParams {
        DisplayParams::default()
    }

    fn layer(primaries: Option<ColorPrimaries>, eotf: Option<TransferFunction>) -> DisplayParams {
        DisplayParams {
            color_primaries: primaries,
            source_eotf: eotf,
            ..DisplayParams::default()
        }
    }

    /// A connector that can carry HDR: the blob and ten bits.
    fn hdr_capable() -> ConnectorCapabilities {
        ConnectorCapabilities::new(1)
            .with_hdr_output_metadata(true)
            .with_max_bpc(MaxBpc {
                min: 8,
                max: 10,
                current: 10,
            })
    }

    // ── Canonical primaries ────────────────────────────────────────────

    #[test]
    fn bt709_is_canonical_srgb() {
        let c = ColorPrimaries::Bt709.colorimetry();
        assert!((c.red.x - 0.640).abs() < 1e-6 && (c.red.y - 0.330).abs() < 1e-6);
        assert!((c.green.x - 0.300).abs() < 1e-6 && (c.green.y - 0.600).abs() < 1e-6);
        assert!((c.blue.x - 0.150).abs() < 1e-6 && (c.blue.y - 0.060).abs() < 1e-6);
        assert!((c.white.x - 0.3127).abs() < 1e-6 && (c.white.y - 0.3290).abs() < 1e-6);
        assert!(c.has_primaries && c.has_default_white);
    }

    #[test]
    fn bt2020_matches_rec2100() {
        let c = ColorPrimaries::Bt2020.colorimetry();
        assert!((c.red.x - 0.708).abs() < 1e-6);
        assert!((c.green.y - 0.797).abs() < 1e-6);
        assert!((c.blue.x - 0.131).abs() < 1e-6);
    }

    #[test]
    fn dci_p3_d65_matches_smpte_eg432() {
        let c = ColorPrimaries::DciP3.colorimetry();
        assert!((c.red.x - 0.680).abs() < 1e-6);
        assert!((c.green.x - 0.265).abs() < 1e-6);
        // D65, not the DCI theatre white -- this is the consumer variant.
        assert!((c.white.x - 0.3127).abs() < 1e-6);
    }

    #[test]
    fn adobe_rgb_differs_from_bt709_only_in_green() {
        // The whole of Adobe RGB's extra gamut is the green primary. Red and
        // blue matching BT.709 is the check that the table was not copied
        // from the wrong row.
        let adobe = ColorPrimaries::AdobeRgb.colorimetry();
        let bt709 = ColorPrimaries::Bt709.colorimetry();
        assert_eq!(adobe.red, bt709.red);
        assert_eq!(adobe.blue, bt709.blue);
        assert_ne!(adobe.green, bt709.green);
        assert!((adobe.green.y - 0.710).abs() < 1e-6);
    }

    // ── Widest gamut ───────────────────────────────────────────────────

    #[test]
    fn no_layers_name_no_gamut() {
        assert_eq!(widest_gamut(&[] as &[DisplayParams]), None);
        assert_eq!(widest_gamut(&[plain(), plain()]), None);
    }

    #[test]
    fn one_layer_passes_its_own_through() {
        let layers = [layer(Some(ColorPrimaries::DciP3), None)];
        assert_eq!(widest_gamut(&layers), Some(ColorPrimaries::DciP3));
    }

    #[test]
    fn the_ranking_runs_bt2020_over_p3_over_adobe_over_bt709() {
        for (wider, narrower) in [
            (ColorPrimaries::Bt2020, ColorPrimaries::DciP3),
            (ColorPrimaries::DciP3, ColorPrimaries::AdobeRgb),
            (ColorPrimaries::AdobeRgb, ColorPrimaries::Bt709),
        ] {
            assert!(wider.is_wider_than(narrower), "{wider:?} over {narrower:?}");
            // Order-independent: the answer is the gamut, not the position.
            assert_eq!(
                widest_gamut(&[layer(Some(narrower), None), layer(Some(wider), None)]),
                Some(wider)
            );
            assert_eq!(
                widest_gamut(&[layer(Some(wider), None), layer(Some(narrower), None)]),
                Some(wider)
            );
        }
    }

    #[test]
    fn a_mixed_set_selects_the_widest() {
        let layers = [
            layer(Some(ColorPrimaries::Bt709), None),
            plain(),
            layer(Some(ColorPrimaries::Bt2020), None),
            layer(Some(ColorPrimaries::AdobeRgb), None),
        ];
        assert_eq!(widest_gamut(&layers), Some(ColorPrimaries::Bt2020));
    }

    #[test]
    fn layers_that_say_nothing_are_passed_over_not_counted_as_bt709() {
        // Counting an unset layer as BT.709 would be harmless here but wrong
        // in `derive`: a frame of nothing-but-unset layers must leave the
        // connector alone, not write Default over whatever is there.
        let layers = [plain(), layer(Some(ColorPrimaries::DciP3), None), plain()];
        assert_eq!(widest_gamut(&layers), Some(ColorPrimaries::DciP3));
    }

    // ── Primaries to connector enum ────────────────────────────────────

    #[test]
    fn bt709_maps_to_default_rather_than_bt709_ycc() {
        // BT709_YCC would force YCC encoding on an RGB source. `Default` lets
        // the driver use its standard SDR signalling, which is what an sRGB
        // desktop wants.
        assert_eq!(ColorPrimaries::Bt709.colorspace(), Colorspace::Default);
    }

    #[test]
    fn the_other_three_map_to_their_own_entries() {
        assert_eq!(ColorPrimaries::Bt2020.colorspace(), Colorspace::Bt2020Rgb);
        assert_eq!(ColorPrimaries::DciP3.colorspace(), Colorspace::DciP3RgbD65);
        assert_eq!(ColorPrimaries::AdobeRgb.colorspace(), Colorspace::OpRgb);
    }

    // ── Deriving the whole thing ───────────────────────────────────────

    #[test]
    fn no_layers_signal_nothing() {
        let signalling = derive_output_signalling(&[], None);
        assert_eq!(signalling.colorspace, None);
        assert_eq!(signalling.hdr_metadata, None);
        assert!(!signalling.hdr_downgraded);
    }

    #[test]
    fn an_all_sdr_frame_that_names_nothing_leaves_the_connector_alone() {
        let signalling = derive_output_signalling(&[plain(), plain()], None);
        assert_eq!(signalling.colorspace, None);
        assert_eq!(signalling.hdr_metadata, None);
    }

    #[test]
    fn an_sdr_bt709_layer_asks_for_default_and_no_hdr() {
        let layers = [layer(Some(ColorPrimaries::Bt709), None)];
        let signalling = derive_output_signalling(&layers, None);
        assert_eq!(signalling.colorspace, Some(Colorspace::Default));
        assert_eq!(signalling.hdr_metadata, None);
    }

    #[test]
    fn a_bt2020_pq_layer_produces_hdr_signalling() {
        let layers = [layer(
            Some(ColorPrimaries::Bt2020),
            Some(TransferFunction::SmpteSt2084Pq),
        )];
        let signalling = derive_output_signalling(&layers, None);
        assert_eq!(signalling.colorspace, Some(Colorspace::Bt2020Rgb));
        let hdr = signalling.hdr_metadata.expect("hdr");
        assert_eq!(hdr.eotf, TransferFunction::SmpteSt2084Pq);
        assert_eq!(
            hdr.display_primaries,
            ColorPrimaries::Bt2020.colorimetry(),
            "the mastering primaries come from the widest-gamut layer"
        );
        // Left at zero deliberately: inventing luminance would have the sink
        // tone-map to numbers nobody measured.
        assert_eq!(hdr.max_display_mastering_luminance, 0);
        assert_eq!(hdr.max_content_light_level, 0);
    }

    #[test]
    fn one_hdr_layer_among_sdr_ones_carries_the_frame() {
        let layers = [
            layer(Some(ColorPrimaries::Bt709), None),
            layer(
                Some(ColorPrimaries::Bt2020),
                Some(TransferFunction::SmpteSt2084Pq),
            ),
        ];
        let signalling = derive_output_signalling(&layers, None);
        assert_eq!(signalling.colorspace, Some(Colorspace::Bt2020Rgb));
        assert!(signalling.hdr_metadata.is_some());
    }

    #[test]
    fn pq_wins_when_both_hdr_curves_are_present() {
        // HDR10 is the common path, and the two cannot both be signalled.
        for order in [[0, 1], [1, 0]] {
            let both = [
                layer(
                    Some(ColorPrimaries::Bt2020),
                    Some(TransferFunction::SmpteSt2084Pq),
                ),
                layer(
                    Some(ColorPrimaries::Bt2020),
                    Some(TransferFunction::Bt2100Hlg),
                ),
            ];
            let layers = [both[order[0]], both[order[1]]];
            let hdr = derive_output_signalling(&layers, None)
                .hdr_metadata
                .expect("hdr");
            assert_eq!(hdr.eotf, TransferFunction::SmpteSt2084Pq);
        }
    }

    #[test]
    fn an_hlg_only_frame_signals_hlg() {
        let layers = [layer(
            Some(ColorPrimaries::Bt2020),
            Some(TransferFunction::Bt2100Hlg),
        )];
        let hdr = derive_output_signalling(&layers, None)
            .hdr_metadata
            .expect("hdr");
        assert_eq!(hdr.eotf, TransferFunction::Bt2100Hlg);
    }

    #[test]
    fn an_hdr_layer_that_names_no_gamut_masters_in_bt2020() {
        // BT.709 would be the wrong default here: content that is HDR and did
        // not say is far more likely to be BT.2020, and telling the sink
        // BT.709 shrinks the gamut it renders into.
        let layers = [layer(None, Some(TransferFunction::SmpteSt2084Pq))];
        let signalling = derive_output_signalling(&layers, None);
        assert_eq!(signalling.colorspace, None, "no layer named a gamut");
        let hdr = signalling.hdr_metadata.expect("hdr");
        assert_eq!(hdr.display_primaries, ColorPrimaries::Bt2020.colorimetry());
    }

    #[test]
    fn traditional_gamma_hdr_is_not_hdr_signalling() {
        // HDR content over a BT.1886 path. Publishing PQ metadata for it would
        // tell the sink to apply a curve the content is not in.
        let layers = [layer(
            Some(ColorPrimaries::Bt2020),
            Some(TransferFunction::TraditionalGammaHdr),
        )];
        let signalling = derive_output_signalling(&layers, None);
        assert_eq!(signalling.hdr_metadata, None);
        assert!(!signalling.hdr_downgraded, "nothing was dropped");
        assert_eq!(signalling.colorspace, Some(Colorspace::Bt2020Rgb));
    }

    // ── Connector constraints ──────────────────────────────────────────

    #[test]
    fn a_capable_connector_keeps_the_metadata() {
        let layers = [layer(
            Some(ColorPrimaries::Bt2020),
            Some(TransferFunction::SmpteSt2084Pq),
        )];
        let signalling = derive_output_signalling(&layers, Some(&hdr_capable()));
        assert!(signalling.hdr_metadata.is_some());
        assert!(!signalling.hdr_downgraded);
    }

    #[test]
    fn a_connector_without_the_blob_downgrades() {
        let layers = [layer(
            Some(ColorPrimaries::Bt2020),
            Some(TransferFunction::SmpteSt2084Pq),
        )];
        let no_blob = ConnectorCapabilities::new(1).with_max_bpc(MaxBpc {
            min: 8,
            max: 12,
            current: 12,
        });
        let signalling = derive_output_signalling(&layers, Some(&no_blob));
        assert_eq!(signalling.hdr_metadata, None);
        assert!(signalling.hdr_downgraded);
        // The gamut still goes out: that half is not gated on HDR.
        assert_eq!(signalling.colorspace, Some(Colorspace::Bt2020Rgb));
    }

    #[test]
    fn a_connector_capped_at_eight_bits_downgrades() {
        // The metadata would be accepted and the sink would show banded SDR.
        let layers = [layer(
            Some(ColorPrimaries::Bt2020),
            Some(TransferFunction::SmpteSt2084Pq),
        )];
        let eight_bit = ConnectorCapabilities::new(1)
            .with_hdr_output_metadata(true)
            .with_max_bpc(MaxBpc {
                min: 6,
                max: 8,
                current: 8,
            });
        let signalling = derive_output_signalling(&layers, Some(&eight_bit));
        assert_eq!(signalling.hdr_metadata, None);
        assert!(signalling.hdr_downgraded);
    }

    #[test]
    fn ten_bits_is_enough() {
        // The boundary, and it is inclusive: ten is where PQ stops banding.
        let layers = [layer(
            Some(ColorPrimaries::Bt2020),
            Some(TransferFunction::SmpteSt2084Pq),
        )];
        assert!(
            derive_output_signalling(&layers, Some(&hdr_capable()))
                .hdr_metadata
                .is_some()
        );
    }

    #[test]
    fn an_sdr_frame_is_never_downgraded() {
        // There was nothing to drop, so the flag must stay false even on a
        // connector that could not have carried HDR.
        let layers = [layer(Some(ColorPrimaries::Bt709), None)];
        let eight_bit = ConnectorCapabilities::new(1).with_max_bpc(MaxBpc {
            min: 6,
            max: 8,
            current: 8,
        });
        let signalling = derive_output_signalling(&layers, Some(&eight_bit));
        assert!(!signalling.hdr_downgraded);
        assert_eq!(signalling.colorspace, Some(Colorspace::Default));
    }

    #[test]
    fn without_capabilities_the_constraint_is_not_checked() {
        // For a caller that gated up front, and for a test that wants the
        // derive in isolation.
        let layers = [layer(
            Some(ColorPrimaries::Bt2020),
            Some(TransferFunction::SmpteSt2084Pq),
        )];
        let signalling = derive_output_signalling(&layers, None);
        assert!(signalling.hdr_metadata.is_some());
        assert!(!signalling.hdr_downgraded);
    }

    #[test]
    fn the_defaults_name_nothing() {
        let params = DisplayParams::default();
        assert_eq!(params.color_primaries, None);
        assert_eq!(params.source_eotf, None);
    }
}
