// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! Mode selection and page-flip dispatch for drmkit.
//!
//! Port of `src/modeset/` from drm-cxx @ `dc2915b`, less `atomic.*`, which
//! lives in `drmkit-core` alongside the property cache it is built from.
//!
//! # Invariant 3
//!
//! [`PageFlip::dispatch`] never reports an interrupted wait. `EINTR` is
//! retried internally, and a bounded wait recomputes its remaining budget from
//! a monotonic clock across retries, so a signal storm cannot stretch the wait
//! past what was asked for. See that method for the Rust-specific hazard this
//! guards against.

mod error;
mod mode;
mod page_flip;

pub use error::{ModesetError, Result};
pub use mode::{ModeInfo, select_mode, select_preferred_mode};
pub use page_flip::{Handler, PageFlip, PageFlipEvent, SourceCallback, Timeout};

#[cfg(test)]
mod tests;
