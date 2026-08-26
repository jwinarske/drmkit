// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! One `drm_info` dump, as much of it as drmkit's types can express.

use serde_json::Value;

use drmkit_planes::{PlaneCapabilities, PlaneType};

/// A device from the corpus.
pub(crate) struct Device {
    /// The dump's file stem, which is how a finding is reported.
    pub(crate) name: String,
    pub(crate) driver: String,
    /// Whether the dump carries atomic plane properties.
    ///
    /// A dump taken on a pre-atomic driver, or by a client that did not set
    /// the atomic capability, has planes with almost no properties at all.
    /// Rules about atomic properties must not be applied to those -- and
    /// getting this wrong is how "978 planes lack `SRC_W`" reads as a finding
    /// when every one of them is from a radeon or nouveau dump.
    pub(crate) atomic: bool,
    /// The CRTCs the dump lists. Their count bounds the `possible_crtcs` bits
    /// worth asking about.
    pub(crate) crtcs: Vec<Crtc>,
    pub(crate) planes: Vec<Plane>,
}

/// One CRTC, as much of it as the rules ask about.
pub(crate) struct Crtc {
    /// Property names the dump lists.
    pub(crate) properties: Vec<String>,
}

impl Crtc {
    pub(crate) fn has(&self, name: &str) -> bool {
        self.properties.iter().any(|p| p == name)
    }
}

/// One plane, in drmkit's own capability type plus what the dump adds.
pub(crate) struct Plane {
    pub(crate) caps: PlaneCapabilities,
    /// Property names the dump lists, for rules asking about presence.
    pub(crate) properties: Vec<String>,
    /// `rotation` enum entry names, when it has them.
    pub(crate) rotation_names: Vec<String>,
    /// Whether `zpos` is flagged immutable, which is the honest form of a
    /// plane whose stacking position cannot move.
    pub(crate) zpos_immutable: bool,
}

impl Device {
    /// How many CRTCs the dump lists, as a bit index bound.
    pub(crate) fn crtc_count(&self) -> u32 {
        u32::try_from(self.crtcs.len()).unwrap_or(u32::MAX)
    }

    pub(crate) fn from_json(name: &str, value: &Value) -> Self {
        let driver = value
            .get("driver")
            .and_then(|d| d.get("name"))
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_owned();

        let planes: Vec<Plane> = value
            .get("planes")
            .and_then(Value::as_array)
            .map(|planes| planes.iter().map(plane_from_json).collect())
            .unwrap_or_default();

        // Atomic-only properties are the tell. `CRTC_X` is one the kernel
        // hides from a non-atomic client, so a dump carrying it was taken by
        // an atomic one.
        let atomic = planes
            .iter()
            .any(|plane| plane.properties.iter().any(|p| p == "CRTC_X"));

        let crtcs: Vec<Crtc> = value
            .get("crtcs")
            .and_then(Value::as_array)
            .map(|crtcs| {
                crtcs
                    .iter()
                    .map(|crtc| Crtc {
                        properties: crtc
                            .get("properties")
                            .and_then(Value::as_object)
                            .map(|map| map.keys().cloned().collect())
                            .unwrap_or_default(),
                    })
                    .collect()
            })
            .unwrap_or_default();

        Self {
            name: name.to_owned(),
            driver,
            atomic,
            crtcs,
            planes,
        }
    }
}

fn plane_from_json(value: &Value) -> Plane {
    let properties: Vec<String> = value
        .get("properties")
        .and_then(Value::as_object)
        .map(|map| map.keys().cloned().collect())
        .unwrap_or_default();

    let props = value.get("properties");
    let has = |name: &str| properties.iter().any(|p| p == name);

    let formats: Vec<u32> = value
        .get("formats")
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .filter_map(|entry| {
                    // Older dumps list a bare integer; newer ones an object
                    // carrying the fourcc alongside its name.
                    entry.as_u64().or_else(|| entry.get("fourcc")?.as_u64())
                })
                .filter_map(|raw| u32::try_from(raw).ok())
                .collect()
        })
        .unwrap_or_default();

    let plane_type = match enum_value(props, "type").as_deref() {
        Some("Primary") => PlaneType::Primary,
        Some("Cursor") => PlaneType::Cursor,
        _ => PlaneType::Overlay,
    };

    let (zpos_min, zpos_max) = range_of(props, "zpos");
    let zpos_immutable = props
        .and_then(|p| p.get("zpos"))
        .and_then(|z| z.get("immutable"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let rotation_names = bitmask_names(props, "rotation");

    Plane {
        caps: PlaneCapabilities {
            id: u32::try_from(value.get("id").and_then(Value::as_u64).unwrap_or(0)).unwrap_or(0),
            possible_crtcs: u32::try_from(
                value
                    .get("possible_crtcs")
                    .and_then(Value::as_u64)
                    .unwrap_or(0),
            )
            .unwrap_or(0),
            plane_type,
            formats,
            zpos_min,
            zpos_max,
            supports_rotation: has("rotation"),
            supports_scaling: has("SRC_W"),
            has_fb_damage_clips: has("FB_DAMAGE_CLIPS"),
            has_pixel_blend_mode: has("pixel blend mode"),
            has_per_plane_alpha: has("alpha"),
            has_color_encoding: has("COLOR_ENCODING"),
            has_color_range: has("COLOR_RANGE"),
            ..PlaneCapabilities::default()
        },
        properties,
        rotation_names,
        zpos_immutable,
    }
}

/// The name of an enum property's current value.
fn enum_value(props: Option<&Value>, name: &str) -> Option<String> {
    let property = props?.get(name)?;
    let raw = property.get("raw_value")?.as_u64()?;
    let spec = property.get("spec")?.as_array()?;
    spec.iter()
        .find(|entry| entry.get("value").and_then(Value::as_u64) == Some(raw))
        .and_then(|entry| entry.get("name")?.as_str())
        .map(ToOwned::to_owned)
}

/// A range property's inclusive bounds.
fn range_of(props: Option<&Value>, name: &str) -> (Option<u64>, Option<u64>) {
    let Some(property) = props.and_then(|p| p.get(name)) else {
        return (None, None);
    };
    let spec = property.get("spec");
    let min = spec.and_then(|s| s.get("min")).and_then(Value::as_u64);
    // `Max`, capitalised, is what drm_info emits -- and reading only the
    // lowercase spelling silently made every range look degenerate, which
    // read as the whole fleet having fixed zpos. Both are accepted so a
    // future spelling change is a missing value rather than a wrong survey.
    let max = spec
        .and_then(|s| s.get("Max"))
        .or_else(|| spec?.get("max"))
        .and_then(Value::as_u64);
    // Only a range the dump actually states. An earlier version fell back to
    // the property's current value here, on the reasoning that one value is a
    // range of one -- which manufactures a degenerate range out of a dump that
    // never claimed one, and a degenerate range is exactly what the P-6 survey
    // looks for. A property present with no stated bounds is unknown, not
    // fixed.
    (min, max)
}

/// The names a bitmask property advertises.
fn bitmask_names(props: Option<&Value>, name: &str) -> Vec<String> {
    let Some(spec) = props
        .and_then(|p| p.get(name))
        .and_then(|p| p.get("spec"))
        .and_then(Value::as_array)
    else {
        return Vec::new();
    };
    spec.iter()
        .filter_map(|entry| entry.get("name")?.as_str())
        .map(ToOwned::to_owned)
        .collect()
}
