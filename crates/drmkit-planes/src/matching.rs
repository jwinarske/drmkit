// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! Maximum-cardinality bipartite matching, Hopcroft–Karp.
//!
//! Port of `src/planes/matching.{hpp,cpp}`.
//!
//! Plane allocation is a bipartite graph:
//!
//! - **left** nodes are layers,
//! - **right** nodes are planes,
//! - an **edge** means the layer could potentially use that plane.
//!
//! Runs in `O(E·√V)` — microseconds at the counts that occur in practice,
//! where both sides are usually eight or fewer.
//!
//! # Cardinality first, score second
//!
//! The matcher maximizes the **number of placements**. Scores only order the
//! search, so a preferred plane is tried first among otherwise equal options.
//! A score can never cause a layer to go unplaced that could have been placed.
//! That ordering is what makes the allocator's stability bonus safe to add —
//! see [`Allocator`](crate::Allocator).

use std::collections::VecDeque;

/// One candidate placement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Edge {
    right: usize,
    /// Higher is more preferred. Ordering only; never affects cardinality.
    score: i32,
}

/// A reusable Hopcroft–Karp matcher.
///
/// Built to be reset and refilled each frame: the internal vectors keep their
/// capacity across [`reset`](Self::reset), so a caller with a stable graph
/// shape pays one set of allocations on the first frame and none after.
#[derive(Debug, Default, Clone)]
pub struct BipartiteMatching {
    n_left: usize,
    n_right: usize,
    /// `adj[u]` is the edges out of left node `u`. Sorted by descending score
    /// in [`solve`](Self::solve).
    adj: Vec<Vec<Edge>>,
    match_left: Vec<Option<usize>>,
    match_right: Vec<Option<usize>>,
    /// BFS layer distance per left node, plus one sentinel slot at `n_left`
    /// standing for "reached a free right node". `None` is infinity.
    dist: Vec<Option<usize>>,
    matched: usize,
}

impl BipartiteMatching {
    /// An empty matcher. Shape it with [`reset`](Self::reset).
    #[must_use]
    pub const fn new() -> Self {
        Self {
            n_left: 0,
            n_right: 0,
            adj: Vec::new(),
            match_left: Vec::new(),
            match_right: Vec::new(),
            dist: Vec::new(),
            matched: 0,
        }
    }

    /// A matcher shaped for `n_left` layers and `n_right` planes.
    #[must_use]
    pub fn with_shape(n_left: usize, n_right: usize) -> Self {
        let mut matching = Self::new();
        matching.reset(n_left, n_right);
        matching
    }

    /// Re-shape and clear, keeping allocated capacity.
    ///
    /// Every per-node container is reset, so a fresh call sees exactly the
    /// state a freshly constructed matcher would.
    pub fn reset(&mut self, n_left: usize, n_right: usize) {
        self.n_left = n_left;
        self.n_right = n_right;
        self.matched = 0;

        // `resize` leaves existing slots' edges intact, so clear each one: the
        // caller wants a fresh edge list, not last frame's.
        self.adj.resize_with(n_left, Vec::new);
        self.adj.truncate(n_left);
        for edges in &mut self.adj {
            edges.clear();
        }

        self.match_left.clear();
        self.match_left.resize(n_left, None);
        self.match_right.clear();
        self.match_right.resize(n_right, None);
        self.dist.clear();
        self.dist.resize(n_left + 1, None);
    }

    /// Add an unscored edge: layer `left` could use plane `right`.
    ///
    /// Out-of-range endpoints are ignored rather than panicking — a caller
    /// filtering candidates should not have to re-check bounds the matcher
    /// already knows.
    pub fn add_edge(&mut self, left: usize, right: usize) {
        self.add_scored_edge(left, right, 0);
    }

    /// Add an edge with a preference score. Higher is tried first.
    ///
    /// The score cannot change how *many* layers get placed — see the module
    /// documentation.
    pub fn add_scored_edge(&mut self, left: usize, right: usize, score: i32) {
        if left >= self.n_left || right >= self.n_right {
            return;
        }
        self.adj[left].push(Edge { right, score });
    }

    /// Compute the maximum-cardinality matching, returning how many pairs it
    /// found.
    pub fn solve(&mut self) -> usize {
        self.matched = 0;
        for edges in &mut self.match_left {
            *edges = None;
        }
        for edges in &mut self.match_right {
            *edges = None;
        }

        // Sort each adjacency list by descending score so the DFS tries the
        // caller's preferred planes first. A stable sort keeps insertion order
        // among equal scores, which makes the result deterministic. Without
        // this, Hopcroft-Karp still finds a maximum matching but pairs
        // arbitrarily -- wrong when the caller expressed a preference.
        for edges in &mut self.adj {
            edges.sort_by(|a, b| b.score.cmp(&a.score));
        }

        while self.bfs() {
            for u in 0..self.n_left {
                if self.match_left[u].is_none() && self.dfs(u) {
                    self.matched += 1;
                }
            }
        }
        self.matched
    }

    /// Build the BFS layering. Returns whether an augmenting path exists.
    fn bfs(&mut self) -> bool {
        let sentinel = self.n_left;
        let mut queue = VecDeque::with_capacity(self.n_left);

        for u in 0..self.n_left {
            if self.match_left[u].is_none() {
                self.dist[u] = Some(0);
                queue.push_back(u);
            } else {
                self.dist[u] = None; // infinity
            }
        }
        self.dist[sentinel] = None;

        while let Some(u) = queue.pop_front() {
            // Only expand nodes closer than the best free right node found so
            // far; `None` at the sentinel means none has been reached yet.
            let closer = match (self.dist[u], self.dist[sentinel]) {
                (Some(_), None) => true,
                (Some(du), Some(ds)) => du < ds,
                (None, _) => false,
            };
            if !closer {
                continue;
            }
            let Some(du) = self.dist[u] else { continue };

            for index in 0..self.adj[u].len() {
                let v = self.adj[u][index].right;
                let next = self.match_right[v].unwrap_or(sentinel);
                if self.dist[next].is_none() {
                    self.dist[next] = Some(du + 1);
                    if next != sentinel {
                        queue.push_back(next);
                    }
                }
            }
        }

        self.dist[sentinel].is_some()
    }

    /// Walk one augmenting path from left node `u`.
    ///
    /// Recursion depth is bounded by the number of left nodes: each step moves
    /// to a strictly deeper BFS layer, and there are at most `n_left + 1` of
    /// them. With layer counts in the single digits this cannot approach the
    /// stack limit.
    fn dfs(&mut self, u: usize) -> bool {
        let sentinel = self.n_left;
        if u == sentinel {
            return true; // reached a free right node
        }

        let Some(du) = self.dist[u] else {
            return false;
        };

        for index in 0..self.adj[u].len() {
            let v = self.adj[u][index].right;
            let next = self.match_right[v].unwrap_or(sentinel);
            if self.dist[next] == Some(du + 1) && self.dfs(next) {
                self.match_right[v] = Some(u);
                self.match_left[u] = Some(v);
                return true;
            }
        }

        self.dist[u] = None; // exhausted; do not revisit this round
        false
    }

    /// The plane matched to a layer, after [`solve`](Self::solve).
    #[must_use]
    pub fn match_for_left(&self, left: usize) -> Option<usize> {
        self.match_left.get(left).copied().flatten()
    }

    /// The layer matched to a plane, after [`solve`](Self::solve).
    #[must_use]
    pub fn match_for_right(&self, right: usize) -> Option<usize> {
        self.match_right.get(right).copied().flatten()
    }

    /// How many pairs the last [`solve`](Self::solve) found.
    #[must_use]
    pub const fn matched_count(&self) -> usize {
        self.matched
    }

    /// The number of left nodes.
    #[must_use]
    pub const fn left_len(&self) -> usize {
        self.n_left
    }

    /// The number of right nodes.
    #[must_use]
    pub const fn right_len(&self) -> usize {
        self.n_right
    }
}
