// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! Property tests for the bipartite matcher (plan §9 T1).
//!
//! The phase 2 gate names "cardinality-first" as a required proptest invariant.
//! The check here is the strong form: for every generated graph small enough to
//! brute-force, the matcher's result must equal the true maximum. A weaker
//! self-consistency check — "the matching is valid" — would pass against an
//! implementation that simply placed fewer layers.

use drmkit_planes::BipartiteMatching;
use proptest::prelude::*;

/// True maximum matching, by exhaustive search. Correct by construction and
/// far too slow for production, which is exactly what makes it a good oracle.
fn brute_force_maximum(n_left: usize, n_right: usize, edges: &[(usize, usize)]) -> usize {
    fn search(
        left: usize,
        n_left: usize,
        used_right: u32,
        edges: &[(usize, usize)],
        best: &mut usize,
        placed: usize,
    ) {
        if left == n_left {
            *best = (*best).max(placed);
            return;
        }
        // Leave this layer unplaced.
        search(left + 1, n_left, used_right, edges, best, placed);
        // Or place it on any free, compatible plane.
        for &(u, v) in edges {
            if u == left && used_right & (1 << v) == 0 {
                search(
                    left + 1,
                    n_left,
                    used_right | (1 << v),
                    edges,
                    best,
                    placed + 1,
                );
            }
        }
    }

    let _ = n_right;
    let mut best = 0;
    search(0, n_left, 0, edges, &mut best, 0);
    best
}

fn graph_strategy() -> impl Strategy<Value = (usize, usize, Vec<(usize, usize)>)> {
    (1usize..=6, 1usize..=6).prop_flat_map(|(n_left, n_right)| {
        proptest::collection::vec((0..n_left, 0..n_right), 0..=(n_left * n_right)).prop_map(
            move |mut edges| {
                edges.sort_unstable();
                edges.dedup();
                (n_left, n_right, edges)
            },
        )
    })
}

proptest! {
    /// **Cardinality-first.** The matcher must find a true maximum matching.
    #[test]
    fn finds_a_maximum_matching((n_left, n_right, edges) in graph_strategy()) {
        let mut matching = BipartiteMatching::with_shape(n_left, n_right);
        for &(u, v) in &edges {
            matching.add_edge(u, v);
        }
        let found = matching.solve();

        prop_assert_eq!(
            found,
            brute_force_maximum(n_left, n_right, &edges),
            "matcher found {} placements for {:?}, brute force found more",
            found,
            edges
        );
    }

    /// **Scores never reduce cardinality.** This is what makes the allocator's
    /// warm-start stability bonus safe: it can reorder preferences but can
    /// never cost a layer its plane.
    #[test]
    fn scores_never_change_cardinality(
        (n_left, n_right, edges) in graph_strategy(),
        scores in proptest::collection::vec(-1000i32..1000, 0..=36),
    ) {
        let mut unscored = BipartiteMatching::with_shape(n_left, n_right);
        for &(u, v) in &edges {
            unscored.add_edge(u, v);
        }
        let baseline = unscored.solve();

        let mut scored = BipartiteMatching::with_shape(n_left, n_right);
        for (index, &(u, v)) in edges.iter().enumerate() {
            scored.add_scored_edge(u, v, scores.get(index).copied().unwrap_or(0));
        }
        let with_scores = scored.solve();

        prop_assert_eq!(
            baseline, with_scores,
            "scoring changed the placement count from {} to {}",
            baseline, with_scores
        );
    }

    /// The reported matching must be internally consistent and legal: every
    /// pair is a real edge, and the two directions agree.
    #[test]
    fn matching_is_valid_and_symmetric((n_left, n_right, edges) in graph_strategy()) {
        let mut matching = BipartiteMatching::with_shape(n_left, n_right);
        for &(u, v) in &edges {
            matching.add_edge(u, v);
        }
        let count = matching.solve();

        let mut seen = 0;
        for u in 0..n_left {
            if let Some(v) = matching.match_for_left(u) {
                seen += 1;
                prop_assert!(edges.contains(&(u, v)), "matched a non-existent edge");
                prop_assert_eq!(
                    matching.match_for_right(v),
                    Some(u),
                    "the two directions disagree"
                );
            }
        }
        prop_assert_eq!(seen, count, "matched_count disagrees with the pairs");
    }

    /// Solving twice on the same graph gives the same answer -- the matcher
    /// resets its own state, so a per-frame caller reusing one instance is not
    /// carrying last frame's pairing forward.
    #[test]
    fn solve_is_idempotent((n_left, n_right, edges) in graph_strategy()) {
        let mut matching = BipartiteMatching::with_shape(n_left, n_right);
        for &(u, v) in &edges {
            matching.add_edge(u, v);
        }
        let first = matching.solve();
        let pairs_first: Vec<_> = (0..n_left).map(|u| matching.match_for_left(u)).collect();

        let second = matching.solve();
        let pairs_second: Vec<_> = (0..n_left).map(|u| matching.match_for_left(u)).collect();

        prop_assert_eq!(first, second);
        prop_assert_eq!(pairs_first, pairs_second);
    }

    /// `reset` must leave no trace of the previous graph.
    #[test]
    fn reset_clears_previous_edges((n_left, n_right, edges) in graph_strategy()) {
        let mut matching = BipartiteMatching::with_shape(n_left, n_right);
        for &(u, v) in &edges {
            matching.add_edge(u, v);
        }
        matching.solve();

        matching.reset(n_left, n_right);
        prop_assert_eq!(matching.solve(), 0, "reset must drop every edge");
        for u in 0..n_left {
            prop_assert_eq!(matching.match_for_left(u), None);
        }
    }
}
