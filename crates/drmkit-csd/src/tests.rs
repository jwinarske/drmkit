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

// --- decoration geometry ------------------------------------------------------
//
// Parity port of the `CsdDecorationGeometry` cases in
// `tests/unit/test_csd_renderer.cpp`.

use crate::decoration_geometry;

/// The panel is inset by the shadow on every side.
///
/// The shadow is drawn *outside* the panel, so the decoration is larger than
/// the window it decorates by twice the extent. A panel that started at the
/// origin would put the shadow off the top-left of its own buffer.
#[test]
fn the_panel_is_inset_by_the_shadow_on_every_side() {
    let theme = glass_default();
    let geometry = decoration_geometry(&theme, 600, 360);

    assert_eq!(geometry.panel_x, theme.shadow_extent);
    assert_eq!(geometry.panel_y, theme.shadow_extent);
    assert_eq!(geometry.panel_w, 600 - 2 * theme.shadow_extent);
    assert_eq!(geometry.panel_h, 360 - 2 * theme.shadow_extent);
}

/// With no shadow, the panel is the whole canvas.
#[test]
fn a_theme_with_no_shadow_fills_the_canvas() {
    let geometry = decoration_geometry(&glass_minimal(), 600, 360);

    assert_eq!((geometry.panel_x, geometry.panel_y), (0, 0));
    assert_eq!((geometry.panel_w, geometry.panel_h), (600, 360));
}

/// A decoration too small for its own shadow gets a zero panel, not a
/// negative one.
///
/// A negative width reaching a rasterizer is either a crash or a very large
/// unsigned number, and asking for a 30-pixel decoration under a 24-pixel
/// shadow is an ordinary mistake rather than a reason to fail.
#[test]
fn a_decoration_too_small_for_its_shadow_clamps_to_a_zero_panel() {
    let geometry = decoration_geometry(&glass_default(), 30, 30);

    assert_eq!(geometry.panel_w, 0);
    assert_eq!(geometry.panel_h, 0);
    assert!(
        geometry.panel_w >= 0 && geometry.panel_h >= 0,
        "clamped, not wrapped"
    );
}

/// A negative shadow extent is treated as none.
///
/// A hand-written theme can carry one, and a panel inset the *other* way is
/// drawn outside its own decoration — reproducing that faithfully would put
/// pixels wherever the buffer's stride happened to lead.
#[test]
fn a_negative_shadow_extent_is_treated_as_none() {
    let theme = Theme {
        shadow_extent: -20,
        ..glass_default()
    };
    let geometry = decoration_geometry(&theme, 600, 360);

    assert_eq!((geometry.panel_x, geometry.panel_y), (0, 0));
    assert_eq!((geometry.panel_w, geometry.panel_h), (600, 360));
}

/// The buttons run right to left, evenly spaced.
///
/// Close is outermost: it is the one a user reaches for by feel, so it must
/// not move when the others are absent.
#[test]
fn the_buttons_run_right_to_left_evenly_spaced() {
    let geometry = decoration_geometry(&glass_default(), 600, 360);

    assert!(geometry.close_cx > geometry.minimize_cx);
    assert!(geometry.minimize_cx > geometry.maximize_cx);
    assert_eq!(
        geometry.close_cx - geometry.minimize_cx,
        geometry.minimize_cx - geometry.maximize_cx,
        "evenly spaced, or the row looks like a mistake"
    );
}

/// Every button lands inside the title bar.
///
/// The one property that makes the row usable rather than decorative: a
/// button whose circle crosses the panel edge or the title bar's bottom is
/// clipped, and a clipped button is one a user cannot reliably hit.
#[test]
fn every_button_lands_inside_the_title_bar() {
    let theme = glass_default();
    let geometry = decoration_geometry(&theme, 600, 360);

    for (name, cx) in [
        ("close", geometry.close_cx),
        ("minimize", geometry.minimize_cx),
        ("maximize", geometry.maximize_cx),
    ] {
        assert!(
            cx - geometry.button_radius >= geometry.panel_x,
            "{name} crosses the panel's left edge"
        );
        assert!(
            cx + geometry.button_radius < geometry.panel_x + geometry.panel_w,
            "{name} crosses the panel's right edge"
        );
    }

    assert!(
        geometry.button_cy - geometry.button_radius >= geometry.panel_y,
        "the row crosses the top of the panel"
    );
    assert!(
        geometry.button_cy + geometry.button_radius < geometry.panel_y + geometry.title_bar_height,
        "the row hangs below the title bar and into the window's content"
    );
}

/// A panel too narrow for the buttons still reports where they would be.
///
/// Geometry answers where things go; whether they fit is the caller's to
/// check, which is what the case above does. Refusing here would leave a
/// caller with nothing to check.
#[test]
fn a_panel_too_narrow_for_the_buttons_still_answers() {
    let geometry = decoration_geometry(&glass_default(), 60, 360);

    assert_eq!(geometry.panel_w, 60 - 2 * glass_default().shadow_extent);
    assert!(
        geometry.maximize_cx < geometry.panel_x,
        "the leftmost button does not fit, which is exactly what a caller \
         checking the bounds needs to be able to see"
    );
}

// --- shadow cache -------------------------------------------------------------
//
// Parity port of `tests/unit/test_csd_shadow_cache.cpp`.

use crate::{DEFAULT_CAPACITY, Elevation, ShadowCache, ShadowDest, ShadowKey, theme_id};

fn key(width: u32, height: u32, elevation: Elevation, theme: &Theme) -> ShadowKey {
    ShadowKey {
        width,
        height,
        elevation,
        theme_id: theme_id(theme),
    }
}

/// A destination big enough for a patch, zeroed.
fn dest(width: u32, height: u32) -> (Vec<u8>, u32) {
    (vec![0u8; (width * height * 4) as usize], width * 4)
}

/// The same theme hashes the same, every time.
#[test]
fn a_theme_id_is_stable_across_rebuilds() {
    assert_eq!(theme_id(&glass_default()), theme_id(&glass_default()));
    assert_eq!(theme_id(&glass_minimal()), theme_id(&glass_minimal()));
}

/// A colour change is a different theme.
#[test]
fn a_theme_id_changes_when_a_colour_does() {
    let base = glass_default();
    let changed = Theme {
        colors: crate::Colors {
            shadow: Color::new(1, 2, 3, 4),
            ..base.colors
        },
        ..base.clone()
    };

    assert_ne!(
        theme_id(&base),
        theme_id(&changed),
        "a cached shadow drawn in the old colour would outlive the change"
    );
}

/// A shadow-extent change is a different theme.
#[test]
fn a_theme_id_changes_when_the_shadow_extent_does() {
    let base = glass_default();
    let changed = Theme {
        shadow_extent: base.shadow_extent + 1,
        ..base.clone()
    };

    assert_ne!(theme_id(&base), theme_id(&changed));
}

/// The name does not change how anything looks, so it does not change the id.
///
/// Hashing it would evict every cached shadow when a caller renamed a theme —
/// a full re-blur per window, for a string.
#[test]
fn a_theme_id_ignores_the_name() {
    let base = glass_default();
    let renamed = Theme {
        name: "something-else".to_owned(),
        ..base.clone()
    };

    assert_eq!(theme_id(&base), theme_id(&renamed));
}

/// Nor does the animation duration.
#[test]
fn a_theme_id_ignores_the_animation_duration() {
    let base = glass_default();
    let quicker = Theme {
        animation_duration_ms: base.animation_duration_ms / 2,
        ..base.clone()
    };

    assert_eq!(
        theme_id(&base),
        theme_id(&quicker),
        "how long a shadow takes to fade cannot change what it looks like"
    );
}

/// Zero capacity means the default, not none.
///
/// A cache holding nothing would re-blur every shadow every frame, which is
/// the cost this type exists to remove.
#[test]
fn a_zero_capacity_cache_uses_the_default() {
    assert_eq!(ShadowCache::new(0).capacity(), DEFAULT_CAPACITY);
    assert_eq!(ShadowCache::default().capacity(), DEFAULT_CAPACITY);
}

/// An explicit capacity is respected.
#[test]
fn an_explicit_capacity_is_respected() {
    assert_eq!(ShadowCache::new(3).capacity(), 3);
}

/// Clearing empties the cache.
#[test]
fn clearing_empties_the_cache() {
    let theme = glass_default();
    let mut cache = ShadowCache::new(4);
    let (mut pixels, stride) = dest(64, 64);
    let mut target = ShadowDest {
        pixels: &mut pixels,
        stride,
        width: 64,
        height: 64,
    };

    assert!(cache.blit_into(key(64, 64, Elevation::Focused, &theme), &theme, &mut target));
    assert_eq!(cache.len(), 1);

    cache.clear();
    assert!(cache.is_empty());
    assert!(!cache.contains(&key(64, 64, Elevation::Focused, &theme)));
}

/// A zero-sized key writes nothing.
///
/// A window being resized passes through zero, and a decoration that errored
/// on the way would flicker.
#[test]
fn a_zero_sized_shadow_writes_nothing() {
    let theme = glass_default();
    let mut cache = ShadowCache::new(4);
    let (mut pixels, stride) = dest(64, 64);
    let mut target = ShadowDest {
        pixels: &mut pixels,
        stride,
        width: 64,
        height: 64,
    };

    assert!(!cache.blit_into(key(0, 64, Elevation::Focused, &theme), &theme, &mut target));
    assert!(!cache.blit_into(key(64, 0, Elevation::Focused, &theme), &theme, &mut target));
    assert!(cache.is_empty(), "and nothing was cached either");
}

/// A zero-sized destination writes nothing.
#[test]
fn a_zero_sized_destination_writes_nothing() {
    let theme = glass_default();
    let mut cache = ShadowCache::new(4);
    let (mut pixels, stride) = dest(64, 64);
    let mut target = ShadowDest {
        pixels: &mut pixels,
        stride,
        width: 0,
        height: 64,
    };

    assert!(!cache.blit_into(key(64, 64, Elevation::Focused, &theme), &theme, &mut target));
}

/// The first blit renders the shadow and writes visible alpha.
#[test]
fn the_first_blit_renders_a_shadow_with_visible_alpha() {
    let theme = glass_default();
    let mut cache = ShadowCache::new(4);
    let (mut pixels, stride) = dest(96, 96);
    let mut target = ShadowDest {
        pixels: &mut pixels,
        stride,
        width: 96,
        height: 96,
    };

    assert!(cache.blit_into(key(96, 96, Elevation::Focused, &theme), &theme, &mut target));
    assert_eq!(cache.len(), 1);

    let centre = ((48 * 96) + 48) * 4;
    assert!(
        pixels[centre + 3] > 0,
        "the middle of a shadow patch is where it is strongest; zero there \
         means nothing was drawn"
    );
}

/// The second blit of the same key is a hit.
#[test]
fn a_second_blit_of_the_same_shadow_is_a_hit() {
    let theme = glass_default();
    let mut cache = ShadowCache::new(4);
    let (mut pixels, stride) = dest(64, 64);
    let shadow = key(64, 64, Elevation::Focused, &theme);

    for _ in 0..3 {
        let mut target = ShadowDest {
            pixels: &mut pixels,
            stride,
            width: 64,
            height: 64,
        };
        assert!(cache.blit_into(shadow, &theme, &mut target));
    }
    assert_eq!(cache.len(), 1, "three blits of one shadow render it once");
}

/// Focused and blurred are different shadows.
///
/// They differ in strength, which is what makes the focused window read as
/// nearer. Sharing a cache entry would make every window look focused.
#[test]
fn focused_and_blurred_are_separate_entries() {
    let theme = glass_default();
    let mut cache = ShadowCache::new(4);
    let (mut focused_px, stride) = dest(64, 64);
    let (mut blurred_px, _) = dest(64, 64);

    let mut target = ShadowDest {
        pixels: &mut focused_px,
        stride,
        width: 64,
        height: 64,
    };
    cache.blit_into(key(64, 64, Elevation::Focused, &theme), &theme, &mut target);
    let mut target = ShadowDest {
        pixels: &mut blurred_px,
        stride,
        width: 64,
        height: 64,
    };
    cache.blit_into(key(64, 64, Elevation::Blurred, &theme), &theme, &mut target);

    assert_eq!(cache.len(), 2, "two elevations, two shadows");
    assert_ne!(
        focused_px, blurred_px,
        "and they must actually differ, or the elevation says nothing"
    );

    let centre = ((32 * 64) + 32) * 4 + 3;
    assert!(
        focused_px[centre] > blurred_px[centre],
        "the focused shadow is the stronger one"
    );
}

/// Past capacity, the least recently used goes.
#[test]
fn the_least_recently_used_shadow_is_evicted() {
    let theme = glass_default();
    let mut cache = ShadowCache::new(2);
    let (mut pixels, stride) = dest(64, 64);

    for size in [32u32, 48, 64] {
        let mut target = ShadowDest {
            pixels: &mut pixels,
            stride,
            width: 64,
            height: 64,
        };
        cache.blit_into(
            key(size, size, Elevation::Focused, &theme),
            &theme,
            &mut target,
        );
    }

    assert_eq!(cache.len(), 2, "the cap holds");
    assert!(
        !cache.contains(&key(32, 32, Elevation::Focused, &theme)),
        "the oldest went"
    );
    assert!(cache.contains(&key(64, 64, Elevation::Focused, &theme)));
}

/// Using a shadow again makes it recent.
///
/// Without this the LRU would evict whatever the caller draws every frame and
/// keep whatever it drew once — the opposite of a cache.
#[test]
fn using_a_shadow_again_keeps_it_from_eviction() {
    let theme = glass_default();
    let mut cache = ShadowCache::new(2);
    let (mut pixels, stride) = dest(64, 64);
    let blit = |cache: &mut ShadowCache, size: u32, pixels: &mut Vec<u8>| {
        let mut target = ShadowDest {
            pixels,
            stride,
            width: 64,
            height: 64,
        };
        cache.blit_into(
            key(size, size, Elevation::Focused, &theme),
            &theme,
            &mut target,
        );
    };

    blit(&mut cache, 32, &mut pixels);
    blit(&mut cache, 48, &mut pixels);
    // Touch the older one, then add a third.
    blit(&mut cache, 32, &mut pixels);
    blit(&mut cache, 64, &mut pixels);

    assert!(
        cache.contains(&key(32, 32, Elevation::Focused, &theme)),
        "32 was used most recently before the eviction, so it stays"
    );
    assert!(
        !cache.contains(&key(48, 48, Elevation::Focused, &theme)),
        "48 was the stalest"
    );
}

/// A theme with no shadow produces a transparent patch.
#[test]
fn a_theme_with_no_shadow_produces_nothing_visible() {
    let theme = glass_minimal();
    let mut cache = ShadowCache::new(2);
    let (mut pixels, stride) = dest(64, 64);
    let mut target = ShadowDest {
        pixels: &mut pixels,
        stride,
        width: 64,
        height: 64,
    };

    assert!(cache.blit_into(key(64, 64, Elevation::Focused, &theme), &theme, &mut target));
    assert!(
        pixels.iter().all(|byte| *byte == 0),
        "a zero-alpha shadow colour must write nothing visible, whatever the \
         extent says"
    );
}

/// A destination smaller than the patch is clipped, not overrun.
///
/// The alternative is writing past the caller's buffer, which is the one
/// outcome worse than a clipped shadow.
#[test]
fn a_destination_smaller_than_the_patch_is_clipped() {
    let theme = glass_default();
    let mut cache = ShadowCache::new(2);
    let (mut pixels, stride) = dest(32, 32);
    let mut target = ShadowDest {
        pixels: &mut pixels,
        stride,
        width: 32,
        height: 32,
    };

    assert!(cache.blit_into(key(96, 96, Elevation::Focused, &theme), &theme, &mut target));
    assert_eq!(pixels.len(), 32 * 32 * 4, "nothing grew the destination");
}

/// A cross fade blends the two endpoints, and clamps outside them.
///
/// `t` past the ends is a caller bug — an animation that overshoots —
/// and extrapolating a colour past its endpoints produces values that are not
/// a shadow at all.
#[test]
fn a_cross_fade_blends_between_its_endpoints() {
    let theme = glass_default();
    let mut cache = ShadowCache::new(4);
    let focused = key(64, 64, Elevation::Focused, &theme);
    let blurred = key(64, 64, Elevation::Blurred, &theme);
    let centre = ((32 * 64) + 32) * 4 + 3;

    let sample = |cache: &mut ShadowCache, t: f32| {
        let (mut pixels, stride) = dest(64, 64);
        let mut target = ShadowDest {
            pixels: &mut pixels,
            stride,
            width: 64,
            height: 64,
        };
        assert!(cache.blit_cross_fade(focused, blurred, &theme, &mut target, t));
        pixels[centre]
    };

    let at_start = sample(&mut cache, 0.0);
    let midway = sample(&mut cache, 0.5);
    let at_end = sample(&mut cache, 1.0);

    assert!(at_start > at_end, "focused is the stronger endpoint");
    assert!(
        midway < at_start && midway > at_end,
        "halfway is between them, not at one of them"
    );
    assert_eq!(
        sample(&mut cache, -5.0),
        at_start,
        "clamped, not extrapolated"
    );
    assert_eq!(sample(&mut cache, 5.0), at_end);
}
