// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! A [`ScanoutProducer`] backed by GBM.

use drmkit_core::Device;
use drmkit_scene::LayerBufferSource;
use drmkit_scene_sources::GbmBufferSource;

use crate::{ProducerError, ScanoutProducer};

/// Allocates scanout buffers straight from the DRM node's GBM device.
///
/// The plainest producer there is: no render API, no context, just an
/// allocation a caller can map or hand to a renderer. It is what the scanout
/// backend is exercised against, and the shape a GL or Vulkan producer fills
/// in around.
pub struct GbmScanoutProducer<'a> {
    device: &'a Device,
}

impl std::fmt::Debug for GbmScanoutProducer<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GbmScanoutProducer").finish_non_exhaustive()
    }
}

impl<'a> GbmScanoutProducer<'a> {
    /// Produce buffers from `device`'s GBM device.
    #[must_use]
    pub const fn new(device: &'a Device) -> Self {
        Self { device }
    }
}

/// The one layout every driver can allocate and every plane can scan out.
const LINEAR: u64 = 0;

impl ScanoutProducer for GbmScanoutProducer<'_> {
    /// `LINEAR`, and only `LINEAR`.
    ///
    /// Not a shortcut, and not a claim that the driver cannot do better: bare
    /// GBM has no way to ask *which layouts can this device export for
    /// scanout*. That question is answered by the render API on top of it --
    /// `EGL_EXT_image_dma_buf_import_modifiers` for GL, a format-properties
    /// query for Vulkan -- and a producer built on one of those overrides this
    /// with the real list.
    ///
    /// Reporting the one layout that is universally true is the honest answer
    /// for a producer that cannot ask. It costs bandwidth on hardware that
    /// could have compressed, and it never costs correctness.
    fn exportable_modifiers(&mut self, _fourcc: u32) -> Vec<u64> {
        vec![LINEAR]
    }

    fn create_buffer(
        &mut self,
        width: u32,
        height: u32,
        fourcc: u32,
        allowed: &[u64],
    ) -> Result<Box<dyn LayerBufferSource>, ProducerError> {
        let source = GbmBufferSource::create(self.device, width, height, fourcc, allowed)
            .map_err(|error| ProducerError::Allocate(error.to_string()))?;
        Ok(Box::new(source))
    }
}
