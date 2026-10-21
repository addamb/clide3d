//! The auto-detail passes: one call each adds a level of detail.
//!
//! - `panel`: secondary forms on large flat areas. Each flat face is cut
//!   into panels (largest free rectangles, long ones split) that become
//!   raised chamfered plates with seams between them (`style: plates`),
//!   engraved seam lines cut into clay (`engrave`) or seam strips
//!   (`lines`); bolts at the corners and along long seams; a hatch or two.
//! - `bolt`: tertiary fasteners along hard edges and plate borders at a
//!   spacing, set in from the edge on the flat side.
//! - `greeble`: vents, boxes, lights, handles... on chosen areas with a
//!   density and a size range, never overlapping each other, decals,
//!   stencils or parts already there.
//! - `cable`: hoses and cable bundles across joints (a limb's inner side,
//!   behind a hinge), with clips and connectors.
//!
//! Sizes scale with the asset (`unit` = asset size / 6.5 m), so the same
//! call details a mech and a rifle. Everything is seeded.

use std::collections::HashMap;

use glam::{Mat4, Vec2, Vec3};
use serde::Deserialize;
use serde_json::{Value, json};

use super::place::{Placement, frame, on_surface};
use super::surface::{Analysis, Keep, Region, Room, Surface};
use super::{Kind, Part};
use crate::envkit::Rand;
use crate::rig::Bone;

#[derive(Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum Pass {
    Panel,
    Bolt,
    Greeble,
    Cable,
}

/// A limit on where a pass works: a world sphere.
#[derive(Deserialize, Clone, Debug)]
#[serde(deny_unknown_fields)]
pub struct Area {
    pub center: [f32; 3],
    pub radius: f32,
}

/// One cable route: ends as bones (their joint with the parent) or world
/// points.
#[derive(Deserialize, Clone, Debug)]
#[serde(deny_unknown_fields)]
pub struct Route {
    /// A bone: the route crosses its joint (bone head), from its parent's
    /// part to its own.
    #[serde(default)]
    pub joint: Option<String>,
    /// Or explicit world points (snapped to the surface).
    #[serde(default)]
    pub from: Option<[f32; 3]>,
    #[serde(default)]
    pub to: Option<[f32; 3]>,
    /// inner, outer, front or back (default inner).
    #[serde(default)]
    pub side: Option<String>,
    /// hose, cable, bundle, corrugated_tube.
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub radius: Option<f32>,
}

/// A greeble choice: a kind (and style) with a weight.
#[derive(Deserialize, Clone, Debug)]
#[serde(untagged)]
pub enum Choice {
    Name(String),
    Full { kind: Kind, #[serde(default)] style: Option<String>, #[serde(default = "one")] weight: f32 },
}

fn one() -> f32 {
    1.0
}

/// Settings shared by the passes (unset = scale-aware defaults).
#[derive(Deserialize, Clone, Debug)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    pub pass: Pass,
    #[serde(default)]
    pub seed: u64,
    /// Multiplies every automatic size.
    #[serde(default = "one")]
    pub scale: f32,
    /// Greebles per square meter (scaled by unit^2); cables per joint.
    #[serde(default)]
    pub density: Option<f32>,
    #[serde(default)]
    pub count: Option<u32>,
    /// Bolt spacing in meters.
    #[serde(default)]
    pub spacing: Option<f32>,
    /// Greeble size range (meters, longest side); bolt diameter [d].
    #[serde(default)]
    pub size: Option<Vec<f32>>,
    /// Panel: plates (default), engrave, lines. Bolt: bolt style.
    #[serde(default)]
    pub style: Option<String>,
    /// Bolt style for panel and bolt passes.
    #[serde(default)]
    pub bolt: Option<String>,
    #[serde(default)]
    pub parts: Vec<Choice>,
    /// Only faces turned toward this direction (within `max_angle`).
    #[serde(default)]
    pub facing: Option<[f32; 3]>,
    #[serde(default = "default_angle")]
    pub max_angle: f32,
    #[serde(default)]
    pub area: Option<Area>,
    /// Smallest flat face to work on (square meters, scaled).
    #[serde(default)]
    pub min_area: Option<f32>,
    #[serde(default)]
    pub hatches: Option<u32>,
    /// Bolts: "larger" (the bigger face of each edge, default) or "both".
    #[serde(default)]
    pub side: Option<String>,
    /// Bolts along inside corners too.
    #[serde(default)]
    pub concave: bool,
    /// Panel: share of each flat face to cover (default 0.6, leaving room
    /// for greebles).
    #[serde(default)]
    pub coverage: Option<f32>,
    /// Plate thickness (meters).
    #[serde(default)]
    pub thickness: Option<f32>,
    #[serde(default)]
    pub routes: Vec<Route>,
    /// Paint for every part instead of the surface's (material, sRGB color).
    #[serde(default)]
    pub paint: Option<String>,
    #[serde(default)]
    pub color: Option<[f32; 3]>,
    /// Role materials for every part (metal, rubber...).
    #[serde(default)]
    pub materials: std::collections::BTreeMap<String, String>,
}

fn default_angle() -> f32 {
    75.0
}

/// What a pass needs to know about the targets.
pub struct Ctx<'a> {
    pub surface: &'a Surface,
    pub analysis: &'a Analysis,
    /// Owner indices (into `surface.owners`) that are targets.
    pub targets: Vec<u32>,
    /// Asset size / reference size.
    pub unit: f32,
    /// World discs to keep clear (decals, parts already placed).
    pub keep_clear: Vec<Keep>,
    /// The rig the targets ride (bones in rig space, rig -> world) and the
    /// bone each owner rides.
    pub rig: Option<(Vec<Bone>, Mat4)>,
    pub owner_bone: HashMap<u64, String>,
    /// Plane of symmetry (world x).
    pub mirror_x: f32,
}

/// What a pass made.
#[derive(Default)]
pub struct Out {
    pub placements: Vec<Placement>,
    /// Engraved seams: (clay object, band blob JSON in world space points).
    pub engraves: Vec<(u64, Vec<Vec3>, Vec3, f32, f32)>,
    pub notes: serde_json::Map<String, Value>,
}

/// Most placements one pass makes (a scatter holds at most 20 000 copies).
pub const MAX_PLACEMENTS: usize = 20_000;

impl Settings {
    /// Checks numbers and clamps them so no call asks for millions of parts.
    pub fn sanitize(&mut self, unit: f32) -> Result<(), String> {
        let fin = |v: f32, name: &str| if v.is_finite() { Ok(v) } else { Err(format!("{name} must be finite")) };
        self.scale = fin(self.scale, "scale")?.clamp(0.05, 20.0);
        self.density = self.density.map(|d| fin(d, "density").map(|d| d.clamp(0.0, 200.0))).transpose()?;
        self.max_angle = fin(self.max_angle, "max_angle")?.clamp(0.0, 180.0);
        // Nothing smaller than a millimeter per meter of asset.
        let small = 0.001 * unit * REFERENCE_SIZE_HINT;
        self.spacing = self.spacing.map(|v| fin(v, "spacing").map(|v| v.max(small * 4.0))).transpose()?;
        self.thickness = self.thickness.map(|v| fin(v, "thickness").map(|v| v.clamp(small * 0.5, 10.0))).transpose()?;
        if let Some(size) = &mut self.size {
            for v in size.iter_mut() {
                *v = fin(*v, "size")?.clamp(small, 50.0);
            }
        }
        self.min_area = self.min_area.map(|v| fin(v, "min_area").map(|v| v.max(0.0))).transpose()?;
        self.coverage = self.coverage.map(|v| fin(v, "coverage")).transpose()?;
        self.hatches = self.hatches.map(|h| h.min(64));
        self.count = self.count.map(|c| c.min(MAX_PLACEMENTS as u32));
        self.routes.truncate(64);
        self.parts.truncate(64);
        if let Some(a) = &self.area
            && !(Vec3::from(a.center).is_finite() && a.radius.is_finite() && a.radius > 0.0)
        {
            return Err("area needs a finite center and a positive radius".into());
        }
        Ok(())
    }
}

/// The asset size the unit is measured against.
const REFERENCE_SIZE_HINT: f32 = super::REFERENCE_SIZE;

pub fn run(ctx: &Ctx, s: &Settings) -> Out {
    let mut out = match s.pass {
        Pass::Panel => panel(ctx, s),
        Pass::Bolt => bolts(ctx, s),
        Pass::Greeble => greebles(ctx, s),
        Pass::Cable => cables(ctx, s),
    };
    if out.placements.len() > MAX_PLACEMENTS {
        out.notes.insert("capped_at".into(), json!(MAX_PLACEMENTS));
        out.placements.truncate(MAX_PLACEMENTS);
    }
    out
}

impl Ctx<'_> {
    fn eligible(&self, r: &Region, s: &Settings, min_area: f32) -> bool {
        if !self.targets.contains(&r.owner) || r.area < min_area {
            return false;
        }
        // Faces turned to the ground are not seen: skip them unless asked.
        if s.facing.is_none() && r.normal.y < -0.6 {
            return false;
        }
        if let Some(f) = s.facing
            && r.normal.dot(Vec3::from(f).normalize_or(Vec3::Y)) < s.max_angle.to_radians().cos()
        {
            return false;
        }
        if let Some(a) = &s.area
            && Vec3::from(a.center).distance(r.center) > a.radius + r.size().length() * 0.5
        {
            return false;
        }
        true
    }

    fn in_area(&self, p: Vec3, s: &Settings) -> bool {
        s.area.as_ref().is_none_or(|a| Vec3::from(a.center).distance(p) <= a.radius)
    }

    /// The part painted like the region it sits on, with the call's roles.
    fn dress(&self, mut part: Part, region: &Region, s: &Settings, seed: u64) -> Part {
        let o = &self.surface.owners[region.owner as usize];
        let tri = region.tris[0];
        let v = &o.mesh.vertices[o.mesh.indices[self.surface.tri_owner[tri as usize].1 as usize * 3] as usize];
        part.paint = s.paint.clone().or_else(|| o.mesh.materials.get(v.material as usize).map(|m| m.name.clone()));
        part.color = Some(s.color.unwrap_or(region.color));
        part.materials = s.materials.clone();
        part.seed = seed;
        part
    }
}

fn room_cell(unit: f32, r: &Region) -> f32 {
    (0.015 * unit).max(r.size().max_element() / 300.0)
}

// ---------- panel ----------

fn panel(ctx: &Ctx, s: &Settings) -> Out {
    let mut out = Out::default();
    let u = ctx.unit * s.scale;
    let style = s.style.clone().unwrap_or_else(|| "plates".into());
    let t = s.thickness.unwrap_or(0.022 * u);
    let bolt_d = s.size.as_ref().and_then(|v| v.first().copied()).unwrap_or(0.038 * u);
    let bolt_style = s.bolt.clone().unwrap_or_else(|| "hex".into());
    // A ring left round each face for its border bolts.
    let margin = 0.075 * u;
    let gap = 0.014 * u;
    let min_side = 0.22 * u;
    let min_area = s.min_area.unwrap_or(0.12) * u * u;
    let mut rng = Rand::new(s.seed ^ 0x9A4E);
    let mut hatches = s.hatches.unwrap_or(2) as i32;
    let coverage = s.coverage.unwrap_or(0.6).clamp(0.05, 1.0);
    let (mut panels, mut plates, mut seams) = (0, 0, 0);
    let mut order: Vec<usize> = (0..ctx.analysis.regions.len()).filter(|&i| ctx.eligible(&ctx.analysis.regions[i], s, min_area)).collect();
    order.sort_by(|&a, &b| ctx.analysis.regions[b].area.total_cmp(&ctx.analysis.regions[a].area).then(a.cmp(&b)));
    for ri in order {
        let r = &ctx.analysis.regions[ri];
        if r.size().min_element() < min_side {
            continue;
        }
        let mut room = Room::new(ctx.surface, r, room_cell(u, r), &ctx.keep_clear);
        let mut rects = Vec::new();
        let mut covered = 0.0;
        for _ in 0..6 {
            if covered > coverage * r.area {
                break;
            }
            let Some((lo, hi)) = room.largest_rect(margin) else { break };
            let sz = hi - lo;
            if sz.min_element() < min_side || sz.x * sz.y < min_side * min_side * 1.5 {
                break;
            }
            rects.push((lo, hi));
            covered += sz.x * sz.y;
            room.take(lo, hi);
        }
        for (lo, hi) in rects {
            let sz = hi - lo;
            let along_u = sz.x >= sz.y;
            let (long, short) = if along_u { (sz.x, sz.y) } else { (sz.y, sz.x) };
            let k = ((long / (short * 1.25)).round() as usize).clamp(1, 4);
            // Split points along the long side, jittered.
            let mut cuts = vec![0.0f32];
            for i in 1..k {
                cuts.push((i as f32 + rng.signed() * 0.15) / k as f32);
            }
            cuts.push(1.0);
            for w in cuts.windows(2) {
                let (a, b) = (w[0], w[1]);
                let (plo, phi) = if along_u {
                    (Vec2::new(lo.x + a * sz.x + if a > 0.0 { gap * 0.5 } else { 0.0 }, lo.y), Vec2::new(lo.x + b * sz.x - if b < 1.0 { gap * 0.5 } else { 0.0 }, hi.y))
                } else {
                    (Vec2::new(lo.x, lo.y + a * sz.y + if a > 0.0 { gap * 0.5 } else { 0.0 }), Vec2::new(hi.x, lo.y + b * sz.y - if b < 1.0 { gap * 0.5 } else { 0.0 }))
                };
                let c = (plo + phi) * 0.5;
                let ps = phi - plo;
                if ps.min_element() < min_side * 0.6 {
                    continue;
                }
                let p = r.at_uv(c);
                if !ctx.in_area(p, s) {
                    continue;
                }
                panels += 1;
                let base = frame(p, r.normal, r.u, 1.0);
                let seed = rng.next();
                let hatch = hatches > 0 && ps.max_element() < 1.3 * u && ps.min_element() > 0.3 * u && rng.chance(0.45);
                match style.as_str() {
                    "engrave" | "lines" => {
                        let owner = ctx.surface.owners[r.owner as usize].id;
                        let is_clay = style == "engrave";
                        if is_clay {
                            let corners = [Vec2::new(plo.x, plo.y), Vec2::new(phi.x, plo.y), Vec2::new(phi.x, phi.y), Vec2::new(plo.x, phi.y), Vec2::new(plo.x, plo.y)];
                            out.engraves.push((owner, corners.iter().map(|q| r.at_uv(*q)).collect(), r.normal, (0.01 * u).max(1e-4), (0.006 * u).max(1e-4)));
                        } else {
                            let seam = Part { size: Some([ps.x, 0.0015 * u, ps.y]), radius: Some(0.008 * u), ..ctx.dress(Part::new(Kind::Seam), r, s, seed) };
                            out.placements.push(Placement { part: seam, frame: base, owner, bone: bone_at_point(ctx, p, r), sink: 0.0, priority: 2 });
                        }
                        seams += 1;
                        corner_bolts(ctx, s, r, &mut out, base, ps, 0.0, bolt_d, &bolt_style, seed, 0);
                    }
                    _ => {
                        let owner = ctx.surface.owners[r.owner as usize].id;
                        if hatch {
                            hatches -= 1;
                            let st = ["handle", "latch", "plain", "vented"][(rng.f() * 4.0) as usize % 4];
                            let part = Part { size: Some([ps.x, t * 1.3, ps.y]), style: Some(st.into()), ..ctx.dress(Part::new(Kind::Hatch), r, s, seed) };
                            out.placements.push(Placement { part, frame: base, owner, bone: bone_at_point(ctx, p, r), sink: 0.0, priority: 3 });
                        } else {
                            plates += 1;
                            let part = Part { size: Some([ps.x, t, ps.y]), style: Some("plain".into()), ..ctx.dress(Part::new(Kind::ArmorPlate), r, s, seed) };
                            out.placements.push(Placement { part, frame: base, owner, bone: bone_at_point(ctx, p, r), sink: 0.0, priority: 3 });
                            corner_bolts(ctx, s, r, &mut out, base, ps, t, bolt_d, &bolt_style, seed, 1);
                        }
                    }
                }
            }
        }
    }
    out.notes.insert("panels".into(), json!(panels));
    out.notes.insert("plates".into(), json!(plates));
    out.notes.insert("seams".into(), json!(seams));
    out.notes.insert("hatches".into(), json!(s.hatches.unwrap_or(2) as i32 - hatches));
    out
}

/// Bolts at a panel's corners (and along long sides): on top of a plate
/// `top` high, or on the surface.
#[allow(clippy::too_many_arguments)]
fn corner_bolts(ctx: &Ctx, s: &Settings, r: &Region, out: &mut Out, base: Mat4, size: Vec2, top: f32, d: f32, style: &str, seed: u64, bevel_steps: u32) {
    let inset = d * 1.05 + top * 0.5 * bevel_steps as f32 + size.min_element() * 0.02;
    let (hx, hz) = (size.x * 0.5 - inset, size.y * 0.5 - inset);
    if hx <= 0.0 || hz <= 0.0 {
        return;
    }
    let spacing = d * 7.0;
    let mut spots = Vec::new();
    let nx = ((2.0 * hx / spacing).floor() as usize).max(1);
    let nz = ((2.0 * hz / spacing).floor() as usize).max(1);
    for i in 0..=nx {
        let x = -hx + 2.0 * hx * i as f32 / nx as f32;
        spots.push(Vec2::new(x, -hz));
        spots.push(Vec2::new(x, hz));
    }
    for j in 1..nz {
        let z = -hz + 2.0 * hz * j as f32 / nz as f32;
        spots.push(Vec2::new(-hx, z));
        spots.push(Vec2::new(hx, z));
    }
    let owner = ctx.surface.owners[r.owner as usize].id;
    let bolt = Part { size: Some([d, d * 0.5, d]), style: Some(style.into()), ..ctx.dress(Part::new(Kind::Bolt), r, s, 0) };
    let _ = seed;
    for q in spots {
        let f = base * Mat4::from_translation(Vec3::new(q.x, top, q.y));
        let p = f.transform_point3(Vec3::ZERO);
        out.placements.push(Placement { part: bolt.clone(), frame: f, owner, bone: bone_at_point(ctx, p, r), sink: 0.0, priority: 1 });
    }
}

/// The bone at a point on a region (rigged owners).
fn bone_at_point(ctx: &Ctx, p: Vec3, r: &Region) -> Option<String> {
    let h = ctx.surface.snap(p, r.normal, ctx.unit * 0.2 + 1e-3)?;
    ctx.surface.bone_at(&h)
}

// ---------- bolts ----------

fn bolts(ctx: &Ctx, s: &Settings) -> Out {
    let mut out = Out::default();
    let u = ctx.unit * s.scale;
    let d = s.size.as_ref().and_then(|v| v.first().copied()).unwrap_or(0.04 * u);
    let spacing = s.spacing.unwrap_or(0.24 * u).max(d * 1.5);
    let style = s.bolt.clone().or_else(|| s.style.clone()).unwrap_or_else(|| "hex".into());
    let both = s.side.as_deref() == Some("both");
    let min_area = s.min_area.unwrap_or(0.02) * u * u;
    let inset = d * 1.05 + 0.006 * u;
    let mut rooms: HashMap<usize, Room> = HashMap::new();
    let mut taken: HashMap<(i32, i32, i32), Vec<Vec3>> = HashMap::new();
    let cell = spacing * 0.7;
    let key = |p: Vec3| ((p.x / cell).floor() as i32, (p.y / cell).floor() as i32, (p.z / cell).floor() as i32);
    let mut skipped = HashMap::<&str, usize>::new();
    for e in &ctx.analysis.edges {
        if !e.convex && !s.concave {
            continue;
        }
        let len = e.a.distance(e.b);
        if len < inset * 2.0 + d {
            continue;
        }
        let dir = (e.b - e.a) / len;
        let sides: Vec<usize> = {
            let [r0, r1] = e.sides;
            let ok = |r: usize| ctx.eligible(&ctx.analysis.regions[r], s, min_area);
            let mut v: Vec<usize> = [r0, r1].into_iter().filter(|&r| ok(r)).collect();
            if !both && v.len() == 2 {
                let big = if ctx.analysis.regions[r0].area >= ctx.analysis.regions[r1].area { r0 } else { r1 };
                v = vec![big];
            }
            v
        };
        for ri in sides {
            let r = &ctx.analysis.regions[ri];
            let mut w = r.normal.cross(dir).normalize_or(Vec3::ZERO);
            let mid = (e.a + e.b) * 0.5;
            if w.dot(r.center - mid) < 0.0 {
                w = -w;
            }
            if out.placements.len() >= MAX_PLACEMENTS {
                break;
            }
            let n = (((len - 2.0 * inset) / spacing).floor() as usize + 1).min(MAX_PLACEMENTS);
            let start = (len - (n - 1) as f32 * spacing) * 0.5;
            let room = rooms.entry(ri).or_insert_with(|| Room::new(ctx.surface, r, room_cell(u, r).min(d * 0.6), &ctx.keep_clear));
            for i in 0..n {
                let p = e.a + dir * (start + i as f32 * spacing) + w * inset;
                if !ctx.in_area(p, s) {
                    continue;
                }
                if room.at(r.to_uv(p)) < d * 0.55 {
                    *skipped.entry("no_room").or_default() += 1;
                    continue;
                }
                let k = key(p);
                let near = (-1..=1).any(|a| (-1..=1).any(|b| (-1..=1).any(|c| taken.get(&(k.0 + a, k.1 + b, k.2 + c)).is_some_and(|v| v.iter().any(|q| q.distance(p) < spacing * 0.7)))));
                if near {
                    *skipped.entry("crowded").or_default() += 1;
                    continue;
                }
                let Some(h) = ctx.surface.snap(p, r.normal, inset * 2.0) else { continue };
                if h.face.dot(r.normal) < 0.97 || ctx.analysis.region_of[h.tri as usize] != ri {
                    *skipped.entry("off_face").or_default() += 1;
                    continue;
                }
                let part = Part { size: Some([d, d * 0.5, d]), style: Some(style.clone()), ..ctx.dress(Part::new(Kind::Bolt), r, s, 0) };
                if let Ok(pl) = on_surface(ctx.surface, &h, part, dir, 0.3) {
                    taken.entry(k).or_default().push(p);
                    out.placements.push(pl);
                }
            }
        }
    }
    // Plate borders: around each flat face, set in from its border (rounded
    // edges have no hard edge to follow).
    let mut border = 0;
    let mut order: Vec<usize> = (0..ctx.analysis.regions.len()).filter(|&i| ctx.eligible(&ctx.analysis.regions[i], s, min_area)).collect();
    order.sort_by(|&a, &b| ctx.analysis.regions[b].area.total_cmp(&ctx.analysis.regions[a].area).then(a.cmp(&b)));
    for ri in order {
        let r = &ctx.analysis.regions[ri];
        if r.size().min_element() < spacing * 1.5 {
            continue;
        }
        // The face's own outline (nothing kept clear), then room per spot.
        let shape = Room::new(ctx.surface, r, room_cell(u, r).min(d * 0.6), &[]);
        let room = rooms.entry(ri).or_insert_with(|| Room::new(ctx.surface, r, room_cell(u, r).min(d * 0.6), &ctx.keep_clear));
        let Some((lo, hi)) = shape.largest_rect(inset) else { continue };
        let sz = hi - lo;
        if sz.min_element() < spacing {
            continue;
        }
        let (lo, hi) = (lo + Vec2::splat(d * 0.6), hi - Vec2::splat(d * 0.6));
        let sz = hi - lo;
        if out.placements.len() >= MAX_PLACEMENTS {
            break;
        }
        let (nx, ny) = (((sz.x / spacing).round() as usize).clamp(1, 2000), ((sz.y / spacing).round() as usize).clamp(1, 2000));
        let mut spots = Vec::new();
        for i in 0..=nx {
            let x = lo.x + sz.x * i as f32 / nx as f32;
            spots.push((Vec2::new(x, lo.y), r.u));
            spots.push((Vec2::new(x, hi.y), r.u));
        }
        for j in 1..ny {
            let y = lo.y + sz.y * j as f32 / ny as f32;
            spots.push((Vec2::new(lo.x, y), r.v));
            spots.push((Vec2::new(hi.x, y), r.v));
        }
        for (q, along) in spots {
            let p = r.at_uv(q);
            if !ctx.in_area(p, s) || room.at(q) < d * 0.55 {
                continue;
            }
            let k = key(p);
            let near = (-1..=1).any(|a| (-1..=1).any(|b| (-1..=1).any(|c| taken.get(&(k.0 + a, k.1 + b, k.2 + c)).is_some_and(|v| v.iter().any(|q| q.distance(p) < spacing * 0.7)))));
            if near {
                continue;
            }
            let Some(h) = ctx.surface.snap(p, r.normal, inset * 2.0) else { continue };
            if h.face.dot(r.normal) < 0.97 {
                continue;
            }
            let part = Part { size: Some([d, d * 0.5, d]), style: Some(style.clone()), ..ctx.dress(Part::new(Kind::Bolt), r, s, 0) };
            if let Ok(pl) = on_surface(ctx.surface, &h, part, along, 0.3) {
                taken.entry(k).or_default().push(p);
                out.placements.push(pl);
                border += 1;
            }
        }
    }
    out.notes.insert("along_borders".into(), json!(border));
    out.notes.insert("edges".into(), json!(ctx.analysis.edges.len()));
    out.notes.insert("spacing".into(), json!(spacing));
    out.notes.insert("bolt_size".into(), json!(d));
    out.notes.insert("skipped".into(), json!(skipped));
    out
}

// ---------- greebles ----------

fn choices(s: &Settings) -> Vec<(Kind, Option<String>, f32)> {
    if s.parts.is_empty() {
        return vec![
            (Kind::Vent, Some("louver".into()), 2.0),
            (Kind::Vent, Some("slots".into()), 1.0),
            (Kind::Vent, Some("grille".into()), 0.7),
            (Kind::Greeble, Some("box".into()), 1.5),
            (Kind::Greeble, Some("junction".into()), 1.2),
            (Kind::Greeble, Some("canister".into()), 0.6),
            (Kind::Light, Some("round".into()), 0.8),
            (Kind::Light, Some("rect".into()), 0.5),
            (Kind::Handle, Some("bar".into()), 1.0),
            (Kind::Connector, None, 0.6),
            (Kind::Latch, None, 0.4),
        ];
    }
    s.parts.iter().filter_map(|c| match c {
        Choice::Name(n) => serde_json::from_value::<Kind>(json!(n)).ok().map(|k| (k, None, 1.0)),
        Choice::Full { kind, style, weight } => Some((*kind, style.clone(), weight.max(0.0))),
    }).filter(|c| !c.0.info().between_points).collect()
}

fn greebles(ctx: &Ctx, s: &Settings) -> Out {
    let mut out = Out::default();
    let u = ctx.unit * s.scale;
    let list = choices(s);
    let range = match s.size.as_deref() {
        Some([a, b, ..]) => (a.min(*b), a.max(*b)),
        Some([a]) => (*a, *a),
        _ => (0.14 * u, 0.5 * u),
    };
    // Parts shrink to the room left (after a panel pass) down to this.
    let smallest = range.0 * 0.5;
    let density = s.density.unwrap_or(1.6) / (u * u);
    let min_area = s.min_area.unwrap_or(0.05) * u * u;
    let regions: Vec<usize> = (0..ctx.analysis.regions.len()).filter(|&i| {
        let r = &ctx.analysis.regions[i];
        ctx.eligible(r, s, min_area) && r.size().min_element() >= smallest * 0.8
    }).collect();
    let area: f32 = regions.iter().map(|&i| ctx.analysis.regions[i].area).sum();
    let wanted = s.count.map_or((area * density).round() as usize, |c| c as usize).min(2000);
    let mut rng = Rand::new(s.seed ^ 0x6EEB);
    let mut accepted: Vec<(Vec3, f32)> = Vec::new();
    let mut rooms: HashMap<usize, Room> = HashMap::new();
    let mut tries = 0;
    // Per kind (name/style): placed, and why copies were skipped.
    let label = |c: &(Kind, Option<String>, f32)| match &c.1 { Some(st) => format!("{}/{st}", c.0.info().name), None => c.0.info().name.to_string() };
    let mut placed_k = vec![0usize; list.len()];
    let mut fails_k = vec![0usize; list.len()];
    let mut why_k: Vec<std::collections::BTreeMap<&'static str, usize>> = vec![Default::default(); list.len()];
    let mut shrunk = 0usize;
    // A kind that keeps failing gives up (its room is gone), so the
    // others still get their turns.
    let give_up = 60;
    if regions.is_empty() || wanted == 0 || list.is_empty() {
        out.notes.insert("placed".into(), json!(0));
        out.notes.insert("wanted".into(), json!(wanted));
        return out;
    }
    while out.placements.len() < wanted && tries < wanted * 60 + 50 {
        tries += 1;
        // Kinds take turns by weight: the one furthest behind its share.
        let Some(k) = (0..list.len()).filter(|&i| list[i].2 > 0.0 && fails_k[i] < give_up).min_by(|&a, &b| {
            let share = |i: usize| (placed_k[i] as f32 + 1.0) / list[i].2;
            share(a).total_cmp(&share(b))
        }) else { break };
        let choice = &list[k];
        // A region by area.
        let mut pick = rng.f() * area;
        let mut ri = regions[0];
        for &i in &regions {
            ri = i;
            if pick < ctx.analysis.regions[i].area {
                break;
            }
            pick -= ctx.analysis.regions[i].area;
        }
        let r = &ctx.analysis.regions[ri];
        let mut part = Part { style: choice.1.clone(), ..Part::new(choice.0) };
        let info = choice.0.info();
        let base = Vec3::from(info.size);
        let dims = if info.real_size {
            base
        } else {
            let long = rng.range(range.0, range.1);
            base * (long / base.x.max(base.z).max(1e-4))
        };
        part.size = Some(dims.to_array());
        let part = ctx.dress(part, r, s, rng.next() % 4);
        let (hx, hz, _) = part.footprint().unwrap_or((dims.x * 0.5, dims.z * 0.5, dims.y));
        let quarter = if matches!(choice.0, Kind::Handle | Kind::Latch | Kind::Light) || rng.chance(0.5) { 0.0 } else { std::f32::consts::FRAC_PI_2 };
        let margin = 0.012 * u;
        let room = rooms.entry(ri).or_insert_with(|| Room::new(ctx.surface, r, room_cell(u, r), &ctx.keep_clear));
        // A spot with room: a few tries for a cell with clearance.
        let min_half = smallest * 0.5 * (hx.min(hz) / hx.max(hz).max(1e-6));
        let mut spot = None;
        for _ in 0..6 {
            let c = Vec2::new(rng.range(r.lo.x, r.hi.x), rng.range(r.lo.y, r.hi.y));
            if room.at(c) >= min_half + margin {
                spot = Some(c);
                break;
            }
        }
        let Some(c) = spot else {
            *why_k[k].entry("no_room").or_default() += 1;
            fails_k[k] += 1;
            continue;
        };
        let p = r.at_uv(c);
        if !ctx.in_area(p, s) {
            continue;
        }
        // Fit the size to the room there (never below the smallest size).
        let mut scale = ((room.at(c) - margin) / hx.max(hz).max(1e-6)).min(1.0);
        let mut fitted = false;
        while hx.max(hz) * 2.0 * scale >= smallest * 0.999 && !info.real_size {
            if room.fits(c, Vec2::new(hx, hz) * scale, quarter, margin) {
                fitted = true;
                break;
            }
            scale *= 0.85;
        }
        if info.real_size {
            scale = 1.0;
            fitted = room.fits(c, Vec2::new(hx, hz), quarter, margin);
        }
        if !fitted {
            *why_k[k].entry("no_room").or_default() += 1;
            fails_k[k] += 1;
            continue;
        }
        let part = if scale < 0.999 {
            shrunk += 1;
            let mut q = part.clone();
            q.size = Some((dims * scale).to_array());
            q
        } else {
            part
        };
        let (hx, hz) = (hx * scale, hz * scale);
        let rad = hx.hypot(hz);
        if accepted.iter().any(|(q, qr)| q.distance(p) < rad + qr + 0.02 * u) {
            *why_k[k].entry("crowded").or_default() += 1;
            fails_k[k] += 1;
            continue;
        }
        let Some(h) = ctx.surface.snap(p, r.normal, rad) else { continue };
        let along = if quarter == 0.0 { r.u } else { r.v };
        let Ok(pl) = on_surface(ctx.surface, &h, part, along, 0.15) else {
            *why_k[k].entry("curved").or_default() += 1;
            fails_k[k] += 1;
            continue;
        };
        let ext = if quarter == 0.0 { Vec2::new(hx, hz) } else { Vec2::new(hz, hx) };
        room.take(c - ext, c + ext);
        accepted.push((p, rad));
        placed_k[k] += 1;
        fails_k[k] = 0;
        out.placements.push(Placement { priority: 2, ..pl });
    }
    let mut kinds = serde_json::Map::new();
    let mut totals: std::collections::BTreeMap<&str, usize> = Default::default();
    for (i, c) in list.iter().enumerate() {
        let mut v = json!({ "placed": placed_k[i] });
        for (why, n) in &why_k[i] {
            v[*why] = json!(n);
            *totals.entry(why).or_default() += n;
        }
        if fails_k[i] >= give_up {
            v["gave_up"] = json!("no room left for it after many tries");
        }
        kinds.insert(label(c), v);
    }
    out.notes.insert("wanted".into(), json!(wanted));
    out.notes.insert("placed".into(), json!(out.placements.len()));
    out.notes.insert("kinds".into(), Value::Object(kinds));
    out.notes.insert("skipped".into(), json!(totals));
    out.notes.insert("shrunk_to_fit".into(), json!(shrunk));
    out.notes.insert("size_range".into(), json!([range.0, range.1]));
    out
}

// ---------- cables ----------

fn cables(ctx: &Ctx, s: &Settings) -> Out {
    let mut out = Out::default();
    let u = ctx.unit * s.scale;
    let mut made = Vec::new();
    let mut failed = Vec::new();
    let per = s.count.unwrap_or(2).clamp(1, 6) as usize;
    let routes: Vec<Route> = if s.routes.is_empty() { auto_routes(ctx) } else { s.routes.clone() };
    for (ri, route) in routes.iter().enumerate() {
        let side = side_dir(ctx, route, s);
        let ends = match route_ends(ctx, route, side, u) {
            Ok(e) => e,
            Err(why) => {
                failed.push(json!({ "route": ri, "why": why }));
                continue;
            }
        };
        let (a, b, joint, mid_hit) = ends;
        let lateral = (b.0 - a.0).cross(side).normalize_or(side.any_orthonormal_vector());
        for k in 0..per {
            let kind = route.kind.clone().unwrap_or_else(|| ["hose", "bundle", "cable", "hose", "cable", "bundle"][k % 6].to_string());
            let (pk, style, r) = match kind.as_str() {
                "cable" => (Kind::Cable, "single", 0.012 * u),
                "bundle" => (Kind::Cable, "bundle", 0.02 * u),
                "corrugated_tube" => (Kind::CorrugatedTube, "ribbed", 0.05 * u),
                _ => (Kind::Hose, "corrugated", 0.028 * u),
            };
            let r = route.radius.unwrap_or(r);
            let shift = (k as f32 - (per as f32 - 1.0) * 0.5) * (0.075 * u);
            let a_hit = ctx.surface.snap(a.0 + lateral * shift, a.1, 0.15 * u).map_or(a, |h| (h.p, h.n, h.tri));
            let b_hit = ctx.surface.snap(b.0 + lateral * shift, b.1, 0.15 * u).map_or(b, |h| (h.p, h.n, h.tri));
            let lift = 1.6 * r + 0.006 * u;
            let mid = mid_hit + side * (2.0 * r + 0.04 * u + k as f32 * 0.6 * r) + lateral * shift;
            let mut pts = vec![a_hit.0 - a_hit.1 * r * 0.5, a_hit.0 + a_hit.1 * lift];
            // Clips: on the surface a little way toward the joint.
            let mut clips = Vec::new();
            for (h, toward) in [(a_hit, joint), (b_hit, joint)] {
                let dirv = toward - h.0;
                let along = (dirv - h.1 * dirv.dot(h.1)).normalize_or(Vec3::ZERO);
                let q = h.0 + along * (0.12 * u);
                if let Some(ch) = ctx.surface.snap(q, h.1, 0.1 * u)
                    && ch.n.dot(h.1) > 0.8
                {
                    clips.push((ch, along));
                }
            }
            if let Some((ch, _)) = clips.first() {
                pts.push(ch.p + ch.n * (r * 1.05 + r * 0.16));
            }
            pts.push(mid);
            if let Some((ch, _)) = clips.get(1) {
                pts.push(ch.p + ch.n * (r * 1.05 + r * 0.16));
            }
            pts.push(b_hit.0 + b_hit.1 * lift);
            pts.push(b_hit.0 - b_hit.1 * r * 0.5);
            let owner = ctx.surface.owner_of(a_hit.2);
            let base = Part { kind: pk, style: Some(style.into()), radius: Some(r), sag: Some(0.03), clips: Some(if pk == Kind::Cable && style == "bundle" { 0 } else { 1 }), points: pts.iter().map(|p| p.to_array()).collect(), materials: s.materials.clone(), seed: s.seed.wrapping_add(k as u64), ..Part::new(pk) };
            let bone = ctx.owner_bone.get(&owner.id).cloned().or_else(|| {
                let h = ctx.surface.snap(a_hit.0, a_hit.1, 0.05 * u)?;
                ctx.surface.bone_at(&h)
            });
            out.placements.push(Placement { part: base, frame: Mat4::IDENTITY, owner: owner.id, bone: bone.clone(), sink: 0.0, priority: 2 });
            for (ch, along) in clips {
                let o = ctx.surface.owner_of(ch.tri).id;
                let clip = Part { size: Some([r * 1.6, r * 2.1, r * 3.0]), materials: s.materials.clone(), ..Part::new(Kind::Clip) };
                let f = frame(ch.p, ch.n, along, 1.0);
                out.placements.push(Placement { part: clip, frame: f, owner: o, bone: ctx.owner_bone.get(&o).cloned().or_else(|| ctx.surface.bone_at(&ch)), sink: 0.0, priority: 1 });
            }
            made.push(json!({ "route": ri, "kind": kind, "radius": r }));
        }
    }
    out.notes.insert("routes".into(), json!(routes.len()));
    out.notes.insert("made".into(), json!(made));
    if !failed.is_empty() {
        out.notes.insert("failed".into(), json!(failed));
    }
    out
}

/// Joints between target parts: a bone whose parent bone another target
/// rides (thigh -> shin: the knee).
fn auto_routes(ctx: &Ctx) -> Vec<Route> {
    let Some((bones, _)) = &ctx.rig else { return Vec::new() };
    let ridden: Vec<&String> = ctx.targets.iter().filter_map(|&i| ctx.owner_bone.get(&ctx.surface.owners[i as usize].id)).collect();
    let mut out = Vec::new();
    for b in bones {
        if ridden.contains(&&b.name) && b.parent.as_ref().is_some_and(|p| ridden.contains(&p)) {
            out.push(Route { joint: Some(b.name.clone()), from: None, to: None, side: None, kind: None, radius: None });
        }
    }
    out
}

fn side_dir(ctx: &Ctx, route: &Route, _s: &Settings) -> Vec3 {
    let joint = route.joint.as_ref().and_then(|j| joint_point(ctx, j)).or_else(|| route.from.zip(route.to).map(|(a, b)| (Vec3::from(a) + Vec3::from(b)) * 0.5)).unwrap_or(Vec3::ZERO);
    let inward = Vec3::X * (ctx.mirror_x - joint.x).signum();
    let centered = (joint.x - ctx.mirror_x).abs() < 0.1 * ctx.unit;
    match route.side.as_deref() {
        Some("outer") => if centered { Vec3::Z } else { -inward },
        Some("front") => Vec3::Z,
        Some("back") => Vec3::NEG_Z,
        _ => if centered { Vec3::NEG_Z } else { inward },
    }
}

/// A joint's world position (the bone's head).
fn joint_point(ctx: &Ctx, name: &str) -> Option<Vec3> {
    let (bones, world) = ctx.rig.as_ref()?;
    bones.iter().find(|b| b.name == name).map(|b| world.transform_point3(b.head))
}

type End = (Vec3, Vec3, u32);

/// Where a route starts and ends on the surface, the joint, and the
/// outermost surface point over the joint on the cable's side.
fn route_ends(ctx: &Ctx, route: &Route, side: Vec3, u: f32) -> Result<(End, End, Vec3, Vec3), String> {
    let far = ctx.surface.diag.max(1.0);
    let hit_toward = |p: Vec3, owner: Option<u64>| -> Option<End> {
        let from = p + side * far;
        // March inward: the first hit on the wanted owner.
        let mut origin = from;
        for _ in 0..8 {
            let h = ctx.surface.ray(origin, -side, far * 2.0)?;
            if owner.is_none_or(|o| ctx.surface.owner_of(h.tri).id == o) {
                return Some((h.p, h.n, h.tri));
            }
            origin = h.p - side * 1e-4;
        }
        None
    };
    if let (Some(a), Some(b)) = (route.from, route.to) {
        let (a, b) = (Vec3::from(a), Vec3::from(b));
        let sa = ctx.surface.snap(a, side, 0.2 * u).ok_or("from is not near the surface")?;
        let sb = ctx.surface.snap(b, side, 0.2 * u).ok_or("to is not near the surface")?;
        let j = (a + b) * 0.5;
        let mid = hit_toward(j, None).map_or(j, |h| h.0);
        return Ok(((sa.p, sa.n, sa.tri), (sb.p, sb.n, sb.tri), j, mid));
    }
    let name = route.joint.as_ref().ok_or("a route needs joint, or from and to")?;
    let (bones, world) = ctx.rig.as_ref().ok_or("the targets ride no rig: give from and to")?;
    let b = bones.iter().find(|b| &b.name == name).ok_or_else(|| format!("no bone {name}"))?;
    let parent = b.parent.as_ref().and_then(|p| bones.iter().find(|x| &x.name == p)).ok_or_else(|| format!("{name} has no parent bone"))?;
    let j = world.transform_point3(b.head);
    let up = world.transform_point3(parent.head);
    let down = world.transform_point3(b.tail);
    let owner_of_bone = |bone: &str| ctx.owner_bone.iter().find(|(id, bn)| bn.as_str() == bone && ctx.targets.iter().any(|&t| ctx.surface.owners[t as usize].id == **id)).map(|(id, _)| *id);
    let oa = owner_of_bone(&parent.name);
    let ob = owner_of_bone(&b.name);
    let reach = |toward: Vec3| (toward - j).normalize_or(Vec3::NEG_Y) * ((toward - j).length() * 0.3).min(0.7 * u).max(0.1 * u);
    let a = hit_toward(j + reach(up), oa).ok_or("no surface on the parent side")?;
    let bb = hit_toward(j + reach(down), ob).ok_or("no surface on the child side")?;
    // Over the joint: the outermost of the two parts' surfaces on this side.
    let mid = [oa, ob].into_iter().filter_map(|o| hit_toward(j, o)).map(|h| h.0).max_by(|x, y| x.dot(side).total_cmp(&y.dot(side))).unwrap_or(j + side * 0.3 * u);
    Ok((a, bb, j, mid))
}

/// Points for an engraved seam band in a clay's own space.
pub fn band_blob(name: &str, points: &[Vec3], normal: Vec3, width: f32, depth: f32, to_local: Mat4) -> Option<crate::clay::Blob> {
    let local: Vec<[f32; 3]> = points.iter().map(|p| to_local.transform_point3(*p).to_array()).collect();
    let n = to_local.transform_vector3(normal).normalize_or(Vec3::Z);
    let k = to_local.transform_vector3(Vec3::X).length();
    serde_json::from_value(json!({ "type": "band", "name": name, "points": local, "width": width * k, "normal": n.to_array(), "op": "engrave", "wall": depth * k, "blend": 0.0, "color": [0.12, 0.12, 0.12] })).ok()
}
