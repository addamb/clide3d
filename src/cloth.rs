//! Sewn surfaces and deterministic fixed-step XPBD. All coordinates are in
//! the parent clay's rest space. No distance-field clothing or copied code.
use crate::{
    clay::{Clay, Field},
    material::Material,
    mesh::{Mesh, Textures, Vertex},
    progress::Progress,
    rig::{self, Rig, Skin},
};
use glam::{Mat4, Vec2, Vec3};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::f32::consts::TAU;
use std::sync::Arc;

#[derive(Clone, Serialize, Deserialize, PartialEq, Debug)]
#[serde(default, deny_unknown_fields)]
pub struct Pattern {
    pub pattern: String,
    /// Numeric length in meters or anatomical hem landmark.
    pub length: serde_json::Value,
    pub fit: f32,
    pub flare: f32,
    /// Excess waist circumference gathered into pleats.
    pub gather: f32,
    pub resolution: u32,
    pub thickness: f32,
    pub fabric: String,
    pub bone: Option<String>,
    pub center: Option<Vec3>,
    pub radius: Option<Vec2>,
    pub points: Vec<Vec3>,
    /// Panel corner indices, or additional generated mesh vertex indices.
    pub pins: Vec<u32>,
    pub belt: Option<f32>,
    pub neckline: String,
    pub sleeves: bool,
    /// Loop subdivision of the simulated midsurface before shell/rim generation.
    pub subdivision: u32,
}
impl Default for Pattern {
    fn default() -> Self {
        Self {
            pattern: "tunic".into(),
            length: serde_json::json!("mid_thigh"),
            fit: 0.02,
            flare: 0.3,
            gather: 0.18,
            resolution: 56,
            thickness: 0.002,
            fabric: "cotton".into(),
            bone: None,
            center: None,
            radius: None,
            points: vec![],
            pins: vec![],
            belt: None,
            neckline: "round".into(),
            sleeves: false,
            subdivision: 1,
        }
    }
}
#[derive(Clone, Serialize, Deserialize, PartialEq, Debug)]
pub struct Cloth {
    pub settings: Pattern,
    pub rest: Vec<Vec3>,
    pub positions: Vec<Vec3>,
    pub uv: Vec<Vec2>,
    pub triangles: Vec<[u32; 3]>,
    /// Corner UVs allow welded sleeve nodes to belong to distinct pattern pieces.
    #[serde(default)]
    pub triangle_uv: Vec<[Vec2; 3]>,
    pub pins: Vec<u32>,
    pub joints: Vec<[u16; 4]>,
    pub weights: Vec<[f32; 4]>,
    #[serde(default)]
    pub cache: Vec<CacheFrame>,
    #[serde(default)]
    pub clip: Option<String>,
}
#[derive(Clone, Serialize, Deserialize, PartialEq, Debug)]
pub struct CacheFrame {
    pub frame: f32,
    pub positions: Vec<Vec3>,
}

struct Fabric {
    stretch: f32,
    shear: f32,
    bend: f32,
    density: f32,
    damping: f32,
    friction: f32,
}
impl Fabric {
    fn get(name: &str) -> Result<Self, String> {
        let (stretch, bend, density, damping, friction) = match name {
            "cotton" => (1e-8, 0.02, 0.2, 0.015, 0.45),
            "linen" => (5e-9, 0.008, 0.18, 0.018, 0.5),
            "wool" => (2e-8, 0.006, 0.35, 0.025, 0.6),
            "silk" => (2e-8, 0.08, 0.08, 0.01, 0.2),
            "leather" => (2e-9, 0.0004, 0.65, 0.035, 0.65),
            "denim" => (3e-9, 0.002, 0.4, 0.025, 0.55),
            _ => return Err("fabric must be cotton, linen, wool, silk, leather or denim".into()),
        };
        Ok(Self {
            stretch,
            shear: stretch * 4.0,
            bend: bend * 1000.0,
            density,
            damping,
            friction,
        })
    }
}
impl Pattern {
    fn clearance(&self) -> f32 {
        // Leather lies close to coarsely meshed calves. Reserve 3 mm so
        // the rendered body envelope cannot poke through the outer face.
        self.thickness * 0.5 + self.fit * 0.1 + if self.fabric == "leather" { 0.003 } else { 0.0 }
    }
    pub fn validate(&self) -> Result<(), String> {
        Fabric::get(&self.fabric)?;
        if !["tube", "skirt", "shirt", "tunic", "cape", "panel"].contains(&self.pattern.as_str()) {
            return Err("unknown garment pattern".into());
        }
        if self.subdivision > 2
            || !(8..=96).contains(&self.resolution)
            || !self.fit.is_finite()
            || !(0.001..=0.2).contains(&self.fit)
            || !self.gather.is_finite()
            || !(0.0..=0.75).contains(&self.gather)
            || !self.flare.is_finite()
            || !(0.0..=3.0).contains(&self.flare)
            || !self.thickness.is_finite()
            || !(0.0002..=0.02).contains(&self.thickness)
            || self.center.is_some_and(|p| !p.is_finite())
            || self
                .radius
                .is_some_and(|r| !r.is_finite() || r.min_element() <= 0.0)
            || self.belt.is_some_and(|b| !b.is_finite())
            || self.points.iter().any(|p| !p.is_finite())
        {
            return Err("invalid cloth dimensions (resolution 8–96, fit 0.001–0.2m, thickness 0.0002–0.02m, flare 0–3)".into());
        }
        if !["round", "v", "boat"].contains(&self.neckline.as_str()) {
            return Err("neckline must be round, v or boat".into());
        }
        Ok(())
    }
}
impl Cloth {
    pub fn generate(settings: Pattern, body: &Mesh, rig: Option<&Rig>) -> Result<Self, String> {
        settings.validate()?;
        if body.vertices.is_empty() {
            return Err("parent body is empty".into());
        }
        let mut c = Self {
            settings,
            rest: vec![],
            positions: vec![],
            uv: vec![],
            triangles: vec![],
            triangle_uv: vec![],
            pins: vec![],
            joints: vec![],
            weights: vec![],
            cache: vec![],
            clip: None,
        };
        let (lo, hi) = body.bounds();
        let height = hi.y - lo.y;
        let landmark = |name: &str, fraction: f32| {
            rig.and_then(|r| r.bones.iter().find(|b| b.name == name))
                .map_or(lo.y + height * fraction, |b| b.head.y)
        };
        let waist = landmark("hips", 0.55);
        let shoulder = landmark("upperarm.L", 0.82);
        let n = c.settings.resolution as usize;
        let yoke_rows = (n as f32 * 0.08).ceil().max(1.0) as usize;
        let length = match &c.settings.length {
            serde_json::Value::Number(v) => {
                v.as_f64()
                    .filter(|v| v.is_finite() && *v > 0.01 && *v < 10.0)
                    .ok_or("length must be 0.01–10 meters")? as f32
            }
            serde_json::Value::String(s) => {
                let bottom = match s.as_str() {
                    "waist" => waist,
                    "hip" => waist - height * 0.08,
                    "mid_thigh" => (waist + landmark("shin.L", 0.27)) * 0.5,
                    "knee" => landmark("shin.L", 0.27),
                    "ankle" => lo.y + height * 0.05,
                    _ => return Err("length: meters, waist, hip, mid_thigh, knee or ankle".into()),
                };
                (if c.settings.pattern == "skirt" {
                    waist
                } else {
                    shoulder
                }) - bottom
            }
            _ => return Err("length must be meters or a hem landmark".into()),
        }
        .max(0.03);
        if c.settings.pattern == "panel" {
            c.polygon(n)?;
        } else if c.settings.pattern == "cape" {
            let center = c.settings.center.unwrap_or(Vec3::new(
                (lo.x + hi.x) * 0.5,
                shoulder,
                lo.z - c.settings.fit,
            ));
            let half = c.settings.radius.map_or(height * 0.2, |r| r.x);
            for y in 0..=n {
                for x in 0..=n {
                    let u = x as f32 / n as f32;
                    let v = y as f32 / n as f32;
                    c.push(
                        center
                            + Vec3::new(
                                (u * 2.0 - 1.0) * half * (1.0 + c.settings.flare * v),
                                -length * v,
                                -0.08 * height * v,
                            ),
                        Vec2::new(u, v),
                    );
                    if y == 0 && (x == n / 4 || x == 3 * n / 4) {
                        c.pins.push((y * (n + 1) + x) as u32);
                    }
                }
            }
            c.grid(n, n, false, None);
        } else {
            let tube = c.settings.pattern == "tube";
            let bone = c
                .settings
                .bone
                .as_ref()
                .map(|name| {
                    rig.and_then(|r| r.bones.iter().find(|b| &b.name == name))
                        .ok_or_else(|| format!("no bone {name}"))
                })
                .transpose()?;
            if tube && bone.is_none() && c.settings.center.is_none() {
                return Err("tube needs bone or center (in parent rest space)".into());
            }
            let top = if c.settings.pattern == "skirt" {
                waist
            } else {
                shoulder
            };
            let center = c.settings.center.unwrap_or_else(|| {
                bone.map_or(
                    Vec3::new(
                        (lo.x + hi.x) * 0.5,
                        top,
                        rig.and_then(|r| {
                            r.bones.iter().find(|b| {
                                b.name
                                    == if c.settings.pattern == "skirt" {
                                        "hips"
                                    } else {
                                        "chest"
                                    }
                            })
                        })
                        .map_or((lo.z + hi.z) * 0.5, |b| b.head.z),
                    ),
                    |b| b.head,
                )
            });
            let axis = bone
                .filter(|_| tube)
                .map_or(Vec3::NEG_Y, |b| (b.tail - b.head).normalize_or(Vec3::NEG_Y));
            let uaxis = if tube {
                axis.any_orthonormal_vector()
            } else {
                Vec3::X
            };
            let vaxis = axis.cross(uaxis);
            // Measure a cross-section, excluding arms by an anatomical torso window.
            let fit = c.settings.fit;
            let section = |at: Vec3| {
                if tube {
                    // Use the first surface in each cross-section direction.
                    // A maximum over the plane can accidentally measure the
                    // chest behind an upper arm and make enormous sleeves.
                    let mut side = [f32::INFINITY; 4];
                    for v in &body.vertices {
                        let d = Vec3::from(v.pos) - at;
                        if d.dot(axis).abs() > height * 0.018 {
                            continue;
                        }
                        let (u, v) = (d.dot(uaxis), d.dot(vaxis));
                        if v.abs() < u.abs() * 0.35 {
                            let i = usize::from(u < 0.0);
                            side[i] = side[i].min(u.abs());
                        }
                        if u.abs() < v.abs() * 0.35 {
                            let i = 2 + usize::from(v < 0.0);
                            side[i] = side[i].min(v.abs());
                        }
                    }
                    let fallback = height * 0.03;
                    let r = |i: usize| {
                        if side[i].is_finite() {
                            side[i].max(height * 0.008)
                        } else {
                            fallback
                        }
                    };
                    return Vec2::new(r(0).max(r(1)), r(2).max(r(3))) + fit;
                }
                let mut radius = Vec2::splat(height * 0.04);
                for v in &body.vertices {
                    let d = Vec3::from(v.pos) - at;
                    if d.dot(axis).abs() < height * 0.025
                        && (if tube {
                            (d - axis * d.dot(axis)).length()
                                < (height * 0.09)
                                    .max(bone.map_or(0.0, |b| b.head.distance(b.tail) * 0.4))
                        } else {
                            d.x.abs()
                                < if at.y > waist + height * 0.05 {
                                    rig.and_then(|r| {
                                        r.bones.iter().find(|b| b.name == "upperarm.L")
                                    })
                                    .map_or(height * 0.12, |b| (b.head.x - at.x).abs() * 0.95)
                                } else {
                                    height * 0.14
                                }
                        })
                    {
                        radius = radius.max(Vec2::new(d.dot(uaxis).abs(), d.dot(vaxis).abs()));
                    }
                }
                radius + fit
            };
            let base = c.settings.radius.unwrap_or_else(|| {
                section(center + axis * if tube { length * 0.3 } else { height * 0.05 })
            });
            for y in 0..=n {
                let t = y as f32 / n as f32;
                let shirt = matches!(c.settings.pattern.as_str(), "shirt" | "tunic");
                let radius = if shirt {
                    section(center + axis * (length * t).min((shoulder - waist).max(0.0)))
                        .max(base * 0.75)
                        * (1.0 + c.settings.flare * t * t)
                } else if tube {
                    c.settings.radius.unwrap_or_else(|| {
                        let at = center + axis * (length * t);
                        let span = height * 0.012;
                        (section(at - axis * span) + section(at) * 2.0 + section(at + axis * span))
                            * 0.25
                    }) * (1.0 + c.settings.flare * t)
                } else {
                    base * (1.0
                        + c.settings.flare * t
                        + if c.settings.pattern == "skirt" {
                            c.settings.gather * (1.0 - t)
                        } else {
                            0.0
                        })
                };
                for x in 0..n {
                    let angle = TAU * x as f32 / n as f32;
                    let radial = uaxis * angle.cos() * radius.x + vaxis * angle.sin() * radius.y;
                    let mut p = center + axis * (length * t) + radial;
                    // Neck opening and a short shoulder yoke are part of
                    // the cut, rather than an unsupported giant torso hole.
                    if shirt && y <= yoke_rows {
                        let neck = if c.settings.neckline == "boat" {
                            0.52
                        } else {
                            0.30
                        };
                        let transition = y as f32 / yoke_rows as f32;
                        let neck_y = landmark("neck", 0.88);
                        let collar = center
                            + Vec3::Y * (neck_y - center.y - height * 0.008)
                            + uaxis * angle.cos() * base.x * neck
                            + vaxis * angle.sin() * base.y * 0.4
                            - Vec3::Y
                                * height
                                * if c.settings.neckline == "v" {
                                    0.045 * angle.sin().max(0.0)
                                } else {
                                    0.008 * angle.sin().abs()
                                };
                        let shoulder_width = rig
                            .and_then(|r| r.bones.iter().find(|b| b.name == "upperarm.L"))
                            .map_or(radius.x, |b| (b.head.x - center.x).abs() + height * 0.035)
                            .max(radius.x);
                        let edge = center
                            + uaxis * angle.cos() * shoulder_width
                            + vaxis * angle.sin() * radius.y
                            - Vec3::Y * height * 0.012;
                        p = collar.lerp(edge, transition);
                    }
                    c.push(p, Vec2::new(x as f32 / n as f32, t));
                    if (y == 0 && (c.settings.pattern != "skirt" || x % 4 == 0))
                        || (shirt && y <= yoke_rows)
                    {
                        c.pins.push((y * n + x) as u32);
                    }
                }
            }
            c.grid(
                n,
                n,
                true,
                matches!(c.settings.pattern.as_str(), "shirt" | "tunic").then_some((
                    yoke_rows,
                    ((n as f32 * (0.08 + height * 0.06 / length)).ceil() as usize)
                        .clamp(yoke_rows + 1, n - 1),
                )),
            );
        }
        if c.settings.sleeves && matches!(c.settings.pattern.as_str(), "shirt" | "tunic") {
            c.sew_sleeves(body, rig.ok_or("attached sleeves need a body rig")?, height)?;
        }
        if c.settings.pattern != "panel" {
            if c.settings.pins.iter().any(|&i| i as usize >= c.rest.len()) {
                return Err("pin vertex out of range".into());
            }
            c.pins.extend(&c.settings.pins);
        }
        if let Some(y) = c.settings.belt {
            let row = c
                .rest
                .iter()
                .map(|p| (p.y - y).abs())
                .fold(f32::INFINITY, f32::min);
            let center = Vec3::new((lo.x + hi.x) * 0.5, y, (lo.z + hi.z) * 0.5);
            for (i, p) in c.rest.iter_mut().enumerate() {
                if (p.y - y).abs() <= row + (length / n as f32 * 0.1).max(0.018) {
                    // Cinch the band to the body cross-section, then collision
                    // projection below moves it exactly outside the SDF.
                    let d = *p - center;
                    let radial = Vec3::new(d.x, 0.0, d.z);
                    *p -= radial * 0.15;
                    p.y = y + (p.y - y).clamp(-0.018, 0.018);
                    c.pins.push(i as u32);
                }
            }
        }
        c.pins.sort_unstable();
        c.pins.dedup();
        let used: BTreeSet<u32> = c.triangles.iter().flatten().copied().collect();
        let mut remap = vec![u32::MAX; c.rest.len()];
        let mut rest = Vec::new();
        let mut uv = Vec::new();
        for old in used {
            remap[old as usize] = rest.len() as u32;
            rest.push(c.rest[old as usize]);
            uv.push(c.uv[old as usize]);
        }
        for t in &mut c.triangles {
            for i in t {
                *i = remap[*i as usize];
            }
        }
        c.pins = c
            .pins
            .iter()
            .filter_map(|&i| (remap[i as usize] != u32::MAX).then_some(remap[i as usize]))
            .collect();
        c.rest = rest;
        c.uv = uv;
        c.positions = c.rest.clone();
        if c.settings.pattern == "skirt" && c.settings.gather > 0.0 {
            let y = c.positions[0].y;
            let center = Vec3::new(
                (lo.x + hi.x) * 0.5,
                y,
                rig.and_then(|r| r.bones.iter().find(|b| b.name == "hips"))
                    .map_or((lo.z + hi.z) * 0.5, |b| b.head.z),
            );
            for p in c.positions.iter_mut().take(n) {
                let d = *p - center;
                *p = center + d / (1.0 + c.settings.gather);
            }
            for (i, p) in c.positions.iter_mut().enumerate().skip(n) {
                let angle = TAU * (i % n) as f32 / n as f32;
                let radial = Vec3::new(p.x - center.x, 0.0, p.z - center.z).normalize_or(Vec3::X);
                *p += radial * (0.001 * (angle * 7.0).sin());
            }
        }
        Ok(c)
    }
    fn push(&mut self, p: Vec3, uv: Vec2) {
        self.rest.push(p);
        self.uv.push(uv);
    }
    fn grid(&mut self, nx: usize, ny: usize, closed: bool, armholes: Option<(usize, usize)>) {
        let row = if closed { nx } else { nx + 1 };
        for y in 0..ny {
            for x in 0..nx {
                // Sleeveless shirt: remove side patches under the shoulder yoke.
                if armholes.is_some_and(|(start, end)| y >= start && y < end)
                    && ((x as f32 + 0.5) * TAU / nx as f32).cos().abs() > 0.91
                {
                    continue;
                }
                let a = (y * row + x) as u32;
                let b = (y * row + (x + 1) % row) as u32;
                let d = a + row as u32;
                let e = b + row as u32;
                self.triangles.extend([[a, b, e], [a, e, d]]);
            }
        }
    }
    /// Extend each oriented armhole boundary through elbow to wrist. The
    /// first ring reuses torso nodes: every seam edge has exactly two faces.
    fn sew_sleeves(&mut self, body: &Mesh, rig: &Rig, height: f32) -> Result<(), String> {
        self.triangle_uv = self
            .triangles
            .iter()
            .map(|t| {
                let mut uv = t.map(|i| self.uv[i as usize]);
                let min = uv.iter().map(|p| p.x).fold(f32::INFINITY, f32::min);
                let max = uv.iter().map(|p| p.x).fold(f32::NEG_INFINITY, f32::max);
                for p in &mut uv {
                    if max - min > 0.5 && p.x < 0.5 {
                        p.x += 1.0;
                    }
                    p.x *= 0.5;
                }
                uv
            })
            .collect();
        let mut edges: BTreeMap<u32, u32> = self.boundaries().into_iter().collect();
        let mut loops = Vec::new();
        while let Some((&start, _)) = edges.first_key_value() {
            let mut ring = vec![start];
            let mut current = start;
            while let Some(next) = edges.remove(&current) {
                if next == start {
                    break;
                }
                ring.push(next);
                current = next;
            }
            let center =
                ring.iter().map(|&i| self.rest[i as usize]).sum::<Vec3>() / ring.len() as f32;
            if center.x.abs() > height * 0.045 {
                loops.push((ring, center));
            }
        }
        if loops.len() != 2 {
            return Err("tunic cut must have two armholes".into());
        }
        for (seam, center) in loops {
            let side = if center.x > 0.0 { "L" } else { "R" };
            let upper = rig
                .bones
                .iter()
                .find(|b| b.name == format!("upperarm.{side}"))
                .ok_or("sleeves need upperarm bones")?;
            let lower = rig
                .bones
                .iter()
                .find(|b| b.name == format!("forearm.{side}"))
                .ok_or("sleeves need forearm bones")?;
            let axis = (upper.tail - upper.head).normalize();
            let u = Vec3::Z.cross(axis).normalize();
            let v = axis.cross(u);
            let angles: Vec<f32> = seam
                .iter()
                .map(|&i| {
                    let d = self.rest[i as usize] - center;
                    d.dot(v).atan2(d.dot(u))
                })
                .collect();
            let turn: f32 = angles
                .iter()
                .enumerate()
                .map(|(i, &a)| {
                    let d = angles[(i + 1) % angles.len()] - a;
                    (d + std::f32::consts::PI).rem_euclid(TAU) - std::f32::consts::PI
                })
                .sum();
            let top = seam
                .iter()
                .enumerate()
                .max_by(|(_, a), (_, b)| {
                    let score = |i: u32| (self.rest[i as usize] - center).dot(u) * u.y.signum();
                    score(**a).total_cmp(&score(**b))
                })
                .unwrap()
                .0;
            let top_angle = if u.y >= 0.0 {
                0.0
            } else {
                std::f32::consts::PI
            };
            let angles: Vec<f32> = (0..seam.len())
                .map(|i| {
                    top_angle + turn.signum() * TAU * (i as f32 - top as f32) / seam.len() as f32
                })
                .collect();
            self.pins.extend(&seam);
            let segments = self.settings.resolution as usize / 2;
            let mut previous = seam.clone();
            for row in 1..=segments {
                let t = row as f32 / segments as f32;
                let (at, direction) = if t < 0.5 {
                    (upper.head.lerp(upper.tail, t * 2.0), axis)
                } else {
                    (
                        lower.head.lerp(lower.tail, (t - 0.5) * 2.0),
                        (lower.tail - lower.head).normalize(),
                    )
                };
                let u = Vec3::Z.cross(direction).normalize();
                let v = direction.cross(u);
                let radius = limb_section(body, at, direction, u, v, height)
                    + self.settings.fit * (1.0 - 0.65 * t);
                let mut next = Vec::new();
                for (x, &angle) in angles.iter().enumerate() {
                    let circle = at + u * radius.x * angle.cos() + v * radius.y * angle.sin();
                    let transition = (t / 0.10).min(1.0);
                    let p =
                        (self.rest[seam[x] as usize] + (at - upper.head)).lerp(circle, transition);
                    next.push(self.rest.len() as u32);
                    self.push(p, Vec2::new(x as f32 / seam.len() as f32, 1.0 + t));
                    // Secure cap rings and cuffs; the sleeve between remains free.
                    if row == segments || row <= 2 {
                        self.pins.push(*next.last().unwrap());
                    }
                }
                // Tack the sleeve's upper seam to the arm at the elbow.
                // This prevents a tube slipping off an oblique limb under gravity.
                if row == segments / 2 {
                    let top = *next
                        .iter()
                        .max_by(|&&a, &&b| {
                            self.rest[a as usize].y.total_cmp(&self.rest[b as usize].y)
                        })
                        .unwrap();
                    self.pins.push(top);
                }
                for x in 0..seam.len() {
                    let j = (x + 1) % seam.len();
                    self.triangles.extend([
                        [previous[j], previous[x], next[x]],
                        [previous[j], next[x], next[j]],
                    ]);
                    let uv = |column: usize, r: usize| {
                        Vec2::new(
                            0.5 + if side == "L" { 0.0 } else { 0.25 }
                                + 0.25 * column as f32 / seam.len() as f32,
                            r as f32 / segments as f32,
                        )
                    };
                    self.triangle_uv.extend([
                        [uv(x + 1, row - 1), uv(x, row - 1), uv(x, row)],
                        [uv(x + 1, row - 1), uv(x, row), uv(x + 1, row)],
                    ]);
                }
                previous = next;
            }
        }
        Ok(())
    }
    fn polygon(&mut self, n: usize) -> Result<(), String> {
        let points = self.settings.points.clone();
        if !(3..=20).contains(&points.len()) {
            return Err("panel requires 3–20 ordered polygon points".into());
        }
        let normal = points
            .iter()
            .enumerate()
            .map(|(i, p)| p.cross(points[(i + 1) % points.len()]))
            .sum::<Vec3>()
            .normalize_or_zero();
        if normal == Vec3::ZERO {
            return Err("degenerate panel polygon".into());
        }
        let u = normal.any_orthonormal_vector();
        let v = normal.cross(u);
        let flat: Vec<Vec2> = points
            .iter()
            .map(|p| Vec2::new(p.dot(u), p.dot(v)))
            .collect();
        let mut ring: Vec<usize> = (0..points.len()).collect();
        let cross = |a: usize, b: usize, c: usize| (flat[b] - flat[a]).perp_dot(flat[c] - flat[a]);
        let mut triangles = vec![];
        while ring.len() > 2 {
            let i = (0..ring.len())
                .find(|&i| {
                    let (a, b, c) = (
                        ring[(i + ring.len() - 1) % ring.len()],
                        ring[i],
                        ring[(i + 1) % ring.len()],
                    );
                    cross(a, b, c) > 1e-10
                        && !ring.iter().any(|&k| {
                            k != a
                                && k != b
                                && k != c
                                && cross(a, b, k) >= 0.0
                                && cross(b, c, k) >= 0.0
                                && cross(c, a, k) >= 0.0
                        })
                })
                .ok_or("panel must be a simple polygon")?;
            triangles.push([
                ring[(i + ring.len() - 1) % ring.len()] as u32,
                ring[i] as u32,
                ring[(i + 1) % ring.len()] as u32,
            ]);
            ring.remove(i);
        }
        self.rest = points;
        let lo = flat
            .iter()
            .copied()
            .fold(Vec2::splat(f32::INFINITY), Vec2::min);
        let hi = flat
            .iter()
            .copied()
            .fold(Vec2::splat(f32::NEG_INFINITY), Vec2::max);
        let span = (hi - lo).max_element().max(1e-6);
        self.uv = flat.iter().map(|p| (*p - lo) / span).collect();
        for _ in 0..(n.ilog2().saturating_sub(1)).min(5) {
            let mut mids = BTreeMap::new();
            let mut next = vec![];
            for [a, b, c] in triangles {
                let mut mid = |a: u32, b: u32| {
                    *mids.entry((a.min(b), a.max(b))).or_insert_with(|| {
                        let i = self.rest.len() as u32;
                        self.rest
                            .push((self.rest[a as usize] + self.rest[b as usize]) * 0.5);
                        self.uv
                            .push((self.uv[a as usize] + self.uv[b as usize]) * 0.5);
                        i
                    })
                };
                let (ab, bc, ca) = (mid(a, b), mid(b, c), mid(c, a));
                next.extend([[a, ab, ca], [ab, b, bc], [ca, bc, c], [ab, bc, ca]]);
            }
            triangles = next;
        }
        self.triangles = triangles;
        self.pins = if self.settings.pins.is_empty() {
            vec![0, 1]
        } else {
            self.settings.pins.clone()
        };
        if self
            .pins
            .iter()
            .any(|&i| i as usize >= self.settings.points.len())
        {
            return Err("panel pins must be polygon corner indices".into());
        }
        Ok(())
    }
    pub fn valid(&self) -> bool {
        let n = self.rest.len();
        self.settings.validate().is_ok()
            && (3..=20000).contains(&n)
            && self.positions.len() == n
            && self.uv.len() == n
            && self
                .rest
                .iter()
                .chain(&self.positions)
                .all(|p| p.is_finite())
            && self.uv.iter().all(|p| p.is_finite())
            && (self.triangle_uv.is_empty()
                || (self.triangle_uv.len() == self.triangles.len()
                    && self.triangle_uv.iter().flatten().all(|p| p.is_finite())))
            && self.triangles.len() <= 40000
            && self.triangles.iter().flatten().all(|&i| (i as usize) < n)
            && self.pins.iter().all(|&i| (i as usize) < n)
            && ((self.joints.is_empty() && self.weights.is_empty())
                || (self.joints.len() == n && self.weights.len() == n))
            && self
                .weights
                .iter()
                .flatten()
                .all(|w| w.is_finite() && *w >= 0.0)
            && self
                .weights
                .iter()
                .all(|w| (w.iter().sum::<f32>() - 1.0).abs() < 0.002)
            && self.cache.len() <= 240
            && self.cache.windows(2).all(|f| f[1].frame > f[0].frame)
            && self.cache.iter().all(|f| {
                f.frame.is_finite()
                    && f.positions.len() == n
                    && f.positions.iter().all(|p| p.is_finite())
            })
    }
    pub fn transfer_skin(&mut self, body: &Mesh, skin: Option<&Skin>) {
        self.joints.clear();
        self.weights.clear();
        if let Some(s) = skin {
            let near = rig::NearestVertex::new(body, 0.05);
            for p in &self.positions {
                let i = near.index(*p).unwrap_or(0);
                self.joints.push(s.joints[i]);
                self.weights.push(s.weights[i]);
            }
        }
    }
    fn anchor_weights(&mut self, rig: Option<&Rig>) {
        let Some(hips) = rig.and_then(|r| r.bone_index("hips")) else {
            return;
        };
        if self.joints.is_empty() {
            return;
        }
        for &index in &self.pins {
            let i = index as usize;
            if self.settings.pattern == "skirt"
                || self
                    .settings
                    .belt
                    .is_some_and(|y| (self.positions[i].y - y).abs() < 0.03)
            {
                self.joints[i] = [hips as u16, 0, 0, 0];
                self.weights[i] = [1.0, 0.0, 0.0, 0.0];
            }
        }
    }
    fn boundaries(&self) -> Vec<(u32, u32)> {
        let mut edges = BTreeMap::new();
        for [a, b, c] in &self.triangles {
            for (a, b) in [(*a, *b), (*b, *c), (*c, *a)] {
                edges
                    .entry((a.min(b), a.max(b)))
                    .and_modify(|e: &mut (u32, u32, usize)| e.2 += 1)
                    .or_insert((a, b, 1));
            }
        }
        edges
            .values()
            .filter(|e| e.2 == 1)
            .map(|e| (e.0, e.1))
            .collect()
    }
    /// A watertight shell: two offset surfaces joined by rounded half-circle
    /// rims. The seam uses duplicate render vertices but one simulated node.
    /// Subdivide the midsurface, never the solid shell: thickness and hem
    /// rims are rebuilt afterwards. The same stencils interpolate skin.
    fn subdivided(&self, positions: &[Vec3]) -> Self {
        let mut out = Self {
            settings: self.settings.clone(),
            rest: Vec::new(),
            positions: positions.to_vec(),
            uv: self.uv.clone(),
            triangles: self.triangles.clone(),
            triangle_uv: self.triangle_uv.clone(),
            pins: self.pins.clone(),
            joints: self.joints.clone(),
            weights: self.weights.clone(),
            cache: Vec::new(),
            clip: None,
        };
        for _ in 0..self.settings.subdivision {
            let mut edges: BTreeMap<(u32, u32), Vec<u32>> = BTreeMap::new();
            let mut neighbors = vec![BTreeSet::new(); out.positions.len()];
            for &[a, b, c] in &out.triangles {
                for (a, b, opposite) in [(a, b, c), (b, c, a), (c, a, b)] {
                    edges
                        .entry((a.min(b), a.max(b)))
                        .or_default()
                        .push(opposite);
                    neighbors[a as usize].insert(b);
                    neighbors[b as usize].insert(a);
                }
            }
            let boundary: BTreeSet<u32> = edges
                .iter()
                .filter(|(_, opposite)| opposite.len() == 1)
                .flat_map(|(&(a, b), _)| [a, b])
                .collect();
            let pins: BTreeSet<u32> = out.pins.iter().copied().collect();
            let mut stencils = Vec::new();
            for (i, near) in neighbors.iter().enumerate() {
                if boundary.contains(&(i as u32)) || pins.contains(&(i as u32)) || near.is_empty() {
                    stencils.push(vec![(i as u32, 1.0)]);
                } else {
                    let beta = if near.len() == 3 {
                        3.0 / 16.0
                    } else {
                        3.0 / (8.0 * near.len() as f32)
                    };
                    let mut stencil = vec![(i as u32, 1.0 - near.len() as f32 * beta)];
                    stencil.extend(near.iter().map(|&j| (j, beta)));
                    stencils.push(stencil);
                }
            }
            let mut mids = BTreeMap::new();
            for (&(a, b), opposite) in &edges {
                mids.insert((a, b), stencils.len() as u32);
                stencils.push(
                    if opposite.len() == 2 && !(pins.contains(&a) && pins.contains(&b)) {
                        vec![
                            (a, 0.375),
                            (b, 0.375),
                            (opposite[0], 0.125),
                            (opposite[1], 0.125),
                        ]
                    } else {
                        vec![(a, 0.5), (b, 0.5)]
                    },
                );
            }
            let positions = stencils
                .iter()
                .map(|s| s.iter().map(|&(i, w)| out.positions[i as usize] * w).sum())
                .collect();
            // UVs are linear in the cut, rather than smoothed across seams.
            let mut uv = out.uv.clone();
            for &(a, b) in edges.keys() {
                let (mut a, mut b) = (out.uv[a as usize], out.uv[b as usize]);
                if (a.x - b.x).abs() > 0.5 {
                    if a.x < b.x {
                        a.x += 1.0;
                    } else {
                        b.x += 1.0;
                    }
                }
                let mut mid = (a + b) * 0.5;
                mid.x %= 1.0;
                uv.push(mid);
            }
            if !out.joints.is_empty() {
                let mut joints = Vec::new();
                let mut weights = Vec::new();
                for stencil in &stencils {
                    let mut influences = BTreeMap::<u16, f32>::new();
                    for &(i, w) in stencil {
                        for k in 0..4 {
                            *influences.entry(out.joints[i as usize][k]).or_default() +=
                                out.weights[i as usize][k] * w;
                        }
                    }
                    let mut influences: Vec<_> = influences.into_iter().collect();
                    influences.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
                    let sum: f32 = influences.iter().take(4).map(|i| i.1).sum();
                    let mut j = [0; 4];
                    let mut w = [0.0; 4];
                    for (k, &(bone, weight)) in influences.iter().take(4).enumerate() {
                        j[k] = bone;
                        w[k] = weight / sum;
                    }
                    joints.push(j);
                    weights.push(w);
                }
                out.joints = joints;
                out.weights = weights;
            }
            let mid = |a: u32, b: u32| mids[&(a.min(b), a.max(b))];
            if !out.triangle_uv.is_empty() {
                out.triangle_uv = out
                    .triangle_uv
                    .iter()
                    .flat_map(|&[a, b, c]| {
                        let (ab, bc, ca) = ((a + b) * 0.5, (b + c) * 0.5, (c + a) * 0.5);
                        [[a, ab, ca], [ab, b, bc], [ca, bc, c], [ab, bc, ca]]
                    })
                    .collect();
            }
            out.triangles = out
                .triangles
                .iter()
                .flat_map(|&[a, b, c]| {
                    let (ab, bc, ca) = (mid(a, b), mid(b, c), mid(c, a));
                    [[a, ab, ca], [ab, b, bc], [ca, bc, c], [ab, bc, ca]]
                })
                .collect();
            out.positions = positions;
            out.uv = uv;
        }
        out.rest = out.positions.clone();
        out
    }
    pub fn mesh(&self, positions: &[Vec3], material: &Material) -> (Mesh, Option<Skin>) {
        if !self.valid() || positions.len() != self.rest.len() {
            return (Mesh::default(), None);
        }
        let smooth = self.subdivided(positions);
        smooth.shell(&smooth.positions, material)
    }
    fn shell(&self, positions: &[Vec3], material: &Material) -> (Mesh, Option<Skin>) {
        let mut mesh = Mesh {
            materials: vec![material.clone()],
            ..Default::default()
        };
        let mut normals = vec![Vec3::ZERO; positions.len()];
        for [a, b, c] in &self.triangles {
            let n = (positions[*b as usize] - positions[*a as usize])
                .cross(positions[*c as usize] - positions[*a as usize]);
            for i in [a, b, c] {
                normals[*i as usize] += n;
            }
        }
        let normals: Vec<Vec3> = normals.iter().map(|n| n.normalize_or(Vec3::Z)).collect();
        let half = self.settings.thickness * 0.5;
        let mut sources = vec![];
        let mut add = |i: usize, p: Vec3, normal: Vec3| {
            sources.push(i);
            mesh.vertices.push(Vertex {
                pos: p.into(),
                normal: normal.into(),
                color: [1.0; 3],
                surface: material.surface(),
                material: 0,
                uv: [
                    self.uv[i].x,
                    self.uv[i].y / if self.settings.sleeves { 2.0 } else { 1.0 },
                ],
                tangent: [0.0; 4],
                light: material.light(),
            
            base: [1.0, 1.0, 1.0, -1.0],
            wear: [0.0; 3],
            painted: [0; 4],});
            (mesh.vertices.len() - 1) as u32
        };
        for side in [1.0, -1.0] {
            for (i, p) in positions.iter().enumerate() {
                add(i, *p + normals[i] * half * side, normals[i] * side);
            }
        }
        let n = positions.len() as u32;
        for [a, b, c] in &self.triangles {
            mesh.indices.extend([*a, *b, *c, *a + n, *c + n, *b + n]);
        }
        if !self.triangle_uv.is_empty() {
            let mut assigned = vec![false; mesh.vertices.len()];
            let mut seams = HashMap::new();
            for (face, indices) in mesh.indices.chunks_exact_mut(3).enumerate() {
                let uv = self.triangle_uv[face / 2];
                let order = if face % 2 == 0 { [0, 1, 2] } else { [0, 2, 1] };
                for (corner, index) in indices.iter_mut().enumerate() {
                    let old = *index as usize;
                    let at = uv[order[corner]];
                    let key = (old, at.x.to_bits(), at.y.to_bits());
                    *index = *seams.entry(key).or_insert_with(|| {
                        if !assigned[old] {
                            assigned[old] = true;
                            mesh.vertices[old].uv = at.into();
                            old as u32
                        } else {
                            let mut v = mesh.vertices[old];
                            v.uv = at.into();
                            let next = mesh.vertices.len() as u32;
                            mesh.vertices.push(v);
                            sources.push(sources[old]);
                            next
                        }
                    });
                }
            }
        }
        // Duplicate the sewn ring's render seam at U=1 without splitting
        // the simulated nodes. Otherwise the last quad stretches the weave
        // across the entire garment.
        if self.triangle_uv.is_empty()
            && !matches!(self.settings.pattern.as_str(), "panel" | "cape")
        {
            for t in mesh.indices.chunks_exact_mut(3) {
                let lo = t
                    .iter()
                    .map(|&i| mesh.vertices[i as usize].uv[0])
                    .fold(f32::INFINITY, f32::min);
                let hi = t
                    .iter()
                    .map(|&i| mesh.vertices[i as usize].uv[0])
                    .fold(f32::NEG_INFINITY, f32::max);
                if hi - lo > 0.5 {
                    for i in t {
                        if mesh.vertices[*i as usize].uv[0] < 0.5 {
                            let old = *i as usize;
                            let mut v = mesh.vertices[old];
                            v.uv[0] += 1.0;
                            *i = mesh.vertices.len() as u32;
                            mesh.vertices.push(v);
                            sources.push(sources[old]);
                        }
                    }
                }
            }
        }
        let inner_start = n as usize;
        for (i, v) in mesh.vertices.iter_mut().enumerate() {
            let inner = if i < 2 * n as usize {
                i >= inner_start
            } else {
                Vec3::from(v.normal).dot(normals[sources[i]]) < 0.0
            };
            v.uv[0] = v.uv[0] * 0.5 + if inner { 0.5 } else { 0.0 };
        }
        // Borrow vertices separately for rims after the surface closure ends.
        for (a, b) in self.boundaries() {
            let (a, b) = (a as usize, b as usize);
            let tangent = (positions[b] - positions[a]).normalize_or(Vec3::X);
            let outward = tangent
                .cross((normals[a] + normals[b]).normalize_or(Vec3::Z))
                .normalize_or(Vec3::Y);
            let start = mesh.vertices.len() as u32;
            for step in 0..=4 {
                let angle = std::f32::consts::PI * step as f32 / 4.0;
                for i in [a, b] {
                    let normal =
                        (normals[i] * angle.cos() + outward * angle.sin()).normalize_or(normals[i]);
                    sources.push(i);
                    mesh.vertices.push(Vertex {
                        pos: (positions[i]
                            + (normals[i] * angle.cos() + outward * angle.sin() * 1.4) * half)
                            .into(),
                        normal: normal.into(),
                        color: [1.0; 3],
                        surface: material.surface(),
                        material: 0,
                        uv: mesh.vertices[i].uv,
                        tangent: [0.0; 4],
                        light: material.light(),
                    
                    base: [1.0, 1.0, 1.0, -1.0],
                    wear: [0.0; 3],
                    painted: [0; 4],});
                }
            }
            for s in 0..4 {
                let i = start + s * 2;
                mesh.indices.extend([i, i + 2, i + 3, i, i + 3, i + 1]);
            }
        }
        mesh.edges = crate::bake::edges(&mesh.indices);
        crate::bake::tangents(&mut mesh);
        let wear = material
            .weathered()
            .then(|| crate::weather::wear_maps(&mesh, 0.01));
        for (i, v) in mesh.vertices.iter_mut().enumerate() {
            let (color, rough) = crate::weather::apply(
                material,
                v.pos.into(),
                v.normal.into(),
                Vec3::ONE,
                material.roughness,
                wear.as_ref().map_or([0.0; 2], |w| w[i]),
                0.01,
            );
            v.color = color.into();
            v.surface[0] = rough;
        }
        // Smart-material masks (edges, creases) and layers, e.g. grimy_cloth.
        crate::smart::finish_mesh(&mut mesh, 0.01);
        let skin = (!self.joints.is_empty()).then(|| Skin {
            joints: sources.iter().map(|&i| self.joints[i]).collect(),
            weights: sources.iter().map(|&i| self.weights[i]).collect(),
        });
        (mesh, skin)
    }
    pub fn at_frame(&self, rig: Option<&Rig>, frame: Option<f32>) -> Vec<Vec3> {
        if let Some(f) = frame
            && self.clip.as_ref() == rig.and_then(|r| r.active.as_ref())
            && !self.cache.is_empty()
            && f >= self.cache[0].frame
            && f <= self.cache[self.cache.len() - 1].frame
        {
            let next = self
                .cache
                .partition_point(|p| p.frame < f)
                .min(self.cache.len() - 1);
            let prev = next.saturating_sub(1);
            let (a, b) = (&self.cache[prev], &self.cache[next]);
            let t = if b.frame > a.frame {
                ((f - a.frame) / (b.frame - a.frame)).clamp(0.0, 1.0)
            } else {
                0.0
            };
            return a
                .positions
                .iter()
                .zip(&b.positions)
                .map(|(a, b)| a.lerp(*b, t))
                .collect();
        }
        let Some(rig) = rig.filter(|_| !self.joints.is_empty() && frame.is_some()) else {
            return self.positions.clone();
        };
        let mats = rig.skin_matrices(frame);
        self.positions
            .iter()
            .enumerate()
            .map(|(i, p)| blend(&mats, self.joints[i], self.weights[i]).transform_point3(*p))
            .collect()
    }
}
/// Anatomical first-surface cross-section, avoiding the torso behind a limb.
fn limb_section(body: &Mesh, at: Vec3, axis: Vec3, u: Vec3, v: Vec3, height: f32) -> Vec2 {
    let mut side = [f32::INFINITY; 4];
    for vertex in &body.vertices {
        let d = Vec3::from(vertex.pos) - at;
        if d.dot(axis).abs() > height * 0.012 {
            continue;
        }
        let (x, z) = (d.dot(u), d.dot(v));
        if z.abs() < x.abs() * 0.45 {
            let i = usize::from(x < 0.0);
            side[i] = side[i].min(x.abs());
        }
        if x.abs() < z.abs() * 0.45 {
            let i = 2 + usize::from(z < 0.0);
            side[i] = side[i].min(z.abs());
        }
    }
    let r = |i: usize| {
        if side[i].is_finite() {
            side[i].max(height * 0.008)
        } else {
            height * 0.025
        }
    };
    Vec2::new(r(0).max(r(1)), r(2).max(r(3)))
}
fn blend(mats: &[Mat4], joints: [u16; 4], weights: [f32; 4]) -> Mat4 {
    (0..4).fold(Mat4::ZERO, |m, i| {
        m + mats
            .get(joints[i] as usize)
            .copied()
            .unwrap_or(Mat4::IDENTITY)
            * weights[i]
    })
}
struct Constraint {
    a: usize,
    b: usize,
    length: f32,
    compliance: f32,
    lambda: f32,
}
struct Solver {
    p: Vec<Vec3>,
    old: Vec<Vec3>,
    velocity: Vec<Vec3>,
    inv_mass: Vec<f32>,
    edges: Vec<Constraint>,
    adjacent: BTreeSet<(usize, usize)>,
    triangles: Vec<[u32; 3]>,
    pins: Vec<usize>,
    pin_rest: Vec<Vec3>,
    thickness: f32,
    fabric: Fabric,
}
impl Solver {
    fn new(c: &Cloth) -> Result<Self, String> {
        let fabric = Fabric::get(&c.settings.fabric)?;
        let mut mass = vec![0.0; c.rest.len()];
        let mut edges: BTreeMap<(usize, usize), Vec<usize>> = BTreeMap::new();
        for [a, b, d] in &c.triangles {
            let (a, b, d) = (*a as usize, *b as usize, *d as usize);
            let area = (c.rest[b] - c.rest[a])
                .cross(c.rest[d] - c.rest[a])
                .length()
                * 0.5;
            for i in [a, b, d] {
                mass[i] += area * fabric.density / 3.0;
            }
            for (a, b, o) in [(a, b, d), (b, d, a), (d, a, b)] {
                edges.entry((a.min(b), a.max(b))).or_default().push(o);
            }
        }
        let adjacent: BTreeSet<_> = edges.keys().copied().collect();
        let mut constraints: Vec<_> = edges
            .iter()
            .map(|(&(a, b), _)| Constraint {
                a,
                b,
                length: c.rest[a].distance(c.rest[b]),
                compliance: if (c.uv[a].x - c.uv[b].x).abs() > 1e-5
                    && (c.uv[a].y - c.uv[b].y).abs() > 1e-5
                {
                    fabric.shear
                } else {
                    fabric.stretch
                },
                lambda: 0.0,
            })
            .collect();
        for opposite in edges.values().filter(|v| v.len() == 2) {
            let (a, b) = (opposite[0], opposite[1]);
            constraints.push(Constraint {
                a,
                b,
                length: c.rest[a].distance(c.rest[b]),
                compliance: fabric.bend,
                lambda: 0.0,
            });
        }
        let mut inv_mass: Vec<_> = mass.iter().map(|m| 1.0 / m.max(1e-5)).collect();
        let pins: Vec<usize> = c.pins.iter().map(|&i| i as usize).collect();
        for &i in &pins {
            inv_mass[i] = 0.0;
        }
        Ok(Self {
            p: c.positions.clone(),
            old: c.positions.clone(),
            velocity: vec![Vec3::ZERO; c.rest.len()],
            inv_mass,
            edges: constraints,
            adjacent,
            triangles: c.triangles.clone(),
            pins,
            pin_rest: c.positions.clone(),
            thickness: c.settings.thickness,
            fabric,
        })
    }
    fn step(
        &mut self,
        dt: f32,
        pin: impl Fn(usize, Vec3) -> Vec3,
        collision: &impl Fn(Vec3) -> Option<(Vec3, Vec3)>,
    ) {
        self.old.clone_from(&self.p);
        for i in 0..self.p.len() {
            if self.inv_mass[i] > 0.0 {
                self.velocity[i] += Vec3::NEG_Y * 9.81 * dt;
                self.p[i] += self.velocity[i] * dt;
            }
        }
        for c in &mut self.edges {
            c.lambda = 0.0;
        }
        for iteration in 0..16 {
            for &i in &self.pins {
                self.p[i] = pin(i, self.pin_rest[i]);
            }
            for c in &mut self.edges {
                let d = self.p[c.a] - self.p[c.b];
                let len = d.length();
                let w = self.inv_mass[c.a] + self.inv_mass[c.b];
                if len < 1e-9 || w == 0.0 {
                    continue;
                }
                let alpha = c.compliance / (dt * dt);
                let dl = (-(len - c.length) - alpha * c.lambda) / (w + alpha);
                c.lambda += dl;
                let corr = d * (dl / len);
                self.p[c.a] += corr * self.inv_mass[c.a];
                self.p[c.b] -= corr * self.inv_mass[c.b];
            }
            if iteration % 4 == 3 {
                self.self_collision();
                self.layer_collision();
            }
            if iteration % 4 == 3 {
                for i in 0..self.p.len() {
                    if self.inv_mass[i] > 0.0
                        && let Some((p, n)) = collision(self.p[i])
                    {
                        self.p[i] = p;
                        // Coulomb-like positional friction; never changes normal clearance.
                        if self.inv_mass[i] > 0.0 {
                            let delta = self.p[i] - self.old[i];
                            self.p[i] -= (delta - n * delta.dot(n)) * self.fabric.friction;
                        }
                    }
                }
            }
        }
        for triangle in &self.triangles {
            let ids = triangle.map(|i| i as usize);
            let mut samples = vec![[1.0 / 3.0; 3]];
            for (a, b) in [(0, 1), (1, 2), (2, 0)] {
                if self.p[ids[a]].distance(self.p[ids[b]]) > 0.05 {
                    let mut weights = [0.0; 3];
                    weights[a] = 0.5;
                    weights[b] = 0.5;
                    samples.push(weights);
                }
            }
            for weights in samples {
                let point = (0..3).map(|j| self.p[ids[j]] * weights[j]).sum::<Vec3>();
                if let Some((q, _)) = collision(point) {
                    let w = (0..3)
                        .map(|j| self.inv_mass[ids[j]] * weights[j] * weights[j])
                        .sum::<f32>();
                    if w > 0.0 {
                        for j in 0..3 {
                            self.p[ids[j]] +=
                                (q - point) * (self.inv_mass[ids[j]] * weights[j] / w);
                        }
                    }
                }
            }
        }
        for i in 0..self.p.len() {
            if self.inv_mass[i] == 0.0 {
                self.p[i] = pin(i, self.pin_rest[i]);
            } else if let Some((q, _)) = collision(self.p[i]) {
                self.p[i] = q;
            }
            self.velocity[i] = (self.p[i] - self.old[i]) / dt * (1.0 - self.fabric.damping);
        }
    }
    // Vertex/triangle contacts keep folds separated even when vertex spacing
    // is larger than thickness. Stable triangle and vertex order is essential
    // for deterministic Gauss–Seidel projection.
    fn layer_collision(&mut self) {
        let gap = self.thickness * 1.1;
        let cell = 0.03_f32.max(gap * 2.0);
        let mut grid: HashMap<[i32; 3], Vec<usize>> = HashMap::new();
        for (index, t) in self.triangles.iter().enumerate() {
            let (a, b, c) = (
                self.p[t[0] as usize],
                self.p[t[1] as usize],
                self.p[t[2] as usize],
            );
            let lo = ((a.min(b).min(c) - gap) / cell).floor().as_ivec3();
            let hi = ((a.max(b).max(c) + gap) / cell).floor().as_ivec3();
            // A corrupt or exploded triangle must not allocate unbounded cells.
            if (hi - lo).max_element() > 64 {
                continue;
            }
            for z in lo.z..=hi.z {
                for y in lo.y..=hi.y {
                    for x in lo.x..=hi.x {
                        grid.entry([x, y, z]).or_default().push(index);
                    }
                }
            }
        }
        for i in 0..self.p.len() {
            let key = (self.p[i] / cell).floor().as_ivec3().to_array();
            for &index in grid.get(&key).into_iter().flatten() {
                let ids = self.triangles[index].map(|v| v as usize);
                if ids.contains(&i)
                    || ids
                        .iter()
                        .any(|&j| self.adjacent.contains(&(i.min(j), i.max(j))))
                {
                    continue;
                }
                let [a, b, c] = ids.map(|j| self.p[j]);
                let n = (b - a).cross(c - a).normalize_or_zero();
                if n == Vec3::ZERO {
                    continue;
                }
                let distance = (self.p[i] - a).dot(n);
                if distance.abs() > gap {
                    continue;
                }
                let q = self.p[i] - n * distance;
                let (u, v) = (b - a, c - a);
                let d = q - a;
                let denom = u.dot(u) * v.dot(v) - u.dot(v).powi(2);
                if denom.abs() < 1e-15 {
                    continue;
                }
                let w1 = (d.dot(u) * v.dot(v) - d.dot(v) * u.dot(v)) / denom;
                let w2 = (d.dot(v) * u.dot(u) - d.dot(u) * u.dot(v)) / denom;
                let bary = [1.0 - w1 - w2, w1, w2];
                if bary.iter().any(|w| *w < 0.0) {
                    continue;
                }
                let old_n = (self.old[ids[1]] - self.old[ids[0]])
                    .cross(self.old[ids[2]] - self.old[ids[0]])
                    .normalize_or(n);
                let side = if (self.old[i] - self.old[ids[0]]).dot(old_n) < 0.0 {
                    -1.0
                } else {
                    1.0
                };
                let w = self.inv_mass[i]
                    + (0..3)
                        .map(|j| self.inv_mass[ids[j]] * bary[j] * bary[j])
                        .sum::<f32>();
                if w == 0.0 {
                    continue;
                }
                let correction = n * side * (gap - distance * side) / w;
                self.p[i] += correction * self.inv_mass[i];
                for j in 0..3 {
                    self.p[ids[j]] -= correction * (self.inv_mass[ids[j]] * bary[j]);
                }
            }
        }
    }
    fn self_collision(&mut self) {
        let distance = self.thickness * 1.5;
        let mut grid: HashMap<[i32; 3], Vec<usize>> = HashMap::new();
        for (i, p) in self.p.iter().enumerate() {
            grid.entry((*p / distance).floor().as_ivec3().to_array())
                .or_default()
                .push(i);
        }
        for a in 0..self.p.len() {
            let cell = (self.p[a] / distance).floor().as_ivec3().to_array();
            for z in -1..=1 {
                for y in -1..=1 {
                    for x in -1..=1 {
                        for &b in grid
                            .get(&[cell[0] + x, cell[1] + y, cell[2] + z])
                            .into_iter()
                            .flatten()
                        {
                            if b <= a || self.adjacent.contains(&(a, b)) {
                                continue;
                            }
                            let d = self.p[a] - self.p[b];
                            let len = d.length();
                            let w = self.inv_mass[a] + self.inv_mass[b];
                            if len < distance && w > 0.0 {
                                let corr = d.normalize_or(Vec3::X) * (distance - len) / w;
                                self.p[a] += corr * self.inv_mass[a];
                                self.p[b] -= corr * self.inv_mass[b];
                            }
                        }
                    }
                }
            }
        }
    }
}
/// Collision projects into the clay field. Animated collision maps the
/// query back through the weights of the nearest posed body surface.
struct Collider<'a> {
    field: Field,
    body: &'a Mesh,
    skin: Option<&'a Skin>,
    mats: Vec<Mat4>,
    posed: Mesh,
    render_sdf: crate::human::MeshSdf,
    belt_plane: Option<(Vec3, Vec3)>,
    clearance: f32,
}
impl<'a> Collider<'a> {
    fn new(
        clay: &Clay,
        body: &'a Mesh,
        skin: Option<&'a Skin>,
        rig: Option<&Rig>,
        frame: Option<f32>,
        clearance: f32,
        belt: Option<f32>,
    ) -> Self {
        let mut field = Field::new(clay, 0.05);
        field.limit = 0.5;
        let mats = rig
            .filter(|_| frame.is_some())
            .map_or(vec![], |r| r.skin_matrices(frame));
        let posed = match skin.filter(|_| !mats.is_empty()) {
            Some(s) => rig::pose_mesh(body, s, &mats),
            None => body.clone(),
        };
        let points: Vec<Vec3> = posed.vertices.iter().map(|v| v.pos.into()).collect();
        let triangles: Vec<[u32; 3]> = posed
            .indices
            .chunks_exact(3)
            .map(|t| [t[0], t[1], t[2]])
            .collect();
        let render_sdf = crate::human::MeshSdf::new(&points, &triangles);
        let belt_plane = belt.map(|y| {
            let hips = rig.and_then(|r| r.bone_index("hips"));
            let center = hips
                .and_then(|i| rig.map(|r| r.bones[i].head))
                .unwrap_or(Vec3::ZERO);
            let matrix = hips
                .and_then(|i| mats.get(i))
                .copied()
                .unwrap_or(Mat4::IDENTITY);
            (
                matrix.transform_point3(Vec3::new(center.x, y, center.z)),
                matrix.transform_vector3(Vec3::Y).normalize_or(Vec3::Y),
            )
        });
        Self {
            field,
            body,
            skin,
            mats,
            posed,
            render_sdf,
            belt_plane,
            clearance,
        }
    }
    /// The meshed render body can sit outside its exact clay field at coarse
    /// cells. Contact both envelopes, including the actual posed triangles.
    fn render_contact(&self, mut p: Vec3) -> Option<(Vec3, Vec3)> {
        let origin = p;
        let distance = self.render_sdf.distance(p);
        // Intersecting component shells can have ambiguous signs far from
        // their outer surface. This is a local display guard, not the SDF.
        if distance < -0.015 {
            return None;
        }
        let list = self.field.near(p - Vec3::splat(0.6), p + Vec3::splat(0.6));
        if self
            .belt_plane
            .is_some_and(|(center, normal)| (p - center).dot(normal).abs() < 0.045)
            || self.render_sdf.distance(p) >= self.clearance
        {
            return None;
        }
        let sample = |q| self.render_sdf.distance(q);
        let mut normal = Vec3::Y;
        for _ in 0..8 {
            let distance = sample(p);
            if distance >= self.clearance - 1e-6 {
                break;
            }
            let e = 0.0005;
            normal = Vec3::new(
                sample(p + Vec3::X * e) - sample(p - Vec3::X * e),
                sample(p + Vec3::Y * e) - sample(p - Vec3::Y * e),
                sample(p + Vec3::Z * e) - sample(p - Vec3::Z * e),
            )
            .normalize_or(normal);
            let next = p + normal * (self.clearance - distance).min(0.015);
            if next.distance(origin) > 0.015
                || (self.mats.is_empty()
                    && self.field.sample(next, &list).0 < self.field.sample(p, &list).0 - 0.0001)
            {
                return None;
            }
            p = next;
        }
        Some((p, normal))
    }
    fn project(&self, p: Vec3, nearest: &rig::NearestVertex<'_>) -> Option<(Vec3, Vec3)> {
        let animated = self.skin.is_some() && !self.mats.is_empty();
        let index = if animated { nearest.index(p)? } else { 0 };
        let transform = self.skin.filter(|_| animated).map_or(Mat4::IDENTITY, |s| {
            blend(&self.mats, s.joints[index], s.weights[index])
        });
        let inverse = transform.inverse();
        let mut q = inverse.transform_point3(p);
        let list = self.field.near(q - Vec3::splat(0.6), q + Vec3::splat(0.6));
        let sample = |q| self.field.sample(q, &list).0;
        if sample(q) >= self.clearance {
            return self.render_contact(p);
        }
        let mut normal = Vec3::from(self.body.vertices[index].normal);
        for _ in 0..8 {
            let d = sample(q);
            if d >= self.clearance - 1e-6 {
                break;
            }
            let e = 0.0005;
            let g = Vec3::new(
                sample(q + Vec3::X * e) - sample(q - Vec3::X * e),
                sample(q + Vec3::Y * e) - sample(q - Vec3::Y * e),
                sample(q + Vec3::Z * e) - sample(q - Vec3::Z * e),
            );
            if g.length_squared() < 1e-14
                && !animated
                && let Some(i) = nearest.index(q)
            {
                normal = Vec3::from(self.body.vertices[i].normal);
            }
            normal = g.normalize_or(normal);
            q += normal * (self.clearance - d).min(0.1);
        }
        let p = transform.transform_point3(q);
        self.render_contact(p)
            .or_else(|| Some((p, transform.transform_vector3(normal).normalize_or(Vec3::Y))))
    }
}
/// A cinch belongs underneath existing visible belt hardware. Keeping those
/// blobs in the scene but excluding them from contact prevents covering them.
fn collision_clay(clay: &Clay, belt: Option<f32>) -> Clay {
    let mut out = clay.clone();
    if belt.is_some() {
        out.blobs.retain(|b| {
            let name = b.name.to_ascii_lowercase();
            !(name.contains("belt") || name.contains("buckle"))
        });
    }
    out
}
pub fn drape(
    c: &mut Cloth,
    clay: &Clay,
    body: &Mesh,
    skin: Option<&Skin>,
    rig: Option<&Rig>,
    seconds: f32,
    progress: &Progress,
) -> Result<(), String> {
    if !c.valid() || !seconds.is_finite() || !(0.05..=20.0).contains(&seconds) {
        return Err("invalid garment or seconds (0.05–20)".into());
    }
    let collision_body = collision_clay(clay, c.settings.belt);
    let collider = Collider::new(
        &collision_body,
        body,
        skin,
        rig,
        None,
        c.settings.clearance(),
        c.settings.belt,
    );
    let near = rig::NearestVertex::new(&collider.posed, 0.05);
    for p in &mut c.positions {
        if let Some((q, _)) = collider.project(*p, &near) {
            *p = q;
        }
    }
    // Attach the collar and the entire shoulder seam to the actual skin,
    // including points initially outside it. Collision alone only pushes out.
    if matches!(c.settings.pattern.as_str(), "shirt" | "tunic") {
        for &index in &c.pins {
            let i = index as usize;
            if c.uv[i].y > 0.5 && !(c.settings.sleeves && (1.0..=1.075).contains(&c.uv[i].y)) {
                continue;
            }
            let mut q = c.positions[i];
            let list = collider
                .field
                .near(q - Vec3::splat(0.3), q + Vec3::splat(0.3));
            let sample = |p| collider.field.sample(p, &list).0;
            let mut clearance = collider.clearance;
            if c.uv[i].y > 0.0
                && let Some(rig) = rig
            {
                let side = if q.x >= 0.0 {
                    "upperarm.L"
                } else {
                    "upperarm.R"
                };
                if let Some(arm) = rig.bones.iter().find(|b| b.name == side)
                    && q.x.abs() > arm.head.x.abs() * 0.55
                    && q.y > arm.head.y - 0.055
                {
                    // Reserve part of the requested ease for the curved cap:
                    // interpolating pinned surface points otherwise cuts skin.
                    clearance += c.settings.fit * 0.65;
                    // A shoulder seam sits on the upper skin. Nearest-point
                    // projection can instead attach it to the chest's side.
                    let top = arm.head.y + 0.18;
                    let mut above = Vec3::new(q.x, top, q.z);
                    for step in 1..=80 {
                        let below = Vec3::new(q.x, top - step as f32 * 0.005, q.z);
                        if sample(below) < clearance {
                            let mut lo = below.y;
                            let mut hi = above.y;
                            for _ in 0..18 {
                                let mid = (lo + hi) * 0.5;
                                if sample(Vec3::new(q.x, mid, q.z)) < clearance {
                                    lo = mid;
                                } else {
                                    hi = mid;
                                }
                            }
                            q.y = hi;
                            break;
                        }
                        above = below;
                    }
                }
            }
            for _ in 0..16 {
                let d = sample(q) - clearance;
                if d.abs() < 0.00001 {
                    break;
                }
                let e = 0.0005;
                let g = Vec3::new(
                    sample(q + Vec3::X * e) - sample(q - Vec3::X * e),
                    sample(q + Vec3::Y * e) - sample(q - Vec3::Y * e),
                    sample(q + Vec3::Z * e) - sample(q - Vec3::Z * e),
                )
                .normalize_or(Vec3::Y);
                q -= g * d.clamp(-0.03, 0.03);
            }
            c.positions[i] = q;
            c.rest[i] = q;
        }
    }
    if let Some(belt) = c.settings.belt {
        let center = rig
            .and_then(|r| r.bones.iter().find(|b| b.name == "hips"))
            .map_or(Vec3::new(0.0, belt, 0.0), |b| {
                Vec3::new(b.head.x, belt, b.head.z)
            });
        for &index in &c.pins {
            let i = index as usize;
            if (c.positions[i].y - belt).abs() < 0.04 {
                let origin = Vec3::new(center.x, c.positions[i].y, center.z);
                let direction = (c.positions[i] - origin).normalize_or(Vec3::X);
                let mut lo = 0.0;
                let mut hi = c.positions[i].distance(origin) + c.settings.fit;
                let list = collider
                    .field
                    .near(origin - Vec3::splat(0.6), origin + Vec3::splat(0.6));
                for _ in 0..20 {
                    let mid = (lo + hi) * 0.5;
                    if collider.field.sample(origin + direction * mid, &list).0 < collider.clearance
                    {
                        lo = mid;
                    } else {
                        hi = mid;
                    }
                }
                c.positions[i] = origin + direction * hi;
            }
        }
    }
    // Match anchors to the displayed body too, before freezing them.
    for p in &mut c.positions {
        if let Some((q, _)) = collider.render_contact(*p) {
            *p = q;
        }
    }
    // Project anchors too, so shoulder seams/belts cannot pin inside skin.
    let mut solver = Solver::new(c)?;
    let steps = (seconds * 120.0).ceil() as usize;
    for i in 0..steps {
        progress.check()?;
        progress.stage("draping cloth", i as f32 / steps as f32);
        solver.step(1.0 / 120.0, |_, p| p, &|p| collider.project(p, &near));
    }
    c.positions = solver.p;
    c.cache.clear();
    c.clip = None;
    c.transfer_skin(body, skin);
    c.anchor_weights(rig);
    progress.set(1.0);
    Ok(())
}
#[allow(clippy::too_many_arguments)]
pub fn simulate(
    c: &mut Cloth,
    clay: &Clay,
    body: &Mesh,
    skin: &Skin,
    rig: &Rig,
    frames: &[f32],
    fps: f32,
    progress: &Progress,
) -> Result<(), String> {
    if frames.is_empty()
        || frames.len() > 240
        || frames.windows(2).any(|v| v[1] <= v[0])
        || frames.iter().any(|f| !f.is_finite())
        || !(1.0..=120.0).contains(&fps)
    {
        return Err("simulate requires 1–240 increasing frames and fps 1–120".into());
    }
    if rig.active.is_none() {
        return Err("select an animation clip for simulate".into());
    }
    let collision_body = collision_clay(clay, c.settings.belt);
    c.transfer_skin(body, Some(skin));
    c.anchor_weights(Some(rig));
    let mut solver = Solver::new(c)?;
    if c.settings.sleeves && matches!(c.settings.pattern.as_str(), "shirt" | "tunic") {
        // The local inverse-skin collider is reliable for a hanging hem but
        // ambiguous at a bent elbow. Keep fitted upper pieces on their skin;
        // only the free cloth below the belt/waist carries simulated inertia.
        let waist = c.settings.belt.or_else(|| {
            rig.bones
                .iter()
                .find(|b| b.name == "hips")
                .map(|b| b.head.y)
        });
        if let Some(waist) = waist {
            for (i, p) in c.positions.iter().enumerate() {
                if p.y >= waist - 0.02 || c.uv[i].y > 1.0 {
                    solver.inv_mass[i] = 0.0;
                    if !solver.pins.contains(&i) {
                        solver.pins.push(i);
                    }
                }
            }
        }
    }

    c.cache.clear();
    c.clip = rig.active.clone();
    let mut previous = frames[0] - 1.0;
    for (index, &frame) in frames.iter().enumerate() {
        let steps = (((frame - previous) / fps) * 120.0).ceil().max(1.0) as usize;
        if steps > 2400 {
            return Err("frame gap too large (maximum 20 simulated seconds)".into());
        }
        for step in 0..steps {
            progress.check()?;
            progress.stage(
                "simulating animation",
                (index as f32 + step as f32 / steps as f32) / frames.len() as f32,
            );
            let f = previous + (frame - previous) * (step + 1) as f32 / steps as f32;
            let collider = Collider::new(
                &collision_body,
                body,
                Some(skin),
                Some(rig),
                Some(f),
                c.settings.clearance(),
                c.settings.belt,
            );
            let near = rig::NearestVertex::new(&collider.posed, 0.05);
            let mats = rig.skin_matrices(Some(f));
            solver.step(
                (frame - previous) / fps / steps as f32,
                |i, p| blend(&mats, c.joints[i], c.weights[i]).transform_point3(p),
                &|p| collider.project(p, &near),
            );
            if step + 1 == steps
                && c.settings.sleeves
                && matches!(c.settings.pattern.as_str(), "shirt" | "tunic")
            {
                // Skin interpolation can cut the concave armpit even when
                // rest contact is clear. Correct the cached fitted pieces
                // against the actual posed triangles, without inverse skin.
                for (i, p) in solver.p.iter_mut().enumerate() {
                    if solver.inv_mass[i] == 0.0
                        && let Some((q, _)) = collider.render_contact(*p)
                    {
                        *p = q;
                    }
                }
            }
        }
        c.cache.push(CacheFrame {
            frame,
            positions: solver.p.clone(),
        });
        previous = frame;
    }
    progress.set(1.0);
    Ok(())
}
/// Preserve pattern UVs and skin while simplifying. A repeating woven
/// normal texture is defined in pattern space, independent of tessellation.
pub fn bake(
    mesh: &Mesh,
    skin: Option<&Skin>,
    settings: crate::bake::Settings,
    progress: &Progress,
) -> Result<(Mesh, Option<Skin>), String> {
    progress.check()?;
    progress.stage("simplifying cloth with pattern UVs", 0.0);
    let mut out = mesh.clone();
    let adapter = meshopt::VertexDataAdapter::new(
        bytemuck::cast_slice(&mesh.vertices),
        std::mem::size_of::<Vertex>(),
        0,
    )
    .map_err(|e| e.to_string())?;
    out.indices = meshopt::simplify(
        &mesh.indices,
        &adapter,
        (settings.triangles as usize * 3).min(mesh.indices.len()),
        1.0,
        meshopt::SimplifyOptions::None,
        None,
    );
    if out.indices.len() > settings.triangles as usize * 3 {
        return Err("cloth seams prevent this triangle budget; increase triangles or use a lower pattern resolution".into());
    }
    let mut origins = vec![];
    let mut remap = vec![u32::MAX; mesh.vertices.len()];
    for i in &mut out.indices {
        let old = *i as usize;
        if remap[old] == u32::MAX {
            remap[old] = origins.len() as u32;
            origins.push(old);
        }
        *i = remap[old];
    }
    out.vertices = origins.iter().map(|&i| mesh.vertices[i]).collect();
    out.edges = crate::bake::edges(&out.indices);
    crate::bake::tangents(&mut out);
    let size = settings.texture;
    let mut color = vec![];
    let mut normal = vec![];
    let mut orm = vec![];
    let material = mesh.materials.first().cloned().unwrap_or_default();
    let high = crate::bake::raster(mesh, size);
    let low = crate::bake::raster(&out, size);
    let interpolate = |m: &Mesh, tri: u32, w: Vec2| {
        let t = &m.indices[tri as usize * 3..tri as usize * 3 + 3];
        let bary = [1.0 - w.x - w.y, w.x, w.y];
        let normal = (0..3)
            .map(|j| Vec3::from(m.vertices[t[j] as usize].normal) * bary[j])
            .sum::<Vec3>()
            .normalize_or(Vec3::Z);
        let tangent = (0..3)
            .map(|j| Vec3::from_slice(&m.vertices[t[j] as usize].tangent) * bary[j])
            .sum::<Vec3>();
        let tangent =
            (tangent - normal * normal.dot(tangent)).normalize_or(normal.any_orthonormal_vector());
        let bitangent = normal.cross(tangent) * m.vertices[t[0] as usize].tangent[3].signum();
        (normal, tangent, bitangent)
    };
    for y in 0..size {
        progress.check()?;
        progress.stage("baking cloth weave", y as f32 / size as f32);
        for x in 0..size {
            color.extend([255, 255, 255, 255]);
            let dx = (TAU * x as f32 / size as f32 * 64.0).sin() * 0.08;
            let dy = (TAU * y as f32 / size as f32 * 64.0).sin() * 0.08;
            let pixel = (y * size + x) as usize;
            let fold = match (high[pixel], low[pixel]) {
                (Some((h, hw)), Some((l, lw))) => {
                    let (hn, _, _) = interpolate(mesh, h, hw);
                    let (ln, t, b) = interpolate(&out, l, lw);
                    Vec3::new(hn.dot(t), hn.dot(b), hn.dot(ln)).normalize_or(Vec3::Z)
                }
                _ => Vec3::Z,
            };
            let n = (fold + Vec3::new(dx, dy, 0.0)).normalize();
            normal.extend(n.to_array().map(|v| ((v * 0.5 + 0.5) * 255.0) as u8));
            normal.push(255);
            orm.extend([
                255,
                (material.roughness * 255.0) as u8,
                (material.metallic * 255.0) as u8,
                255,
            ]);
        }
    }
    out.textures = Some(Arc::new(Textures {
        size,
        color,
        normal,
        orm,
        alpha: false,
    }));
    let skin = skin.map(|s| Skin {
        joints: origins.iter().map(|&i| s.joints[i]).collect(),
        weights: origins.iter().map(|&i| s.weights[i]).collect(),
    });
    Ok((out, skin))
}

/// Carry simulated motion onto a simplified mesh without changing its UVs.
pub fn pose_reduced(rest: &Mesh, shown: &Mesh, reduced: &Mesh) -> Mesh {
    let nearest = rig::NearestVertex::new(rest, 0.02);
    let mut out = reduced.clone();
    for v in &mut out.vertices {
        if let Some(i) = nearest.index(v.pos.into()) {
            v.pos = (Vec3::from(v.pos) + Vec3::from(shown.vertices[i].pos)
                - Vec3::from(rest.vertices[i].pos))
            .into();
            let rotation = glam::Quat::from_rotation_arc(
                Vec3::from(rest.vertices[i].normal).normalize_or(Vec3::Z),
                Vec3::from(shown.vertices[i].normal).normalize_or(Vec3::Z),
            );
            v.normal = (rotation * Vec3::from(v.normal)).into();
            let tangent = rotation * Vec3::from_slice(&v.tangent);
            v.tangent = [tangent.x, tangent.y, tangent.z, v.tangent[3]];
        }
    }
    out
}

/// Vertex animation expressed as translation joints is portable to both
/// existing exporters. Each render vertex has a joint so rounded rims and
/// normals can follow the cache exactly; this is intentionally an offline
/// cache format, rather than a small real-time character rig.
pub fn export_cache(
    c: &Cloth,
    mesh: &Mesh,
    material: &Material,
    parent: Option<&Rig>,
) -> Option<(Rig, Skin)> {
    if c.cache.is_empty() || mesh.vertices.len() >= u16::MAX as usize {
        return None;
    }
    let mut rig = Rig::default();
    rig.bones.push(rig::Bone {
        name: "cloth_cache_root".into(),
        parent: None,
        head: Vec3::ZERO,
        tail: Vec3::Y * 0.01,
        deform: false,
    });
    let full = c.mesh(&c.positions, material).0;
    let nearest = rig::NearestVertex::new(&full, 0.02);
    let source: Vec<usize> = mesh
        .vertices
        .iter()
        .map(|v| nearest.index(v.pos.into()).unwrap_or(0))
        .collect();
    for (i, v) in mesh.vertices.iter().enumerate() {
        rig.bones.push(rig::Bone {
            name: format!("cloth_vertex_{i}"),
            parent: Some("cloth_cache_root".into()),
            head: v.pos.into(),
            tail: Vec3::from(v.pos) + Vec3::Y * 0.001,
            deform: true,
        });
    }
    let mut ranges: Vec<(String, f32, f32)> = parent.map_or(Vec::new(), |r| {
        r.clips
            .iter()
            .filter_map(|clip| {
                clip.range()
                    .map(|(start, end)| (clip.name.clone(), start, end))
            })
            .collect()
    });
    if !ranges
        .iter()
        .any(|(name, _, _)| Some(name) == c.clip.as_ref())
    {
        ranges.push((
            c.clip.clone().unwrap_or("cloth".into()),
            c.cache[0].frame,
            c.cache.last()?.frame,
        ));
    }
    for (name, start, end) in ranges {
        let mut active = parent.cloned().unwrap_or_default();
        active.active = Some(name.clone());
        // Exporters sample the exact last key, without playback wrapping.
        for clip in &mut active.clips {
            clip.looped = false;
        }
        let mut times: Vec<f32> = (0..)
            .map(|i| start + i as f32)
            .take_while(|f| *f < end)
            .take(100_000)
            .collect();
        times.push(end);
        if c.clip.as_ref() == Some(&name) {
            times.extend(c.cache.iter().map(|f| f.frame));
        }
        times.sort_by(f32::total_cmp);
        times.dedup();
        let frames: Vec<_> = times
            .into_iter()
            .map(|frame| {
                let positions = if parent.is_some() {
                    c.at_frame(Some(&active), Some(frame))
                } else {
                    let next = c
                        .cache
                        .partition_point(|p| p.frame < frame)
                        .min(c.cache.len() - 1);
                    c.cache[next].positions.clone()
                };
                (frame, c.mesh(&positions, material).0)
            })
            .collect();
        let mut clip = rig::Clip::new(&name);
        for (i, &at) in source.iter().enumerate() {
            let mut keys = Vec::with_capacity(frames.len());
            for (frame, posed) in &frames {
                let mut key = rig::Key::new(*frame);
                key.interp = rig::Interp::Linear;
                key.translate =
                    Vec3::from(posed.vertices[at].pos) - Vec3::from(full.vertices[at].pos);
                let q = glam::Quat::from_rotation_arc(
                    Vec3::from(full.vertices[at].normal).normalize_or(Vec3::Z),
                    Vec3::from(posed.vertices[at].normal).normalize_or(Vec3::Z),
                );
                let (z, y, x) = q.to_euler(glam::EulerRot::ZYX);
                key.rotate = Vec3::new(x, y, z) * 180.0 / std::f32::consts::PI;
                keys.push(key);
            }
            clip.tracks.push(rig::Track {
                bone: format!("cloth_vertex_{i}"),
                keys,
            });
        }
        rig.clips.push(clip);
    }
    rig.active = c.clip.clone();
    let skin = Skin {
        joints: (0..mesh.vertices.len())
            .map(|i| [(i + 1) as u16, 0, 0, 0])
            .collect(),
        weights: vec![[1.0, 0.0, 0.0, 0.0]; mesh.vertices.len()],
    };
    Some((rig, skin))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn sphere() -> (Clay, Mesh) {
        let clay:Clay=serde_json::from_value(serde_json::json!({"blobs":[{"type":"sphere","center":[0,0,0],"radius":0.5}],"resolution":24})).unwrap();
        let mesh = clay.build_mesh();
        (clay, mesh)
    }
    fn panel() -> Cloth {
        let (_, body) = sphere();
        Cloth::generate(
            Pattern {
                pattern: "panel".into(),
                points: vec![
                    Vec3::new(-0.4, 1.0, 0.0),
                    Vec3::new(0.4, 1.0, 0.0),
                    Vec3::new(0.4, 1.0, 0.8),
                    Vec3::new(-0.4, 1.0, 0.8),
                ],
                resolution: 16,
                ..Default::default()
            },
            &body,
            None,
        )
        .unwrap()
    }
    #[test]
    fn default_human_tunic_is_connected_and_shoulders_attach_to_skin() {
        for sex in [0.0, 0.5, 1.0] {
            let human = crate::human::Human {
                sex,
                ..Default::default()
            };
            let body = human.build().unwrap();
            let rig = Rig {
                bones: body.rig(),
                ..Default::default()
            };
            let mesh = Mesh {
                vertices: body
                    .points
                    .iter()
                    .map(|&p| Vertex {
                        pos: p.into(),
                        normal: Vec3::Y.into(),
                        color: [1.0; 3],
                        surface: [0.0; 4],
                        material: 0,
                        uv: [0.0; 2],
                        tangent: [0.0; 4],
                        light: [0.0; 2],
                        base: [1.0, 1.0, 1.0, -1.0],
                        wear: [0.0; 3],
                        painted: [0; 4],
                    })
                    .collect(),
                indices: body.triangles.iter().flatten().copied().collect(),
                ..Default::default()
            };
            let clay: Clay = serde_json::from_value(
            serde_json::json!({"blobs":[{"type":"human","human":human,"blend":0}],"resolution":24}),
        )
        .unwrap();
            let waist = rig.bones.iter().find(|b| b.name == "hips").unwrap().head.y;
            let mut c = Cloth::generate(
                Pattern {
                    sleeves: true,
                    belt: Some(waist),
                    ..Default::default()
                },
                &mesh,
                Some(&rig),
            )
            .unwrap();
            assert!(c.valid());
            let mut neighbors = vec![Vec::new(); c.rest.len()];
            let mut edges = BTreeMap::<(u32, u32), Vec<(u32, u32)>>::new();
            for &[a, b, d] in &c.triangles {
                for (a, b) in [(a, b), (b, d), (d, a)] {
                    neighbors[a as usize].push(b);
                    neighbors[b as usize].push(a);
                    edges.entry((a.min(b), a.max(b))).or_default().push((a, b));
                }
            }
            for faces in edges.values() {
                assert!(faces.len() <= 2, "nonmanifold sewn seam");
                if faces.len() == 2 {
                    assert_eq!(faces[0], (faces[1].1, faces[1].0), "seam winding");
                }
            }
            let mut seen = BTreeSet::new();
            let mut pending = vec![0];
            while let Some(i) = pending.pop() {
                if seen.insert(i) {
                    pending.extend(&neighbors[i as usize]);
                }
            }
            assert_eq!(seen.len(), c.rest.len(), "disconnected sleeves");
            let mut boundary: BTreeMap<u32, u32> = c.boundaries().into_iter().collect();
            let mut openings = 0;
            while let Some((&i, _)) = boundary.first_key_value() {
                openings += 1;
                let mut next = i;
                while let Some(j) = boundary.remove(&next) {
                    next = j;
                }
            }
            assert_eq!(openings, 4, "only collar, hem and two cuffs remain open");
            drape(
                &mut c,
                &clay,
                &mesh,
                None,
                Some(&rig),
                0.05,
                &Progress::default(),
            )
            .unwrap();
            let field = Field::new(&clay, 0.05);
            let list = field.near(Vec3::splat(-2.0), Vec3::splat(2.0));
            for &i in &c.pins {
                if c.uv[i as usize].y <= 0.081 {
                    let d = field.sample(c.positions[i as usize], &list).0;
                    assert!(
                        (0.001..=c.settings.fit).contains(&d),
                        "unsupported shoulder/collar: {d}"
                    );
                }
            }
            let (smooth, _) = c.mesh(&c.positions, &Material::default());
            assert!(smooth.indices.len() > c.triangles.len() * 3 * 8);
            for v in &smooth.vertices {
                assert!((Vec3::from(v.normal).length() - 1.0).abs() < 0.001);
            }
            // Rays from above the sleeve cap must meet fabric before skin.
            // Merely checking seam connectivity misses a cap buried in the body.
            let arm = rig.bones.iter().find(|b| b.name == "upperarm.L").unwrap();
            let mut exposed = Vec::new();
            for x in [0.9, 1.0, 1.1] {
                for z in [-0.02, 0.0, 0.02] {
                    let mut point = Vec3::new(arm.head.x * x, arm.head.y + 0.2, arm.head.z + z);
                    for _ in 0..100 {
                        let distance = field.sample(point, &list).0;
                        if distance < 0.0001 {
                            break;
                        }
                        point.y -= distance.clamp(0.0001, 0.01);
                    }
                    let mut top = f32::NEG_INFINITY;
                    for ids in smooth.indices.chunks_exact(3) {
                        let p = ids
                            .iter()
                            .map(|&i| Vec3::from(smooth.vertices[i as usize].pos))
                            .collect::<Vec<_>>();
                        let a = Vec2::new(p[0].x, p[0].z);
                        let b = Vec2::new(p[1].x, p[1].z);
                        let d = Vec2::new(p[2].x, p[2].z);
                        let cross = (b - a).perp_dot(d - a);
                        if cross.abs() < 1e-10 {
                            continue;
                        }
                        let q = Vec2::new(point.x, point.z) - a;
                        let u = q.perp_dot(d - a) / cross;
                        let v = (b - a).perp_dot(q) / cross;
                        if u >= 0.0 && v >= 0.0 && u + v <= 1.0 {
                            top = top.max(p[0].y * (1.0 - u - v) + p[1].y * u + p[2].y * v);
                        }
                    }
                    if top <= point.y {
                        exposed.push((point, top));
                    }
                }
            }
            assert!(exposed.is_empty(), "exposed shoulder samples: {exposed:?}");
            let (baked, _) = bake(
                &smooth,
                None,
                crate::bake::Settings {
                    triangles: 9000,
                    texture: 512,
            ..Default::default()
                },
                &Progress::default(),
            )
            .unwrap();
            assert!(baked.indices.len() <= 27000);
        }
    }

    #[test]
    fn subdivision_keeps_thickness_pins_and_interpolates_skin() {
        let mut c = panel();
        c.joints = (0..c.rest.len()).map(|i| [i as u16 % 2, 0, 0, 0]).collect();
        c.weights = vec![[1.0, 0.0, 0.0, 0.0]; c.rest.len()];
        let smooth = c.subdivided(&c.positions);
        for &i in &c.pins {
            assert_eq!(smooth.positions[i as usize], c.positions[i as usize]);
        }
        assert_eq!(smooth.triangles.len(), 4 * c.triangles.len());
        assert!(
            smooth
                .weights
                .iter()
                .all(|w| (w.iter().sum::<f32>() - 1.0).abs() < 1e-6)
        );
        assert!(smooth.weights.iter().any(|w| w[0] > 0.0 && w[1] > 0.0));
        let (mesh, skin) = c.mesh(&c.positions, &Material::default());
        assert_eq!(skin.unwrap().joints.len(), mesh.vertices.len());
        for i in 0..smooth.positions.len() {
            assert!(
                (Vec3::from(mesh.vertices[i].pos)
                    .distance(mesh.vertices[i + smooth.positions.len()].pos.into())
                    - c.settings.thickness)
                    .abs()
                    < 1e-6
            );
        }
    }
    #[test]
    fn sleeved_upper_garment_follows_skin_while_hem_keeps_inertia() {
        let (clay, mesh) = sphere();
        let mut c = panel();
        c.settings.pattern = "tunic".into();
        c.settings.sleeves = true;
        c.settings.belt = Some(0.5);
        c.positions[2].y = 0.4;
        c.rest = c.positions.clone();
        let mut rig = Rig {
            bones: vec![rig::Bone {
                name: "hips".into(),
                parent: None,
                head: Vec3::ZERO,
                tail: Vec3::Y,
                deform: true,
            }],
            ..Default::default()
        };
        let mut clip = rig::Clip::new("walk");
        for frame in [0.0, 1.0] {
            let mut key = rig::Key::new(frame);
            key.translate.x = frame * 0.02;
            clip.track_mut("hips").insert(key);
        }
        clip.looped = false;
        rig.clips.push(clip);
        rig.active = Some("walk".into());
        let skin = Skin {
            joints: vec![[0; 4]; mesh.vertices.len()],
            weights: vec![[1.0, 0.0, 0.0, 0.0]; mesh.vertices.len()],
        };
        simulate(
            &mut c,
            &clay,
            &mesh,
            &skin,
            &rig,
            &[0.0, 1.0],
            24.0,
            &Progress::default(),
        )
        .unwrap();
        for f in &c.cache {
            let matrix = rig.skin_matrices(Some(f.frame))[0];
            for i in [0, 1, 3] {
                assert!(f.positions[i].distance(matrix.transform_point3(c.positions[i])) < 1e-6);
            }
        }
        assert!(
            c.cache[1].positions[2]
                .distance(rig.skin_matrices(Some(1.0))[0].transform_point3(c.positions[2]))
                > 1e-5
        );
    }
    #[test]
    fn displayed_body_envelope_is_also_a_collider() {
        let (clay, mut body) = sphere();
        // A coarse/offset display surface can lie outside the exact field.
        for v in &mut body.vertices {
            v.pos = (Vec3::from(v.pos) * 1.04).into();
        }
        let collider = Collider::new(&clay, &body, None, None, None, 0.003, None);
        let nearest = rig::NearestVertex::new(&body, 0.05);
        assert!(
            collider.render_contact(Vec3::ZERO).is_none(),
            "display guard must stay local"
        );
        let point = Vec3::Y * 0.51;
        let list = collider.field.near(Vec3::splat(-1.0), Vec3::splat(1.0));
        assert!(collider.field.sample(point, &list).0 > 0.003);
        let (outside, _) = collider
            .project(point, &nearest)
            .expect("inside displayed mesh");
        assert!(collider.render_sdf.distance(outside) >= 0.00299);
        assert_eq!(collider.project(point, &nearest).unwrap().0, outside);
    }
    #[test]
    fn pinned_square_hangs_and_keeps_edge_lengths() {
        let c = panel();
        let mut s = Solver::new(&c).unwrap();
        for _ in 0..360 {
            s.step(1.0 / 120.0, |_, p| p, &|_| None);
        }
        assert_eq!(s.p[0], c.positions[0]);
        assert_eq!(s.p[1], c.positions[1]);
        assert!(s.p.iter().map(|p| p.y).fold(f32::INFINITY, f32::min) < 0.5);
        for constraint in s.edges.iter().filter(|e| e.compliance <= s.fabric.shear) {
            let error =
                (s.p[constraint.a].distance(s.p[constraint.b]) / constraint.length - 1.0).abs();
            assert!(error < 0.035, "edge strain {error}");
        }
    }
    #[test]
    fn drape_stays_outside_sphere_and_is_deterministic() {
        let (clay, body) = sphere();
        let mut a = panel();
        for p in &mut a.positions {
            p.y = 0.55;
            p.z -= 0.4;
        }
        a.rest = a.positions.clone();
        let mut b = a.clone();
        drape(&mut a, &clay, &body, None, None, 2.0, &Progress::default()).unwrap();
        drape(&mut b, &clay, &body, None, None, 2.0, &Progress::default()).unwrap();
        assert_eq!(a, b);
        for p in &a.positions {
            assert!(
                p.length() >= 0.5 + a.settings.thickness * 0.49,
                "penetration {p}"
            );
        }
        for t in &a.triangles {
            let center = t.iter().map(|&i| a.positions[i as usize]).sum::<Vec3>() / 3.0;
            assert!(
                center.length() >= 0.5,
                "triangle interior penetrates sphere"
            );
        }
    }
    #[test]
    fn moving_thigh_does_not_cross_skirt_vertices() {
        let clay:Clay=serde_json::from_value(serde_json::json!({"blobs":[{"type":"capsule","a":[0,1,0],"b":[0,0.2,0],"radius":0.12,"bone":"thigh"}],"resolution":24})).unwrap();
        let body = clay.build_mesh();
        let skin = Skin {
            joints: vec![[1, 0, 0, 0]; body.vertices.len()],
            weights: vec![[1.0, 0.0, 0.0, 0.0]; body.vertices.len()],
        };
        let mut rig = Rig {
            bones: vec![
                rig::Bone {
                    name: "hips".into(),
                    parent: None,
                    head: Vec3::Y,
                    tail: Vec3::Y * 1.1,
                    deform: false,
                },
                rig::Bone {
                    name: "thigh".into(),
                    parent: Some("hips".into()),
                    head: Vec3::Y,
                    tail: Vec3::Y * 0.2,
                    deform: true,
                },
            ],
            ..Default::default()
        };
        let mut clip = rig::Clip::new("walk");
        for (f, r) in [(0.0, -25.0), (10.0, 25.0), (20.0, -25.0)] {
            let mut k = rig::Key::new(f);
            k.rotate.x = r;
            clip.track_mut("thigh").insert(k);
        }
        rig.clips.push(clip);
        rig.active = Some("walk".into());
        let mut c = Cloth::generate(
            Pattern {
                pattern: "skirt".into(),
                center: Some(Vec3::Y),
                radius: Some(Vec2::splat(0.17)),
                length: serde_json::json!(0.6),
                resolution: 12,
                ..Default::default()
            },
            &body,
            Some(&rig),
        )
        .unwrap();
        drape(
            &mut c,
            &clay,
            &body,
            Some(&skin),
            Some(&rig),
            0.5,
            &Progress::default(),
        )
        .unwrap();
        simulate(
            &mut c,
            &clay,
            &body,
            &skin,
            &rig,
            &(0..=20).map(|f| f as f32).collect::<Vec<_>>(),
            24.0,
            &Progress::default(),
        )
        .unwrap();
        for frame in &c.cache {
            for &i in &c.pins {
                assert!(
                    frame.positions[i as usize].distance(c.positions[i as usize]) < 0.005,
                    "waist pin moved with thigh"
                );
            }
            let inv = rig.skin_matrices(Some(frame.frame))[1].inverse();
            let field = Field::new(&clay, 0.05);
            let list = field.near(Vec3::splat(-2.0), Vec3::splat(2.0));
            for p in &frame.positions {
                assert!(
                    field.sample(inv.transform_point3(*p), &list).0 >= c.settings.thickness * 0.45,
                    "frame {} penetration",
                    frame.frame
                );
            }
        }
    }
    #[test]
    fn overlapping_layers_separate_against_triangles() {
        let mut c = panel();
        c.rest = vec![
            Vec3::ZERO,
            Vec3::Z,
            Vec3::X,
            Vec3::new(0.2, 0.0005, 0.2),
            Vec3::new(0.3, 0.0005, 0.2),
            Vec3::new(0.2, 0.0005, 0.3),
        ];
        c.positions = c.rest.clone();
        c.uv = vec![Vec2::ZERO; 6];
        c.triangles = vec![[0, 1, 2], [3, 5, 4]];
        c.pins = vec![0, 1, 2];
        let mut solver = Solver::new(&c).unwrap();
        solver.layer_collision();
        for i in 3..6 {
            assert!(solver.p[i].y >= c.settings.thickness * 1.09);
        }
    }
    #[test]
    fn malformed_cloth_is_rejected_and_cancel_is_observed() {
        let (clay, body) = sphere();
        let mut c = panel();
        let p = Progress::default();
        p.cancel();
        assert!(drape(&mut c, &clay, &body, None, None, 1.0, &p).is_err());
        c.triangles.push([u32::MAX, 0, 1]);
        assert!(!c.valid());
        assert!(
            c.mesh(&c.positions, &Material::default())
                .0
                .vertices
                .is_empty()
        );
        assert!(
            Cloth::generate(
                Pattern {
                    fabric: "unknown".into(),
                    ..Default::default()
                },
                &body,
                None
            )
            .is_err()
        );
    }
    #[test]
    fn scene_skin_bake_and_cached_exports_round_trip() {
        let mut scene = crate::scene::Scene::default();
        let body = scene.add(crate::scene::Shape::Clay);
        let (clay, mesh) = sphere();
        scene.get_mut(body).unwrap().clay = Some(clay);
        scene.get_mut(body).unwrap().rig = Some(Rig {
            bones: vec![rig::Bone {
                name: "root".into(),
                parent: None,
                head: Vec3::ZERO,
                tail: Vec3::Y,
                deform: true,
            }],
            ..Default::default()
        });
        let id = scene.add(crate::scene::Shape::Cloth);
        let mut c = panel();
        let skin = Skin {
            joints: vec![[0; 4]; mesh.vertices.len()],
            weights: vec![[1.0, 0.0, 0.0, 0.0]; mesh.vertices.len()],
        };
        c.transfer_skin(&mesh, Some(&skin));
        let o = scene.get_mut(id).unwrap();
        o.parent = Some(body);
        o.material = Some("cloth".into());
        o.cloth = Some(c.clone());
        let back: crate::scene::Scene =
            serde_json::from_str(&serde_json::to_string(&scene).unwrap()).unwrap();
        assert_eq!(scene, back);
        let mut lib = crate::mesh::MeshLib::new();
        lib.sync(&scene);
        let settings = crate::bake::Settings {
            triangles: 1000,
            texture: 512,
            ..Default::default()
        };
        lib.bake(&scene, id, settings).unwrap();
        let (baked, skin) = lib.baked(id).unwrap();
        assert!(baked.indices.len() / 3 <= 1000);
        assert_eq!(baked.vertices.len(), skin.unwrap().joints.len());
        let options = crate::gltf::Options {
            roots: vec![],
            animations: true,
            fps: 24.0,
            file_stem: "cloth".into(),
            lods: false, atlas: true, texel_density: None,
        };
        let exported = crate::gltf::export(&scene, &lib, &options).unwrap();
        let loaded = crate::glb_read::from_bytes(&exported.bytes, None, None).unwrap();
        assert_eq!(loaded.summary["skins"].as_array().unwrap().len(), 2);
        let parent_rig = scene.get_mut(body).unwrap().rig.as_mut().unwrap();
        for (name, end, dx) in [("idle", 2.0, 0.1), ("drop", 4.0, 0.2)] {
            let mut clip = rig::Clip::new(name);
            clip.track_mut("root").insert(rig::Key::new(0.0));
            let mut key = rig::Key::new(end);
            key.translate.x = dx;
            clip.track_mut("root").insert(key);
            parent_rig.clips.push(clip);
        }
        c.cache = vec![
            CacheFrame {
                frame: 0.0,
                positions: c.positions.clone(),
            },
            CacheFrame {
                frame: 2.0,
                positions: c.positions.iter().map(|p| *p - Vec3::Y * 0.1).collect(),
            },
        ];
        c.clip = Some("drop".into());
        scene.get_mut(id).unwrap().cloth = Some(c);
        lib.sync(&scene);
        let exported = crate::gltf::export(&scene, &lib, &options).unwrap();
        let loaded = crate::glb_read::from_bytes(&exported.bytes, Some("drop"), Some(2.0)).unwrap();
        assert!(loaded.summary["animations"].as_array().is_some());
        let point = |loaded: &crate::glb_read::Loaded| {
            loaded.items[1]
                .world
                .transform_point3(loaded.items[1].mesh.vertices[0].pos.into())
        };
        let baseline = crate::glb_read::from_bytes(&exported.bytes, None, None).unwrap();
        assert!(
            ((point(&loaded) - point(&baseline)).y + 0.1).abs() < 1e-4,
            "cached vertex motion was lost"
        );
        let idle = crate::glb_read::from_bytes(&exported.bytes, Some("idle"), Some(2.0)).unwrap();
        assert!(
            ((point(&idle) - point(&baseline)).x - 0.1).abs() < 1e-4,
            "other clip stopped moving cloth"
        );
        let outside =
            crate::glb_read::from_bytes(&exported.bytes, Some("drop"), Some(4.0)).unwrap();
        assert!(
            ((point(&outside) - point(&baseline)).x - 0.2).abs() < 1e-4,
            "motion outside cache was lost"
        );
        let fbx = crate::fbx::export(&scene, &lib, &options).unwrap();
        assert!(fbx.bytes.starts_with(b"Kaydara FBX Binary"));
    }
}
