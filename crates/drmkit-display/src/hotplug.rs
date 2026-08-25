// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! Noticing that a display was plugged in or pulled out.
//!
//! The kernel says so through a udev uevent rather than through anything on
//! the DRM descriptor, so this is a second thing to poll alongside the card.

use std::path::{Path, PathBuf};

use udev::{EventType, MonitorBuilder, MonitorSocket};

use drmkit_core::{CoreError, Result};

/// One connector changing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HotplugEvent {
    /// The card the kernel named, usually `/dev/dri/cardN`.
    ///
    /// Absent when the uevent carried no `DEVNAME`. Owned rather than
    /// borrowed from the udev device, which is freed as soon as the event has
    /// been read.
    pub card: Option<PathBuf>,

    /// Which connector, when the kernel says.
    ///
    /// The `CONNECTOR=<id>` hint arrived in 4.16. Absent on older kernels,
    /// and absent on the blanket events a GPU reset produces -- and absent is
    /// not "nothing changed", it is "re-enumerate everything", because the
    /// alternative is a display that was plugged in and never noticed.
    pub connector_id: Option<u32>,
}

/// Where the uevents are read from.
///
/// Not a detail on the machines drmkit is for. A desktop has udevd, which
/// applies its rules and only then republishes the event -- by which point the
/// device node exists with the right permissions. A minimal embedded init
/// often has no udevd at all, and a monitor waiting for one to republish
/// waits forever, so the display that was plugged in is never noticed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Source {
    /// What udevd republishes. The right answer where udevd runs.
    #[default]
    Udev,
    /// The kernel's own netlink broadcast, which arrives whether or not
    /// anything else is listening.
    Kernel,
}

/// A pollable watch on DRM hotplug.
///
/// Add [`AsFd`](std::os::fd::AsFd) to the poll set that already has the input
/// devices and the card in it, and call [`dispatch`](Self::dispatch) when it
/// is readable.
pub struct HotplugMonitor {
    socket: MonitorSocket,
}

impl HotplugMonitor {
    /// Start watching.
    ///
    /// # Errors
    ///
    /// [`CoreError::Io`] if udev is unavailable -- no `/run/udev`, or a
    /// container without the netlink socket. A caller that cannot watch can
    /// still poll connectors on a timer; what it loses is knowing *when*.
    pub fn open(source: Source) -> Result<Self> {
        let builder = match source {
            Source::Udev => MonitorBuilder::new(),
            Source::Kernel => MonitorBuilder::new_kernel(),
        };
        let socket = builder
            .and_then(|builder| builder.match_subsystem("drm"))
            .and_then(MonitorBuilder::listen)
            .map_err(|error| CoreError::Io(io_errno(&error)))?;
        Ok(Self { socket })
    }

    /// Read every pending uevent, appending the ones that are hotplug.
    ///
    /// Everything is drained whether or not it is kept, so the descriptor
    /// goes back to unreadable and a caller polling in a loop does not spin.
    pub fn dispatch(&mut self, out: &mut Vec<HotplugEvent>) {
        for event in self.socket.iter() {
            // The netlink filter matches on subsystem alone. DRM's `change`
            // also fires for lease updates and property-blob swaps, and
            // `HOTPLUG=1` is what separates a display being plugged in from
            // a compositor having set a property.
            let device = event.device();
            let hotplug = device
                .property_value("HOTPLUG")
                .and_then(std::ffi::OsStr::to_str);
            if !is_hotplug(event.event_type(), hotplug) {
                continue;
            }
            out.push(HotplugEvent {
                card: device.devnode().map(Path::to_path_buf),
                connector_id: parse_connector_id(
                    device
                        .property_value("CONNECTOR")
                        .and_then(std::ffi::OsStr::to_str),
                ),
            });
        }
    }
}

impl std::fmt::Debug for HotplugMonitor {
    /// The socket has nothing printable in it; the descriptor is what a
    /// caller debugging a poll set is looking for.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        use std::os::fd::AsFd as _;
        f.debug_struct("HotplugMonitor")
            .field("fd", &self.socket.as_fd())
            .finish()
    }
}

impl std::os::fd::AsFd for HotplugMonitor {
    fn as_fd(&self) -> std::os::fd::BorrowedFd<'_> {
        self.socket.as_fd()
    }
}

/// Whether one uevent is a display being plugged in or pulled out.
///
/// The netlink filter matches on subsystem alone, and DRM's `change` also
/// fires for lease updates and property-blob swaps -- several times a second
/// on a running compositor. `HOTPLUG=1` is what separates them, and acting on
/// the rest means re-enumerating every connector on every frame.
fn is_hotplug(event_type: EventType, hotplug: Option<&str>) -> bool {
    event_type == EventType::Change && hotplug == Some("1")
}

/// The kernel's `CONNECTOR=<id>` hint, if it is one.
///
/// Anything that is not a whole number is treated as absent rather than
/// guessed at: absent means re-enumerate, which is correct but slow, while a
/// wrong id means looking at a connector that did not change and missing the
/// one that did.
fn parse_connector_id(value: Option<&str>) -> Option<u32> {
    value?.parse().ok()
}

fn io_errno(error: &std::io::Error) -> rustix::io::Errno {
    rustix::io::Errno::from_io_error(error).unwrap_or(rustix::io::Errno::IO)
}

#[cfg(test)]
mod tests {
    use super::{EventType, is_hotplug, parse_connector_id};

    #[test]
    fn only_a_change_carrying_hotplug_counts() {
        assert!(is_hotplug(EventType::Change, Some("1")));
    }

    #[test]
    fn an_ordinary_change_is_ignored() {
        // A compositor setting a CRTC property produces one of these, and on
        // a running system that is several a second. Re-enumerating every
        // connector each time is a round trip to every sink on the machine.
        assert!(!is_hotplug(EventType::Change, None));
        assert!(!is_hotplug(EventType::Change, Some("0")));
    }

    #[test]
    fn an_add_or_remove_is_not_a_hotplug() {
        // Those are the card itself appearing or going away, which is a
        // different event with different handling -- the descriptor a caller
        // holds is about to stop working, not just its connector list.
        assert!(!is_hotplug(EventType::Add, Some("1")));
        assert!(!is_hotplug(EventType::Remove, Some("1")));
        assert!(!is_hotplug(EventType::Bind, Some("1")));
    }

    #[test]
    fn the_connector_hint_is_read_when_it_is_a_number() {
        assert_eq!(parse_connector_id(Some("393")), Some(393));
        assert_eq!(parse_connector_id(Some("0")), Some(0));
    }

    #[test]
    fn anything_else_means_re_enumerate() {
        // Absent on kernels before 4.16 and on the blanket events a GPU reset
        // produces. Guessing an id from a malformed one would send a caller
        // to look at a connector that did not change.
        assert_eq!(parse_connector_id(None), None);
        assert_eq!(parse_connector_id(Some("")), None);
        assert_eq!(parse_connector_id(Some("393x")), None);
        assert_eq!(parse_connector_id(Some("-1")), None);
        assert_eq!(parse_connector_id(Some("4294967296")), None);
    }
}
