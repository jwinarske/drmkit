// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! Check a lane's path filter still covers what its job reads.
//!
//! The drmdb lane replays drmkit's own logic over the corpus, so a change to
//! a crate the scanner depends on can change what it reports.
//! `pull_request.paths` is what decides whether that gets checked before
//! merge, and a filter that has fallen behind the dependencies fails in the
//! quietest way there is: by not running.
//!
//! That already happened. `drmkit-cursor` became a dependency of the scanner
//! in the same change that added the cursor survey, and the filter was not
//! updated -- so the survey that found P-23 would not have re-run on a pull
//! request touching the crate it was about.

use std::path::Path;

/// The lane, and the manifest whose dependencies it has to cover.
const WORKFLOW: &str = ".github/workflows/drmdb.yml";
const MANIFEST: &str = "validation/drmdb/Cargo.toml";

pub(crate) fn run() -> Result<(), String> {
    let root = repository_root()?;
    let manifest = read(&root.join(MANIFEST))?;
    let workflow = read(&root.join(WORKFLOW))?;

    let mut deps: Vec<&str> = manifest
        .lines()
        .filter_map(|line| {
            let name = line.split('=').next()?.trim();
            (name.starts_with("drmkit-") && line.contains('=')).then_some(name)
        })
        .collect();
    deps.sort_unstable();
    deps.dedup();
    if deps.is_empty() {
        return Err(format!(
            "{MANIFEST} lists no drmkit dependencies; has it moved?"
        ));
    }

    let listed: Vec<&str> = workflow
        .lines()
        .filter_map(|line| {
            let line = line.trim().strip_prefix("- ")?;
            // Both quote styles, and none. YAML accepts all three, and a
            // linter that recognised only the spelling in use today would
            // report a reformat as a missing path -- which is worse than not
            // checking, because it is a failure that is right to ignore.
            let path = line.trim_matches(|c| c == '\'' || c == '"');
            path.strip_prefix("crates/")?.strip_suffix("/**")
        })
        .collect();

    let missing: Vec<&str> = deps
        .iter()
        .copied()
        .filter(|dep| !listed.contains(dep))
        .collect();

    if missing.is_empty() {
        println!(
            "lanes: {WORKFLOW} covers all {} crate(s) the scanner depends on",
            deps.len()
        );
        return Ok(());
    }

    let list = missing
        .iter()
        .map(|name| format!("      - 'crates/{name}/**'"))
        .collect::<Vec<_>>()
        .join("\n");
    Err(format!(
        "{WORKFLOW} does not run on changes to crates the scanner reads:\n{list}\n\n\
         Add them under `pull_request.paths`, or the lane stays silent on \
         exactly the\npull requests it exists to check."
    ))
}

fn read(path: &Path) -> Result<String, String> {
    std::fs::read_to_string(path).map_err(|error| format!("{}: {error}", path.display()))
}

/// The workspace root, from this crate's manifest directory.
fn repository_root() -> Result<std::path::PathBuf, String> {
    let manifest_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest_dir
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| "xtask has no parent directory".to_owned())
}
