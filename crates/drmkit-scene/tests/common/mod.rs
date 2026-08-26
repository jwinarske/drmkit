// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! Device selection shared by the on-device lanes.
//!
//! The rule itself lives in `drmkit_testkit::crtc`, because `drmkit-planes`
//! needs the same answer and two copies would drift -- and the whole point of
//! it is that every suite measures the same CRTC.

pub(crate) use drmkit_testkit::crtc::pick_crtc;
