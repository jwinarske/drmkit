// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! Fuzz the `IN_FORMATS` blob parser (plan §9 T2).
//!
//! This parser reads a blob the kernel hands over, whose offsets and counts are
//! attacker-influenced in the sense that matters here: a buggy or hostile
//! driver can present anything. The C++ parser had a real misaligned-read bug
//! in exactly this code, fixed by copying each record out rather than casting
//! into the blob.
//!
//! `from_blob` is total by contract -- every malformed input yields an empty or
//! partial table, never a panic and never a read past the end. That is the
//! property under test.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let table = drmkit_fmt::FormatTable::from_blob(data);

    // Whatever came back must be internally consistent: sorted, de-duplicated,
    // and queryable without panicking.
    let pairs = table.all();
    for window in pairs.windows(2) {
        assert!(window[0] < window[1], "table must be sorted and deduplicated");
    }

    for pair in pairs {
        assert!(
            table.supports(pair.fourcc, pair.modifier),
            "every listed pair must be reported as supported"
        );
        // Exercise the grouped index against the flat list.
        assert!(table.modifiers_for(pair.fourcc).any(|m| m == pair.modifier));
    }
});
