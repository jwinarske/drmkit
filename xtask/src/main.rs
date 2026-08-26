// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! drmkit repository automation.
//!
//! The two linters here exist because drm-cxx keeps moving during the port —
//! 104 commits in the three weeks before the plan was written (plan §12). A
//! test file or invariant appearing upstream without a corresponding entry
//! here must surface as red CI, not as silence.
//!
//! Run via the workspace alias:
//!
//! ```text
//! cargo xtask check                       # both linters
//! cargo xtask parity --ref /path/to/drm-cxx
//! cargo xtask invariants
//! cargo xtask lanes
//! ```
//!
//! The reference tree is located, in order, from `--ref`, then the
//! `DRMKIT_DRM_CXX` environment variable.

use std::process::ExitCode;

mod coverage;
mod invariants;
mod lanes;
mod parity;
mod refs;
mod validate;

const USAGE: &str = "\
drmkit xtask

USAGE:
    cargo xtask <COMMAND> [OPTIONS]

COMMANDS:
    check         Run every linter (parity + invariants)
    parity        Verify TEST_PARITY.md covers every upstream test file
    invariants    Verify INVARIANTS.md matches the upstream contract list
    lanes         Verify the drmdb lane runs on the crates its scanner reads
    coverage      Compare a device test run against that device's coverage baseline
    validate      Probe the hardware pool and diff against the baselines
    help          Show this message

OPTIONS:
    --ref <PATH>  Path to a drm-cxx checkout. Defaults to $DRMKIT_DRM_CXX.

COVERAGE OPTIONS:
    --device=<ID>        Which device's baseline this run is measured against
    --record             Write the baseline from this run instead of checking it

    Reads a test run on stdin. How the suites reach a device is not its
    business -- locally, over ssh, or in a lane -- only whether a case that
    asserted there before still does:

        cargo test --workspace -- --ignored --nocapture 2>&1 \\
          | cargo xtask coverage --device=localhost-amdgpu

VALIDATE OPTIONS:
    --device=<ID>        Only this device; repeatable
    --require=<A,B>      Fail if these devices could not be reached
    --update-baselines   Accept what the hardware now reports
    --verbose            Show the full build output for a device that fails

    A device that cannot be reached is reported, never counted as passing.
    See docs/validation.md.
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some((head, options)) = args.split_first() else {
        eprint!("{USAGE}");
        return ExitCode::FAILURE;
    };
    let command = head.as_str();

    // `validate` carries its own options, and the reference parser rejects
    // anything it does not know -- so it is parsed only for the commands that
    // take a reference.
    if command == "validate" {
        return report(validate::run(options));
    }
    if command == "coverage" {
        return report(coverage::run(options));
    }

    let reference = match refs::parse_ref_option(options) {
        Ok(value) => value,
        Err(message) => {
            eprintln!("error: {message}");
            return ExitCode::FAILURE;
        }
    };

    let outcome = match command {
        "check" => run_check(reference.as_deref()),
        "parity" => parity::run(reference.as_deref()),
        "invariants" => invariants::run(reference.as_deref()),
        "lanes" => lanes::run(),
        "help" | "--help" | "-h" => {
            print!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        other => Err(format!("unknown command `{other}`\n\n{USAGE}")),
    };

    report(outcome)
}

/// Turn a command's result into an exit code.
fn report(outcome: Result<(), String>) -> ExitCode {
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("error: {message}");
            ExitCode::FAILURE
        }
    }
}

/// Run both linters and report every failure, not just the first — a
/// contributor fixing drift wants the whole list in one pass.
fn run_check(reference: Option<&str>) -> Result<(), String> {
    let mut failures = Vec::new();

    if let Err(message) = parity::run(reference) {
        failures.push(message);
    }
    if let Err(message) = invariants::run(reference) {
        failures.push(message);
    }
    if let Err(message) = lanes::run() {
        failures.push(message);
    }

    if failures.is_empty() {
        Ok(())
    } else {
        Err(failures.join("\n\n"))
    }
}
