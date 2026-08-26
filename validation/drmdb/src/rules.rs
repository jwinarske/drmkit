// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! The rules, and what each one being wrong would mean.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use drmkit_planes::{PlaneRegistry, PlaneType};
use drmkit_scene::canvas_format_for_plane;

use crate::device::Device;

/// One rule's outcome.
pub(crate) struct Finding {
    pub(crate) rule: &'static str,
    /// What a violation means, in the terms of the thing that breaks.
    pub(crate) matters: &'static str,
    pub(crate) checked: usize,
    /// Devices that broke it, with what was wrong.
    pub(crate) violations: Vec<String>,
}

/// A distribution worth knowing, which is not pass or fail.
pub(crate) struct Survey {
    pub(crate) title: String,
    /// Why the shape matters, in the terms of the thing that depends on it.
    pub(crate) note: &'static str,
    /// Label and count, already ordered by whatever the survey ranks on.
    pub(crate) rows: Vec<(String, usize)>,
}

pub(crate) struct Report {
    findings: Vec<Finding>,
    surveys: Vec<Survey>,
}

impl Report {
    pub(crate) fn violations(&self) -> usize {
        self.findings.iter().map(|f| f.violations.len()).sum()
    }

    pub(crate) fn render(&self, devices: &[Device]) -> String {
        let mut out = String::new();
        let atomic = devices.iter().filter(|d| d.atomic).count();
        let _ = writeln!(
            out,
            "{atomic} of {} dumps carry atomic plane properties; rules about \
             them apply to those.\n",
            devices.len()
        );

        for finding in &self.findings {
            if finding.violations.is_empty() {
                let _ = writeln!(out, "ok    {} ({} checked)", finding.rule, finding.checked);
                continue;
            }
            let _ = writeln!(
                out,
                "FAIL  {} -- {} of {} checked",
                finding.rule,
                finding.violations.len(),
                finding.checked
            );
            let _ = writeln!(out, "      {}", finding.matters);
            for violation in finding.violations.iter().take(10) {
                let _ = writeln!(out, "      {violation}");
            }
            if finding.violations.len() > 10 {
                let _ = writeln!(out, "      ... and {} more", finding.violations.len() - 10);
            }
        }

        for survey in &self.surveys {
            let _ = writeln!(out, "\n{}", survey.title);
            let _ = writeln!(out, "  ({})", survey.note);
            for (label, count) in survey.rows.iter().take(SURVEY_ROWS) {
                let _ = writeln!(out, "  {count:>6}  {label}");
            }
            // Disclosed, never silent: a truncated tail that reads as the whole
            // distribution is what hid the two P-6 planes behind a histogram
            // whose visible rows all looked unanimous.
            if let Some(dropped) = survey.rows.len().checked_sub(SURVEY_ROWS)
                && dropped > 0
            {
                let tail: usize = survey.rows[SURVEY_ROWS..].iter().map(|(_, n)| n).sum();
                let _ = writeln!(out, "         ... {dropped} more row(s), {tail} plane(s)");
            }
        }
        out
    }
}

/// How many survey rows are printed before the tail is summarised.
const SURVEY_ROWS: usize = 14;

pub(crate) fn check_all(devices: &[Device]) -> Report {
    Report {
        findings: vec![
            the_corpus_parsed_into_something_usable(devices),
            src_w_on_every_atomic_plane(devices),
            rotation_always_advertises_zero(devices),
            every_primary_can_host_a_canvas(devices),
        ],
        surveys: vec![
            damage_clips_by_driver(devices),
            rotation_shapes(devices),
            zpos_shapes(devices),
            degenerate_mutable_zpos(devices),
            cursor_paths(devices),
        ],
    }
}

/// Every device has a plane that can feed a CRTC.
///
/// This one is about the scanner, not the hardware. Real hardware always
/// satisfies it -- a plane that can drive nothing is not a plane anyone
/// shipped -- so it can only fail when a field stopped being read.
///
/// It is here because a survey answering "zero" reads exactly like a survey
/// answering a real question. Twice now a field quietly went unparsed and the
/// output stayed confident: once when `Max` was read only in lowercase, so
/// every zpos range looked degenerate, and once when `possible_crtcs` was
/// never wired, so every CRTC in the corpus appeared to have no cursor plane
/// and to fall back to the legacy path. Both looked like findings.
///
/// A rule that fails loudly on the shape a parse error produces is cheaper
/// than noticing the number is implausible.
fn the_corpus_parsed_into_something_usable(devices: &[Device]) -> Finding {
    let mut checked = 0;
    let mut violations = Vec::new();
    for device in devices {
        if !device.atomic || device.planes.is_empty() || device.crtc_count == 0 {
            continue;
        }
        checked += 1;
        let usable = device
            .planes
            .iter()
            .any(|plane| (0..device.crtc_count).any(|crtc| plane.caps.compatible_with_crtc(crtc)));
        if !usable {
            violations.push(format!(
                "{} ({}): {} plane(s), {} CRTC(s), and no plane can feed any of them",
                device.name,
                device.driver,
                device.planes.len(),
                device.crtc_count
            ));
        }
    }
    Finding {
        rule: "every device has a plane that can feed a CRTC",
        matters: "not a hardware claim -- this is the shape a field that \
                  stopped being parsed produces, and every survey downstream \
                  would report zeros with the same confidence as a real answer",
        checked,
        violations,
    }
}

/// `SRC_W` is present on every plane of an atomic device.
///
/// This is what makes `supports_scaling` carry no information: it is set from
/// the property's presence, and the property is always there. If a real
/// device ever lacks it, the field starts meaning something and the matcher
/// edge that consumes it stops being dead.
fn src_w_on_every_atomic_plane(devices: &[Device]) -> Finding {
    let mut checked = 0;
    let mut violations = Vec::new();
    for device in devices.iter().filter(|d| d.atomic) {
        for plane in &device.planes {
            checked += 1;
            if !plane.caps.supports_scaling {
                violations.push(format!(
                    "{} ({}): plane {} has no SRC_W",
                    device.name, device.driver, plane.caps.id
                ));
            }
        }
    }
    Finding {
        rule: "every atomic plane has SRC_W",
        matters: "supports_scaling would start carrying information, and the \
                  matcher's scaling edge would stop being dead",
        checked,
        violations,
    }
}

/// A plane exposing `rotation` advertises `rotate-0`.
///
/// The port's device tests assert this, on the grounds that there is no such
/// thing as a plane that must be rotated. If one exists, a caller writing
/// `rotate-0` to it gets `EINVAL` and the frame is refused.
fn rotation_always_advertises_zero(devices: &[Device]) -> Finding {
    let mut checked = 0;
    let mut violations = Vec::new();
    for device in devices {
        for plane in &device.planes {
            if plane.rotation_names.is_empty() {
                continue;
            }
            checked += 1;
            if !plane.rotation_names.iter().any(|n| n == "rotate-0") {
                violations.push(format!(
                    "{} ({}): plane {} advertises {:?} without rotate-0",
                    device.name, device.driver, plane.caps.id, plane.rotation_names
                ));
            }
        }
    }
    Finding {
        rule: "a rotatable plane advertises rotate-0",
        matters: "writing rotate-0 to such a plane is EINVAL, which refuses \
                  the whole frame",
        checked,
        violations,
    }
}

/// Every primary plane can host a composition canvas.
///
/// This runs drmkit's own `canvas_format_for_plane` over real hardware. A
/// primary it cannot find a format for is a device where composition is
/// impossible: every layer past the plane count is dropped rather than
/// blended, and nothing reports it.
fn every_primary_can_host_a_canvas(devices: &[Device]) -> Finding {
    let mut checked = 0;
    let mut violations = Vec::new();
    for device in devices {
        for plane in &device.planes {
            if plane.caps.plane_type != PlaneType::Primary || plane.caps.formats.is_empty() {
                continue;
            }
            checked += 1;
            if canvas_format_for_plane(&plane.caps).is_none() {
                let names: Vec<String> = plane
                    .caps
                    .formats
                    .iter()
                    .take(6)
                    .map(|f| drmkit_fmt::fourcc::format_name(*f).to_owned())
                    .collect();
                violations.push(format!(
                    "{} ({}): primary {} scans out none of the canvas formats; it has {}",
                    device.name,
                    device.driver,
                    plane.caps.id,
                    names.join(", ")
                ));
            }
        }
    }
    Finding {
        rule: "every primary plane can host a composition canvas",
        matters: "composition is impossible on that device, so layers past the \
                  plane count are dropped rather than blended",
        checked,
        violations,
    }
}

/// Planes whose zpos range is degenerate but *not* flagged immutable.
///
/// This is the P-6 hardware, and it is real: two Allwinner boards in the
/// corpus advertise `zpos` with `min == max == 0` and leave it mutable.
///
/// The reference guards this with `zpos_is_fixed`, skipping the write --
/// because it writes the caller's requested value through unchanged, and
/// anything but 0 on such a plane is `EINVAL`, which refuses the whole frame.
///
/// The port does not need that guard: `PlanePropertyMap::clamp_zpos`, added
/// for P-15, clamps a derived zpos into the plane's own range first, so a
/// degenerate range yields a write of 0 -- which the kernel takes. The clamp
/// subsumes the guard. `degenerate_zpos_clamps_to_the_only_value` in
/// drmkit-scene pins that against this exact shape.
///
/// Reported rather than asserted: the port stays correct however many of
/// these exist. What is worth knowing is that the count is not zero, because
/// that is what makes the clamp load-bearing rather than decorative.
fn degenerate_mutable_zpos(devices: &[Device]) -> Survey {
    let mut by_driver: BTreeMap<&str, usize> = BTreeMap::new();
    for device in devices {
        for plane in &device.planes {
            let (Some(min), Some(max)) = (plane.caps.zpos_min, plane.caps.zpos_max) else {
                continue;
            };
            if min == max && !plane.zpos_immutable {
                *by_driver.entry(device.driver.as_str()).or_default() += 1;
            }
        }
    }
    let mut rows: Vec<(String, usize)> = by_driver
        .into_iter()
        .map(|(driver, count)| (driver.to_owned(), count))
        .collect();
    rows.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    Survey {
        title: "degenerate but mutable zpos, by driver (the P-6 shape)".to_owned(),
        note: "clamp_zpos turns these into a write of the only legal value; \
               without it the frame is refused whole",
        rows,
    }
}

/// Which cursor path each CRTC in the corpus would actually get.
///
/// `select_plane` prefers a dedicated cursor plane, falls back to an overlay
/// when there is none or when the cursor plane cannot take `ARGB8888`, and
/// falls back again to the legacy `drmModeSetCursor`. Each fallback costs
/// something real -- an overlay the compositor wanted, or the ability to
/// coalesce the cursor with a frame and to rotate it.
///
/// This is a genuine replay rather than a count: the plane list is rebuilt as
/// a `PlaneRegistry` and `select_plane` is asked, once per CRTC, with the same
/// `possible_crtcs` the device reported.
///
/// What it answers is whether the fallbacks are reachable. `supports_scaling`
/// is the cautionary case -- a field read from a property that is always
/// present, feeding a matcher edge that never excluded anything. A path no
/// hardware takes is the same thing wearing different clothes.
fn cursor_paths(devices: &[Device]) -> Survey {
    let mut counts: BTreeMap<&'static str, usize> = BTreeMap::new();
    let mut no_cursor_plane = 0;
    let mut cursor_plane_without_argb = 0;

    for device in devices {
        // A pre-atomic dump has no plane types to speak of, and the cursor
        // path is not a question that can be asked of it.
        if !device.atomic {
            continue;
        }
        let registry = PlaneRegistry::from_capabilities(
            device.planes.iter().map(|p| p.caps.clone()).collect(),
        );
        for crtc in 0..device.crtc_count {
            let cursors: Vec<_> = device
                .planes
                .iter()
                .filter(|p| {
                    p.caps.plane_type == PlaneType::Cursor && p.caps.compatible_with_crtc(crtc)
                })
                .collect();
            if cursors.is_empty() {
                no_cursor_plane += 1;
            } else if !cursors
                .iter()
                .any(|p| p.caps.supports_format(drmkit_fmt::fourcc::ARGB8888))
            {
                cursor_plane_without_argb += 1;
            }

            let label = match drmkit_cursor::select_plane(&registry, crtc, None, true, &|_| false) {
                Ok(selected) => match selected.path {
                    drmkit_cursor::PlanePath::AtomicCursor => "a dedicated cursor plane",
                    drmkit_cursor::PlanePath::AtomicOverlay => "an overlay, atomically",
                    drmkit_cursor::PlanePath::Legacy => "drmModeSetCursor, the legacy path",
                },
                Err(_) => "nothing can carry a cursor",
            };
            *counts.entry(label).or_default() += 1;
        }
    }

    let mut rows: Vec<(String, usize)> = counts
        .into_iter()
        .map(|(label, count)| (label.to_owned(), count))
        .collect();
    rows.push((
        "  (of which: no cursor plane on the CRTC)".to_owned(),
        no_cursor_plane,
    ));
    rows.push((
        "  (of which: a cursor plane that cannot take ARGB8888)".to_owned(),
        cursor_plane_without_argb,
    ));
    Survey {
        title: "cursor path per CRTC, replayed through select_plane".to_owned(),
        note: "each fallback costs an overlay, or the ability to coalesce the \
               cursor with a frame and to rotate it",
        rows,
    }
}

fn damage_clips_by_driver(devices: &[Device]) -> Survey {
    let mut with: BTreeMap<&str, (usize, usize)> = BTreeMap::new();
    for device in devices {
        let entry = with.entry(device.driver.as_str()).or_default();
        for plane in &device.planes {
            entry.1 += 1;
            if plane.caps.has_fb_damage_clips {
                entry.0 += 1;
            }
        }
    }
    let mut rows: Vec<(String, usize)> = with
        .iter()
        .map(|(driver, (w, t))| (format!("{driver}: {w} of {t} planes"), *w))
        .collect();
    rows.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    Survey {
        title: "FB_DAMAGE_CLIPS by driver".to_owned(),
        note: "a comment in the reference names vc4 as capable; the driver \
               never attaches the property -- this is issue #255",
        rows,
    }
}

fn rotation_shapes(devices: &[Device]) -> Survey {
    let mut shapes: BTreeMap<String, usize> = BTreeMap::new();
    for device in devices {
        for plane in &device.planes {
            if plane.rotation_names.is_empty() {
                continue;
            }
            let mut names = plane.rotation_names.clone();
            names.sort();
            *shapes.entry(names.join(",")).or_default() += 1;
        }
    }
    let mut rows: Vec<(String, usize)> = shapes.into_iter().collect();
    rows.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    Survey {
        title: "rotation shapes in the wild".to_owned(),
        note: "the allocator gates on bits, not on the property's presence, \
               because a rotatable plane that cannot do 90/270 is common",
        rows,
    }
}

fn zpos_shapes(devices: &[Device]) -> Survey {
    let mut shapes: BTreeMap<String, usize> = BTreeMap::new();
    for device in devices {
        for plane in &device.planes {
            let key = match (plane.caps.zpos_min, plane.caps.zpos_max) {
                (Some(min), Some(max)) if min == max && plane.zpos_immutable => {
                    "degenerate and immutable".to_owned()
                }
                (Some(min), Some(max)) if min == max => {
                    format!("degenerate at {min}, NOT immutable")
                }
                (Some(min), Some(max)) => format!("settable [{min},{max}]"),
                // The two are not the same, and collapsing them is how a
                // dump that states no bounds would read as a device with no
                // stacking control at all.
                _ if plane.properties.iter().any(|p| p == "zpos") => {
                    "zpos present, no stated range".to_owned()
                }
                _ => "no zpos property".to_owned(),
            };
            *shapes.entry(key).or_default() += 1;
        }
    }
    let mut rows: Vec<(String, usize)> = shapes.into_iter().collect();
    rows.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    Survey {
        title: "zpos shapes".to_owned(),
        note: "a degenerate range that is NOT immutable is the P-6 case; \
               clamp_zpos is what keeps it from refusing the frame",
        rows,
    }
}
