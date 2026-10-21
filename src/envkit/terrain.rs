//! Ground: a heightfield for cave floors and outdoor terrain. The shape is
//! noise (rolling, or ridged for rocky ground), a network of cracks, and
//! brush strokes on top (raise, lower, smooth, flatten, noise), stored as
//! a grid of offsets; color is dirt on the flats and rock on the slopes,
//! darker in cracks, plus painted color stored the same way.
//!
//! The terrain spans `size` in the object's XZ plane, centered on the
//! origin, with y up; heights are in meters.

use base64::Engine;
use glam::{Vec2, Vec3};
use serde::{Deserialize, Serialize};

use super::{Builder, cells, fbm, smoothstep};
use crate::material::Material;
use crate::mesh::Mesh;

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
#[serde(deny_unknown_fields)]
pub struct Terrain {
    /// Width (X) and depth (Z) in meters.
    #[serde(default = "default_size")]
    pub size: [f32; 2],
    /// Grid cells along the longer side (8..512).
    #[serde(default = "default_resolution")]
    pub resolution: u32,
    /// Noise height in meters (peak to trough, roughly).
    #[serde(default = "default_height")]
    pub height: f32,
    /// Bumps per meter of the largest noise layer.
    #[serde(default = "default_frequency")]
    pub frequency: f32,
    /// Noise layers (1..8): more gives finer grit.
    #[serde(default = "default_octaves")]
    pub octaves: u32,
    /// 0 = rolling, 1 = sharp ridged crests (rocky ground).
    #[serde(default)]
    pub ridged: f32,
    /// Crack depth in meters (0 = none), spacing of the crack network
    /// cells and the width of a crack.
    #[serde(default)]
    pub cracks: f32,
    #[serde(default = "default_crack_scale")]
    pub crack_scale: f32,
    #[serde(default = "default_crack_width")]
    pub crack_width: f32,
    #[serde(default)]
    pub seed: u32,
    /// sRGB colors: flat ground and steep (rocky) ground.
    #[serde(default = "default_color")]
    pub color: [f32; 3],
    #[serde(default = "default_rock_color")]
    pub rock_color: [f32; 3],
    /// Brush offsets in meters per grid vertex (row-major, (nx+1) x (nz+1)).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sculpt: Option<Grid>,
    /// Painted sRGB color and coverage per grid vertex (RGBA bytes).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub paint: Option<Grid>,
}

fn default_size() -> [f32; 2] { [8.0, 8.0] }
fn default_resolution() -> u32 { 128 }
fn default_height() -> f32 { 0.25 }
fn default_frequency() -> f32 { 0.35 }
fn default_octaves() -> u32 { 5 }
fn default_crack_scale() -> f32 { 0.9 }
fn default_crack_width() -> f32 { 0.025 }
fn default_color() -> [f32; 3] { [0.33, 0.29, 0.24] }
fn default_rock_color() -> [f32; 3] { [0.42, 0.41, 0.39] }

impl Default for Terrain {
    fn default() -> Self {
        serde_json::from_value(serde_json::json!({})).expect("defaults")
    }
}

/// A per-vertex grid saved as base64 (f32 offsets or RGBA bytes).
#[derive(Clone, PartialEq, Debug)]
pub struct Grid {
    pub nx: u32,
    pub nz: u32,
    pub data: Vec<u8>,
}

impl Serialize for Grid {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut st = s.serialize_struct("Grid", 3)?;
        st.serialize_field("nx", &self.nx)?;
        st.serialize_field("nz", &self.nz)?;
        st.serialize_field("data", &base64::engine::general_purpose::STANDARD.encode(&self.data))?;
        st.end()
    }
}

impl<'de> Deserialize<'de> for Grid {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Raw { nx: u32, nz: u32, data: String }
        let r = Raw::deserialize(d)?;
        let data = base64::engine::general_purpose::STANDARD.decode(r.data).map_err(serde::de::Error::custom)?;
        Ok(Grid { nx: r.nx, nz: r.nz, data })
    }
}

impl Grid {
    fn floats(&self) -> Vec<f32> {
        self.data.chunks_exact(4).map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect()
    }

    fn from_floats(nx: u32, nz: u32, v: &[f32]) -> Grid {
        Grid { nx, nz, data: v.iter().flat_map(|f| f.to_le_bytes()).collect() }
    }

    fn valid(&self, width: usize) -> bool {
        self.data.len() == ((self.nx + 1) * (self.nz + 1)) as usize * width
    }
}

/// One brush stroke on a terrain (terrain-local XZ, meters).
#[derive(Deserialize, Clone, Debug)]
#[serde(deny_unknown_fields)]
pub struct Stroke {
    pub brush: Brush,
    /// Path points: [x, z] or [x, y, z] (y is ignored).
    pub points: Vec<Vec<f32>>,
    #[serde(default = "default_radius")]
    pub radius: f32,
    /// Meters for raise / lower / noise; 0..1 for smooth, flatten, paint.
    #[serde(default = "default_strength")]
    pub strength: f32,
    /// flatten: the height to level toward (default: the height under the first point).
    #[serde(default)]
    pub height: Option<f32>,
    /// paint: sRGB color.
    #[serde(default)]
    pub color: Option<[f32; 3]>,
    /// noise: bump size in meters.
    #[serde(default)]
    pub scale: Option<f32>,
}

fn default_radius() -> f32 { 0.5 }
fn default_strength() -> f32 { 0.1 }

#[derive(Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum Brush {
    Raise,
    Lower,
    Smooth,
    Flatten,
    Noise,
    Paint,
    Erase,
}

impl Terrain {
    pub fn sanitize(&mut self) {
        let fix = |v: f32, d: f32, lo: f32, hi: f32| if v.is_finite() { v.clamp(lo, hi) } else { d };
        self.size = self.size.map(|s| fix(s, 8.0, 0.1, 500.0));
        self.resolution = self.resolution.clamp(8, 512);
        self.height = fix(self.height, 0.25, 0.0, 100.0);
        self.frequency = fix(self.frequency, 0.35, 0.001, 50.0);
        self.octaves = self.octaves.clamp(1, 8);
        self.ridged = fix(self.ridged, 0.0, 0.0, 1.0);
        self.cracks = fix(self.cracks, 0.0, 0.0, 2.0);
        self.crack_scale = fix(self.crack_scale, 0.9, 0.05, 50.0);
        self.crack_width = fix(self.crack_width, 0.025, 0.001, 1.0);
        self.color = self.color.map(|c| fix(c, 0.3, 0.0, 1.0));
        self.rock_color = self.rock_color.map(|c| fix(c, 0.4, 0.0, 1.0));
        if self.sculpt.as_ref().is_some_and(|g| !g.valid(4)) {
            self.sculpt = None;
        }
        if self.paint.as_ref().is_some_and(|g| !g.valid(4)) {
            self.paint = None;
        }
    }

    /// Grid cells along X and Z.
    pub fn cells(&self) -> (u32, u32) {
        let [w, d] = self.size;
        let long = w.max(d).max(1e-6);
        let r = self.resolution as f32;
        (((w / long) * r).round().max(1.0) as u32, ((d / long) * r).round().max(1.0) as u32)
    }

    fn vertex_xz(&self, i: u32, j: u32) -> Vec2 {
        let (nx, nz) = self.cells();
        Vec2::new((i as f32 / nx as f32 - 0.5) * self.size[0], (j as f32 / nz as f32 - 0.5) * self.size[1])
    }

    /// Height from noise and cracks only (no brushes), and the crack amount 0..1.
    pub fn base_height(&self, p: Vec2) -> (f32, f32) {
        let q = Vec3::new(p.x, 0.0, p.y) * self.frequency;
        let smooth = fbm(q, self.octaves, self.seed);
        let ridged = 1.0 - 2.0 * fbm(q, self.octaves.min(4), self.seed ^ 0x51).abs();
        let mut h = self.height * 0.5 * (smooth * (1.0 - self.ridged) + ridged * self.ridged);
        let mut crack = 0.0;
        if self.cracks > 0.0 {
            // Wobbly cell edges: a crack network like dried mud or split rock.
            let warp = Vec2::new(fbm(q * 3.0 + Vec3::splat(5.0), 2, self.seed ^ 7), fbm(q * 3.0 + Vec3::splat(9.0), 2, self.seed ^ 8)) * 0.25;
            let (edge, _) = cells(p / self.crack_scale + warp, self.seed ^ 0xC3);
            // A crack narrower than the grid would break into dots and
            // zigzags: at least two cells wide.
            let (nx, nz) = self.cells();
            let cell = (self.size[0] / nx as f32).max(self.size[1] / nz as f32);
            let w = self.crack_width.max(cell * 2.0) / self.crack_scale;
            crack = 1.0 - smoothstep(0.0, w, edge);
            h -= self.cracks * crack * crack;
        }
        (h, crack)
    }

    fn offsets(&self) -> Option<Vec<f32>> {
        let (nx, nz) = self.cells();
        self.sculpt.as_ref().filter(|g| g.nx == nx && g.nz == nz).map(Grid::floats)
    }

    /// Heights of every grid vertex, brushes included.
    pub fn heights(&self) -> Vec<f32> {
        let (nx, nz) = self.cells();
        let offsets = self.offsets();
        let mut h = Vec::with_capacity(((nx + 1) * (nz + 1)) as usize);
        for j in 0..=nz {
            for i in 0..=nx {
                let k = (j * (nx + 1) + i) as usize;
                h.push(self.base_height(self.vertex_xz(i, j)).0 + offsets.as_ref().map_or(0.0, |o| o[k]));
            }
        }
        h
    }

    /// Height at a terrain-local XZ point (bilinear between grid vertices).
    #[cfg(test)]
    pub fn height_at(&self, p: Vec2) -> f32 {
        let (nx, nz) = self.cells();
        let h = self.heights();
        let fx = ((p.x / self.size[0] + 0.5) * nx as f32).clamp(0.0, nx as f32);
        let fz = ((p.y / self.size[1] + 0.5) * nz as f32).clamp(0.0, nz as f32);
        let (i, j) = ((fx as u32).min(nx - 1), (fz as u32).min(nz - 1));
        let (tx, tz) = (fx - i as f32, fz - j as f32);
        let at = |i: u32, j: u32| h[(j * (nx + 1) + i) as usize];
        let a = at(i, j) + (at(i + 1, j) - at(i, j)) * tx;
        let b = at(i, j + 1) + (at(i + 1, j + 1) - at(i, j + 1)) * tx;
        a + (b - a) * tz
    }

    pub fn build(&self, library: &[Material], main: Option<&Material>) -> Mesh {
        let (nx, nz) = self.cells();
        let material = main.cloned().unwrap_or_else(|| library.iter().find(|m| m.name == "ground").cloned().unwrap_or_else(ground_material));
        let mut b = Builder::new(vec![material]);
        let h = self.heights();
        let paint = self.paint.as_ref().filter(|g| g.nx == nx && g.nz == nz);
        let (dx, dz) = (self.size[0] / nx as f32, self.size[1] / nz as f32);
        let at = |i: i64, j: i64| h[(j.clamp(0, nz as i64) as u32 * (nx + 1) + i.clamp(0, nx as i64) as u32) as usize];
        let (dirt, rock) = (Vec3::from(self.color), Vec3::from(self.rock_color));
        for j in 0..=nz {
            for i in 0..=nx {
                let (ii, jj) = (i as i64, j as i64);
                let gx = (at(ii + 1, jj) - at(ii - 1, jj)) / (2.0 * dx);
                let gz = (at(ii, jj + 1) - at(ii, jj - 1)) / (2.0 * dz);
                let n = Vec3::new(-gx, 1.0, -gz).normalize();
                let xz = self.vertex_xz(i, j);
                let (_, crack) = self.base_height(xz);
                let slope = smoothstep(0.35, 0.8, 1.0 - n.y);
                let q = Vec3::new(xz.x, 0.0, xz.y);
                let mottle = 1.0 + 0.18 * fbm(q * 1.7, 3, self.seed ^ 0xD1) + 0.08 * fbm(q * 9.0, 2, self.seed ^ 0xD2);
                // Low places collect darker, damper dirt.
                let low = smoothstep(self.height * 0.3, -self.height * 0.5, at(ii, jj));
                let mut c = dirt.lerp(rock, slope) * mottle * (1.0 - 0.25 * low) * (1.0 - 0.6 * crack);
                if let Some(g) = paint {
                    let k = (j * (nx + 1) + i) as usize * 4;
                    let px = &g.data[k..k + 4];
                    let a = px[3] as f32 / 255.0;
                    c = c.lerp(Vec3::new(px[0] as f32, px[1] as f32, px[2] as f32) / 255.0, a);
                }
                b.vertex(Vec3::new(xz.x, at(ii, jj), xz.y), n, c, 0);
            }
        }
        let idx = |i: u32, j: u32| j * (nx + 1) + i;
        for j in 0..nz {
            for i in 0..nx {
                // Counter-clockwise seen from above.
                b.quad(idx(i, j), idx(i, j + 1), idx(i + 1, j + 1), idx(i + 1, j));
            }
        }
        // Outline: the border.
        for i in 0..nx {
            b.edge(idx(i, 0), idx(i + 1, 0));
            b.edge(idx(i, nz), idx(i + 1, nz));
        }
        for j in 0..nz {
            b.edge(idx(0, j), idx(0, j + 1));
            b.edge(idx(nx, j), idx(nx, j + 1));
        }
        b.finish(dx.max(dz) * 2.0)
    }

    /// Applies brush strokes; returns how many grid vertices changed.
    pub fn apply(&mut self, strokes: &[Stroke]) -> Result<usize, String> {
        let (nx, nz) = self.cells();
        let count = ((nx + 1) * (nz + 1)) as usize;
        let mut offsets = self.offsets().unwrap_or_else(|| vec![0.0; count]);
        let mut paint = self.paint.as_ref().filter(|g| g.nx == nx && g.nz == nz).map(|g| g.data.clone()).unwrap_or_else(|| vec![0; count * 4]);
        let mut changed = std::collections::HashSet::new();
        let mut painted = false;
        for (si, s) in strokes.iter().enumerate() {
            let points: Vec<Vec2> = s.points.iter().map(|p| match p.len() {
                2 => Ok(Vec2::new(p[0], p[1])),
                3 => Ok(Vec2::new(p[0], p[2])),
                _ => Err(format!("stroke {si}: points are [x, z] or [x, y, z]")),
            }).collect::<Result<_, _>>()?;
            if points.is_empty() || points.iter().any(|p| !p.is_finite()) {
                return Err(format!("stroke {si} needs finite points"));
            }
            if !(s.radius.is_finite() && s.radius > 0.0 && s.strength.is_finite()) {
                return Err(format!("stroke {si}: radius must be positive and strength finite"));
            }
            let radius = s.radius.min(self.size[0].max(self.size[1]));
            // Dabs every quarter radius along the path.
            let mut dabs = vec![points[0]];
            for w in points.windows(2) {
                let steps = (w[0].distance(w[1]) / (radius * 0.25)).ceil().max(1.0) as usize;
                dabs.extend((1..=steps).map(|k| w[0].lerp(w[1], k as f32 / steps as f32)));
            }
            let heights: Vec<f32> = {
                let base: Vec<f32> = (0..=nz).flat_map(|j| (0..=nx).map(move |i| (i, j))).map(|(i, j)| self.base_height(self.vertex_xz(i, j)).0).collect();
                base.iter().zip(&offsets).map(|(a, b)| a + b).collect()
            };
            let target = s.height.unwrap_or_else(|| {
                let p = points[0];
                let fx = ((p.x / self.size[0] + 0.5) * nx as f32).round().clamp(0.0, nx as f32) as u32;
                let fz = ((p.y / self.size[1] + 0.5) * nz as f32).round().clamp(0.0, nz as f32) as u32;
                heights[(fz * (nx + 1) + fx) as usize]
            });
            // Each vertex gets the strongest falloff of any dab (strokes
            // do not pile up where dabs overlap).
            let mut weight = vec![0.0f32; count];
            for d in &dabs {
                let i0 = (((d.x - radius) / self.size[0] + 0.5) * nx as f32).floor().max(0.0) as u32;
                let i1 = ((((d.x + radius) / self.size[0] + 0.5) * nx as f32).ceil().max(0.0) as u32).min(nx);
                let j0 = (((d.y - radius) / self.size[1] + 0.5) * nz as f32).floor().max(0.0) as u32;
                let j1 = ((((d.y + radius) / self.size[1] + 0.5) * nz as f32).ceil().max(0.0) as u32).min(nz);
                for j in j0..=j1 {
                    for i in i0..=i1 {
                        let t = self.vertex_xz(i, j).distance(*d) / radius;
                        if t < 1.0 {
                            let k = (j * (nx + 1) + i) as usize;
                            let f = 1.0 - smoothstep(0.0, 1.0, t);
                            weight[k] = weight[k].max(f);
                        }
                    }
                }
            }
            let scale = s.scale.unwrap_or(radius * 0.3).max(0.005);
            for k in 0..count {
                let w = weight[k];
                if w <= 0.0 {
                    continue;
                }
                changed.insert(k);
                let (i, j) = (k as u32 % (nx + 1), k as u32 / (nx + 1));
                match s.brush {
                    Brush::Raise => offsets[k] += s.strength * w,
                    Brush::Lower => offsets[k] -= s.strength * w,
                    Brush::Flatten => offsets[k] += (target - heights[k]) * (s.strength.clamp(0.0, 1.0) * w),
                    Brush::Smooth => {
                        let mut sum = 0.0;
                        let mut n = 0.0;
                        for (di, dj) in [(-1i64, 0i64), (1, 0), (0, -1), (0, 1), (-1, -1), (1, 1), (-1, 1), (1, -1)] {
                            let (a, b) = (i as i64 + di, j as i64 + dj);
                            if a >= 0 && b >= 0 && a <= nx as i64 && b <= nz as i64 {
                                sum += heights[(b as u32 * (nx + 1) + a as u32) as usize];
                                n += 1.0;
                            }
                        }
                        offsets[k] += (sum / n - heights[k]) * (s.strength.clamp(0.0, 1.0) * w);
                    }
                    Brush::Noise => {
                        let xz = self.vertex_xz(i, j);
                        offsets[k] += s.strength * w * fbm(Vec3::new(xz.x, 0.0, xz.y) / scale, 3, self.seed ^ si as u32 ^ 0xAB);
                    }
                    Brush::Paint | Brush::Erase => {
                        painted = true;
                        let a = (s.strength.clamp(0.0, 1.0) * w).clamp(0.0, 1.0);
                        let px = &mut paint[k * 4..k * 4 + 4];
                        if s.brush == Brush::Erase {
                            px[3] = (px[3] as f32 * (1.0 - a)) as u8;
                        } else {
                            let c = Vec3::from(s.color.unwrap_or([0.3, 0.27, 0.22])).clamp(Vec3::ZERO, Vec3::ONE) * 255.0;
                            let old = px[3] as f32 / 255.0;
                            let cover = old + a * (1.0 - old);
                            for ch in 0..3 {
                                px[ch] = ((px[ch] as f32 * old * (1.0 - a) + c[ch] * a) / cover.max(1e-6)).round().clamp(0.0, 255.0) as u8;
                            }
                            px[3] = (cover * 255.0).round() as u8;
                        }
                    }
                }
            }
        }
        if offsets.iter().any(|v| *v != 0.0) {
            self.sculpt = Some(Grid::from_floats(nx, nz, &offsets));
        }
        if painted {
            self.paint = Some(Grid { nx, nz, data: paint });
        }
        Ok(changed.len())
    }
}

/// The ground's default material: matte, with stone grit.
pub fn ground_material() -> Material {
    Material { name: "ground".into(), roughness: 0.92, detail: Some("stone".into()), detail_scale: 0.7, detail_strength: 1.0, detail_color: 0.45, ..Material::default() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terrain_builds_sculpts_paints_and_saves() {
        let mut t = Terrain { size: [4.0, 2.0], resolution: 32, cracks: 0.05, ..Terrain::default() };
        t.sanitize();
        assert_eq!(t.cells(), (32, 16));
        let m = t.build(&[], None);
        assert_eq!(m.vertices.len(), 33 * 17);
        let (lo, hi) = m.bounds();
        assert!((lo.x + 2.0).abs() < 1e-4 && (hi.z - 1.0).abs() < 1e-4);
        assert!(m.vertices.iter().all(|v| v.normal[1] > 0.0), "normals point up");
        let before = t.height_at(Vec2::ZERO);
        let raise = Stroke { brush: Brush::Raise, points: vec![vec![0.0, 0.0]], radius: 0.6, strength: 0.3, height: None, color: None, scale: None };
        assert!(t.apply(&[raise]).unwrap() > 0);
        assert!((t.height_at(Vec2::ZERO) - before - 0.3).abs() < 0.02);
        assert!((t.height_at(Vec2::new(1.5, 0.0)) - t.base_height(Vec2::new(1.5, 0.0)).0).abs() < 1e-5, "far away is untouched");
        let flat = Stroke { brush: Brush::Flatten, points: vec![vec![0.0, 0.0], vec![1.0, 0.0]], radius: 0.4, strength: 1.0, height: Some(0.0), color: None, scale: None };
        t.apply(&[flat]).unwrap();
        assert!(t.height_at(Vec2::new(0.5, 0.0)).abs() < 0.01);
        let paint = Stroke { brush: Brush::Paint, points: vec![vec![-1.0, 0.0, 0.0]], radius: 0.3, strength: 1.0, height: None, color: Some([1.0, 0.0, 0.0]), scale: None };
        t.apply(&[paint]).unwrap();
        let m = t.build(&[], None);
        let red = m.vertices.iter().min_by(|a, b| Vec2::new(a.pos[0] + 1.0, a.pos[2]).length().total_cmp(&Vec2::new(b.pos[0] + 1.0, b.pos[2]).length())).unwrap();
        assert!(red.color[0] > 0.9 && red.color[1] < 0.1, "{:?}", red.color);
        let back: Terrain = serde_json::from_str(&serde_json::to_string(&t).unwrap()).unwrap();
        assert_eq!(back, t);
    }
}
