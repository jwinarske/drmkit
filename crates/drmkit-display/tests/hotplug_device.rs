// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! The hotplug watch against real kernel uevents.
//!
//! # What is reachable here, and what is not
//!
//! Since Linux 4.13 a uevent can be synthesized by writing
//! `ACTION UUID [KEY=VALUE ...]` to a device's `uevent` file. Measured on
//! 7.1.9, the UUID is mandatory the moment any argument follows -- `change
//! HOTPLUG=1` is `EINVAL` -- and every added property arrives **prefixed**:
//!
//! ```text
//! write "change 0000...0001 HOTPLUG=1 CONNECTOR=42"
//!   -> ACTION=change ... SYNTH_ARG_CONNECTOR=42 SYNTH_ARG_HOTPLUG=1 SYNTH_UUID=0000...0001
//! ```
//!
//! So no synthetic uevent can carry a genuine `HOTPLUG=1`, and the accept path
//! is not reachable without a display actually being plugged or unplugged. It
//! is covered instead by the filter's unit tests, which take the property
//! value directly.
//!
//! What *is* reachable, and is what these check, is the half that a unit test
//! cannot reach: that the socket is connected to the right subsystem and that
//! an ordinary DRM `change` -- the kind a compositor produces several times a
//! second -- is read off it and discarded. Ground truth comes from a second,
//! raw monitor, so "delivered nothing" cannot pass by the socket being dead.
//!
//! Writing a `uevent` file is root's, which the device lanes are and a desktop
//! session is not.

use std::fs;
use std::os::fd::AsFd as _;
use std::path::Path;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use rustix::event::{PollFd, PollFlags, poll};
use udev::MonitorBuilder;

use drmkit_display::{HotplugEvent, HotplugMonitor, Source};

/// Shared with the other device tests: they open the same card.
static CARD_LOCK: Mutex<()> = Mutex::new(());

fn card_guard() -> std::sync::MutexGuard<'static, ()> {
    CARD_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// The card's sysfs directory and its device node.
fn card() -> (String, String) {
    let node = std::env::var("DRMKIT_TEST_CARD").unwrap_or_else(|_| "/dev/dri/card0".to_owned());
    let name = Path::new(&node)
        .file_name()
        .expect("a card path ends in a name")
        .to_string_lossy()
        .into_owned();
    (format!("/sys/class/drm/{name}"), node)
}

/// Whether this process can synthesize uevents.
///
/// A privilege boundary rather than a missing feature, so it is reported
/// rather than asserted -- but the device lanes run as root, and
/// `DRMKIT_REQUIRE_MASTER` marks the runs where quietly doing nothing is a
/// failure.
fn can_synthesize(uevent: &str) -> bool {
    if !Path::new(uevent).exists() {
        assert!(
            std::env::var_os("DRMKIT_REQUIRE_MASTER").is_none(),
            "{uevent} does not exist, but DRMKIT_REQUIRE_MASTER is set"
        );
        println!("note: skipped -- no {uevent}");
        return false;
    }
    if rustix::process::geteuid().is_root() {
        return true;
    }
    assert!(
        std::env::var_os("DRMKIT_REQUIRE_MASTER").is_none(),
        "writing {uevent} needs root, but DRMKIT_REQUIRE_MASTER is set"
    );
    println!("note: skipped -- writing {uevent} needs root (try sudo)");
    false
}

/// A raw monitor on the same source, for ground truth.
fn raw_monitor() -> udev::MonitorSocket {
    MonitorBuilder::new_kernel()
        .and_then(|builder| builder.match_subsystem("drm"))
        .and_then(MonitorBuilder::listen)
        .expect("kernel monitor")
}

/// Whether `socket` produced any event within `timeout`, and what its
/// `SEQNUM`s were.
fn raw_events(socket: &udev::MonitorSocket, timeout: Duration) -> Vec<String> {
    let mut seen = Vec::new();
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        let fd = socket.as_fd();
        let mut fds = [PollFd::new(&fd, PollFlags::IN)];
        let step = Duration::from_millis(100).try_into().expect("timeout");
        if poll(&mut fds, Some(&step)).expect("poll") == 0 {
            continue;
        }
        for event in socket.iter() {
            seen.push(format!("{:?}", event.event_type()));
        }
        break;
    }
    seen
}

/// Everything the monitor under test yields within `timeout`.
fn hotplug_events(monitor: &mut HotplugMonitor, timeout: Duration) -> Vec<HotplugEvent> {
    let mut events = Vec::new();
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        {
            let fd = monitor.as_fd();
            let mut fds = [PollFd::new(&fd, PollFlags::IN)];
            let step = Duration::from_millis(100).try_into().expect("timeout");
            if poll(&mut fds, Some(&step)).expect("poll") == 0 {
                continue;
            }
        }
        monitor.dispatch(&mut events);
        break;
    }
    events
}

#[test]
#[ignore = "needs a DRM device and root"]
fn an_ordinary_drm_change_is_read_and_discarded() {
    let _guard = card_guard();
    let (sysfs, _node) = card();
    let uevent = format!("{sysfs}/uevent");
    if !can_synthesize(&uevent) {
        return;
    }

    // The kernel's own broadcast rather than udevd's republication: a device
    // lane has no udevd, and a monitor waiting for one waits forever.
    let mut monitor = HotplugMonitor::open(Source::Kernel).expect("monitor");
    let ground_truth = raw_monitor();

    fs::write(&uevent, "change").expect("synthesize");

    let raw = raw_events(&ground_truth, Duration::from_secs(5));
    assert!(
        !raw.is_empty(),
        "the socket delivered nothing at all, so this proves nothing"
    );

    let delivered = hotplug_events(&mut monitor, Duration::from_millis(500));
    assert!(
        delivered.is_empty(),
        "a change with no HOTPLUG property was reported as a hotplug: {delivered:?}"
    );
}

#[test]
#[ignore = "needs a DRM device and root"]
fn a_property_that_merely_looks_like_hotplug_is_not_one() {
    // The kernel prefixes every property a synthetic uevent adds, so this is
    // what a synthesized `HOTPLUG=1` actually arrives as. Reporting it would
    // mean any process that can write a uevent file could make a compositor
    // re-enumerate every connector on demand.
    let _guard = card_guard();
    let (sysfs, _node) = card();
    let uevent = format!("{sysfs}/uevent");
    if !can_synthesize(&uevent) {
        return;
    }

    let mut monitor = HotplugMonitor::open(Source::Kernel).expect("monitor");
    let ground_truth = raw_monitor();

    fs::write(
        &uevent,
        "change 4d1a4b1e-0000-4000-8000-000000000001 HOTPLUG=1 CONNECTOR=42",
    )
    .expect("synthesize");

    let raw = raw_events(&ground_truth, Duration::from_secs(5));
    assert!(
        !raw.is_empty(),
        "the socket delivered nothing at all, so this proves nothing"
    );

    let delivered = hotplug_events(&mut monitor, Duration::from_millis(500));
    assert!(
        delivered.is_empty(),
        "SYNTH_ARG_HOTPLUG was taken for HOTPLUG: {delivered:?}"
    );
}

#[test]
#[ignore = "needs a DRM device"]
fn an_idle_monitor_says_nothing() {
    // No root needed: this is the case a poll loop spends all its time in.
    let _guard = card_guard();
    let mut monitor = HotplugMonitor::open(Source::Kernel).expect("monitor");
    let delivered = hotplug_events(&mut monitor, Duration::from_millis(300));
    assert!(delivered.is_empty(), "{delivered:?}");
}
