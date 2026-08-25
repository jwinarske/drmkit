// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! Fuzz the `.xcursor` loader (plan §9 T2).
//!
//! A cursor file is untrusted input: it comes from a user theme directory, or
//! from wherever `XCURSOR_PATH` points, and every number in it -- the
//! table-of-contents count, each chunk's offset, each image's dimensions -- is
//! chosen by whoever wrote the file.
//!
//! This is not a hypothetical class of bug. The `xcursor` crate at 0.3.11
//! sizes its pixel buffer from the declared dimensions *before* reading, so a
//! 60-byte file claiming a 32767x32767 image asks for 4.3 GB and the process
//! aborts -- Rust does not unwind on allocation failure, so no caller can
//! catch it and no check placed after the parse can help. drmkit reads from a
//! slice already in memory and checks every length against what remains before
//! allocating, which is what this target exists to keep true.
//!
//! `Cursor::from_bytes` is total by contract: every input yields a cursor or
//! an error, and never panics, aborts, or allocates unboundedly.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // A size of zero exercises the default-size substitution; the fuzzer
    // reaches the rest of the size-selection logic through the file's own
    // nominal sizes.
    let Ok(cursor) = drmkit_cursor::Cursor::from_bytes(data, 0) else {
        // A refusal is the common outcome and a fine one. The contract is that
        // it is a refusal rather than a crash.
        return;
    };

    // Anything that loaded has to be internally coherent.
    let frames = cursor.frames();
    assert!(!frames.is_empty(), "a loaded cursor has at least one frame");
    assert!(
        frames.len() <= 256,
        "frame count past the limit: {}",
        frames.len()
    );

    let mut total = std::time::Duration::ZERO;
    for frame in frames {
        assert!(frame.width > 0 && frame.height > 0, "a zero-area frame");
        assert!(
            frame.width <= 512 && frame.height <= 512,
            "frame past the dimension limit: {}x{}",
            frame.width,
            frame.height
        );
        // The pixel buffer has to match the dimensions it claims, or a
        // consumer indexing by row and column reads somewhere else.
        assert_eq!(
            frame.pixels.len(),
            frame.width as usize * frame.height as usize,
            "pixel count disagrees with {}x{}",
            frame.width,
            frame.height
        );
        // The hotspot is a pixel inside the frame.
        assert!(
            frame.xhot <= frame.width && frame.yhot <= frame.height,
            "hotspot ({}, {}) outside a {}x{} frame",
            frame.xhot,
            frame.yhot,
            frame.width,
            frame.height
        );
        total += frame.delay;
    }

    assert_eq!(total, cursor.cycle(), "the cycle is the sum of the delays");

    // `frame_at` is total: every instant names a frame, including instants far
    // past the cycle and the boundaries themselves.
    for ms in [0u64, 1, 999, u64::from(u32::MAX)] {
        let at = cursor.frame_at(std::time::Duration::from_millis(ms));
        assert!(
            frames.iter().any(|frame| std::ptr::eq(frame, at)),
            "frame_at returned something not in the cursor"
        );
    }
});
