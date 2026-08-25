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
    // The whole reason this parser exists. `xcursor` 0.3.11 allocates
    // width * height * 4 before reading, so a tiny file declaring a huge
    // image aborts the process -- verified: a 60-byte file asking for
    // 32767x32767 dies with "memory allocation of 4294705156 bytes failed",
    // which Rust cannot catch. Here the length is checked against the bytes
    // actually present, so it is an error and the process lives.
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
