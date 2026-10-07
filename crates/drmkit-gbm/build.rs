// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! Choose the modifier entry points the target's libgbm actually exports.
//!
//! `gbm_bo_create_with_modifiers2` and `gbm_surface_create_with_modifiers2`
//! are Mesa 21.3. Older Mesa and vendor implementations have the v1 pair only
//! -- the SA8155P's exports no `*2` symbol at all -- and a binary referencing
//! one does not link there, let alone run. Upstream decides this at configure
//! time (`HAVE_GBM_BO_CREATE_WITH_MODIFIERS2`); the equivalent here is
//! `DRMKIT_GBM_NO_MODIFIERS2=1`, which builds against v1 instead.
//!
//! An environment switch rather than a Cargo feature because it describes the
//! target, not the API: a feature would be unified across the dependency graph,
//! and any crate asking for the default would put the missing symbol back.

fn main() {
    println!("cargo::rerun-if-env-changed=DRMKIT_GBM_NO_MODIFIERS2");
    println!("cargo::rustc-check-cfg=cfg(drmkit_gbm_v1)");
    if std::env::var("DRMKIT_GBM_NO_MODIFIERS2").is_ok_and(|value| value == "1") {
        println!("cargo::rustc-cfg=drmkit_gbm_v1");
    }
}
