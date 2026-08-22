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

    println!(
        "invariants: {} contracts, numbering and titles match upstream",
        ours.len()
    );
    Ok(())
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
