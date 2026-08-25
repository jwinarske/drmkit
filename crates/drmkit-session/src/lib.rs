// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! Holding devices across a VT switch.
//!
//! A compositor that owns the display cannot simply open `/dev/dri/card0` and
//! keep it: switching to another virtual terminal has to take the display away
//! and give it back. A seat -- logind, seatd, or a bare-root fallback -- is
//! what arbitrates that, handing out descriptors it can revoke.
//!
//! What arrives from the seat is two events. *Paused*: stop touching every
//! descriptor immediately, because the ioctls will start failing. *Resumed*:
//! here are fresh descriptors, and every per-descriptor kernel object you had
//! -- framebuffers, property blobs, client caps, GEM handles -- is gone.
//! [`drmkit_cursor::Paused`] is the shape that pairs with this on the other
//! side.
//!
//! # Why the backend is a trait
//!
//! Because a seat cannot be opened on a machine that has no session. Over SSH,
//! in a container, on a CI runner, logind says the session is busy, seatd is
//! not running, and the builtin backend wants root and a TTY. That is most
//! places this code will be built.
//!
//! So the sequencing -- which devices are held, what happens to them across a
//! pause, when the pause is acknowledged, what a caller is allowed to do while
//! inactive -- lives in [`Session`] over a [`Backend`], and is tested against
//! a fake. The libseat binding is a thin adapter behind the `libseat` feature.

#![cfg_attr(docsrs, feature(doc_cfg))]

mod session;

#[cfg(feature = "libseat")]
mod seat;

#[cfg(test)]
mod tests;

pub use session::{Backend, DeviceId, RawEvent, Session, SessionEvent, TakenDevice};

/// What can go wrong holding a session.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum SessionError {
    /// No seat backend is available.
    ///
    /// No logind session, no seatd, and no permission for the builtin
    /// fallback. A caller can still open devices directly; they simply will
    /// not survive a VT switch, which is the right trade for a tool that is
    /// not a compositor.
    #[error("no seat backend is available")]
    NoBackend,

    /// The seat is not active, so devices cannot be opened.
    ///
    /// Between a pause and the resume that follows it. Not an error in the
    /// usual sense -- it is the session working -- so a caller waits rather
    /// than gives up.
    #[error("the seat is not active")]
    Inactive,

    /// The device is not one this session opened.
    #[error("no such device in this session")]
    UnknownDevice,

    /// The seat refused.
    #[error("seat: {0}")]
    Seat(String),
}
