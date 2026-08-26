// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! Run drmkit's capability logic over every device in the drmdb corpus.
//!
//! [drmdb](https://drmdb.emersion.fr) collects `drm_info` dumps from real
//! hardware -- around 1,200 machines across 50 drivers at the time of
//! writing. A lab has the devices you bought; this has the devices people
//! actually have, including the ones nobody would think to buy.
//!
//! The point is not compatibility reporting. It is that assumptions in this
//! tree get checked against hardware rather than against a comment. Every
//! rule below is one that was, at some point, asserted from a single board:
//!
//! - `SRC_W` is on every atomic plane, so `supports_scaling` carries no
//!   information ([drm-cxx#252](https://github.com/jwinarske/drm-cxx/issues/252)).
//! - A plane exposing `rotation` always advertises `rotate-0`.
//! - Every primary plane can host a composition canvas, or composition is
//!   impossible on that device and layers past the plane count are dropped.
//!
//! A rule that fails here has found real hardware the code is wrong about.

use std::collections::BTreeMap;
use std::path::Path;

use serde_json::Value;

mod device;
mod rules;

use device::Device;

fn main() -> std::process::ExitCode {
    let mut args = std::env::args().skip(1);
    let dir = args.next().unwrap_or_else(|| "drmdb-data".to_owned());

    let devices = match load(Path::new(&dir)) {
        Ok(devices) if devices.is_empty() => {
            eprintln!("no drm_info dumps under {dir}");
            return std::process::ExitCode::FAILURE;
        }
        Ok(devices) => devices,
        Err(error) => {
            eprintln!("error: {error}");
            return std::process::ExitCode::FAILURE;
        }
    };

    let mut by_driver: BTreeMap<&str, usize> = BTreeMap::new();
    for device in &devices {
        *by_driver.entry(device.driver.as_str()).or_default() += 1;
    }
    println!(
        "{} device(s), {} driver(s)\n",
        devices.len(),
        by_driver.len()
    );

    let report = rules::check_all(&devices);
    print!("{}", report.render(&devices));

    if report.violations() == 0 {
        std::process::ExitCode::SUCCESS
    } else {
        std::process::ExitCode::FAILURE
    }
}

fn load(dir: &Path) -> Result<Vec<Device>, String> {
    let entries = std::fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let mut devices = Vec::new();
    let mut unreadable = 0;

    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_none_or(|ext| ext != "json") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            unreadable += 1;
            continue;
        };
        let Ok(value) = serde_json::from_str::<Value>(&text) else {
            // A dump this cannot parse is skipped and counted, never treated
            // as a device that passed every rule.
            unreadable += 1;
            continue;
        };
        let name = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        devices.push(Device::from_json(&name, &value));
    }

    if unreadable > 0 {
        println!("note: {unreadable} dump(s) could not be read and were skipped");
    }
    devices.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(devices)
}
