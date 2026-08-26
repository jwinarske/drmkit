// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! Parity port of `tests/unit/test_buffer_ring.cpp` from drm-cxx @ `4a0b64a`.
//!
//! Upstream is one binary with a `CHECK` macro and a failure counter, so a
//! break there reports a line number and keeps going. These are separate
//! cases, which is what makes a break say *which contract* broke.

use crate::{BufferRing, Rect, Repaint};

fn rect(x: i32, y: i32, width: u32, height: u32) -> Rect {
    Rect {
        x,
        y,
        width,
        height,
    }
}

/// The ring grows on demand and stops at its cap, and the scanning slot is
/// never handed out.
#[test]
fn it_grows_to_capacity_and_never_leases_the_scanning_slot() {
    let mut ring = BufferRing::new(2, BufferRing::DEFAULT_MAX_DAMAGE_RECTS);
    assert_eq!(ring.max_slots(), 2);
    assert!(ring.is_empty());

    let a = ring.acquire().expect("the first acquire grows a slot");
    assert!(a.fresh, "a grown slot has no buffer yet");
    assert_eq!(a.repaint, Repaint::Full, "and so nothing to preserve");
    assert_eq!(ring.len(), 1);
    assert_eq!(ring.in_flight(), 1);

    ring.present(a.slot, &[]);

    let b = ring
        .acquire()
        .expect("a second slot is still under the cap");
    assert_ne!(b.slot, a.slot, "the scanning slot is not available");
    assert!(b.fresh);
    assert_eq!(ring.len(), 2);

    assert!(
        ring.acquire().is_none(),
        "one scanning and one in flight, at capacity: the producer waits"
    );
}

/// A released slot is reused rather than grown past, and the scanning slot
/// stays out of reach even when it is the only alternative.
#[test]
fn a_released_slot_is_reused_before_the_ring_grows() {
    let mut ring = BufferRing::new(2, BufferRing::DEFAULT_MAX_DAMAGE_RECTS);
    let a = ring.acquire().expect("first");
    ring.present(a.slot, &[]);
    let b = ring.acquire().expect("second");
    ring.present(b.slot, &[]);
    ring.release(a.slot);

    let c = ring.acquire().expect("the released slot comes back");
    assert_eq!(c.slot, a.slot);
    assert!(!c.fresh, "it already has a buffer");
    assert_eq!(ring.len(), 2, "and the ring did not grow");

    // Abandon it and ask again: the still-scanning slot must not be offered.
    ring.release(c.slot);
    let d = ring.acquire().expect("the abandoned slot comes back");
    assert_eq!(d.slot, c.slot);
    assert_ne!(d.slot, b.slot, "b is scanning");
}

/// Age counts presents since the slot was last presented, and a slot that has
/// never been presented reports zero.
#[test]
fn age_counts_presents_since_the_slot_was_last_on_screen() {
    let mut ring = BufferRing::new(3, BufferRing::DEFAULT_MAX_DAMAGE_RECTS);
    let a = ring.acquire().expect("a");
    ring.present(a.slot, &[]);
    assert_eq!(ring.age(a.slot), 0, "just presented");

    let b = ring.acquire().expect("b");
    ring.present(b.slot, &[]);
    assert_eq!(ring.age(a.slot), 1);

    let c = ring.acquire().expect("c");
    ring.present(c.slot, &[]);
    assert_eq!(ring.age(a.slot), 2);
    assert_eq!(ring.age(b.slot), 1);

    let mut fresh = BufferRing::new(2, BufferRing::DEFAULT_MAX_DAMAGE_RECTS);
    let never = fresh.acquire().expect("grown but never presented");
    assert_eq!(
        fresh.age(never.slot),
        0,
        "undefined contents, not current ones"
    );
}

/// Double buffering: a slot one frame stale repaints exactly that frame's
/// damage.
#[test]
fn a_slot_one_frame_stale_repaints_that_frame() {
    let mut ring = BufferRing::new(2, BufferRing::DEFAULT_MAX_DAMAGE_RECTS);
    let f1 = ring.acquire().expect("f1");
    ring.present(f1.slot, &[rect(0, 0, 10, 10)]);

    let f2 = ring.acquire().expect("f2");
    ring.present(f2.slot, &[rect(20, 20, 5, 5)]);
    ring.release(f1.slot);

    let f3 = ring.acquire().expect("f1 comes back");
    assert_eq!(f3.slot, f1.slot);
    assert_eq!(ring.age(f3.slot), 1);
    assert_eq!(
        f3.repaint,
        Repaint::Region(vec![rect(20, 20, 5, 5)]),
        "frame 2's damage is exactly what this slot missed"
    );
}

/// Triple buffering: a slot two frames stale repaints both, oldest first.
#[test]
fn a_slot_two_frames_stale_repaints_both_in_order() {
    let mut ring = BufferRing::new(3, BufferRing::DEFAULT_MAX_DAMAGE_RECTS);
    let f1 = ring.acquire().expect("f1");
    ring.present(f1.slot, &[rect(1, 1, 1, 1)]);
    let f2 = ring.acquire().expect("f2");
    ring.present(f2.slot, &[rect(2, 2, 2, 2)]);
    let f3 = ring.acquire().expect("f3");
    ring.present(f3.slot, &[rect(3, 3, 3, 3)]);
    ring.release(f1.slot);

    let f4 = ring.acquire().expect("f1 comes back");
    assert_eq!(f4.slot, f1.slot);
    assert_eq!(ring.age(f4.slot), 2);
    assert_eq!(
        f4.repaint,
        Repaint::Region(vec![rect(2, 2, 2, 2), rect(3, 3, 3, 3)]),
        "in present order, so a producer replaying them lands on the newest last"
    );
}

/// A slot that falls out of the retained damage history gets a full repaint
/// rather than an under-painted partial one.
#[test]
fn a_slot_older_than_the_damage_history_repaints_everything() {
    let mut ring = BufferRing::new(3, BufferRing::DEFAULT_MAX_DAMAGE_RECTS);
    let s0 = ring.acquire().expect("s0");
    ring.present(s0.slot, &[rect(0, 0, 1, 1)]); // seq 1
    let s1 = ring.acquire().expect("s1");
    ring.present(s1.slot, &[rect(0, 0, 2, 2)]); // seq 2
    let s2 = ring.acquire().expect("s2");
    ring.present(s2.slot, &[rect(0, 0, 3, 3)]); // seq 3, history {1,2,3}

    ring.release(s0.slot);
    ring.release(s1.slot);

    // acquire prefers the freshest free slot, so s0 keeps losing and falls
    // further behind.
    let r4 = ring.acquire().expect("r4");
    assert_eq!(r4.slot, s1.slot, "seq 2 is fresher than seq 1");
    ring.present(r4.slot, &[rect(0, 0, 4, 4)]); // seq 4, history {2,3,4}, base 1
    ring.release(s2.slot);
    let r5 = ring.acquire().expect("r5");
    assert_eq!(r5.slot, s2.slot, "seq 3 is fresher than seq 1");
    ring.present(r5.slot, &[rect(0, 0, 5, 5)]); // seq 5, history {3,4,5}, base 2

    // s0 is now the only free slot, and its last present (1) is below base (2).
    let r6 = ring.acquire().expect("r6");
    assert_eq!(r6.slot, s0.slot);
    assert_eq!(
        r6.repaint,
        Repaint::Full,
        "its damage is gone, so partial would under-paint"
    );
}

/// A union past the rect cap collapses to a full repaint.
#[test]
fn a_union_past_the_rect_cap_collapses_to_full() {
    let mut ring = BufferRing::new(2, 2);
    let a = ring.acquire().expect("a");
    ring.present(a.slot, &[]); // seq 1

    let b = ring.acquire().expect("b");
    // Damage *after* a's present is what a must repaint on reuse: three
    // rectangles against a cap of two.
    ring.present(
        b.slot,
        &[rect(0, 0, 1, 1), rect(1, 1, 1, 1), rect(2, 2, 1, 1)],
    );
    ring.release(a.slot);

    let c = ring.acquire().expect("a comes back");
    assert_eq!(c.slot, a.slot);
    assert_eq!(
        c.repaint,
        Repaint::Full,
        "scattered rectangles cost more than the bandwidth they save"
    );
}

/// A ring asked for no slots is given one, since a ring that can never lease
/// is not a request anyone means to make.
#[test]
fn a_zero_slot_ring_is_clamped_to_one() {
    let mut ring = BufferRing::new(0, 4);
    assert_eq!(ring.max_slots(), 1);
    assert!(ring.acquire().is_some());
    assert!(ring.acquire().is_none(), "the one slot is in flight");
}

/// Releasing the scanning slot is ignored: what is on screen cannot be freed.
#[test]
fn releasing_the_scanning_slot_does_nothing() {
    let mut ring = BufferRing::new(2, 4);
    let a = ring.acquire().expect("a");
    ring.present(a.slot, &[]);

    ring.release(a.slot);
    let b = ring.acquire().expect("b");
    assert_ne!(b.slot, a.slot, "still scanning, so still not available");
}

// --- negotiate: parity port of tests/unit/test_negotiate.cpp -----------------

mod negotiate {
    use crate::negotiate;
    use drmkit_fmt::{Modifier, Rotation};

    /// The three layouts the reference's cases use, in the same roles.
    const LINEAR: Modifier = Modifier::LINEAR;
    /// An ARM AFBC layout — the compression class.
    const AFBC: Modifier = Modifier(0x0800_0000_0000_0001);
    /// A Broadcom VC4 T-tiled layout — the tiling class.
    const TILED: Modifier = Modifier(0x0700_0000_0000_0001);

    /// The three constants above really are one of each bandwidth class.
    ///
    /// Without this the ranking case below passes just as happily on three
    /// modifiers that all classify the same way, which would make it a test of
    /// nothing.
    #[test]
    fn the_fixtures_cover_one_of_each_class() {
        use drmkit_fmt::{BandwidthClass, classify};
        assert_eq!(classify(LINEAR), BandwidthClass::Linear);
        assert_eq!(classify(AFBC), BandwidthClass::Compression);
        assert_eq!(classify(TILED), BandwidthClass::Tiling);
    }

    /// A producer listing linear first still gets compression ranked first.
    #[test]
    fn the_intersection_is_ranked_compression_then_tiling_then_linear() {
        let producer = [LINEAR, TILED, AFBC];
        let plane = [AFBC, LINEAR, TILED];
        assert_eq!(
            negotiate(&producer, &plane, Rotation::Rotate0),
            vec![AFBC, TILED, LINEAR],
            "bandwidth order, not the order either side happened to list"
        );
    }

    /// Only modifiers in both sets survive.
    #[test]
    fn a_modifier_only_one_side_has_is_dropped() {
        assert_eq!(
            negotiate(&[AFBC, LINEAR], &[LINEAR, TILED], Rotation::Rotate0),
            vec![LINEAR]
        );
    }

    /// No overlap, and either side empty, are all well-defined.
    #[test]
    fn no_overlap_yields_nothing() {
        assert!(negotiate(&[AFBC], &[TILED, LINEAR], Rotation::Rotate0).is_empty());
        assert!(negotiate(&[], &[TILED, LINEAR], Rotation::Rotate0).is_empty());
        assert!(negotiate(&[AFBC], &[], Rotation::Rotate0).is_empty());
    }

    /// A producer listing a modifier twice gets it once.
    #[test]
    fn duplicates_in_the_producer_collapse() {
        assert_eq!(
            negotiate(&[AFBC, AFBC], &[AFBC], Rotation::Rotate0).len(),
            1
        );
    }

    /// A 90 or 270 rotation transposes the fetch order, which a linear buffer
    /// cannot provide — so linear drops out and the tiled layouts stay.
    #[test]
    fn a_transposing_rotation_drops_linear() {
        let both = [LINEAR, AFBC, TILED];
        assert_eq!(negotiate(&both, &both, Rotation::Rotate0).len(), 3);

        for rotation in [Rotation::Rotate90, Rotation::Rotate270] {
            let got = negotiate(&both, &both, rotation);
            assert_eq!(got.len(), 2, "{rotation:?}");
            assert!(!got.contains(&LINEAR), "{rotation:?} cannot fetch linear");
        }
    }

    /// 180 does not transpose, so nothing is filtered.
    ///
    /// The reference's cases do not cover this, and it is the half of
    /// `transposes()` that would silently over-filter if it were wrong.
    #[test]
    fn a_half_turn_filters_nothing() {
        let both = [LINEAR, AFBC, TILED];
        assert_eq!(negotiate(&both, &both, Rotation::Rotate180).len(), 3);
    }
}

// --- frame economy: parity port of tests/unit/test_frame_economy.cpp --------

mod frame_economy {
    use crate::{FrameAction, FrameEconomy};

    /// The first frame commits full whatever it is told, because the scanout
    /// buffer's contents are undefined until something has been put in it.
    #[test]
    fn the_first_frame_commits_full_regardless() {
        let mut economy = FrameEconomy::new();
        assert_eq!(
            economy.decide(false, true),
            FrameAction::CommitFull,
            "not skipped, despite nothing having changed"
        );
        assert_eq!(economy.committed(), 1);
        assert_eq!(economy.skipped(), 0);
    }

    /// An unchanged frame after the first is skipped: no commit at all.
    #[test]
    fn an_idle_frame_costs_nothing() {
        let mut economy = FrameEconomy::new();
        economy.decide(true, false);

        for _ in 0..5 {
            assert_eq!(economy.decide(false, true), FrameAction::Skip);
        }
        assert_eq!(economy.committed(), 1);
        assert_eq!(economy.skipped(), 5);
    }

    /// Changed content commits: damaged when the producer supplied damage,
    /// full when it did not.
    #[test]
    fn changed_content_commits_damaged_only_when_damage_exists() {
        let mut economy = FrameEconomy::new();
        economy.decide(true, false);

        assert_eq!(economy.decide(true, true), FrameAction::CommitDamaged);
        assert_eq!(
            economy.decide(true, false),
            FrameAction::CommitFull,
            "no damage to attach means the whole frame"
        );
        assert_eq!(economy.committed(), 3);
        assert_eq!(economy.skipped(), 0);
    }

    /// `force_full` overrides available damage, and is one-shot.
    #[test]
    fn force_full_wins_once_over_available_damage() {
        let mut economy = FrameEconomy::new();
        economy.decide(true, false);

        economy.force_full();
        assert_eq!(economy.decide(true, true), FrameAction::CommitFull);
        assert_eq!(
            economy.decide(true, true),
            FrameAction::CommitDamaged,
            "the force applied to one frame, not to the rest"
        );
    }

    /// `force_full` also turns an idle frame into a commit — the post-resume
    /// case, where nothing changed but what is on screen is no longer ours.
    #[test]
    fn force_full_overrides_an_idle_frame() {
        let mut economy = FrameEconomy::new();
        economy.decide(true, false);

        economy.force_full();
        assert_eq!(economy.decide(false, true), FrameAction::CommitFull);
    }

    /// Forcing full before the first frame consumes one flag, not two.
    ///
    /// The reference has no equivalent: `first_` and `force_full_` are cleared
    /// by the same branch, so a caller who forces before the first decision
    /// would lose the force if the branches were ever separated.
    #[test]
    fn forcing_before_the_first_frame_does_not_swallow_a_later_one() {
        let mut economy = FrameEconomy::new();
        economy.force_full();
        assert_eq!(economy.decide(true, true), FrameAction::CommitFull);
        assert_eq!(
            economy.decide(true, true),
            FrameAction::CommitDamaged,
            "the first frame and the force were the same commit, not two"
        );
    }

    /// `commits` says what it looks like it says.
    #[test]
    fn only_skip_is_not_a_commit() {
        assert!(!FrameAction::Skip.commits());
        assert!(FrameAction::CommitFull.commits());
        assert!(FrameAction::CommitDamaged.commits());
    }
}
