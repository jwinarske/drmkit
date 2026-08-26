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
#[derive(Debug, Clone, PartialEq, Eq)]
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
    if options.iter().any(|o| o == "--audit") {
        return audit();
    }
    let record = options.iter().any(|o| o == "--record");
    let device = options
        .iter()
        .find_map(|o| o.strip_prefix("--device="))
        .ok_or("coverage needs --device=<id>; see docs/validation.md")?;

    let mut input = String::new();
    std::io::stdin()
        .read_to_string(&mut input)
        .map_err(|error| format!("reading test output from stdin: {error}"))?;

    let mut observed = parse(&input);
    let announced = announced_driver(&input);
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
        environment_is_fit_to_record(&observed)?;
        if let Some(driver) = &announced {
            observed.insert(DEVICE_KEY.to_owned(), Case::Skipped(driver.clone()));
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| format!("{}: {error}", parent.display()))?;
        }
        std::fs::write(&path, render(&observed))
            .map_err(|error| format!("{}: {error}", path.display()))?;
        // The device key rides in the same map so it round-trips with the
        // baseline, but it is metadata: counting it as a skipped case
        // overstates both numbers by one.
        let cases = observed
            .iter()
            .filter(|(name, _)| name.as_str() != DEVICE_KEY);
        let total = cases.clone().count();
        let skipped = cases.filter(|(_, case)| **case != Case::Ran).count();
        println!(
            "recorded {total} case(s) for {device}, {skipped} of them skipping ({})",
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

    // The mistake this prevents: a run against the wrong card compared against
    // a baseline recorded from the right one. Both are green, and the label is
    // the only thing that was ever wrong.
    if let (Some(Case::Skipped(was)), Some(now)) = (baseline.get(DEVICE_KEY), &announced)
        && was != now
    {
        return Err(format!(
            "{device}: this run is from a different device than the baseline\n\n  \
             baseline: {was}\n  this run: {now}\n\n\
             Set DRMKIT_TEST_CARD to the card this device's baseline was taken \
             from, or\nrecord a new baseline for the card you meant."
        ));
    }

    compare(device, &baseline, &observed)
}

/// The case name from a `test <name> ... ` report, wherever it sits.
///
/// A name is a Rust path, so it has no spaces; that is what tells a real
/// report from the words "test " appearing in prose. Scans every candidate
/// rather than the first, since the line may open with something else
/// entirely.
fn test_report(line: &str) -> Option<String> {
    let mut from = 0;
    while let Some(at) = line[from..].find("test ") {
        let start = from + at + "test ".len();
        from = start;
        let Some(end) = line[start..].find(" ... ") else {
            continue;
        };
        // The harness annotates a `#[should_panic]` case as
        // `test <name> - should panic ... ok`. That is the harness talking
        // about the case, not part of its name, and carrying it into the
        // baseline makes the identity depend on an attribute.
        let name = line[start..start + end]
            .strip_suffix(" - should panic")
            .unwrap_or(&line[start..start + end]);
        // A name is a Rust path, so it has no spaces left once the annotation
        // is off. This is what separates a report from the words "test "
        // appearing in prose.
        if name.is_empty() || name.contains(char::is_whitespace) {
            continue;
        }
        return Some(name.rsplit("::").next().unwrap_or(name).to_owned());
    }
    None
}

/// The card and driver a run announced, if it announced one.
///
/// Recorded in the baseline under a key no case name can collide with, since
/// a case name is a Rust identifier and cannot contain a space.
const DEVICE_KEY: &str = "the driver this was recorded from";

fn announced_driver(input: &str) -> Option<String> {
    input.lines().find_map(|line| {
        let at = line.find("DRMKIT-DEVICE")?;
        let mut fields = line[at..].splitn(3, '\t').skip(1);
        let path = fields.next()?.trim();
        let driver = fields.next()?.trim();
        Some(format!("{driver} at {path}"))
    })
}

/// Read a harness run: which cases reported, and which of them bailed.
fn parse(input: &str) -> BTreeMap<String, Case> {
    let mut cases: BTreeMap<String, Case> = BTreeMap::new();

    for line in input.lines() {
        // `test <name> ... ok`, found anywhere in the line rather than at its
        // start.
        //
        // Under `--nocapture` the harness's own output and the library's log
        // share a descriptor with no locking between them, so a log line lands
        // mid-report:
        //
        //     [drm:test tests::a_resume_without_a_pause_changes_nothing ... ok
        //
        // That is a real line from the vkms lane. Anchored at the start it
        // parses as nothing, the case vanishes from the run, and the check
        // calls it coverage lost -- a false failure about a test that ran and
        // passed. Same lesson as the skip marker, which was already unanchored
        // for the progress dots.
        if let Some(name) = test_report(line) {
            cases.entry(name).or_insert(Case::Ran);
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
    let baseline: BTreeMap<String, Case> = baseline
        .iter()
        .filter(|(name, _)| name.as_str() != DEVICE_KEY)
        .map(|(name, case)| (name.clone(), case.clone()))
        .collect();
    let observed: BTreeMap<String, Case> = observed
        .iter()
        .filter(|(name, _)| name.as_str() != DEVICE_KEY)
        .map(|(name, case)| (name.clone(), case.clone()))
        .collect();
    let (baseline, observed) = (&baseline, &observed);
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

/// Skips that describe the moment rather than the device.
///
/// A baseline is a claim about hardware: this board has no gamma stage, that
/// one has nine planes. A skip caused by whatever else was running is not
/// that, and recording one bakes a passing excuse into the baseline -- the
/// case then reports `ok`, skips forever, and the check agrees it should.
///
/// Found by making the mistake. A run against amdgpu with a desktop session
/// on it skipped 43 of 118 cases for want of DRM master, and every one would
/// have been recorded as something that device cannot do.
const ENVIRONMENTAL: [(&str, &str); 2] = [
    (
        "holds DRM master",
        "stop the compositor, or run from a console: the card is in use",
    ),
    ("needs root", "re-run as root; these cases write to sysfs"),
];

/// Refuse to record a baseline that is mostly about the machine's mood.
fn environment_is_fit_to_record(observed: &BTreeMap<String, Case>) -> Result<(), String> {
    let mut blocked: BTreeMap<&str, (usize, &str)> = BTreeMap::new();
    for case in observed.values() {
        let Case::Skipped(why) = case else { continue };
        for (needle, remedy) in ENVIRONMENTAL {
            if why.contains(needle) {
                let entry = blocked.entry(needle).or_insert((0, remedy));
                entry.0 += 1;
            }
        }
    }
    if blocked.is_empty() {
        return Ok(());
    }

    let mut message = String::from(
        "refusing to record: some cases skipped for reasons that are about \
         this run, not\nthis device\n",
    );
    for (needle, (count, remedy)) in &blocked {
        let _ = write!(message, "\n  {count} case(s): \"{needle}\"\n      {remedy}");
    }
    let _ = write!(
        &mut message,
        "\n\nRecording these would enshrine them: the case reports ok, skips \
         forever, and\nthe check agrees that it should. Fix the run and \
         record again."
    );
    Err(message)
}

/// One device's recorded coverage: its id, and what each case did on it.
type DeviceCoverage = (String, BTreeMap<String, Case>);

/// Every recorded baseline, by device.
fn load_baselines() -> Result<Vec<DeviceCoverage>, String> {
    let root = repo_root()?;
    let dir = root.join("validation/coverage");
    let entries = std::fs::read_dir(&dir).map_err(|error| format!("{}: {error}", dir.display()))?;

    let mut devices: Vec<DeviceCoverage> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_none_or(|ext| ext != "json") {
            continue;
        }
        let name = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let text = std::fs::read_to_string(&path)
            .map_err(|error| format!("{}: {error}", path.display()))?;
        devices.push((name, parse_baseline(&text)?));
    }
    if devices.is_empty() {
        return Err(format!(
            "no coverage baselines under {}; record one with --record",
            dir.display()
        ));
    }
    devices.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(devices)
}

/// What the pool covers, split into the three answers that differ.
struct Classified<'a> {
    /// Every case any device reported, and what each said about it.
    known: BTreeMap<&'a str, Vec<(&'a str, &'a str)>>,
    /// Asserted on one device and *skipped* on another: real single coverage.
    narrow: Vec<(&'a str, &'a str)>,
    /// Asserted on one device and not run at all elsewhere -- the run's scope
    /// rather than the pool's, so counted and not listed as a risk.
    unmeasured: usize,
    /// Skipped everywhere it was tried. These assert nothing, anywhere.
    nowhere: Vec<&'a str>,
}

fn classify(devices: &[DeviceCoverage]) -> Classified<'_> {
    let mut ran_somewhere: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    let mut known: BTreeMap<&str, Vec<(&str, &str)>> = BTreeMap::new();
    for (device, cases) in devices {
        for (name, case) in cases.iter().filter(|(name, _)| name.as_str() != DEVICE_KEY) {
            known
                .entry(name.as_str())
                .or_default()
                .push((device.as_str(), case.reason()));
            if *case == Case::Ran {
                ran_somewhere
                    .entry(name.as_str())
                    .or_default()
                    .push(device.as_str());
            }
        }
    }

    let nowhere: Vec<&str> = known
        .keys()
        .copied()
        .filter(|name| !ran_somewhere.contains_key(name))
        .collect();

    let mut narrow = Vec::new();
    let mut unmeasured = 0;
    for (name, on) in ran_somewhere.iter().filter(|(_, on)| on.len() == 1) {
        // Skipped elsewhere is a real single point of coverage. Absent
        // elsewhere -- a crate that does not cross-build for that target --
        // is a gap in the run, and calling it a risk would cry wolf.
        if known[name].len() > 1 {
            narrow.push((*name, on[0]));
        } else if devices.len() > 1 {
            unmeasured += 1;
        }
    }

    Classified {
        known,
        narrow,
        unmeasured,
        nowhere,
    }
}

/// Which cases assert on no device at all.
///
/// The question a single device cannot answer. A case that skips on every
/// board in the pool is an assertion that never runs anywhere -- the shape
/// P-13 and P-14 already have, written down rather than discovered by
/// accident a year later.
///
/// It reads the recorded baselines, so it describes the pool as last
/// measured. A device whose baseline is missing is a device this cannot speak
/// for, and it says so rather than treating absence as coverage.
fn audit() -> Result<(), String> {
    let devices = load_baselines()?;

    let Classified {
        known,
        narrow,
        unmeasured,
        nowhere,
    } = classify(&devices);

    println!(
        "{} device(s): {}",
        devices.len(),
        devices
            .iter()
            .map(|(name, cases): &DeviceCoverage| {
                let n = cases.keys().filter(|k| k.as_str() != DEVICE_KEY).count();
                format!("{name} ({n} cases)")
            })
            .collect::<Vec<_>>()
            .join(", ")
    );

    report_narrow(&narrow, unmeasured);

    if nowhere.is_empty() {
        println!(
            "\n{} case(s) known, every one of them asserted on some device",
            known.len()
        );
        return Ok(());
    }

    let mut message = String::from("assertions that run on no device in the pool\n");
    for name in &nowhere {
        let _ = write!(message, "\n  {name}");
        for (device, why) in &known[name] {
            let _ = write!(message, "\n      {device}: {why}");
        }
    }
    let _ = write!(
        &mut message,
        "\n\nEach of these is a case that has never checked anything. Either the \
         pool is\nmissing the hardware it needs -- add it to \
         validation/devices.toml and record a\nrun -- or the case is asserting \
         something no device does, which is a finding\nabout the case rather \
         than about the hardware."
    );
    Err(message)
}

/// Say which cases rest on a single device, and how many were simply not run.
fn report_narrow(narrow: &[(&str, &str)], unmeasured: usize) {
    if !narrow.is_empty() {
        println!(
            "\nAsserted on one device and skipped on the others -- lose it and \
             these stop running:"
        );
        for (name, device) in narrow {
            println!("  {name}\n      only on {device}");
        }
    }
    if unmeasured > 0 {
        println!(
            "\n{unmeasured} case(s) ran on one device and were not run at all \
             on the others.\nThat is the run's scope, not the pool's: a crate \
             that does not cross-build\nfor a target never reports there. Run \
             `--audit` again once every device has\na baseline covering the \
             same crates."
        );
    }
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

    /// The skip line can land either side of the case's own `test ... ok`.
    ///
    /// With `--nocapture` the case prints while it runs, so its skip reaches
    /// the log before the harness reports it; across binaries the order can
    /// go the other way. Whichever arrives second must not undo the first.
    #[test]
    fn a_skip_is_recorded_whichever_side_of_its_test_line_it_lands() {
        let before = parse("DRMKIT-SKIP\ta_case\tno gamma stage\ntest a_case ... ok\n");
        assert_eq!(before["a_case"].reason(), "no gamma stage", "skip first");

        let after = parse("test a_case ... ok\nDRMKIT-SKIP\ta_case\tno gamma stage\n");
        assert_eq!(after["a_case"].reason(), "no gamma stage", "skip second");
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

    /// A log line landing mid-report must not lose the case.
    ///
    /// Verbatim from the vkms lane, where it made the check report a passing
    /// test as coverage lost and failed the run.
    #[test]
    fn a_test_report_interleaved_with_a_log_line_is_still_read() {
        let cases = parse("[drm:test tests::a_resume_without_a_pause_changes_nothing ... ok\n");
        assert!(
            cases.contains_key("a_resume_without_a_pause_changes_nothing"),
            "got {:?}",
            cases.keys().collect::<Vec<_>>()
        );
    }

    /// The harness's `should panic` annotation is not part of the name.
    #[test]
    fn a_should_panic_case_is_named_without_the_annotation() {
        let cases = parse("test tests::a_case_that_panics - should panic ... ok\n");
        assert!(
            cases.contains_key("a_case_that_panics"),
            "got {:?}",
            cases.keys().collect::<Vec<_>>()
        );
    }

    /// The words "test " in ordinary output are not a report.
    #[test]
    fn prose_mentioning_a_test_is_not_mistaken_for_one() {
        let cases = parse("note: this test ... is not a report\n");
        assert!(
            cases.is_empty(),
            "got {:?}",
            cases.keys().collect::<Vec<_>>()
        );
    }

    /// A line that opens with junk and then carries a real report yields the
    /// report, not the junk.
    #[test]
    fn a_report_after_unrelated_text_is_found() {
        let cases = parse("[drm:info] whatever test tests::a_case ... ok\n");
        assert_eq!(cases.len(), 1);
        assert!(cases.contains_key("a_case"));
    }

    /// The card a run used is not in its name, and used not to be anywhere.
    #[test]
    fn a_run_says_which_card_and_driver_it_used() {
        let announced =
            super::announced_driver("DRMKIT-DEVICE\t/dev/dri/card0\tvkms\ntest a_case ... ok\n");
        assert_eq!(announced.as_deref(), Some("vkms at /dev/dri/card0"));
    }

    /// The announcement can sit behind a progress dot like the skip lines do.
    #[test]
    fn the_announcement_is_found_behind_a_progress_dot() {
        let announced = super::announced_driver(".DRMKIT-DEVICE\t/dev/dri/card1\tamdgpu\n");
        assert_eq!(announced.as_deref(), Some("amdgpu at /dev/dri/card1"));
    }

    /// A run against a different card must not be compared against this
    /// baseline. Both are green; the label is the only thing that is wrong,
    /// which is exactly how a vkms run got reported as amdgpu.
    #[test]
    fn a_run_from_another_driver_is_refused_before_it_is_compared() {
        let baseline = parse_baseline(&format!(
            "{{\n  \"{}\": \"vkms at /dev/dri/card0\",\n  \"a_case\": \"ran\"\n}}\n",
            super::DEVICE_KEY
        ))
        .expect("baseline");
        assert_eq!(
            baseline[super::DEVICE_KEY].reason(),
            "vkms at /dev/dri/card0"
        );

        // The metadata key is not a case, so it never shows up as one.
        let observed = parse("test a_case ... ok\n");
        compare("board", &baseline, &observed).expect("the key is not a missing case");
    }

    /// A skip caused by whatever else was running is not a fact about the
    /// device, and recording one turns it into a permanent excuse.
    #[test]
    fn a_run_that_lost_drm_master_cannot_be_recorded() {
        let observed =
            parse("test a_case ... ok\nDRMKIT-SKIP\ta_case\tanother client holds DRM master\n");
        let error = super::environment_is_fit_to_record(&observed).expect_err("must refuse");
        assert!(error.contains("holds DRM master"));
        assert!(error.contains("stop the compositor"), "and says what to do");
    }

    #[test]
    fn a_run_that_needed_root_cannot_be_recorded() {
        let observed = parse(
            "test a_case ... ok\nDRMKIT-SKIP\ta_case\twriting /sys/class/drm/card0/uevent needs root (try sudo)\n",
        );
        super::environment_is_fit_to_record(&observed).expect_err("must refuse");
    }

    /// A skip the device genuinely cannot help is exactly what a baseline is
    /// for, and must not be caught by the guard.
    #[test]
    fn a_capability_skip_records_fine() {
        let observed = parse(
            "test a_case ... ok\nDRMKIT-SKIP\ta_case\tno CRTC on this device has a colour pipeline\n",
        );
        super::environment_is_fit_to_record(&observed).expect("a real capability skip");
    }

    fn device(id: &str, cases: &[(&str, &str)]) -> super::DeviceCoverage {
        let mut map = std::collections::BTreeMap::new();
        for (name, reason) in cases {
            let case = if *reason == "ran" {
                Case::Ran
            } else {
                Case::Skipped((*reason).to_owned())
            };
            map.insert((*name).to_owned(), case);
        }
        (id.to_owned(), map)
    }

    /// The question a single device cannot answer.
    #[test]
    fn a_case_skipped_on_every_device_asserts_nowhere() {
        let pool = vec![
            device("board-a", &[("a_case", "no HOTSPOT_X here")]),
            device("board-b", &[("a_case", "no HOTSPOT_X here")]),
        ];
        let found = super::classify(&pool);
        assert_eq!(found.nowhere, vec!["a_case"]);
    }

    #[test]
    fn a_case_that_asserts_on_one_device_asserts_somewhere() {
        let pool = vec![
            device("board-a", &[("a_case", "ran")]),
            device("board-b", &[("a_case", "no gamma stage")]),
        ];
        let found = super::classify(&pool);
        assert!(found.nowhere.is_empty(), "one device is enough");
        assert_eq!(
            found.narrow,
            vec![("a_case", "board-a")],
            "and it rests on that one device"
        );
    }

    /// The distinction that keeps the narrow list from crying wolf: a crate
    /// that does not cross-build for a target never reports there at all, and
    /// that is the run's scope rather than a coverage risk.
    #[test]
    fn a_case_absent_elsewhere_is_unmeasured_not_narrow() {
        let pool = vec![
            device("board-a", &[("a_case", "ran"), ("shared", "ran")]),
            device("board-b", &[("shared", "ran")]),
        ];
        let found = super::classify(&pool);
        assert!(
            found.narrow.is_empty(),
            "board-b never ran it, which is not the same as skipping it"
        );
        assert_eq!(found.unmeasured, 1);
        assert!(found.nowhere.is_empty());
    }

    /// A case every device asserts is neither at risk nor missing.
    #[test]
    fn a_case_asserted_everywhere_raises_nothing() {
        let pool = vec![
            device("board-a", &[("a_case", "ran")]),
            device("board-b", &[("a_case", "ran")]),
        ];
        let found = super::classify(&pool);
        assert!(found.nowhere.is_empty());
        assert!(found.narrow.is_empty());
        assert_eq!(found.unmeasured, 0);
        assert_eq!(found.known.len(), 1);
    }

    #[test]
    fn the_marker_matches_the_one_testkit_prints() {
        assert_eq!(super::drmkit_testkit_marker(), drmkit_testkit::MARKER);
    }
}
