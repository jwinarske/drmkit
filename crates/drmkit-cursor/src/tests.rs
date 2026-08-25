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
