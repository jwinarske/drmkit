// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! The blend arithmetic, diffed against drm-cxx's own output.
//!
//! Every expectation here was produced by running `CompositeCanvas::blend_into`
//! from the reference over the same inputs and recording what came back — not
//! derived by hand. Hand-derived expectations are worth very little for this
//! kind of code: the rounding, the premultiplied convention and the
//! nearest-neighbour mapping all have several plausible readings, and a wrong
//! reading tends to produce a wrong expectation and a matching wrong
//! implementation.
//!
//! Regenerate with `parity/blend-oracle.cc` against a built drm-cxx.

use drmkit_fmt::fourcc;
use drmkit_scene::{CompositeRect, CompositeSrc, blend_into};

const ARGB: u32 = fourcc::ARGB8888;
const XRGB: u32 = fourcc::XRGB8888;

fn solid(w: u32, h: u32, value: u32) -> Vec<u8> {
    value.to_ne_bytes().repeat((w * h) as usize)
}

fn read(buf: &[u8], stride: u32, w: u32, h: u32) -> Vec<u32> {
    let mut out = Vec::new();
    for y in 0..h {
        for x in 0..w {
            let at = (y * stride + x * 4) as usize;
            out.push(u32::from_ne_bytes([
                buf[at],
                buf[at + 1],
                buf[at + 2],
                buf[at + 3],
            ]));
        }
    }
    out
}

/// One input and the answer drm-cxx gives for it.
struct Case {
    name: &'static str,
    dst: (u32, u32, u32),
    src: (u32, u32, u32),
    drm_fourcc: u32,
    plane_alpha: u16,
    src_rect: CompositeRect,
    dst_rect: CompositeRect,
    want: &'static [u32],
}

const fn r(x: i32, y: i32, w: u32, h: u32) -> CompositeRect {
    CompositeRect { x, y, w, h }
}

/// The table, transcribed from `parity/blend-oracle.cc`'s output.
const CASES: &[Case] = &[
    Case {
        name: "opaque_over_opaque",
        dst: (2, 1, 0xFF00_00FF),
        src: (2, 1, 0xFFFF_0000),
        drm_fourcc: ARGB,
        plane_alpha: 0xFFFF,
        src_rect: r(0, 0, 2, 1),
        dst_rect: r(0, 0, 2, 1),
        want: &[0xFFFF_0000, 0xFFFF_0000],
    },
    Case {
        name: "transparent_over_opaque",
        dst: (2, 1, 0xFF00_00FF),
        src: (2, 1, 0x0000_0000),
        drm_fourcc: ARGB,
        plane_alpha: 0xFFFF,
        src_rect: r(0, 0, 2, 1),
        dst_rect: r(0, 0, 2, 1),
        want: &[0xFF00_00FF, 0xFF00_00FF],
    },
    Case {
        name: "half_alpha_over_opaque",
        dst: (2, 1, 0xFF00_00FF),
        src: (2, 1, 0x8080_0000),
        drm_fourcc: ARGB,
        plane_alpha: 0xFFFF,
        src_rect: r(0, 0, 2, 1),
        dst_rect: r(0, 0, 2, 1),
        want: &[0xFF80_007F, 0xFF80_007F],
    },
    Case {
        name: "alpha_33_over_opaque",
        dst: (2, 1, 0xFF00_00FF),
        src: (2, 1, 0x3333_0000),
        drm_fourcc: ARGB,
        plane_alpha: 0xFFFF,
        src_rect: r(0, 0, 2, 1),
        dst_rect: r(0, 0, 2, 1),
        want: &[0xFF33_00CC, 0xFF33_00CC],
    },
    Case {
        name: "alpha_cc_over_grey",
        dst: (2, 1, 0xFF80_8080),
        src: (2, 1, 0xCC00_CC00),
        drm_fourcc: ARGB,
        plane_alpha: 0xFFFF,
        src_rect: r(0, 0, 2, 1),
        dst_rect: r(0, 0, 2, 1),
        want: &[0xFF1A_E61A, 0xFF1A_E61A],
    },
    Case {
        name: "xrgb_padding_ignored",
        dst: (2, 1, 0xFF00_00FF),
        src: (2, 1, 0x00FF_0000),
        drm_fourcc: XRGB,
        plane_alpha: 0xFFFF,
        src_rect: r(0, 0, 2, 1),
        dst_rect: r(0, 0, 2, 1),
        want: &[0xFFFF_0000, 0xFFFF_0000],
    },
    Case {
        name: "plane_alpha_8000",
        dst: (2, 1, 0x0000_0000),
        src: (2, 1, 0xFFFF_FFFF),
        drm_fourcc: ARGB,
        plane_alpha: 0x8000,
        src_rect: r(0, 0, 2, 1),
        dst_rect: r(0, 0, 2, 1),
        want: &[0x8080_8080, 0x8080_8080],
    },
    Case {
        name: "plane_alpha_ffff",
        dst: (2, 1, 0x0000_0000),
        src: (2, 1, 0xFFAB_CDEF),
        drm_fourcc: ARGB,
        plane_alpha: 0xFFFF,
        src_rect: r(0, 0, 2, 1),
        dst_rect: r(0, 0, 2, 1),
        want: &[0xFFAB_CDEF, 0xFFAB_CDEF],
    },
    Case {
        name: "plane_alpha_4000_on_half",
        dst: (2, 1, 0x0000_0000),
        src: (2, 1, 0x8040_2010),
        drm_fourcc: ARGB,
        plane_alpha: 0x4000,
        src_rect: r(0, 0, 2, 1),
        dst_rect: r(0, 0, 2, 1),
        want: &[0x2010_0804, 0x2010_0804],
    },
    Case {
        name: "negative_dst_x",
        dst: (4, 1, 0x0000_0000),
        src: (4, 1, 0xFFFF_0000),
        drm_fourcc: ARGB,
        plane_alpha: 0xFFFF,
        src_rect: r(0, 0, 4, 1),
        dst_rect: r(-2, 0, 4, 1),
        want: &[0xFFFF_0000, 0xFFFF_0000, 0, 0],
    },
    Case {
        name: "offscreen_dst",
        dst: (2, 1, 0xFF00_00FF),
        src: (2, 1, 0xFFFF_0000),
        drm_fourcc: ARGB,
        plane_alpha: 0xFFFF,
        src_rect: r(0, 0, 2, 1),
        dst_rect: r(100, 0, 2, 1),
        want: &[0xFF00_00FF, 0xFF00_00FF],
    },
    Case {
        name: "scale_up_2_to_4",
        dst: (4, 1, 0x0000_0000),
        src: (2, 1, 0xFF11_2233),
        drm_fourcc: ARGB,
        plane_alpha: 0xFFFF,
        src_rect: r(0, 0, 2, 1),
        dst_rect: r(0, 0, 4, 1),
        want: &[0xFF11_2233, 0xFF11_2233, 0xFF11_2233, 0xFF11_2233],
    },
    Case {
        name: "scale_down_4_to_2",
        dst: (2, 1, 0x0000_0000),
        src: (4, 1, 0xFF44_5566),
        drm_fourcc: ARGB,
        plane_alpha: 0xFFFF,
        src_rect: r(0, 0, 4, 1),
        dst_rect: r(0, 0, 2, 1),
        want: &[0xFF44_5566, 0xFF44_5566],
    },
    Case {
        name: "src_rect_offset",
        dst: (2, 1, 0x0000_0000),
        src: (4, 1, 0xFF77_8899),
        drm_fourcc: ARGB,
        plane_alpha: 0xFFFF,
        src_rect: r(2, 0, 2, 1),
        dst_rect: r(0, 0, 2, 1),
        want: &[0xFF77_8899, 0xFF77_8899],
    },
];

/// Every case, byte for byte against the reference.
#[test]
fn the_blend_matches_drm_cxx() {
    let mut wrong = Vec::new();
    for case in CASES {
        let (dw, dh, dst_fill) = case.dst;
        let (sw, sh, src_fill) = case.src;
        let mut dst = solid(dw, dh, dst_fill);
        let pixels = solid(sw, sh, src_fill);
        let source = CompositeSrc {
            pixels: &pixels,
            src_stride_bytes: sw * 4,
            src_width: sw,
            src_height: sh,
            drm_fourcc: case.drm_fourcc,
            plane_alpha: case.plane_alpha,
        };
        blend_into(
            &mut dst,
            dw * 4,
            dw,
            dh,
            &source,
            case.src_rect,
            case.dst_rect,
        );

        let got = read(&dst, dw * 4, dw, dh);
        if got != case.want {
            wrong.push(format!(
                "{}: got {:08X?}, drm-cxx gives {:08X?}",
                case.name, got, case.want
            ));
        }
    }
    assert!(
        wrong.is_empty(),
        "blend diverges from the reference:\n{}",
        wrong.join("\n")
    );
}
