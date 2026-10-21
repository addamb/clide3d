//! Deterministic bounded Eulerian fire. Semi-Lagrangian transport, projected
//! velocity, buoyancy and vorticity confinement; combustion is an artist model.
use crate::progress::Progress;
use glam::{Mat4, Vec3};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

pub const PRESETS: [&str; 8] = [
    "torch",
    "candle",
    "campfire",
    "brazier",
    "bonfire",
    "burning_debris",
    "smoke_column",
    "embers",
];
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    pub preset: String,
    pub resolution: usize,
    /// Old scenes retain their original domain when this field is absent.
    #[serde(default)]
    pub tight_domain: bool,
    pub radius: f32,
    pub fuel: f32,
    pub smoke: f32,
    pub intensity: f32,
    /// Art-directed multiplier for the emission-weighted scene light estimate.
    #[serde(default = "default_light_gain")]
    pub light_gain: f32,
    pub temperature: f32,
    pub wind: Vec3,
    pub seed: u32,
    /// Optional emitter samples in local space: sphere centers or mesh vertices.
    pub points: Vec<Vec3>,
    pub fps: f32,
    pub start: i32,
    pub frames: usize,
    pub warmup: f32,
}
fn default_light_gain() -> f32 {
    1.0
}
impl Settings {
    pub fn preset(name: &str) -> Result<Self, String> {
        if !PRESETS.contains(&name) {
            return Err(format!("fire preset must be one of {PRESETS:?}"));
        }
        let (radius, smoke, fuel) = match name {
            "candle" => (0.008, 0.02, 1.0),
            "brazier" => (0.14, 0.3, 1.2),
            "campfire" => (0.22, 0.4, 1.3),
            "bonfire" => (0.5, 0.5, 1.5),
            "burning_debris" => (0.28, 0.8, 1.2),
            "smoke_column" => (0.2, 6.0, 0.0),
            "embers" => (0.08, 0.0, 0.0),
            _ => (0.045, 0.16, 1.0),
        };
        Ok(Self {
            preset: name.into(),
            resolution: 32,
            tight_domain: true,
            radius,
            fuel,
            smoke,
            intensity: 7.0,
            light_gain: match name {
                "candle" => 2.4,
                "torch" => 0.8,
                "brazier" => 0.4,
                "campfire" => 0.12,
                "bonfire" => 0.08,
                "burning_debris" => 0.16,
                _ => 1.0,
            },
            temperature: 1.0,
            wind: Vec3::ZERO,
            seed: 1,
            points: if matches!(name, "campfire" | "brazier" | "bonfire" | "burning_debris") {
                // Overlapping roots form one bed; eight peripheral sources
                // separate into tongues as the rising gases cool.
                std::iter::once(Vec3::ZERO)
                    .chain((0..8).map(|i| {
                        let a = i as f32 * std::f32::consts::TAU / 8.0;
                        Vec3::new(a.cos(), 0.0, a.sin()) * radius * 1.15
                    }))
                    .collect()
            } else {
                Vec::new()
            },
            fps: 24.0,
            start: 0,
            frames: 24,
            warmup: 1.5,
        })
    }
    pub fn validate(&self) -> Result<(), String> {
        if !PRESETS.contains(&self.preset.as_str())
            || !(12..=128).contains(&self.resolution)
            || !(1..=96).contains(&self.frames)
            || self.points.len() > 256
            || !(0.002..=4.0).contains(&self.radius)
            || !(1.0..=120.0).contains(&self.fps)
            || !(0.0..=5.0).contains(&self.fuel)
            || !(0.0..=10.0).contains(&self.smoke)
            || !(0.0..=1000.0).contains(&self.intensity)
            || !(0.0..=500.0).contains(&self.light_gain)
            || !(0.2..=3.0).contains(&self.temperature)
            || !(0.0..=10.0).contains(&self.warmup)
            || !self.wind.is_finite()
            || self.wind.length() > 10.0
            || self
                .points
                .iter()
                .any(|p| !p.is_finite() || p.abs().max_element() > self.radius * 3.0)
            || self.resolution.pow(3) * 2 * self.frames > 16_777_216
        {
            return Err("invalid fire settings or cache exceeds 256 MiB; resolution 12..128, frames 1..96, reduce resolution/frames".into());
        }
        Ok(())
    }
    pub fn bounds(&self) -> (Vec3, Vec3) {
        if !self.tight_domain {
            return (
                Vec3::new(-4.0, -0.5, -4.0) * self.radius,
                Vec3::new(4.0, 15.5, 4.0) * self.radius,
            );
        }
        // Tight emitter-local box follows the prop transform. Expand for custom
        // sources, retaining cubic voxels and the 1:2:1 grid aspect ratio.
        let half = self.points.iter().fold(2.5f32, |a, p| {
            a.max(p.x.abs() / self.radius + 1.0)
                .max(p.z.abs() / self.radius + 1.0)
        });
        let bottom = self
            .points
            .iter()
            .fold(-0.5f32, |a, p| a.min(p.y / self.radius - 0.5));
        (
            Vec3::new(-half, bottom, -half) * self.radius,
            Vec3::new(half, bottom + half * 4.0, half) * self.radius,
        )
    }
}
impl Default for Settings {
    fn default() -> Self {
        Self::preset("torch").unwrap()
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Frame {
    /// fuel, excess temperature / 2600K, smoke, soot (nonnegative).
    pub cells: Vec<[f32; 4]>,
    pub embers: Vec<[f32; 4]>,
    #[serde(default)]
    pub light: [f32; 6],
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Fire {
    pub settings: Settings,
    #[serde(with = "cache_serde")]
    pub cache: Arc<Vec<Frame>>,
}
mod cache_serde {
    use super::*;
    pub fn serialize<S: serde::Serializer>(v: &Arc<Vec<Frame>>, s: S) -> Result<S::Ok, S::Error> {
        v.as_ref().serialize(s)
    }
    pub fn deserialize<'de, D: serde::Deserializer<'de>>(
        d: D,
    ) -> Result<Arc<Vec<Frame>>, D::Error> {
        Vec::<Frame>::deserialize(d).map(Arc::new)
    }
}
impl PartialEq for Fire {
    fn eq(&self, other: &Self) -> bool {
        self.settings == other.settings
            && (Arc::ptr_eq(&self.cache, &other.cache) || self.cache == other.cache)
    }
}
impl Fire {
    pub fn frame_index(&self, frame: Option<f32>) -> usize {
        ((frame.unwrap_or(self.settings.start as f32) - self.settings.start as f32)
            .max(0.0)
            .floor() as usize)
            .min(self.cache.len().saturating_sub(1))
    }
    pub fn valid(&self) -> bool {
        self.settings.validate().is_ok()
            && self.cache.len() == self.settings.frames
            && self.cache.iter().all(|f| {
                f.cells.len() == self.settings.resolution.pow(3) * 2
                    && f.cells.iter().flatten().all(|v| v.is_finite() && *v >= 0.0)
                    && f.embers.iter().all(|p| p.iter().all(|v| v.is_finite()))
            })
    }
    pub fn volume(&self, world: Mat4, frame: Option<f32>) -> Option<Volume> {
        if !self.valid() || world.determinant().abs() < 1e-10 {
            return None;
        }
        let (lo, hi) = self.settings.bounds();
        let n = self.settings.resolution;
        let f = &self.cache[self.frame_index(frame)];
        let h = (hi.x - lo.x) / n as f32;
        let cells: Vec<[f32; 8]> = f
            .cells
            .iter()
            .enumerate()
            .map(|(i, c)| {
                let heat = c[1];
                let y = lo.y + (((i / n) % (2 * n)) as f32 + 0.5) * h;
                let e = emission(heat, y, &self.settings) * (-c[3] * 2.0).exp();
                // Hot gas has an effective absorption matched to its radiance:
                // the root reads as a continuous luminous body through smoke.
                let sigma = (c[2] * 1.4 + c[3] * 2.5 + heat.powi(3) * 2.5) / self.settings.radius;
                let albedo = if self.settings.preset == "smoke_column" {
                    0.55
                } else {
                    0.10
                };
                let scatter = c[2] * albedo * (1.0 - heat.min(1.0)).powi(2)
                    / self.settings.radius
                    / (1.0 + c[3] * 8.0);
                // Mixture fraction is transported fuel versus burnt hot gas.
                let mixture = c[0] / (c[0] + c[1] * 0.12 + 0.0001);
                [e.x, e.y, e.z, sigma, scatter, scatter, scatter, mixture]
            })
            .collect();
        Some(Volume {
            detail: [
                self.settings.radius,
                self.settings.warmup + self.frame_index(frame) as f32 / self.settings.fps,
                if self.settings.preset == "candle" {
                    1.0
                } else {
                    0.0
                },
                (self.settings.seed % 64) as f32 * 0.17,
            ],
            inverse: world.inverse(),
            light_gain: self.settings.light_gain,
            lo,
            extent: hi - lo,
            dims: [n, 2 * n, n],
            cells,
            step: h * world.to_scale_rotation_translation().0.abs().min_element() * 0.5,
        })
    }
    /// Sub-voxel sparks are tiny emissive octahedra, not hot voxel cubes.
    pub fn ember_mesh(&self, frame: Option<f32>) -> crate::mesh::Mesh {
        use crate::material::Material;
        use crate::mesh::{Mesh, Vertex};
        let m = Material {
            name: "flame".into(),
            emission: 18.0,
            roughness: 1.0,
            ..Material::default()
        };
        let mut mesh = Mesh {
            materials: vec![m.clone()],
            ..Mesh::default()
        };
        if !self.valid() {
            return mesh;
        }
        let radius = self.settings.radius * 0.022;
        for p in &self.cache[self.frame_index(frame)].embers {
            let center = Vec3::new(p[0], p[1], p[2]);
            let color = crate::envkit::light::blackbody(p[3])
                .powf(1.0 / 2.2)
                .to_array();
            let base = mesh.vertices.len() as u32;
            for axis in [Vec3::X, -Vec3::X, Vec3::Y, -Vec3::Y, Vec3::Z, -Vec3::Z] {
                mesh.vertices.push(Vertex {
                    pos: (center + axis * radius).to_array(),
                    normal: axis.to_array(),
                    color,
                    surface: m.surface(),
                    material: 0,
                    uv: [0.0; 2],
                    tangent: [0.0; 4],
                    light: m.light(),
                    base: [color[0], color[1], color[2], -1.0],
                    wear: [0.0; 3],
                    painted: [0; 4],
                });
            }
            mesh.indices.extend(
                [
                    0, 2, 4, 4, 2, 1, 1, 2, 5, 5, 2, 0, 0, 4, 3, 4, 1, 3, 1, 5, 3, 5, 0, 3,
                ]
                .map(|i| i + base),
            );
        }
        mesh
    }
}
/// Charred fuel prop with an independent emissive fissure map. Scale and
/// arrange several instances under campfire emitters; no flame geometry.
pub fn charred_log(seed: u32) -> crate::mesh::Mesh {
    use crate::{
        material::Material,
        mesh::{ImportedMaterial, Mesh, Textures},
        scene::Shape,
    };
    let mut mesh = Mesh::build(Shape::Cylinder);
    let m = Material {
        name: "charred_wood".into(),
        roughness: 0.95,
        ..Default::default()
    };
    mesh.materials = vec![m.clone()];
    for v in &mut mesh.vertices {
        let p = Vec3::from(v.pos);
        let a = p.z.atan2(p.x);
        let rough = 1.0 + crate::envkit::noise(p * 12.0, seed) * 0.035;
        v.pos = [p.x * rough, p.y, p.z * rough];
        v.uv = [a / std::f32::consts::TAU + 0.5, p.y + 0.5];
        v.tangent = [-a.sin(), 0.0, a.cos(), 1.0];
        v.surface = m.surface();
        v.color = [1.0; 3];
        v.material = 0;
    }
    let size = 256usize;
    let mut color = Vec::with_capacity(size * size * 4);
    let mut glow = Vec::with_capacity(size * size * 4);
    for y in 0..size {
        for x in 0..size {
            let u = x as f32 / size as f32;
            let v = y as f32 / size as f32;
            let grain = crate::envkit::noise(Vec3::new(u * 70.0, v * 9.0, 0.0), seed);
            let split = (u * 85.0 + (v * 19.0).sin() * 0.9 + grain * 0.3)
                .sin()
                .abs();
            let hot =
                ((0.12 - split) * 12.0).clamp(0.0, 1.0) * (0.6 + 0.4 * (v * 27.0 + u * 9.0).sin());
            let ash = (22.0 + grain * 12.0).max(4.0) as u8;
            color.extend_from_slice(&[ash, ash, ash, 255]);
            glow.extend_from_slice(&[
                (hot * 255.0) as u8,
                (hot * 90.0) as u8,
                (hot * 12.0) as u8,
                255,
            ]);
        }
    }
    mesh.textures = Some(Arc::new(Textures {
        size: size as u32,
        color,
        normal: [128, 128, 255, 255].repeat(size * size),
        orm: [255, 240, 0, 255].repeat(size * size),
        alpha: false,
    }));
    mesh.imported = Some(Arc::new(ImportedMaterial {
        emissive: [5.0; 3],
        emissive_texture: Some(glow),
        size: size as u32,
        base_alpha: 1.0,
        wraps: [[10497; 2]; 5],
        ..Default::default()
    }));
    mesh
}

// Smooth, filled lower flame blends into a reaction sheet near the tips.
// Keep this formula identical to fire_at in trace.wgsl.
fn reaction_weight(mixture: f32, height: f32) -> f32 {
    let band = (1.0 - ((mixture - 0.35) / 0.5).abs()).max(0.0);
    let core = (1.0 - height.max(0.0) / 2.5).clamp(0.0, 1.0);
    (band * band * 2.5).max(core * 2.5)
}

fn emission(heat: f32, height: f32, s: &Settings) -> Vec3 {
    let t = ((heat * s.temperature - 0.12) / 0.88).clamp(0.0, 1.4);
    let h = height.max(0.0) / s.radius;
    let kelvin = (1000.0 + 2600.0 * t).min((3600.0 - h * 500.0).max(1000.0));
    crate::envkit::light::blackbody(kelvin)
        * s.intensity
        * 3.0
        * (1.0 + 12.0 * (-h * h * 3.0).exp())
        * t.powi(5)
        * (kelvin / 3000.0).powi(3)
        / s.radius
}
fn index(n: usize, x: usize, y: usize, z: usize) -> usize {
    (z * 2 * n + y) * n + x
}
fn sample<const K: usize>(a: &[[f32; K]], n: usize, p: Vec3) -> [f32; K] {
    if p.min_element() < 0.0
        || p.x > n as f32 - 1.0
        || p.y > (2 * n) as f32 - 1.0
        || p.z > n as f32 - 1.0
    {
        return [0.0; K];
    }
    let q = p.floor();
    let f = p - q;
    let [x, y, z] = [q.x as usize, q.y as usize, q.z as usize];
    let mut out = [0.0; K];
    for dz in 0..2 {
        for dy in 0..2 {
            for dx in 0..2 {
                let w = if dx == 0 { 1.0 - f.x } else { f.x }
                    * if dy == 0 { 1.0 - f.y } else { f.y }
                    * if dz == 0 { 1.0 - f.z } else { f.z };
                let c = a[index(
                    n,
                    (x + dx).min(n - 1),
                    (y + dy).min(2 * n - 1),
                    (z + dz).min(n - 1),
                )];
                for k in 0..K {
                    out[k] += c[k] * w;
                }
            }
        }
    }
    out
}

/// Independent z slabs use scoped workers; small previews stay serial.
fn grid<T: Send>(out: &mut [T], n: usize, f: impl Fn(usize, usize, usize, usize, &mut T) + Sync) {
    let slab = n * n * 2;
    let workers = if n >= 48 {
        std::thread::available_parallelism()
            .map_or(1, usize::from)
            .min(8)
    } else {
        1
    };
    if workers == 1 {
        for z in 1..n - 1 {
            for y in 1..2 * n - 1 {
                for x in 1..n - 1 {
                    let i = index(n, x, y, z);
                    f(i, x, y, z, &mut out[i]);
                }
            }
        }
        return;
    }
    let chunk = n.div_ceil(workers) * slab;
    std::thread::scope(|scope| {
        for (block, cells) in out.chunks_mut(chunk).enumerate() {
            let f = &f;
            scope.spawn(move || {
                for (j, cell) in cells.iter_mut().enumerate() {
                    let i = block * chunk + j;
                    let x = i % n;
                    let y = (i / n) % (2 * n);
                    let z = i / slab;
                    if x > 0 && x < n - 1 && y > 0 && y < 2 * n - 1 && z > 0 && z < n - 1 {
                        f(i, x, y, z, cell);
                    }
                }
            });
        }
    });
}

pub fn simulate(settings: Settings, progress: &Progress) -> Result<Fire, String> {
    settings.validate()?;
    let n = settings.resolution;
    let len = n * n * n * 2;
    let (lo, hi) = settings.bounds();
    let h = (hi.x - lo.x) / n as f32;
    let mut a = vec![[0.0; 4]; len];
    let mut u = vec![[0.0; 3]; len];
    let mut cache = Vec::new();
    let dt = 1.0 / 120.0;
    let warm = (settings.warmup / dt).ceil() as usize;
    let total = warm + (settings.frames as f32 / settings.fps / dt).ceil() as usize;
    let mut next = 0;
    let mut particles: Vec<[f32; 4]> = Vec::new();
    progress.stage("Advecting fuel, temperature and smoke", 0.0);
    for step in 0..=total {
        progress.check()?;
        progress.set(step as f32 / total.max(1) as f32);
        if step >= warm && cache.len() < settings.frames && step - warm >= next {
            let mut light = [0.0; 6];
            let mut weight = 0.0;
            for z in 0..n {
                for y in 0..2 * n {
                    for x in 0..n {
                        let e = emission(
                            a[index(n, x, y, z)][1],
                            lo.y + (y as f32 + 0.5) * h,
                            &settings,
                        ) * (h * h * h);
                        let w = e.max_element();
                        let p =
                            lo + (Vec3::new(x as f32, y as f32, z as f32) + Vec3::splat(0.5)) * h;
                        for k in 0..3 {
                            light[k] += p[k] * w;
                            light[k + 3] += e[k];
                        }
                        weight += w;
                    }
                }
            }
            if weight > 0.0 {
                for x in &mut light[..3] {
                    *x /= weight;
                }
            }
            cache.push(Frame {
                cells: a.clone(),
                embers: particles.clone(),
                light,
            });
            next = (cache.len() as f32 / settings.fps / dt).round() as usize;
        }
        if cache.len() == settings.frames {
            break;
        }
        let time = step as f32 * dt;
        let mut advected = vec![[0.0; 4]; len];
        grid(&mut advected, n, |i, x, y, z, cell| {
            let q = Vec3::new(x as f32, y as f32, z as f32);
            *cell = sample(&a, n, q - Vec3::from(u[i]) * (dt / h));
        });
        let mut reacted = vec![([0.0; 4], [0.0; 3]); len];
        grid(&mut reacted, n, |i, x, _y, z, cell| {
            let y = (i / n) % (2 * n);
            let q = Vec3::new(x as f32, y as f32, z as f32);
            let back = q - Vec3::from(u[i]) * (dt / h);
            // Limited MacCormack correction preserves thin hot tongues without
            // ringing or creating negative fuel/density at the boundary.
            let reverse = sample(&advected, n, q + Vec3::from(u[i]) * (dt / h));
            let base = back.floor().as_ivec3();
            let mut low = [f32::MAX; 4];
            let mut high = [0.0f32; 4];
            for dz in 0..2 {
                for dy in 0..2 {
                    for dx in 0..2 {
                        let at = base + glam::IVec3::new(dx, dy, dz);
                        let c = if at.min_element() < 0
                            || at.x >= n as i32
                            || at.y >= 2 * n as i32
                            || at.z >= n as i32
                        {
                            [0.0; 4]
                        } else {
                            a[index(n, at.x as usize, at.y as usize, at.z as usize)]
                        };
                        for k in 0..4 {
                            low[k] = low[k].min(c[k]);
                            high[k] = high[k].max(c[k]);
                        }
                    }
                }
            }
            for k in 0..4 {
                cell.0[k] = (advected[i][k] + 0.5 * (a[i][k] - reverse[k])).clamp(low[k], high[k]);
            }
            cell.1 = sample(&u, n, back);
            let p = lo + (q + Vec3::splat(0.5)) * h;
            let source = if settings.points.is_empty() {
                Vec3::ZERO
            } else {
                *settings
                    .points
                    .iter()
                    .min_by(|a, b| a.distance_squared(p).total_cmp(&b.distance_squared(p)))
                    .unwrap()
            };
            let mut r = (p - source) / settings.radius;
            let steady = settings.preset == "candle";
            let (emit, warp) = if r.y.abs() < 1.2 && r.x * r.x + r.z * r.z < 4.0 {
                let warp = if steady {
                    0.0
                } else {
                    crate::envkit::noise(r * 2.5 + Vec3::Y * time * 3.7, settings.seed ^ 41)
                };
                r.x += warp * 0.32;
                r.z += crate::envkit::noise(r * 2.5 + Vec3::Z * time * 4.3, settings.seed ^ 93)
                    * if steady { 0.0 } else { 0.32 };
                let flicker = 0.8
                    + 0.65
                        * crate::envkit::noise(
                            Vec3::new(time * 4.5, x as f32 * 0.2, z as f32 * 0.2),
                            settings.seed,
                        );
                let width = if steady {
                    4.0
                } else if settings.points.is_empty() {
                    2.0
                } else {
                    7.0
                };
                let emit = (-(r.x * r.x + r.z * r.z) * width).exp()
                    * (-r.y * r.y * 6.0).exp()
                    * if steady { 1.0 } else { flicker };
                (if emit > 0.001 { emit } else { 0.0 }, warp)
            } else {
                (0.0, 0.0)
            };
            if emit == 0.0 && cell.0.iter().all(|v| *v < 1e-6) {
                cell.0 = [0.0; 4];
                cell.1 = [0.0; 3];
                return;
            }
            cell.0[0] += (emit * settings.fuel * 8.0 * dt).max(0.0);
            let burn_rate = if steady { 3.0 } else { 2.4 };
            let height = (p.y / settings.radius).max(0.0);
            let cooling = if steady {
                4.5
            } else {
                1.8 + height.powi(2) * 0.85 + warp * 0.5
            };
            let burn = cell.0[0].min(dt * burn_rate * (0.2 + cell.0[1]));
            cell.0[0] -= burn;
            cell.0[1] = (cell.0[1] + burn * 4.0 + emit * dt * 4.0 * settings.fuel.min(1.0))
                .min(1.15)
                * (-dt * cooling).exp();
            if settings.preset == "smoke_column" {
                cell.0[1] *= 0.3;
            }
            if settings.preset == "embers" {
                cell.0[1] = 0.0;
            }
            cell.0[2] = (cell.0[2]
                + (burn * 0.22
                    + emit
                        * dt
                        * if settings.preset == "smoke_column" {
                            12.0
                        } else {
                            0.4
                        })
                    * settings.smoke)
                * (-dt * 0.3).exp();
            cell.0[3] = (cell.0[3] + burn * settings.smoke * 0.06) * (-dt * 0.55).exp();
            cell.0[0] *= (-dt * (0.12 + height.powi(2) * 0.7)).exp();
            // Open outflow sponge: hot fuel must cool before leaving a tight
            // domain, otherwise hero tongues end in a visible flat box cap.
            if p.y > hi.y - settings.radius * 1.5 {
                let edge = ((p.y - hi.y) / settings.radius / 1.5 + 1.0)
                    .clamp(0.0, 1.0)
                    .powi(2);
                cell.0[0] *= (-dt * edge * 16.0).exp();
                cell.0[1] *= (-dt * edge * 60.0).exp();
                cell.0[2] *= (-dt * edge * 8.0).exp();
                cell.0[3] *= (-dt * edge * 8.0).exp();
            }
            let noise = Vec3::new(
                crate::envkit::noise(
                    p / settings.radius * 2.0 + Vec3::splat(time * 3.0),
                    settings.seed,
                ),
                0.0,
                crate::envkit::noise(
                    p / settings.radius * 2.0 + Vec3::splat(time * 2.7),
                    settings.seed ^ 71,
                ),
            );
            let buoy = Vec3::Y
                * (cell.0[1] * 55.0
                    + emit * 2.0
                    + if settings.preset == "smoke_column" {
                        cell.0[2] * 40.0
                    } else {
                        -cell.0[2] * 0.08
                    });
            cell.1 = ((Vec3::from(cell.1)
                + (buoy
                    + noise
                        * if settings.preset == "candle" {
                            0.0
                        } else {
                            6.0
                        })
                    * settings.radius
                    * dt
                + settings.wind * dt)
                * (-dt * 0.6).exp())
            .clamp(
                Vec3::splat(-settings.radius * 12.0),
                Vec3::splat(settings.radius * 12.0),
            )
            .to_array();
            if settings.preset == "smoke_column" {
                cell.1[1] = cell.1[1].max(emit * settings.radius * 12.0);
            }
        });
        let (b, mut v): (Vec<_>, Vec<_>) = reacted.into_iter().unzip();
        // Curl and confinement recover rolling detail lost to interpolation.
        let mut curl = vec![Vec3::ZERO; len];
        grid(&mut curl, n, |_i, x, y, z, cell| {
            let dx = (Vec3::from(v[index(n, x + 1, y, z)]) - Vec3::from(v[index(n, x - 1, y, z)]))
                / (2.0 * h);
            let dy = (Vec3::from(v[index(n, x, y + 1, z)]) - Vec3::from(v[index(n, x, y - 1, z)]))
                / (2.0 * h);
            let dz = (Vec3::from(v[index(n, x, y, z + 1)]) - Vec3::from(v[index(n, x, y, z - 1)]))
                / (2.0 * h);
            *cell = Vec3::new(dy.z - dz.y, dz.x - dx.z, dx.y - dy.x);
        });
        grid(&mut v, n, |i, x, y, z, cell| {
            let g = Vec3::new(
                curl[index(n, x + 1, y, z)].length() - curl[index(n, x - 1, y, z)].length(),
                curl[index(n, x, y + 1, z)].length() - curl[index(n, x, y - 1, z)].length(),
                curl[index(n, x, y, z + 1)].length() - curl[index(n, x, y, z - 1)].length(),
            )
            .normalize_or_zero();
            *cell = (Vec3::from(*cell) + g.cross(curl[i]) * (h * 5.0 * dt)).to_array();
        });
        // Jacobi pressure projection, open zero-pressure domain boundaries.
        let mut div = vec![0.0; len];
        let mut pressure = vec![0.0; len];
        grid(&mut div, n, |_i, x, y, z, cell| {
            *cell = (v[index(n, x + 1, y, z)][0] - v[index(n, x - 1, y, z)][0]
                + v[index(n, x, y + 1, z)][1]
                - v[index(n, x, y - 1, z)][1]
                + v[index(n, x, y, z + 1)][2]
                - v[index(n, x, y, z - 1)][2])
                * h
                * 0.5;
        });
        for _ in 0..10 {
            let mut q = vec![0.0; len];
            grid(&mut q, n, |i, x, y, z, cell| {
                *cell = (pressure[index(n, x + 1, y, z)]
                    + pressure[index(n, x - 1, y, z)]
                    + pressure[index(n, x, y + 1, z)]
                    + pressure[index(n, x, y - 1, z)]
                    + pressure[index(n, x, y, z + 1)]
                    + pressure[index(n, x, y, z - 1)]
                    - div[i])
                    / 6.0;
            });
            pressure = q;
        }
        grid(&mut v, n, |_i, x, y, z, cell| {
            let g = Vec3::new(
                pressure[index(n, x + 1, y, z)] - pressure[index(n, x - 1, y, z)],
                pressure[index(n, x, y + 1, z)] - pressure[index(n, x, y - 1, z)],
                pressure[index(n, x, y, z + 1)] - pressure[index(n, x, y, z - 1)],
            ) / (2.0 * h);
            *cell = (Vec3::from(*cell) - g).to_array();
        });
        particles.retain_mut(|p| {
            let pos = Vec3::new(p[0], p[1], p[2]);
            let flow = Vec3::from(sample(&v, n, (pos - lo) / h - Vec3::splat(0.5)));
            let pos = pos + (flow + Vec3::Y * settings.radius * 3.0 + settings.wind) * dt;
            p[0] = pos.x;
            p[1] = pos.y;
            p[2] = pos.z;
            p[3] -= dt * 500.0;
            p[3] > 900.0 && pos.y < hi.y
        });
        if step % 8 == 0
            && (settings.fuel > 0.0 || settings.preset == "embers")
            && !matches!(settings.preset.as_str(), "smoke_column" | "candle")
            && particles.len() < 96
        {
            let rand = |k| crate::envkit::noise(Vec3::new(step as f32, k, 0.7), settings.seed);
            particles.push([
                rand(1.0) * settings.radius,
                settings.radius * 0.2,
                rand(3.0) * settings.radius,
                2400.0,
            ]);
        }
        a = b;
        u = v;
    }
    progress.check()?;
    Ok(Fire {
        settings,
        cache: Arc::new(cache),
    })
}

/// Analytic divergence-free curl of a periodic vector potential. Two advected
/// octaves supply lookup detail below the solver grid (mirrored in WGSL).
fn detail_curl(p: Vec3, t: f32) -> Vec3 {
    let q = p + Vec3::new(t, -t * 1.3, t * 0.7);
    Vec3::new(
        q.y.cos() * q.x.sin() - q.z.cos() * q.x.sin(),
        q.z.cos() * q.y.sin() - q.x.cos() * q.y.sin(),
        q.x.cos() * q.z.sin() - q.y.cos() * q.z.sin(),
    )
}

/// Ready-to-trace cell coefficients, shared by CPU and WGSL (no color fit drift).
#[derive(Clone)]
pub struct Volume {
    pub detail: [f32; 4],
    pub inverse: Mat4,
    pub light_gain: f32,
    pub lo: Vec3,
    pub extent: Vec3,
    pub dims: [usize; 3],
    pub cells: Vec<[f32; 8]>,
    pub step: f32,
}
impl Volume {
    pub fn interval(&self, o: Vec3, d: Vec3, far: f32) -> Option<(f32, f32)> {
        let o = self.inverse.transform_point3(o);
        let d = self.inverse.transform_vector3(d);
        let mut a = 0.0f32;
        let mut b = far;
        for k in 0..3 {
            if d[k].abs() < 1e-9 {
                if o[k] < self.lo[k] || o[k] > self.lo[k] + self.extent[k] {
                    return None;
                }
            } else {
                let t0 = (self.lo[k] - o[k]) / d[k];
                let t1 = (self.lo[k] + self.extent[k] - o[k]) / d[k];
                a = a.max(t0.min(t1));
                b = b.min(t0.max(t1));
            }
        }
        (b > a).then_some((a, b))
    }
    pub fn at(&self, p: Vec3) -> [f32; 8] {
        let local = self.inverse.transform_point3(p);
        let dims = Vec3::new(
            self.dims[0] as f32,
            self.dims[1] as f32,
            self.dims[2] as f32,
        );
        let lookup = |p| {
            sample(
                &self.cells,
                self.dims[0],
                (p - self.lo) / self.extent * dims - Vec3::splat(0.5),
            )
        };
        let coarse = lookup(local);
        let energy = (coarse[0] * self.detail[0] + coarse[3] * self.detail[0]).min(1.0);
        let r = local / self.detail[0];
        let phase = self.detail[1] * 3.0 + self.detail[3];
        let strength = if self.detail[2] > 0.5 {
            0.015
        } else {
            0.03 + 0.32 * (r.y.max(0.0) / 2.0).clamp(0.0, 1.0)
        };
        let displacement = (detail_curl(r * 3.0, phase) + detail_curl(r * 7.0, phase * 1.7) * 0.35)
            * (self.detail[0] * strength * energy);
        let mut c = lookup(local + displacement);
        let base = (1.0 - r.y.max(0.0) * 1.8).max(0.0);
        let sheet = reaction_weight(c[7], r.y);
        for e in &mut c[..3] {
            *e *= sheet;
        }
        if self.detail[2] > 0.5 {
            let blue = base * (1.0 - base) * 4.0;
            let energy = c[0];
            c[0] *= 1.0 - blue * 0.85;
            c[1] *= 1.0 - blue * 0.6;
            c[2] += energy * blue * 0.85;
        }
        c
    }
    pub fn lights(&self) -> Vec<crate::film::lighting::Light> {
        let mut power = [Vec3::ZERO; 4];
        let mut pos = [Vec3::ZERO; 4];
        let mut weight = [0.0; 4];
        let world = self.inverse.inverse();
        let cell = self.extent
            / Vec3::new(
                self.dims[0] as f32,
                self.dims[1] as f32,
                self.dims[2] as f32,
            );
        let dv = cell.x * cell.y * cell.z * world.determinant().abs();
        for z in 0..self.dims[2] {
            for y in 0..self.dims[1] {
                for x in 0..self.dims[0] {
                    let c = self.cells[index(self.dims[0], x, y, z)];
                    let height = (self.lo.y + (y as f32 + 0.5) * cell.y) / self.detail[0];
                    let sheet = reaction_weight(c[7], height);
                    let e = Vec3::new(c[0], c[1], c[2]) * (dv * sheet);
                    let w = e.max_element();
                    let k = (y * 4 / self.dims[1]).min(3);
                    let p = world.transform_point3(
                        self.lo
                            + (Vec3::new(x as f32, y as f32, z as f32) + Vec3::splat(0.5)) * cell,
                    );
                    power[k] += e;
                    pos[k] += p * w;
                    weight[k] += w;
                }
            }
        }
        (0..4)
            .filter(|&i| weight[i] > 1e-7)
            .map(|i| crate::film::lighting::Light::Sphere {
                position: pos[i] / weight[i],
                radius: (self.step * 2.0).max(
                    self.detail[0]
                        * world.to_scale_rotation_translation().0.abs().min_element()
                        * 0.5,
                ),
                power: power[i] * self.light_gain,
            })
            .collect()
    }
}
/// Front-to-back quadrature; skips empty spaces between volume bounds. Overlaps
/// sum coefficients before integration. `shade` supplies incident smoke light.
pub fn march(
    volumes: &[Volume],
    o: Vec3,
    d: Vec3,
    far: f32,
    shade: impl FnMut(Vec3, Vec3) -> Vec3,
) -> (Vec3, f32) {
    march_with_fog(volumes, o, d, far, shade, None)
}
pub fn march_with_fog(
    volumes: &[Volume],
    o: Vec3,
    d: Vec3,
    far: f32,
    mut shade: impl FnMut(Vec3, Vec3) -> Vec3,
    fog: Option<crate::film::lighting::Fog>,
) -> (Vec3, f32) {
    let mut cursor = 0.0;
    let mut e = Vec3::ZERO;
    let mut tr = 1.0;
    for _ in 0..volumes.len() {
        let next = volumes
            .iter()
            .filter_map(|v| {
                v.interval(o, d, far)
                    .map(|(a, b)| (a.max(cursor), b, v.step))
            })
            .filter(|(a, b, _)| b > a)
            .min_by(|a, b| a.0.total_cmp(&b.0));
        let Some((start, end, h)) = next else { break };
        let count = ((end - start) / h.max(1e-6)).ceil().clamp(1.0, 512.0) as usize;
        let ds = (end - start) / count as f32;
        // Shadowed incident radiance varies more slowly than flame sheets.
        // Reuse it over four half-voxel steps, matching the GPU quadrature.
        let mut incident = Vec3::ZERO;
        let mut shaded = false;
        for i in 0..count {
            let p = o + d * (start + (i as f32 + 0.5) * ds);
            let mut c = [0.0; 8];
            for v in volumes {
                let a = v.at(p);
                for k in 0..8 {
                    c[k] += a[k];
                }
            }
            let sigma = c[3];
            let t = (-sigma * ds).exp();
            let integ = if sigma > 1e-6 { (1.0 - t) / sigma } else { ds };
            if c[4] > 0.0 && (!shaded || i % 4 == 0) {
                incident = shade(p, d);
                shaded = true;
            }
            let source = Vec3::new(c[0], c[1], c[2]) + Vec3::new(c[4], c[5], c[6]) * incident;
            e += source
                * (tr
                    * integ
                    * fog.map_or(1.0, |f| {
                        f.transmittance(o, d, start + (i as f32 + 0.5) * ds)
                    }));
            tr *= t;
        }
        cursor = end;
    }
    (e, tr)
}
pub fn transmittance(v: &[Volume], o: Vec3, d: Vec3, far: f32) -> f32 {
    let mut tau = 0.0;
    for v in v {
        if let Some((a, b)) = v.interval(o, d, far) {
            let ds = (b - a) / 16.0;
            for i in 0..16 {
                tau += v.at(o + d * (a + (i as f32 + 0.5) * ds))[3] * ds;
            }
        }
    }
    (-tau).exp()
}
pub fn phase(c: f32) -> f32 {
    let g = 0.25;
    (1.0 - g * g) / (4.0 * std::f32::consts::PI * (1.0 + g * g - 2.0 * g * c).max(1e-6).powf(1.5))
}

/// GPU packed vec4 buffer: header, 8 vec4 per volume, 2 vec4 per cell.
pub fn pack(volumes: &[Volume]) -> Vec<[f32; 4]> {
    let mut out = vec![[0.0; 4]; 1 + volumes.len() * 8];
    out[0][0] = volumes.len() as f32;
    for (i, v) in volumes.iter().enumerate() {
        let k = 1 + i * 8;
        let cols = v.inverse.to_cols_array_2d();
        out[k..k + 4].copy_from_slice(&cols);
        out[k + 4] = [v.lo.x, v.lo.y, v.lo.z, v.step];
        out[k + 5] = [v.extent.x, v.extent.y, v.extent.z, 0.0];
        out[k + 7] = v.detail;
        out[k + 6] = [
            v.dims[0] as f32,
            v.dims[1] as f32,
            v.dims[2] as f32,
            // Keep large multi-hero buffer addresses exact beyond 2^24.
            f32::from_bits(out.len() as u32),
        ];
        for c in &v.cells {
            out.push([c[0], c[1], c[2], c[3]]);
            out.push([c[4], c[5], c[6], c[7]]);
        }
    }
    out
}

/// Orthographic atlas, rows = views, columns = cached frames. Unassociated
/// alpha and scene-linear emission (tonemapped to sRGB for portable cards).
pub fn atlas(fire: &Fire, tile: usize, views: usize, frames: usize) -> (usize, Vec<u8>) {
    let columns = frames.min(fire.cache.len()).max(1);
    let rows = views.max(1);
    let size = (columns.max(rows) * tile).next_power_of_two();
    let mut pixels = vec![0u8; size * size * 4];
    let (lo, hi) = fire.settings.bounds();
    let center = (lo + hi) * 0.5;
    let span = hi.y - lo.y;
    for f in 0..columns {
        let frame = fire.settings.start as f32
            + (f * (fire.cache.len() - 1) / columns.max(2).saturating_sub(1)) as f32;
        let Some(v) = fire.volume(Mat4::IDENTITY, Some(frame)) else {
            continue;
        };
        for view in 0..rows {
            let angle = view as f32 * std::f32::consts::PI / rows as f32;
            let dir = Vec3::new(angle.sin(), 0.0, angle.cos());
            let right = Vec3::new(dir.z, 0.0, -dir.x);
            for y in 0..tile {
                for x in 0..tile {
                    let o = center
                        + right * ((x as f32 + 0.5) / tile as f32 - 0.5) * span
                        + Vec3::Y * (0.5 - (y as f32 + 0.5) / tile as f32) * span
                        - dir * span;
                    let (mut e, tr) =
                        march(std::slice::from_ref(&v), o, dir, span * 2.0, |_, _| {
                            Vec3::splat(0.015)
                        });
                    for spark in &fire.cache[fire.frame_index(Some(frame))].embers {
                        let p = Vec3::new(spark[0], spark[1], spark[2]);
                        let along = (p - o).dot(dir);
                        let dist = (p - o - dir * along).length();
                        let footprint = span / tile as f32 * 0.55;
                        if along > 0.0 && dist < footprint {
                            e += crate::envkit::light::blackbody(spark[3])
                                * ((1.0 - dist / footprint) * 8.0);
                        }
                    }
                    let a = (1.0 - tr).max((e.max_element() * 0.12).min(1.0));
                    let c = if a > 1e-5 { e / a } else { Vec3::ZERO };
                    let c = (c / (Vec3::ONE + c)).powf(1.0 / 2.2) * 255.0;
                    let k = ((view * tile + y) * size + f * tile + x) * 4;
                    pixels[k..k + 4].copy_from_slice(&[
                        c.x as u8,
                        c.y as u8,
                        c.z as u8,
                        (a * 255.0) as u8,
                    ]);
                }
            }
        }
    }
    (size, pixels)
}
pub fn cards(fire: &Fire, frame: Option<f32>, export: bool) -> crate::mesh::Mesh {
    use crate::material::Material;
    use crate::mesh::{Mesh, Textures, Vertex};
    use std::sync::Arc;
    let frames = if export { fire.cache.len().min(16) } else { 1 };
    let views = if export { 2 } else { 1 };
    let single = Fire {
        settings: Settings {
            start: 0,
            frames: 1,
            ..fire.settings.clone()
        },
        cache: Arc::new(vec![fire.cache[fire.frame_index(frame)].clone()]),
    };
    let (size, color) = atlas(if export { fire } else { &single }, 64, views, frames);
    let t = Textures {
        size: size as u32,
        color,
        normal: [128, 128, 255, 255].repeat(size * size),
        orm: [255, 255, 0, 255].repeat(size * size),
        alpha: true,
    };
    let m = Material {
        name: "fire_flipbook".into(),
        emission: 3.0,
        roughness: 1.0,
        ..Material::default()
    };
    let mut mesh = Mesh {
        materials: vec![m.clone()],
        textures: Some(Arc::new(t)),
        ..Mesh::default()
    };
    let (lo, hi) = fire.settings.bounds();
    let span = hi.y - lo.y;
    for card in 0..2 {
        let base = mesh.vertices.len() as u32;
        let w = if card == 0 { Vec3::X } else { Vec3::Z } * span * 0.5;
        let c = (lo + hi) * 0.5;
        for (p, uv) in [
            (c - w - Vec3::Y * span * 0.5, [0.0, 1.0]),
            (c + w - Vec3::Y * span * 0.5, [1.0, 1.0]),
            (c + w + Vec3::Y * span * 0.5, [1.0, 0.0]),
            (c - w + Vec3::Y * span * 0.5, [0.0, 0.0]),
        ] {
            mesh.vertices.push(Vertex {
                pos: p.to_array(),
                normal: if card == 0 { Vec3::Z } else { -Vec3::X }.to_array(),
                color: [1.0; 3],
                surface: m.surface(),
                material: 0,
                uv: [
                    uv[0] * 64.0 / size as f32,
                    (uv[1] + if export { card as f32 } else { 0.0 }) * 64.0 / size as f32,
                ],
                tangent: if card == 0 {
                    [1.0, 0.0, 0.0, -1.0]
                } else {
                    [0.0, 0.0, 1.0, -1.0]
                },
                light: m.light(),
                base: [1.0, 1.0, 1.0, -1.0],
                wear: [0.0; 3],
                painted: [0; 4],
            });
        }
        mesh.indices
            .extend([base, base + 1, base + 2, base, base + 2, base + 3]);
        mesh.edges.extend([
            base,
            base + 1,
            base + 1,
            base + 2,
            base + 2,
            base + 3,
            base + 3,
            base,
        ]);
    }
    mesh
}

#[cfg(test)]
mod tests {
    use super::*;
    fn small() -> Settings {
        Settings {
            resolution: 12,
            frames: 4,
            warmup: 1.5,
            ..Settings::default()
        }
    }
    #[test]
    fn deterministic_bounded_buoyant_fire() {
        let a = simulate(small(), &Progress::default()).unwrap();
        let b = simulate(small(), &Progress::default()).unwrap();
        assert_eq!(a, b);
        assert!(a.valid());
        let n = a.settings.resolution;
        let f = a.cache.last().unwrap();
        let mut mass = 0.0;
        let mut height = 0.0;
        for z in 0..n {
            for y in 0..2 * n {
                for x in 0..n {
                    let c = f.cells[index(n, x, y, z)];
                    assert!(c[1] < 1.2 && c[2] < 5.0);
                    mass += c[2];
                    height += c[2] * y as f32;
                }
            }
        }
        assert!(
            mass > 0.0 && height / mass > 2.0,
            "smoke must rise: {}",
            height / mass
        );
        assert_ne!(a.cache[0], a.cache[3]);
    }
    #[test]
    fn blackbody_and_fire_light_energy() {
        let cold = crate::envkit::light::blackbody(1400.0);
        let hot = crate::envkit::light::blackbody(3500.0);
        assert!(hot.y > cold.y && hot.z > cold.z);
        let f = simulate(small(), &Progress::default()).unwrap();
        let v = f.volume(Mat4::IDENTITY, None).unwrap();
        assert!(!v.lights().is_empty());
        let (e, tr) = march(&[v], Vec3::new(0.0, 0.1, -1.0), Vec3::Z, 2.0, |_, _| {
            Vec3::ZERO
        });
        assert!(e.max_element() > 0.0 && (0.0..=1.0).contains(&tr));
    }
    #[test]
    fn flame_has_bright_root_and_cools_before_upper_smoke() {
        let f = simulate(small(), &Progress::default()).unwrap();
        let v = f.volume(Mat4::IDENTITY, None).unwrap();
        let r = f.settings.radius;
        let mut root = 0.0f32;
        let mut upper = 0.0f32;
        for y in 0..v.dims[1] {
            for z in 0..v.dims[2] {
                for x in 0..v.dims[0] {
                    let p = v.lo
                        + (Vec3::new(x as f32, y as f32, z as f32) + Vec3::splat(0.5))
                            * v.step
                            * 2.0;
                    let c = v.at(p);
                    if p.y < r {
                        root += c[0];
                    }
                    if p.y > r * 5.0 {
                        upper += c[0];
                    }
                }
            }
        }
        assert!(root > 100.0, "root must remain luminous: {root}");
        assert!(
            upper < root * 0.01,
            "upper plume must be dark: {upper}/{root}"
        );
    }

    #[test]
    fn hero_budget_and_legacy_domain() {
        for (resolution, frames) in [(64, 32), (96, 9), (128, 4)] {
            let s = Settings {
                resolution,
                frames,
                ..Default::default()
            };
            assert!(s.validate().is_ok());
            assert!(
                Settings {
                    frames: frames + 1,
                    ..s.clone()
                }
                .validate()
                .is_err()
            );
        }
        let mut legacy = serde_json::to_value(Settings::default()).unwrap();
        legacy.as_object_mut().unwrap().remove("tight_domain");
        let s: Settings = serde_json::from_value(legacy).unwrap();
        assert_eq!(s.bounds().1.y, s.radius * 15.5);
        assert_eq!(
            Settings::default().bounds().1.y,
            Settings::default().radius * 9.5
        );
    }
    #[test]
    fn parallel_slabs_match_serial_stencil() {
        let n = 64;
        let mut a = vec![0usize; n * n * n * 2];
        grid(&mut a, n, |i, x, y, z, v| *v = i + x + 2 * y + 3 * z);
        for z in 0..n {
            for y in 0..2 * n {
                for x in 0..n {
                    let i = index(n, x, y, z);
                    let expected =
                        if x > 0 && x < n - 1 && y > 0 && y < 2 * n - 1 && z > 0 && z < n - 1 {
                            i + x + 2 * y + 3 * z
                        } else {
                            0
                        };
                    assert_eq!(a[i], expected);
                }
            }
        }
    }
    #[test]
    fn cancellation_and_cache_limits() {
        let p = Progress::default();
        p.cancel();
        assert!(simulate(small(), &p).is_err());
        let s = Settings {
            resolution: 128,
            frames: 96,
            ..small()
        };
        assert!(s.validate().is_err());
    }
}
