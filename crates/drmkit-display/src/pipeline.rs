// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! The CRTC's colour pipeline: degamma, matrix, gamma.
//!
//! Three blob properties in a fixed order. Content arrives encoded, degamma
//! takes it to linear light, the matrix moves it between primaries -- which is
//! only meaningful in linear light -- and gamma encodes it for the sink.

use drmkit_core::{AtomicRequest, CoreError, Device, PropertyBlob, PropertyStore};

use crate::capabilities::CrtcCapabilities;
use crate::curves::{
    ColorCtm, ColorLut, build_bt2020_to_bt709_ctm, build_hlg_oetf_inverse_lut, build_identity_ctm,
    build_identity_lut, build_pq_eotf_lut, build_pq_oetf_lut,
};

/// Which stage a call was about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    /// `DEGAMMA_LUT`: encoded to linear.
    Degamma,
    /// `CTM`: the 3x3 matrix.
    Ctm,
    /// `GAMMA_LUT`: linear to encoded.
    Gamma,
}

impl Stage {
    /// The kernel's property name.
    #[must_use]
    pub const fn property(self) -> &'static str {
        match self {
            Self::Degamma => "DEGAMMA_LUT",
            Self::Ctm => "CTM",
            Self::Gamma => "GAMMA_LUT",
        }
    }
}

/// What can go wrong programming the pipeline.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum PipelineError {
    /// The CRTC exposes none of the three stages, so there is nothing to
    /// program. Older and simpler display controllers.
    #[error("this CRTC has no colour pipeline at all")]
    NoPipeline,

    /// A LUT was asked for on the matrix stage, which does not take one.
    #[error("{} is a matrix, not a LUT", .0.property())]
    NotALut(Stage),

    /// The kernel refused.
    #[error(transparent)]
    Core(#[from] CoreError),

    /// The CRTC does not have this stage.
    ///
    /// Common and not a defect: plenty of drivers expose one or two of the
    /// three. A caller checks
    /// [`capabilities`](CrtcColorPipeline::capabilities) and works with what
    /// is there.
    #[error("this CRTC has no {} stage", .0.property())]
    NoSuchStage(Stage),

    /// A custom LUT was the wrong length.
    ///
    /// The driver states exactly how many entries it reads, and a blob of any
    /// other length is rejected -- which fails the whole atomic commit, not
    /// just this property.
    #[error("{} wants {expected} entries, not {given}", .stage.property())]
    WrongLutSize {
        /// Which stage.
        stage: Stage,
        /// What the driver asks for.
        expected: u32,
        /// What was supplied.
        given: usize,
    },
}

/// The colour pipeline of one CRTC.
///
/// Holds the blobs it creates and destroys them on drop, and borrows the
/// device so a blob cannot outlive the descriptor that has to free it.
#[derive(Debug)]
pub struct CrtcColorPipeline<'a> {
    device: &'a Device,
    crtc_id: u32,
    capabilities: CrtcCapabilities,
    /// The property ids, resolved once.
    ///
    /// The reference re-reads the CRTC's whole property set on every apply --
    /// nine properties on amdgpu, seven on vkms, so eight or ten ioctls per
    /// commit for three numbers that never change.
    properties: StageProperties,
    degamma: Option<PropertyBlob<'a>>,
    ctm: Option<PropertyBlob<'a>>,
    gamma: Option<PropertyBlob<'a>>,
}

/// The property ids for the three stages, where they exist.
#[derive(Debug, Clone, Copy, Default)]
struct StageProperties {
    degamma: Option<u32>,
    ctm: Option<u32>,
    gamma: Option<u32>,
}

impl<'a> CrtcColorPipeline<'a> {
    /// Read a CRTC's colour capabilities and prepare to program them.
    ///
    /// A driver exposing only part of the pipeline -- vkms has gamma and
    /// nothing else -- succeeds; the stages it lacks refuse.
    ///
    /// # Errors
    ///
    /// - [`PipelineError::NoPipeline`] if the CRTC has none of the three.
    /// - [`PipelineError::Core`] if its properties cannot be read.
    pub fn new(device: &'a Device, crtc_id: u32) -> Result<Self, PipelineError> {
        let mut properties = PropertyStore::new();
        properties.cache_properties(device, crtc_id, drmkit_core::ObjectType::Crtc)?;
        let capabilities = CrtcCapabilities::from_properties(&properties, crtc_id);
        if !capabilities.has_degamma_lut && !capabilities.has_ctm && !capabilities.has_gamma_lut {
            return Err(PipelineError::NoPipeline);
        }
        let id = |stage: Stage| properties.property_id(crtc_id, stage.property()).ok();
        Ok(Self {
            device,
            crtc_id,
            capabilities,
            properties: StageProperties {
                degamma: id(Stage::Degamma),
                ctm: id(Stage::Ctm),
                gamma: id(Stage::Gamma),
            },
            degamma: None,
            ctm: None,
            gamma: None,
        })
    }

    /// What this CRTC can do.
    #[must_use]
    pub const fn capabilities(&self) -> &CrtcCapabilities {
        &self.capabilities
    }

    /// The blob currently staged for a stage, if any.
    #[must_use]
    pub fn blob_id(&self, stage: Stage) -> Option<u64> {
        self.slot(stage).map(PropertyBlob::id)
    }

    /// Set every stage this CRTC has to identity.
    ///
    /// For taking over a display from something that left a curve on it: the
    /// kernel keeps whatever the last client set, so a compositor that never
    /// writes these inherits them.
    ///
    /// # Errors
    ///
    /// [`PipelineError::Core`] if a blob cannot be created.
    pub fn set_identity(&mut self) -> Result<(), PipelineError> {
        if self.capabilities.has_degamma_lut {
            self.set_lut(Stage::Degamma, build_identity_lut)?;
        }
        if self.capabilities.has_ctm {
            self.set_ctm_blob(build_identity_ctm())?;
        }
        if self.capabilities.has_gamma_lut {
            self.set_lut(Stage::Gamma, build_identity_lut)?;
        }
        Ok(())
    }

    /// Degamma: PQ encoded to linear, for HDR10 content.
    ///
    /// # Errors
    ///
    /// [`PipelineError::NoSuchStage`] if the CRTC has no degamma stage.
    pub fn set_pq_to_linear(&mut self) -> Result<(), PipelineError> {
        self.require(Stage::Degamma)?;
        self.set_lut(Stage::Degamma, build_pq_eotf_lut)
    }

    /// Degamma: HLG encoded to scene-linear.
    ///
    /// # Errors
    ///
    /// As [`CrtcColorPipeline::set_pq_to_linear`].
    pub fn set_hlg_to_linear(&mut self) -> Result<(), PipelineError> {
        self.require(Stage::Degamma)?;
        self.set_lut(Stage::Degamma, build_hlg_oetf_inverse_lut)
    }

    /// Matrix: BT.2020 to BT.709, in linear light.
    ///
    /// Needs a degamma stage in front of it that produces linear values.
    ///
    /// # Errors
    ///
    /// [`PipelineError::NoSuchStage`] if the CRTC has no matrix stage.
    pub fn set_bt2020_to_bt709(&mut self) -> Result<(), PipelineError> {
        self.require(Stage::Ctm)?;
        self.set_ctm_blob(build_bt2020_to_bt709_ctm())
    }

    /// Gamma: linear to PQ encoded, for HDR10 output.
    ///
    /// # Errors
    ///
    /// [`PipelineError::NoSuchStage`] if the CRTC has no gamma stage.
    pub fn set_linear_to_pq(&mut self) -> Result<(), PipelineError> {
        self.require(Stage::Gamma)?;
        self.set_lut(Stage::Gamma, build_pq_oetf_lut)
    }

    /// Stage a LUT of the caller's own.
    ///
    /// # Errors
    ///
    /// - [`PipelineError::NoSuchStage`] if the CRTC has no such stage,
    ///   [`PipelineError::NotALut`] for the matrix, or
    ///   [`PipelineError::WrongLutSize`] if `lut` is not the length the driver
    ///   asks for.
    /// - [`PipelineError::Core`] if the blob cannot be created.
    pub fn set_custom_lut(&mut self, stage: Stage, lut: &[ColorLut]) -> Result<(), PipelineError> {
        self.require(stage)?;
        let expected = self.lut_size(stage).ok_or(PipelineError::NotALut(stage))?;
        if lut.len() != expected as usize {
            return Err(PipelineError::WrongLutSize {
                stage,
                expected,
                given: lut.len(),
            });
        }
        self.replace(stage, self.device.create_property_blob(lut)?);
        Ok(())
    }

    /// Stage a matrix of the caller's own.
    ///
    /// # Errors
    ///
    /// As [`CrtcColorPipeline::set_bt2020_to_bt709`].
    pub fn set_custom_ctm(&mut self, ctm: ColorCtm) -> Result<(), PipelineError> {
        self.require(Stage::Ctm)?;
        self.set_ctm_blob(ctm)
    }

    /// Write the staged blob ids into `request`.
    ///
    /// A stage that has not been set is left out. The kernel keeps whatever
    /// the property already holds, which is what a caller programming only
    /// gamma wants -- and is also why [`set_identity`](Self::set_identity)
    /// exists for a caller that does not.
    ///
    /// # Errors
    ///
    /// [`PipelineError::Core`] if a property cannot be added to the request.
    pub fn apply(&self, request: &mut AtomicRequest) -> Result<(), PipelineError> {
        for stage in [Stage::Degamma, Stage::Ctm, Stage::Gamma] {
            let (Some(blob), Some(property)) = (self.blob_id(stage), self.property_id(stage))
            else {
                continue;
            };
            request.add_property(self.crtc_id, property, blob)?;
        }
        Ok(())
    }

    /// Forget every blob without destroying it.
    ///
    /// For a session pause, which revokes the descriptor the blobs were made
    /// on -- the kernel reclaimed them at that moment, and destroying through
    /// the replacement descriptor would at best fail and at worst free a
    /// different blob that happens to have the same id there.
    pub fn forget_for_session_loss(&mut self) {
        for slot in [&mut self.degamma, &mut self.ctm, &mut self.gamma] {
            if let Some(blob) = slot.take() {
                blob.forget();
            }
        }
    }

    fn require(&self, stage: Stage) -> Result<(), PipelineError> {
        let present = match stage {
            Stage::Degamma => self.capabilities.has_degamma_lut,
            Stage::Ctm => self.capabilities.has_ctm,
            Stage::Gamma => self.capabilities.has_gamma_lut,
        };
        if present {
            Ok(())
        } else {
            Err(PipelineError::NoSuchStage(stage))
        }
    }

    /// How many entries the driver reads for a LUT stage.
    ///
    /// `None` for the matrix, which does not take one.
    const fn lut_size(&self, stage: Stage) -> Option<u32> {
        match stage {
            Stage::Degamma => Some(self.capabilities.degamma_lut_size),
            Stage::Gamma => Some(self.capabilities.gamma_lut_size),
            Stage::Ctm => None,
        }
    }

    const fn property_id(&self, stage: Stage) -> Option<u32> {
        match stage {
            Stage::Degamma => self.properties.degamma,
            Stage::Ctm => self.properties.ctm,
            Stage::Gamma => self.properties.gamma,
        }
    }

    const fn slot(&self, stage: Stage) -> Option<&PropertyBlob<'a>> {
        match stage {
            Stage::Degamma => self.degamma.as_ref(),
            Stage::Ctm => self.ctm.as_ref(),
            Stage::Gamma => self.gamma.as_ref(),
        }
    }

    /// Replace a stage's blob, destroying whatever was there.
    ///
    /// Safe to do before the commit that switches the property over: the
    /// kernel holds its own reference to the blob a property currently names,
    /// so the one being replaced stays alive until nothing points at it.
    fn replace(&mut self, stage: Stage, blob: PropertyBlob<'a>) {
        let slot = match stage {
            Stage::Degamma => &mut self.degamma,
            Stage::Ctm => &mut self.ctm,
            Stage::Gamma => &mut self.gamma,
        };
        *slot = Some(blob);
    }

    fn set_lut(&mut self, stage: Stage, build: fn(&mut [ColorLut])) -> Result<(), PipelineError> {
        let size = self.lut_size(stage).ok_or(PipelineError::NotALut(stage))? as usize;
        let mut lut = vec![ColorLut::default(); size];
        build(&mut lut);
        let blob = self.device.create_property_blob(lut.as_slice())?;
        self.replace(stage, blob);
        Ok(())
    }

    fn set_ctm_blob(&mut self, ctm: ColorCtm) -> Result<(), PipelineError> {
        let blob = self.device.create_property_blob(&ctm)?;
        self.replace(Stage::Ctm, blob);
        Ok(())
    }
}
