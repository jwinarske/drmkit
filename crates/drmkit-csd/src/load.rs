// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! Reading a theme from TOML.

use crate::{Button, Buttons, Color, ColorError, Colors, Theme};

/// Why a theme could not be read.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ThemeError {
    /// The file could not be read.
    #[error("reading {path}: {source}")]
    Io {
        /// What was asked for.
        path: String,
        /// Why not.
        source: std::io::Error,
    },

    /// The text is not TOML.
    #[error("parsing the theme: {0}")]
    Syntax(String),

    /// A field held the wrong kind of value.
    ///
    /// Named, because a theme file is hand-written and "invalid argument" over
    /// forty fields is not something a person can act on.
    #[error("{field}: expected {expected}")]
    WrongType {
        /// Which field.
        field: String,
        /// What it should have been.
        expected: &'static str,
    },

    /// A colour was not readable.
    #[error("{field}: {source}")]
    BadColor {
        /// Which field.
        field: String,
        /// Why not.
        source: ColorError,
    },
}

/// Load a theme from TOML text, filling anything absent from `base`.
///
/// Inheritance is what makes a theme file worth hand-writing: a user who wants
/// a different accent colour writes three lines, not forty. `base` is
/// typically [`glass_default`](crate::glass_default).
///
/// # Errors
///
/// [`ThemeError::Syntax`] if the text is not TOML, [`ThemeError::WrongType`]
/// if a field holds the wrong kind of value, and [`ThemeError::BadColor`] if a
/// colour is not `#RRGGBB` or `#RRGGBBAA` — each naming the field, since a
/// theme file has forty of them and a caller cannot bisect a hand-written one.
pub fn load_theme_str(text: &str, base: Theme) -> Result<Theme, ThemeError> {
    // `from_str`, not `str::parse`: the `FromStr` impl on `Value` parses a
    // single TOML *value*, so a whole document is rejected as trailing
    // content -- and the error says "unexpected content", which reads like a
    // problem with the theme rather than with how it was parsed.
    let doc: toml::Value = toml::from_str(text)
        .map_err(|error: toml::de::Error| ThemeError::Syntax(error.to_string()))?;

    let mut theme = base;
    read_string(&doc, "name", &mut theme.name)?;
    read_int(&doc, "corner_radius", &mut theme.corner_radius)?;
    read_float(&doc, "noise_amplitude", &mut theme.noise_amplitude)?;
    read_int(&doc, "shadow_extent", &mut theme.shadow_extent)?;
    read_int(
        &doc,
        "animation_duration_ms",
        &mut theme.animation_duration_ms,
    )?;

    if let Some(bar) = doc.get("title_bar") {
        read_int(bar, "height", &mut theme.title_bar.height)?;
        read_string(bar, "font", &mut theme.title_bar.font)?;
        read_int(bar, "font_size", &mut theme.title_bar.font_size)?;
    }

    if let Some(colors) = doc.get("colors") {
        let Colors {
            panel_top,
            panel_bottom,
            rim_focused,
            rim_blurred,
            shadow,
            title_text,
            title_shadow,
        } = &mut theme.colors;
        read_color(colors, "panel_top", panel_top)?;
        read_color(colors, "panel_bottom", panel_bottom)?;
        read_color(colors, "rim_focused", rim_focused)?;
        read_color(colors, "rim_blurred", rim_blurred)?;
        read_color(colors, "shadow", shadow)?;
        read_color(colors, "title_text", title_text)?;
        read_color(colors, "title_shadow", title_shadow)?;
    }

    if let Some(buttons) = doc.get("buttons") {
        let Buttons {
            close,
            minimize,
            maximize,
        } = &mut theme.buttons;
        read_button(buttons, "close", close)?;
        read_button(buttons, "minimize", minimize)?;
        read_button(buttons, "maximize", maximize)?;
    }

    Ok(theme)
}

/// Load a theme from a file, filling anything absent from `base`.
///
/// # Errors
///
/// [`ThemeError::Io`] if the file cannot be read, and otherwise as
/// [`load_theme_str`].
pub fn load_theme_file(path: &str, base: Theme) -> Result<Theme, ThemeError> {
    let text = std::fs::read_to_string(path).map_err(|source| ThemeError::Io {
        path: path.to_owned(),
        source,
    })?;
    load_theme_str(&text, base)
}

/// Read one string field, leaving the base value if it is absent.
fn read_string(table: &toml::Value, field: &str, into: &mut String) -> Result<(), ThemeError> {
    let Some(value) = table.get(field) else {
        return Ok(());
    };
    let text = value.as_str().ok_or_else(|| ThemeError::WrongType {
        field: field.to_owned(),
        expected: "a string",
    })?;
    into.clear();
    into.push_str(text);
    Ok(())
}

/// Read one integer field.
fn read_int(table: &toml::Value, field: &str, into: &mut i32) -> Result<(), ThemeError> {
    let Some(value) = table.get(field) else {
        return Ok(());
    };
    let wrong = || ThemeError::WrongType {
        field: field.to_owned(),
        expected: "an integer",
    };
    *into = i32::try_from(value.as_integer().ok_or_else(wrong)?).map_err(|_| wrong())?;
    Ok(())
}

/// Read one float field.
///
/// An integer is accepted where a float is expected: `noise_amplitude = 0` is
/// what a theme author writes for none, and TOML types it as an integer.
fn read_float(table: &toml::Value, field: &str, into: &mut f64) -> Result<(), ThemeError> {
    let Some(value) = table.get(field) else {
        return Ok(());
    };
    *into = match value {
        toml::Value::Float(float) => *float,
        // A theme author writes `noise_amplitude = 0` for none, and TOML
        // types that as an integer. Anything a theme carries is far inside
        // what an f64 represents exactly, so the conversion is lossless here
        // even though the types say it need not be in general.
        toml::Value::Integer(int) => {
            f64::from(i32::try_from(*int).map_err(|_| ThemeError::WrongType {
                field: field.to_owned(),
                expected: "a number a theme could use",
            })?)
        }
        _ => {
            return Err(ThemeError::WrongType {
                field: field.to_owned(),
                expected: "a number",
            });
        }
    };
    Ok(())
}

/// Read one colour field, reporting it by its own name.
fn read_color(table: &toml::Value, field: &str, into: &mut Color) -> Result<(), ThemeError> {
    read_color_as(table, field, field, into)
}

/// Read the colour at `key`, reporting it as `label`.
///
/// The two differ inside a nested table: a button's colour is keyed `fill`
/// but is worth reporting as `buttons.close.fill`, because that is what the
/// author has to find in the file. Looking it up by the qualified name
/// instead finds nothing — which is not an error, since an absent field
/// inherits, so the colour silently stays at the base value.
fn read_color_as(
    table: &toml::Value,
    key: &str,
    label: &str,
    into: &mut Color,
) -> Result<(), ThemeError> {
    let Some(value) = table.get(key) else {
        return Ok(());
    };
    let text = value.as_str().ok_or_else(|| ThemeError::WrongType {
        field: label.to_owned(),
        expected: "a #RRGGBB or #RRGGBBAA string",
    })?;
    *into = Color::from_hex(text).map_err(|source| ThemeError::BadColor {
        field: label.to_owned(),
        source,
    })?;
    Ok(())
}

/// Read one button's two colours.
fn read_button(table: &toml::Value, field: &str, into: &mut Button) -> Result<(), ThemeError> {
    let Some(button) = table.get(field) else {
        return Ok(());
    };
    read_color_as(
        button,
        "fill",
        &format!("buttons.{field}.fill"),
        &mut into.fill,
    )?;
    read_color_as(
        button,
        "hover",
        &format!("buttons.{field}.hover"),
        &mut into.hover,
    )
}
