// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! `cargo xtask validate` -- run the pool and diff what it found.
//!
//! The recurring defect this exists for is not a wrong line of code, it is a
//! hardware claim nobody measured: a comment saying a driver carries a
//! property it does not, a lane running for months against one plane, a
//! finding asserting a board gave a threshold teeth when the number was flat.
//! Each was found by accident and each could have been found by a run.
//!
//! So a run measures what every device *is*, and diffs that against a
//! baseline checked in beside it. Hardware that changed under us -- a vendor
//! kernel bump that drops a property, a board swapped for a different
//! revision -- fails loudly instead of quietly changing what the suite covers.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;

/// One entry from `validation/devices.toml`.
#[derive(Debug, Default, Clone)]
pub(crate) struct DeviceEntry {
    pub(crate) id: String,
    pub(crate) ssh: String,
    pub(crate) target: String,
    pub(crate) sysroot: String,
    pub(crate) cards: Vec<String>,
    pub(crate) notes: String,
}

impl DeviceEntry {
    /// Whether this device runs on the orchestrator's own machine.
    fn is_local(&self) -> bool {
        self.ssh.is_empty()
    }
}

/// What happened to one device in a run.
enum Outcome {
    /// Probed, and its profile matches the baseline.
    Matches,
    /// Probed, and it differs. Carries the diff.
    Drifted(String),
    /// Probed, and there is no baseline yet.
    NoBaseline(String),
    /// Could not be reached. Never a pass.
    Unreached(String),
}

pub(crate) fn run(options: &[String]) -> Result<(), String> {
    let root = repo_root()?;
    let devices = parse_inventory(&root.join("validation/devices.toml"))?;

    let update = options.iter().any(|o| o == "--update-baselines");
    let verbose = options.iter().any(|o| o == "--verbose");
    let required: Vec<&str> = options
        .iter()
        .filter_map(|o| o.strip_prefix("--require="))
        .flat_map(|list| list.split(','))
        .collect();
    let only: Vec<&str> = options
        .iter()
        .filter_map(|o| o.strip_prefix("--device="))
        .collect();

    println!("validating {} device(s)\n", devices.len());
    let mut failures = Vec::new();
    let mut unreached = Vec::new();

    for device in &devices {
        if !only.is_empty() && !only.contains(&device.id.as_str()) {
            continue;
        }
        print!("{:<24} ", device.id);
        let outcome = validate_one(&root, device, update, verbose);
        match outcome {
            Outcome::Matches => println!("ok"),
            Outcome::NoBaseline(profile) => {
                let path = baseline_path(&root, &device.id);
                std::fs::write(&path, &profile).map_err(|e| format!("{}: {e}", path.display()))?;
                println!("baseline recorded ({})", path.display());
            }
            Outcome::Drifted(diff) => {
                println!("DRIFTED");
                println!("{diff}");
                failures.push(device.id.clone());
            }
            Outcome::Unreached(why) => {
                println!("unreached -- {why}");
                unreached.push(device.id.clone());
            }
        }
    }

    // Unreached is only a failure for the devices a release depends on.
    // Everything else is an honest gap, reported and not fatal, because a
    // pool where every board must be up is a pool nobody runs.
    let missing: Vec<&String> = unreached
        .iter()
        .filter(|id| required.contains(&id.as_str()))
        .collect();

    println!();
    if !unreached.is_empty() {
        println!("unreached: {}", unreached.join(", "));
    }
    if !missing.is_empty() {
        return Err(format!(
            "required device(s) not reached: {}",
            missing
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    if !failures.is_empty() {
        return Err(format!(
            "capability drift on: {}\n\nIf the change is intended, re-run with --update-baselines \
             and say in the commit what changed on the hardware and why.",
            failures.join(", ")
        ));
    }
    println!("all reached devices match their baselines");
    Ok(())
}

fn validate_one(root: &Path, device: &DeviceEntry, update: bool, verbose: bool) -> Outcome {
    let profile = match probe(root, device, verbose) {
        Ok(profile) => profile,
        Err(why) => return Outcome::Unreached(why),
    };

    let path = baseline_path(root, &device.id);
    let Ok(baseline) = std::fs::read_to_string(&path) else {
        return Outcome::NoBaseline(profile);
    };

    if baseline.trim() == profile.trim() {
        return Outcome::Matches;
    }
    if update {
        let _ = std::fs::write(&path, &profile);
        return Outcome::Matches;
    }
    Outcome::Drifted(diff(&baseline, &profile))
}

fn baseline_path(root: &Path, id: &str) -> PathBuf {
    root.join("validation/baselines").join(format!("{id}.json"))
}

/// Build the probe for the device's target and run it there.
fn probe(root: &Path, device: &DeviceEntry, verbose: bool) -> Result<String, String> {
    let binary = build_probe(root, device, verbose)?;

    if device.is_local() {
        let output = Command::new(&binary)
            .args(&device.cards)
            .output()
            .map_err(|e| format!("running {}: {e}", binary.display()))?;
        return finish(&output);
    }

    // Copy and run. A board with no Rust toolchain is the normal case, which
    // is why this is a cross-build and a copy rather than a remote cargo.
    let remote = format!("/tmp/drmkit-probe-{}", device.id);
    let scp = Command::new("scp")
        .arg("-q")
        .arg("-o")
        .arg("ConnectTimeout=10")
        .arg(&binary)
        .arg(format!("{}:{remote}", device.ssh))
        .status()
        .map_err(|e| format!("scp: {e}"))?;
    if !scp.success() {
        return Err("could not copy the probe (host down?)".to_owned());
    }

    let output = Command::new("ssh")
        .arg("-o")
        .arg("ConnectTimeout=10")
        .arg(&device.ssh)
        .arg(format!(
            "chmod +x {remote} && {remote} {}",
            device.cards.join(" ")
        ))
        .output()
        .map_err(|e| format!("ssh: {e}"))?;
    finish(&output)
}

fn finish(output: &std::process::Output) -> Result<String, String> {
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("probe failed: {}", stderr.trim()));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn build_probe(root: &Path, device: &DeviceEntry, verbose: bool) -> Result<PathBuf, String> {
    let mut cargo = Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()));
    cargo
        .current_dir(root)
        .args(["build", "--quiet", "-p", "drmkit-probe", "--release"]);

    let mut out = root.join("target");
    if !device.target.is_empty() {
        cargo.args(["--target", &device.target]);
        out = out.join(&device.target);

        // The borrowed sysroot, exactly as docs/cross-to-a-board.md sets it
        // up. Left to the environment when the entry names none, so a host
        // with a real cross libc needs no sysroot at all.
        if !device.sysroot.is_empty() {
            let sysroot = expand(&device.sysroot);
            let key = format!(
                "CARGO_TARGET_{}_RUSTFLAGS",
                device.target.to_uppercase().replace('-', "_")
            );
            cargo.env(
                key,
                format!(
                    "-C link-arg=--sysroot={sysroot} \
                     -C link-arg=-B{sysroot}/usr/lib/{arch} \
                     -C link-arg=-L{sysroot}/usr/lib/{arch} \
                     -C link-arg=-L{sysroot}/usr/lib/gcc/{arch}/14",
                    arch = gnu_arch(&device.target)
                ),
            );
            cargo.env(
                format!(
                    "CARGO_TARGET_{}_LINKER",
                    device.target.to_uppercase().replace('-', "_")
                ),
                format!("{}-gcc", gnu_arch(&device.target)),
            );
        }
    }

    // Captured rather than inherited: a device whose sysroot is missing
    // produces twenty lines of linker detail, and a pool run with three of
    // those in it is unreadable. The last error line names the cause, which
    // is what the operator needs; `--verbose` has the rest.
    let output = cargo.output().map_err(|e| format!("cargo: {e}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if verbose {
            eprintln!("{stderr}");
        }
        // The most specific line, not the last: cargo's final line is
        // always "could not compile", which names the crate and not the
        // cause. A missing library or sysroot says so earlier.
        let cause = stderr
            .lines()
            .find(|line| line.contains("cannot find") || line.contains("No such file"))
            .or_else(|| stderr.lines().find(|line| line.contains("error:")))
            .unwrap_or("see --verbose")
            .trim();
        return Err(format!("cross build failed: {cause}"));
    }
    Ok(out.join("release/drmkit-probe"))
}

/// `aarch64-unknown-linux-gnu` to `aarch64-linux-gnu`, which is what the
/// multiarch directories and the cross gcc are named after.
fn gnu_arch(target: &str) -> String {
    target.replace("-unknown-", "-")
}

fn expand(path: &str) -> String {
    match path.strip_prefix("~/") {
        Some(rest) => match std::env::var("HOME") {
            Ok(home) => format!("{home}/{rest}"),
            Err(_) => path.to_owned(),
        },
        None => path.to_owned(),
    }
}

/// A positional diff that says which card a change is on.
///
/// Set-based comparison was the first attempt and it reported
/// `- "with_damage_clips": 56` twice with no way to tell which of the
/// device's two cards moved. A profile has the same shape run to run unless a
/// card appeared or went away, so walking the two in step is both simpler and
/// answerable.
fn diff(baseline: &str, current: &str) -> String {
    let old: Vec<&str> = baseline.lines().collect();
    let new: Vec<&str> = current.lines().collect();
    let mut out = String::new();

    if old.len() != new.len() {
        let _ = writeln!(
            out,
            "  the profile changed shape: {} lines recorded, {} now -- a card \
             appeared or went away",
            old.len(),
            new.len()
        );
    }

    let mut card = String::from("(before any card)");
    let mut heading = String::new();
    let mut shown = 0;
    for (index, line) in new.iter().enumerate() {
        // The nearest preceding path is the card this line belongs to.
        if let Some(path) = line.trim().strip_prefix("\"path\": ") {
            path.trim_matches(|c| c == '"' || c == ',')
                .clone_into(&mut card);
        }
        let Some(was) = old.get(index) else { break };
        if was == line {
            continue;
        }
        // A header per card, not one per diff: a change on the second card
        // printed under the first card's heading is worse than no heading.
        if heading != card {
            let _ = writeln!(out, "  on {card}:");
            heading.clone_from(&card);
        }
        let _ = writeln!(out, "    - {}", was.trim());
        let _ = writeln!(out, "    + {}", line.trim());
        shown += 1;
        if shown == 20 {
            let _ = writeln!(out, "    ... and more; the shape may have moved");
            break;
        }
    }

    if out.is_empty() {
        out.push_str("  (whitespace only)\n");
    }
    out
}

fn repo_root() -> Result<PathBuf, String> {
    let dir = std::env::current_dir().map_err(|e| format!("{e}"))?;
    for candidate in dir.ancestors() {
        if candidate.join("validation/devices.toml").is_file() {
            return Ok(candidate.to_path_buf());
        }
    }
    Err("run this from inside the repository".to_owned())
}

/// Parse the inventory.
///
/// A hand-rolled reader for the one shape this file has, because the
/// workspace's tooling carries no dependencies and a TOML crate here would be
/// the first.
fn parse_inventory(path: &Path) -> Result<Vec<DeviceEntry>, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut devices = Vec::new();
    let mut current: Option<DeviceEntry> = None;

    for (number, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line == "[[device]]" {
            if let Some(device) = current.take() {
                devices.push(device);
            }
            current = Some(DeviceEntry::default());
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            return Err(format!(
                "{}:{}: not a key = value",
                path.display(),
                number + 1
            ));
        };
        let Some(device) = current.as_mut() else {
            return Err(format!(
                "{}:{}: a field before any [[device]]",
                path.display(),
                number + 1
            ));
        };
        let key = key.trim();
        let value = value.trim();
        if key == "cards" {
            device.cards = parse_list(value);
            continue;
        }
        let value = value.trim_matches('"').to_owned();
        match key {
            "id" => device.id = value,
            "ssh" => device.ssh = value,
            "target" => device.target = value,
            "sysroot" => device.sysroot = value,
            "notes" => device.notes = value,
            other => {
                return Err(format!(
                    "{}:{}: unknown field `{other}`",
                    path.display(),
                    number + 1
                ));
            }
        }
    }
    if let Some(device) = current {
        devices.push(device);
    }

    if devices.iter().any(|d| d.id.is_empty()) {
        return Err("every device needs an id".to_owned());
    }
    Ok(devices)
}

fn parse_list(value: &str) -> Vec<String> {
    value
        .trim_matches(|c| c == '[' || c == ']')
        .split(',')
        .map(|item| item.trim().trim_matches('"').to_owned())
        .filter(|item| !item.is_empty())
        .collect()
}
