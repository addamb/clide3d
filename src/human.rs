//! Human base bodies: a correct human to start a character from, built from
//! MakeHuman's CC0 base mesh and shape targets (see assets/makehuman).
//!
//! A target is a set of point moves (a longer nose, an older face, a heavier
//! body). `Human` says how much of each to apply: "macro" controls (sex, age,
//! weight, muscle, height, proportions, ancestry) choose and blend the
//! matching whole-body targets, and `shapes` sets any other target by name
//! (`nose/nose-scale-horiz`: -1 narrower .. 1 wider). The result is a closed
//! mesh, used in clay as a blob (`Form::Human`) through its exact signed
//! distance, so sculpting, paint, rigs, bakes and film renders all work on it.
//!
//! File layout of human.bin.gz (little endian, gzip): "MHB1"; u32 vertex
//! count, then x y z f32 each (meters, feet at y = 0); u32 face count, then
//! 4 u32 indices each (u32::MAX = triangle); u16 marker count, then each: u8
//! name length, name, u16 point count, u32 indices; u16 target count, then
//! each: u16 name length, name, u32 entry count, then u16 index and 3 i16
//! moves in steps of 0.05 mm.

use std::collections::{BTreeMap, HashMap};
use std::io::Read;
use std::sync::{Arc, Mutex, OnceLock};

use glam::Vec3;
use serde::{Deserialize, Serialize};

const STEP: f32 = 5e-5;

pub struct Asset {
    base: Vec<Vec3>,
    faces: Vec<[u32; 4]>,
    markers: HashMap<String, Vec<u32>>,
    targets: HashMap<String, (Vec<u16>, Vec<[i16; 3]>)>,
}

/// The built-in human data, unpacked on first use.
pub fn asset() -> &'static Asset {
    static ASSET: OnceLock<Asset> = OnceLock::new();
    ASSET.get_or_init(|| Asset::parse(include_bytes!("../assets/makehuman/human.bin.gz")).expect("built-in human data is valid"))
}

struct Reader<'a>(&'a [u8]);

impl Reader<'_> {
    fn take(&mut self, n: usize) -> Option<&[u8]> {
        let (a, b) = (self.0.get(..n)?, self.0.get(n..)?);
        self.0 = b;
        Some(a)
    }
    fn u8(&mut self) -> Option<u8> {
        Some(self.take(1)?[0])
    }
    fn u16(&mut self) -> Option<u16> {
        Some(u16::from_le_bytes(self.take(2)?.try_into().ok()?))
    }
    fn i16(&mut self) -> Option<i16> {
        Some(i16::from_le_bytes(self.take(2)?.try_into().ok()?))
    }
    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }
    fn f32(&mut self) -> Option<f32> {
        Some(f32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }
    fn name(&mut self, len: usize) -> Option<String> {
        String::from_utf8(self.take(len)?.to_vec()).ok()
    }
}

impl Asset {
    fn parse(gz: &[u8]) -> Option<Asset> {
        let mut raw = Vec::new();
        flate2::read::GzDecoder::new(gz).read_to_end(&mut raw).ok()?;
        let mut r = Reader(&raw);
        (r.take(4)? == b"MHB1").then_some(())?;
        let nv = r.u32()? as usize;
        let base = (0..nv).map(|_| Some(Vec3::new(r.f32()?, r.f32()?, r.f32()?))).collect::<Option<Vec<_>>>()?;
        let nf = r.u32()? as usize;
        let faces = (0..nf).map(|_| Some([r.u32()?, r.u32()?, r.u32()?, r.u32()?])).collect::<Option<Vec<_>>>()?;
        let mut markers = HashMap::new();
        for _ in 0..r.u16()? {
            let len = r.u8()? as usize;
            let name = r.name(len)?;
            let n = r.u16()? as usize;
            markers.insert(name, (0..n).map(|_| r.u32()).collect::<Option<Vec<_>>>()?);
        }
        let mut targets = HashMap::new();
        for _ in 0..r.u16()? {
            let len = r.u16()? as usize;
            let name = r.name(len)?;
            let n = r.u32()? as usize;
            let (mut idx, mut d) = (Vec::with_capacity(n), Vec::with_capacity(n));
            for _ in 0..n {
                idx.push(r.u16()?);
                d.push([r.i16()?, r.i16()?, r.i16()?]);
            }
            targets.insert(name, (idx, d));
        }
        Some(Asset { base, faces, markers, targets })
    }

    /// Every target that `Human::shapes` can name: (control name, kind),
    /// where kind is "-1..1" for a decrease/increase pair or "0..1".
    pub fn controls(&self) -> Vec<(String, &'static str)> {
        let mut out = BTreeMap::new();
        for name in self.targets.keys().filter(|n| !n.starts_with("macrodetails/")) {
            let paired = PAIRS.iter().find_map(|(lo, hi)| name.strip_suffix(lo).or_else(|| name.strip_suffix(hi)).filter(|stem| self.targets.contains_key(&format!("{stem}{lo}")) && self.targets.contains_key(&format!("{stem}{hi}"))));
            match paired {
                Some(stem) => out.insert(both_sides(stem), "-1..1"),
                None => out.insert(both_sides(name), "0..1"),
            };
        }
        out.into_iter().collect()
    }
}

/// Opposite target endings: a negative control value uses the first.
const PAIRS: [(&str, &str); 5] = [("-decr", "-incr"), ("-down", "-up"), ("-backward", "-forward"), ("-in", "-out"), ("-less", "-more")];

/// "ears/l-ear-lobe" -> "ears/ear-lobe": one control for both sides.
fn both_sides(name: &str) -> String {
    match name.rsplit_once('/') {
        Some((dir, file)) if file.starts_with("l-") || file.starts_with("r-") => format!("{dir}/{}", &file[2..]),
        _ => name.to_string(),
    }
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
#[serde(default)]
pub struct Human {
    /// 0 = female, 1 = male.
    pub sex: f32,
    /// Years, 18 to 90.
    pub age: f32,
    /// 0 = thin, 0.5 = average, 1 = heavy.
    pub weight: f32,
    /// 0 = soft, 0.5 = average, 1 = muscular.
    pub muscle: f32,
    /// 0 = short, 0.5 = average, 1 = tall.
    pub height: f32,
    /// 0 = uncommon, 0.5 = average, 1 = idealized proportions.
    pub proportions: f32,
    /// Ancestry mix (normalized): shapes of face and body.
    pub african: f32,
    pub asian: f32,
    pub caucasian: f32,
    /// Any other target by name (see `Asset::controls`), e.g.
    /// "nose/nose-scale-horiz": 0.4 or "ears/ear-shape-pointed": 1.
    pub shapes: BTreeMap<String, f32>,
    /// Face skin detail (see `face.rs`): pores, wrinkles, color, lips.
    /// It changes shading only, never the shape.
    #[serde(skip_serializing_if = "is_default_skin")]
    pub skin: crate::face::Skin,
}

fn is_default_skin(s: &crate::face::Skin) -> bool {
    *s == crate::face::Skin::default()
}

impl Default for Human {
    fn default() -> Self {
        Human { sex: 0.5, age: 25.0, weight: 0.5, muscle: 0.5, height: 0.5, proportions: 0.5, african: 1.0 / 3.0, asian: 1.0 / 3.0, caucasian: 1.0 / 3.0, shapes: BTreeMap::new(), skin: crate::face::Skin::default() }
    }
}

/// A morphed human: triangles in meters (feet at y = 0) and marker centers.
pub struct Body {
    pub points: Vec<Vec3>,
    pub triangles: Vec<[u32; 3]>,
    markers: HashMap<String, Vec3>,
}

impl Body {
    /// Center of a MakeHuman marker: "joint-head", "joint-l-knee",
    /// "helper-l-eye"...
    pub fn marker(&self, name: &str) -> Option<Vec3> {
        self.markers.get(name).copied()
    }

    /// Radius of an eyeball helper.
    pub fn eye_radius(&self, side: &str) -> Option<f32> {
        let idx = asset().markers.get(&format!("helper-{side}-eye"))?;
        let c = self.marker(&format!("helper-{side}-eye"))?;
        Some(idx.iter().map(|&i| self.points[i as usize].distance(c)).sum::<f32>() / idx.len().max(1) as f32)
    }
}

impl Human {
    pub fn sanitize(&mut self) {
        let fix = |v: f32, lo: f32, hi: f32, d: f32| if v.is_finite() { v.clamp(lo, hi) } else { d };
        let d = Human::default();
        self.sex = fix(self.sex, 0.0, 1.0, d.sex);
        self.age = fix(self.age, 18.0, 90.0, d.age);
        for (v, dv) in [(&mut self.weight, 0.5), (&mut self.muscle, 0.5), (&mut self.height, 0.5), (&mut self.proportions, 0.5)] {
            *v = fix(*v, 0.0, 1.0, dv);
        }
        for v in [&mut self.african, &mut self.asian, &mut self.caucasian] {
            *v = fix(*v, 0.0, 1.0, 1.0 / 3.0);
        }
        if self.african + self.asian + self.caucasian <= 1e-6 {
            (self.african, self.asian, self.caucasian) = (1.0 / 3.0, 1.0 / 3.0, 1.0 / 3.0);
        }
        self.shapes.retain(|k, v| k.len() <= 128 && v.is_finite());
        for v in self.shapes.values_mut() {
            *v = v.clamp(-1.0, 1.0);
        }
        self.skin.sanitize();
        if self.shapes.len() > 256 {
            let keep: Vec<String> = self.shapes.keys().take(256).cloned().collect();
            self.shapes.retain(|k, _| keep.contains(k));
        }
    }

    /// Targets and how much of each, for these controls.
    pub fn weights(&self) -> Result<Vec<(String, f32)>, String> {
        let mut h = self.clone();
        h.sanitize();
        let a = asset();
        let mut out = Vec::new();
        let sexes = [("female", 1.0 - h.sex), ("male", h.sex)];
        let old = ((h.age - 25.0) / 65.0).clamp(0.0, 1.0);
        let ages = [("young", 1.0 - old), ("old", old)];
        let three = |v: f32| -> [(&'static str, f32); 3] {
            if v < 0.5 { [("min", 1.0 - 2.0 * v), ("average", 2.0 * v), ("max", 0.0)] } else { [("min", 0.0), ("average", 2.0 - 2.0 * v), ("max", 2.0 * v - 1.0)] }
        };
        let race_sum = h.african + h.asian + h.caucasian;
        let races = [("african", h.african / race_sum), ("asian", h.asian / race_sum), ("caucasian", h.caucasian / race_sum)];
        let (hlo, hhi) = (((0.5 - h.height) * 2.0).max(0.0), ((h.height - 0.5) * 2.0).max(0.0));
        let (plo, phi) = (((0.5 - h.proportions) * 2.0).max(0.0), ((h.proportions - 0.5) * 2.0).max(0.0));
        for (s, ws) in sexes {
            for (g, wa) in ages {
                let w = ws * wa;
                if w <= 0.0 {
                    continue;
                }
                for (r, wr) in races {
                    out.push((format!("macrodetails/{r}-{s}-{g}"), w * wr));
                }
                for (m, wm) in three(h.muscle) {
                    for (wt, ww) in three(h.weight) {
                        out.push((format!("macrodetails/universal-{s}-{g}-{m}muscle-{wt}weight"), w * wm * ww));
                    }
                }
                let stem = format!("{s}-{g}-averagemuscle-averageweight");
                out.push((format!("macrodetails/height/{stem}-minheight"), w * hlo));
                out.push((format!("macrodetails/height/{stem}-maxheight"), w * hhi));
                out.push((format!("macrodetails/proportions/{stem}-uncommonproportions"), w * plo));
                out.push((format!("macrodetails/proportions/{stem}-idealproportions"), w * phi));
            }
        }
        for (key, &v) in &h.shapes {
            let names = shape_targets(a, key, v).ok_or_else(|| format!("unknown human shape {key}; list them with human_controls"))?;
            out.extend(names);
        }
        out.retain(|(n, w)| *w > 1e-4 && a.targets.contains_key(n));
        Ok(out)
    }

    pub fn build(&self) -> Result<Body, String> {
        let a = asset();
        let mut points = a.base.clone();
        for (name, w) in self.weights()? {
            let (idx, d) = &a.targets[&name];
            for (&i, d) in idx.iter().zip(d) {
                points[i as usize] += Vec3::new(d[0] as f32, d[1] as f32, d[2] as f32) * (STEP * w);
            }
        }
        let mut triangles = Vec::with_capacity(a.faces.len() * 2);
        for f in &a.faces {
            triangles.push([f[0], f[1], f[2]]);
            if f[3] != u32::MAX {
                triangles.push([f[0], f[2], f[3]]);
            }
        }
        let markers = a.markers.iter().map(|(n, idx)| (n.clone(), idx.iter().map(|&i| points[i as usize]).sum::<Vec3>() / idx.len().max(1) as f32)).collect();
        Ok(Body { points, triangles, markers })
    }
}

/// Targets for one `shapes` entry: a pair by sign, a single target by
/// amount, on both sides when the name leaves out "l-"/"r-".
fn shape_targets(a: &Asset, key: &str, v: f32) -> Option<Vec<(String, f32)>> {
    let (dir, file) = key.rsplit_once('/')?;
    let sides: Vec<String> = if a.targets.keys().any(|n| n.starts_with(&format!("{dir}/l-{file}"))) { vec![format!("{dir}/l-{file}"), format!("{dir}/r-{file}")] } else { vec![key.to_string()] };
    let mut out = Vec::new();
    for stem in sides {
        if a.targets.contains_key(&stem) {
            out.push((stem, v.max(0.0)));
            continue;
        }
        let (lo, hi) = PAIRS.iter().find(|(lo, hi)| a.targets.contains_key(&format!("{stem}{lo}")) && a.targets.contains_key(&format!("{stem}{hi}")))?;
        out.push((format!("{stem}{}", if v < 0.0 { lo } else { hi }), v.abs()));
    }
    Some(out)
}

/// One bone of the standard human rig: (name, parent, head marker, tail marker).
type BoneSpec = (&'static str, Option<&'static str>, &'static str, &'static str);

/// The skeleton `create_human` builds, on MakeHuman's joint markers.
/// Sided bones are written for the left (+X) side and mirrored.
const RIG: [BoneSpec; 16] = [
    ("hips", None, "joint-pelvis", "joint-spine-3"),
    ("spine", Some("hips"), "joint-spine-3", "joint-spine-2"),
    ("chest", Some("spine"), "joint-spine-2", "joint-spine-1"),
    ("upper_chest", Some("chest"), "joint-spine-1", "joint-neck"),
    ("neck", Some("upper_chest"), "joint-neck", "joint-head"),
    ("head", Some("neck"), "joint-head", "joint-head-2"),
    ("shoulder.L", Some("upper_chest"), "joint-l-clavicle", "joint-l-shoulder"),
    ("upperarm.L", Some("shoulder.L"), "joint-l-shoulder", "joint-l-elbow"),
    ("forearm.L", Some("upperarm.L"), "joint-l-elbow", "joint-l-hand"),
    ("hand.L", Some("forearm.L"), "joint-l-hand", "joint-l-finger-3-1"),
    ("thigh.L", Some("hips"), "joint-l-upper-leg", "joint-l-knee"),
    ("shin.L", Some("thigh.L"), "joint-l-knee", "joint-l-ankle"),
    ("foot.L", Some("shin.L"), "joint-l-ankle", "joint-l-foot-1"),
    ("toe.L", Some("foot.L"), "joint-l-foot-1", "joint-l-foot-2"),
    ("jaw", Some("head"), "joint-head", "joint-jaw"),
    ("root", None, "joint-ground", "joint-pelvis"),
];

impl Body {
    /// The standard human rig: spine, neck, head, jaw, arms with three bones
    /// per finger (thumb = finger1), legs with feet and toes. Left bones are
    /// `.L` (+X), right `.R`.
    pub fn rig(&self) -> Vec<crate::rig::Bone> {
        let m = |n: &str| self.marker(n).unwrap_or(Vec3::ZERO);
        let mut bones = Vec::new();
        let mut push = |name: String, parent: Option<String>, head: Vec3, tail: Vec3, deform: bool| bones.push(crate::rig::Bone { name, parent, head, tail, deform });
        for (name, parent, head, tail) in RIG {
            // The head clay rides the head bone rigidly, so a jaw bone would
            // only pull at the neck; it is kept for props and later use.
            let deform = !matches!(name, "root" | "jaw");
            if let Some(stem) = name.strip_suffix(".L") {
                for (suffix, side) in [(".L", "l"), (".R", "r")] {
                    let parent = parent.map(|p| p.replace(".L", suffix));
                    push(format!("{stem}{suffix}"), parent, m(&head.replace("-l-", &format!("-{side}-"))), m(&tail.replace("-l-", &format!("-{side}-"))), deform);
                }
            } else {
                push(name.to_string(), parent.map(str::to_string), m(head), m(tail), deform);
            }
        }
        for (suffix, side) in [(".L", "l"), (".R", "r")] {
            for f in 1..=5 {
                for k in 1..=3 {
                    let parent = if k == 1 { format!("hand{suffix}") } else { format!("finger{f}_{}{suffix}", k - 1) };
                    let (a, b) = (m(&format!("joint-{side}-finger-{f}-{k}")), m(&format!("joint-{side}-finger-{f}-{}", k + 1)));
                    push(format!("finger{f}_{k}{suffix}"), Some(parent), a, b, true);
                }
            }
        }
        bones
    }

    /// Detail regions for a whole-person clay: the face (brows to chin,
    /// cheek to cheek, placed from the eyes) and each hand with its
    /// fingers, at the given cells (None = no region).
    pub fn detail_regions(&self, face: Option<f32>, hands: Option<f32>) -> Vec<crate::clay::Region> {
        let mut regions = Vec::new();
        let [(l, size), (r, _)] = self.eyes();
        if let Some(cell) = face {
            let mid = (l + r) * 0.5;
            let width = (l - r).length().max(0.05) + size;
            regions.push(crate::clay::Region { name: "face".into(), center: mid + Vec3::new(0.0, -0.035, 0.0), radii: Vec3::new(width * 1.05, width * 1.3, width), cell });
        }
        if let Some(cell) = hands {
            for side in ["l", "r"] {
                let (Some(wrist), Some(knuckle)) = (self.marker(&format!("joint-{side}-hand")), self.marker(&format!("joint-{side}-finger-3-1"))) else { continue };
                let along = knuckle - wrist;
                let len = along.length().max(0.05);
                regions.push(crate::clay::Region { name: format!("hand.{}", side.to_uppercase()), center: wrist + along * 1.4, radii: Vec3::splat(len * 1.25), cell });
            }
        }
        regions
    }

    /// Eyeball centers and diameters (left, right), sized to fill the
    /// sockets so the lids lie on the eyes (MakeHuman's eye proxy is about
    /// 30 mm across; give these eyes an iris of `EYE_IRIS`).
    pub fn eyes(&self) -> [(Vec3, f32); 2] {
        ["l", "r"].map(|s| (self.marker(&format!("helper-{s}-eye")).unwrap_or(Vec3::ZERO), 2.0 * self.eye_radius(s).unwrap_or(0.012) * 0.97))
    }
}

/// Iris width (as a part of the eye) that keeps a human iris about 11.7 mm
/// across in `Body::eyes`.
pub const EYE_IRIS: f32 = 0.41;

/// Catmull–Clark subdivision, `levels` times: the smooth surface a
/// polygon cage stands for. Faces are quads (`u32::MAX` 4th index =
/// triangle); the result is all quads. Points not on any face keep their
/// place but are dropped from the output (markers are read before this).
pub fn subdivide(points: &[Vec3], faces: &[[u32; 4]], levels: u32) -> (Vec<Vec3>, Vec<[u32; 4]>) {
    let mut pts = points.to_vec();
    let mut polys: Vec<Vec<u32>> = faces.iter().map(|f| f.iter().copied().filter(|&i| i != u32::MAX).collect()).collect();
    for _ in 0..levels {
        let n = pts.len();
        let face_pt: Vec<Vec3> = polys.iter().map(|f| f.iter().map(|&i| pts[i as usize]).sum::<Vec3>() / f.len() as f32).collect();
        // Edges: their faces, and the new point index each will get.
        let mut edges: HashMap<(u32, u32), (Vec<usize>, u32)> = HashMap::new();
        for (fi, f) in polys.iter().enumerate() {
            for k in 0..f.len() {
                let (a, b) = (f[k], f[(k + 1) % f.len()]);
                edges.entry((a.min(b), a.max(b))).or_insert_with(|| (Vec::new(), 0)).0.push(fi);
            }
        }
        let mut out = pts.clone();
        let face_base = n as u32;
        out.extend(face_pt.iter().copied());
        let mut keys: Vec<(u32, u32)> = edges.keys().copied().collect();
        keys.sort_unstable();
        for k in &keys {
            let (fs, idx) = edges.get_mut(k).expect("key");
            let (a, b) = (pts[k.0 as usize], pts[k.1 as usize]);
            let p = if fs.len() == 2 { (a + b + face_pt[fs[0]] + face_pt[fs[1]]) / 4.0 } else { (a + b) / 2.0 };
            *idx = out.len() as u32;
            out.push(p);
        }
        // Move the old points: (F + 2R + (n - 3) P) / n.
        let mut f_sum = vec![Vec3::ZERO; n];
        let mut r_sum = vec![Vec3::ZERO; n];
        let mut valence = vec![0u32; n];
        let mut boundary = vec![false; n];
        for (fi, f) in polys.iter().enumerate() {
            for &i in f {
                f_sum[i as usize] += face_pt[fi];
            }
        }
        for (k, (fs, _)) in &edges {
            let mid = (pts[k.0 as usize] + pts[k.1 as usize]) / 2.0;
            for v in [k.0, k.1] {
                r_sum[v as usize] += mid;
                valence[v as usize] += 1;
                boundary[v as usize] |= fs.len() != 2;
            }
        }
        for v in 0..n {
            let m = valence[v] as f32;
            if m < 3.0 || boundary[v] {
                continue;
            }
            // Faces around a point equal its edges on a closed surface.
            out[v] = (f_sum[v] / m + 2.0 * r_sum[v] / m + (m - 3.0) * pts[v]) / m;
        }
        let mut next = Vec::with_capacity(polys.len() * 4);
        for (fi, f) in polys.iter().enumerate() {
            let e = |a: u32, b: u32| edges[&(a.min(b), a.max(b))].1;
            for k in 0..f.len() {
                let (prev, cur, nxt) = (f[(k + f.len() - 1) % f.len()], f[k], f[(k + 1) % f.len()]);
                next.push(vec![cur, e(cur, nxt), face_base + fi as u32, e(prev, cur)]);
            }
        }
        pts = out;
        polys = next;
    }
    (pts, polys.into_iter().map(|q| [q[0], q[1], q[2], q[3]]).collect())
}

/// Exact signed distance to a closed triangle mesh (negative inside), with
/// a box tree for fast nearest-triangle search and angle-weighted pseudo
/// normals for the sign (Bærentzen & Aanæs).
pub struct MeshSdf {
    tris: Vec<[Vec3; 3]>,
    /// Pseudo normals per triangle: face, edges (ab, bc, ca), corners (a, b, c).
    normals: Vec<[Vec3; 7]>,
    nodes: Vec<Node>,
    order: Vec<u32>,
    pub lo: Vec3,
    pub hi: Vec3,
}

struct Node {
    lo: Vec3,
    hi: Vec3,
    /// Leaf: first index into `order` and count; inner: children.
    a: u32,
    b: u32,
    leaf: bool,
}

impl MeshSdf {
    pub fn new(points: &[Vec3], triangles: &[[u32; 3]]) -> MeshSdf {
        let tris: Vec<[Vec3; 3]> = triangles.iter().map(|t| t.map(|i| points[i as usize])).collect();
        let face: Vec<Vec3> = tris.iter().map(|[a, b, c]| (*b - *a).cross(*c - *a).normalize_or_zero()).collect();
        let mut vertex = vec![Vec3::ZERO; points.len()];
        let mut edge: HashMap<(u32, u32), Vec3> = HashMap::new();
        for (t, tri) in triangles.iter().enumerate() {
            for k in 0..3 {
                let (i, j, l) = (tri[k], tri[(k + 1) % 3], tri[(k + 2) % 3]);
                let (p, q, r) = (points[i as usize], points[j as usize], points[l as usize]);
                let angle = (q - p).normalize_or_zero().dot((r - p).normalize_or_zero()).clamp(-1.0, 1.0).acos();
                vertex[i as usize] += face[t] * angle;
                *edge.entry((i.min(j), i.max(j))).or_default() += face[t];
            }
        }
        let normals = triangles
            .iter()
            .enumerate()
            .map(|(t, tri)| {
                let e = |k: usize| edge[&(tri[k].min(tri[(k + 1) % 3]), tri[k].max(tri[(k + 1) % 3]))];
                [face[t], e(0), e(1), e(2), vertex[tri[0] as usize], vertex[tri[1] as usize], vertex[tri[2] as usize]]
            })
            .collect();
        let mut order: Vec<u32> = (0..tris.len() as u32).collect();
        let mut nodes = Vec::new();
        if !tris.is_empty() {
            build(&mut nodes, &mut order, &tris, 0, tris.len());
        }
        let (lo, hi) = nodes.first().map_or((Vec3::ZERO, Vec3::ZERO), |n| (n.lo, n.hi));
        MeshSdf { tris, normals, nodes, order, lo, hi }
    }

    pub fn distance(&self, p: Vec3) -> f32 {
        if self.nodes.is_empty() {
            return f32::MAX;
        }
        let mut best = (f32::MAX, Vec3::ZERO, Vec3::Y);
        let mut stack = [0u32; 64];
        let mut sp = 1;
        while sp > 0 {
            sp -= 1;
            let n = stack[sp];
            let node = &self.nodes[n as usize];
            if box_dist2(p, node.lo, node.hi) >= best.0 {
                continue;
            }
            if node.leaf {
                for &t in &self.order[node.a as usize..(node.a + node.b) as usize] {
                    let (q, feature) = closest(p, &self.tris[t as usize]);
                    let d2 = p.distance_squared(q);
                    if d2 < best.0 {
                        best = (d2, q, self.normals[t as usize][feature]);
                    }
                }
            } else {
                // Visit the nearer child first.
                let (l, r) = (&self.nodes[node.a as usize], &self.nodes[node.b as usize]);
                let (first, second) = if box_dist2(p, l.lo, l.hi) < box_dist2(p, r.lo, r.hi) { (node.b, node.a) } else { (node.a, node.b) };
                if sp + 2 <= stack.len() {
                    stack[sp] = first;
                    stack[sp + 1] = second;
                    sp += 2;
                }
            }
        }
        let d = best.0.sqrt();
        if (p - best.1).dot(best.2) < 0.0 { -d } else { d }
    }
}

fn box_dist2(p: Vec3, lo: Vec3, hi: Vec3) -> f32 {
    let d = (lo - p).max(p - hi).max(Vec3::ZERO);
    d.length_squared()
}

fn build(nodes: &mut Vec<Node>, order: &mut [u32], tris: &[[Vec3; 3]], start: usize, end: usize) -> u32 {
    let (mut lo, mut hi) = (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN));
    for &t in &order[start..end] {
        for p in tris[t as usize] {
            lo = lo.min(p);
            hi = hi.max(p);
        }
    }
    let me = nodes.len() as u32;
    nodes.push(Node { lo, hi, a: start as u32, b: (end - start) as u32, leaf: true });
    if end - start <= 4 {
        return me;
    }
    let axis = (hi - lo).max_position();
    let center = |t: u32| (tris[t as usize][0] + tris[t as usize][1] + tris[t as usize][2])[axis];
    order[start..end].sort_by(|&a, &b| center(a).total_cmp(&center(b)));
    let mid = (start + end) / 2;
    let a = build(nodes, order, tris, start, mid);
    let b = build(nodes, order, tris, mid, end);
    nodes[me as usize] = Node { lo, hi, a, b, leaf: false };
    me
}

/// Closest point on a triangle, and which pseudo normal applies there
/// (index into `MeshSdf::normals`: 0 face, 1..=3 edges, 4..=6 corners).
pub(crate) fn closest(p: Vec3, [a, b, c]: &[Vec3; 3]) -> (Vec3, usize) {
    // Ericson, Real-Time Collision Detection 5.1.5, with feature tracking.
    let (ab, ac, ap) = (*b - *a, *c - *a, p - *a);
    let (d1, d2) = (ab.dot(ap), ac.dot(ap));
    if d1 <= 0.0 && d2 <= 0.0 {
        return (*a, 4);
    }
    let bp = p - *b;
    let (d3, d4) = (ab.dot(bp), ac.dot(bp));
    if d3 >= 0.0 && d4 <= d3 {
        return (*b, 5);
    }
    let vc = d1 * d4 - d3 * d2;
    if vc <= 0.0 && d1 >= 0.0 && d3 <= 0.0 {
        return (*a + ab * (d1 / (d1 - d3)), 1);
    }
    let cp = p - *c;
    let (d5, d6) = (ab.dot(cp), ac.dot(cp));
    if d6 >= 0.0 && d5 <= d6 {
        return (*c, 6);
    }
    let vb = d5 * d2 - d1 * d6;
    if vb <= 0.0 && d2 >= 0.0 && d6 <= 0.0 {
        return (*a + ac * (d2 / (d2 - d6)), 3);
    }
    let va = d3 * d6 - d5 * d4;
    if va <= 0.0 && (d4 - d3) >= 0.0 && (d5 - d6) >= 0.0 {
        return (*b + (*c - *b) * ((d4 - d3) / ((d4 - d3) + (d5 - d6))), 2);
    }
    let denom = 1.0 / (va + vb + vc);
    (*a + ab * (vb * denom) + ac * (vc * denom), 0)
}

/// A built human, ready for clay.
pub struct Shape {
    pub sdf: MeshSdf,
    /// Bounds of the part above `neck_cut` (the head part's box; the
    /// whole body's box is `sdf.lo` / `sdf.hi`).
    pub head_lo: Vec3,
    pub head_hi: Vec3,
    /// Height of the cut between a head clay and a body clay: across the
    /// neck, below the jaw.
    pub neck_cut: f32,
    /// Where the eyes, nose, mouth and ears are (see `face.rs`).
    pub landmarks: Option<crate::face::Landmarks>,
}

/// Built humans by their settings, so every Field over the same human
/// shares one distance structure.
pub fn shape(h: &Human) -> Result<Arc<Shape>, String> {
    // Skin settings do not change the shape.
    let h = &Human { skin: crate::face::Skin::default(), ..h.clone() };
    type Cache = Mutex<Vec<(Human, Arc<Shape>)>>;
    static CACHE: OnceLock<Cache> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(Vec::new()));
    if let Some((_, s)) = cache.lock().expect("cache").iter().find(|(k, _)| k == h) {
        return Ok(s.clone());
    }
    let mut body = h.build()?;
    // The base mesh is made to be smoothed: subdivide before measuring.
    let (points, quads) = subdivide(&body.points, &asset().faces, 2);
    let landmarks = crate::face::Landmarks::of(&points, &body);
    body.points = points;
    body.triangles = quads.iter().flat_map(|q| [[q[0], q[1], q[2]], [q[0], q[2], q[3]]]).collect();
    let (neck, head) = (body.marker("joint-neck").unwrap_or(Vec3::Y * 1.45), body.marker("joint-head").unwrap_or(Vec3::Y * 1.55));
    let neck_cut = neck.y + 0.3 * (head.y - neck.y);
    let (head_lo, head_hi) = body.points.iter().filter(|p| p.y >= neck_cut).fold((Vec3::splat(f32::MAX), Vec3::splat(f32::MIN)), |(l, h), p| (l.min(*p), h.max(*p)));
    let s = Arc::new(Shape { sdf: MeshSdf::new(&body.points, &body.triangles), head_lo, head_hi, neck_cut, landmarks });
    let mut c = cache.lock().expect("cache");
    c.push((h.clone(), s.clone()));
    if c.len() > 8 {
        c.remove(0);
    }
    Ok(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// create_human detail: a face region around the eyes and one per hand
    /// holding the fingers, all on the body.
    #[test]
    fn detail_regions_sit_on_the_face_and_hands() {
        let body = Human::default().build().unwrap();
        let regions = body.detail_regions(Some(0.00025), Some(0.0003));
        assert_eq!(regions.iter().map(|r| r.name.as_str()).collect::<Vec<_>>(), ["face", "hand.L", "hand.R"]);
        let [(l, _), (r, _)] = body.eyes();
        let face = &regions[0];
        assert!(face.center.distance((l + r) * 0.5) < 0.05 && face.radii.x > (l - r).length());
        for (side, region) in ["l", "r"].iter().zip(&regions[1..]) {
            let tip = body.marker(&format!("joint-{side}-finger-3-4")).or_else(|| body.marker(&format!("joint-{side}-finger-3-3"))).unwrap();
            let wrist = body.marker(&format!("joint-{side}-hand")).unwrap();
            assert!(((tip - region.center) / region.radii).length() < 1.0, "{side}: fingertip outside");
            assert!(((wrist - region.center) / region.radii).length() < 1.3, "{side}: wrist far outside");
            assert_eq!(region.cell, 0.0003);
        }
        assert!(body.detail_regions(None, None).is_empty());
    }

    #[test]
    fn default_human_stands_on_the_ground_and_is_closed() {
        let body = Human::default().build().unwrap();
        let (lo, hi) = body.points.iter().fold((Vec3::splat(f32::MAX), Vec3::splat(f32::MIN)), |(l, h), p| (l.min(*p), h.max(*p)));
        let height = hi.y - lo.y;
        assert!((1.55..1.95).contains(&height), "height {height}");
        assert!(lo.y.abs() < 0.08, "feet at {}", lo.y);
        let s = MeshSdf::new(&body.points, &body.triangles);
        let chest = body.marker("joint-spine-2").unwrap();
        assert!(s.distance(chest) < 0.0, "inside the chest");
        assert!(s.distance(chest + Vec3::new(0.0, 0.0, 1.0)) > 0.5, "in front of the body");
        let head = body.marker("joint-head").unwrap();
        assert!(head.y > 1.3);
        assert!(body.eye_radius("l").unwrap() > 0.008);
    }

    #[test]
    fn controls_change_the_body() {
        let tall = Human { height: 1.0, ..Human::default() }.build().unwrap();
        let short = Human { height: 0.0, ..Human::default() }.build().unwrap();
        let top = |b: &Body| b.points.iter().map(|p| p.y).fold(f32::MIN, f32::max);
        assert!(top(&tall) > top(&short) + 0.1);
        let mut pointed = Human::default();
        pointed.shapes.insert("ears/ear-shape-pointed".into(), 1.0);
        assert!(pointed.weights().unwrap().iter().any(|(n, _)| n == "ears/l-ear-shape-pointed"));
        let mut bad = Human::default();
        bad.shapes.insert("nose/no-such".into(), 1.0);
        assert!(bad.build().is_err());
        assert!(asset().controls().iter().any(|(n, k)| n == "nose/nose-scale-horiz" && *k == "-1..1"));
    }
}


#[cfg(test)]
mod part_tests {
    use crate::clay::Clay;

    #[test]
    fn the_head_part_grid_covers_only_the_head() {
        let clay: Clay = serde_json::from_value(serde_json::json!({ "blobs": [{ "type": "human", "part": "head", "human": { "sex": 0.0 }, "blend": 0 }], "resolution": 256 })).unwrap();
        // About 1.5 mm cells at 256 (the body width would give about 4 mm).
        let cell = clay.cell_size().unwrap();
        assert!(cell < 0.002, "cell {cell}");
    }
}

