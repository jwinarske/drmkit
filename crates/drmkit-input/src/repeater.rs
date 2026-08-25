// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! Auto-repeat on a timerfd, for a caller that already has an event loop.

use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::time::Duration;

use rustix::time::{
    Itimerspec, TimerfdClockId, TimerfdFlags, TimerfdTimerFlags, Timespec, timerfd_create,
    timerfd_settime,
};

use crate::InputError;
use crate::event::KeyEvent;
use crate::repeat::{Repeat, RepeatConfig, Timer};

/// Auto-repeat driven by a timer the caller polls.
///
/// Register [`fd`](Self::fd) on the same epoll as everything else, feed every
/// real key event to [`on_key`](Self::on_key), and call
/// [`dispatch`](Self::dispatch) when the descriptor is readable.
///
/// The decisions live in [`Repeat`]; this is the timer around them.
#[derive(Debug)]
pub struct KeyRepeater {
    repeat: Repeat,
    timer: OwnedFd,
}

impl KeyRepeater {
    /// Create a repeater with its own timer.
    ///
    /// # Errors
    ///
    /// [`InputError::Config`] for a zero interval, which would leave the timer
    /// firing as fast as the kernel can wake the loop. A zero *delay* is
    /// allowed and means the first repeat lands on the next tick.
    ///
    /// [`InputError::Timer`] if the timer cannot be created.
    pub fn new(config: RepeatConfig) -> Result<Self, InputError> {
        if config.interval.is_zero() {
            return Err(InputError::Config("the repeat interval cannot be zero"));
        }
        let timer = timerfd_create(
            TimerfdClockId::Monotonic,
            TimerfdFlags::NONBLOCK | TimerfdFlags::CLOEXEC,
        )
        .map_err(|errno| InputError::Timer(std::io::Error::from(errno)))?;
        Ok(Self {
            repeat: Repeat::new(config),
            timer,
        })
    }

    /// The descriptor to poll. Readable when a repeat is due.
    #[must_use]
    pub fn fd(&self) -> BorrowedFd<'_> {
        self.timer.as_fd()
    }

    /// The key currently repeating.
    #[must_use]
    pub const fn held_key(&self) -> Option<u32> {
        self.repeat.held_key()
    }

    /// Feed a real key event.
    ///
    /// `repeats` is the keymap's answer for this key; see
    /// [`Repeat::on_key`].
    pub fn on_key(&mut self, event: &KeyEvent, repeats: bool) {
        let command = self.repeat.on_key(event, repeats);
        self.apply(self.repeat.config(), command);
    }

    /// Stop repeating, and stop the timer.
    pub fn cancel(&mut self) {
        let command = self.repeat.cancel();
        self.apply(self.repeat.config(), command);
    }

    /// Drain the timer and append whatever repeats are due.
    ///
    /// Nothing is appended if no key is held, or if the descriptor had nothing
    /// to say. The events carry only the key -- run them through
    /// [`Keyboard::resolve`](crate::Keyboard::resolve) for `sym` and `utf8`.
    ///
    /// # Errors
    ///
    /// [`InputError::Timer`] if the timer cannot be read. `EAGAIN` is not one:
    /// a descriptor that was not ready simply produces nothing.
    pub fn dispatch(&mut self, time_ms: u32, out: &mut Vec<KeyEvent>) -> Result<(), InputError> {
        let mut buf = [0_u8; 8];
        let read = match rustix::io::read(&self.timer, &mut buf) {
            Ok(read) => read,
            Err(rustix::io::Errno::AGAIN) => return Ok(()),
            Err(errno) => return Err(InputError::Timer(std::io::Error::from(errno))),
        };
        if read != buf.len() {
            // The kernel writes the count as one u64 or nothing at all, so a
            // short read is not a thing that happens; treating it as "no
            // expirations" is the safe reading if it ever does.
            return Ok(());
        }

        let dropped = self
            .repeat
            .synthesize(u64::from_ne_bytes(buf), time_ms, out);
        if dropped > 0 {
            drmkit_log::log_debug!("dropped {dropped} repeat tick(s): the loop is behind");
        }
        Ok(())
    }

    fn apply(&self, config: RepeatConfig, command: Timer) {
        let spec = match command {
            Timer::Unchanged => return,
            Timer::Disarm => Itimerspec {
                it_value: Timespec {
                    tv_sec: 0,
                    tv_nsec: 0,
                },
                it_interval: Timespec {
                    tv_sec: 0,
                    tv_nsec: 0,
                },
            },
            Timer::Arm => Itimerspec {
                // An it_value of zero means "disarm" to the kernel, which
                // would quietly turn a zero delay into no repeats at all.
                it_value: timespec(config.delay.max(Duration::from_nanos(1))),
                it_interval: timespec(config.interval),
            },
        };
        // Nothing here can fail: the descriptor is one this owns and the spec
        // is in range by construction. A failure would mean a key that stops
        // repeating, which is not worth an error on every keystroke.
        if let Err(errno) = timerfd_settime(&self.timer, TimerfdTimerFlags::empty(), &spec) {
            drmkit_log::log_warn!("arming the repeat timer failed: {errno}");
        }
    }
}

fn timespec(duration: Duration) -> Timespec {
    Timespec {
        tv_sec: i64::try_from(duration.as_secs()).unwrap_or(i64::MAX),
        tv_nsec: i64::from(duration.subsec_nanos()),
    }
}
