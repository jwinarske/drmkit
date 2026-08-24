// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! Fuzz the EDID parser (plan §9 T2, carried from phase 1).
//!
//! EDID arrives from the display over an I2C line nobody controls, and is
//! malformed in the field constantly — truncated reads, bad checksums,
//! extension counts that disagree with what follows, descriptor blocks that run
//! off the end. A parser that trusts any of it reads out of bounds on a cable
//! someone plugged in.
//!
//! `parse_edid` is total by contract: every input either yields a
//! `ConnectorInfo` or an error, and never panics. libdisplay-info does the
//! hard part, so what this exercises is the translation layer — the string
//! handling, the optional gating, the centimetre-to-millimetre arithmetic on
//! values a hostile blob chooses.
//!
//! This was the phase-1 gate item that could not be seeded before the code it
//! targets existed.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Ok(info) = drmkit_display::edid::parse_edid(data) else {
        // A refusal is a fine outcome and the common one; the contract is that
        // it is a refusal rather than a crash.
        return;
    };

    // Anything that parsed must be internally coherent.
    //
    // The name is built from make and model, so it cannot carry text neither
    // of them has — a fuzzer that made it do so would have found the string
    // handling reading past a field.
    if !info.make.is_empty() {
        assert!(
            info.name.contains(&info.make),
            "name {:?} omits make {:?}",
            info.name,
            info.make
        );
    }
    if !info.model.is_empty() {
        assert!(
            info.name.contains(&info.model),
            "name {:?} omits model {:?}",
            info.name,
            info.model
        );
    }

    // Physical size is centimetres scaled by ten. A negative or absurd figure
    // means the conversion overflowed or a signed field was read as unsigned.
    assert!(
        info.width_mm >= 0 && info.height_mm >= 0,
        "negative physical size: {} x {}",
        info.width_mm,
        info.height_mm
    );

    // An HDR block is only reported when the display advertises an HDR
    // transfer function. Traditional SDR gamma alone is not one, and a blob
    // that produced `Some` with every flag clear would mean the gate had come
    // undone.
    if let Some(hdr) = info.hdr {
        assert!(
            hdr.type1 || hdr.traditional_hdr || hdr.pq || hdr.hlg,
            "HDR metadata reported with no HDR transfer function: {hdr:?}"
        );
    }

    // Same for extended colorimetry.
    if let Some(gamut) = info.wide_gamut {
        assert!(
            gamut.bt2020_cycc
                || gamut.bt2020_ycc
                || gamut.bt2020_rgb
                || gamut.st2113_rgb
                || gamut.ictcp,
            "wide gamut reported with no encoding: {gamut:?}"
        );
    }
});
