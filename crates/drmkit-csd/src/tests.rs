// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! Parity port of `tests/unit/test_csd_theme.cpp` from drm-cxx @ `4a0b64a`.

use crate::{Color, ColorError, Theme, ThemeError, glass_default, glass_lite, glass_minimal};
use crate::{load_theme_file, load_theme_str};
use drmkit_fmt::fourcc::XRGB8888;

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

// --- animator -----------------------------------------------------------------
//
// Parity port of `tests/unit/test_csd_animator.cpp`.

use crate::{Dirty, HoverButton, PROGRESS_UNSET, WindowAnim, WindowState, ease_out_cubic};
use std::time::Duration;

const MS: fn(u64) -> Duration = Duration::from_millis;

/// The ease hits both endpoints exactly.
///
/// Not "close enough": a window that settled at 0.999 focused would never
/// quite look focused, and one that started at 0.001 would flicker.
#[test]
fn the_ease_hits_both_endpoints_exactly() {
    assert!((ease_out_cubic(0.0) - 0.0).abs() < f32::EPSILON);
    assert!((ease_out_cubic(1.0) - 1.0).abs() < f32::EPSILON);
}

/// Input outside `0..=1` is clamped, not extrapolated.
///
/// An animator that overshoots its duration would otherwise carry past the
/// target and the window would visibly bounce.
#[test]
fn the_ease_clamps_rather_than_extrapolating() {
    assert!((ease_out_cubic(-5.0) - 0.0).abs() < f32::EPSILON);
    assert!((ease_out_cubic(-0.001) - 0.0).abs() < f32::EPSILON);
    assert!((ease_out_cubic(5.0) - 1.0).abs() < f32::EPSILON);
}

/// It only ever increases.
#[test]
fn the_ease_never_goes_backwards() {
    let mut previous = ease_out_cubic(0.0);
    for step in 1..=100u8 {
        let current = ease_out_cubic(f32::from(step) / 100.0);
        assert!(
            current >= previous,
            "eased backwards at {step}: {current} < {previous}"
        );
        previous = current;
    }
}

/// It is fast at the start.
///
/// That is what "ease out" means, and it is the half a user notices: the
/// change is announced immediately and arrives gently.
#[test]
fn the_ease_is_fast_at_the_start() {
    assert!(
        ease_out_cubic(0.25) > 0.5,
        "a quarter of the way through time should be past halfway in value"
    );
    assert!(ease_out_cubic(0.5) > 0.8);
}

/// A new animator is settled at its focus, not animating toward it.
///
/// A window that appears focused should be drawn focused, not fade in from
/// unfocused on its first frame.
#[test]
fn a_new_animator_is_settled_not_animating() {
    let focused = WindowAnim::new(true);
    assert!((focused.focus_progress() - 1.0).abs() < f32::EPSILON);
    assert!(!focused.is_animating());

    let blurred = WindowAnim::new(false);
    assert!((blurred.focus_progress() - 0.0).abs() < f32::EPSILON);
    assert!(!blurred.is_animating());
}

/// Retargeting to the current target does nothing.
///
/// A caller that reports focus every frame would otherwise restart the
/// timeline every frame, and the window would never finish transitioning.
#[test]
fn retargeting_to_the_same_focus_does_nothing() {
    let mut anim = WindowAnim::new(false);
    anim.retarget_focus(false);
    assert!(!anim.is_animating());

    anim.retarget_focus(true);
    assert!(anim.is_animating());
    anim.tick(MS(50), MS(200));
    let midway = anim.focus_progress();

    anim.retarget_focus(true);
    assert!(
        (anim.focus_progress() - midway).abs() < f32::EPSILON,
        "a redundant retarget must not restart the run"
    );
}

/// Retargeting starts a run.
#[test]
fn retargeting_focus_starts_a_run() {
    let mut anim = WindowAnim::new(false);
    anim.retarget_focus(true);

    assert!(anim.is_animating());
    assert!(
        (anim.focus_progress() - 0.0).abs() < f32::EPSILON,
        "and does not jump -- the run has not been ticked yet"
    );
}

/// A full duration reaches the target.
#[test]
fn a_full_duration_reaches_the_target() {
    let mut anim = WindowAnim::new(false);
    anim.retarget_focus(true);

    let still_going = anim.tick(MS(200), MS(200));

    assert!((anim.focus_progress() - 1.0).abs() < f32::EPSILON);
    assert!(!still_going, "and reports that it has finished");
    assert!(!anim.is_animating());
}

/// A tick past the duration clamps to the target.
#[test]
fn a_tick_past_the_duration_clamps_to_the_target() {
    let mut anim = WindowAnim::new(false);
    anim.retarget_focus(true);

    anim.tick(MS(10_000), MS(200));

    assert!(
        (anim.focus_progress() - 1.0).abs() < f32::EPSILON,
        "a dropped frame must not carry the window past focused"
    );
}

/// Progress only increases across a run.
#[test]
fn progress_only_increases_across_a_run() {
    let mut anim = WindowAnim::new(false);
    anim.retarget_focus(true);

    let mut previous = anim.focus_progress();
    for _ in 0..20 {
        anim.tick(MS(10), MS(200));
        assert!(
            anim.focus_progress() >= previous,
            "went backwards mid-run: {} < {previous}",
            anim.focus_progress()
        );
        previous = anim.focus_progress();
    }
}

/// Retargeting mid-flight eases from where the value is.
///
/// Not from zero. A window that loses focus halfway through gaining it should
/// fade back from half-lit; snapping to unfocused first would flash.
#[test]
fn retargeting_mid_flight_eases_from_where_it_is() {
    let mut anim = WindowAnim::new(false);
    anim.retarget_focus(true);
    anim.tick(MS(100), MS(200));
    let midway = anim.focus_progress();
    assert!(midway > 0.0 && midway < 1.0, "genuinely mid-flight");

    anim.retarget_focus(false);
    assert!(
        (anim.focus_progress() - midway).abs() < f32::EPSILON,
        "the retarget itself must not move the value"
    );

    anim.tick(MS(10), MS(200));
    assert!(
        anim.focus_progress() < midway,
        "and it heads back down from there rather than snapping to zero"
    );
}

/// Nothing is hovered to begin with.
#[test]
fn nothing_is_hovered_to_begin_with() {
    let anim = WindowAnim::default();
    assert_eq!(anim.hover_painted(), HoverButton::None);
    assert!((anim.hover_progress() - 0.0).abs() < f32::EPSILON);
}

/// Entering a button starts it from zero and finishes at one.
#[test]
fn entering_a_button_runs_from_zero_to_one() {
    let mut anim = WindowAnim::default();
    anim.retarget_hover(HoverButton::Close);

    assert_eq!(anim.hover_painted(), HoverButton::Close);
    assert!((anim.hover_progress() - 0.0).abs() < f32::EPSILON);

    anim.tick(MS(200), MS(200));
    assert!((anim.hover_progress() - 1.0).abs() < f32::EPSILON);
    assert_eq!(anim.hover_painted(), HoverButton::Close);
}

/// Leaving fades the button that was left, then stops drawing it.
///
/// The painted button is not the target. Setting the target to `None` and
/// drawing that immediately would make the highlight vanish rather than fade.
#[test]
fn leaving_fades_the_button_that_was_left() {
    let mut anim = WindowAnim::default();
    anim.retarget_hover(HoverButton::Minimize);
    anim.tick(MS(200), MS(200));
    assert!((anim.hover_progress() - 1.0).abs() < f32::EPSILON);

    anim.retarget_hover(HoverButton::None);
    assert_eq!(
        anim.hover_painted(),
        HoverButton::Minimize,
        "still drawn, because it is still fading"
    );

    anim.tick(MS(100), MS(200));
    assert!(anim.hover_progress() < 1.0, "and fading");
    assert_eq!(anim.hover_painted(), HoverButton::Minimize);

    anim.tick(MS(200), MS(200));
    assert!((anim.hover_progress() - 0.0).abs() < f32::EPSILON);
    assert_eq!(
        anim.hover_painted(),
        HoverButton::None,
        "once faded out, it stops being drawn at all"
    );
}

/// Moving between buttons restarts on the new one.
///
/// They are different highlights in different places; carrying the old one's
/// progress across would make the new button appear already half-lit.
#[test]
fn moving_between_buttons_restarts_on_the_new_one() {
    let mut anim = WindowAnim::default();
    anim.retarget_hover(HoverButton::Close);
    anim.tick(MS(200), MS(200));
    assert!((anim.hover_progress() - 1.0).abs() < f32::EPSILON);

    anim.retarget_hover(HoverButton::Maximize);

    assert_eq!(anim.hover_painted(), HoverButton::Maximize);
    assert!(
        (anim.hover_progress() - 0.0).abs() < f32::EPSILON,
        "the new button lights from nothing, not from the old one's value"
    );
}

/// Snapping jumps to the targets and runs nothing.
#[test]
fn snapping_jumps_to_the_targets() {
    let mut anim = WindowAnim::new(false);
    anim.retarget_focus(true);
    anim.retarget_hover(HoverButton::Close);
    assert!(anim.is_animating());

    anim.snap();

    assert!((anim.focus_progress() - 1.0).abs() < f32::EPSILON);
    assert!((anim.hover_progress() - 1.0).abs() < f32::EPSILON);
    assert_eq!(anim.hover_painted(), HoverButton::Close);
    assert!(!anim.is_animating());
}

/// A zero duration snaps.
///
/// The documented way to turn animations off — `glass-minimal` sets it — not
/// an edge case being tolerated.
#[test]
fn a_zero_duration_snaps() {
    let mut anim = WindowAnim::new(false);
    anim.retarget_focus(true);

    let still_going = anim.tick(MS(16), Duration::ZERO);

    assert!((anim.focus_progress() - 1.0).abs() < f32::EPSILON);
    assert!(!still_going);
    assert!(!anim.is_animating());
}

/// Applying writes the animator's progress into the state.
///
/// Including the *painted* button rather than the target, so the renderer
/// draws the one that is fading rather than nothing.
#[test]
fn applying_mirrors_the_progress_into_the_state() {
    let mut anim = WindowAnim::new(false);
    anim.retarget_focus(true);
    anim.retarget_hover(HoverButton::Minimize);
    anim.tick(MS(100), MS(200));

    let mut state = WindowState::default();
    assert!((state.focus_progress - PROGRESS_UNSET).abs() < f32::EPSILON);

    anim.apply_to(&mut state);

    assert!((state.focus_progress - anim.focus_progress()).abs() < f32::EPSILON);
    assert!((state.hover_progress - anim.hover_progress()).abs() < f32::EPSILON);
    assert_eq!(state.hover, HoverButton::Minimize);
}

/// A fresh state is entirely dirty, and the bits are separable.
///
/// Nothing has been drawn yet, so every part of it is new. The separability
/// is what the mask is for: redrawing because the pointer moved costs a
/// rounded-rect fill, redrawing because the geometry changed costs a blur.
#[test]
fn a_fresh_state_is_dirty_and_the_bits_are_separable() {
    let state = WindowState::default();
    assert!(state.dirty.intersects(Dirty::GEOMETRY));
    assert!(state.dirty.intersects(Dirty::HOVER));

    assert!(Dirty::NONE.is_empty());
    assert!(
        !Dirty::HOVER.intersects(Dirty::GEOMETRY),
        "one bit is not another"
    );
    assert!(
        Dirty::HOVER.union(Dirty::FOCUS).intersects(Dirty::FOCUS),
        "and a union carries both"
    );
}

// --- overlay reservation ------------------------------------------------------
//
// Parity port of `tests/unit/test_csd_overlay_reservation.cpp`.

use crate::{OverlayReservation, ReserveError};
use drmkit_planes::{PlaneCapabilities, PlaneRegistry, PlaneType};

/// A plane on the CRTCs in `crtcs`, at `zpos`, taking `XRGB8888`.
fn overlay(id: u32, crtcs: u32, zpos: Option<u64>) -> PlaneCapabilities {
    PlaneCapabilities {
        id,
        possible_crtcs: crtcs,
        plane_type: PlaneType::Overlay,
        formats: vec![XRGB8888],
        zpos_min: zpos,
        zpos_max: zpos,
        ..PlaneCapabilities::default()
    }
}

/// The reference shape: two overlays per CRTC, on two CRTCs, none shared.
fn partitioned() -> PlaneRegistry {
    PlaneRegistry::from_capabilities(vec![
        overlay(10, 0b01, Some(1)),
        overlay(11, 0b01, Some(2)),
        overlay(20, 0b10, Some(1)),
        overlay(21, 0b10, Some(2)),
    ])
}

/// Each CRTC reserves from its own planes, without affecting the other.
#[test]
fn each_crtc_reserves_without_touching_the_other() {
    let registry = partitioned();
    let mut reservation = OverlayReservation::new();

    let first = reservation
        .reserve(&registry, 0, XRGB8888, 2, 0)
        .expect("two overlays on CRTC 0");
    let second = reservation
        .reserve(&registry, 1, XRGB8888, 2, 0)
        .expect("two on CRTC 1, which the first reservation cannot have taken");

    assert_eq!(first, vec![10, 11]);
    assert_eq!(second, vec![20, 21]);
    assert_eq!(reservation.all_reserved(), vec![10, 11, 20, 21]);
}

/// Asking for more than exist is a shortfall, and changes nothing.
///
/// A caller can ask again for fewer, or draw into the window instead. A
/// partial reservation would leave it working out which decoration to drop.
#[test]
fn asking_for_more_planes_than_exist_is_a_shortfall() {
    let registry = partitioned();
    let mut reservation = OverlayReservation::new();

    let error = reservation
        .reserve(&registry, 0, XRGB8888, 3, 0)
        .expect_err("CRTC 0 has two overlays");

    assert_eq!(
        error,
        ReserveError::Shortfall {
            crtc_index: 0,
            wanted: 3,
            found: 2
        }
    );
    assert!(
        reservation.all_reserved().is_empty(),
        "a shortfall must not leave planes claimed that nobody is using"
    );
}

/// A plane reachable from two CRTCs goes to whoever claims it first.
///
/// The second CRTC must not be offered it again — two CRTCs scanning out of
/// one plane is not something the hardware can do, and the refusal has to
/// happen here rather than at the commit.
#[test]
fn a_shared_plane_goes_to_whoever_claims_it_first() {
    // Both planes reachable from both CRTCs.
    let registry = PlaneRegistry::from_capabilities(vec![
        overlay(30, 0b11, Some(1)),
        overlay(31, 0b11, Some(2)),
    ]);
    let mut reservation = OverlayReservation::new();

    let first = reservation
        .reserve(&registry, 0, XRGB8888, 2, 0)
        .expect("CRTC 0 takes the pool");
    assert_eq!(first, vec![30, 31]);

    let error = reservation
        .reserve(&registry, 1, XRGB8888, 1, 0)
        .expect_err("there is nothing left");
    assert_eq!(
        error,
        ReserveError::Shortfall {
            crtc_index: 1,
            wanted: 1,
            found: 0
        }
    );
}

/// Releasing gives the planes back.
#[test]
fn releasing_frees_the_planes_for_another_crtc() {
    let registry = PlaneRegistry::from_capabilities(vec![overlay(30, 0b11, Some(1))]);
    let mut reservation = OverlayReservation::new();

    reservation
        .reserve(&registry, 0, XRGB8888, 1, 0)
        .expect("CRTC 0 takes it");
    assert!(reservation.reserve(&registry, 1, XRGB8888, 1, 0).is_err());

    reservation.release(0);

    assert_eq!(
        reservation.reserve(&registry, 1, XRGB8888, 1, 0),
        Ok(vec![30]),
        "released means available"
    );
    assert!(reservation.reserved_for(0).is_empty());
}

/// Releasing twice, or a CRTC that never reserved, is a no-op.
///
/// Both are what a caller tearing down an output does, sometimes twice.
#[test]
fn releasing_is_idempotent() {
    let registry = partitioned();
    let mut reservation = OverlayReservation::new();
    reservation
        .reserve(&registry, 0, XRGB8888, 1, 0)
        .expect("one overlay");

    reservation.release(0);
    reservation.release(0);
    reservation.release(99);

    assert!(reservation.all_reserved().is_empty());
}

/// Reserving again replaces what the CRTC held.
///
/// A caller responding to a mode change asks with a new count; the old claim
/// would otherwise keep planes it no longer wants out of everyone else's
/// reach.
#[test]
fn reserving_again_replaces_the_previous_claim() {
    let registry = partitioned();
    let mut reservation = OverlayReservation::new();

    reservation
        .reserve(&registry, 0, XRGB8888, 2, 0)
        .expect("both");
    let narrowed = reservation
        .reserve(&registry, 0, XRGB8888, 1, 0)
        .expect("now just one");

    assert_eq!(narrowed.len(), 1);
    assert_eq!(
        reservation.all_reserved(),
        narrowed,
        "the plane it gave up is available again, not still claimed"
    );
}

/// A zpos floor excludes planes that cannot sit above the window.
///
/// A decoration below the content it decorates is invisible, so a caller that
/// knows the window's stacking position says so here rather than discovering
/// it on screen.
#[test]
fn a_zpos_floor_excludes_planes_below_it() {
    let registry = PlaneRegistry::from_capabilities(vec![
        overlay(10, 0b01, Some(1)),
        overlay(11, 0b01, Some(5)),
    ]);
    let mut reservation = OverlayReservation::new();

    let claimed = reservation
        .reserve(&registry, 0, XRGB8888, 1, 4)
        .expect("one plane sits at or above 4");

    assert_eq!(claimed, vec![11], "not the one at zpos 1");
}

/// A plane that cannot scan the format out is not a candidate.
#[test]
fn a_plane_that_cannot_take_the_format_is_not_a_candidate() {
    let mut wrong_format = overlay(11, 0b01, Some(2));
    wrong_format.formats = vec![drmkit_fmt::fourcc::NV12];
    let registry = PlaneRegistry::from_capabilities(vec![overlay(10, 0b01, Some(1)), wrong_format]);
    let mut reservation = OverlayReservation::new();

    let error = reservation
        .reserve(&registry, 0, XRGB8888, 2, 0)
        .expect_err("only one plane takes XRGB8888");

    assert_eq!(
        error,
        ReserveError::Shortfall {
            crtc_index: 0,
            wanted: 2,
            found: 1
        }
    );
}

/// A plane with no zpos is skipped when a floor was asked for.
///
/// It cannot be *shown* to sit above anything. Admitting it would put a
/// decoration wherever the plane's fixed position happens to be, which on a
/// driver with no settable zpos is exactly the case the floor exists for.
#[test]
fn a_plane_without_a_zpos_is_skipped_when_a_floor_is_required() {
    let registry = PlaneRegistry::from_capabilities(vec![overlay(10, 0b01, None)]);
    let mut reservation = OverlayReservation::new();

    assert!(
        reservation.reserve(&registry, 0, XRGB8888, 1, 1).is_err(),
        "it cannot be shown to be above zpos 1"
    );
}

/// With no floor asked for, a plane without a zpos is fine.
///
/// The caller said it does not care where the decoration sits, and refusing
/// here would leave every driver with no settable zpos unable to reserve
/// anything at all.
#[test]
fn a_plane_without_a_zpos_is_admitted_when_no_floor_is_required() {
    let registry = PlaneRegistry::from_capabilities(vec![overlay(10, 0b01, None)]);
    let mut reservation = OverlayReservation::new();

    assert_eq!(
        reservation.reserve(&registry, 0, XRGB8888, 1, 0),
        Ok(vec![10])
    );
}

/// Planes come back in zpos order, and the order is stable.
///
/// A caller stacks decorations by index, so two planes at the same zpos
/// coming back in registry order would make the stack differ run to run.
#[test]
fn reserved_planes_come_back_in_a_stable_zpos_order() {
    // Deliberately out of order, and with a tie.
    let registry = PlaneRegistry::from_capabilities(vec![
        overlay(30, 0b01, Some(5)),
        overlay(10, 0b01, Some(1)),
        overlay(20, 0b01, Some(1)),
    ]);
    let mut reservation = OverlayReservation::new();

    let claimed = reservation
        .reserve(&registry, 0, XRGB8888, 3, 0)
        .expect("all three");

    assert_eq!(
        claimed,
        vec![10, 20, 30],
        "zpos first, then id -- the tie has to break the same way every time"
    );
    assert_eq!(reservation.reserved_for(0), claimed.as_slice());
}

/// Reserving nothing succeeds, and still records the CRTC.
///
/// A caller that asked for none has a reservation; it is just empty.
#[test]
fn reserving_nothing_succeeds() {
    let registry = partitioned();
    let mut reservation = OverlayReservation::new();

    assert_eq!(
        reservation.reserve(&registry, 0, XRGB8888, 0, 0),
        Ok(Vec::new())
    );
    assert!(reservation.reserved_for(0).is_empty());
    assert!(reservation.all_reserved().is_empty());
}

/// A CRTC that never reserved holds nothing.
#[test]
fn a_crtc_that_never_reserved_holds_nothing() {
    let reservation = OverlayReservation::new();
    assert!(reservation.reserved_for(0).is_empty());
    assert!(reservation.all_reserved().is_empty());
}

/// A primary plane is never a candidate.
///
/// It carries the window. Reserving it for a decoration would leave the
/// content with nowhere to go.
#[test]
fn a_primary_plane_is_never_reserved() {
    let mut primary = overlay(10, 0b01, Some(0));
    primary.plane_type = PlaneType::Primary;
    let registry = PlaneRegistry::from_capabilities(vec![primary, overlay(11, 0b01, Some(1))]);
    let mut reservation = OverlayReservation::new();

    let claimed = reservation
        .reserve(&registry, 0, XRGB8888, 1, 0)
        .expect("the overlay");

    assert_eq!(claimed, vec![11], "the primary is not on offer");
}

// --- renderer -----------------------------------------------------------------
//
// Parity port of the `CsdRenderer` and `CsdRendererDraw` cases in
// `tests/unit/test_csd_renderer.cpp`.

use crate::{Canvas, DrawError, Renderer, RendererConfig};

const W: u32 = 240;
const H: u32 = 160;

/// A canvas and its backing bytes.
fn canvas() -> Vec<u8> {
    vec![0u8; (W * H * 4) as usize]
}

/// Draw a decoration and hand back the pixels.
fn render(theme: &Theme, state: &WindowState) -> Vec<u8> {
    let mut pixels = canvas();
    let mut shadows = ShadowCache::new(4);
    let mut target = Canvas {
        pixels: &mut pixels,
        stride: W * 4,
        width: W,
        height: H,
    };
    Renderer::default()
        .draw(theme, state, &mut target, &mut shadows)
        .expect("a 240x160 canvas is drawable");
    pixels
}

/// The alpha byte at a pixel.
fn alpha_at(pixels: &[u8], x: u32, y: u32) -> u8 {
    pixels[((y * W + x) * 4 + 3) as usize]
}

/// A renderer builds with defaults, and reports no font.
///
/// Text is not implemented — a font stack is a separate dependency and a
/// separate decision — so `has_font` answers false rather than having a
/// caller reserve title-bar width for something that never appears.
#[test]
fn a_renderer_builds_and_reports_no_font() {
    let renderer = Renderer::default();
    assert!(!renderer.has_font());
    assert_eq!(renderer.font_path(), None);

    let configured = Renderer::new(RendererConfig {
        font_path: Some("/usr/share/fonts/x.ttf".to_owned()),
    });
    assert_eq!(configured.font_path(), Some("/usr/share/fonts/x.ttf"));
    assert!(
        !configured.has_font(),
        "a path is not a loaded font, and saying otherwise would be a lie a \
         caller lays out against"
    );
}

/// An empty canvas is refused.
///
/// A window mid-resize passes through zero. Reporting success on a loop that
/// wrote nothing would have the caller present an unpainted buffer believing
/// it had been drawn.
#[test]
fn an_empty_canvas_is_refused() {
    let mut shadows = ShadowCache::new(2);
    let mut nothing: Vec<u8> = Vec::new();
    let mut target = Canvas {
        pixels: &mut nothing,
        stride: 0,
        width: 0,
        height: 0,
    };

    assert_eq!(
        Renderer::default().draw(
            &glass_default(),
            &WindowState::default(),
            &mut target,
            &mut shadows
        ),
        Err(DrawError::EmptyCanvas)
    );
}

/// A canvas shorter than its own stride and height is refused.
#[test]
fn a_canvas_shorter_than_it_claims_is_refused() {
    let mut shadows = ShadowCache::new(2);
    let mut pixels = vec![0u8; 16];
    let mut target = Canvas {
        pixels: &mut pixels,
        stride: W * 4,
        width: W,
        height: H,
    };

    let error = Renderer::default()
        .draw(
            &glass_default(),
            &WindowState::default(),
            &mut target,
            &mut shadows,
        )
        .expect_err("sixteen bytes is not a 240x160 canvas");

    assert!(
        matches!(error, DrawError::Undersized { got: 16, .. }),
        "got {error}, which does not say what was wrong with the canvas"
    );
}

/// The middle of the panel is painted.
#[test]
fn the_middle_of_the_panel_is_painted() {
    let state = WindowState {
        title: "test".to_owned(),
        focused: true,
        ..WindowState::default()
    };
    let pixels = render(&glass_default(), &state);

    assert!(
        alpha_at(&pixels, W / 2, H / 2) > 0,
        "the centre of a decoration is inside its panel; zero alpha there \
         means nothing was drawn at all"
    );
}

/// The extreme corner stays clear.
///
/// The shadow fades toward the edges and the panel is inset from them, so the
/// very corner of the canvas is outside both. Something opaque there means
/// the panel is being drawn at the wrong origin.
#[test]
fn the_far_corner_stays_transparent() {
    let state = WindowState {
        focused: true,
        ..WindowState::default()
    };
    let pixels = render(&glass_default(), &state);

    assert_eq!(
        alpha_at(&pixels, 0, 0),
        0,
        "the top-left of the canvas is beyond the shadow's reach"
    );
}

/// The same inputs draw the same bytes.
///
/// What lets a caller skip a redraw it knows would change nothing, and why
/// the panel's dither is a hash of the coordinate rather than a random
/// number.
#[test]
fn drawing_is_deterministic() {
    let state = WindowState {
        focused: true,
        ..WindowState::default()
    };

    let first = render(&glass_default(), &state);
    let second = render(&glass_default(), &state);

    assert_eq!(
        first, second,
        "two draws of one decoration must be byte-identical, or every \
         redraw-skipping optimisation above this is unsound"
    );
}

/// Focused and blurred decorations differ.
///
/// The rim colour is the cue that tells a user where their keystrokes are
/// going. Two windows that looked identical would make it useless.
#[test]
fn a_focused_decoration_differs_from_a_blurred_one() {
    let theme = glass_default();
    let focused = render(
        &theme,
        &WindowState {
            focused: true,
            ..WindowState::default()
        },
    );
    let blurred = render(
        &theme,
        &WindowState {
            focused: false,
            ..WindowState::default()
        },
    );

    assert_ne!(focused, blurred, "the two must not look the same");

    // Specifically at the rim, and against a theme with **no shadow**.
    //
    // Two confounds had to go for this to mean anything. Comparing whole
    // buffers passes on the shadow alone, since focused and blurred use
    // different elevations. And sampling the rim under a shadowed theme still
    // passes, because the rim is composited *over* a shadow that itself
    // differs -- so an identical rim colour still produces different bytes.
    // Both verified by injecting a rim that ignores focus, which passed until
    // the shadow was taken out from under it.
    let unshadowed = glass_minimal();
    let focused = render(
        &unshadowed,
        &WindowState {
            focused: true,
            ..WindowState::default()
        },
    );
    let blurred = render(
        &unshadowed,
        &WindowState {
            focused: false,
            ..WindowState::default()
        },
    );
    let differs_at_rim = (0..W).any(|x| {
        let offset = (x * 4) as usize;
        focused[offset..offset + 4] != blurred[offset..offset + 4]
    });
    assert!(
        differs_at_rim,
        "the rim is the cue that says where keystrokes are going; it has to \
         be the thing that changes, not the shadow behind it"
    );
}

/// Hovering a button changes pixels at that button, and not at the others.
#[test]
fn hovering_a_button_changes_that_button_and_no_other() {
    let theme = glass_default();
    let geometry = decoration_geometry(&theme, W, H);
    let plain = render(&theme, &WindowState::default());
    let hovered = render(
        &theme,
        &WindowState {
            hover: HoverButton::Close,
            hover_progress: 1.0,
            ..WindowState::default()
        },
    );

    let at = |pixels: &[u8], cx: i32| {
        let x = u32::try_from(cx).expect("on canvas");
        let y = u32::try_from(geometry.button_cy).expect("on canvas");
        let offset = ((y * W + x) * 4) as usize;
        pixels[offset..offset + 4].to_vec()
    };

    assert_ne!(
        at(&plain, geometry.close_cx),
        at(&hovered, geometry.close_cx),
        "the hovered button has to light, or hover means nothing"
    );
    assert_eq!(
        at(&plain, geometry.minimize_cx),
        at(&hovered, geometry.minimize_cx),
        "and only that one -- lighting every button on any hover would be \
         worse than lighting none"
    );
}

/// Drawing populates the shadow cache.
///
/// The blur is the expensive part, and it happening through the cache is what
/// makes the second frame cheap.
#[test]
fn drawing_populates_the_shadow_cache() {
    let mut pixels = canvas();
    let mut shadows = ShadowCache::new(4);
    assert!(shadows.is_empty());

    let mut target = Canvas {
        pixels: &mut pixels,
        stride: W * 4,
        width: W,
        height: H,
    };
    Renderer::default()
        .draw(
            &glass_default(),
            &WindowState {
                focused: true,
                ..WindowState::default()
            },
            &mut target,
            &mut shadows,
        )
        .expect("drawable");

    assert_eq!(shadows.len(), 1, "the shadow went through the cache");
}

/// A theme with no shadow draws no shadow and still draws a panel.
#[test]
fn a_theme_with_no_shadow_still_draws_its_panel() {
    let pixels = render(
        &glass_minimal(),
        &WindowState {
            focused: true,
            ..WindowState::default()
        },
    );

    assert!(alpha_at(&pixels, W / 2, H / 2) > 0, "the panel is there");
    assert!(
        alpha_at(&pixels, 0, H / 2) > 0,
        "and with no shadow inset it reaches the canvas edge -- sampled at \
         mid-height, because the panel is still rounded and the literal \
         corner is outside it"
    );
    assert_eq!(
        alpha_at(&pixels, 0, 0),
        0,
        "the corner is clear even with no shadow, because the panel is rounded"
    );
}

/// A decoration too small for its own shadow draws nothing rather than
/// panicking.
#[test]
fn a_decoration_too_small_for_its_shadow_draws_nothing() {
    let theme = glass_default();
    let mut pixels = vec![0u8; (16 * 16 * 4) as usize];
    let mut shadows = ShadowCache::new(2);
    let mut target = Canvas {
        pixels: &mut pixels,
        stride: 16 * 4,
        width: 16,
        height: 16,
    };

    Renderer::default()
        .draw(&theme, &WindowState::default(), &mut target, &mut shadows)
        .expect("a zero panel is not an error");
}
