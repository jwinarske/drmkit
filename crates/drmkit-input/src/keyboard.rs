// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! Turning evdev keycodes into keysyms and text, through xkb.

use std::path::Path;

use xkbcommon::xkb;

use crate::InputError;
use crate::event::KeyEvent;
use crate::leds::Leds;

/// xkb numbers keycodes eight higher than evdev does, for X11's sake.
const XKB_OFFSET: u32 = 8;

/// The lock keys, in evdev codes.
///
/// Layout-independent: RMLVO remaps what a key *produces*, never which
/// physical key carries which code.
const KEY_CAPS_LOCK: u32 = 58;
const KEY_NUM_LOCK: u32 = 69;
const KEY_SCROLL_LOCK: u32 = 70;

/// The names a keymap is compiled from: rules, model, layout, variant,
/// options.
///
/// An empty field means "whatever the system defaults to", which is what
/// xkb does with a null one.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct KeymapNames {
    /// The rules file that maps the rest onto keymap components.
    pub rules: String,
    /// The keyboard model.
    pub model: String,
    /// The layout, such as `us` or `de`.
    pub layout: String,
    /// The layout variant, such as `dvorak`.
    pub variant: String,
    /// Comma-separated options, such as `ctrl:nocaps`.
    pub options: String,
}

/// Which modifiers are in effect.
///
/// Four independent bools rather than a state enum: every combination of
/// these is reachable and meaningful, which is the case the
/// `struct_excessive_bools` lint is not about.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Modifiers {
    /// Shift.
    pub shift: bool,
    /// Control.
    pub ctrl: bool,
    /// Alt.
    pub alt: bool,
    /// The Super, Windows, or Command key.
    pub logo: bool,
}

/// A keymap and the state of the keys pressed against it.
///
/// One per seat, not one per device: a user typing on two keyboards is
/// holding Shift on one and pressing a letter on the other, and xkb has to
/// see both.
pub struct Keyboard {
    context: xkb::Context,
    keymap: xkb::Keymap,
    state: xkb::State,
    /// Evdev codes currently down, in press order.
    ///
    /// Kept so [`Keyboard::reload`] can replay them onto a fresh state: a
    /// layout swap in the middle of a keystroke should not strand the key
    /// that is still held.
    held: Vec<u32>,
}

impl Keyboard {
    /// Compile a keymap from RMLVO names.
    ///
    /// # Errors
    ///
    /// [`InputError::Keymap`] if the names do not compile -- a layout that is
    /// not installed, an option that does not exist.
    pub fn from_names(names: &KeymapNames) -> Result<Self, InputError> {
        let context = Self::context();
        let keymap = compile(&context, names)?;
        Ok(Self::with(context, keymap))
    }

    /// Compile a keymap from a file.
    ///
    /// # Errors
    ///
    /// [`InputError::Io`] if the file cannot be read, [`InputError::Keymap`]
    /// if it does not compile.
    pub fn from_file(path: &Path) -> Result<Self, InputError> {
        let text = std::fs::read_to_string(path).map_err(|source| InputError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        Self::from_string(&text)
    }

    /// Compile a keymap from text already in hand.
    ///
    /// For a keymap that arrived over a socket or through an mmap and never
    /// touched a filesystem.
    ///
    /// # Errors
    ///
    /// [`InputError::Keymap`] if it does not compile.
    pub fn from_string(keymap: &str) -> Result<Self, InputError> {
        let context = Self::context();
        let keymap = xkb::Keymap::new_from_string(
            &context,
            keymap.to_owned(),
            xkb::KEYMAP_FORMAT_TEXT_V1,
            xkb::KEYMAP_COMPILE_NO_FLAGS,
        )
        .ok_or(InputError::Keymap)?;
        Ok(Self::with(context, keymap))
    }

    /// A context whose diagnostics follow drmkit's log level.
    ///
    /// xkb writes its own to stderr through a handler this crate cannot
    /// replace -- `xkb_context_set_log_fn` is not bound -- so a consumer that
    /// installed a sink would still find keymap errors going somewhere else.
    /// The level is the one knob there is.
    ///
    /// Only the ends of the range move it: asking drmkit for silence should
    /// silence xkb too, and asking for debug should get xkb's. In between it
    /// keeps xkb's own default, because the middle levels there are mostly
    /// "which include paths I tried", which is noise on the way to a keymap
    /// that compiled fine.
    fn context() -> xkb::Context {
        let mut context = xkb::Context::new(xkb::CONTEXT_NO_FLAGS);
        context.set_log_level(match drmkit_log::log_level() {
            drmkit_log::LogLevel::Silent => xkb::LogLevel::Critical,
            drmkit_log::LogLevel::Debug => xkb::LogLevel::Debug,
            _ => xkb::LogLevel::Error,
        });
        context
    }

    fn with(context: xkb::Context, keymap: xkb::Keymap) -> Self {
        let state = xkb::State::new(&keymap);
        Self {
            context,
            keymap,
            state,
            held: Vec::new(),
        }
    }

    /// Swap in a keymap compiled from different names.
    ///
    /// Keys still held are replayed onto the new state, and the lock latch is
    /// carried over, so a layout change mid-keystroke does not strand a
    /// modifier down or turn Caps Lock off underneath the user.
    ///
    /// The current keymap is left alone if the new names do not compile.
    ///
    /// # Errors
    ///
    /// [`InputError::Keymap`], as [`Keyboard::from_names`].
    pub fn reload(&mut self, names: &KeymapNames) -> Result<(), InputError> {
        let keymap = compile(&self.context, names)?;
        let mut state = xkb::State::new(&keymap);

        let leds = self.leds();
        for key in &self.held {
            state.update_key(keycode(*key), xkb::KeyDirection::Down);
        }
        self.keymap = keymap;
        self.state = state;
        // After the replay, not before: replaying a held lock key toggles its
        // latch, and what the user had is what they should still have.
        self.set_leds(leds);
        Ok(())
    }

    /// Fill in what a key resolves to, without touching the keyboard state.
    ///
    /// This is what a repeat wants: the same physical key, re-read against
    /// whatever modifiers are held *now*, so pressing Shift partway through a
    /// held key changes what the next repeat types.
    pub fn resolve(&self, event: &mut KeyEvent) {
        let code = keycode(event.key);
        event.sym = self.state.key_get_one_sym(code).raw();
        event.utf8 = self.state.key_get_utf8(code);
    }

    /// Resolve a key event and fold it into the keyboard state.
    ///
    /// Resolution happens *before* the state update, which is the order
    /// xkbcommon documents: a key's own press must not change what that press
    /// produces. With a latching modifier -- a sticky Shift, a dead key -- the
    /// press being resolved is the one that consumes the latch, so reading
    /// afterwards reads a state the user has already left. The reference does
    /// it the other way round; see the module tests.
    pub fn process_key(&mut self, event: &mut KeyEvent) {
        self.resolve(event);

        let code = keycode(event.key);
        let direction = if event.pressed {
            xkb::KeyDirection::Down
        } else {
            xkb::KeyDirection::Up
        };
        self.state.update_key(code, direction);

        if event.pressed {
            if !self.held.contains(&event.key) {
                self.held.push(event.key);
            }
        } else {
            self.held.retain(|key| *key != event.key);
        }
    }

    /// Release everything held, keeping the lock state.
    ///
    /// For a session coming back from a VT switch, which is a thing the user
    /// does *by holding Ctrl and Alt*. Those keys come up while the device is
    /// revoked, and whether their releases survive the round trip is
    /// libinput's business rather than something to depend on -- so the state
    /// is squared away on resume instead. Getting it wrong the other way is a
    /// session where every keystroke is a Ctrl chord until the user presses
    /// and releases Ctrl again.
    ///
    /// Locks survive: Caps Lock is not something the user let go of.
    pub fn release_all(&mut self) {
        let leds = self.leds();
        for key in std::mem::take(&mut self.held) {
            self.state.update_key(keycode(key), xkb::KeyDirection::Up);
        }
        self.set_leds(leds);
    }

    /// Whether the keymap says this key auto-repeats.
    ///
    /// False for modifiers and lock keys, true for letters, digits, arrows
    /// and function keys -- and it is the keymap's answer, not a guess.
    #[must_use]
    pub fn should_repeat(&self, key: u32) -> bool {
        self.keymap.key_repeats(keycode(key))
    }

    /// Which modifiers are in effect.
    #[must_use]
    pub fn modifiers(&self) -> Modifiers {
        Modifiers {
            shift: self.modifier(xkb::MOD_NAME_SHIFT),
            ctrl: self.modifier(xkb::MOD_NAME_CTRL),
            alt: self.modifier(xkb::MOD_NAME_ALT),
            logo: self.modifier(xkb::MOD_NAME_LOGO),
        }
    }

    /// The lock state.
    ///
    /// Compare two of these to spot a transition, then push the new one to
    /// the devices so the physical lights follow.
    #[must_use]
    pub fn leds(&self) -> Leds {
        Leds {
            caps_lock: self.state.led_name_is_active(xkb::LED_NAME_CAPS),
            num_lock: self.state.led_name_is_active(xkb::LED_NAME_NUM),
            scroll_lock: self.state.led_name_is_active(xkb::LED_NAME_SCROLL),
        }
    }

    /// Drive the lock state to `desired`.
    ///
    /// By synthesizing a press and release of each lock key that is on the
    /// wrong side, which is the only way to move a latch xkb owns. Produces
    /// no events and does not disturb the held-key set.
    ///
    /// For honouring a lock state from elsewhere -- what the console had, what
    /// the session manager reports -- and used internally by
    /// [`Keyboard::reload`].
    pub fn set_leds(&mut self, desired: Leds) {
        let current = self.leds();
        if desired.caps_lock != current.caps_lock {
            self.tap(KEY_CAPS_LOCK);
        }
        if desired.num_lock != current.num_lock {
            self.tap(KEY_NUM_LOCK);
        }
        if desired.scroll_lock != current.scroll_lock {
            self.tap(KEY_SCROLL_LOCK);
        }
    }

    /// The keys currently down, in evdev codes and press order.
    #[must_use]
    pub fn held_keys(&self) -> &[u32] {
        &self.held
    }

    /// The compiled keymap, for a caller that needs to hand it on.
    #[must_use]
    pub const fn keymap(&self) -> &xkb::Keymap {
        &self.keymap
    }

    fn modifier(&self, name: &str) -> bool {
        self.state
            .mod_name_is_active(name, xkb::STATE_MODS_EFFECTIVE)
    }

    fn tap(&mut self, key: u32) {
        let code = keycode(key);
        self.state.update_key(code, xkb::KeyDirection::Down);
        self.state.update_key(code, xkb::KeyDirection::Up);
    }
}

impl std::fmt::Debug for Keyboard {
    /// Nothing in xkb's handles is printable, so this reports what is
    /// actually useful: the layouts compiled in and what is held down.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Keyboard")
            .field("layouts", &self.keymap.layouts().collect::<Vec<_>>())
            .field("held", &self.held)
            .field("leds", &self.leds())
            .finish_non_exhaustive()
    }
}

/// An evdev code as xkb numbers it.
fn keycode(key: u32) -> xkb::Keycode {
    // Saturating rather than wrapping: a code near u32::MAX is not a key, and
    // wrapping would land it back down among the real ones.
    xkb::Keycode::new(key.saturating_add(XKB_OFFSET))
}

fn compile(context: &xkb::Context, names: &KeymapNames) -> Result<xkb::Keymap, InputError> {
    xkb::Keymap::new_from_names(
        context,
        names.rules.as_str(),
        names.model.as_str(),
        names.layout.as_str(),
        names.variant.as_str(),
        // The crate takes options as an owned Option because it has to append
        // its own NUL; an empty string means the same thing as None to xkb.
        Some(names.options.clone()),
        xkb::KEYMAP_COMPILE_NO_FLAGS,
    )
    .ok_or(InputError::Keymap)
}
