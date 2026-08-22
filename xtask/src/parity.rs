// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! The `TEST_PARITY` linter.
//!
//! `TEST_PARITY.md` holds a machine-checked table mapping every drm-cxx test
//! file to its Rust counterpart and porting status. This linter enforces that
//! the table and the upstream tree agree in both directions:
//!
//! - an upstream test file with no table row fails CI, so a test appearing
//!   upstream during the port surfaces as red rather than as silence;
//! - a table row naming a file that no longer exists upstream fails too, so a
//!   renamed or deleted upstream test does not leave a stale mapping behind.

use crate::refs;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

/// Porting status of one upstream test file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Status {
    /// Fully ported; the named Rust tests cover it.
    Ported,
    /// Some cases ported, some outstanding.
    Partial,
    /// Not started. Every row starts here.
    Pending,
    /// Deliberately not ported — the C++ construct has no Rust analogue
    /// (e.g. the C++17-polyfill / dual-std machinery, plan DR-8).
    NotApplicable,
}

impl Status {
    fn parse(text: &str) -> Option<Self> {
        match text.trim().to_ascii_lowercase().as_str() {
            "ported" => Some(Self::Ported),
            "partial" => Some(Self::Partial),
            "pending" => Some(Self::Pending),
            "n/a" | "na" | "not applicable" => Some(Self::NotApplicable),
            _ => None,
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Self::Ported => "ported",
            Self::Partial => "partial",
            Self::Pending => "pending",
            Self::NotApplicable => "n/a",
        }
    }
}

/// One parsed row of the table.
#[derive(Debug)]
struct Row {
    cxx_path: String,
    status: Status,
    line: usize,
}

pub(crate) fn run(reference: Option<&str>) -> Result<(), String> {
    let root = refs::repo_root();
    let table_path = root.join("TEST_PARITY.md");
    let table = refs::read(&table_path)?;
    let rows = parse_table(&table)?;

    if rows.is_empty() {
        return Err(format!(
            "TEST_PARITY.md ({}) has no table rows — the linter would pass vacuously",
            table_path.display()
        ));
    }

    let Some(cxx_root) = refs::resolve(reference)? else {
        return Err(format!("parity: {}", refs::missing_reference_hint()));
    };

    let upstream = collect_upstream_tests(&cxx_root)?;
    let mut problems = Vec::new();

    // Duplicate rows would let one of them drift unnoticed.
    let mut seen: BTreeMap<&str, usize> = BTreeMap::new();
    for row in &rows {
        if let Some(first) = seen.insert(&row.cxx_path, row.line) {
            problems.push(format!(
                "  duplicate row for `{}` (lines {first} and {})",
                row.cxx_path, row.line
            ));
        }
    }

    let mapped: BTreeSet<&str> = rows.iter().map(|r| r.cxx_path.as_str()).collect();

    for path in &upstream {
        if !mapped.contains(path.as_str()) {
            problems.push(format!(
                "  upstream test `{path}` has no TEST_PARITY.md row (add one, status `pending`)"
            ));
        }
    }

    for row in &rows {
        if !upstream.contains(&row.cxx_path) {
            problems.push(format!(
                "  TEST_PARITY.md line {} maps `{}`, which no longer exists upstream",
                row.line, row.cxx_path
            ));
        }
    }

    if !problems.is_empty() {
        problems.sort();
        return Err(format!(
            "TEST_PARITY.md is out of sync with {}:\n{}",
            cxx_root.display(),
            problems.join("\n")
        ));
    }

    let mut counts: BTreeMap<Status, usize> = BTreeMap::new();
    for row in &rows {
        *counts.entry(row.status).or_default() += 1;
    }
    let summary = counts
        .iter()
        .map(|(status, n)| format!("{n} {}", status.label()))
        .collect::<Vec<_>>()
        .join(", ");

    println!(
        "parity: {} upstream test files, all mapped ({summary})",
        rows.len()
    );
    Ok(())
}

/// Parse the pipe table. A row counts when its first cell holds a backticked
/// path under `tests/`; the header, separator, and prose are skipped by that
/// rule alone, so the file stays free to carry explanation around the table.
fn parse_table(markdown: &str) -> Result<Vec<Row>, String> {
    let mut rows = Vec::new();

    for (index, raw) in markdown.lines().enumerate() {
        let line = index + 1;
        let trimmed = raw.trim();
        if !trimmed.starts_with('|') {
            continue;
        }

        let cells: Vec<&str> = trimmed
            .trim_matches('|')
            .split('|')
            .map(str::trim)
            .collect();
        if cells.len() < 3 {
            continue;
        }

        let Some(cxx_path) = backticked(cells[0]) else {
            continue;
        };
        if !cxx_path.starts_with("tests/") {
            continue;
        }

        let status = Status::parse(cells[2]).ok_or_else(|| {
            format!(
                "TEST_PARITY.md line {line}: unknown status `{}` for `{cxx_path}` \
                 (expected ported/partial/pending/n/a)",
                cells[2]
            )
        })?;

        rows.push(Row {
            cxx_path,
            status,
            line,
        });
    }

    Ok(rows)
}

/// Extract the contents of the first backtick pair in a cell.
fn backticked(cell: &str) -> Option<String> {
    let start = cell.find('`')? + 1;
    let rest = cell.get(start..)?;
    let end = rest.find('`')?;
    Some(rest[..end].to_owned())
}

/// Every `.cpp` under `tests/unit` and `tests/integration`, as paths relative
/// to the drm-cxx root.
fn collect_upstream_tests(cxx_root: &Path) -> Result<BTreeSet<String>, String> {
    let mut found = BTreeSet::new();

    for subdir in ["unit", "integration"] {
        let dir = cxx_root.join("tests").join(subdir);
        let entries =
            std::fs::read_dir(&dir).map_err(|e| format!("reading `{}`: {e}", dir.display()))?;

        for entry in entries {
            let entry = entry.map_err(|e| format!("reading `{}`: {e}", dir.display()))?;
            let path = entry.path();
            if path.extension().is_some_and(|ext| ext == "cpp")
                && let Some(name) = path.file_name().and_then(|n| n.to_str())
            {
                found.insert(format!("tests/{subdir}/{name}"));
            }
        }
    }

    if found.is_empty() {
        return Err(format!(
            "no test files found under `{}/tests` — is that a drm-cxx checkout?",
            cxx_root.display()
        ));
    }

    Ok(found)
}
