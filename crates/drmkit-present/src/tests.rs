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
