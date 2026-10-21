//! Kitbash: a library of procedural hard-surface parts and the passes
//! that place them, for the secondary and tertiary detail of AAA vehicles,
//! weapons and mechs (panels, plates, hatches, vents, pistons, cables,
//! hoses; bolts, rivets, seams, latches, handles, lights, stencils).
//!
//! - `Part` (this file): one parametric part (`Kind` + size, style,
//!   count, points...). Its mesh is built in `parts.rs` from the hard
//!   surface primitives in `geo.rs`: chamfered edges, flat faces where
//!   edges are hard, UVs, and a material per role (paint takes the
//!   surface the part sits on; metal, rubber, chrome, glow... have
//!   defaults a scene material of the same name replaces).
//! - `surface.rs`: a target's surface in world (rest) space: ray snapping,
//!   flat regions, hard edges, where paint and decals are.
//! - `place.rs`: placements become scene objects. Repeated parts are
//!   instances of one hidden source (a `Scatter` under a group); groups
//!   hang on the target's bone, so parts move with the rig and export
//!   under the joint; every object a call adds carries its group tag, so
//!   one call removes it all.
//! - `passes.rs`: the auto-detail passes (panel, bolt, greeble, cable).
//!
//! Everything is deterministic: the same settings and seed give the same
//! parts in the same places.

pub mod geo;
pub mod parts;
pub mod passes;
pub mod place;
pub mod span;
pub mod surface;
pub mod tool;

#[cfg(test)]
mod check;
#[cfg(test)]
mod tests;

use std::collections::BTreeMap;

use glam::Vec3;
use serde::{Deserialize, Serialize};

use crate::material::Material;
use crate::mesh::Mesh;
use geo::Role;

/// The asset size the default part sizes are made for (a 6.5 m mech).
/// Parts placed without a size scale with the asset they go on.
pub const REFERENCE_SIZE: f32 = 6.5;

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Bolt,
    Nut,
    Washer,
    Hinge,
    Latch,
    Clamp,
    Handle,
    Vent,
    Hatch,
    Piston,
    Cable,
    Hose,
    CorrugatedTube,
    Light,
    Antenna,
    Sensor,
    Exhaust,
    ArmorPlate,
    TrackLink,
    Gear,
    Spring,
    Pipe,
    Connector,
    AmmoBox,
    JerryCan,
    Strap,
    Net,
    Stencil,
    Seam,
    Greeble,
    Clip,
}

/// What a kind is: name, styles (first is the default), default size
/// [along X, out of the surface, across] at `REFERENCE_SIZE`, whether it
/// runs between points, whether its size is real-world (not scaled with
/// the asset), and a short description.
pub struct Info {
    pub kind: Kind,
    pub name: &'static str,
    pub styles: &'static [&'static str],
    pub size: [f32; 3],
    pub between_points: bool,
    pub real_size: bool,
    pub about: &'static str,
}

pub const CATALOG: [Info; 31] = [
    Info { kind: Kind::Bolt, name: "bolt", styles: &["hex", "socket", "rivet", "countersunk", "carriage"], size: [0.045, 0.022, 0.045], between_points: false, real_size: false, about: "bolt head on a washer: hex, socket (hex recess), rivet (dome), countersunk (flush, slotted), carriage (low dome)" },
    Info { kind: Kind::Nut, name: "nut", styles: &["hex", "flange", "wing"], size: [0.05, 0.04, 0.05], between_points: false, real_size: false, about: "nut on a washer with the threaded stud showing" },
    Info { kind: Kind::Washer, name: "washer", styles: &["flat", "lock"], size: [0.06, 0.006, 0.06], between_points: false, real_size: false, about: "flat ring" },
    Info { kind: Kind::Hinge, name: "hinge", styles: &["barrel", "piano"], size: [0.3, 0.06, 0.16], between_points: false, real_size: false, about: "knuckled barrel along X with bolted leaves; count = knuckles" },
    Info { kind: Kind::Latch, name: "latch", styles: &["draw", "toggle", "cam"], size: [0.1, 0.05, 0.22], between_points: false, real_size: false, about: "draw latch: base, lever, hook loop over a keeper (+Z)" },
    Info { kind: Kind::Clamp, name: "clamp", styles: &["band", "saddle"], size: [0.07, 0.1, 0.22], between_points: false, real_size: false, about: "pipe clamp: a band over a pipe of height size[1], bolted ears" },
    Info { kind: Kind::Handle, name: "handle", styles: &["bar", "loop", "recessed"], size: [0.4, 0.1, 0.035], between_points: false, real_size: false, about: "grab bar on standoffs, a bent loop, or a recessed pocket with a bar" },
    Info { kind: Kind::Vent, name: "vent", styles: &["louver", "slots", "grille", "fan", "mesh"], size: [0.5, 0.06, 0.3], between_points: false, real_size: false, about: "framed vent: angled louvers, slots, a crossed grille, a fan, or fine mesh; count = slats" },
    Info { kind: Kind::Hatch, name: "hatch", styles: &["handle", "latch", "plain", "vented"], size: [0.6, 0.03, 0.45], between_points: false, real_size: false, about: "access panel: seam groove all round, hinge on -Z, handle or latch on +Z, corner bolts" },
    Info { kind: Kind::Piston, name: "piston", styles: &["hydraulic", "shock"], size: [0.0, 0.0, 0.0], between_points: true, real_size: false, about: "hydraulic cylinder between points[0] (body) and points[1] (rod): clevis eyes, chrome rod, rubber boot, ports; radius" },
    Info { kind: Kind::Cable, name: "cable", styles: &["single", "bundle", "braided"], size: [0.0, 0.0, 0.0], between_points: true, real_size: false, about: "cable or wire bundle along points with sag, clips and connectors; radius, sag, clips" },
    Info { kind: Kind::Hose, name: "hose", styles: &["corrugated", "smooth", "braided"], size: [0.0, 0.0, 0.0], between_points: true, real_size: false, about: "hose along points with crimped fittings, sag and clips; radius" },
    Info { kind: Kind::CorrugatedTube, name: "corrugated_tube", styles: &["ribbed", "spiral"], size: [0.0, 0.0, 0.0], between_points: true, real_size: false, about: "big ribbed duct along points with end flanges; radius" },
    Info { kind: Kind::Light, name: "light", styles: &["round", "rect", "cage", "strip"], size: [0.16, 0.08, 0.16], between_points: false, real_size: false, about: "lamp: housing, bezel and glowing lens (glow color; lit false = dark lens)" },
    Info { kind: Kind::Antenna, name: "antenna", styles: &["whip", "blade", "stub"], size: [0.06, 1.2, 0.06], between_points: false, real_size: false, about: "whip on a sprung mount, blade fin, or stub; size[1] = length" },
    Info { kind: Kind::Sensor, name: "sensor", styles: &["dome", "camera", "lidar"], size: [0.2, 0.14, 0.2], between_points: false, real_size: false, about: "glass dome, camera with lens and hood (looks +Z), or lidar head" },
    Info { kind: Kind::Exhaust, name: "exhaust", styles: &["stack", "port"], size: [0.18, 0.45, 0.18], between_points: false, real_size: false, about: "stack pipe with perforated heat shield and flange, or a sooty louvered port" },
    Info { kind: Kind::ArmorPlate, name: "armor_plate", styles: &["bolted", "plain", "layered"], size: [0.8, 0.03, 0.6], between_points: false, real_size: false, about: "chamfered plate; bolted adds bolts at the corners and along the edges (count per long side), layered stacks a smaller plate" },
    Info { kind: Kind::TrackLink, name: "track_link", styles: &["grouser", "rubber_pad"], size: [0.18, 0.06, 0.55], between_points: false, real_size: false, about: "tank track link: shoe, grousers, pin bosses; count = links in a row along X" },
    Info { kind: Kind::Gear, name: "gear", styles: &["spur", "sprocket"], size: [0.3, 0.06, 0.3], between_points: false, real_size: false, about: "spur gear or sprocket with hub; count = teeth" },
    Info { kind: Kind::Spring, name: "spring", styles: &["coil", "die"], size: [0.12, 0.4, 0.12], between_points: false, real_size: false, about: "coil spring along +Y (or between points); count = coils" },
    Info { kind: Kind::Pipe, name: "pipe", styles: &["flanged", "plain"], size: [0.0, 0.0, 0.0], between_points: true, real_size: false, about: "rigid pipe along points with rounded bends and bolted flanges; radius" },
    Info { kind: Kind::Connector, name: "connector", styles: &["round", "rect"], size: [0.08, 0.12, 0.08], between_points: false, real_size: false, about: "multi-pin plug on a square mount: knurled ring, back shell" },
    Info { kind: Kind::AmmoBox, name: "ammo_box", styles: &["can", "crate"], size: [0.31, 0.19, 0.16], between_points: false, real_size: true, about: "ammo can (lid, latch, handle, ribs, stencil band) or crate" },
    Info { kind: Kind::JerryCan, name: "jerry_can", styles: &["fuel", "water"], size: [0.345, 0.47, 0.165], between_points: false, real_size: true, about: "jerry can: X-pressed sides, three handles, spout, seam" },
    Info { kind: Kind::Strap, name: "strap", styles: &["buckle", "ratchet"], size: [0.0, 0.0, 0.0], between_points: true, real_size: false, about: "webbing strap along points (lying on the surface) with a buckle; radius = half width" },
    Info { kind: Kind::Net, name: "net", styles: &["cargo", "camo"], size: [1.0, 0.08, 0.8], between_points: false, real_size: false, about: "cargo net of knotted ropes, draped size[1] high; count = cells across" },
    Info { kind: Kind::Stencil, name: "stencil", styles: &["plate", "raised"], size: [0.3, 0.008, 0.1], between_points: false, real_size: false, about: "small plate with raised letters (text: A-Z 0-9 - / . :)" },
    Info { kind: Kind::Seam, name: "seam", styles: &["rect", "line"], size: [0.5, 0.003, 0.4], between_points: false, real_size: false, about: "engraved-looking panel line: a rectangle outline or one line along X; radius = line width" },
    Info { kind: Kind::Greeble, name: "greeble", styles: &["box", "canister", "pipes", "junction", "tank"], size: [0.2, 0.1, 0.15], between_points: false, real_size: false, about: "small machinery: lidded box, canister, pipe run, junction box with glands, tank" },
    Info { kind: Kind::Clip, name: "clip", styles: &["p_clip", "saddle"], size: [0.04, 0.035, 0.06], between_points: false, real_size: false, about: "cable P-clip with a bolted tab" },
];

impl Kind {
    pub fn info(self) -> &'static Info {
        CATALOG.iter().find(|i| i.kind == self).expect("every kind is in the catalog")
    }

    pub fn name(self) -> &'static str {
        self.info().name
    }

    pub fn all_names() -> String {
        CATALOG.iter().map(|i| i.name).collect::<Vec<_>>().join(", ")
    }
}

/// One part and its settings. Unset values take the kind's defaults.
#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
#[serde(deny_unknown_fields)]
pub struct Part {
    pub kind: Kind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub style: Option<String>,
    /// [along X, out of the surface (Y), across (Z)] in meters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<[f32; 3]>,
    /// Slats, knuckles, teeth, coils, cells, bolts per side, links.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub count: Option<u32>,
    /// Path of point-to-point parts (piston, cable, hose, tube, pipe,
    /// strap; optional for springs), in the part's space.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub points: Vec<[f32; 3]>,
    /// Tube radius of point-to-point parts; line width of seams.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub radius: Option<f32>,
    /// How far a cable or hose hangs, as a share of its span (0..0.5).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sag: Option<f32>,
    /// Clips along cables and hoses.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clips: Option<u32>,
    /// Connectors / fittings at the ends of cables and hoses (default true).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ends: Option<bool>,
    /// Stencil text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// Lights and sensors: false gives a dark lens.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lit: Option<bool>,
    /// Lens glow color (sRGB).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub glow: Option<[f32; 3]>,
    /// The paint role's material (the surface the part sits on) and color.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub paint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<[f32; 3]>,
    /// Other roles' materials by role name (metal, dark, rubber, chrome,
    /// glow, glass, brass, seam, marking, strap).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub materials: BTreeMap<String, String>,
    /// Segment density, 0.25..3 (1 = default).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<f32>,
    #[serde(default)]
    pub seed: u64,
    /// Two-bone parts (piston, cable, hose, tube, pipe): the first point
    /// rides `bones[0]`, the last `bones[1]` of the rig the part hangs on,
    /// and the part is solved again for every pose (see `span.rs`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub span: Option<Span>,
}

/// The two bones a two-bone part runs between.
#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
#[serde(deny_unknown_fields)]
pub struct Span {
    pub bones: [String; 2],
    /// Set on a posed copy only (never saved): the rest length a piston's
    /// cylinder and rod keep.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rest_length: Option<f32>,
    /// Set on export copies only: a piston's cylinder (0) or rod (1) alone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub half: Option<u8>,
}

impl Part {
    pub fn new(kind: Kind) -> Part {
        Part { kind, style: None, size: None, count: None, points: Vec::new(), radius: None, sag: None, clips: None, ends: None, text: None, lit: None, glow: None, paint: None, color: None, materials: BTreeMap::new(), detail: None, seed: 0, span: None }
    }

    pub fn style(&self) -> &str {
        self.style.as_deref().unwrap_or(self.kind.info().styles[0])
    }

    /// The size, the kind's default when unset.
    pub fn dims(&self) -> Vec3 {
        Vec3::from(self.size.unwrap_or(self.kind.info().size))
    }

    /// Tube radius of point-to-point parts (default per kind).
    pub fn tube_radius(&self) -> f32 {
        self.radius.unwrap_or(match self.kind {
            Kind::Piston => 0.07,
            Kind::Cable => 0.016,
            Kind::Hose => 0.035,
            Kind::CorrugatedTube => 0.07,
            Kind::Pipe => 0.045,
            Kind::Strap => 0.03,
            Kind::Seam => 0.006,
            _ => 0.02,
        })
    }

    /// Checks free-text settings and that point parts have points.
    pub fn validate(&self) -> Result<(), String> {
        let info = self.kind.info();
        if let Some(s) = &self.style
            && !info.styles.contains(&s.as_str())
        {
            return Err(format!("{} style must be one of {:?}", info.name, info.styles));
        }
        if info.between_points && self.points.len() < 2 {
            return Err(format!("{} runs between points: give at least 2 (from/to or points)", info.name));
        }
        if let Some(r) = &self.materials.keys().find(|k| !Role::ALL.iter().any(|r| r.name() == k.as_str())) {
            return Err(format!("unknown material role {r}; roles: {}", Role::ALL.map(Role::name).join(", ")));
        }
        Ok(())
    }

    /// Clamps settings so no call can ask for an endless mesh.
    pub fn sanitize(&mut self) {
        let fin = |v: f32, lo: f32, hi: f32, d: f32| if v.is_finite() { v.clamp(lo, hi) } else { d };
        if let Some(s) = &mut self.size {
            *s = s.map(|v| fin(v, 0.0005, 50.0, 0.05));
        }
        self.count = self.count.map(|c| c.clamp(1, 64));
        self.radius = self.radius.map(|r| fin(r, 0.0002, 5.0, 0.02));
        self.sag = self.sag.map(|s| fin(s, 0.0, 0.5, 0.0));
        self.clips = self.clips.map(|c| c.min(32));
        self.detail = self.detail.map(|d| fin(d, 0.25, 3.0, 1.0));
        self.points.truncate(64);
        for p in &mut self.points {
            *p = p.map(|v| fin(v, -1000.0, 1000.0, 0.0));
        }
        if let Some(t) = &self.text {
            self.text = Some(t.chars().take(24).collect::<String>().to_uppercase());
        }
        for c in [&mut self.color, &mut self.glow].into_iter().flatten() {
            *c = c.map(|v| fin(v, 0.0, 1.0, 0.5));
        }
        if let Some(n) = &self.paint {
            self.paint = Some(n.chars().take(64).collect());
        }
    }

    /// Footprint on the surface: half extents along X and Z and the
    /// height, for placement (point parts have none).
    pub fn footprint(&self) -> Option<(f32, f32, f32)> {
        if self.kind.info().between_points && self.kind != Kind::Spring {
            return None;
        }
        let s = self.dims();
        Some(match self.kind {
            Kind::Bolt | Kind::Nut | Kind::Washer => (s.x * 0.62, s.z * 0.62, s.y),
            Kind::Light | Kind::Sensor | Kind::Connector | Kind::Exhaust if self.style() != "rect" && self.style() != "port" => (s.x * 0.65, s.z * 0.65, s.y),
            Kind::Antenna => (s.x * 1.2, s.z * 1.2, s.y),
            _ => (s.x * 0.5, s.z * 0.5, s.y),
        })
    }

    /// Each role's material (scene library first, then built-ins and
    /// presets) and color.
    pub fn roles(&self, library: &[Material]) -> Vec<(Material, [f32; 3])> {
        Role::ALL.iter().map(|&r| {
            let name = self.materials.get(r.name()).cloned().unwrap_or_else(|| match r {
                Role::Paint => self.paint.clone().unwrap_or_else(|| "kb_paint".into()),
                Role::Glow if self.lit == Some(false) => "kb_glass".into(),
                _ => format!("kb_{}", r.name()),
            });
            let color = match r {
                Role::Paint => self.color.unwrap_or([0.33, 0.35, 0.27]),
                Role::Metal => [0.52, 0.52, 0.53],
                Role::Dark => [0.17, 0.17, 0.18],
                Role::Rubber => [0.045, 0.045, 0.045],
                Role::Chrome => [0.86, 0.86, 0.88],
                Role::Glow => {
                    let g = self.glow.unwrap_or([1.0, 0.78, 0.42]);
                    if self.lit == Some(false) { g.map(|c| c * 0.25) } else { g }
                }
                Role::Glass => [0.12, 0.14, 0.17],
                Role::Brass => [0.74, 0.56, 0.3],
                Role::Seam => [0.03, 0.03, 0.03],
                Role::Marking => [0.86, 0.84, 0.76],
                Role::Strap => [0.24, 0.27, 0.17],
            };
            (part_material(library, &name), color)
        }).collect()
    }

    /// The finished mesh.
    pub fn build(&self, library: &[Material]) -> Mesh {
        let mut p = self.clone();
        p.sanitize();
        let size = p.extent();
        // Wear bands about a bevel wide: thin plates are not all edge.
        let thin = if p.kind.info().between_points { size } else { p.dims().min_element() };
        let wear = (size * 0.04).min(thin * 0.5);
        let mut g = geo::Geo::new(p.roles(library), size, p.detail.unwrap_or(1.0));
        g.band_width = wear * 2.0;
        parts::build(&mut g, &p);
        g.finish(wear)
    }

    /// The part's overall size (for UV scale and wear bands).
    pub fn extent(&self) -> f32 {
        if self.kind.info().between_points && self.kind != Kind::Spring {
            return (self.tube_radius() * 8.0).max(1e-3);
        }
        self.dims().max_element().max(1e-3)
    }
}

/// A material for a part: the scene's or a preset or a built-in `kb_`
/// one. Layers masked by height are left out: one mesh is shared by
/// copies at every height, so its own height means nothing.
pub fn part_material(library: &[Material], name: &str) -> Material {
    let mut m = Material::find(library, name).unwrap_or_else(|| builtin(name));
    let before = m.layers.len();
    m.layers.retain(|l| !l.mask.iter().any(|t| t.source == crate::smart::Source::Height));
    if m.layers.len() != before {
        m.name = format!("{name}_part");
    }
    m
}

/// The kitbash materials (a scene material of the same name wins).
pub fn builtin(name: &str) -> Material {
    let base = Material { name: name.to_string(), ..Material::default() };
    let scratched = |s: f32| (Some("metal_scratched".to_string()), s);
    match name {
        "kb_paint" => {
            let (detail, detail_strength) = scratched(0.3);
            Material { roughness: 0.6, metallic: 0.0, cavity: 0.55, edge_wear: 0.45, dirt: 0.3, variation: 0.2, detail, detail_strength, detail_color: 0.15, ..base }
        }
        "kb_metal" => {
            let (detail, detail_strength) = scratched(0.5);
            Material { roughness: 0.42, metallic: 0.85, cavity: 0.6, edge_wear: 0.5, dirt: 0.3, detail, detail_strength, detail_color: 0.25, ..base }
        }
        "kb_dark" => {
            let (detail, detail_strength) = scratched(0.3);
            Material { roughness: 0.55, metallic: 0.45, cavity: 0.5, edge_wear: 0.35, dirt: 0.25, detail, detail_strength, detail_color: 0.2, ..base }
        }
        "kb_rubber" => Material { roughness: 0.86, cavity: 0.4, dirt: 0.35, ..base },
        "kb_chrome" => Material { roughness: 0.1, metallic: 1.0, ..base },
        "kb_glow" => Material { roughness: 0.08, clearcoat: 1.0, emission: 5.0, ..base },
        "kb_glass" => Material { roughness: 0.04, clearcoat: 1.0, ..base },
        "kb_brass" => Material { roughness: 0.32, metallic: 1.0, edge_wear: 0.3, cavity: 0.4, ..base },
        "kb_seam" => Material { roughness: 0.92, ..base },
        "kb_marking" => Material { roughness: 0.7, cavity: 0.3, ..base },
        "kb_strap" => Material { roughness: 0.92, sheen: 0.25, detail: Some("linen".into()), detail_strength: 0.8, detail_color: 0.4, cavity: 0.4, dirt: 0.4, ..base },
        _ => Material::resolve(&[], name),
    }
}
