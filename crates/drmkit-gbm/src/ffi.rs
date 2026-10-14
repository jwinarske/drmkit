// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! libgbm, opened at run time.
//!
//! Linking libgbm made it a build input: every sysroot a board was built
//! against had to stage its `libgbm.so` (and libdrm and libexpat behind it),
//! and which modifier entry points it exported was decided at build time --
//! a binary referencing `gbm_bo_create_with_modifiers2` would not even load
//! against the SA8155P's libgbm, which has only the v1 pair (P-39). Opened
//! here instead, a missing library is [`GbmError::NotGbmCapable`] rather than
//! a binary that fails to start, and v1 or v2 is whichever the library has.
//!
//! The signatures are `gbm.h`'s, transcribed. Nothing checks them against the
//! header at build time, so each is kept to the exact C types, and every one
//! is exercised by this crate's device tests.

use std::ffi::{c_char, c_int, c_uint, c_void};
use std::sync::OnceLock;

use libloading::Library;

use crate::GbmError;

/// `struct gbm_device`, `struct gbm_bo`, `struct gbm_surface`: opaque.
pub(crate) type DevicePtr = *mut c_void;
pub(crate) type BoPtr = *mut c_void;
pub(crate) type SurfacePtr = *mut c_void;

/// `union gbm_bo_handle`, returned by value. Every arm is mirrored so the
/// union has the C layout and is returned the way the C ABI returns it.
#[repr(C)]
#[derive(Clone, Copy)]
pub(crate) union BoHandle {
    pub(crate) ptr: *mut c_void,
    pub(crate) s32: i32,
    pub(crate) u32_: u32,
    pub(crate) s64: i64,
    pub(crate) u64_: u64,
}

type CreateBoV1 = unsafe extern "C" fn(DevicePtr, u32, u32, u32, *const u64, c_uint) -> BoPtr;
type CreateBoV2 = unsafe extern "C" fn(DevicePtr, u32, u32, u32, *const u64, c_uint, u32) -> BoPtr;
type CreateSurfaceV1 =
    unsafe extern "C" fn(DevicePtr, u32, u32, u32, *const u64, c_uint) -> SurfacePtr;
type CreateSurfaceV2 =
    unsafe extern "C" fn(DevicePtr, u32, u32, u32, *const u64, c_uint, u32) -> SurfacePtr;

/// The entry points this crate uses, resolved once.
pub(crate) struct Gbm {
    pub(crate) create_device: unsafe extern "C" fn(c_int) -> DevicePtr,
    pub(crate) device_destroy: unsafe extern "C" fn(DevicePtr),
    pub(crate) device_get_backend_name: unsafe extern "C" fn(DevicePtr) -> *const c_char,

    pub(crate) bo_create: unsafe extern "C" fn(DevicePtr, u32, u32, u32, u32) -> BoPtr,
    /// Mesa 21.3: the modifier list *and* the usage flags.
    pub(crate) bo_create_with_modifiers2: Option<CreateBoV2>,
    /// The older entry point, with no usage flags. The SA8155P's libgbm has
    /// only this one.
    pub(crate) bo_create_with_modifiers: Option<CreateBoV1>,
    pub(crate) bo_destroy: unsafe extern "C" fn(BoPtr),
    pub(crate) bo_get_width: unsafe extern "C" fn(BoPtr) -> u32,
    pub(crate) bo_get_height: unsafe extern "C" fn(BoPtr) -> u32,
    pub(crate) bo_get_stride: unsafe extern "C" fn(BoPtr) -> u32,
    pub(crate) bo_get_format: unsafe extern "C" fn(BoPtr) -> u32,
    pub(crate) bo_get_modifier: unsafe extern "C" fn(BoPtr) -> u64,
    pub(crate) bo_get_plane_count: unsafe extern "C" fn(BoPtr) -> c_int,
    pub(crate) bo_get_handle: unsafe extern "C" fn(BoPtr) -> BoHandle,
    pub(crate) bo_get_fd: unsafe extern "C" fn(BoPtr) -> c_int,

    pub(crate) surface_create: unsafe extern "C" fn(DevicePtr, u32, u32, u32, u32) -> SurfacePtr,
    pub(crate) surface_create_with_modifiers2: Option<CreateSurfaceV2>,
    pub(crate) surface_create_with_modifiers: Option<CreateSurfaceV1>,
    pub(crate) surface_destroy: unsafe extern "C" fn(SurfacePtr),
    pub(crate) surface_lock_front_buffer: unsafe extern "C" fn(SurfacePtr) -> BoPtr,
    pub(crate) surface_release_buffer: unsafe extern "C" fn(SurfacePtr, BoPtr),
    pub(crate) surface_has_free_buffers: unsafe extern "C" fn(SurfacePtr) -> c_int,

    /// Held for the process's lifetime, never closed: a library unloaded
    /// under a live buffer would leave its destructor pointing at nothing.
    _library: Library,
}

/// The library names tried, in order: the runtime soname every distribution
/// ships, then the development symlink some vendor trees have instead.
const NAMES: [&str; 2] = ["libgbm.so.1", "libgbm.so"];

static GBM: OnceLock<Result<Gbm, String>> = OnceLock::new();

/// libgbm, opened on first use.
///
/// # Errors
///
/// [`GbmError::NotGbmCapable`] when no libgbm can be opened or it lacks an
/// entry point this crate cannot do without. The reason is kept and repeated
/// on every later call; the open is not retried.
pub(crate) fn gbm() -> Result<&'static Gbm, GbmError> {
    GBM.get_or_init(open)
        .as_ref()
        .map_err(|reason| GbmError::NotGbmCapable(reason.clone()))
}

fn open() -> Result<Gbm, String> {
    let mut failures = Vec::new();
    for name in NAMES {
        // SAFETY: loading libgbm runs its initializers, which have no
        // preconditions on the caller.
        match unsafe { Library::new(name) } {
            Ok(library) => return resolve(library),
            Err(error) => failures.push(format!("{name}: {error}")),
        }
    }
    Err(failures.join("; "))
}

/// Look up a required symbol, copying the function pointer out.
macro_rules! required {
    ($library:expr, $name:literal) => {
        // SAFETY: the type the pointer is read as is the symbol's prototype
        // in gbm.h, and the library it points into is never unloaded.
        *unsafe { $library.get(concat!($name, "\0").as_bytes()) }
            .map_err(|error| format!("{}: {error}", $name))?
    };
}

/// Look up a symbol the library may not have.
macro_rules! optional {
    ($library:expr, $name:literal) => {
        // SAFETY: as `required!`.
        unsafe { $library.get(concat!($name, "\0").as_bytes()) }
            .ok()
            .map(|symbol| *symbol)
    };
}

fn resolve(library: Library) -> Result<Gbm, String> {
    Ok(Gbm {
        create_device: required!(library, "gbm_create_device"),
        device_destroy: required!(library, "gbm_device_destroy"),
        device_get_backend_name: required!(library, "gbm_device_get_backend_name"),
        bo_create: required!(library, "gbm_bo_create"),
        bo_create_with_modifiers2: optional!(library, "gbm_bo_create_with_modifiers2"),
        bo_create_with_modifiers: optional!(library, "gbm_bo_create_with_modifiers"),
        bo_destroy: required!(library, "gbm_bo_destroy"),
        bo_get_width: required!(library, "gbm_bo_get_width"),
        bo_get_height: required!(library, "gbm_bo_get_height"),
        bo_get_stride: required!(library, "gbm_bo_get_stride"),
        bo_get_format: required!(library, "gbm_bo_get_format"),
        bo_get_modifier: required!(library, "gbm_bo_get_modifier"),
        bo_get_plane_count: required!(library, "gbm_bo_get_plane_count"),
        bo_get_handle: required!(library, "gbm_bo_get_handle"),
        bo_get_fd: required!(library, "gbm_bo_get_fd"),
        surface_create: required!(library, "gbm_surface_create"),
        surface_create_with_modifiers2: optional!(library, "gbm_surface_create_with_modifiers2"),
        surface_create_with_modifiers: optional!(library, "gbm_surface_create_with_modifiers"),
        surface_destroy: required!(library, "gbm_surface_destroy"),
        surface_lock_front_buffer: required!(library, "gbm_surface_lock_front_buffer"),
        surface_release_buffer: required!(library, "gbm_surface_release_buffer"),
        surface_has_free_buffers: required!(library, "gbm_surface_has_free_buffers"),
        _library: library,
    })
}

/// What the last failed libgbm call left in `errno`, for an error message.
pub(crate) fn last_error(call: &str) -> String {
    format!("{call}: {}", std::io::Error::last_os_error())
}
