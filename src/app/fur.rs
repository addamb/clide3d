//! `hair.grow {fur: true}` (character follow-ups, feature 3): guides spread
//! evenly over a clay's body by area (blue noise: area-weighted
//! candidates thinned by weighted sample elimination, Yuksel 2015), short
//! and combed along a flow field (down the body, back over the head from
//! the nose, out along the limbs, smoothed between neighbours; `direction`
//! overrides), each as long as its region asks, kept out of the clay; and
//! the `Fur` settings their strands grow by (`hair/fur.rs`). Every size is
//! bounded from the arguments (and the clay's area) before anything is
//! allocated, reusing the hair limits (`hair::MAX_GUIDES`,
//! `hair::MAX_STRANDS`).

use std::collections::BinaryHeap;

use glam::{Mat4, Vec3};
use serde_json::{Value, json};

use super::ClideApp;
use crate::hair::{Fur, Guide, MAX_GUIDES, MAX_STRANDS};
use crate::hair::fur::{FurLid, FurRegion, MAX_LID_COPIES, MAX_LIDS, MAX_LIST};
use crate::scene::Shape;

/// Grow keys fur takes.
const KEYS: [&str; 29] = ["fur", "lid_length", "on", "count", "strands", "density", "length", "lengths", "exclude", "include", "direction", "lie", "tint", "tip_color", "tip_mix", "lift", "seed", "strand_width", "jitter", "eyes", "segments", "drape", "floor", "clump", "clump_size", "part", "wave", "waves", "ragged"];
/// Most segments a fur guide takes (long, draped fur wants more).
const MAX_SEGMENTS: usize = 32;
/// Default guides and film strands.
const COUNT: usize = 2000;
const STRANDS: u32 = 160_000;
/// Candidates per guide before thinning.
const OVERSAMPLE: usize = 6;

/// What `grow_fur` made.
pub(super) struct FurGrown {
    /// In the hair's space.
    pub guides: Vec<Guide>,
    pub fur: Fur,
    pub strands: u32,
    pub strand_width: f32,
    /// The hair's lift, in its units.
    pub lift: f32,
    /// A gentle wave along each strand (the hair's units).
    pub frizz: f32,
    pub report: Value,
}

/// A region of the body: blobs by name (the blob making the surface
/// there) or a sphere (world).
enum Region {
    Blobs(Vec<String>),
    Sphere(Vec3, f32),
}

impl Region {
    fn parse(v: &Value, what: &str) -> Result<Region, String> {
        if let Some(names) = v.get("blobs") {
            let names: Vec<String> = serde_json::from_value(names.clone()).map_err(|_| format!("{what}.blobs must be a list of blob names"))?;
            if names.is_empty() || names.len() > MAX_LIST {
                return Err(format!("{what}.blobs takes 1 to {MAX_LIST} names"));
            }
            return Ok(Region::Blobs(with_sides(names)));
        }
        let c: [f32; 3] = serde_json::from_value(v.get("center").cloned().unwrap_or(Value::Null)).map_err(|_| format!("{what} needs blobs: [names] or center [x, y, z] and radius (world)"))?;
        let r = v.get("radius").and_then(Value::as_f64).ok_or(format!("{what} needs a radius with its center"))? as f32;
        if !(r.is_finite() && r > 0.0) || !Vec3::from(c).is_finite() {
            return Err(format!("{what}: center must be finite and radius above 0"));
        }
        Ok(Region::Sphere(Vec3::from(c), r))
    }

    fn holds(&self, p: Vec3, owner: Option<&str>) -> bool {
        match self {
            Region::Blobs(names) => owner.is_some_and(|o| names.iter().any(|n| n == o)),
            Region::Sphere(c, r) => p.distance(*c) < *r,
        }
    }
}

/// Names with their other side added (`brow.L` also names `brow.R`): a
/// mirrored blob's copy carries the stored side's name, a split one its
/// own.
fn with_sides(names: Vec<String>) -> Vec<String> {
    let mut out = Vec::new();
    for n in names {
        if let Some(o) = crate::pose::swap_side_token(&n)
            && !out.contains(&o)
        {
            out.push(o);
        }
        if !out.contains(&n) {
            out.push(n);
        }
    }
    out
}

/// A list of names or spheres: names go to `names`, spheres to `spheres`.
fn names_and_spheres(v: Option<&Value>, what: &str) -> Result<(Vec<String>, Vec<(Vec3, f32)>), String> {
    let Some(v) = v.filter(|v| !v.is_null()) else { return Ok((Vec::new(), Vec::new())) };
    let list = v.as_array().ok_or(format!("{what} must be a list of blob names or {{center, radius}} spheres"))?;
    if list.len() > MAX_LIST {
        return Err(format!("{what} takes at most {MAX_LIST} entries"));
    }
    let (mut names, mut spheres) = (Vec::new(), Vec::new());
    for e in list {
        match e {
            Value::String(n) => names.push(n.clone()),
            v => match Region::parse(v, what)? {
                Region::Blobs(n) => names.extend(n),
                Region::Sphere(c, r) => spheres.push((c, r)),
            },
        }
    }
    Ok((with_sides(names), spheres))
}

/// Weighted sample elimination (Yuksel 2015): of `points` (on a surface
/// of area `area`), the `keep` most evenly spread, in their order. Each
/// point is weighed by its neighbours within twice the spread's radius;
/// the heaviest goes first (ties: the later index), its neighbours
/// lighten, until `keep` remain. Deterministic.
fn eliminate(points: &[Vec3], keep: usize, area: f32) -> Vec<usize> {
    let n = points.len();
    if keep >= n {
        return (0..n).collect();
    }
    let r_max = (area / (2.0 * 3f32.sqrt() * keep as f32)).sqrt();
    let reach = 2.0 * r_max;
    let mut grid: std::collections::HashMap<(i32, i32, i32), Vec<u32>> = std::collections::HashMap::new();
    let cell = |p: Vec3| {
        let c = (p / reach).floor().as_ivec3();
        (c.x, c.y, c.z)
    };
    for (i, p) in points.iter().enumerate() {
        grid.entry(cell(*p)).or_default().push(i as u32);
    }
    let weight = |d: f32| (1.0 - d / reach).max(0.0).powi(8);
    let mut nbrs: Vec<Vec<(u32, f32)>> = vec![Vec::new(); n];
    let mut w = vec![0.0f64; n];
    for (i, p) in points.iter().enumerate() {
        let (x, y, z) = cell(*p);
        for dx in -1..=1 {
            for dy in -1..=1 {
                for dz in -1..=1 {
                    for &j in grid.get(&(x + dx, y + dy, z + dz)).into_iter().flatten() {
                        if j as usize == i {
                            continue;
                        }
                        let d = p.distance(points[j as usize]);
                        if d < reach {
                            let wt = weight(d);
                            nbrs[i].push((j, wt));
                            w[i] += wt as f64;
                        }
                    }
                }
            }
        }
    }
    // A max-heap of (weight bits, index), lazily refreshed: weights are
    // nonnegative, so their bits order as the values do.
    let mut heap: BinaryHeap<(u64, usize)> = (0..n).map(|i| (w[i].to_bits(), i)).collect();
    let mut alive = vec![true; n];
    let mut left = n;
    while left > keep {
        let Some((bits, i)) = heap.pop() else { break };
        if !alive[i] || bits != w[i].to_bits() {
            continue;
        }
        alive[i] = false;
        left -= 1;
        for &(j, wt) in &nbrs[i] {
            let j = j as usize;
            if alive[j] {
                w[j] = (w[j] - wt as f64).max(0.0);
                heap.push((w[j].to_bits(), j));
            }
        }
    }
    (0..n).filter(|&i| alive[i]).collect()
}

/// The radius round an eye (radius `r`) with no fur roots: past its
/// rim, plus the fur's length so short fur by the eye cannot reach it,
/// but at most half the eye's radius more: long fur would otherwise
/// leave the whole face bare (a 0.22 coat round a 0.04 eye cleared a
/// 0.27 sphere), and strands that would enter the eye are dealt with
/// on their own (`Fur::avoid`).
pub(super) fn eye_bare_radius(r: f32, length: f32) -> f32 {
    r * 1.15 + length.min(0.5 * r)
}

/// The plane through `at` with normal `n` (world) in the space of an
/// object placed at `m`: [nx, ny, nz, offset], `n·p >= offset` on the
/// normal's side.
fn plane_in(m: Mat4, n: Vec3, at: Vec3) -> [f32; 4] {
    let nh = m.transpose().transform_vector3(n);
    let off = n.dot(at) - n.dot(m.transform_point3(Vec3::ZERO));
    let l = nh.length().max(1e-12);
    [nh.x / l, nh.y / l, nh.z / l, off / l]
}

/// A small deterministic random stream (xorshift).
struct Rng(u32);
impl Rng {
    fn next(&mut self) -> f32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 17;
        self.0 ^= self.0 << 5;
        (self.0 >> 8) as f32 / (1u32 << 24) as f32
    }
}

impl ClideApp {
    /// Create > Fur on Selected Clay's request: a Hair under the selected
    /// clay (the first selected object), fur grown over it with the
    /// defaults, under a free name; None without a selected clay.
    pub(super) fn fur_request(&self) -> Option<Value> {
        let clay = self.selection.first().and_then(|id| self.scene.get(*id)).filter(|o| o.shape == Shape::Clay)?;
        let taken = |n: &str| self.scene.objects.iter().any(|o| o.name == n);
        let stem = format!("{}_fur", clay.name);
        let name = if taken(&stem) { (2..).map(|k| format!("{stem}{k}")).find(|n| !taken(n)).expect("a free name") } else { stem };
        Some(json!({ "shape": "Hair", "name": name, "parent": clay.name, "hair": { "grow": { "fur": true, "on": clay.name } } }))
    }

    /// `grow: {fur: true, on, ...}` for hair `hair_id` (see the module).
    pub(super) fn grow_fur(&mut self, hair_id: u64, g: &Value) -> Result<FurGrown, String> {
        let o = g.as_object().ok_or("grow must be an object")?;
        if let Some(k) = o.keys().find(|k| !KEYS.contains(&k.as_str())) {
            return Err(format!("unknown fur grow key {k}; use {}", KEYS.join(", ")));
        }
        let num = |k: &str| -> Result<Option<f64>, String> {
            match g.get(k).filter(|v| !v.is_null()) {
                None => Ok(None),
                Some(v) => v.as_f64().filter(|x| x.is_finite()).map(Some).ok_or(format!("grow.{k} must be a number")),
            }
        };
        // Bounds from the arguments alone.
        let count = match num("count")? {
            None => COUNT,
            Some(c) if c.fract() == 0.0 && (1.0..=MAX_GUIDES as f64).contains(&c) => c as usize,
            Some(c) => return Err(format!("grow.count (fur guides) must be a whole number 1 to {MAX_GUIDES} (got {c})")),
        };
        let strands_arg = match num("strands")? {
            None => None,
            Some(s) if s.fract() == 0.0 && (0.0..=MAX_STRANDS as f64).contains(&s) => Some(s as u32),
            Some(s) => return Err(format!("grow.strands must be a whole number 0 to {MAX_STRANDS} (the film strand budget; got {s})")),
        };
        let density = num("density")?;
        if strands_arg.is_some() && density.is_some() {
            return Err("grow takes strands or density, not both".into());
        }
        if density.is_some_and(|d| d <= 0.0) {
            return Err("grow.density (film strands per square unit of surface) must be above 0".into());
        }
        let drape = num("drape")?.unwrap_or(0.0) as f32;
        if !(0.0..=1.0).contains(&drape) {
            return Err("grow.drape must be 0 (short fur, no weight) to 1 (long fur hangs under gravity)".into());
        }
        let clump = num("clump")?.map_or(if drape > 0.0 { 0.6 } else { 0.0 }, |v| v as f32);
        let clump_size = num("clump_size")?.map_or(crate::hair::fur::CLUMP_SIZE, |v| v as f32);
        if !(0.0..=1.0).contains(&clump) || !(1.0..=8.0).contains(&clump_size) {
            return Err("grow.clump must be 0 to 1 and grow.clump_size 1 to 8 guide spacings".into());
        }
        if clump > 0.0 && drape == 0.0 {
            return Err("grow.clump gathers long fur into locks: it needs drape above 0".into());
        }
        let wave_arg = num("wave")?.map(|v| v as f32);
        let waves = num("waves")?.map_or(crate::hair::fur::WAVES, |v| v as f32);
        let ragged = num("ragged")?.map_or(if drape > 0.0 { 0.25 } else { 0.0 }, |v| v as f32);
        if wave_arg.is_some_and(|w| !(0.0..=1.0).contains(&w)) || !(0.0..=16.0).contains(&waves) || !(0.0..=0.9).contains(&ragged) {
            return Err("grow.wave must be 0 to 1 (a distance), grow.waves 0 to 16 and grow.ragged 0 to 0.9".into());
        }
        if drape == 0.0 && (wave_arg.is_some_and(|w| w > 0.0) || ragged > 0.0) {
            return Err("grow.wave and grow.ragged shape long fur's locks: they need drape above 0".into());
        }
        // A parting: the clay's centre plane (true) or the plane at its
        // own x (a number).
        let part_x: Option<f32> = match g.get("part").filter(|v| !v.is_null()) {
            None | Some(Value::Bool(false)) => None,
            Some(Value::Bool(true)) => Some(0.0),
            Some(v) => Some(v.as_f64().filter(|x| x.is_finite() && x.abs() <= 1000.0).ok_or("grow.part must be true (the clay's centre line), false, or the parting's x in the clay's space")? as f32),
        };
        let floor_on = match g.get("floor").filter(|v| !v.is_null()) {
            None => drape > 0.0,
            Some(v) => v.as_bool().ok_or("grow.floor must be true or false")?,
        };
        let segments = match num("segments")? {
            None if drape > 0.0 => 16,
            None => 6,
            Some(s) if s.fract() == 0.0 && (2.0..=MAX_SEGMENTS as f64).contains(&s) => s as usize,
            Some(_) => return Err(format!("grow.segments must be a whole number 2 to {MAX_SEGMENTS}")),
        };
        let lie = num("lie")?.unwrap_or(0.8) as f32;
        if !(0.0..=1.0).contains(&lie) {
            return Err("grow.lie must be 0 (fur stands up) to 1 (it lies flat)".into());
        }
        let color = |k: &str| -> Result<Option<[f32; 3]>, String> {
            match g.get(k).filter(|v| !v.is_null()) {
                None => Ok(None),
                Some(v) => serde_json::from_value::<[f32; 3]>(v.clone()).ok().filter(|c| c.iter().all(|x| x.is_finite() && (0.0..=4.0).contains(x))).map(Some).ok_or(format!("grow.{k} must be [r, g, b], each 0 to 4")),
            }
        };
        let tint = color("tint")?.unwrap_or([1.0; 3]);
        let tip_color = color("tip_color")?;
        if tip_color.is_some_and(|c| c.iter().any(|x| *x > 1.0)) {
            return Err("grow.tip_color must be sRGB, each 0 to 1".into());
        }
        let tip_mix = num("tip_mix")?.unwrap_or(0.5) as f32;
        let jitter = num("jitter")?.unwrap_or(12.0) as f32;
        if !(0.0..=1.0).contains(&tip_mix) || !(0.0..=90.0).contains(&jitter) {
            return Err("grow.tip_mix must be 0 to 1 and grow.jitter 0 to 90 degrees".into());
        }
        let (exclude, bare) = names_and_spheres(g.get("exclude"), "grow.exclude")?;
        let (include, include_spheres) = names_and_spheres(g.get("include"), "grow.include")?;
        let lengths: Vec<(Region, f32)> = match g.get("lengths").filter(|v| !v.is_null()) {
            None => Vec::new(),
            Some(v) => {
                let list = v.as_array().ok_or("grow.lengths must be a list of {blobs | center, radius, length}")?;
                if list.len() > MAX_LIST {
                    return Err(format!("grow.lengths takes at most {MAX_LIST} entries"));
                }
                list.iter().map(|e| {
                    let l = e.get("length").and_then(Value::as_f64).ok_or("each grow.lengths entry needs a length")? as f32;
                    if !(l.is_finite() && l > 0.0 && l <= 3.0) {
                        return Err("grow.lengths: length must be above 0 and at most 3".to_string());
                    }
                    Ok((Region::parse(e, "grow.lengths")?, l))
                }).collect::<Result<_, String>>()?
            }
        };
        let directions: Vec<(Region, Vec3)> = match g.get("direction").filter(|v| !v.is_null()) {
            None => Vec::new(),
            Some(v) => {
                let list = v.as_array().ok_or("grow.direction must be a list of {blobs | center, radius, direction}")?;
                if list.len() > MAX_LIST {
                    return Err(format!("grow.direction takes at most {MAX_LIST} entries"));
                }
                list.iter().map(|e| {
                    let d: [f32; 3] = serde_json::from_value(e.get("direction").cloned().unwrap_or(Value::Null)).map_err(|_| "each grow.direction entry needs direction [x, y, z] (world)".to_string())?;
                    let d = Vec3::from(d);
                    if !d.is_finite() || d.length() < 1e-6 {
                        return Err("grow.direction: direction must be a nonzero [x, y, z]".to_string());
                    }
                    Ok((Region::parse(e, "grow.direction")?, d.normalize()))
                }).collect::<Result<_, String>>()?
            }
        };
        let seed = num("seed")?.map_or(1, |s| s as u32);
        let on = self.resolve(g.get("on").ok_or("grow.on: the clay to grow fur over is required")?)?;
        let clay_o = self.scene.get(on).ok_or("object vanished")?;
        if clay_o.shape != Shape::Clay {
            return Err("grow.on must be a clay object".into());
        }
        let rig = clay_o.rig.clone();
        let nose_blob = crate::pose::named_point(clay_o, "nose");
        self.settle_clays([on])?;
        self.meshes.sync(&self.scene);
        let head = self.head_collider(on)?;
        let clay_world = self.scene.rest_world_matrix(on);
        let mesh = self.meshes.whole(on).cloned().ok_or("the clay has no mesh yet")?;
        let (lo, hi) = mesh.bounds();
        let size = (clay_world.transform_point3(hi) - clay_world.transform_point3(lo)).abs().max_element().max(1e-6);
        let length = num("length")?.map_or(0.012 * size, |v| v as f32);
        if !(length.is_finite() && length > 0.0 && length <= 3.0) {
            return Err("grow.length must be above 0 and at most 3".into());
        }
        let lid_arg = num("lid_length")?.map(|v| v as f32);
        if lid_arg.is_some_and(|l| !(l.is_finite() && (0.0..=3.0).contains(&l))) {
            return Err("grow.lid_length must be 0 (bare lids) to 3".into());
        }
        let lift = num("lift")?.map_or(crate::hair::GROW_LIFT, |v| v as f32);
        if !(0.0..=0.1).contains(&lift) {
            return Err("grow.lift must be 0 to 0.1".into());
        }
        // The surface: triangles in world space, by area.
        let normal_m = clay_world.inverse().transpose();
        let pos = |i: u32| clay_world.transform_point3(Vec3::from(mesh.vertices[i as usize].pos));
        let nrm = |i: u32| normal_m.transform_vector3(Vec3::from(mesh.vertices[i as usize].normal));
        let mut cumulative = Vec::with_capacity(mesh.indices.len() / 3);
        let mut area = 0.0f64;
        for t in mesh.indices.chunks_exact(3) {
            area += ((pos(t[1]) - pos(t[0])).cross(pos(t[2]) - pos(t[0])).length() * 0.5) as f64;
            cumulative.push(area);
        }
        let area = area as f32;
        if area <= 0.0 {
            return Err("the clay has no surface".into());
        }
        let strands = match density {
            Some(d) => {
                let want = (d * area as f64).ceil();
                if want > MAX_STRANDS as f64 {
                    return Err(format!("grow.density {d} over the clay's {area:.4} square units is {want} strands; the film budget is {MAX_STRANDS}: at most {:.1} per square unit", MAX_STRANDS as f64 / area as f64));
                }
                want as u32
            }
            None => strands_arg.unwrap_or(STRANDS),
        };
        // Eyes on this clay: no roots round them, no strand into them.
        let avoid_eyes = g.get("eyes").and_then(Value::as_bool).unwrap_or(true);
        let eyes: Vec<(Vec3, f32)> = if avoid_eyes {
            self.scene.objects.iter().filter(|e| e.shape == Shape::Eye && self.scene.ancestors(e.id).contains(&on)).map(|e| {
                let m = self.scene.rest_world_matrix(e.id);
                let r = 0.5 * m.to_scale_rotation_translation().0.abs().max_element();
                (m.transform_point3(Vec3::ZERO), r)
            }).collect()
        } else {
            Vec::new()
        };
        let eye_bones: Vec<String> = self.scene.objects.iter().filter(|e| e.shape == Shape::Eye && self.scene.ancestors(e.id).contains(&on)).filter_map(|e| e.bone.clone()).collect();
        let mut bare_all: Vec<(Vec3, f32)> = bare.clone();
        bare_all.extend(eyes.iter().map(|(c, r)| (*c, eye_bare_radius(*r, length))));
        // Lids (blobs of the `lid` form) grow short fur on their caps
        // unless excluded (world space here).
        let lids: Vec<FurLid> = if lid_arg != Some(0.0) {
            let clay = self.scene.get(on).and_then(|o| o.clay.as_ref()).ok_or("the clay vanished")?;
            let named = |n: &str| with_sides(vec![n.to_string()]);
            let world_scale = clay_world.to_scale_rotation_translation().0.abs().max_element();
            let mut out = Vec::new();
            for b in clay.blobs.iter().filter(|b| matches!(b.form, crate::clay::Form::Lid { .. }) && b.op == crate::clay::Op::Add) {
                if named(&b.name).iter().any(|n| exclude.contains(n)) || (!include.is_empty() && !named(&b.name).iter().any(|n| include.contains(n))) {
                    continue;
                }
                let (c, n, rm, h) = crate::clay::lid_geometry(&b.form);
                let flip = |v: Vec3| Vec3::new(-v.x, v.y, v.z);
                let sides: Vec<(Vec3, Vec3)> = match (b.mirror, b.flip) {
                    (false, false) => vec![(c, n)],
                    (false, true) => vec![(flip(c), flip(n))],
                    (true, _) => vec![(flip(c), flip(n)), (c, n)],
                };
                // Short against the body's fur, and against the lid (long
                // fur's lids keep a short coat).
                let lid_length = lid_arg.unwrap_or((crate::hair::fur::LID_LENGTH * length).min(crate::hair::fur::LID_SIZE * rm * world_scale));
                for (c, n) in sides {
                    out.push(FurLid {
                        center: clay_world.transform_point3(c).into(),
                        axis: clay_world.transform_vector3(n).normalize_or(Vec3::Y).into(),
                        radius: rm * world_scale,
                        half: h * world_scale,
                        // The cap and its roll into the face as far as its
                        // eye's bare sphere reaches (the rest of the blend
                        // grows the face's fur).
                        reach: {
                            let at = clay_world.transform_point3(c);
                            let bare_r = bare_all.iter().filter(|(e, _)| e.distance(at) < 0.5 * rm * world_scale).map(|(_, r)| *r).fold(0.0f32, f32::max);
                            ((rm + h + b.blend.max(0.0)) * world_scale).min(((rm + 2.0 * h) * world_scale).max(bare_r))
                        },
                        length: lid_length,
                        copies: (length / lid_length).round().clamp(1.0, MAX_LID_COPIES as f32) as u32,
                    });
                }
            }
            if out.len() > MAX_LIDS {
                return Err(format!("fur grows on at most {MAX_LIDS} lids; exclude some or set grow.lid_length 0"));
            }
            out
        } else {
            Vec::new()
        };
        let lid_at = |p: Vec3| lids.iter().find(|l| l.holds(p));
        // A root on a lid's cap is not bared by its own eye (`FurLid`).
        let bared = |p: Vec3| bare_all.iter().any(|(c, r)| p.distance(*c) < *r && !lid_at(p).is_some_and(|l| c.distance(Vec3::from(l.center)) < 0.5 * l.radius));
        // The ground: the clay's lowest point (the soles stand on it).
        let floor_y = floor_on.then(|| mesh.vertices.iter().map(|v| clay_world.transform_point3(Vec3::from(v.pos)).y).fold(f32::MAX, f32::min));
        // Candidates, area-weighted, then the excluded ones dropped.
        let tries = (count * OVERSAMPLE).min(MAX_GUIDES * OVERSAMPLE);
        let mut rng = Rng(seed.wrapping_mul(2654435761).max(1));
        let mut cands: Vec<(Vec3, Vec3)> = Vec::with_capacity(tries);
        let total = *cumulative.last().unwrap_or(&1.0);
        let mut excluded = 0usize;
        for _ in 0..tries {
            let a = rng.next() as f64 * total;
            let t = cumulative.partition_point(|c| *c < a).min(cumulative.len() - 1);
            let tri = &mesh.indices[3 * t..3 * t + 3];
            let (u, v) = (rng.next(), rng.next());
            let (u, v) = if u + v > 1.0 { (1.0 - u, 1.0 - v) } else { (u, v) };
            let p = pos(tri[0]) * (1.0 - u - v) + pos(tri[1]) * u + pos(tri[2]) * v;
            let n = (nrm(tri[0]) * (1.0 - u - v) + nrm(tri[1]) * u + nrm(tri[2]) * v).normalize_or(Vec3::Y);
            let owner = head.owner_name(p);
            // No hair on the soles: under-surfaces by the ground, which
            // the hair could only lie wedged under.
            let sole = floor_y.is_some_and(|y| n.y < -0.3 && p.y - y < 0.03 * size);
            // A lid root needs room for its fur before the margin.
            let no_room = lid_at(p).is_some_and(|l| l.strand(p, 0.0, 1.0, 2, 0.0).is_none());
            let out = sole || no_room || bared(p)
                || owner.as_ref().is_some_and(|o| exclude.contains(o))
                || ((!include.is_empty() || !include_spheres.is_empty()) && !(owner.as_ref().is_some_and(|o| include.contains(o)) || include_spheres.iter().any(|(c, r)| p.distance(*c) < *r)));
            if out {
                excluded += 1;
                continue;
            }
            cands.push((p, n));
        }
        if cands.is_empty() {
            return Err("no surface is left for fur: exclude and include leave nothing".into());
        }
        // The kept area: the share of candidates kept.
        let kept_area = area * cands.len() as f32 / tries as f32;
        let roots_i = eliminate(&cands.iter().map(|c| c.0).collect::<Vec<_>>(), count, kept_area);
        let roots: Vec<(Vec3, Vec3)> = roots_i.iter().map(|&i| cands[i]).collect();
        let (spacing, cv) = crate::hair::fur::spacing_stats(&roots.iter().map(|r| r.0).collect::<Vec<_>>());
        // The flow: per root, from the bone of the blob there.
        let bone_of = |name: &str| rig.as_ref().and_then(|r| r.bones.iter().find(|b| b.name == name)).map(|b| (clay_world.transform_point3(b.head), clay_world.transform_point3(b.tail)));
        let head_bone = eye_bones.first().cloned().or_else(|| rig.as_ref().and_then(|r| r.bones.iter().find(|b| b.name == "head")).map(|b| b.name.clone()));
        let eye_mid = (!eyes.is_empty()).then(|| eyes.iter().map(|e| e.0).sum::<Vec3>() / eyes.len() as f32);
        let nose = nose_blob.map(|p| clay_world.transform_point3(p)).or_else(|| eye_mid.map(|m| m + clay_world.transform_vector3(Vec3::Z).normalize_or(Vec3::Z) * eyes[0].1 * 1.5));
        let up = clay_world.transform_vector3(Vec3::Y).normalize_or(Vec3::Y);
        let back = clay_world.transform_vector3(Vec3::NEG_Z).normalize_or(Vec3::NEG_Z);
        let centre_x = clay_world.transform_point3(Vec3::ZERO);
        let side = clay_world.transform_vector3(Vec3::X).normalize_or(Vec3::X);
        let tangent = |raw: Vec3, n: Vec3| -> Vec3 {
            let f = raw - n * n.dot(raw);
            if f.length() > 0.25 * raw.length().max(1e-9) {
                return f.normalize();
            }
            let f = back - n * n.dot(back);
            if f.length() > 0.25 { f.normalize() } else { n.any_orthonormal_vector() }
        };
        let mut pinned = vec![false; roots.len()];
        // The parting (world): its plane's point; which side a root is on.
        let part_at = part_x.map(|x| clay_world.transform_point3(Vec3::X * x));
        let part_side = |p: Vec3| part_at.is_none_or(|a| (p - a).dot(side) >= 0.0);
        // Skin on top (nothing of the clay above it: the back and the
        // crown, not the top of a paw under the body).
        let on_top = |p: Vec3| (1..=24).all(|k| !head.inside(p + up * (size * k as f32 / 24.0)));
        let mut flow: Vec<Vec3> = roots.iter().enumerate().map(|(i, (p, n))| {
            // On a lid: down its cap toward the margin.
            if let Some(l) = lid_at(*p) {
                pinned[i] = true;
                let (c, a) = (Vec3::from(l.center), Vec3::from(l.axis));
                let u = (*p - c).normalize_or(*n);
                return tangent(-(a - u * u.dot(a)), *n);
            }
            if let Some((_, d)) = directions.iter().find(|(r, _)| r.holds(*p, head.owner_name(*p).as_deref())) {
                pinned[i] = true;
                return tangent(*d, *n);
            }
            // Parted: where the skin on top faces up (the back, the
            // crown) the hair leaves the parting sideways, to fall down
            // its side.
            if part_at.is_some() && n.dot(up) > 0.35 && on_top(*p) {
                return tangent(if part_side(*p) { side } else { -side }, *n);
            }
            let bone = head.owner_bone(*p);
            let raw = match (bone.as_deref(), &head_bone) {
                (Some(b), Some(h)) if b == h => nose.map_or(-up, |a| *p - a),
                (Some(b), _) => match bone_of(b) {
                    Some((a, t)) => {
                        let d = (t - a).normalize_or(-up);
                        let centred = (a - centre_x).dot(side).abs() < 0.02 * size && (t - centre_x).dot(side).abs() < 0.02 * size;
                        if centred && d.dot(up).abs() > 0.5 { -up } else { d }
                    }
                    None => -up,
                },
                (None, _) => -up,
            };
            tangent(raw, *n)
        }).collect();
        // Smoothed between neighbours (Jacobi: each pass reads the last).
        let reach = 2.2 * spacing.max(1e-6);
        let mut grid: std::collections::HashMap<(i32, i32, i32), Vec<usize>> = std::collections::HashMap::new();
        let key = |p: Vec3| {
            let c = (p / reach).floor().as_ivec3();
            (c.x, c.y, c.z)
        };
        for (i, (p, _)) in roots.iter().enumerate() {
            grid.entry(key(*p)).or_default().push(i);
        }
        for _ in 0..4 {
            let last = flow.clone();
            for (i, (p, n)) in roots.iter().enumerate() {
                if pinned[i] {
                    continue;
                }
                let (x, y, z) = key(*p);
                let mut sum = last[i];
                for dx in -1..=1 {
                    for dy in -1..=1 {
                        for dz in -1..=1 {
                            for &j in grid.get(&(x + dx, y + dy, z + dz)).into_iter().flatten() {
                                let d = p.distance(roots[j].0);
                                if j != i && d < reach && roots[j].1.dot(*n) > 0.3 && part_side(roots[j].0) == part_side(*p) {
                                    sum += last[j] * (1.0 - d / reach);
                                }
                            }
                        }
                    }
                }
                flow[i] = tangent(sum, *n);
            }
        }
        // The guides: rising off the root, settling toward the surface.
        let to_hair = self.scene.rest_world_matrix(hair_id).inverse();
        let hair_scale = to_hair.to_scale_rotation_translation().0.abs().max_element().max(1e-9);
        let world = crate::groom::head::InHair::new(&head, Mat4::IDENTITY, lift);
        let rise = (5.0 + 60.0 * (1.0 - lie)).to_radians();
        let settle = rise * 0.25;
        let mut lengths_used = (f32::MAX, 0.0f32);
        // Long fur hangs: over the body, the eyes and the ground (the
        // clay's lowest point: the soles stand on it).
        let over: Vec<[f32; 4]> = eyes.iter().map(|(c, r)| [c.x, c.y, c.z, r * 1.02]).collect();
        let hang = crate::hair::fur::Drape { weight: drape, down: Vec3::NEG_Y, floor: floor_y.map(|y| (Vec3::Y, y)), body: Some(&world), over: &over, lift };
        let guides: Vec<Guide> = roots.iter().zip(&flow).map(|((p, n), f)| {
            let owner = head.owner_name(*p);
            let l = lengths.iter().find(|(r, _)| r.holds(*p, owner.as_deref())).map_or(length, |(_, l)| *l);
            lengths_used = (lengths_used.0.min(l), lengths_used.1.max(l));
            let mut points = vec![*p + *n * lift];
            if let Some((lid, lp)) = lid_at(*p).and_then(|lid| Some((lid, lid.strand(*p, 0.0, 1.0, segments, lift)?))) {
                // Lying on the lid toward its margin (`FurLid::strand`).
                lengths_used = (lengths_used.0.min(lid.length), lengths_used.1.max(lid.length));
                points = lp;
            } else if drape > 0.0 {
                points = crate::hair::fur::drape_guide(points[0], *f * rise.cos() + *n * rise.sin(), l, segments, &hang);
            } else {
                for k in 0..segments {
                    let t = (k as f32 + 0.5) / segments as f32;
                    let a = rise + (settle - rise) * t;
                    let last = *points.last().expect("root");
                    points.push(last + (*f * a.cos() + *n * a.sin()) * (l / segments as f32));
                }
            }
            crate::groom::keep_out(&mut points, &world);
            if let Some(y) = floor_y {
                // The ground holds: nothing under it (a paw's sole would
                // push hair down through it).
                for q in points.iter_mut().skip(1) {
                    q.y = q.y.max(y + lift);
                }
            }
            Guide { points: points.iter().map(|q| to_hair.transform_point3(*q)).collect(), width: None }
        }).collect();
        // A quarter of the strands' own spacing: they cover the skin about
        // one and a half times over (the skin, the same colour, shows a
        // little between them, as under real short fur).
        let strand_width = num("strand_width")?.map_or_else(|| (0.25 * (area / strands.max(1) as f32).sqrt()).clamp(1e-5, 0.01), |v| v as f32);
        if !(1e-6..=0.1).contains(&strand_width) {
            return Err("grow.strand_width must be 1e-6 to 0.1".into());
        }
        let sphere = |(c, r): &(Vec3, f32)| {
            let q = to_hair.transform_point3(*c);
            [q.x, q.y, q.z, r * hair_scale]
        };
        let fur = Fur {
            spacing: spacing * hair_scale,
            tint,
            tip_color,
            tip_mix,
            exclude: exclude.clone(),
            include: include.clone(),
            bare: bare_all.iter().map(sphere).collect(),
            avoid: eyes.iter().map(|(c, r)| sphere(&(*c, r * 1.02))).collect(),
            jitter,
            drape,
            clump,
            clump_size,
            // A gentle wave (3% of the length) unless asked otherwise.
            wave: wave_arg.unwrap_or(if drape > 0.0 { 0.03 * length } else { 0.0 }) * hair_scale,
            waves,
            ragged,
            part: part_at.map(|a| plane_in(self.scene.rest_world_matrix(hair_id), side, a)),
            floor: floor_y.map(|y| plane_in(self.scene.rest_world_matrix(hair_id), Vec3::Y, Vec3::Y * y)),
            // The regions again, per strand (`Fur::regions`).
            regions: lengths.iter().map(|(r, l)| (r, Some(*l), None)).chain(directions.iter().map(|(r, d)| (r, None, Some(*d)))).map(|(r, length, direction)| FurRegion {
                blobs: match r { Region::Blobs(n) => n.clone(), Region::Sphere(..) => Vec::new() },
                sphere: match r { Region::Sphere(c, rad) => Some(sphere(&(*c, *rad))), Region::Blobs(_) => None },
                length: length.map(|l| l * hair_scale),
                direction: direction.map(|d: Vec3| to_hair.transform_vector3(d).normalize_or(d).into()),
            }).collect(),
            length: if lengths.is_empty() { 0.0 } else { length * hair_scale },
            lids: lids.iter().map(|l| FurLid {
                center: to_hair.transform_point3(Vec3::from(l.center)).into(),
                axis: to_hair.transform_vector3(Vec3::from(l.axis)).normalize_or(Vec3::Y).into(),
                radius: l.radius * hair_scale,
                half: l.half * hair_scale,
                reach: l.reach * hair_scale,
                length: l.length * hair_scale,
                copies: l.copies,
            }).collect(),
        };
        let report = json!({
            "fur": true, "on": self.scene.get(on).map(|o| o.name.clone()), "guides": guides.len(), "candidates": tries, "candidates_excluded": excluded,
            "surface_area": area, "furred_area": kept_area, "root_spacing": spacing, "root_spacing_cv": cv,
            "strands": strands, "strands_per_square_unit": strands as f32 / kept_area.max(1e-9), "strand_width": strand_width,
            "length": length, "lengths": [lengths_used.0, lengths_used.1], "segments": segments, "lie": lie, "drape": drape, "floor": floor_y, "clump": clump, "clump_size": clump_size, "part": part_x, "wave": wave_arg.unwrap_or(if drape > 0.0 { 0.03 * length } else { 0.0 }), "waves": waves, "ragged": ragged,
            "exclude": exclude, "include": include, "bare_spheres": fur.bare.len(), "eyes_avoided": eyes.len(),
            "lids_furred": lids.len(), "lid_length": lids.iter().map(|l| l.length).fold(None, |a: Option<f32>, l| Some(a.map_or(l, |a| a.max(l)))), "lid_guides": roots.iter().filter(|(p, _)| lid_at(*p).is_some()).count(),
        });
        Ok(FurGrown { guides, fur, strands, strand_width: strand_width * hair_scale, lift: lift * hair_scale, frizz: if drape > 0.0 { 0.02 } else { 0.08 } * length * hair_scale, report })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Sample elimination on a unit square's random points keeps exactly
    /// the asked count and spreads them evenly: nearest-neighbour spacing
    /// varies by under a fifth (random points: about half).
    #[test]
    fn elimination_spreads_evenly() {
        let mut rng = Rng(7);
        let pts: Vec<Vec3> = (0..6000).map(|_| Vec3::new(rng.next(), rng.next(), 0.0)).collect();
        let (_, random_cv) = crate::hair::fur::spacing_stats(&pts[..1000]);
        let kept = eliminate(&pts, 1000, 1.0);
        assert_eq!(kept.len(), 1000);
        assert!(kept.windows(2).all(|w| w[0] < w[1]), "in order");
        let sel: Vec<Vec3> = kept.iter().map(|&i| pts[i]).collect();
        let (mean, cv) = crate::hair::fur::spacing_stats(&sel);
        eprintln!("1000 of 6000: mean spacing {mean:.4} (hex packing {:.4}), cv {cv:.3}; random points cv {random_cv:.3}", (2.0 / (3f32.sqrt() * 1000.0)).sqrt());
        assert!(cv < 0.2 && random_cv > 0.4);
        assert_eq!(eliminate(&pts, 1000, 1.0), kept, "deterministic");
    }
}
