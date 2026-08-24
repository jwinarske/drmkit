// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! Cached DRM object properties.
//!
//! Port of `src/core/property_store.{hpp,cpp}`.

use std::collections::HashMap;

use drm::control::Device as ControlDevice;

use crate::device::Device;
use crate::error::{CoreError, Result};

/// A DRM object kind, for property queries.
///
/// The C++ passes the raw `DRM_MODE_OBJECT_*` constant. Here the kind is an
/// enum so the `drm` crate's typed handle can be reconstructed, which is what
/// keeps `get_properties` type-safe on the way out.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub enum ObjectType {
    /// A CRTC.
    Crtc,
    /// A connector.
    Connector,
    /// An encoder.
    Encoder,
    /// A plane.
    Plane,
    /// A framebuffer.
    Framebuffer,
}

/// One cached property.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PropertyInfo {
    /// The property id, for use in an atomic request.
    pub id: u32,
    /// The property name as the kernel reports it.
    pub name: String,
    /// The value at the time the object was cached.
    pub value: u64,
    /// Whether the kernel marks this property `DRM_MODE_PROP_IMMUTABLE`.
    ///
    /// The C++ keeps the whole raw `flags` word so callers can distinguish
    /// immutable, range, enum, and blob properties without re-querying. Only
    /// the immutable bit is actually consulted anywhere in the tree, and the
    /// `drm` crate surfaces the rest through typed accessors, so this carries
    /// the one bit that has a caller and leaves the rest to
    /// [`get_property`](drm::control::Device::get_property).
    pub immutable: bool,
    /// Inclusive `(min, max)` for an **unsigned** range property, `None` for
    /// every other kind -- signed ranges included.
    ///
    /// The kernel rejects a range write outside these bounds with `EINVAL`,
    /// and an atomic commit is all-or-nothing: one out-of-range value fails
    /// the whole frame, including every layer that was fine. A caller that
    /// derives a value rather than echoing one back -- a zpos computed from a
    /// layer count, say -- has no other way to know what the plane will take.
    ///
    /// Signed ranges are deliberately not reported. The atomic ioctl carries
    /// every value as a `u64`, so a signed range arrives as two's complement:
    /// `CRTC_X`'s bounds are `(-2^31, 2^31 - 1)`, which as `u64` is
    /// `(18446744071562067968, 2147483647)` -- a pair where the minimum
    /// compares *greater* than the maximum. Anything doing unsigned arithmetic
    /// on that would turn a small negative offset into `INT32_MAX`, so the
    /// safe thing is to hand back nothing rather than a bound that is wrong in
    /// a direction the caller cannot see.
    pub range: Option<(u64, u64)>,
}

/// Per-object property cache.
///
/// Building an atomic request means naming properties by id, and resolving a
/// name to an id costs an ioctl per property. Caching once per object turns
/// per-frame property lookup into a hash-map hit.
#[derive(Debug, Clone, Default)]
pub struct PropertyStore {
    store: HashMap<u32, Vec<PropertyInfo>>,
}

impl PropertyStore {
    /// An empty store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Query and cache every property of one object, replacing any previous
    /// entry for it.
    ///
    /// A property whose `get_property` lookup fails is skipped rather than
    /// failing the whole object, matching the C++ `continue`: one unreadable
    /// property should not make a plane unusable.
    ///
    /// # Errors
    ///
    /// [`CoreError::Io`] if the object's property set cannot be read at all.
    pub fn cache_properties(
        &mut self,
        device: &Device,
        object_id: u32,
        object_type: ObjectType,
    ) -> Result<()> {
        let values = get_properties_raw(device, object_id, object_type)?;
        let (handles, raw_values) = values.as_props_and_values();

        let mut entries = Vec::with_capacity(handles.len());
        for (handle, value) in handles.iter().zip(raw_values.iter()) {
            let Ok(info) = device.get_property(*handle) else {
                // One unreadable property must not disqualify the object.
                continue;
            };
            let range = match info.value_type() {
                drm::control::property::ValueType::UnsignedRange(lo, hi) => Some((*lo, *hi)),
                _ => None,
            };
            entries.push(PropertyInfo {
                id: (*handle).into(),
                name: info.name().to_string_lossy().into_owned(),
                value: *value,
                immutable: !info.mutable(),
                range,
            });
        }

        self.store.insert(object_id, entries);
        Ok(())
    }

    /// Every cached property of an object, or `None` if it was never cached.
    #[must_use]
    pub fn properties(&self, object_id: u32) -> Option<&[PropertyInfo]> {
        self.store.get(&object_id).map(Vec::as_slice)
    }

    /// Look up one property by name.
    fn find(&self, object_id: u32, name: &str) -> Result<&PropertyInfo> {
        let entries = self
            .store
            .get(&object_id)
            .ok_or(CoreError::NoSuchObject { object_id })?;

        entries
            .iter()
            .find(|p| p.name == name)
            .ok_or_else(|| CoreError::NoSuchProperty {
                object_id,
                name: name.to_owned(),
            })
    }

    /// The id of a named property, for building an atomic request.
    ///
    /// # Errors
    ///
    /// [`CoreError::NoSuchObject`] if the object was never cached, or
    /// [`CoreError::NoSuchProperty`] if it has no such property. The C++
    /// reports `no_such_file_or_directory` for both; splitting them says which
    /// mistake was made.
    pub fn property_id(&self, object_id: u32, name: &str) -> Result<u32> {
        self.find(object_id, name).map(|p| p.id)
    }

    /// The cached value of a named property.
    ///
    /// # Errors
    ///
    /// As [`PropertyStore::property_id`].
    pub fn property_value(&self, object_id: u32, name: &str) -> Result<u64> {
        self.find(object_id, name).map(|p| p.value)
    }

    /// The raw value of an enum property's named variant.
    ///
    /// Enum properties are named by string in the KMS ABI but written as
    /// integers, and the integer a driver assigns to a given name is its own
    /// business. Hardcoding one that happened to be right on the card in front
    /// of you silently selects a different variant elsewhere -- for
    /// `COLOR_ENCODING` that is a wrong YCbCr matrix, which is a visible tint
    /// rather than an error.
    ///
    /// # Errors
    ///
    /// [`CoreError::NoSuchProperty`] if the object has no such property, if it
    /// is not an enum, or if it has no variant by that name.
    pub fn enum_value(
        &self,
        device: &Device,
        object_id: u32,
        property: &str,
        variant: &str,
    ) -> Result<u64> {
        let id = self.property_id(object_id, property)?;
        let missing = || CoreError::NoSuchProperty {
            object_id,
            name: property.to_owned(),
        };
        // `RawResourceHandle` is a `NonZeroU32`; a zero id is not a property.
        let handle = drm::control::RawResourceHandle::new(id).ok_or_else(missing)?;
        let info =
            drm::control::Device::get_property(device, handle.into()).map_err(|_| missing())?;
        let drm::control::property::ValueType::Enum(values) = info.value_type() else {
            return Err(missing());
        };
        let (raw, enums) = values.values();
        enums
            .iter()
            .zip(raw.iter())
            .find(|(entry, _)| entry.name().to_string_lossy() == variant)
            .map(|(_, value)| *value)
            .ok_or_else(missing)
    }

    /// Whether a named property is `DRM_MODE_PROP_IMMUTABLE`.
    ///
    /// Writing an immutable property in an atomic commit is rejected with
    /// `EINVAL` whatever the value, so a request builder must skip it.
    ///
    /// # Errors
    ///
    /// As [`PropertyStore::property_id`].
    pub fn is_immutable(&self, object_id: u32, name: &str) -> Result<bool> {
        self.find(object_id, name).map(|p| p.immutable)
    }

    /// The inclusive `(min, max)` of a named unsigned range property.
    ///
    /// `None` when the property is not an unsigned range -- see
    /// [`PropertyInfo::range`] -- which callers treat as "no bound to enforce"
    /// rather than an error.
    ///
    /// # Errors
    ///
    /// As [`PropertyStore::property_id`].
    pub fn range(&self, object_id: u32, name: &str) -> Result<Option<(u64, u64)>> {
        self.find(object_id, name).map(|p| p.range)
    }

    /// Drop everything cached.
    pub fn clear(&mut self) {
        self.store.clear();
    }

    /// Drop the cache for one object, for instance after a hotplug.
    pub fn forget(&mut self, object_id: u32) {
        self.store.remove(&object_id);
    }

    /// How many objects are cached.
    #[must_use]
    pub fn len(&self) -> usize {
        self.store.len()
    }

    /// Whether nothing is cached.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.store.is_empty()
    }
}

/// Fetch an object's property set, reconstructing the `drm` crate's typed
/// handle from the raw id and kind.
fn get_properties_raw(
    device: &Device,
    object_id: u32,
    object_type: ObjectType,
) -> Result<drm::control::PropertyValueSet> {
    use drm::control::{connector, crtc, encoder, framebuffer, plane};

    let raw: drm::control::RawResourceHandle = object_id
        .try_into()
        .map_err(|_| CoreError::NoSuchObject { object_id })?;

    let result = match object_type {
        ObjectType::Crtc => device.get_properties(crtc::Handle::from(raw)),
        ObjectType::Connector => device.get_properties(connector::Handle::from(raw)),
        ObjectType::Encoder => device.get_properties(encoder::Handle::from(raw)),
        ObjectType::Plane => device.get_properties(plane::Handle::from(raw)),
        ObjectType::Framebuffer => device.get_properties(framebuffer::Handle::from(raw)),
    };

    result.map_err(|e| CoreError::from_io(&e))
}
