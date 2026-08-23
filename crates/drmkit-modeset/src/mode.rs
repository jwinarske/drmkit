// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! Mode inspection and selection.
//!
//! Port of `src/modeset/mode.{hpp,cpp}`.

use drm::control::{Mode, ModeFlags, ModeTypeFlags};

use crate::error::{ModesetError, Result};

/// Inspection helpers for a [`Mode`].
///
/// The C++ wraps `drmModeModeInfo` in a `ModeInfo` struct to hang accessors
/// off. The `drm` crate's [`Mode`] already is that wrapper, so this is an
/// extension trait rather than a second newtype — one less layer between a
/// caller and the kernel's mode.
pub trait ModeInfo {
    /// Horizontal active pixels (`hdisplay`).
    fn width(&self) -> u32;
    /// Vertical active lines (`vdisplay`).
    fn height(&self) -> u32;
    /// Vertical refresh in Hz (`vrefresh`).
    fn refresh(&self) -> u32;
    /// Whether the connector flags this mode `DRM_MODE_TYPE_PREFERRED`.
    fn preferred(&self) -> bool;
    /// Whether the mode is interlaced.
    fn interlaced(&self) -> bool;
    /// Pixel clock in kHz.
    fn clock_khz(&self) -> u32;
    /// Active pixel count, for "largest mode" comparisons.
    fn area(&self) -> u64;
}

impl ModeInfo for Mode {
    fn width(&self) -> u32 {
        u32::from(self.size().0)
    }

    fn height(&self) -> u32 {
        u32::from(self.size().1)
    }

    fn refresh(&self) -> u32 {
        self.vrefresh()
    }

    fn preferred(&self) -> bool {
        self.mode_type().contains(ModeTypeFlags::PREFERRED)
    }

    fn interlaced(&self) -> bool {
        self.flags().contains(ModeFlags::INTERLACE)
    }

    fn clock_khz(&self) -> u32 {
        self.clock()
    }

    fn area(&self) -> u64 {
        u64::from(self.width()) * u64::from(self.height())
    }
}

/// Pick the connector's preferred mode.
///
/// A mode flagged `DRM_MODE_TYPE_PREFERRED` wins outright. With none flagged —
/// which happens on panels whose EDID is absent or unparseable — the fallback
/// is the largest area, breaking ties on the higher refresh.
///
/// # Errors
///
/// [`ModesetError::NoModes`] if `modes` is empty.
///
/// ```
/// # use drmkit_modeset::{ModeInfo as _, select_preferred_mode};
/// # fn demo(modes: &[drm::control::Mode]) -> Result<(), drmkit_modeset::ModesetError> {
/// let mode = select_preferred_mode(modes)?;
/// println!("{}x{}@{}", mode.width(), mode.height(), mode.refresh());
/// # Ok(())
/// # }
/// ```
pub fn select_preferred_mode(modes: &[Mode]) -> Result<Mode> {
    if modes.is_empty() {
        return Err(ModesetError::NoModes);
    }

    if let Some(preferred) = modes.iter().find(|m| m.preferred()) {
        return Ok(*preferred);
    }

    // Largest area, then highest refresh. `max_by_key` returns the *last*
    // maximum, so comparing on (area, refresh) reproduces the C++ loop, which
    // replaces `best` only on a strict improvement and therefore keeps the
    // first of any tie... except that it also replaces on equal area with
    // higher refresh. Folding both into one key and taking the first maximum
    // gives the same answer.
    let best = modes
        .iter()
        .copied()
        .reduce(|best, m| {
            if (m.area(), m.refresh()) > (best.area(), best.refresh()) {
                m
            } else {
                best
            }
        })
        .ok_or(ModesetError::NoModes)?;

    Ok(best)
}

/// Pick the mode closest to a target resolution.
///
/// Scores by squared pixel distance from the target, with an optional refresh
/// term weighted at 100 per Hz. Interlaced modes are skipped: nothing in
/// drmkit asks for one, and silently selecting one produces a display that
/// looks broken in a way that is hard to trace back here.
///
/// `target_refresh` of 0 means "do not care".
///
/// # Errors
///
/// [`ModesetError::NoModes`] if `modes` is empty or contains only interlaced
/// modes.
pub fn select_mode(
    modes: &[Mode],
    target_width: u32,
    target_height: u32,
    target_refresh: u32,
) -> Result<Mode> {
    if modes.is_empty() {
        return Err(ModesetError::NoModes);
    }

    let score = |m: &Mode| -> u64 {
        let dw = u64::from(m.width().abs_diff(target_width));
        let dh = u64::from(m.height().abs_diff(target_height));
        let mut score = dw * dw + dh * dh;
        if target_refresh > 0 {
            score += u64::from(m.refresh().abs_diff(target_refresh)) * 100;
        }
        score
    };

    let mut best: Option<(Mode, u64)> = None;
    for mode in modes.iter().filter(|m| !m.interlaced()) {
        let candidate = score(mode);
        let replace = match best {
            None => true,
            // Strictly better, or an equal score at a higher refresh --
            // matching the C++ tie-break exactly.
            Some((current, current_score)) => {
                candidate < current_score
                    || (candidate == current_score && mode.refresh() > current.refresh())
            }
        };
        if replace {
            best = Some((*mode, candidate));
        }
    }

    best.map(|(mode, _)| mode).ok_or(ModesetError::NoModes)
}
