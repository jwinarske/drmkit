// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! The `INVARIANTS` linter.
//!
//! drm-cxx ships `INVARIANTS.md`: externally-relied-upon contracts, each with
//! its defining source location and pinning test. The port keeps its own file
//! with identical numbering and titles (plan §6). This linter fails CI when
//! the two diverge — an invariant added, removed, renumbered, or retitled
//! upstream is an API-level event for the port, and it must not land quietly.
//!
//! Only numbering and titles are compared. The bodies deliberately differ:
//! the Rust file names Rust source locations and Rust pinning tests.

use crate::refs;
use std::path::Path;

/// One `## N. Title` heading.
#[derive(Debug, PartialEq, Eq)]
struct Invariant {
    number: u32,
    title: String,
}

pub(crate) fn run(reference: Option<&str>) -> Result<(), String> {
    let root = refs::repo_root();
    let ours_path = root.join("INVARIANTS.md");
    let ours = parse(&refs::read(&ours_path)?, &ours_path)?;

    if ours.is_empty() {
        return Err(format!(
            "{} has no `## N. Title` invariant headings",
            ours_path.display()
        ));
    }

    let Some(cxx_root) = refs::resolve(reference)? else {
        return Err(format!("invariants: {}", refs::missing_reference_hint()));
    };

    let theirs_path = cxx_root.join("INVARIANTS.md");
    let theirs = parse(&refs::read(&theirs_path)?, &theirs_path)?;

    let mut problems = Vec::new();

    for upstream in &theirs {
        match ours.iter().find(|o| o.number == upstream.number) {
            None => problems.push(format!(
                "  invariant {} (`{}`) exists upstream but not in the port",
                upstream.number, upstream.title
            )),
            Some(local) if local.title != upstream.title => problems.push(format!(
                "  invariant {} title diverged:\n    upstream: {}\n    port:     {}",
                upstream.number, upstream.title, local.title
            )),
            Some(_) => {}
        }
    }

    for local in &ours {
        if !theirs.iter().any(|t| t.number == local.number) {
            problems.push(format!(
                "  invariant {} (`{}`) exists in the port but not upstream",
                local.number, local.title
            ));
        }
    }

    if !problems.is_empty() {
        return Err(format!(
            "INVARIANTS.md diverged from {}:\n{}",
            theirs_path.display(),
            problems.join("\n")
        ));
    }

    check_named_tests_exist(&refs::repo_root(), &refs::read(&ours_path)?)?;

    println!(
        "invariants: {} contracts, numbering and titles match upstream",
        ours.len()
    );
    Ok(())
}

/// Verify every test named in `INVARIANTS.md` actually exists in the tree.
///
/// The file records which test pins each contract. Nothing stopped those names
/// from going stale — an entry could keep saying "will be pinned by" long after
/// the pin landed, or name a test that was renamed away, and the numbering
/// check above would still pass. That happened: invariant 4's entry claimed its
/// pins were still pending through the whole phase that landed them.
///
/// A name is any `snake_case` identifier in backticks. Names ending in `_vkms`
/// that do not exist yet are allowed — those are the pins still to be written,
/// and the file says so in prose — but a name **without** that suffix must
/// resolve to a real `fn`, so a claimed pin cannot be imaginary.
fn check_named_tests_exist(root: &Path, markdown: &str) -> Result<(), String> {
    let sources = collect_rust_sources(root)?;
    let mut missing = Vec::new();

    for name in backticked_test_names(markdown) {
        if name.ends_with("_vkms") {
            continue; // not written yet, by design
        }
        // A pin is normally a `fn`, but one entry names a whole test module.
        let as_fn = format!("fn {name}(");
        let as_mod = format!("mod {name} ");
        let found = sources
            .iter()
            .any(|text| text.contains(&as_fn) || text.contains(&as_mod));
        if !found {
            missing.push(name);
        }
    }

    if missing.is_empty() {
        return Ok(());
    }
    missing.sort();
    missing.dedup();
    Err(format!(
        "INVARIANTS.md names tests that do not exist:\n{}\n\
         Either the pin was renamed, or the entry claims a pin that was never \
         written. A `_vkms` suffix marks one deliberately still to come.",
        missing
            .iter()
            .map(|name| format!("  {name}"))
            .collect::<Vec<_>>()
            .join("\n")
    ))
}

/// Test names claimed by the "pinned by" bullets.
///
/// Scoped to those bullets deliberately. An earlier version scanned the whole
/// file and matched prose identifiers -- `atomic_check`, `pthread_kill`,
/// `timeout_ms` -- none of which are tests. Only a bullet that *claims a pin*
/// is making a checkable assertion; everything else is explanation.
fn backticked_test_names(markdown: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut in_pin_bullet = false;

    for line in markdown.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("- **") {
            // A new bullet: is this one claiming a pin?
            in_pin_bullet = trimmed.contains("inned by:**");
        } else if trimmed.is_empty() || trimmed.starts_with("##") || trimmed.starts_with('>') {
            in_pin_bullet = false;
        } else if !line.starts_with("  ") {
            // Unindented prose ends the bullet.
            in_pin_bullet = false;
        }

        if !in_pin_bullet {
            continue;
        }
        for chunk in line.split('`').skip(1).step_by(2) {
            let looks_like_an_item = chunk.len() > 8
                && chunk.contains('_')
                && chunk
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
                && !chunk.starts_with('_')
                && !chunk.ends_with('_');
            if looks_like_an_item {
                names.push(chunk.to_owned());
            }
        }
    }
    names
}

/// Every `.rs` file under `crates/`, read once.
fn collect_rust_sources(root: &Path) -> Result<Vec<String>, String> {
    let mut sources = Vec::new();
    let mut stack = vec![root.join("crates")];

    while let Some(dir) = stack.pop() {
        let entries =
            std::fs::read_dir(&dir).map_err(|e| format!("reading `{}`: {e}", dir.display()))?;
        for entry in entries {
            let entry = entry.map_err(|e| format!("reading `{}`: {e}", dir.display()))?;
            let path = entry.path();
            if path.is_dir() {
                if path.file_name().is_some_and(|name| name == "target") {
                    continue;
                }
                stack.push(path);
            } else if path.extension().is_some_and(|ext| ext == "rs") {
                sources.push(refs::read(&path)?);
            }
        }
    }
    Ok(sources)
}

/// Parse `## <number>. <title>` headings, rejecting duplicate numbers.
fn parse(markdown: &str, path: &Path) -> Result<Vec<Invariant>, String> {
    let mut found: Vec<Invariant> = Vec::new();

    for (index, raw) in markdown.lines().enumerate() {
        let Some(rest) = raw.strip_prefix("## ") else {
            continue;
        };
        let Some((number_text, title)) = rest.split_once(". ") else {
            continue;
        };
        let Ok(number) = number_text.trim().parse::<u32>() else {
            continue;
        };

        let title = normalize(title);
        if let Some(previous) = found.iter().find(|i| i.number == number) {
            return Err(format!(
                "{} line {}: invariant {number} declared twice (`{}` and `{title}`)",
                path.display(),
                index + 1,
                previous.title
            ));
        }

        found.push(Invariant { number, title });
    }

    found.sort_by_key(|i| i.number);
    Ok(found)
}

/// Collapse whitespace so a reflowed heading is not reported as a divergence.
fn normalize(title: &str) -> String {
    title.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parsed(markdown: &str) -> Vec<Invariant> {
        parse(markdown, Path::new("<test>")).expect("parses")
    }

    #[test]
    fn parses_numbered_headings_only() {
        let doc = "\
# Title

Some prose.

## 1. Deferred buffer release

body

### 1.1 Not an invariant

## 2. A failed commit releases that frame's acquisitions
";
        let found = parsed(doc);
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].number, 1);
        assert_eq!(found[0].title, "Deferred buffer release");
        assert_eq!(found[1].number, 2);
    }

    #[test]
    fn normalizes_reflowed_titles() {
        let found = parsed("## 3. `PageFlip::dispatch`   never   returns\n");
        assert_eq!(found[0].title, "`PageFlip::dispatch` never returns");
    }

    #[test]
    fn rejects_duplicate_numbers() {
        let err = parse("## 1. First\n## 1. Second\n", Path::new("<test>"))
            .expect_err("duplicate numbers must fail");
        assert!(err.contains("declared twice"), "{err}");
    }

    #[test]
    fn orders_by_number_regardless_of_file_order() {
        let found = parsed("## 6. Sixth\n## 2. Second\n");
        assert_eq!(found[0].number, 2);
        assert_eq!(found[1].number, 6);
    }
}
