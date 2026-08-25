// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! Theme discovery and cursor-name resolution, against a fixture tree.
//!
//! Every test builds its own theme directories under a scratch root so none of
//! this depends on what the machine happens to have installed -- which also
//! means the tests say the same thing on a bare CI container as on a desktop.

use std::path::{Path, PathBuf};

use crate::{CursorError, Theme, ThemeResolution};

/// A scratch directory tree, removed when the test ends.
struct Fixture {
    root: PathBuf,
}

impl Fixture {
    fn new(name: &str) -> Self {
        // Per-process and per-name, so two tests running in parallel cannot
        // collide on one directory.
        let mut root = std::env::temp_dir();
        root.push(format!("drmkit-cursor-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("scratch root");
        Self { root }
    }

    /// `<root>/<name>/cursors/`, plus an `index.theme` when `inherits` is
    /// non-empty.
    fn theme(&self, name: &str, inherits: &[&str]) -> PathBuf {
        let dir = self.root.join(name);
        std::fs::create_dir_all(dir.join("cursors")).expect("theme dir");
        if !inherits.is_empty() {
            let body = format!(
                "[Icon Theme]\nName={name}\nInherits={}\n",
                inherits.join(",")
            );
            std::fs::write(dir.join("index.theme"), body).expect("index.theme");
        }
        dir
    }

    /// A theme directory with a literal `index.theme`, for the parser tests.
    fn theme_with_index(&self, name: &str, index: &str) -> PathBuf {
        let dir = self.root.join(name);
        std::fs::create_dir_all(dir.join("cursors")).expect("theme dir");
        std::fs::write(dir.join("index.theme"), index).expect("index.theme");
        dir
    }

    /// An empty file standing in for a cursor. Resolution only stats it.
    fn cursor(dir: &Path, name: &str) {
        std::fs::write(dir.join("cursors").join(name), b"").expect("cursor file");
    }

    fn discover(&self) -> Result<Theme, CursorError> {
        Theme::discover_with_paths(std::slice::from_ref(&self.root))
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[test]
fn discovery_finds_a_theme() {
    let fixture = Fixture::new("discover");
    fixture.theme("Adwaita", &[]);
    let theme = fixture.discover().expect("discover");
    assert_eq!(theme.theme_names(), vec!["Adwaita"]);
}

#[test]
fn discovery_fails_when_nothing_is_installed() {
    // Distinct from a resolve miss: nothing is installed at all, and a caller
    // wants to say something different about that.
    let fixture = Fixture::new("empty");
    assert_eq!(fixture.discover().unwrap_err(), CursorError::NoThemes);
}

#[test]
fn a_directory_with_only_metadata_is_still_indexed() {
    // Inheritance passes through themes that carry no cursors of their own, so
    // one has to be indexed to be walked through.
    let fixture = Fixture::new("metadata-only");
    let dir = fixture.root.join("Bridge");
    std::fs::create_dir_all(&dir).expect("dir");
    std::fs::write(dir.join("index.theme"), "[Icon Theme]\nInherits=Base\n").expect("index");
    let theme = fixture.discover().expect("discover");
    assert_eq!(theme.theme_names(), vec!["Bridge"]);
}

#[test]
fn a_directory_that_says_nothing_is_not_a_theme() {
    let fixture = Fixture::new("not-a-theme");
    std::fs::create_dir_all(fixture.root.join("random")).expect("dir");
    fixture.theme("Real", &[]);
    let theme = fixture.discover().expect("discover");
    assert_eq!(theme.theme_names(), vec!["Real"]);
}

#[test]
fn resolve_finds_a_cursor_in_the_requested_theme() {
    let fixture = Fixture::new("direct");
    let dir = fixture.theme("Adwaita", &[]);
    Fixture::cursor(&dir, "left_ptr");

    let theme = fixture.discover().expect("discover");
    let found = theme.resolve("left_ptr", "Adwaita").expect("resolve");
    assert_eq!(found.theme_name, "Adwaita");
    assert_eq!(found.source, dir.join("cursors").join("left_ptr"));
}

#[test]
fn resolve_reports_a_miss() {
    let fixture = Fixture::new("miss");
    fixture.theme("Adwaita", &[]);
    let theme = fixture.discover().expect("discover");
    assert_eq!(
        theme.resolve("nonexistent-shape", "Adwaita").unwrap_err(),
        CursorError::NotFound
    );
}

#[test]
fn resolve_walks_the_inherits_chain() {
    let fixture = Fixture::new("inherits");
    fixture.theme("Child", &["Parent"]);
    let parent = fixture.theme("Parent", &[]);
    Fixture::cursor(&parent, "left_ptr");

    let theme = fixture.discover().expect("discover");
    let found = theme.resolve("left_ptr", "Child").expect("resolve");
    assert_eq!(found.theme_name, "Parent");
    assert!(found.chain.contains(&"Child".to_owned()));
    assert!(found.chain.contains(&"Parent".to_owned()));
}

#[test]
fn resolve_walks_the_chain_transitively() {
    let fixture = Fixture::new("transitive");
    fixture.theme("Child", &["Middle"]);
    fixture.theme("Middle", &["Grandparent"]);
    let grandparent = fixture.theme("Grandparent", &[]);
    Fixture::cursor(&grandparent, "left_ptr");

    let theme = fixture.discover().expect("discover");
    let found = theme.resolve("left_ptr", "Child").expect("resolve");
    assert_eq!(found.theme_name, "Grandparent");
    assert_eq!(
        found.chain,
        vec!["Child", "Middle", "Grandparent"],
        "the chain reports the walk in order"
    );
}

#[test]
fn an_alias_finds_the_legacy_name() {
    // The caller asks for the modern name; the theme ships the old one.
    let fixture = Fixture::new("alias-legacy");
    let dir = fixture.theme("Adwaita", &[]);
    Fixture::cursor(&dir, "sb_h_double_arrow");

    let theme = fixture.discover().expect("discover");
    let found = theme.resolve("ew-resize", "Adwaita").expect("resolve");
    assert_eq!(found.source.file_name().unwrap(), "sb_h_double_arrow");
}

#[test]
fn an_alias_finds_the_modern_name() {
    // And the other way round.
    let fixture = Fixture::new("alias-modern");
    let dir = fixture.theme("Adwaita", &[]);
    Fixture::cursor(&dir, "pointer");

    let theme = fixture.discover().expect("discover");
    let found = theme.resolve("hand2", "Adwaita").expect("resolve");
    assert_eq!(found.source.file_name().unwrap(), "pointer");
}

#[test]
fn alias_order_is_the_tables_not_the_requests() {
    // A theme shipping two members of one class answers with whichever comes
    // first in the table, even when the caller named the other. Worth pinning
    // because it looks like a bug until you know it is the reference's
    // behaviour, and changing it silently changes which file a theme resolves
    // to.
    let fixture = Fixture::new("alias-order");
    let dir = fixture.theme("Adwaita", &[]);
    Fixture::cursor(&dir, "pointer");
    Fixture::cursor(&dir, "hand2");

    let theme = fixture.discover().expect("discover");
    let found = theme.resolve("hand2", "Adwaita").expect("resolve");
    assert_eq!(
        found.source.file_name().unwrap(),
        "pointer",
        "the class is ordered, and `pointer` comes first in it"
    );
}

#[test]
fn a_name_in_no_class_resolves_as_itself() {
    let fixture = Fixture::new("no-class");
    let dir = fixture.theme("Adwaita", &[]);
    Fixture::cursor(&dir, "my-custom-shape");

    let theme = fixture.discover().expect("discover");
    let found = theme
        .resolve("my-custom-shape", "Adwaita")
        .expect("resolve");
    assert_eq!(found.source.file_name().unwrap(), "my-custom-shape");
}

#[test]
fn resolve_falls_back_through_indexed_themes() {
    // Neither the named chain nor `default` had it, so every other theme gets
    // tried in discovery order.
    let fixture = Fixture::new("fallback");
    fixture.theme("Named", &[]);
    let other = fixture.theme("Other", &[]);
    Fixture::cursor(&other, "left_ptr");

    let theme = fixture.discover().expect("discover");
    let found = theme.resolve("left_ptr", "Named").expect("resolve");
    assert_eq!(found.theme_name, "Other");
}

#[test]
fn a_path_traversal_name_is_refused() {
    // A cursor name is an identifier. Joined onto a directory, `..` composes
    // into somewhere else entirely, and a compositor passing a name straight
    // through from a client should not be able to reach it.
    let fixture = Fixture::new("traversal");
    fixture.theme("Adwaita", &[]);
    let theme = fixture.discover().expect("discover");

    for name in [
        "..",
        ".",
        "",
        "../../etc/passwd",
        "sub/dir",
        "back\\slash",
        "../Adwaita/cursors/left_ptr",
    ] {
        assert_eq!(
            theme.resolve(name, "Adwaita").unwrap_err(),
            CursorError::InvalidName,
            "{name:?} must be refused"
        );
    }
}

#[test]
fn a_refused_name_never_reaches_the_cache() {
    // The guard runs before the cache is touched, so a caller spamming bogus
    // names cannot grow the map. Checked by asking twice and confirming the
    // answer is the guard's, not a memoized anything.
    let fixture = Fixture::new("traversal-cache");
    fixture.theme("Adwaita", &[]);
    let theme = fixture.discover().expect("discover");
    for _ in 0..8 {
        assert_eq!(
            theme.resolve("../escape", "Adwaita").unwrap_err(),
            CursorError::InvalidName
        );
    }
    assert_eq!(theme.cache_len(), 0, "refused names must not be memoized");

    // A legitimate name does get memoized, so the assertion above is about
    // the guard rather than about the cache being broken.
    let _ = theme.resolve("left_ptr", "Adwaita");
    assert_eq!(theme.cache_len(), 1);
}

#[test]
fn the_inherits_parser_takes_a_semicolon() {
    // The spec says comma; themes in the wild use both.
    let fixture = Fixture::new("semicolon");
    fixture.theme_with_index("Child", "[Icon Theme]\nName=Child\nInherits=Parent;Other\n");
    let parent = fixture.theme("Parent", &[]);
    Fixture::cursor(&parent, "left_ptr");

    let theme = fixture.discover().expect("discover");
    let found = theme.resolve("left_ptr", "Child").expect("resolve");
    assert_eq!(found.theme_name, "Parent");
}

#[test]
fn the_inherits_parser_ignores_comments_and_other_sections() {
    let fixture = Fixture::new("sections");
    fixture.theme_with_index(
        "Child",
        "# a comment\n\
         [Icon Theme]\n\
         # Inherits=NotThisOne\n\
         Name=Child\n\
         Inherits=Parent\n\
         \n\
         [X-Something]\n\
         Inherits=NorThisOne\n",
    );
    let parent = fixture.theme("Parent", &[]);
    Fixture::cursor(&parent, "left_ptr");
    let decoy = fixture.theme("NotThisOne", &[]);
    Fixture::cursor(&decoy, "left_ptr");

    let theme = fixture.discover().expect("discover");
    let found = theme.resolve("left_ptr", "Child").expect("resolve");
    assert_eq!(found.theme_name, "Parent");
}

#[test]
fn an_inherits_before_the_icon_theme_section_is_ignored() {
    // A key outside `[Icon Theme]` is not ours, including one that appears
    // before any section header at all.
    let fixture = Fixture::new("preamble");
    fixture.theme_with_index("Child", "Inherits=Wrong\n[Icon Theme]\nInherits=Parent\n");
    let parent = fixture.theme("Parent", &[]);
    Fixture::cursor(&parent, "left_ptr");
    let wrong = fixture.theme("Wrong", &[]);
    Fixture::cursor(&wrong, "left_ptr");

    let theme = fixture.discover().expect("discover");
    assert_eq!(
        theme
            .resolve("left_ptr", "Child")
            .expect("resolve")
            .theme_name,
        "Parent"
    );
}

#[test]
fn a_key_that_merely_starts_with_inherits_is_not_it() {
    // `InheritsFrom=` is not `Inherits=`, and a prefix match would take it.
    let fixture = Fixture::new("prefix");
    fixture.theme_with_index("Child", "[Icon Theme]\nInheritsFrom=Wrong\n");
    let wrong = fixture.theme("Wrong", &[]);
    Fixture::cursor(&wrong, "left_ptr");
    let other = fixture.theme("Other", &[]);
    Fixture::cursor(&other, "left_ptr");

    let theme = fixture.discover().expect("discover");
    let found = theme.resolve("left_ptr", "Child").expect("resolve");
    assert!(
        !found.chain.contains(&"Wrong".to_owned())
            || found.chain.iter().position(|t| t == "Wrong")
                > found.chain.iter().position(|t| t == "Child"),
        "Wrong must not have been reached as Child's parent: {:?}",
        found.chain
    );
}

#[test]
fn resolution_is_memoized() {
    // The point of the cache: a caller cycling through a fixed shape set pays
    // the walk once per name. Checked by deleting the file underneath and
    // confirming the answer still comes back -- which only a cache can do.
    let fixture = Fixture::new("memo");
    let dir = fixture.theme("Adwaita", &[]);
    Fixture::cursor(&dir, "left_ptr");

    let theme = fixture.discover().expect("discover");
    let first = theme.resolve("left_ptr", "Adwaita").expect("resolve");
    std::fs::remove_file(dir.join("cursors").join("left_ptr")).expect("remove");
    let second = theme.resolve("left_ptr", "Adwaita").expect("still cached");

    assert_eq!(first, second);
}

#[test]
fn a_miss_is_memoized_too() {
    // Negative results are cached as well, or a caller asking repeatedly for a
    // shape no theme ships pays the whole walk every time.
    let fixture = Fixture::new("memo-miss");
    let dir = fixture.theme("Adwaita", &[]);

    let theme = fixture.discover().expect("discover");
    assert_eq!(
        theme.resolve("left_ptr", "Adwaita").unwrap_err(),
        CursorError::NotFound
    );
    // Now provide it. The cached miss stands, because the index is a snapshot.
    Fixture::cursor(&dir, "left_ptr");
    assert_eq!(
        theme.resolve("left_ptr", "Adwaita").unwrap_err(),
        CursorError::NotFound,
        "the negative result was memoized"
    );
    // A fresh index sees it.
    let reindexed = fixture.discover().expect("discover");
    assert!(reindexed.resolve("left_ptr", "Adwaita").is_ok());
}

#[test]
fn the_chain_is_deduplicated() {
    // A diamond in the inherits graph must not report a theme twice, and must
    // not walk it twice either.
    let fixture = Fixture::new("diamond");
    fixture.theme("Child", &["Left", "Right"]);
    fixture.theme("Left", &["Base"]);
    fixture.theme("Right", &["Base"]);
    let base = fixture.theme("Base", &[]);
    Fixture::cursor(&base, "left_ptr");

    let theme = fixture.discover().expect("discover");
    let found = theme.resolve("left_ptr", "Child").expect("resolve");
    assert_eq!(found.theme_name, "Base");

    let mut seen = found.chain.clone();
    seen.sort();
    let deduped = {
        let mut copy = seen.clone();
        copy.dedup();
        copy
    };
    assert_eq!(seen, deduped, "chain has a repeat: {:?}", found.chain);
}

#[test]
fn a_missing_parent_is_recorded_and_survived() {
    // An `Inherits=` naming a theme that is not installed is normal. It has to
    // appear in the chain -- "looked, not there" is what diagnostics are for
    // -- and must not stop the walk.
    let fixture = Fixture::new("ghost-parent");
    fixture.theme("Child", &["NotInstalled", "Parent"]);
    let parent = fixture.theme("Parent", &[]);
    Fixture::cursor(&parent, "left_ptr");

    let theme = fixture.discover().expect("discover");
    let found = theme.resolve("left_ptr", "Child").expect("resolve");
    assert_eq!(found.theme_name, "Parent");
    assert!(
        found.chain.contains(&"NotInstalled".to_owned()),
        "a parent that is missing is still part of the walk: {:?}",
        found.chain
    );
}

#[test]
fn an_inherits_cycle_terminates() {
    // Two themes naming each other is a malformed pair that exists in the
    // wild. The visited set is what stops it, and without a test the failure
    // mode is a hang rather than an error.
    let fixture = Fixture::new("cycle");
    fixture.theme("A", &["B"]);
    fixture.theme("B", &["A"]);

    let theme = fixture.discover().expect("discover");
    assert_eq!(
        theme.resolve("left_ptr", "A").unwrap_err(),
        CursorError::NotFound
    );
}

#[test]
fn the_first_search_path_wins() {
    // Two roots providing the same theme name: the earlier one supplies the
    // cursor, which is what lets a user override a system theme.
    let outer = Fixture::new("priority");
    let user = outer.root.join("user");
    let system = outer.root.join("system");
    for root in [&user, &system] {
        std::fs::create_dir_all(root.join("Adwaita").join("cursors")).expect("dirs");
    }
    std::fs::write(user.join("Adwaita/cursors/left_ptr"), b"user").expect("user cursor");
    std::fs::write(system.join("Adwaita/cursors/left_ptr"), b"system").expect("system cursor");

    let theme = Theme::discover_with_paths(&[user.clone(), system]).expect("discover");
    let found = theme.resolve("left_ptr", "Adwaita").expect("resolve");
    assert!(
        found.source.starts_with(&user),
        "the earlier search path must win: {:?}",
        found.source
    );
}

#[test]
fn the_default_search_path_is_ordered_user_first() {
    // Not a filesystem test -- just that the order is the one the spec asks
    // for, since getting it backwards would let a system theme shadow a
    // user's and nothing else would notice.
    let paths = crate::default_search_paths();
    assert!(!paths.is_empty());
    let strings: Vec<String> = paths.iter().map(|p| p.display().to_string()).collect();
    if let (Some(user), Some(system)) = (
        strings.iter().position(|p| p.contains("/.icons")),
        strings
            .iter()
            .position(|p| p.starts_with("/usr/share/icons")),
    ) {
        assert!(user < system, "user paths come first: {strings:?}");
    }
    assert_eq!(
        strings.last().map(String::as_str),
        Some("/usr/share/pixmaps"),
        "the pixmaps leftover is last: {strings:?}"
    );
}

#[test]
fn a_resolution_reports_where_it_looked() {
    let fixture = Fixture::new("chain-report");
    fixture.theme("Child", &["Parent"]);
    let parent = fixture.theme("Parent", &[]);
    Fixture::cursor(&parent, "left_ptr");

    let theme = fixture.discover().expect("discover");
    let ThemeResolution {
        theme_name,
        source,
        chain,
    } = theme.resolve("left_ptr", "Child").expect("resolve");

    assert_eq!(theme_name, "Parent");
    assert!(source.is_absolute() || source.exists());
    assert_eq!(chain.first().map(String::as_str), Some("Child"));
}

// --- cursor files ----------------------------------------------------------

use crate::Cursor;
use std::time::Duration;

/// Build a cursor file in memory.
///
/// Everything a test needs to be wrong about is a parameter, because the whole
/// point of the parser is what it does with numbers that disagree with the
/// bytes present.
#[derive(Debug, Clone)]
struct FileBuilder {
    images: Vec<ImageSpec>,
    /// Override the declared table-of-contents count.
    declared_entries: Option<u32>,
    /// Truncate the finished file to this many bytes.
    truncate_to: Option<usize>,
}

#[derive(Debug, Clone)]
struct ImageSpec {
    nominal_size: u32,
    width: u32,
    height: u32,
    xhot: u32,
    yhot: u32,
    delay_ms: u32,
    fill: u32,
    /// Write fewer pixels than the header declares.
    short_by: usize,
}

impl ImageSpec {
    fn new(nominal_size: u32, width: u32, height: u32) -> Self {
        Self {
            nominal_size,
            width,
            height,
            xhot: 0,
            yhot: 0,
            delay_ms: 0,
            fill: 0xffff_0000,
            short_by: 0,
        }
    }
    fn delay(mut self, ms: u32) -> Self {
        self.delay_ms = ms;
        self
    }
    fn fill(mut self, pixel: u32) -> Self {
        self.fill = pixel;
        self
    }
    fn hot(mut self, x: u32, y: u32) -> Self {
        self.xhot = x;
        self.yhot = y;
        self
    }
}

impl FileBuilder {
    fn new() -> Self {
        Self {
            images: Vec::new(),
            declared_entries: None,
            truncate_to: None,
        }
    }
    fn image(mut self, spec: ImageSpec) -> Self {
        self.images.push(spec);
        self
    }
    fn build(&self) -> Vec<u8> {
        const HEADER_LEN: u32 = 16;
        let toc_len = self.images.len() * 12;
        let body_start = HEADER_LEN as usize + toc_len;

        let mut chunks: Vec<Vec<u8>> = Vec::new();
        let mut offsets: Vec<u32> = Vec::new();
        let mut at = body_start;
        for spec in &self.images {
            offsets.push(u32::try_from(at).expect("small file"));
            let mut chunk = Vec::new();
            for value in [
                0x24,
                0xfffd_0002,
                spec.nominal_size,
                1,
                spec.width,
                spec.height,
                spec.xhot,
                spec.yhot,
                spec.delay_ms,
            ] {
                chunk.extend_from_slice(&u32::to_le_bytes(value));
            }
            let declared = spec.width as usize * spec.height as usize;
            for _ in 0..declared.saturating_sub(spec.short_by) {
                chunk.extend_from_slice(&spec.fill.to_le_bytes());
            }
            at += chunk.len();
            chunks.push(chunk);
        }

        let mut out = Vec::new();
        out.extend_from_slice(b"Xcur");
        out.extend_from_slice(&HEADER_LEN.to_le_bytes());
        out.extend_from_slice(&1u32.to_le_bytes());
        let entries = self
            .declared_entries
            .unwrap_or_else(|| u32::try_from(self.images.len()).expect("small"));
        out.extend_from_slice(&entries.to_le_bytes());
        for (spec, offset) in self.images.iter().zip(&offsets) {
            out.extend_from_slice(&0xfffd_0002u32.to_le_bytes());
            out.extend_from_slice(&spec.nominal_size.to_le_bytes());
            out.extend_from_slice(&offset.to_le_bytes());
        }
        for chunk in &chunks {
            out.extend_from_slice(chunk);
        }
        if let Some(limit) = self.truncate_to {
            out.truncate(limit);
        }
        out
    }
}

#[test]
fn a_static_cursor_carries_its_metadata() {
    let bytes = FileBuilder::new()
        .image(ImageSpec::new(24, 4, 3).hot(1, 2).fill(0xff00_ff00))
        .build();
    let cursor = Cursor::from_bytes(&bytes, 24).expect("load");

    assert_eq!(cursor.frames().len(), 1);
    let frame = cursor.first();
    assert_eq!((frame.width, frame.height), (4, 3));
    assert_eq!((frame.xhot, frame.yhot), (1, 2));
    assert_eq!(frame.pixels.len(), 12);
    assert!(frame.pixels.iter().all(|p| *p == 0xff00_ff00));
    assert!(!cursor.is_animated());
    assert_eq!(cursor.cycle(), Duration::ZERO);
}

#[test]
fn a_static_cursor_shows_the_same_frame_forever() {
    let bytes = FileBuilder::new().image(ImageSpec::new(24, 2, 2)).build();
    let cursor = Cursor::from_bytes(&bytes, 24).expect("load");
    for ms in [0u64, 1, 1_000, 86_400_000] {
        assert_eq!(cursor.frame_at(Duration::from_millis(ms)), cursor.first());
    }
}

#[test]
fn an_animated_cursor_reports_its_cycle() {
    let bytes = FileBuilder::new()
        .image(ImageSpec::new(24, 2, 2).delay(100))
        .image(ImageSpec::new(24, 2, 2).delay(150))
        .image(ImageSpec::new(24, 2, 2).delay(50))
        .build();
    let cursor = Cursor::from_bytes(&bytes, 24).expect("load");

    assert_eq!(cursor.frames().len(), 3);
    assert!(cursor.is_animated());
    assert_eq!(cursor.cycle(), Duration::from_millis(300));
}

#[test]
fn frame_at_walks_the_delays() {
    let bytes = FileBuilder::new()
        .image(ImageSpec::new(24, 2, 2).delay(100).fill(1))
        .image(ImageSpec::new(24, 2, 2).delay(150).fill(2))
        .image(ImageSpec::new(24, 2, 2).delay(50).fill(3))
        .build();
    let cursor = Cursor::from_bytes(&bytes, 24).expect("load");

    let shown = |ms: u64| cursor.frame_at(Duration::from_millis(ms)).pixels[0];
    // Frame boundaries are half-open: a frame shows up to, not including, its
    // own end. Getting that wrong shows the previous frame for one tick.
    assert_eq!(shown(0), 1);
    assert_eq!(shown(99), 1);
    assert_eq!(shown(100), 2, "the first frame's 100ms is up");
    assert_eq!(shown(249), 2);
    assert_eq!(shown(250), 3);
    assert_eq!(shown(299), 3);
}

#[test]
fn frame_at_wraps_at_the_cycle() {
    let bytes = FileBuilder::new()
        .image(ImageSpec::new(24, 2, 2).delay(100).fill(1))
        .image(ImageSpec::new(24, 2, 2).delay(200).fill(2))
        .build();
    let cursor = Cursor::from_bytes(&bytes, 24).expect("load");

    let shown = |ms: u64| cursor.frame_at(Duration::from_millis(ms)).pixels[0];
    assert_eq!(
        shown(300),
        shown(0),
        "one full cycle later is the same frame"
    );
    assert_eq!(shown(350), shown(50));
    assert_eq!(shown(3_000_000_050), shown(50), "and much later still");
}

#[test]
fn frames_that_all_declare_no_delay_are_static() {
    // Some themes ship multi-frame packs with delay=0 throughout. Playing them
    // would loop instantly and flicker.
    let bytes = FileBuilder::new()
        .image(ImageSpec::new(24, 2, 2).fill(1))
        .image(ImageSpec::new(24, 2, 2).fill(2))
        .build();
    let cursor = Cursor::from_bytes(&bytes, 24).expect("load");

    assert_eq!(cursor.frames().len(), 2);
    assert!(!cursor.is_animated());
    assert_eq!(cursor.frame_at(Duration::from_millis(500)).pixels[0], 1);
}

#[test]
fn the_nearest_size_is_chosen() {
    // Cursor files are multi-resolution packs.
    let bytes = FileBuilder::new()
        .image(ImageSpec::new(16, 2, 2).fill(16))
        .image(ImageSpec::new(32, 2, 2).fill(32))
        .image(ImageSpec::new(64, 2, 2).fill(64))
        .build();

    for (requested, want) in [
        (16, 16),
        (20, 16),
        (30, 32),
        (32, 32),
        (48, 32),
        (64, 64),
        (99, 64),
    ] {
        let cursor = Cursor::from_bytes(&bytes, requested).expect("load");
        assert_eq!(
            cursor.first().pixels[0],
            want,
            "asking for {requested} should pick {want}"
        );
    }
}

#[test]
fn a_tie_on_size_goes_to_the_smaller() {
    // 24 is equidistant from 16 and 32. The smaller one sits inside the
    // plane; the larger has to be cropped or scaled.
    let bytes = FileBuilder::new()
        .image(ImageSpec::new(16, 2, 2).fill(16))
        .image(ImageSpec::new(32, 2, 2).fill(32))
        .build();
    let cursor = Cursor::from_bytes(&bytes, 24).expect("load");
    assert_eq!(cursor.first().pixels[0], 16);
}

#[test]
fn every_frame_of_the_chosen_size_is_kept() {
    // An animated pack holds several sizes, each with its own frames. Picking
    // a size must take all of that size's frames and none of the others'.
    let bytes = FileBuilder::new()
        .image(ImageSpec::new(16, 2, 2).delay(10).fill(16))
        .image(ImageSpec::new(32, 2, 2).delay(20).fill(32))
        .image(ImageSpec::new(16, 2, 2).delay(30).fill(16))
        .image(ImageSpec::new(32, 2, 2).delay(40).fill(32))
        .build();

    let small = Cursor::from_bytes(&bytes, 16).expect("load");
    assert_eq!(small.frames().len(), 2);
    assert!(small.frames().iter().all(|f| f.pixels[0] == 16));
    assert_eq!(small.cycle(), Duration::from_millis(40));

    let large = Cursor::from_bytes(&bytes, 32).expect("load");
    assert_eq!(large.frames().len(), 2);
    assert_eq!(large.cycle(), Duration::from_millis(60));
}

#[test]
fn a_size_of_zero_means_the_default() {
    let bytes = FileBuilder::new()
        .image(ImageSpec::new(24, 2, 2).fill(24))
        .image(ImageSpec::new(64, 2, 2).fill(64))
        .build();
    // 0 must not mean "the smallest" or "whatever is first" -- it means 64,
    // the smallest size a real cursor plane commonly is.
    let cursor = Cursor::from_bytes(&bytes, 0).expect("load");
    assert_eq!(cursor.first().pixels[0], 64);
}

#[test]
fn pixels_are_read_as_little_endian_argb() {
    // The format stores each pixel as a little-endian 32-bit ARGB value, so a
    // byte-order slip shows up as swapped channels rather than as a failure.
    let bytes = FileBuilder::new()
        .image(ImageSpec::new(24, 1, 1).fill(0x8844_2211))
        .build();
    let cursor = Cursor::from_bytes(&bytes, 24).expect("load");
    assert_eq!(cursor.first().pixels[0], 0x8844_2211);
}

// --- what a crafted file must not do ---------------------------------------

#[test]
fn a_declared_image_larger_than_the_file_is_refused_not_allocated() {
    // The whole reason this parser exists. `xcursor` 0.3.11 sizes its buffer
    // from width * height * 4 before reading, so a 60-byte file can reserve
    // 4.3 GB. Whether that is fatal depends on the platform -- see the
    // `xcursor` module for the measured cases -- but it is never a good way
    // to reject a 60-byte file. Here the length is checked against the bytes
    // actually present.
    let mut spec = ImageSpec::new(24, 512, 512);
    spec.short_by = 512 * 512; // declare a full image, write no pixels
    let bytes = FileBuilder::new().image(spec).build();
    assert!(bytes.len() < 100, "the file is tiny: {} bytes", bytes.len());

    assert_eq!(
        Cursor::from_bytes(&bytes, 24).unwrap_err(),
        CursorError::Malformed
    );
}

#[test]
fn an_oversize_image_is_refused_before_its_pixel_count_is_computed() {
    let bytes = FileBuilder::new()
        .image({
            let mut spec = ImageSpec::new(24, 513, 1);
            spec.short_by = 513;
            spec
        })
        .build();
    assert_eq!(
        Cursor::from_bytes(&bytes, 24).unwrap_err(),
        CursorError::TooLarge
    );
}

#[test]
fn too_many_frames_is_refused() {
    let mut builder = FileBuilder::new();
    for _ in 0..257 {
        builder = builder.image(ImageSpec::new(24, 1, 1));
    }
    assert_eq!(
        Cursor::from_bytes(&builder.build(), 24).unwrap_err(),
        CursorError::TooLarge
    );
}

#[test]
fn a_toc_count_larger_than_the_file_is_refused() {
    // The other unbounded number in the header.
    let mut builder = FileBuilder::new().image(ImageSpec::new(24, 1, 1));
    builder.declared_entries = Some(u32::MAX);
    assert_eq!(
        Cursor::from_bytes(&builder.build(), 24).unwrap_err(),
        CursorError::Malformed
    );
}

#[test]
fn a_truncated_file_is_refused_at_every_length() {
    // Every prefix of a valid file must be an error and not a panic. This is
    // the cheap version of the fuzz target, and it runs on every build.
    let whole = FileBuilder::new()
        .image(ImageSpec::new(24, 4, 4).delay(50))
        .image(ImageSpec::new(24, 4, 4).delay(50))
        .build();
    for length in 0..whole.len() {
        let result = Cursor::from_bytes(&whole[..length], 24);
        assert!(
            result.is_err(),
            "a {length}-byte prefix of a {}-byte file must not load",
            whole.len()
        );
    }
    assert!(
        Cursor::from_bytes(&whole, 24).is_ok(),
        "the whole file loads"
    );
}

#[test]
fn garbage_is_refused() {
    for bytes in [
        b"".to_vec(),
        b"Xcur".to_vec(),
        b"not a cursor file at all".to_vec(),
        vec![0xff; 64],
    ] {
        assert!(
            Cursor::from_bytes(&bytes, 24).is_err(),
            "{bytes:?} must not load"
        );
    }
}

#[test]
fn a_hotspot_outside_the_image_is_refused() {
    let bytes = FileBuilder::new()
        .image(ImageSpec::new(24, 4, 4).hot(5, 0))
        .build();
    assert_eq!(
        Cursor::from_bytes(&bytes, 24).unwrap_err(),
        CursorError::Malformed
    );
}

#[test]
fn a_zero_dimension_is_refused() {
    for (w, h) in [(0, 4), (4, 0)] {
        let bytes = FileBuilder::new().image(ImageSpec::new(24, w, h)).build();
        assert_eq!(
            Cursor::from_bytes(&bytes, 24).unwrap_err(),
            CursorError::Malformed,
            "{w}x{h}"
        );
    }
}

#[test]
fn a_file_with_no_images_is_refused() {
    let bytes = FileBuilder::new().build();
    assert_eq!(
        Cursor::from_bytes(&bytes, 24).unwrap_err(),
        CursorError::NoImages
    );
}

// --- caller-supplied pixels ------------------------------------------------

#[test]
fn from_argb_builds_a_static_cursor() {
    let pixels = vec![0xff00_00ff; 16];
    let cursor = Cursor::from_argb(&pixels, 4, 4, 1, 2).expect("build");
    assert_eq!(cursor.frames().len(), 1);
    assert_eq!((cursor.first().width, cursor.first().height), (4, 4));
    assert_eq!((cursor.first().xhot, cursor.first().yhot), (1, 2));
    assert_eq!(cursor.first().pixels, pixels);
    assert!(!cursor.is_animated());
}

#[test]
fn from_argb_refuses_dimensions_that_do_not_describe_an_image() {
    let pixels = vec![0u32; 16];
    for (w, h) in [(0, 4), (4, 0), (513, 1), (1, 513)] {
        assert_eq!(
            Cursor::from_argb(&pixels, w, h, 0, 0).unwrap_err(),
            CursorError::InvalidImage,
            "{w}x{h}"
        );
    }
    // Right dimensions, wrong number of pixels.
    assert_eq!(
        Cursor::from_argb(&pixels, 4, 5, 0, 0).unwrap_err(),
        CursorError::InvalidImage
    );
}

#[test]
fn loading_through_a_theme_finds_the_file() {
    // The path a caller actually takes: resolve a name, load what it points
    // at. Ties the two halves of the crate together.
    let fixture = Fixture::new("load-named");
    let dir = fixture.theme("Adwaita", &[]);
    let bytes = FileBuilder::new()
        .image(ImageSpec::new(24, 2, 2).fill(0xff12_3456))
        .build();
    std::fs::write(dir.join("cursors").join("left_ptr"), &bytes).expect("write cursor");

    let theme = fixture.discover().expect("discover");
    let cursor = Cursor::load_named(&theme, "left_ptr", "Adwaita", 24).expect("load");
    assert_eq!(cursor.first().pixels[0], 0xff12_3456);
}

#[test]
fn loading_a_missing_file_reports_io() {
    let fixture = Fixture::new("load-missing");
    let dir = fixture.theme("Adwaita", &[]);
    let theme = fixture.discover().expect("discover");
    // Resolve against a file that exists, then delete it.
    std::fs::write(dir.join("cursors").join("left_ptr"), b"").expect("write");
    let resolved = theme.resolve("left_ptr", "Adwaita").expect("resolve");
    std::fs::remove_file(&resolved.source).expect("remove");

    assert!(matches!(
        Cursor::load(&resolved, 24),
        Err(CursorError::Io(_))
    ));
}

#[test]
fn every_fuzz_seed_loads() {
    // A seed that does not parse teaches the fuzzer nothing -- it starts from
    // a dead input and has to rediscover the format before it can explore it.
    // That was worth a whole fuzzing session's coverage on the EDID target
    // before it was noticed, so it gets a test here.
    // This is a statement about the repository's contents, so it only means
    // anything where the repository is. The HIL lane copies test binaries to a
    // board and nothing else, and asserting there that a source path exists
    // would fail for a reason that has nothing to do with the code.
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../../fuzz/seeds/cursor_file");
    if !std::path::Path::new(dir).is_dir() {
        println!("note: skipped -- not running from the source tree, so there is no seed corpus");
        return;
    }
    let mut checked = 0;
    let mut animated = 0;
    let mut sizes = std::collections::BTreeSet::new();
    for entry in std::fs::read_dir(dir).expect("the seed corpus must exist") {
        let path = entry.expect("entry").path();
        let bytes = std::fs::read(&path).expect("read seed");
        let cursor = Cursor::from_bytes(&bytes, 0).unwrap_or_else(|error| {
            panic!("seed {} does not load: {error}", path.display());
        });
        if cursor.is_animated() {
            animated += 1;
        }
        sizes.insert(cursor.first().width);
        checked += 1;
    }
    assert!(
        checked >= 4,
        "expected at least four seeds, found {checked}"
    );
    assert!(
        animated >= 1,
        "no seed animates, so the frame-walking path is unseeded"
    );
    assert!(
        sizes.len() >= 2,
        "every seed is the same size, so size selection is unseeded: {sizes:?}"
    );
}

// --- composing a sprite into a plane buffer --------------------------------

use crate::Frame;
use crate::sprite::{Placement, Rotation, compose};

/// A frame whose pixels encode their own coordinates, so a rotation that
/// transposes or mirrors is visible rather than plausible.
fn coded_frame(width: u32, height: u32, xhot: u32, yhot: u32) -> Frame {
    let pixels = (0..width * height)
        .map(|i| {
            let (x, y) = (i % width, i / width);
            0xff00_0000 | (x << 8) | y
        })
        .collect();
    Frame {
        pixels,
        width,
        height,
        xhot,
        yhot,
        delay: Duration::ZERO,
    }
}

/// A solid frame of one colour.
fn solid_frame(width: u32, height: u32, pixel: u32) -> Frame {
    Frame {
        pixels: vec![pixel; (width * height) as usize],
        width,
        height,
        xhot: 0,
        yhot: 0,
        delay: Duration::ZERO,
    }
}

/// Compose into a fresh buffer, returning it alongside the placement.
fn composed(frame: &Frame, rotation: Rotation, w: u32, h: u32) -> (Vec<u32>, Placement) {
    let mut buffer = vec![0xdead_beef; (w * h) as usize];
    let placement = compose(frame, rotation, &mut buffer, w, h, w as usize);
    (buffer, placement)
}

fn at(buffer: &[u32], stride: u32, x: u32, y: u32) -> u32 {
    buffer[(y * stride + x) as usize]
}

#[test]
fn an_unrotated_sprite_is_copied_and_centred() {
    let frame = coded_frame(2, 2, 0, 0);
    let (buffer, placement) = composed(&frame, Rotation::None, 4, 4);

    assert_eq!((placement.width, placement.height), (2, 2));
    assert_eq!(
        (placement.x, placement.y),
        (1, 1),
        "centred in a 4x4 buffer"
    );
    assert_eq!(at(&buffer, 4, 1, 1), 0xff00_0000, "source (0,0)");
    assert_eq!(at(&buffer, 4, 2, 1), 0xff00_0100, "source (1,0)");
    assert_eq!(at(&buffer, 4, 1, 2), 0xff00_0001, "source (0,1)");
    assert_eq!(at(&buffer, 4, 2, 2), 0xff00_0101, "source (1,1)");
}

#[test]
fn the_buffer_is_wiped_before_the_sprite_lands() {
    // A smaller frame must not leave the previous one showing around its
    // edges, and a rotation can move the sprite off its own former footprint
    // entirely. Either ghosts on screen.
    let frame = solid_frame(2, 2, 0xffff_ffff);
    let (buffer, _) = composed(&frame, Rotation::None, 4, 4);

    let corners = [(0, 0), (3, 0), (0, 3), (3, 3)];
    for (x, y) in corners {
        assert_eq!(
            at(&buffer, 4, x, y),
            0,
            "({x}, {y}) still holds what was there before"
        );
    }
}

#[test]
fn each_rotation_moves_pixels_where_it_should() {
    // A 3x2 frame, so a quarter turn is visible in the dimensions as well as
    // in the pixels -- a square frame would hide a transposition.
    let frame = coded_frame(3, 2, 0, 0);
    let code = |x: u32, y: u32| 0xff00_0000 | (x << 8) | y;

    // Buffer exactly the rotated size, so nothing is centred or scaled and
    // the mapping is the only thing under test.
    let (upright, p0) = composed(&frame, Rotation::None, 3, 2);
    assert_eq!((p0.width, p0.height), (3, 2));
    assert_eq!(at(&upright, 3, 0, 0), code(0, 0));
    assert_eq!(at(&upright, 3, 2, 1), code(2, 1));

    let (quarter, p90) = composed(&frame, Rotation::Quarter, 2, 3);
    assert_eq!((p90.width, p90.height), (2, 3), "a quarter turn swaps axes");
    // Clockwise: the source's bottom-left corner becomes the top-left.
    assert_eq!(at(&quarter, 2, 0, 0), code(0, 1));
    assert_eq!(at(&quarter, 2, 1, 0), code(0, 0));
    assert_eq!(at(&quarter, 2, 0, 2), code(2, 1));

    let (half, p180) = composed(&frame, Rotation::Half, 3, 2);
    assert_eq!((p180.width, p180.height), (3, 2));
    assert_eq!(
        at(&half, 3, 0, 0),
        code(2, 1),
        "half turn is a double mirror"
    );
    assert_eq!(at(&half, 3, 2, 1), code(0, 0));

    let (three, p270) = composed(&frame, Rotation::ThreeQuarter, 2, 3);
    assert_eq!((p270.width, p270.height), (2, 3));
    assert_eq!(at(&three, 2, 0, 0), code(2, 0));
    assert_eq!(at(&three, 2, 1, 2), code(0, 1));
}

#[test]
fn four_quarter_turns_are_the_identity() {
    // The cheapest check that the four formulas are consistent with each
    // other rather than four independently plausible guesses.
    let frame = coded_frame(4, 4, 0, 0);
    let mut current = frame.clone();
    for _ in 0..4 {
        let (buffer, placement) = composed(&current, Rotation::Quarter, 4, 4);
        current = Frame {
            pixels: buffer,
            width: placement.width,
            height: placement.height,
            xhot: placement.hotspot_x,
            yhot: placement.hotspot_y,
            delay: Duration::ZERO,
        };
    }
    assert_eq!(
        current.pixels, frame.pixels,
        "four quarter turns is upright"
    );
}

#[test]
fn the_hotspot_turns_with_the_pixels() {
    // The hotspot is where the pointer actually points. If it does not follow
    // the rotation, every click on a rotated display lands somewhere else --
    // and the sprite looks perfectly correct while it happens.
    let frame = coded_frame(4, 2, 3, 0); // tip at the top-right corner

    let (_, none) = composed(&frame, Rotation::None, 4, 2);
    assert_eq!((none.hotspot_x, none.hotspot_y), (3, 0));

    // Turned a quarter clockwise, a top-right tip is at the bottom-right...
    let (_, quarter) = composed(&frame, Rotation::Quarter, 2, 4);
    assert_eq!((quarter.hotspot_x, quarter.hotspot_y), (1, 3));

    // ...half turn puts it at the bottom-left...
    let (_, half) = composed(&frame, Rotation::Half, 4, 2);
    assert_eq!((half.hotspot_x, half.hotspot_y), (0, 1));

    // ...and three quarters at the top-left.
    let (_, three) = composed(&frame, Rotation::ThreeQuarter, 2, 4);
    assert_eq!((three.hotspot_x, three.hotspot_y), (0, 0));
}

#[test]
fn the_hotspot_is_offset_by_the_centring() {
    let frame = coded_frame(2, 2, 1, 1);
    let (_, placement) = composed(&frame, Rotation::None, 8, 8);
    assert_eq!((placement.x, placement.y), (3, 3));
    assert_eq!(
        (placement.hotspot_x, placement.hotspot_y),
        (4, 4),
        "the hotspot is in buffer space, not frame space"
    );
}

#[test]
fn an_oversize_sprite_is_shrunk_to_fit() {
    // The common case, not a rare one: a theme ships the nearest size it has,
    // which is often above what was asked for. Adwaita ships 24/30/36/48/72/96,
    // so a 64-pixel plane gets a 72-pixel sprite.
    let frame = solid_frame(72, 72, 0xff40_8060);
    let (buffer, placement) = composed(&frame, Rotation::None, 64, 64);

    assert_eq!(
        (placement.width, placement.height),
        (64, 64),
        "72 -> 64 must land on 64, not 63: truncating leaves a black border"
    );
    assert_eq!((placement.x, placement.y), (0, 0));
    // A solid source downscales to the same solid colour.
    assert!(
        buffer.iter().all(|p| *p == 0xff40_8060),
        "a flat colour must survive the box filter unchanged"
    );
}

#[test]
fn shrinking_keeps_the_aspect_ratio() {
    // Only one axis overflows, but both shrink by the same ratio -- otherwise
    // the sprite stretches.
    let frame = solid_frame(128, 32, 0xffff_ffff);
    let (_, placement) = composed(&frame, Rotation::None, 64, 64);
    assert_eq!((placement.width, placement.height), (64, 16));
    assert_eq!((placement.x, placement.y), (0, 24), "and stays centred");
}

#[test]
fn a_shrunk_hotspot_stays_inside_the_sprite() {
    // Scaling the hotspot truncates where the pixels round, which keeps it
    // inside rather than one pixel past the edge.
    let frame = coded_frame(72, 72, 71, 71);
    let (_, placement) = composed(&frame, Rotation::None, 64, 64);
    assert!(
        placement.hotspot_x < placement.x + placement.width
            && placement.hotspot_y < placement.y + placement.height,
        "hotspot ({}, {}) is outside a {}x{} sprite at ({}, {})",
        placement.hotspot_x,
        placement.hotspot_y,
        placement.width,
        placement.height,
        placement.x,
        placement.y
    );
}

#[test]
fn rotation_and_shrink_compose() {
    // A quarter turn swaps the axes, and the swapped dimensions are what the
    // shrink is computed against. Getting the order wrong gives a sprite that
    // fits one axis and overflows the other.
    let frame = solid_frame(128, 32, 0xffff_ffff);
    let (_, placement) = composed(&frame, Rotation::Quarter, 64, 64);
    assert_eq!(
        (placement.width, placement.height),
        (16, 64),
        "rotated to 32x128, then shrunk to fit 64x64"
    );
}

#[test]
fn the_box_filter_conserves_energy() {
    // Plan §9 T1: the downscale is area-weighted, so the average of the output
    // has to match the average of the input to within rounding. A sampler that
    // dropped or double-counted source pixels would still look plausible on a
    // flat colour and fail here.
    let mut frame = solid_frame(96, 96, 0);
    for (index, pixel) in frame.pixels.iter_mut().enumerate() {
        // A gradient, so every source pixel differs from its neighbours.
        let value = u32::try_from(index % 256).expect("small");
        *pixel = 0xff00_0000 | (value << 16) | (value << 8) | value;
    }

    let source_mean: f64 = frame
        .pixels
        .iter()
        .map(|p| f64::from(p & 0xff))
        .sum::<f64>()
        / f64::from(frame.width * frame.height);

    for size in [64u32, 48, 32, 24, 17] {
        let (buffer, placement) = composed(&frame, Rotation::None, size, size);
        let sprite: Vec<u32> = (0..placement.height)
            .flat_map(|y| (0..placement.width).map(move |x| (x, y)))
            .map(|(x, y)| at(&buffer, size, placement.x + x, placement.y + y))
            .collect();
        let out_mean: f64 = sprite.iter().map(|p| f64::from(p & 0xff)).sum::<f64>()
            / f64::from(u32::try_from(sprite.len()).expect("small sprite"));
        assert!(
            (out_mean - source_mean).abs() < 2.0,
            "{size}: mean moved from {source_mean:.2} to {out_mean:.2}"
        );
    }
}

#[test]
fn the_box_filter_never_leaves_the_sprite() {
    // Every write has to land inside the placed rectangle. A sampler running
    // one pixel past its bounds would scribble on the wiped border, which is
    // invisible against a black cursor and obvious against a light one.
    let frame = solid_frame(100, 60, 0xffff_ffff);
    let (buffer, placement) = composed(&frame, Rotation::None, 64, 64);

    for y in 0..64 {
        for x in 0..64 {
            let inside = x >= placement.x
                && x < placement.x + placement.width
                && y >= placement.y
                && y < placement.y + placement.height;
            if !inside {
                assert_eq!(at(&buffer, 64, x, y), 0, "({x}, {y}) is outside the sprite");
            }
        }
    }
}

#[test]
fn a_sprite_the_size_of_the_buffer_is_not_scaled() {
    // Equal is not larger. Taking the shrink path here would run a whole box
    // filter to reproduce the input.
    let frame = coded_frame(64, 64, 0, 0);
    let (buffer, placement) = composed(&frame, Rotation::None, 64, 64);
    assert_eq!((placement.width, placement.height), (64, 64));
    assert_eq!((placement.x, placement.y), (0, 0));
    assert_eq!(buffer, frame.pixels, "copied, not resampled");
}

#[test]
fn a_one_pixel_sprite_survives_an_extreme_shrink() {
    // The shrink floor: a sprite must never come out zero-sized, because a
    // zero-width blit is a cursor that vanishes rather than one that is small.
    let frame = solid_frame(512, 512, 0xffff_ffff);
    let (_, placement) = composed(&frame, Rotation::None, 1, 1);
    assert_eq!((placement.width, placement.height), (1, 1));
}

#[test]
fn a_stride_wider_than_the_buffer_is_respected() {
    // Cursor buffers come from dumb allocations, whose stride is aligned and
    // routinely wider than width * 4. Writing at width instead of stride
    // shears the sprite.
    let frame = solid_frame(2, 2, 0xffff_ffff);
    let (w, h, stride) = (4u32, 4u32, 8usize);
    let mut buffer = vec![0u32; h as usize * stride];
    let placement = compose(&frame, Rotation::None, &mut buffer, w, h, stride);

    assert_eq!((placement.x, placement.y), (1, 1));
    assert_eq!(buffer[stride + 1], 0xffff_ffff);
    assert_eq!(buffer[2 * stride + 2], 0xffff_ffff);
    // The padding past the visible width is never written.
    for y in 0..h as usize {
        for x in w as usize..stride {
            assert_eq!(
                buffer[y * stride + x],
                0,
                "padding at ({x}, {y}) was touched"
            );
        }
    }
}

#[test]
fn the_drm_rotation_bits_are_the_uapi_ones() {
    // These are compared against a plane's supported mask, so a wrong bit
    // silently sends every rotation down the software path -- or worse, writes
    // a rotation the plane does not support.
    assert_eq!(Rotation::None.drm_mask(), 1);
    assert_eq!(Rotation::Quarter.drm_mask(), 2);
    assert_eq!(Rotation::Half.drm_mask(), 4);
    assert_eq!(Rotation::ThreeQuarter.drm_mask(), 8);
    assert!(Rotation::Quarter.swaps_axes() && Rotation::ThreeQuarter.swaps_axes());
    assert!(!Rotation::None.swaps_axes() && !Rotation::Half.swaps_axes());
}

// --- picking a plane -------------------------------------------------------

use crate::plane::{PlanePath, SelectedPlane, select_plane};
use drmkit_planes::{PlaneCapabilities, PlaneRegistry, PlaneType};

/// A plane on CRTC 0 that can carry ARGB8888.
fn plane(id: u32, plane_type: PlaneType) -> PlaneCapabilities {
    PlaneCapabilities {
        id,
        possible_crtcs: 0b1,
        plane_type,
        formats: vec![drmkit_fmt::fourcc::ARGB8888],
        ..PlaneCapabilities::default()
    }
}

/// The same, but carrying only an opaque format.
fn opaque_plane(id: u32, plane_type: PlaneType) -> PlaneCapabilities {
    PlaneCapabilities {
        formats: vec![drmkit_fmt::fourcc::XRGB8888],
        ..plane(id, plane_type)
    }
}

fn registry_of(planes: Vec<PlaneCapabilities>) -> PlaneRegistry {
    PlaneRegistry::from_capabilities(planes)
}

/// Nothing is bound to another CRTC.
fn all_free(_: u32) -> bool {
    false
}

#[test]
fn a_cursor_plane_wins() {
    // Per-CRTC hardware built for the job, with no overlay taken from anyone.
    let registry = registry_of(vec![
        plane(31, PlaneType::Primary),
        plane(32, PlaneType::Overlay),
        plane(33, PlaneType::Cursor),
    ]);
    let chosen = select_plane(&registry, 0, None, true, &all_free).expect("select");
    assert_eq!(chosen.plane_id, 33);
    assert_eq!(chosen.path, PlanePath::AtomicCursor);
}

#[test]
fn an_overlay_is_taken_when_there_is_no_cursor_plane() {
    let registry = registry_of(vec![
        plane(31, PlaneType::Primary),
        plane(32, PlaneType::Overlay),
    ]);
    let chosen = select_plane(&registry, 0, None, true, &all_free).expect("select");
    assert_eq!(chosen.plane_id, 32);
    assert_eq!(chosen.path, PlanePath::AtomicOverlay);
}

#[test]
fn a_cursor_plane_that_cannot_do_alpha_is_not_a_cursor_plane() {
    // A cursor without an alpha channel is a rectangle. Better to spend an
    // overlay than to draw one.
    let registry = registry_of(vec![
        opaque_plane(33, PlaneType::Cursor),
        plane(32, PlaneType::Overlay),
    ]);
    let chosen = select_plane(&registry, 0, None, true, &all_free).expect("select");
    assert_eq!(chosen.plane_id, 32);
    assert_eq!(chosen.path, PlanePath::AtomicOverlay);
}

#[test]
fn a_free_overlay_is_preferred_to_a_busy_one() {
    // Overlays are shared on many parts, and taking one another output is
    // using costs that output a layer.
    let registry = registry_of(vec![
        plane(32, PlaneType::Overlay),
        plane(34, PlaneType::Overlay),
    ]);
    let busy = |id: u32| id == 32;
    let chosen = select_plane(&registry, 0, None, true, &busy).expect("select");
    assert_eq!(chosen.plane_id, 34, "the free overlay, not the first one");
}

#[test]
fn a_busy_overlay_beats_no_cursor_at_all() {
    // Sharing an overlay is a cost; dropping to legacy is a bigger one,
    // because the cursor then cannot be committed with the frame.
    let registry = registry_of(vec![plane(32, PlaneType::Overlay)]);
    let chosen = select_plane(&registry, 0, None, true, &|_| true).expect("select");
    assert_eq!(chosen.plane_id, 32);
    assert_eq!(chosen.path, PlanePath::AtomicOverlay);
}

#[test]
fn the_primary_plane_is_never_taken() {
    // It is carrying the desktop. Putting the cursor on it would replace what
    // is on screen with a pointer.
    let registry = registry_of(vec![plane(31, PlaneType::Primary)]);
    let chosen = select_plane(&registry, 0, None, true, &all_free).expect("select");
    assert_eq!(chosen.path, PlanePath::Legacy);
    assert_eq!(chosen.plane_id, 0);
}

#[test]
fn legacy_is_the_last_resort_and_only_if_allowed() {
    let registry = registry_of(vec![opaque_plane(32, PlaneType::Overlay)]);
    assert_eq!(
        select_plane(&registry, 0, None, true, &all_free)
            .expect("select")
            .path,
        PlanePath::Legacy
    );
    // A compositor that must commit the cursor atomically with everything else
    // would rather be told than be quietly downgraded.
    assert_eq!(
        select_plane(&registry, 0, None, false, &all_free).unwrap_err(),
        CursorError::NoPlane
    );
}

#[test]
fn a_plane_on_another_crtc_is_not_a_candidate() {
    let mut elsewhere = plane(33, PlaneType::Cursor);
    elsewhere.possible_crtcs = 0b10; // CRTC 1 only
    let registry = registry_of(vec![elsewhere, plane(32, PlaneType::Overlay)]);
    let chosen = select_plane(&registry, 0, None, true, &all_free).expect("select");
    assert_eq!(chosen.plane_id, 32, "the cursor plane cannot feed CRTC 0");
}

#[test]
fn a_forced_plane_is_taken_as_asked() {
    let registry = registry_of(vec![
        plane(33, PlaneType::Cursor),
        plane(32, PlaneType::Overlay),
    ]);
    // The overlay, even though a cursor plane is available and would win.
    let chosen = select_plane(&registry, 0, Some(32), true, &all_free).expect("select");
    assert_eq!(chosen.plane_id, 32);
    assert_eq!(chosen.path, PlanePath::AtomicOverlay);
}

#[test]
fn a_forced_plane_that_will_not_work_is_refused_not_replaced() {
    // The caller overrode the policy. Quietly picking something else leaves
    // them on a plane they did not ask for, which is worse than an error.
    let registry = registry_of(vec![
        plane(33, PlaneType::Cursor),
        opaque_plane(32, PlaneType::Overlay),
    ]);
    let mut wrong_crtc = plane(35, PlaneType::Overlay);
    wrong_crtc.possible_crtcs = 0b10;
    let registry_with = registry_of(vec![plane(33, PlaneType::Cursor), wrong_crtc]);

    for (registry, forced, why) in [
        (&registry, 999, "a plane that does not exist"),
        (&registry, 32, "a plane that cannot do alpha"),
        (&registry_with, 35, "a plane on another CRTC"),
    ] {
        assert_eq!(
            select_plane(registry, 0, Some(forced), true, &all_free).unwrap_err(),
            CursorError::PlaneUnusable,
            "{why}"
        );
    }
}

#[test]
fn a_forced_plane_does_not_fall_back_to_legacy_either() {
    // Even with legacy allowed: the caller named a plane.
    let registry = registry_of(vec![plane(33, PlaneType::Cursor)]);
    assert_eq!(
        select_plane(&registry, 0, Some(999), true, &all_free).unwrap_err(),
        CursorError::PlaneUnusable
    );
}

#[test]
fn the_cursor_plane_reports_its_size_limit() {
    // The renderer sizes its buffer from this; a zero means the plane states
    // no limit of its own rather than a zero-by-zero cursor.
    let mut cursor = plane(33, PlaneType::Cursor);
    cursor.cursor_max_w = 64;
    cursor.cursor_max_h = 64;
    let registry = registry_of(vec![cursor, plane(32, PlaneType::Overlay)]);

    let chosen = select_plane(&registry, 0, None, true, &all_free).expect("select");
    assert_eq!(
        (chosen.cursor_max_w, chosen.cursor_max_h),
        (64, 64),
        "the cursor plane's own maximum"
    );

    let overlay = select_plane(&registry, 0, Some(32), true, &all_free).expect("select");
    assert_eq!(
        (overlay.cursor_max_w, overlay.cursor_max_h),
        (0, 0),
        "an overlay is bounded by the mode, not by a cursor limit"
    );
}

#[test]
fn an_empty_registry_falls_back() {
    let registry = registry_of(Vec::new());
    assert_eq!(
        select_plane(&registry, 0, None, true, &all_free)
            .expect("select")
            .path,
        PlanePath::Legacy
    );
    assert_eq!(
        select_plane(&registry, 0, None, false, &all_free).unwrap_err(),
        CursorError::NoPlane
    );
}

#[test]
fn the_selection_is_a_value_worth_comparing() {
    let registry = registry_of(vec![plane(33, PlaneType::Cursor)]);
    let a = select_plane(&registry, 0, None, true, &all_free).expect("select");
    let b = select_plane(&registry, 0, None, true, &all_free).expect("select");
    assert_eq!(a, b, "the same registry gives the same answer");
    assert_eq!(
        a,
        SelectedPlane {
            plane_id: 33,
            path: PlanePath::AtomicCursor,
            cursor_max_w: 0,
            cursor_max_h: 0,
        }
    );
}

// --- buffer size -----------------------------------------------------------

use crate::size::{preferred_size, probe_size};

#[test]
fn a_named_size_is_taken_as_asked() {
    for path in [
        PlanePath::AtomicCursor,
        PlanePath::AtomicOverlay,
        PlanePath::Legacy,
    ] {
        assert_eq!(preferred_size(128, path, Some(256)), 128, "{path:?}");
    }
}

#[test]
fn an_unset_size_takes_the_drivers_cap_on_the_atomic_paths() {
    // A plane that wants 256 should get 256, not a 64 that leaves the sprite
    // small in the middle of it.
    assert_eq!(preferred_size(0, PlanePath::AtomicCursor, Some(256)), 256);
    assert_eq!(preferred_size(0, PlanePath::AtomicOverlay, Some(128)), 128);
}

#[test]
fn legacy_ignores_the_cap() {
    // drmModeSetCursor has no capability query, and every driver hard-codes
    // 64, so a cap read from somewhere else does not apply to it.
    assert_eq!(preferred_size(0, PlanePath::Legacy, Some(256)), 64);
}

#[test]
fn a_missing_or_zero_cap_falls_back_to_64() {
    assert_eq!(preferred_size(0, PlanePath::AtomicCursor, None), 64);
    assert_eq!(preferred_size(0, PlanePath::AtomicCursor, Some(0)), 64);
}

#[test]
fn the_probe_takes_the_largest_size_the_kernel_accepts() {
    let mut asked = Vec::new();
    let size = probe_size(256, |candidate| {
        asked.push(candidate);
        candidate <= 128
    });
    assert_eq!(size, 128);
    assert_eq!(
        asked,
        vec![256, 128],
        "largest first, stopping at the first yes"
    );
}

#[test]
fn the_probe_never_goes_above_what_was_asked_for() {
    // A caller who wanted a small cursor does not get a large one because the
    // driver could manage it.
    let mut asked = Vec::new();
    probe_size(64, |candidate| {
        asked.push(candidate);
        true
    });
    assert_eq!(asked, vec![64], "256 and 128 are above the preference");
}

#[test]
fn a_size_between_ladder_steps_probes_the_step_below() {
    // The ladder is the sizes hardware actually implements. Asking for 100
    // gets 64 tried, not 100: there is no driver that does 100.
    let mut asked = Vec::new();
    let size = probe_size(100, |candidate| {
        asked.push(candidate);
        true
    });
    assert_eq!(asked, vec![64]);
    assert_eq!(size, 64);
}

#[test]
fn a_size_below_the_floor_still_gets_a_buffer() {
    // Nothing on the ladder is at or below 32, so nothing is probed, and the
    // answer is the 64 floor. A 32-pixel cursor is drawn centred inside it
    // rather than not at all.
    let mut asked = Vec::new();
    let size = probe_size(32, |candidate| {
        asked.push(candidate);
        true
    });
    assert!(asked.is_empty(), "nothing on the ladder fits under 32");
    assert_eq!(size, 64);
}

#[test]
fn a_kernel_that_refuses_everything_still_yields_a_size() {
    // Usually this is not about size at all: with no mode set, every
    // TEST_ONLY fails for unrelated reasons. There still has to be a buffer,
    // and the first real commit will surface the objection.
    let mut asked = Vec::new();
    let size = probe_size(256, |candidate| {
        asked.push(candidate);
        false
    });
    assert_eq!(asked, vec![256, 128, 64], "every candidate was tried");
    assert_eq!(size, 256, "falls back to what was asked for");

    // And the floor applies to the fallback too.
    assert_eq!(probe_size(16, |_| false), 64);
}

#[test]
fn the_probe_stops_asking_once_something_is_accepted() {
    // Each probe allocates a buffer and issues a test commit, so asking after
    // an answer is real work for nothing.
    let mut calls = 0;
    let size = probe_size(256, |_| {
        calls += 1;
        true
    });
    assert_eq!(calls, 1);
    assert_eq!(size, 256);
}

// --- placing the plane -----------------------------------------------------

use crate::stage::{hardware_rotation, plane_origin};

#[test]
fn the_plane_is_offset_back_by_the_hotspot() {
    // The hotspot is the pixel that lands under the pointer. Skip this and the
    // sprite looks right while every click lands somewhere else.
    assert_eq!(plane_origin((100, 100), (0, 0)), (100, 100));
    assert_eq!(plane_origin((100, 100), (4, 7)), (96, 93));
}

#[test]
fn a_cursor_near_the_top_left_hangs_off_the_edge() {
    // A negative origin is correct, not something to clamp: the pointer is at
    // (1, 1) and the tip of the arrow is 4 pixels into the buffer, so the
    // buffer starts off-screen.
    assert_eq!(plane_origin((1, 1), (4, 4)), (-3, -3));
    assert_eq!(plane_origin((0, 0), (31, 31)), (-31, -31));
}

#[test]
fn the_origin_does_not_wrap_at_the_extremes() {
    // A pointer coordinate near i32::MIN with a large hotspot would wrap to a
    // huge positive number, putting the cursor at the far edge instead of off
    // the near one.
    assert_eq!(plane_origin((i32::MIN, i32::MIN), (64, 64)).0, i32::MIN);
    assert_eq!(plane_origin((i32::MAX, 0), (0, 0)).0, i32::MAX);
}

#[test]
fn hardware_rotation_is_used_when_the_plane_has_it() {
    let all = Rotation::None.drm_mask()
        | Rotation::Quarter.drm_mask()
        | Rotation::Half.drm_mask()
        | Rotation::ThreeQuarter.drm_mask();
    for rotation in [
        Rotation::None,
        Rotation::Quarter,
        Rotation::Half,
        Rotation::ThreeQuarter,
    ] {
        assert_eq!(
            hardware_rotation(rotation, all),
            rotation.drm_mask(),
            "{rotation:?}"
        );
    }
}

#[test]
fn an_unsupported_rotation_asks_the_hardware_for_none() {
    // i915's cursor planes expose the property but list only 0 and 180. A
    // quarter turn there is done in software, so telling the hardware to turn
    // it as well would turn it twice.
    let half_only = Rotation::None.drm_mask() | Rotation::Half.drm_mask();
    assert_eq!(
        hardware_rotation(Rotation::Half, half_only),
        Rotation::Half.drm_mask(),
        "180 is in the mask, so the hardware does it"
    );
    for software in [Rotation::Quarter, Rotation::ThreeQuarter] {
        assert_eq!(
            hardware_rotation(software, half_only),
            Rotation::None.drm_mask(),
            "{software:?} is not in the mask, so the pixels were already turned"
        );
    }
}

#[test]
fn a_plane_with_no_rotation_support_asks_for_none() {
    for rotation in [Rotation::None, Rotation::Quarter, Rotation::Half] {
        assert_eq!(
            hardware_rotation(rotation, 0),
            Rotation::None.drm_mask(),
            "{rotation:?}"
        );
    }
}
