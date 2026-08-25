// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! Resolving a cursor name to a file on disk.
//!
//! Walks the XDG cursor search path, indexes every theme that looks like it
//! carries cursors, follows each theme's `Inherits=` chain, and expands cursor
//! names through the alias table. What it does *not* do is read pixels: it
//! returns a path and the theme it came from, which keeps it cheap and lets it
//! be tested without a display or a megabyte of image data.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::CursorError;
use crate::alias;

/// Where a cursor name resolved to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThemeResolution {
    /// The theme the cursor was actually found in, which may not be the one
    /// asked for -- the `Inherits=` chain or the fallback may have supplied it.
    pub theme_name: String,
    /// The cursor file on disk.
    pub source: PathBuf,
    /// Every theme considered, in order. For diagnostics: it answers "why did
    /// I get this one" without re-running the walk.
    pub chain: Vec<String>,
}

/// One indexed theme.
#[derive(Debug)]
struct Record {
    name: String,
    /// Every search-path directory providing this theme, in path priority
    /// order. More than one is normal -- a user override under `~/.icons` and
    /// the system copy under `/usr/share/icons` share a name.
    dirs: Vec<PathBuf>,
    inherits: Vec<String>,
}

/// What a resolve is keyed on. Both halves fully determine the outcome.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct CacheKey {
    cursor_name: String,
    preferred_theme: String,
}

/// An index of the cursor themes on disk.
///
/// Cheap to construct -- a directory walk and a small metadata parse, no
/// pixels. Resolutions are memoized for the index's lifetime, hits and misses
/// alike, so a caller cycling through a fixed set of shapes pays the walk once
/// per name.
///
/// The index is a snapshot: cursors appearing or disappearing on disk
/// afterwards are not noticed. Build a new one, which is cheap.
///
/// Not `Sync`. The memo cache is a [`RefCell`], matching the reference, which
/// documents `Theme` as unsynchronised. Sharing one across threads is a
/// compile error rather than a race.
#[derive(Debug)]
pub struct Theme {
    themes: Vec<Record>,
    by_name: HashMap<String, usize>,
    search_paths: Vec<PathBuf>,
    cache: RefCell<HashMap<CacheKey, Result<ThemeResolution, CursorError>>>,
}

impl Theme {
    /// Index the themes on the standard search path.
    ///
    /// # Errors
    ///
    /// [`CursorError::NoThemes`] when no theme directory is readable at all.
    /// That is deliberately distinct from a resolve failing on a healthy
    /// index: one means nothing is installed, the other means this cursor is
    /// not in what *is* installed, and a caller wants to say different things
    /// about them.
    pub fn discover() -> Result<Self, CursorError> {
        Self::discover_with_paths(&default_search_paths())
    }

    /// Index the themes under an explicit set of roots.
    ///
    /// The seam the tests use, so they can point at a fixture tree rather than
    /// whatever the machine happens to have installed.
    ///
    /// # Errors
    ///
    /// As [`Theme::discover`].
    pub fn discover_with_paths(search_paths: &[PathBuf]) -> Result<Self, CursorError> {
        let mut themes: Vec<Record> = Vec::new();
        let mut by_name: HashMap<String, usize> = HashMap::new();

        for root in search_paths {
            let Ok(entries) = std::fs::read_dir(root) else {
                continue;
            };
            // Sorted, because `read_dir` order is whatever the filesystem
            // feels like and discovery order decides the fallback order. An
            // unsorted walk makes the last-resort theme choice differ between
            // machines with identical trees.
            let mut dirs: Vec<PathBuf> = entries
                .flatten()
                .map(|entry| entry.path())
                .filter(|path| path.is_dir())
                .collect();
            dirs.sort();

            for theme_dir in dirs {
                // A directory counts as a theme if it carries cursors or says
                // anything about itself. Indexing metadata-only themes matters
                // because inheritance passes *through* cursor-less
                // intermediates.
                let has_cursors = theme_dir.join("cursors").exists();
                let index_theme = theme_dir.join("index.theme");
                let cursor_theme = theme_dir.join("cursor.theme");
                if !has_cursors && !index_theme.exists() && !cursor_theme.exists() {
                    continue;
                }
                let Some(name) = theme_dir.file_name().and_then(|n| n.to_str()) else {
                    continue;
                };
                if name.is_empty() {
                    continue;
                }

                let index = *by_name.entry(name.to_owned()).or_insert_with(|| {
                    themes.push(Record {
                        name: name.to_owned(),
                        dirs: Vec::new(),
                        inherits: Vec::new(),
                    });
                    themes.len() - 1
                });
                themes[index].dirs.push(theme_dir.clone());

                // Parsed once per theme name, from the first directory that
                // has something to say. Two copies disagreeing means the
                // earlier search path wins, the same precedence as everywhere
                // else here.
                if themes[index].inherits.is_empty() {
                    let mut inherits = parse_inherits(&cursor_theme);
                    if inherits.is_empty() {
                        inherits = parse_inherits(&index_theme);
                    }
                    themes[index].inherits = inherits;
                }
            }
        }

        if themes.is_empty() {
            return Err(CursorError::NoThemes);
        }

        Ok(Self {
            themes,
            by_name,
            search_paths: search_paths.to_vec(),
            cache: RefCell::new(HashMap::new()),
        })
    }

    /// The roots this index was built from.
    #[must_use]
    pub fn search_paths(&self) -> &[PathBuf] {
        &self.search_paths
    }

    /// How many resolutions are memoized.
    ///
    /// Exists so the tests can check that a refused name never reaches the
    /// cache, which is otherwise unobservable from outside.
    #[cfg(test)]
    pub(crate) fn cache_len(&self) -> usize {
        self.cache.borrow().len()
    }

    /// The themes indexed, in discovery order.
    #[must_use]
    pub fn theme_names(&self) -> Vec<&str> {
        self.themes.iter().map(|r| r.name.as_str()).collect()
    }

    /// Resolve a cursor name to a file.
    ///
    /// `preferred_theme` may be empty, in which case `$XCURSOR_THEME` is used,
    /// then `default`. The `Inherits=` chain is followed breadth-first and
    /// every alias of the name is tried in each theme.
    ///
    /// # Errors
    ///
    /// - [`CursorError::InvalidName`] for a name that is empty, `.`, `..`, or
    ///   contains a path separator.
    /// - [`CursorError::NotFound`] if no theme on the search path has it.
    pub fn resolve(
        &self,
        cursor_name: &str,
        preferred_theme: &str,
    ) -> Result<ThemeResolution, CursorError> {
        // The name is caller-controlled and gets joined onto a directory, so
        // `../../etc/passwd` would compose into a real path. A compositor
        // should sanitise upstream; the cost of refusing an ambiguous name
        // here is one comparison. `..` is the classic escape, and a separator
        // anywhere means the caller passed a path where an identifier belongs.
        //
        // Checked before the cache is touched, so a caller spamming bogus
        // names cannot grow the map.
        if cursor_name.is_empty()
            || cursor_name == "."
            || cursor_name == ".."
            || cursor_name.contains('/')
            || cursor_name.contains('\\')
        {
            return Err(CursorError::InvalidName);
        }

        let key = CacheKey {
            cursor_name: cursor_name.to_owned(),
            preferred_theme: preferred_theme.to_owned(),
        };
        if let Some(hit) = self.cache.borrow().get(&key) {
            return hit.clone();
        }

        let outcome = self.walk(cursor_name, preferred_theme);
        self.cache.borrow_mut().insert(key, outcome.clone());
        outcome
    }

    /// The uncached resolve.
    fn walk(
        &self,
        cursor_name: &str,
        preferred_theme: &str,
    ) -> Result<ThemeResolution, CursorError> {
        let mut start = preferred_theme.to_owned();
        if start.is_empty() {
            start = env_string("XCURSOR_THEME");
        }
        if start.is_empty() {
            // Nearly every distribution ships this as a symlink to whatever
            // the active cursor pack is.
            "default".clone_into(&mut start);
        }

        let mut chain: Vec<String> = Vec::new();
        let mut visited: HashSet<String> = HashSet::new();

        if let Some(found) = self.bfs(&start, cursor_name, &mut chain, &mut visited) {
            return Ok(found);
        }

        // The named theme and its parents did not have it. Try the system
        // `default` chain before falling back to discovery order: on most
        // distributions `default` points at the active cursor pack, so this
        // gets a sensible pointer rather than whichever theme sorted first.
        // Skipped when the walk already covered it.
        if start != "default"
            && !visited.contains("default")
            && let Some(found) = self.bfs("default", cursor_name, &mut chain, &mut visited)
        {
            return Ok(found);
        }

        // Last resort: every remaining theme in discovery order, which is
        // search-path order, so user directories come before system ones.
        for record in &self.themes {
            if chain.iter().any(|seen| seen == &record.name) {
                continue;
            }
            let found = record
                .dirs
                .iter()
                .find_map(|dir| find_in_cursors_dir(&dir.join("cursors"), cursor_name));
            chain.push(record.name.clone());
            if let Some(source) = found {
                return Ok(ThemeResolution {
                    theme_name: record.name.clone(),
                    source,
                    chain,
                });
            }
        }

        Err(CursorError::NotFound)
    }

    /// Breadth-first walk of the inherits graph from `root`, checking each
    /// theme for the cursor as it is reached.
    ///
    /// `chain` and `visited` are threaded through so a second root can be
    /// walked onto the same state without redoing work or re-reporting themes.
    fn bfs(
        &self,
        root: &str,
        cursor_name: &str,
        chain: &mut Vec<String>,
        visited: &mut HashSet<String>,
    ) -> Option<ThemeResolution> {
        let mut queue: std::collections::VecDeque<String> =
            std::collections::VecDeque::from([root.to_owned()]);

        while let Some(current) = queue.pop_front() {
            if !visited.insert(current.clone()) {
                continue;
            }
            chain.push(current.clone());

            let Some(&index) = self.by_name.get(&current) else {
                // A theme named by an Inherits= that is not installed. Still
                // recorded in the chain: "we looked for it and it was not
                // there" is exactly what the diagnostics are for.
                continue;
            };
            let record = &self.themes[index];
            for parent in &record.inherits {
                queue.push_back(parent.clone());
            }
            for dir in &record.dirs {
                if let Some(source) = find_in_cursors_dir(&dir.join("cursors"), cursor_name) {
                    return Some(ThemeResolution {
                        theme_name: current,
                        source,
                        chain: std::mem::take(chain),
                    });
                }
            }
        }
        None
    }
}

/// The first alias of `cursor_name` that exists in `cursors_dir`.
fn find_in_cursors_dir(cursors_dir: &Path, cursor_name: &str) -> Option<PathBuf> {
    let mut found = None;
    alias::for_each(cursor_name, |candidate| {
        if found.is_some() {
            return;
        }
        let path = cursors_dir.join(candidate);
        if path.exists() {
            found = Some(path);
        }
    });
    found
}

/// An environment variable as a `String`, empty when unset or not UTF-8.
fn env_string(name: &str) -> String {
    std::env::var(name).unwrap_or_default()
}

/// Split a `PATH`-style value. Empty entries are dropped, so a leading or
/// trailing separator does not contribute an empty path.
fn split_path_list(value: &str) -> Vec<PathBuf> {
    value
        .split(':')
        .filter(|part| !part.is_empty())
        .map(PathBuf::from)
        .collect()
}

/// The default cursor search path.
///
/// Mirrors libxcursor: `XCURSOR_PATH` wins outright; otherwise the user and
/// system XDG roots with `/icons` appended, plus `/usr/share/pixmaps`, which
/// themes still drop cursors into.
#[must_use]
pub fn default_search_paths() -> Vec<PathBuf> {
    let explicit = env_string("XCURSOR_PATH");
    if !explicit.is_empty() {
        return split_path_list(&explicit);
    }

    let mut out = Vec::new();
    let home = env_string("HOME");
    if !home.is_empty() {
        out.push(PathBuf::from(&home).join(".icons"));
    }

    let data_home = env_string("XDG_DATA_HOME");
    if data_home.is_empty() {
        if !home.is_empty() {
            out.push(
                PathBuf::from(&home)
                    .join(".local")
                    .join("share")
                    .join("icons"),
            );
        }
    } else {
        for path in split_path_list(&data_home) {
            out.push(path.join("icons"));
        }
    }

    let data_dirs = env_string("XDG_DATA_DIRS");
    let dirs = if data_dirs.is_empty() {
        split_path_list("/usr/local/share:/usr/share")
    } else {
        split_path_list(&data_dirs)
    };
    for path in dirs {
        out.push(path.join("icons"));
    }

    out.push(PathBuf::from("/usr/share/pixmaps"));
    out
}

/// Read `Inherits=` out of an `index.theme`-style file.
///
/// Only that one key is needed, so this is a few lines of ini parsing rather
/// than a dependency. Returns empty for a missing or unreadable file, which is
/// the same as a theme that inherits nothing -- there is no useful difference
/// to a caller.
fn parse_inherits(path: &Path) -> Vec<String> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };

    let mut in_icon_theme = false;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.starts_with('[') {
            // A new section. If the Icon Theme section is over, so is the
            // search: a later section's `Inherits` is not ours.
            if in_icon_theme {
                break;
            }
            in_icon_theme = line == "[Icon Theme]";
            continue;
        }
        if !in_icon_theme {
            continue;
        }
        let Some(rest) = line.strip_prefix("Inherits") else {
            continue;
        };
        let Some(rest) = rest.trim_start().strip_prefix('=') else {
            continue;
        };
        return rest
            // The spec says comma; themes in the wild also use semicolon. No
            // known theme mixes them in one list.
            .split([',', ';'])
            .map(str::trim)
            .filter(|parent| !parent.is_empty())
            .map(str::to_owned)
            .collect();
    }
    Vec::new()
}
