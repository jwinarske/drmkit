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
//! ```
//!
//! The reference tree is located, in order, from `--ref`, then the
//! `DRMKIT_DRM_CXX` environment variable.

use std::process::ExitCode;

mod invariants;
mod parity;
mod refs;

const USAGE: &str = "\
drmkit xtask

USAGE:
    cargo xtask <COMMAND> [OPTIONS]

COMMANDS:
    check         Run every linter (parity + invariants)
    parity        Verify TEST_PARITY.md covers every upstream test file
    invariants    Verify INVARIANTS.md matches the upstream contract list
    help          Show this message

OPTIONS:
    --ref <PATH>  Path to a drm-cxx checkout. Defaults to $DRMKIT_DRM_CXX.
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some((head, options)) = args.split_first() else {
        eprint!("{USAGE}");
        return ExitCode::FAILURE;
    };
    let command = head.as_str();

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
        "help" | "--help" | "-h" => {
            print!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        other => Err(format!("unknown command `{other}`\n\n{USAGE}")),
    };

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

    if failures.is_empty() {
        Ok(())
    } else {
        Err(failures.join("\n\n"))
    }
}
