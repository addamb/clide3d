//! Brush presets (docs/SCULPT_PLAN.md 3.7): a stroke's settings without
//! its points, by name. About 20 are built in (`assets/brushes/*.json`,
//! compiled in); scenes save their own (`Scene.brushes`, `brush_preset
//! save`). A `sculpt` call or stroke names one with `preset`: the preset
//! goes under the stroke's own fields (the stroke wins, key by key), and
//! the record keeps the merged stroke, so it replays without the preset.

use std::collections::BTreeMap;
use std::sync::{Arc, OnceLock};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use super::spec::StrokeSpec;

/// Most presets a scene saves.
pub const MAX_PRESETS: usize = 256;
/// Longest preset, as JSON text.
pub const MAX_PRESET_BYTES: usize = 8192;
/// Longest preset name.
pub const MAX_NAME: usize = 64;

/// The built-in presets: (name, JSON).
const BUILTIN: [(&str, &str); 21] = [
    ("Standard", include_str!("../../assets/brushes/Standard.json")),
    ("Clay", include_str!("../../assets/brushes/Clay.json")),
    ("ClayBuildup", include_str!("../../assets/brushes/ClayBuildup.json")),
    ("ClayTubes", include_str!("../../assets/brushes/ClayTubes.json")),
    ("DamStandard", include_str!("../../assets/brushes/DamStandard.json")),
    ("TrimDynamic", include_str!("../../assets/brushes/TrimDynamic.json")),
    ("hPolish", include_str!("../../assets/brushes/hPolish.json")),
    ("Inflate", include_str!("../../assets/brushes/Inflate.json")),
    ("Move", include_str!("../../assets/brushes/Move.json")),
    ("Pinch", include_str!("../../assets/brushes/Pinch.json")),
    ("Snakehook", include_str!("../../assets/brushes/Snakehook.json")),
    ("Layer", include_str!("../../assets/brushes/Layer.json")),
    ("Flatten", include_str!("../../assets/brushes/Flatten.json")),
    ("Planar", include_str!("../../assets/brushes/Planar.json")),
    ("Smooth", include_str!("../../assets/brushes/Smooth.json")),
    ("SmoothPeaks", include_str!("../../assets/brushes/SmoothPeaks.json")),
    ("SmoothValleys", include_str!("../../assets/brushes/SmoothValleys.json")),
    ("Pores", include_str!("../../assets/brushes/Pores.json")),
    ("Wrinkles", include_str!("../../assets/brushes/Wrinkles.json")),
    ("Scales", include_str!("../../assets/brushes/Scales.json")),
    // For the mask tool's paint strokes ({paint: [{preset: "MaskPen", ..}]}).
    ("MaskPen", include_str!("../../assets/brushes/MaskPen.json")),
];

/// Stroke fields a preset may not hold: where the stroke goes is the
/// stroke's own.
const PLACE: [&str; 4] = ["points", "point", "frame", "preset"];

/// A stroke's settings (a JSON object with at least `brush`), checked to
/// read as a stroke once given points and a radius. Shared: the scene is
/// cloned every frame and per undo step.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "Value", into = "Value")]
pub struct Preset(Arc<Map<String, Value>>);

impl TryFrom<Value> for Preset {
    type Error = String;

    fn try_from(v: Value) -> Result<Preset, String> {
        let Value::Object(map) = v else { return Err("a preset is an object of stroke fields (brush, strength, falloff, alpha ...)".into()) };
        if serde_json::to_string(&map).map_or(usize::MAX, |t| t.len()) > MAX_PRESET_BYTES {
            return Err(format!("a preset is at most {MAX_PRESET_BYTES} bytes of JSON"));
        }
        if let Some(k) = PLACE.iter().find(|k| map.contains_key(**k)) {
            return Err(format!("a preset holds settings, not {k}: strokes give their own points"));
        }
        if map.get("kind").is_some_and(|k| k.get("drag_rect").is_some()) {
            return Err("a preset cannot hold a drag_rect (it has positions)".into());
        }
        if !map.contains_key("brush") {
            return Err("a preset needs brush".into());
        }
        // It must read as a stroke once a stroke gives points and a radius.
        let mut probe = json!({ "radius": 0.05, "points": [[0, 0, 0], [0.01, 0, 0]] });
        for (k, v) in &map {
            probe[k] = v.clone();
        }
        serde_json::from_value::<StrokeSpec>(probe).map_err(|e| format!("not a stroke's settings: {e}"))?;
        Ok(Preset(Arc::new(map)))
    }
}

impl From<Preset> for Value {
    fn from(p: Preset) -> Value {
        Value::Object(Arc::unwrap_or_clone(p.0))
    }
}

impl Preset {
    pub fn fields(&self) -> &Map<String, Value> {
        &self.0
    }

    /// A preset from a stroke as a record keeps it: its settings without
    /// where it went.
    pub fn from_spec(spec: &StrokeSpec) -> Result<Preset, String> {
        let mut v = serde_json::to_value(spec).map_err(|e| e.to_string())?;
        if let Some(o) = v.as_object_mut() {
            for k in PLACE {
                o.remove(k);
            }
            if o.get("kind").is_some_and(|k| k.get("drag_rect").is_some()) {
                o.remove("kind");
            }
            o.remove("radius");
        }
        Preset::try_from(v)
    }

    /// A short summary for lists.
    pub fn summary(&self, name: &str, builtin: bool) -> Value {
        let f = |k: &str| self.0.get(k).cloned().unwrap_or(Value::Null);
        json!({ "name": name, "builtin": builtin, "brush": f("brush"), "radius": f("radius"), "strength": f("strength"), "falloff": f("falloff"), "alpha": f("alpha"), "kind": f("kind") })
    }
}

/// The built-in presets, by name.
pub fn builtin() -> &'static BTreeMap<String, Preset> {
    static ALL: OnceLock<BTreeMap<String, Preset>> = OnceLock::new();
    ALL.get_or_init(|| {
        BUILTIN
            .iter()
            .map(|(name, text)| {
                let v: Value = serde_json::from_str(text).unwrap_or_else(|e| panic!("built-in preset {name}: {e}"));
                (name.to_string(), Preset::try_from(v).unwrap_or_else(|e| panic!("built-in preset {name}: {e}")))
            })
            .collect()
    })
}

/// A preset by name: the scene's own, else a built-in.
pub fn find<'a>(scene: &'a BTreeMap<String, Preset>, name: &str) -> Option<&'a Preset> {
    scene.get(name).or_else(|| builtin().get(name))
}

/// Whether `name` can name a preset.
pub fn name_ok(name: &str) -> bool {
    !name.trim().is_empty() && name.chars().count() <= MAX_NAME
}

/// A scene's presets, read at most `MAX_PRESETS` long.
pub fn library<'de, D: serde::Deserializer<'de>>(d: D) -> Result<BTreeMap<String, Preset>, D::Error> {
    let lib = super::alpha::capped_map::<D, String, Preset, MAX_PRESETS>(d)?;
    if lib.keys().any(|n| !name_ok(n)) {
        return Err(serde::de::Error::custom(format!("preset names are 1 to {MAX_NAME} characters")));
    }
    Ok(lib)
}

/// A stroke with its preset merged in: the call's `preset` under the
/// stroke's own `preset` under the stroke's fields.
pub fn merge(scene: &BTreeMap<String, Preset>, call: Option<&str>, stroke: &Value) -> Result<Value, String> {
    let Some(fields) = stroke.as_object() else { return Ok(stroke.clone()) };
    let own = match fields.get("preset") {
        None | Some(Value::Null) => None,
        Some(Value::String(n)) => Some(n.as_str()),
        Some(_) => return Err("preset must be a preset's name (brush_preset list)".into()),
    };
    if own.is_none() && call.is_none() {
        return Ok(stroke.clone());
    }
    let get = |n: &str| find(scene, n).ok_or_else(|| format!("no brush preset {n}; brush_preset list shows them"));
    let mut out = match call {
        Some(n) => get(n)?.fields().clone(),
        None => Map::new(),
    };
    if let Some(n) = own {
        out.extend(get(n)?.fields().clone());
    }
    for (k, v) in fields {
        if k != "preset" {
            out.insert(k.clone(), v.clone());
        }
    }
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_builtin_reads_and_round_trips() {
        assert_eq!(builtin().len(), BUILTIN.len());
        for (name, p) in builtin() {
            let v: Value = p.clone().into();
            assert_eq!(&Preset::try_from(v).unwrap(), p, "{name}");
        }
    }

    #[test]
    fn presets_hold_settings_only() {
        for bad in [json!({"strength": 0.5}), json!({"brush": "clay", "points": [[0, 0, 0]]}), json!({"brush": "sand"}), json!({"brush": "clay", "preset": "Clay"}), json!([1])] {
            assert!(Preset::try_from(bad.clone()).is_err(), "{bad}");
        }
        let scene = BTreeMap::new();
        let merged = merge(&scene, Some("Clay"), &json!({"preset": "ClayTubes", "radius": 0.1, "strength": 0.2})).unwrap();
        assert_eq!(merged, json!({"brush": "clay_tubes", "radius": 0.1, "strength": 0.2}));
        assert!(merge(&scene, Some("Nope"), &json!({"radius": 0.1})).is_err());
    }
}
