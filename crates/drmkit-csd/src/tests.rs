// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! Parity port of `tests/unit/test_csd_theme.cpp` from drm-cxx @ `4a0b64a`.

use crate::{Color, ColorError, Theme, ThemeError, glass_default, glass_lite, glass_minimal};
use crate::{load_theme_file, load_theme_str};

// --- colour -------------------------------------------------------------------

/// Six digits is a colour, and it is opaque.
///
/// The CSS convention, and the one a theme author expects. Defaulting to
/// transparent would make every colour written the short way invisible — a
/// theme that looks like it loaded and draws nothing.
#[test]
fn six_hex_digits_are_an_opaque_colour() {
    assert_eq!(
        Color::from_hex("#123456"),
        Ok(Color::new(0x12, 0x34, 0x56, 0xFF))
    );
}

/// Eight digits carries its own alpha.
#[test]
fn eight_hex_digits_carry_their_own_alpha() {
    assert_eq!(
        Color::from_hex("#12345678"),
        Ok(Color::new(0x12, 0x34, 0x56, 0x78))
    );
}

/// Case does not matter.
///
/// A hand-written theme file mixes both, and a rule against lowercase would
/// have nothing behind it.
#[test]
fn hex_digits_are_case_insensitive() {
    assert_eq!(
        Color::from_hex("#aabbccdd"),
        Color::from_hex("#AABBCCDD"),
        "the same colour written two ways is the same colour"
    );
    assert_eq!(
        Color::from_hex("#AaBbCcDd"),
        Ok(Color::new(0xAA, 0xBB, 0xCC, 0xDD))
    );
}

/// Anything else is refused, and says what it was given.
#[test]
fn malformed_colours_are_refused_by_what_they_were() {
    for bad in [
        "",           // nothing
        "123456",     // no hash
        "#12345",     // five digits
        "#1234567",   // seven
        "#123456789", // nine
        "#12345g",    // not hex
        "#GGGGGG",    // not hex, in the alpha-less length
    ] {
        assert_eq!(
            Color::from_hex(bad),
            Err(ColorError::Malformed {
                got: bad.to_owned()
            }),
            "{bad:?} should not parse"
        );
    }
}

/// Packing is `0xAARRGGBB`.
///
/// The layout an `ARGB8888` framebuffer word takes on a little-endian host,
/// which is what the renderer writes. Getting the channel order wrong here
/// swaps red and blue on every pixel drawn.
#[test]
fn packing_is_alpha_red_green_blue() {
    assert_eq!(
        Color::new(0x12, 0x34, 0x56, 0x78).packed_argb(),
        0x7812_3456
    );
    assert_eq!(
        Color::new(0xFF, 0x00, 0x00, 0xFF).packed_argb(),
        0xFFFF_0000,
        "opaque red, not opaque blue"
    );
}

/// Two colours are equal when every channel is.
#[test]
fn colour_equality_is_every_channel() {
    let base = Color::new(1, 2, 3, 4);
    assert_eq!(base, Color::new(1, 2, 3, 4));
    for other in [
        Color::new(9, 2, 3, 4),
        Color::new(1, 9, 3, 4),
        Color::new(1, 2, 9, 4),
        Color::new(1, 2, 3, 9),
    ] {
        assert_ne!(base, other, "a channel apart is a different colour");
    }
}

// --- loading ------------------------------------------------------------------

const COMPLETE: &str = r##"
name            = "round-trip-glass"
corner_radius   = 12
noise_amplitude = 0.06
shadow_extent   = 32
animation_duration_ms = 220

[title_bar]
height    = 30
font      = "Cantarell, sans-serif"
font_size = 14

[colors]
panel_top    = "#FFFFFFAA"
panel_bottom = "#FFFFFF22"
rim_focused  = "#AAAAAA88"
rim_blurred  = "#BBBBBB44"
shadow       = "#0000007F"
title_text   = "#11111111"
title_shadow = "#FFFFFFAA"

[buttons.close]
fill  = "#FF0000FF"
hover = "#FF6666FF"

[buttons.minimize]
fill  = "#FFFF00FF"
hover = "#FFFF99FF"

[buttons.maximize]
fill  = "#00FF00FF"
hover = "#99FF99FF"
"##;

/// Every field a theme has survives the round trip.
#[test]
fn a_complete_theme_round_trips() {
    let theme = load_theme_str(COMPLETE, Theme::default()).expect("valid TOML");

    assert_eq!(theme.name, "round-trip-glass");
    assert_eq!(theme.corner_radius, 12);
    assert!((theme.noise_amplitude - 0.06).abs() < f64::EPSILON);
    assert_eq!(theme.shadow_extent, 32);
    assert_eq!(theme.animation_duration_ms, 220);

    assert_eq!(theme.title_bar.height, 30);
    assert_eq!(theme.title_bar.font, "Cantarell, sans-serif");
    assert_eq!(theme.title_bar.font_size, 14);

    assert_eq!(theme.colors.panel_top, Color::new(0xFF, 0xFF, 0xFF, 0xAA));
    assert_eq!(theme.colors.shadow, Color::new(0x00, 0x00, 0x00, 0x7F));
    assert_eq!(theme.colors.title_text, Color::new(0x11, 0x11, 0x11, 0x11));

    assert_eq!(theme.buttons.close.fill, Color::new(0xFF, 0x00, 0x00, 0xFF));
    assert_eq!(
        theme.buttons.maximize.hover,
        Color::new(0x99, 0xFF, 0x99, 0xFF)
    );
}

/// What a file does not say, the base says.
///
/// This is what makes a theme file worth hand-writing: a user who wants a
/// different accent writes three lines rather than forty. A loader that reset
/// the rest to zero would give them a black window with no title bar.
#[test]
fn absent_fields_inherit_from_the_base() {
    let theme = load_theme_str(r#"name = "just-a-name""#, glass_default()).expect("valid TOML");
    let base = glass_default();

    assert_eq!(theme.name, "just-a-name", "what was said is taken");
    assert_eq!(
        theme.corner_radius, base.corner_radius,
        "and what was not is inherited"
    );
    assert_eq!(theme.title_bar, base.title_bar);
    assert_eq!(theme.colors, base.colors);
    assert_eq!(theme.buttons, base.buttons);
}

/// A partial table inherits the rest of that table too.
///
/// Not just the top level: overriding one colour must not clear the other six,
/// or a one-line accent change costs the whole palette.
#[test]
fn a_partial_table_inherits_its_siblings() {
    let theme = load_theme_str("[colors]\npanel_top = \"#01020304\"\n", glass_default())
        .expect("valid TOML");
    let base = glass_default();

    assert_eq!(theme.colors.panel_top, Color::new(1, 2, 3, 4));
    assert_eq!(
        theme.colors.shadow, base.colors.shadow,
        "the siblings of an overridden colour are not collateral"
    );
    assert_eq!(theme.colors.title_text, base.colors.title_text);
}

/// Text that is not TOML is refused as a syntax error.
#[test]
fn text_that_is_not_toml_is_a_syntax_error() {
    let error =
        load_theme_str("this is not = = toml", Theme::default()).expect_err("that is not TOML");
    assert!(matches!(error, ThemeError::Syntax(_)), "got {error}");
}

/// A colour that will not parse names its own field.
///
/// A theme has forty fields. "Invalid argument" leaves the author to bisect a
/// file by hand; naming the field is the difference between a fixable error
/// and a puzzle.
#[test]
fn a_bad_colour_names_the_field_it_was_in() {
    let error = load_theme_str("[colors]\nshadow = \"not-a-colour\"\n", Theme::default())
        .expect_err("that is not a colour");

    match error {
        ThemeError::BadColor { field, source } => {
            assert_eq!(field, "shadow");
            assert_eq!(
                source,
                ColorError::Malformed {
                    got: "not-a-colour".to_owned()
                }
            );
        }
        other => panic!("got {other}, which does not say which field"),
    }
}

/// A colour field holding the wrong kind of value says so.
#[test]
fn a_colour_that_is_not_a_string_names_the_field() {
    let error = load_theme_str("[colors]\nshadow = 42\n", Theme::default())
        .expect_err("a number is not a colour");

    match error {
        ThemeError::WrongType { field, expected } => {
            assert_eq!(field, "shadow");
            assert!(expected.contains("RRGGBB"), "expected {expected:?} to help");
        }
        other => panic!("got {other}"),
    }
}

/// A missing file is an IO error naming the path.
#[test]
fn a_missing_theme_file_says_which_file() {
    let error = load_theme_file("/nonexistent/theme.toml", Theme::default())
        .expect_err("there is no such file");

    match error {
        ThemeError::Io { path, source } => {
            assert_eq!(path, "/nonexistent/theme.toml");
            assert_eq!(source.kind(), std::io::ErrorKind::NotFound);
        }
        other => panic!("got {other}"),
    }
}

// --- built-in themes ----------------------------------------------------------

/// The default theme is named and complete.
#[test]
fn the_default_theme_is_named_and_populated() {
    let theme = glass_default();

    assert_eq!(theme.name, "glass-default");
    assert!(theme.corner_radius > 0);
    assert!(theme.title_bar.height > 0);
    assert!(!theme.title_bar.font.is_empty());
    assert!(
        theme.colors.panel_top.a > 0,
        "a panel nobody can see is not one"
    );
    assert!(theme.buttons.close.fill.a > 0);
}

/// The lite theme costs less than the default.
///
/// Shadow area, noise, and animation frames are the three costs that scale,
/// and "lite" means all three go down. A lite theme that only renamed itself
/// would be a promise the caller cannot collect on.
#[test]
fn the_lite_theme_is_cheaper_than_the_default() {
    let lite = glass_lite();
    let base = glass_default();

    assert_eq!(lite.name, "glass-lite");
    assert!(lite.shadow_extent < base.shadow_extent);
    assert!(lite.animation_duration_ms < base.animation_duration_ms);
    assert!(lite.noise_amplitude < base.noise_amplitude);
}

/// The minimal theme has no shadow and no animation.
///
/// The shadow *colour* is cleared as well as the extent. Either alone stops it
/// drawing, but leaving a visible colour behind a zero extent invites a caller
/// that raises the extent to inherit a shadow it never chose.
#[test]
fn the_minimal_theme_drops_the_shadow_and_the_animation() {
    let theme = glass_minimal();

    assert_eq!(theme.name, "glass-minimal");
    assert_eq!(theme.shadow_extent, 0);
    assert_eq!(theme.animation_duration_ms, 0);
    assert!((theme.noise_amplitude - 0.0).abs() < f64::EPSILON);
    assert_eq!(
        theme.colors.shadow.a, 0,
        "the colour goes with the extent, or raising one brings back the other"
    );
    assert!(
        theme.title_bar.height > 0,
        "minimal is not undecorated -- there is still a title bar"
    );
}

/// A nested colour reports its qualified name but is looked up by its own.
///
/// The two are different, and conflating them fails silently rather than
/// loudly: a button colour keyed `fill` looked up as `buttons.close.fill`
/// finds nothing, an absent field inherits by design, and the theme loads
/// looking correct with the caller's button colours quietly ignored. Caught
/// exactly that way when the round-trip case came back with a default red.
#[test]
fn a_nested_button_colour_is_read_and_reported_by_different_names() {
    let theme = load_theme_str("[buttons.close]\nfill = \"#010203FF\"\n", glass_default())
        .expect("valid TOML");

    assert_eq!(
        theme.buttons.close.fill,
        Color::new(1, 2, 3, 0xFF),
        "the key inside the table is `fill`; looking up the qualified name \
         finds nothing and silently inherits"
    );
    assert_eq!(
        theme.buttons.close.hover,
        glass_default().buttons.close.hover,
        "and its sibling still inherits, which is why the miss is silent"
    );

    // The label, though, is the qualified one -- an author reading
    // "fill: expected ..." would not know which of the six to look at.
    let error = load_theme_str("[buttons.close]\nfill = \"nope\"\n", glass_default())
        .expect_err("that is not a colour");
    match error {
        ThemeError::BadColor { field, .. } => assert_eq!(field, "buttons.close.fill"),
        other => panic!("got {other}"),
    }
}
