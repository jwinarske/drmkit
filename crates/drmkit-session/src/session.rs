// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! The session state machine, over whatever seat is underneath.

use std::collections::VecDeque;
use std::os::fd::RawFd;
use std::path::{Path, PathBuf};

use crate::SessionError;

/// A seat's handle for one open device.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DeviceId(pub i32);

/// A device this session is holding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TakenDevice {
    /// Where it was opened from. Kept because a resume reopens by path.
    pub path: PathBuf,
    /// The seat's handle, for closing it.
    pub id: DeviceId,
    /// The descriptor as it stands now. Replaced by a resume.
    ///
    /// Borrowed, not owned: the seat closes it, and it does so at a moment of
    /// its choosing. Closing it here leaves the seat holding a handle to a
    /// descriptor number the process has since reused for something else.
    pub fd: RawFd,
}

/// What a seat has to be able to do.
///
/// Deliberately small. Everything about *when* to call these -- and what state
/// the session is in between them -- is [`Session`]'s, so it can be tested
/// against a fake on a machine with no seat.
pub trait Backend {
    /// Open a device the seat controls.
    ///
    /// # Errors
    ///
    /// Whatever the seat says, as [`SessionError::Seat`].
    fn open_device(&mut self, path: &Path) -> Result<(DeviceId, RawFd), SessionError>;

    /// Close one this session opened.
    ///
    /// # Errors
    ///
    /// As [`Backend::open_device`].
    fn close_device(&mut self, id: DeviceId) -> Result<(), SessionError>;

    /// Tell the seat the pause has been acted on.
    ///
    /// Until this returns, the seat is waiting: the switch does not complete
    /// and the user is looking at a display that has not changed.
    ///
    /// # Errors
    ///
    /// As [`Backend::open_device`].
    fn acknowledge_pause(&mut self) -> Result<(), SessionError>;

    /// Ask to switch to another session, by VT number.
    ///
    /// # Errors
    ///
    /// As [`Backend::open_device`].
    fn switch_session(&mut self, session: i32) -> Result<(), SessionError>;

    /// A descriptor that becomes readable when the seat has something to say.
    fn poll_fd(&self) -> Option<RawFd>;

    /// Let the seat deliver whatever it has, appending to `events`.
    ///
    /// # Errors
    ///
    /// As [`Backend::open_device`].
    fn dispatch(&mut self, timeout_ms: i32, events: &mut Vec<RawEvent>)
    -> Result<(), SessionError>;
}

/// What a seat says, before the session has made sense of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RawEvent {
    /// The session has become active.
    Enable,
    /// The session is about to become inactive.
    Disable,
}

/// What a caller needs to act on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionEvent {
    /// Stop touching every device now.
    ///
    /// The descriptors are about to stop working. Anything mapped from them
    /// has to be released by the caller -- closing a descriptor does not unmap
    /// it, so a compositor that skips that leaks per switch.
    Paused,
    /// The devices are back, on new descriptors.
    ///
    /// Every per-descriptor kernel object is gone: framebuffers, property
    /// blobs, client capabilities, GEM handles. All of it is rebuilt.
    Resumed {
        /// The devices as they now stand, in the order they were taken.
        devices: Vec<TakenDevice>,
    },
}

/// Devices held across VT switches.
#[derive(Debug)]
pub struct Session<B: Backend> {
    backend: B,
    devices: Vec<TakenDevice>,
    active: bool,
    pending: VecDeque<SessionEvent>,
    raw: Vec<RawEvent>,
}

impl<B: Backend> Session<B> {
    /// Wrap a seat that is currently active.
    pub fn new(backend: B) -> Self {
        Self {
            backend,
            devices: Vec::new(),
            active: true,
            pending: VecDeque::new(),
            raw: Vec::new(),
        }
    }

    /// Whether the session currently owns the display.
    #[must_use]
    pub const fn is_active(&self) -> bool {
        self.active
    }

    /// The devices being held.
    #[must_use]
    pub fn devices(&self) -> &[TakenDevice] {
        &self.devices
    }

    /// The seat underneath, for tests to drive.
    #[cfg(test)]
    pub(crate) const fn backend(&self) -> &B {
        &self.backend
    }

    /// As [`Session::backend`].
    #[cfg(test)]
    pub(crate) const fn backend_mut(&mut self) -> &mut B {
        &mut self.backend
    }

    /// A descriptor to poll for seat activity.
    #[must_use]
    pub fn poll_fd(&self) -> Option<RawFd> {
        self.backend.poll_fd()
    }

    /// Open a device through the seat and hold it.
    ///
    /// Opening the same path twice returns what is already held rather than a
    /// second descriptor: the seat would give one, and then a resume would
    /// have two to reopen and the caller two to keep straight.
    ///
    /// # Errors
    ///
    /// - [`SessionError::Inactive`] while the session does not own the
    ///   display. The seat would refuse anyway; failing here says why.
    /// - [`SessionError::Seat`] if the seat refuses.
    pub fn take_device(&mut self, path: impl AsRef<Path>) -> Result<TakenDevice, SessionError> {
        let path = path.as_ref();
        if !self.active {
            return Err(SessionError::Inactive);
        }
        if let Some(existing) = self.devices.iter().find(|device| device.path == path) {
            return Ok(existing.clone());
        }
        let (id, fd) = self.backend.open_device(path)?;
        let device = TakenDevice {
            path: path.to_path_buf(),
            id,
            fd,
        };
        self.devices.push(device.clone());
        Ok(device)
    }

    /// Give a device back to the seat.
    ///
    /// # Errors
    ///
    /// - [`SessionError::UnknownDevice`] if this session did not open it.
    /// - [`SessionError::Seat`] if the seat refuses.
    pub fn release_device(&mut self, path: impl AsRef<Path>) -> Result<(), SessionError> {
        let path = path.as_ref();
        let index = self
            .devices
            .iter()
            .position(|device| device.path == path)
            .ok_or(SessionError::UnknownDevice)?;
        let device = self.devices.remove(index);
        self.backend.close_device(device.id)
    }

    /// Ask to switch to another session, by VT number.
    ///
    /// Asynchronous: the seat pauses this session when it honours the request,
    /// and resumes it when the user comes back. Useful where the process owns
    /// the TTY in graphics mode and the kernel is no longer handling
    /// Ctrl+Alt+F<n> itself.
    ///
    /// # Errors
    ///
    /// [`SessionError::Seat`] if the seat refuses.
    pub fn switch_session(&mut self, session: i32) -> Result<(), SessionError> {
        self.backend.switch_session(session)
    }

    /// Let the seat deliver what it has, turning it into events to act on.
    ///
    /// A negative timeout blocks. Call [`Session::next_event`] afterwards.
    ///
    /// The reference runs the caller's callbacks synchronously from inside
    /// this call, which means the caller is re-entering the seat from inside
    /// the seat's own dispatch. Queuing instead keeps that from being possible.
    ///
    /// # Errors
    ///
    /// [`SessionError::Seat`] if the seat refuses, or if reopening a device
    /// on resume fails.
    pub fn dispatch(&mut self, timeout_ms: i32) -> Result<(), SessionError> {
        self.raw.clear();
        let mut raw = std::mem::take(&mut self.raw);
        let result = self.backend.dispatch(timeout_ms, &mut raw);
        for event in raw.drain(..) {
            match event {
                RawEvent::Disable => self.on_disable()?,
                RawEvent::Enable => self.on_enable()?,
            }
        }
        self.raw = raw;
        result
    }

    /// The next thing the caller has to act on.
    pub fn next_event(&mut self) -> Option<SessionEvent> {
        self.pending.pop_front()
    }

    /// Handle the seat going away.
    fn on_disable(&mut self) -> Result<(), SessionError> {
        if !self.active {
            // A seat that says it twice is not a reason to tell the caller
            // twice; the caller has already stopped.
            return Ok(());
        }
        self.active = false;
        drmkit_log::log_info!(
            "session paused: {} device(s) are no longer ours",
            self.devices.len()
        );
        self.pending.push_back(SessionEvent::Paused);
        // Acknowledged immediately. The queued event is what the caller acts
        // on, and holding the acknowledgement until they have would stall the
        // switch on a display that has already stopped being ours.
        self.backend.acknowledge_pause()
    }

    /// Handle the seat coming back.
    fn on_enable(&mut self) -> Result<(), SessionError> {
        if self.active {
            return Ok(());
        }
        self.active = true;

        // Every descriptor is replaced. The pessimistic reading, and the right
        // one: the capability-revoking backends leave the descriptor valid but
        // reset per-descriptor state anyway, so a caller that reused it would
        // be working from objects the kernel has forgotten.
        let mut refreshed = Vec::with_capacity(self.devices.len());
        for device in std::mem::take(&mut self.devices) {
            // Closing first: the seat tracks one open device per handle, and
            // opening the same path again without closing leaks its handle for
            // as long as the session runs.
            self.backend.close_device(device.id)?;
            let (id, fd) = self
                .backend
                .open_device(&device.path)
                .inspect_err(|error| {
                    // The caller is told too, by the error out of `dispatch`. This
                    // says which device, which is the part that identifies why.
                    drmkit_log::log_error!("reopening {} failed: {error}", device.path.display());
                })?;
            refreshed.push(TakenDevice {
                path: device.path,
                id,
                fd,
            });
        }
        drmkit_log::log_info!("session resumed: {} device(s) reopened", refreshed.len());
        self.devices.clone_from(&refreshed);
        self.pending
            .push_back(SessionEvent::Resumed { devices: refreshed });
        Ok(())
    }
}
