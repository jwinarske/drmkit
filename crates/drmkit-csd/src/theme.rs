// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! What decorations look like.

use crate::Color;

/// The title bar's geometry and type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TitleBar {
    /// Height in pixels.
    pub height: i32,
    /// A CSS-style font stack.
    pub font: String,
    /// Point size.
    pub font_size: i32,
}

/// Every colour a decoration uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Colors {
    /// Top of the panel gradient.
    pub panel_top: Color,
    /// Bottom of it.
    pub panel_bottom: Color,
    /// The edge highlight while the window has focus.
    pub rim_focused: Color,
    /// And while it does not — the cue that tells a user which window their
    /// keystrokes are going to.
    pub rim_blurred: Color,
    /// The drop shadow.
    pub shadow: Color,
    /// Title text.
    pub title_text: Color,
    /// The text's own shadow, for legibility over a translucent panel.
    pub title_shadow: Color,
}

/// One button's two states.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Button {
    /// At rest.
    pub fill: Color,
    /// Under the pointer.
    pub hover: Color,
}

/// The three window buttons.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Buttons {
    /// Close.
    pub close: Button,
    /// Minimize.
    pub minimize: Button,
    /// Maximize.
    pub maximize: Button,
}

/// A complete decoration theme.
#[derive(Debug, Clone, PartialEq)]
pub struct Theme {
    /// What to call it.
    pub name: String,
    /// Corner rounding, in pixels.
    pub corner_radius: i32,
    /// The title bar.
    pub title_bar: TitleBar,
    /// Colours.
    pub colors: Colors,
    /// Buttons.
    pub buttons: Buttons,
    /// How much dither to mix into the panel gradient, 0 to 1.
    ///
    /// A flat gradient across a wide title bar bands visibly at 8 bits per
    /// channel; a little noise breaks the bands up. Costs a per-pixel
    /// multiply, which is why a cheaper theme turns it down.
    pub noise_amplitude: f64,
    /// How far the shadow reaches, in pixels. Zero means none.
    pub shadow_extent: i32,
    /// How long a decoration animation runs. Zero means none.
    pub animation_duration_ms: i32,
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            name: String::new(),
            corner_radius: 0,
            title_bar: TitleBar {
                height: 0,
                font: String::new(),
                font_size: 0,
            },
            colors: Colors::default(),
            buttons: Buttons::default(),
            noise_amplitude: 0.0,
            shadow_extent: 0,
            animation_duration_ms: 0,
        }
    }
}

/// The reference theme.
#[must_use]
pub fn glass_default() -> Theme {
    Theme {
        name: "glass-default".to_owned(),
        corner_radius: 8,
        title_bar: TitleBar {
            height: 28,
            font: "Inter, sans-serif".to_owned(),
            font_size: 13,
        },
        colors: Colors {
            panel_top: Color::new(0xFF, 0xFF, 0xFF, 0x73),
            panel_bottom: Color::new(0xFF, 0xFF, 0xFF, 0x26),
            rim_focused: Color::new(0xFF, 0xFF, 0xFF, 0x99),
            rim_blurred: Color::new(0xC8, 0xC8, 0xC8, 0x4C),
            shadow: Color::new(0x00, 0x00, 0x00, 0x8C),
            title_text: Color::new(0x1E, 0x1E, 0x1E, 0xFF),
            title_shadow: Color::new(0xFF, 0xFF, 0xFF, 0x66),
        },
        buttons: Buttons {
            close: Button {
                fill: Color::new(0xFF, 0x5F, 0x56, 0xFF),
                hover: Color::new(0xFF, 0x8A, 0x82, 0xFF),
            },
            minimize: Button {
                fill: Color::new(0xFF, 0xBD, 0x2E, 0xFF),
                hover: Color::new(0xFF, 0xD1, 0x66, 0xFF),
            },
            maximize: Button {
                fill: Color::new(0x28, 0xC9, 0x40, 0xFF),
                hover: Color::new(0x5E, 0xE0, 0x71, 0xFF),
            },
        },
        noise_amplitude: 0.04,
        shadow_extent: 24,
        animation_duration_ms: 180,
    }
}

/// The same look, cheaper to draw.
///
/// Less shadow, less noise, shorter animations — the three costs that scale
/// with area and frame count. For a device where the decoration competes with
/// the application for the same CPU.
#[must_use]
pub fn glass_lite() -> Theme {
    Theme {
        name: "glass-lite".to_owned(),
        noise_amplitude: 0.02,
        shadow_extent: 12,
        animation_duration_ms: 120,
        ..glass_default()
    }
}

/// No shadow, no animation, no noise.
///
/// The shadow colour is cleared as well as the extent. Either alone would
/// stop it drawing, but leaving a visible colour behind a zero extent invites
/// a caller that raises the extent to inherit a shadow it never chose.
#[must_use]
pub fn glass_minimal() -> Theme {
    let base = glass_default();
    Theme {
        name: "glass-minimal".to_owned(),
        noise_amplitude: 0.0,
        shadow_extent: 0,
        animation_duration_ms: 0,
        colors: Colors {
            shadow: Color::default(),
            ..base.colors
        },
        ..base
    }
}
