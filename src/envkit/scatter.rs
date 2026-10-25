//! Scatter: many copies of other objects (rocks, bones, debris, candles)
//! over an area or dropped onto surfaces, with random size and turn, a
//! density, a minimum spacing and areas to keep clear.
//!
//! A scatter object stores only instances: which source and a transform.
//! The sources' meshes are shared, so a thousand bones cost one bone mesh
//! (the viewport draws them instanced; glTF exports reference one mesh
//! from many nodes). A source's light (a candle's flame) comes with every
//! copy.

use glam::{Mat4, Quat, Vec2, Vec3};
use serde::{Deserialize, Serialize};

use super::Rand;

/// One copy: source index, position (scatter space), turn and size.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Debug)]
pub struct Instance {
    #[serde(rename = "s")]
    pub source: u32,
    #[serde(rename = "p")]
    pub position: Vec3,
    #[serde(rename = "r")]
    pub rotation: Quat,
    #[serde(rename = "k")]
    pub scale: f32,
}

impl Instance {
    pub fn matrix(&self) -> Mat4 {
        Mat4::from_scale_rotation_translation(Vec3::splat(self.scale), self.rotation, self.position)
    }
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug, Default)]
pub struct Scatter {
    /// Object ids of the sources (they may be hidden; copies still show).
    pub sources: Vec<u64>,
    pub instances: Vec<Instance>,
    /// The settings the instances were placed with.
    #[serde(default)]
    pub settings: Settings,
    /// What placed the instances when not `settings`: "array" for copies
    /// made from an array blob by `instance_array` (scatter tools do not
    /// place them again).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub placed_by: Option<String>,
}

/// What `instance_array` made: the hidden one-copy clay, the scatter of
/// its copies, and how many there are.
pub struct Instanced {
    pub unit: u64,
    pub scatter: u64,
    pub copies: usize,
}

/// Turns an array blob of a clay object into instances: a hidden clay
/// holding one copy (under the object) and a scatter of its copies (under
/// the object too, so they move with it), and removes the blob. The copy
/// is meshed once and every copy draws, renders and exports as an instance
/// of it (see `Clay::instancing` for when this is refused). `leaf` copies
/// the innermost shape of nested arrays.
pub fn instance_array(scene: &mut crate::scene::Scene, target: u64, blob: usize, leaf: bool, force: bool) -> Result<Instanced, String> {
    use crate::scene::Shape;
    let o = scene.get(target).ok_or("object vanished")?;
    let clay = o.clay.as_ref().filter(|_| o.shape == Shape::Clay).ok_or_else(|| format!("{} is not a clay object", o.name))?;
    if o.rig.as_ref().is_some_and(|r| !r.bones.is_empty()) {
        return Err(format!("{} is rigged; instanced copies would not follow its bones", o.name));
    }
    if o.link.is_some() {
        return Err(format!("{} is a linked mirror copy (its clay follows its source); instance the source, or unlink it first", o.name));
    }
    let inst = clay.instancing(blob, leaf, force)?;
    let (name, color, material) = (o.name.clone(), o.color, o.material.clone());
    let blob_name = clay.blobs[blob].name.clone();
    let unit = scene.add(Shape::Clay);
    let u = scene.get_mut(unit).expect("just added");
    u.name = format!("{name}_{}", if blob_name.is_empty() { format!("blob{blob}") } else { blob_name.clone() });
    (u.clay, u.color, u.material, u.visible, u.parent) = (Some(inst.unit), color, material, false, Some(target));
    let sc = scene.add(Shape::Scatter);
    let instances: Vec<Instance> = inst.copies.iter().map(|&(position, rotation)| Instance { source: 0, position, rotation, scale: 1.0 }).collect();
    let copies = instances.len();
    let s = scene.get_mut(sc).expect("just added");
    s.name = format!("{name}_{}_copies", if blob_name.is_empty() { format!("blob{blob}") } else { blob_name });
    s.parent = Some(target);
    s.scatter = Some(Scatter { sources: vec![unit], instances, settings: Settings::default(), placed_by: Some("array".into()) });
    scene.get_mut(target).and_then(|o| o.clay.as_mut()).expect("checked").remove_keeping_cell(blob);
    Ok(Instanced { unit, scatter: sc, copies })
}

/// Where and how to place copies.
#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    /// Area center (world X, Z) and size: `size` [w, d] for a rectangle
    /// or `radius` for a disc.
    #[serde(default)]
    pub center: [f32; 2],
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<[f32; 2]>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub radius: Option<f32>,
    /// Height of the area when there is no surface below (world Y).
    #[serde(default)]
    pub y: f32,
    /// Copies per square meter.
    #[serde(default = "default_density")]
    pub density: f32,
    /// At most this many (overrides density when smaller).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub count: Option<u32>,
    /// No two copies closer than this (meters, in XZ).
    #[serde(default = "default_min_distance")]
    pub min_distance: f32,
    /// Size range (multiplies the source's own scale).
    #[serde(default = "default_scale")]
    pub scale: [f32; 2],
    /// Random turn about Y (degrees, 360 = any), and random tilt (degrees).
    #[serde(default = "default_yaw")]
    pub yaw: f32,
    #[serde(default)]
    pub tilt: f32,
    /// Lean copies with the surface under them (0 = upright, 1 = fully).
    #[serde(default)]
    pub align: f32,
    /// Push copies into the surface by this many meters (rocks bedded in dirt).
    #[serde(default)]
    pub sink: f32,
    /// Circles (world [x, z, radius]) to keep clear.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub avoid: Vec<[f32; 3]>,
    /// Relative chance of each source (default even).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub weights: Vec<f32>,
    /// Steepest ground to place on (degrees from flat).
    #[serde(default = "default_max_slope")]
    pub max_slope: f32,
    #[serde(default)]
    pub seed: u64,
}

fn default_density() -> f32 { 1.0 }
fn default_min_distance() -> f32 { 0.25 }
fn default_scale() -> [f32; 2] { [0.8, 1.2] }
fn default_yaw() -> f32 { 360.0 }
fn default_max_slope() -> f32 { 50.0 }

impl Default for Settings {
    fn default() -> Self {
        serde_json::from_value(serde_json::json!({})).expect("defaults")
    }
}

/// Most copies in one scatter.
pub const MAX_INSTANCES: usize = 20_000;

impl Settings {
    pub fn sanitize(&mut self) -> Result<(), String> {
        let finite = |v: f32, name: &str| if v.is_finite() { Ok(v) } else { Err(format!("{name} must be finite")) };
        finite(self.center[0], "center")?;
        finite(self.center[1], "center")?;
        finite(self.y, "y")?;
        if let Some(s) = self.size
            && !(s[0].is_finite() && s[1].is_finite() && s[0] > 0.0 && s[1] > 0.0)
        {
            return Err("size must be two positive numbers [w, d]".into());
        }
        if let Some(r) = self.radius && !(r.is_finite() && r > 0.0) {
            return Err("radius must be positive".into());
        }
        if self.size.is_none() && self.radius.is_none() {
            self.size = Some([4.0, 4.0]);
        }
        self.density = finite(self.density, "density")?.clamp(0.0, 1000.0);
        self.min_distance = finite(self.min_distance, "min_distance")?.clamp(0.0, 100.0);
        self.scale = [finite(self.scale[0], "scale")?.clamp(0.01, 100.0), finite(self.scale[1], "scale")?.clamp(0.01, 100.0)];
        if self.scale[0] > self.scale[1] {
            self.scale.swap(0, 1);
        }
        self.yaw = finite(self.yaw, "yaw")?.clamp(0.0, 360.0);
        self.tilt = finite(self.tilt, "tilt")?.clamp(0.0, 90.0);
        self.align = finite(self.align, "align")?.clamp(0.0, 1.0);
        self.sink = finite(self.sink, "sink")?.clamp(-10.0, 10.0);
        self.max_slope = finite(self.max_slope, "max_slope")?.clamp(0.0, 90.0);
        if self.avoid.iter().flatten().any(|v| !v.is_finite()) {
            return Err("avoid circles must be finite [x, z, radius]".into());
        }
        if self.weights.iter().any(|w| !w.is_finite() || *w < 0.0) {
            return Err("weights must be zero or more".into());
        }
        Ok(())
    }

    pub fn area(&self) -> f32 {
        match (self.size, self.radius) {
            (Some(s), _) => s[0] * s[1],
            (None, Some(r)) => std::f32::consts::PI * r * r,
            _ => 0.0,
        }
    }

    fn inside(&self, p: Vec2) -> bool {
        let d = p - Vec2::from(self.center);
        match (self.size, self.radius) {
            (Some(s), _) => d.x.abs() <= s[0] * 0.5 && d.y.abs() <= s[1] * 0.5,
            (None, Some(r)) => d.length() <= r,
            _ => false,
        }
    }

    fn sample(&self, rng: &mut Rand) -> Vec2 {
        let c = Vec2::from(self.center);
        match (self.size, self.radius) {
            (Some(s), _) => c + Vec2::new(rng.signed() * s[0] * 0.5, rng.signed() * s[1] * 0.5),
            (None, Some(r)) => {
                let a = rng.f() * std::f32::consts::TAU;
                c + Vec2::new(a.cos(), a.sin()) * r * rng.f().sqrt()
            }
            _ => c,
        }
    }
}

/// What placing found: the copies (world space) and why fewer were made.
pub struct Placed {
    pub instances: Vec<Instance>,
    pub wanted: usize,
    /// Darts that fell too near another copy, in an avoided circle, off
    /// every surface, or on too steep a slope.
    pub crowded: usize,
    pub avoided: usize,
    pub missed: usize,
    pub steep: usize,
}

/// Places copies with dart throwing (a Poisson disk): random points,
/// each kept only if no earlier copy is within `min_distance`. `ground`
/// gives the surface (height, normal) under a world XZ point, if any.
/// `sources` is how many sources there are.
pub fn place(s: &Settings, sources: usize, ground: &dyn Fn(Vec2) -> Option<(f32, Vec3)>) -> Placed {
    let by_density = (s.area() * s.density).round() as usize;
    let wanted = s.count.map_or(by_density, |c| c as usize).min(MAX_INSTANCES);
    let mut rng = Rand::new(s.seed ^ 0x5CA7);
    let mut out = Placed { instances: Vec::new(), wanted, crowded: 0, avoided: 0, missed: 0, steep: 0 };
    if wanted == 0 || sources == 0 {
        return out;
    }
    // A grid of cells min_distance / sqrt(2) wide holds at most one copy.
    let cell = (s.min_distance / std::f32::consts::SQRT_2).max(1e-3);
    let mut grid: std::collections::HashMap<(i32, i32), Vec2> = std::collections::HashMap::new();
    let key = |p: Vec2| ((p.x / cell).floor() as i32, (p.y / cell).floor() as i32);
    let reach = (s.min_distance / cell).ceil() as i32;
    let weights: Vec<f32> = (0..sources).map(|i| s.weights.get(i).copied().unwrap_or(1.0)).collect();
    let total: f32 = weights.iter().sum::<f32>().max(1e-6);
    let max_cos = s.max_slope.to_radians().cos();
    let attempts = wanted * 30 + 100;
    for _ in 0..attempts {
        if out.instances.len() >= wanted {
            break;
        }
        let p = s.sample(&mut rng);
        if !s.inside(p) {
            continue;
        }
        if s.avoid.iter().any(|a| p.distance(Vec2::new(a[0], a[1])) < a[2]) {
            out.avoided += 1;
            continue;
        }
        let (kx, kz) = key(p);
        let near = s.min_distance > 0.0 && (-reach..=reach).any(|dz| (-reach..=reach).any(|dx| grid.get(&(kx + dx, kz + dz)).is_some_and(|q| q.distance(p) < s.min_distance)));
        if near {
            out.crowded += 1;
            continue;
        }
        let Some((y, n)) = ground(p) else {
            out.missed += 1;
            continue;
        };
        if n.y < max_cos {
            out.steep += 1;
            continue;
        }
        if s.min_distance > 0.0 {
            grid.insert((kx, kz), p);
        }
        // Which source, how big, which way.
        let mut pick = rng.f() * total;
        let mut source = 0;
        for (i, w) in weights.iter().enumerate() {
            if pick < *w {
                source = i;
                break;
            }
            pick -= w;
            source = i;
        }
        let scale = rng.range(s.scale[0], s.scale[1]);
        let yaw = Quat::from_rotation_y((rng.f() - 0.5) * s.yaw.to_radians());
        let tilt_axis = Vec3::new(rng.signed(), 0.0, rng.signed()).normalize_or(Vec3::X);
        let tilt = Quat::from_axis_angle(tilt_axis, rng.f() * s.tilt.to_radians());
        let lean = Quat::from_rotation_arc(Vec3::Y, Vec3::Y.lerp(n, s.align).normalize_or(Vec3::Y));
        out.instances.push(Instance { source: source as u32, position: Vec3::new(p.x, y - s.sink * scale, p.y), rotation: (lean * tilt * yaw).normalize(), scale });
    }
    out
}

/// Surfaces to drop copies onto: world triangles binned in XZ, so a
/// straight-down ray only tests the triangles under it.
pub struct Ground {
    tris: Vec<[Vec3; 3]>,
    cells: std::collections::HashMap<(i32, i32), Vec<u32>>,
    cell: f32,
}

impl Ground {
    pub fn new(tris: Vec<[Vec3; 3]>) -> Ground {
        let mut lo = Vec2::splat(f32::MAX);
        let mut hi = Vec2::splat(f32::MIN);
        for t in &tris {
            for p in t {
                lo = lo.min(Vec2::new(p.x, p.z));
                hi = hi.max(Vec2::new(p.x, p.z));
            }
        }
        let area = ((hi - lo).max(Vec2::splat(1e-3))).element_product();
        let cell = (area / (tris.len().max(1) as f32) * 4.0).sqrt().clamp(0.02, 10.0);
        let mut cells: std::collections::HashMap<(i32, i32), Vec<u32>> = std::collections::HashMap::new();
        for (i, t) in tris.iter().enumerate() {
            let tlo = t.iter().fold(Vec2::splat(f32::MAX), |a, p| a.min(Vec2::new(p.x, p.z)));
            let thi = t.iter().fold(Vec2::splat(f32::MIN), |a, p| a.max(Vec2::new(p.x, p.z)));
            let (x0, z0) = ((tlo.x / cell).floor() as i32, (tlo.y / cell).floor() as i32);
            let (x1, z1) = ((thi.x / cell).floor() as i32, (thi.y / cell).floor() as i32);
            if (x1 - x0 + 1) as i64 * (z1 - z0 + 1) as i64 > 10_000 {
                continue;
            }
            for z in z0..=z1 {
                for x in x0..=x1 {
                    cells.entry((x, z)).or_default().push(i as u32);
                }
            }
        }
        Ground { tris, cells, cell }
    }

    /// The highest surface under a world XZ point: (height, upward normal).
    pub fn top(&self, p: Vec2) -> Option<(f32, Vec3)> {
        let k = ((p.x / self.cell).floor() as i32, (p.y / self.cell).floor() as i32);
        let mut best: Option<(f32, Vec3)> = None;
        for &i in self.cells.get(&k)? {
            let [a, b, c] = self.tris[i as usize];
            // Barycentric in XZ.
            let (v0, v1, v2) = (Vec2::new(b.x - a.x, b.z - a.z), Vec2::new(c.x - a.x, c.z - a.z), Vec2::new(p.x - a.x, p.y - a.z));
            let den = v0.x * v1.y - v1.x * v0.y;
            if den.abs() < 1e-12 {
                continue;
            }
            let u = (v2.x * v1.y - v1.x * v2.y) / den;
            let v = (v0.x * v2.y - v2.x * v0.y) / den;
            if u < -1e-5 || v < -1e-5 || u + v > 1.0 + 1e-5 {
                continue;
            }
            let y = a.y + u * (b.y - a.y) + v * (c.y - a.y);
            if best.is_none_or(|(by, _)| y > by) {
                let mut n = (b - a).cross(c - a).normalize_or(Vec3::Y);
                if n.y < 0.0 {
                    n = -n;
                }
                best = Some((y, n));
            }
        }
        best
    }
}

/// A source's own turn and size (its place does not matter: copies go
/// where their instances say).
pub fn source_shape(src: &crate::scene::Object) -> Mat4 {
    Mat4::from_scale_rotation_translation(src.scale, src.rotation, Vec3::ZERO)
}

/// One mesh to draw: the object whose mesh and material it uses, its world
/// matrix, and a number telling copies apart (for flicker and variation).
pub struct Draw<'a> {
    pub object: &'a crate::scene::Object,
    pub world: Mat4,
    pub copy: u64,
    /// A scattered copy (not the object itself).
    pub scattered: bool,
}

/// Every mesh to draw: each visible mesh object, and each copy of every
/// visible scatter (as its source, which may itself be hidden).
pub fn draws<'a>(scene: &'a crate::scene::Scene, eval: &std::collections::HashMap<u64, (Mat4, bool)>) -> Vec<Draw<'a>> {
    let mut out = Vec::new();
    for o in &scene.objects {
        let Some(&(world, true)) = eval.get(&o.id) else { continue };
        if o.shape.is_mesh() {
            out.push(Draw { object: o, world, copy: o.id, scattered: false });
            continue;
        }
        let Some(sc) = o.scatter.as_ref().filter(|_| o.shape == crate::scene::Shape::Scatter) else { continue };
        let sources: Vec<Option<(&crate::scene::Object, Mat4)>> = sc.sources.iter().map(|&id| scene.get(id).filter(|s| s.shape.is_mesh() || s.light.is_some()).map(|s| (s, source_shape(s)))).collect();
        for (i, inst) in sc.instances.iter().enumerate() {
            let Some(Some((src, shape))) = sources.get(inst.source as usize) else { continue };
            out.push(Draw { object: src, world: world * inst.matrix() * *shape, copy: o.id.wrapping_mul(0x9E37_79B9).wrapping_add(i as u64 + 1), scattered: true });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flat(_: Vec2) -> Option<(f32, Vec3)> {
        Some((0.0, Vec3::Y))
    }

    #[test]
    fn scatter_respects_density_spacing_and_avoid() {
        let s = Settings { size: Some([10.0, 10.0]), density: 2.0, min_distance: 0.3, avoid: vec![[2.0, 2.0, 1.5]], seed: 4, ..Settings::default() };
        let placed = place(&s, 2, &flat);
        // 200 asked; the 7 m2 avoided disc takes about 14 of them.
        let n = placed.instances.len();
        assert!(n as f32 > 200.0 * 0.85 && n <= 200, "placed {n}");
        for (i, a) in placed.instances.iter().enumerate() {
            let pa = Vec2::new(a.position.x, a.position.z);
            assert!(pa.distance(Vec2::new(2.0, 2.0)) >= 1.5, "inside the avoided circle");
            assert!(pa.x.abs() <= 5.0 && pa.y.abs() <= 5.0);
            for b in &placed.instances[i + 1..] {
                assert!(pa.distance(Vec2::new(b.position.x, b.position.z)) >= 0.3 - 1e-5, "too close");
            }
        }
        assert!(placed.instances.iter().any(|i| i.source == 1) && placed.instances.iter().any(|i| i.source == 0));
        // A spacing too wide for the density places what fits and says so.
        let tight = Settings { size: Some([2.0, 2.0]), density: 50.0, min_distance: 0.5, ..Settings::default() };
        let placed = place(&tight, 1, &flat);
        assert!(placed.instances.len() < 30 && placed.crowded > 0);
        // A count caps it; the same seed gives the same copies.
        let few = Settings { count: Some(5), ..s.clone() };
        let a = place(&few, 1, &flat);
        let b = place(&few, 1, &flat);
        assert_eq!(a.instances.len(), 5);
        assert_eq!(a.instances, b.instances);
    }

    #[test]
    fn copies_drop_onto_the_highest_surface() {
        let quad = |y: f32| vec![[Vec3::new(-1.0, y, -1.0), Vec3::new(1.0, y, -1.0), Vec3::new(1.0, y, 1.0)], [Vec3::new(-1.0, y, -1.0), Vec3::new(1.0, y, 1.0), Vec3::new(-1.0, y, 1.0)]];
        let mut tris = quad(0.0);
        tris.extend(quad(0.5));
        let g = Ground::new(tris);
        let (y, n) = g.top(Vec2::new(0.3, -0.2)).unwrap();
        assert!((y - 0.5).abs() < 1e-5 && n.y > 0.99);
        assert!(g.top(Vec2::new(3.0, 0.0)).is_none());
        let s = Settings { size: Some([4.0, 4.0]), density: 5.0, min_distance: 0.1, ..Settings::default() };
        let placed = place(&s, 1, &|p| g.top(p));
        assert!(placed.missed > 0 && placed.instances.iter().all(|i| (i.position.y - 0.5).abs() < 1e-4));
    }
}
