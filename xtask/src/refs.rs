// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! Locating the drm-cxx reference tree and the drmkit repository root.

use std::path::{Path, PathBuf};

/// Extract `--ref <PATH>` from an argument list.
pub(crate) fn parse_ref_option(args: &[String]) -> Result<Option<String>, String> {
    let mut iter = args.iter();
    let mut found = None;

    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--ref" => {
                let value = iter
                    .next()
                    .ok_or_else(|| "`--ref` needs a path argument".to_owned())?;
                found = Some(value.clone());
            }
            other if other.starts_with("--ref=") => {
                found = Some(other["--ref=".len()..].to_owned());
            }
            other => return Err(format!("unexpected argument `{other}`")),
        }
    }

    Ok(found)
}

/// The drmkit repository root, derived from this crate's manifest directory.
///
/// `CARGO_MANIFEST_DIR` points at `xtask/`, so the root is its parent. This
/// keeps the linters working regardless of the caller's working directory.
///
/// # Panics
///
/// If `CARGO_MANIFEST_DIR` is unset (i.e. xtask was not run under cargo) or
/// has no parent. Both mean the tool was invoked in a way it does not support.
pub(crate) fn repo_root() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest
        .parent()
        .expect("xtask manifest dir always has a parent")
        .to_path_buf()
}

/// Resolve the drm-cxx checkout from the flag, then the environment.
///
/// Returns `Ok(None)` when neither is set — callers decide whether that is a
/// hard error or a skip, because the invariants linter can still check the
/// local file's shape without a reference tree.
pub(crate) fn resolve(explicit: Option<&str>) -> Result<Option<PathBuf>, String> {
    let candidate = explicit
        .map(ToOwned::to_owned)
        .or_else(|| std::env::var("DRMKIT_DRM_CXX").ok());

    let Some(raw) = candidate else {
        return Ok(None);
    };

    let path = PathBuf::from(raw);
    if !path.is_dir() {
        return Err(format!(
            "drm-cxx reference `{}` is not a directory",
            path.display()
        ));
    }
    if !path.join("INVARIANTS.md").is_file() {
        return Err(format!(
            "`{}` does not look like a drm-cxx checkout (no INVARIANTS.md)",
            path.display()
        ));
    }

    Ok(Some(path))
}

/// Message shown when a linter needs the reference tree and does not have it.
pub(crate) fn missing_reference_hint() -> String {
    "no drm-cxx reference tree. Pass `--ref <PATH>` or set DRMKIT_DRM_CXX.".to_owned()
}

/// Read a file, attributing the path in the error.
pub(crate) fn read(path: &Path) -> Result<String, String> {
    std::fs::read_to_string(path).map_err(|e| format!("reading `{}`: {e}", path.display()))
}
