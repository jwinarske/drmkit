// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! Cursor name aliases.
//!
//! Equivalence classes from freedesktop's cursor spec Appendix B, libxcursor's
//! built-in aliases, and the hashed names some themes still ship internally.
//! Asking for any member of a class makes every other member a candidate, so a
//! caller can ask for the modern name whatever the theme happens to ship.
//!
//! Classes overlap on purpose: `col-resize` and `ew-resize` both contain
//! `sb_h_double_arrow`, so both names reach the same file on a theme that
//! ships only one of them.

/// The alias classes, in the order they are searched.
///
/// Order within a class is load-bearing: a lookup tries the members in the
/// order written here, *not* starting from the name that was asked for. So a
/// theme that ships both `pointer` and `hand2` answers a request for either
/// with `pointer`. That is what the reference does, and changing it would
/// silently change which file a theme resolves to.
const CLASSES: &[&[&str]] = &[
    &["default", "left_ptr", "top_left_arrow", "arrow"],
    &[
        "pointer",
        "hand",
        "hand1",
        "hand2",
        "pointing_hand",
        "9d800788f1b08800ae810202380a0822",
        "e29285e634086352946a0e7090d73106",
    ],
    &["text", "xterm", "ibeam"],
    &["crosshair", "cross", "tcross"],
    &["wait", "watch"],
    &[
        "progress",
        "left_ptr_watch",
        "half-busy",
        "3ecb610c1bf2410f44200f48c40d3599",
        "08e8e1c95fe2fc01f976f1e063a24ccd",
    ],
    &[
        "not-allowed",
        "crossed_circle",
        "forbidden",
        "03b6e0fcb3499374a867c041f52298f0",
    ],
    &[
        "grab",
        "openhand",
        "9141b49c8149039304290b508d208c40",
        "f2d0232ec7c4c06e79e2a2a5ebcb6a43",
    ],
    &[
        "grabbing",
        "closedhand",
        "208530c400c041818281048008011002",
        "fcf21c00b30f7e3f83fe0dfd12e71cff",
    ],
    &[
        "help",
        "question_arrow",
        "whats_this",
        "left_ptr_help",
        "d9ce0ab605698f320427677b458ad60b",
    ],
    &["move", "fleur", "all-scroll"],
    &[
        "col-resize",
        "ew-resize",
        "sb_h_double_arrow",
        "h_double_arrow",
        "14fef782d02440884392942c11205230",
        "028006030e0e7ebffc7f7070c0600140",
    ],
    &[
        "row-resize",
        "ns-resize",
        "sb_v_double_arrow",
        "v_double_arrow",
        "2870a09082c103050810ffdffffe0204",
        "00008160000006810000408080010102",
    ],
    &["n-resize", "top_side"],
    &["s-resize", "bottom_side"],
    &["e-resize", "right_side"],
    &["w-resize", "left_side"],
    &["ne-resize", "top_right_corner", "ur_angle"],
    &["nw-resize", "top_left_corner", "ul_angle"],
    &["se-resize", "bottom_right_corner", "lr_angle"],
    &["sw-resize", "bottom_left_corner", "ll_angle"],
    &["nesw-resize", "fd_double_arrow"],
    &["nwse-resize", "bd_double_arrow"],
    &["zoom-in", "magnifier"],
    &["zoom-out"],
    &["copy", "dnd-copy", "1081e37283d90000800003c07f3ef6bf"],
    &[
        "alias",
        "link",
        "dnd-link",
        "0876e1c15ff2fc01f5c56f42a6e9db1c",
    ],
    &["no-drop", "circle", "dnd-none"],
    &["cell", "plus"],
    &["vertical-text"],
    &["context-menu"],
];

/// The class containing `name`, if any.
fn class_of(name: &str) -> Option<&'static [&'static str]> {
    CLASSES.iter().copied().find(|class| class.contains(&name))
}

/// Visit every name worth trying for `name`, in search order.
///
/// A name in no class is visited once, as itself, so a caller never has to
/// special-case the unknown-name path.
pub(crate) fn for_each<F: FnMut(&str)>(name: &str, mut visit: F) {
    match class_of(name) {
        Some(class) => {
            for alias in class {
                visit(alias);
            }
        }
        None => visit(name),
    }
}
