// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

use std::sync::Mutex;

use std::os::fd::AsRawFd as _;

use drmkit_core::Device;
use drmkit_fmt::fourcc;

use super::{GbmBuffer, GbmDevice, GbmError};

/// `DRM_FORMAT_MOD_INVALID`: the driver not saying which layout it chose.
const INVALID_MODIFIER: u64 = (1 << 56) - 1;

/// DRM master is per open file description, so card-dependent cases serialize.
static CARD_LOCK: Mutex<()> = Mutex::new(());

fn card_guard() -> std::sync::MutexGuard<'static, ()> {
    CARD_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Open the card, or `None` if this machine has no DRM device.
///
/// Absence is a skip locally and a failure in the lane — the line
/// `DRMKIT_REQUIRE_MASTER` already draws. Everything past it asserts.
fn open_card() -> Option<Device> {
    let path = std::env::var("DRMKIT_TEST_CARD").unwrap_or_else(|_| "/dev/dri/card0".to_owned());
    match Device::open(&path) {
        Ok(device) => Some(device),
        Err(error) => {
            assert!(
                std::env::var_os("DRMKIT_REQUIRE_MASTER").is_none(),
                "{path}: {error}, but DRMKIT_REQUIRE_MASTER is set"
            );
            println!("note: skipped -- no DRM device at {path} ({error})");
            None
        }
    }
}

/// Any DRM node opens as a GBM allocator.
///
/// Including a display-only one: the `drm` backend falls back to dumb
/// allocation where there is no render engine, which is what makes the rest of
/// these runnable against vkms rather than needing a GPU.
#[test]
fn a_drm_node_opens_as_a_gbm_device() {
    let _guard = card_guard();
    let Some(device) = open_card() else { return };
    let gbm = GbmDevice::new(&device).expect("open a GBM device");
    println!("note: gbm backend is {}", gbm.backend_name());
}

/// An allocated buffer reports the geometry that was asked for.
#[test]
fn a_buffer_reports_its_geometry() {
    let _guard = card_guard();
    let Some(device) = open_card() else { return };
    let gbm = GbmDevice::new(&device).expect("gbm device");
    let buffer = GbmBuffer::create(&gbm, 64, 32, fourcc::ARGB8888).expect("allocate");

    assert_eq!(buffer.width(), 64);
    assert_eq!(buffer.height(), 32);
    assert_eq!(buffer.fourcc(), fourcc::ARGB8888);
    assert!(
        buffer.stride() >= 64 * 4,
        "a stride below the row's own width cannot be right; got {}",
        buffer.stride()
    );
    assert!(buffer.plane_count() >= 1);
}

/// The buffer exports a dma-buf the caller owns.
///
/// This is the whole point of allocating through GBM rather than as a dumb
/// buffer: the descriptor is what carries the buffer to a scene, another
/// process, or the external-source import path.
#[test]
fn a_buffer_exports_a_dma_buf() {
    let _guard = card_guard();
    let Some(device) = open_card() else { return };
    let gbm = GbmDevice::new(&device).expect("gbm device");
    let buffer = GbmBuffer::create(&gbm, 64, 64, fourcc::ARGB8888).expect("allocate");

    let first = buffer.export().expect("export a dma-buf");
    let second = buffer.export().expect("export again");

    assert!(first.as_raw_fd() >= 0);
    assert_ne!(
        first.as_raw_fd(),
        second.as_raw_fd(),
        "each export must hand back its own descriptor; sharing one would mean \
         closing either invalidates the other"
    );
}

/// A buffer keeps its device's descriptor open for as long as it lives.
///
/// A GEM handle belongs to the descriptor it was allocated on, and freeing the
/// buffer closes the handle through it. Nothing ties a buffer's lifetime to the
/// `GbmDevice` it came from, so if the device took the descriptor with it, every
/// buffer still alive would free through a closed number -- `EBADF` and a leak,
/// or, once the number is reused, a handle closed on some other file. Mesa
/// keeps a descriptor of its own and passes regardless; the SA8155P's libgbm
/// frees and exports through the caller's, and failed this case until each
/// buffer held a share of it.
#[test]
fn a_buffer_outlives_its_device() {
    let _guard = card_guard();
    let Some(device) = open_card() else { return };
    let gbm = GbmDevice::new(&device).expect("gbm device");
    let buffer = GbmBuffer::create(&gbm, 64, 64, fourcc::ARGB8888).expect("allocate");
    drop(gbm);

    buffer
        .export()
        .expect("the buffer's descriptor must outlive the GbmDevice it came from");
}

/// The modifier travels with the buffer, consistently.
///
/// A tiled or compressed buffer scanned out as though it were linear is not
/// slightly wrong, it is unreadable — so whatever the driver chose has to be
/// reportable, including the `INVALID` sentinel a driver uses when it is not
/// saying.
///
/// **What this cannot prove on vkms.** Its buffers really are linear, so a
/// `modifier()` that ignored the driver and returned zero would pass every
/// assertion here — verified by injecting exactly that. Catching it needs a
/// driver with more than one layout, and is
/// [`every_honored_layout_comes_back_as_asked`]'s job (P-13).
#[test]
fn a_buffer_reports_a_modifier() {
    let _guard = card_guard();
    let Some(device) = open_card() else { return };
    let gbm = GbmDevice::new(&device).expect("gbm device");
    let buffer = GbmBuffer::create(&gbm, 64, 64, fourcc::ARGB8888).expect("allocate");

    // What the value *is* cannot be asserted: vkms has no render engine so its
    // buffers come back linear, a GPU may pick a tiled layout, and a driver
    // that is not saying reports the INVALID sentinel. All three are
    // legitimate, and a test that accepted all three would assert nothing.
    //
    // What can be asserted is that it is the driver's answer and a stable one:
    // two identical requests to the same device must describe the same layout,
    // or a caller could not pass a modifier alongside a buffer at all.
    let modifier = buffer.modifier();
    println!("note: modifier is {modifier:#x}");

    let again = GbmBuffer::create(&gbm, 64, 64, fourcc::ARGB8888).expect("allocate again");
    assert_eq!(
        again.modifier(),
        modifier,
        "the same request to the same device must describe the same layout"
    );
    assert_eq!(again.stride(), buffer.stride(), "and the same stride");
}

/// An unsupported format is refused at allocation, not at commit.
///
/// A format the driver cannot allocate should fail here, where the caller can
/// pick another, rather than at the atomic commit where it takes the whole
/// frame down.
#[test]
fn an_unknown_format_is_refused() {
    let _guard = card_guard();
    let Some(device) = open_card() else { return };
    let gbm = GbmDevice::new(&device).expect("gbm device");

    let refused = GbmBuffer::create(&gbm, 64, 64, 0xDEAD_BEEF);

    assert!(
        matches!(refused, Err(GbmError::Allocation(_))),
        "a format no driver knows must be refused at allocation"
    );
}

/// A constrained allocation comes back in a modifier from the list.
///
/// vkms allocates linear, and `LINEAR` is what the list asks for, so this
/// confirms the constrained path reaches the driver and returns something
/// usable rather than that the constraint was honored against an alternative
/// -- vkms has no second layout to pick. That is
/// [`every_honored_layout_comes_back_as_asked`]'s job (P-28).
#[test]
fn a_constrained_allocation_comes_back_in_a_listed_modifier() {
    const LINEAR: u64 = 0;

    let _guard = card_guard();
    let Some(device) = open_card() else { return };
    let gbm = GbmDevice::new(&device).expect("gbm device");
    let buffer = GbmBuffer::create_with_modifiers(&gbm, 64, 64, fourcc::ARGB8888, &[LINEAR])
        .expect("allocate LINEAR");

    // INVALID is the driver not saying, which is not the same as reporting
    // another layout: Mesa's fallback on a device it could not load a driver
    // for (the CI lane's vkms) reports it for every buffer, constrained or not.
    assert!(
        matches!(buffer.modifier(), LINEAR | INVALID_MODIFIER),
        "the driver was offered LINEAR and reported {:#x}",
        buffer.modifier()
    );
    assert_eq!(buffer.width(), 64);
    assert_eq!(buffer.height(), 64);
    assert!(buffer.stride() >= 64 * 4);
}

/// An empty list means "no constraint", not "no modifier is acceptable".
///
/// The distinction matters at the call site: `ScanoutBackend` reaches the
/// empty case whenever no plane exposes `IN_FORMATS`, and refusing there would
/// turn a driver that simply does not advertise layouts into one that cannot
/// allocate at all.
#[test]
fn an_empty_modifier_list_allocates_rather_than_refusing() {
    let _guard = card_guard();
    let Some(device) = open_card() else { return };
    let gbm = GbmDevice::new(&device).expect("gbm device");

    let constrained = GbmBuffer::create_with_modifiers(&gbm, 64, 64, fourcc::ARGB8888, &[])
        .expect("an empty list is no constraint");
    let plain = GbmBuffer::create(&gbm, 64, 64, fourcc::ARGB8888).expect("allocate");

    assert_eq!(
        constrained.modifier(),
        plain.modifier(),
        "an empty list must take the same path as an unconstrained allocation"
    );
}

/// A format the driver cannot allocate is refused on the constrained path too.
///
/// The fallback inside `create_with_modifiers` swallows the modifier attempt's
/// error by design; it must not swallow this one, or an unsupported format
/// would surface at commit time instead.
#[test]
fn an_unknown_format_is_refused_on_the_constrained_path() {
    let _guard = card_guard();
    let Some(device) = open_card() else { return };
    let gbm = GbmDevice::new(&device).expect("gbm device");

    let refused = GbmBuffer::create_with_modifiers(&gbm, 64, 64, 0xDEAD_BEEF, &[0]);

    assert!(
        matches!(refused, Err(GbmError::Allocation(_))),
        "the modifier fallback must not turn an unsupported format into a buffer"
    );
}

/// Every layout the driver honors comes back as the layout that was asked for.
///
/// This is the case vkms cannot fail and real allocators can (P-13, P-28). Each
/// candidate is offered **alone**, so a buffer reporting that modifier is one
/// the driver allocated in it -- not one it happened to pick from a list. An
/// implementation that ignored the list would report the unconstrained default
/// for every candidate, and one whose `modifier()` ignored the driver would
/// report a constant; either collapses the honored set to at most one layout.
///
/// How many layouts a driver honors is a property of the board, so the floor
/// is `DRMKIT_MIN_LAYOUTS` rather than a constant: measured, with
/// `SCANOUT | RENDERING`, at 5 on the i.MX8M Plus (linear, three Vivante
/// tilings and a vendor one) and 2 on the SA8155P (linear and UBWC). The Pi 5 is 1: v3d refuses
/// every non-linear layout once `SCANOUT` is set, and UIF only appears without
/// it. Unset, the set is printed and nothing beyond the per-candidate contract
/// is asserted, which is all a single-layout driver like vkms supports.
#[test]
fn every_honored_layout_comes_back_as_asked() {
    use drmkit_fmt::{mod_code, vendor};

    let _guard = card_guard();
    let Some(device) = open_card() else { return };
    let gbm = GbmDevice::new(&device).expect("gbm device");

    // Low bodies of every vendor: where each one keeps its plain tilings and
    // compression. Broader parameterized families (AFBC, AMD) need a body
    // built from fields, and the boards here do not offer them.
    let mut candidates = vec![0];
    for vendor in [
        vendor::INTEL,
        vendor::SAMSUNG,
        vendor::QCOM,
        vendor::VIVANTE,
        vendor::BROADCOM,
        vendor::ALLWINNER,
    ] {
        candidates.extend((1..=8).map(|body| mod_code(vendor, body)));
    }

    let mut honored = Vec::new();
    let mut substituted = std::collections::BTreeSet::new();
    let mut linear_reported = None;
    for &asked in &candidates {
        let Ok(buffer) = GbmBuffer::create_with_modifiers(&gbm, 64, 64, fourcc::ARGB8888, &[asked])
        else {
            continue;
        };
        let got = buffer.modifier();
        if asked == 0 {
            linear_reported = Some(got);
        }
        if got == asked {
            honored.push(asked);
        } else {
            // Not a refusal: the driver, or the fallback, handed back a buffer
            // in some other layout. Legal, and exactly why the modifier is
            // read back rather than assumed.
            substituted.insert(got);
        }
    }
    println!("note: honored layouts {honored:x?}, substitutes seen {substituted:x?}");

    // Every driver can allocate linear, so asking for it alone must get it --
    // unless the driver reports no modifiers at all (INVALID, as Mesa's
    // fallback does in the CI lane), in which case nothing here can be read
    // and only the DRMKIT_MIN_LAYOUTS floor below says anything.
    match linear_reported {
        Some(INVALID_MODIFIER) => println!("note: this driver reports no modifiers"),
        reported => assert_eq!(
            reported,
            Some(0),
            "every driver can allocate linear, and asking for it alone must get it"
        ),
    }
    if let Ok(minimum) = std::env::var("DRMKIT_MIN_LAYOUTS") {
        let minimum: usize = minimum.parse().expect("DRMKIT_MIN_LAYOUTS is a number");
        assert!(
            honored.len() >= minimum,
            "{} layout(s) came back as asked and DRMKIT_MIN_LAYOUTS asks for {minimum}: \
             either the modifier list is not reaching the driver or the modifier \
             read back is not the driver's",
            honored.len()
        );
    }
}
