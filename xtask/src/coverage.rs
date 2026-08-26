// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! Which device cases actually asserted, and on which device.
//!
//! [`validate`](crate::validate) answers what a device *can* do. This answers
//! what was *checked* on it, which is the half that catches a case asserted on
//! every board and executed on none.
//!
//! A device case that cannot reach its hardware returns early and the harness
//! reports `ok`. `drmkit_testkit::skipped` makes that visible; this turns it
//! into something that can regress. The baseline records, per device, which
//! cases ran and which skipped and why. A case that ran before and skips now
//! is a failure -- coverage was lost, silently, and the count stayed green.
//!
//! A case that skipped before and skips now is fine: a board without a gamma
//! stage is not a board with a bug. What is *not* fine is that becoming true
//! without anyone noticing.
//!
//! Deliberately fed from stdin rather than running the suites itself. How
//! tests reach a device differs per device -- locally, over ssh to a board,
//! inside the vkms lane -- and none of that changes what coverage regression
//! means:
//!
//! ```text
//! cargo test --workspace -- --ignored --nocapture 2>&1 \
//!   | cargo xtask coverage --device=localhost-amdgpu --record
//! ```

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::io::Read as _;
use std::path::{Path, PathBuf};

/// What one case did on one device.
#[derive(Debug, PartialEq, Eq)]
enum Case {
    /// Reached its assertions.
    Ran,
    /// Bailed, with the reason the case gave.
    Skipped(String),
}

impl Case {
    fn reason(&self) -> &str {
        match self {
            Self::Ran => "ran",
            Self::Skipped(why) => why,
        }
    }
}

pub(crate) fn run(options: &[String]) -> Result<(), String> {
    let record = options.iter().any(|o| o == "--record");
    let device = options
        .iter()
        .find_map(|o| o.strip_prefix("--device="))
        .ok_or("coverage needs --device=<id>; see docs/validation.md")?;

    let mut input = String::new();
    std::io::stdin()
        .read_to_string(&mut input)
        .map_err(|error| format!("reading test output from stdin: {error}"))?;

    let observed = parse(&input);
    if observed.is_empty() {
        return Err("no `test <name> ... ok` lines on stdin; run the suites \
                    without --quiet, and remember --ignored for device cases"
            .to_owned());
    }

    let root = repo_root()?;
    let path = root
        .join("validation/coverage")
        .join(format!("{device}.json"));

    if record {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| format!("{}: {error}", parent.display()))?;
        }
        std::fs::write(&path, render(&observed))
            .map_err(|error| format!("{}: {error}", path.display()))?;
        let skipped = observed.values().filter(|c| **c != Case::Ran).count();
        println!(
            "recorded {} case(s) for {device}, {skipped} of them skipping ({})",
            observed.len(),
            path.display()
        );
        return Ok(());
    }

    let text = std::fs::read_to_string(&path).map_err(|_| {
        format!(
            "no coverage baseline for {device} at {}\n\n\
             Record one with --record once you are satisfied the run covered \
             what it should.",
            path.display()
        )
    })?;
    let baseline = parse_baseline(&text)?;

    compare(device, &baseline, &observed)
}

/// Read a harness run: which cases reported, and which of them bailed.
fn parse(input: &str) -> BTreeMap<String, Case> {
    let mut cases: BTreeMap<String, Case> = BTreeMap::new();

    for line in input.lines() {
        // `test <name> ... ok`. The name may carry a module path.
        if let Some(rest) = line.trim_start().strip_prefix("test ")
            && let Some(name) = rest.split(" ... ").next()
            && rest.contains(" ... ")
            && !name.is_empty()
        {
            let name = name.rsplit("::").next().unwrap_or(name);
            cases.entry(name.to_owned()).or_insert(Case::Ran);
        }

        // Not anchored at the line start on purpose: the harness prints its
        // progress dots without a newline, so every skip line after the first
        // begins with a `.` and an anchored match silently sees one of them.
        if let Some(at) = line.find(drmkit_testkit_marker()) {
            let mut fields = line[at..].splitn(3, '\t').skip(1);
            if let (Some(name), Some(reason)) = (fields.next(), fields.next()) {
                cases.insert(
                    name.trim().to_owned(),
                    Case::Skipped(reason.trim().to_owned()),
                );
            }
        }
    }
    cases
}

/// The marker, duplicated rather than depended on.
///
/// xtask is host tooling and drmkit-testkit is a dev-dependency of the test
/// suites; making the first depend on the second to share one string would
/// put a test-only crate in the tooling's build for no gain. The pairing is
/// pinned by `the_marker_matches_the_one_testkit_prints`.
const fn drmkit_testkit_marker() -> &'static str {
    "DRMKIT-SKIP"
}

fn render(cases: &BTreeMap<String, Case>) -> String {
    let mut out = String::from("{\n");
    let mut first = true;
    for (name, case) in cases {
        if !first {
            out.push_str(",\n");
        }
        first = false;
        let _ = write!(out, "  \"{name}\": \"{}\"", escape(case.reason()));
    }
    out.push_str("\n}\n");
    out
}

fn escape(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

/// The inverse of [`escape`].
///
/// A reason is free text written by whoever wrote the case, so it can contain
/// a quote -- `no "HOTSPOT_X" here` is the natural way to say that. Escaping
/// on the way out and not on the way back in turns the baseline into
/// something that compares unequal to itself.
fn unescape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some(next) => out.push(next),
                None => out.push('\\'),
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// The baseline is the same shape this writes, read back without a JSON crate.
fn parse_baseline(text: &str) -> Result<BTreeMap<String, Case>, String> {
    let mut cases = BTreeMap::new();
    for line in text.lines() {
        let line = line.trim().trim_end_matches(',');
        let Some(rest) = line.strip_prefix('"') else {
            continue;
        };
        let Some((name, value)) = rest.split_once("\": \"") else {
            continue;
        };
        // Up to the last quote, not the first: the reason may contain an
        // escaped one, and cutting at the first truncates it mid-word.
        let value = match value.rfind('"') {
            Some(end) => &value[..end],
            None => value,
        };
        let value = unescape(value);
        let case = if value == "ran" {
            Case::Ran
        } else {
            Case::Skipped(value)
        };
        cases.insert(unescape(name), case);
    }
    if cases.is_empty() {
        return Err("the coverage baseline has no cases in it".to_owned());
    }
    Ok(cases)
}

fn compare(
    device: &str,
    baseline: &BTreeMap<String, Case>,
    observed: &BTreeMap<String, Case>,
) -> Result<(), String> {
    let mut lost = Vec::new();
    let mut vanished = Vec::new();
    let mut regained = Vec::new();
    let mut added = Vec::new();

    for (name, was) in baseline {
        match observed.get(name) {
            // The failure this exists for: it asserted here before, and now it
            // does not, while the harness goes on reporting `ok`.
            Some(Case::Skipped(why)) if *was == Case::Ran => {
                lost.push(format!("  {name}\n      now skipping: {why}"));
            }
            Some(Case::Ran) if *was != Case::Ran => {
                regained.push(format!("  {name} (was: {})", was.reason()));
            }
            // A case that stopped being reported at all is coverage lost too,
            // and it is what a renamed or deleted case looks like.
            None => vanished.push(format!("  {name} (was: {})", was.reason())),
            Some(_) => {}
        }
    }
    for name in observed.keys() {
        if !baseline.contains_key(name) {
            added.push(format!("  {name}"));
        }
    }

    for (title, rows) in [("newly covered", &regained), ("new cases", &added)] {
        if !rows.is_empty() {
            println!("{device}: {title}:\n{}", rows.join("\n"));
        }
    }

    if lost.is_empty() && vanished.is_empty() {
        let skipped = observed.values().filter(|c| **c != Case::Ran).count();
        println!(
            "{device}: {} case(s) reported, {skipped} skipping, none lost",
            observed.len()
        );
        return Ok(());
    }

    let mut message = format!("{device}: coverage regressed\n");
    if !lost.is_empty() {
        let _ = write!(
            message,
            "\nThese asserted on this device before and no longer do:\n{}\n",
            lost.join("\n")
        );
    }
    if !vanished.is_empty() {
        let _ = write!(
            message,
            "\nThese are no longer reported at all -- renamed, deleted, or not \
             run:\n{}\n",
            vanished.join("\n")
        );
    }
    message.push_str(
        "\nIf the device genuinely changed -- a kernel that dropped a \
         property, hardware\nswapped out -- re-record with --record. Do that \
         deliberately: it is the same\ngesture as accepting that the case now \
         checks nothing here.",
    );
    Err(message)
}

fn repo_root() -> Result<PathBuf, String> {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest_dir
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| "xtask has no parent directory".to_owned())
}

#[cfg(test)]
mod tests {
    use super::{Case, compare, parse, parse_baseline, render};

    /// The harness writes its progress dots without a newline, so every skip
    /// line after the first starts with a `.` rather than the marker. An
    /// anchored parser sees exactly one of five.
    #[test]
    fn a_skip_line_behind_a_progress_dot_is_still_read() {
        let input = "\
test a_first_case ... ok
DRMKIT-SKIP\ta_first_case\tno DRM device could be opened
test a_second_case ... ok
.DRMKIT-SKIP\ta_second_case\tthis CRTC has no gamma stage
";
        let cases = parse(input);
        assert_eq!(cases.len(), 2);
        assert_eq!(
            cases["a_second_case"].reason(),
            "this CRTC has no gamma stage",
            "the dot must not hide it"
        );
    }

    #[test]
    fn a_case_with_no_skip_line_counts_as_having_run() {
        let cases = parse("test a_case_that_asserted ... ok\n");
        assert_eq!(cases["a_case_that_asserted"], Case::Ran);
    }

    #[test]
    fn losing_an_assertion_is_a_failure_and_says_which() {
        let baseline = parse_baseline("{\n  \"a_case\": \"ran\"\n}\n").expect("baseline");
        let observed = parse("test a_case ... ok\nDRMKIT-SKIP\ta_case\tno cursor plane\n");
        let error = compare("board", &baseline, &observed).expect_err("must fail");
        assert!(error.contains("a_case"), "names the case");
        assert!(error.contains("no cursor plane"), "and why it stopped");
    }

    /// A case that never asserted here is not a regression -- only a change is.
    #[test]
    fn a_case_that_was_already_skipping_is_not_a_regression() {
        let baseline =
            parse_baseline("{\n  \"a_case\": \"this driver has no HOTSPOT_X\"\n}\n").expect("b");
        let observed =
            parse("test a_case ... ok\nDRMKIT-SKIP\ta_case\tthis driver has no HOTSPOT_X\n");
        compare("board", &baseline, &observed).expect("not a regression");
    }

    /// A renamed or deleted case stops being reported, which is coverage lost
    /// just as surely as one that started skipping.
    #[test]
    fn a_case_that_stopped_being_reported_is_a_failure() {
        let baseline = parse_baseline("{\n  \"a_case\": \"ran\"\n}\n").expect("baseline");
        let observed = parse("test a_different_case ... ok\n");
        let error = compare("board", &baseline, &observed).expect_err("must fail");
        assert!(error.contains("no longer reported"));
    }

    #[test]
    fn a_reason_with_a_quote_survives_the_round_trip() {
        let mut cases = std::collections::BTreeMap::new();
        cases.insert(
            "a_case".to_owned(),
            Case::Skipped("no \"HOTSPOT_X\" here".to_owned()),
        );
        let back = parse_baseline(&render(&cases)).expect("round trip");
        assert_eq!(back["a_case"].reason(), "no \"HOTSPOT_X\" here");
    }

    #[test]
    fn the_marker_matches_the_one_testkit_prints() {
        assert_eq!(super::drmkit_testkit_marker(), drmkit_testkit::MARKER);
    }
}
