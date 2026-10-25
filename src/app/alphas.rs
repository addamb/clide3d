//! The `alpha` tool (the scene's alpha library, see `brush::alpha`), how
//! sculpt calls bind the alphas their strokes name, and posable symmetry's
//! way from the posed clay back to its rest pose.

use std::collections::HashMap;

use base64::Engine;
use glam::{IVec3, Mat4, Vec3};
use serde_json::{Value, json};

use super::ClideApp;
use super::tools::text;
use crate::brush::alpha::{Alpha, AlphaRef, AlphaStore, MAX_ALPHAS, MAX_FILE_BYTES, MAX_SIDE, name_ok};
use crate::brush::resolve::Resolved;
use crate::brush::spec::StrokeSpec;
use crate::clay::Detail;
use crate::ledger;
use crate::mcp::ToolResult;

/// Largest preview the tool returns, in pixels.
const MAX_PREVIEW: u32 = 512;

impl ClideApp {
    /// `alpha`: import, list, delete, preview or capture (from_view) the
    /// scene's alphas.
    pub(super) fn alpha_tool(&mut self, args: &Value) -> ToolResult {
        match args["action"].as_str() {
            Some("import") => {
                let name = alpha_name(args)?;
                let path = args["path"].as_str().ok_or("import needs path: an absolute path to a PNG (8 or 16 bit), JPEG or EXR")?;
                let bytes = read_bounded(path)?;
                let alpha = Alpha::decode(&bytes)?;
                self.install_alpha(&name, alpha, "import")
            }
            Some("from_view") => {
                let name = alpha_name(args)?;
                let alpha = self.alpha_from_view(args)?;
                self.install_alpha(&name, alpha, "from_view")
            }
            Some("list") => {
                let list: Vec<Value> = self.scene.alphas.iter().map(|(name, a)| {
                    let (w, h) = a.size();
                    let used: Vec<Value> = self.clays_binding(name, None).into_iter().map(|id| json!(self.scene.get(id).map(|o| o.name.clone()))).collect();
                    json!({ "name": name, "kind": if a.vdm().is_some() { "vdm" } else { "image" }, "hash": a.hash(), "w": w, "h": h, "bits": 16, "used_by": used })
                }).collect();
                Ok(text(json!({ "alphas": list, "max": MAX_ALPHAS })))
            }
            Some("delete") => {
                let name = alpha_name(args)?;
                let gone = self.scene.alphas.remove(&name).ok_or_else(|| format!("no alpha named {name}"))?;
                let kept: Vec<Value> = self.clays_binding(&name, None).into_iter().map(|id| json!(self.scene.get(id).map(|o| o.name.clone()))).collect();
                Ok(text(json!({ "deleted": name, "hash": gone.hash(), "kept_by": kept, "note": "clays keep the images their strokes use, so those strokes still replay" })))
            }
            Some("preview") => {
                let a = match (args["name"].as_str(), args["hash"].as_str()) {
                    (Some(n), _) => self.scene.alphas.get(n).cloned().ok_or_else(|| format!("no alpha named {n}"))?,
                    (None, Some(h)) => {
                        let r = AlphaRef { hash: Some(serde_json::from_value(json!(h)).map_err(|e| format!("hash: {e}"))?), ..AlphaRef::default() };
                        self.scene.alphas.values().find(|a| Some(a.hash()) == r.hash).cloned().ok_or("no alpha with that hash in the library")?
                    }
                    _ => return Err("preview needs name (or hash)".into()),
                };
                let side = args["size"].as_u64().map_or(256, |s| s.clamp(16, MAX_PREVIEW as u64) as u32);
                let (w, h) = a.size();
                let png = a.preview_png(side);
                let mut out = text(json!({ "hash": a.hash(), "w": w, "h": h, "bits": 16, "preview": "8-bit gray, downscaled" }));
                out.push(json!({ "type": "image", "data": base64::engine::general_purpose::STANDARD.encode(png), "mimeType": "image/png" }));
                Ok(out)
            }
            Some("create_vdm") => {
                let name = alpha_name(args)?;
                let (alpha, at) = self.vdm_from_clay(args)?;
                self.install_alpha_with(&name, alpha, "create_vdm", Some(at))
            }
            _ => Err("action must be import, list, delete, preview, from_view or create_vdm".into()),
        }
    }

    /// Clays whose stack binds `name` (to anything but `except`).
    fn clays_binding(&self, name: &str, except: Option<crate::ledger::Hash128>) -> Vec<u64> {
        self.scene
            .objects
            .iter()
            .filter(|o| o.clay.as_ref().and_then(|c| c.sculpt.as_ref()).and_then(|s| s.data().alphas.names.get(name)).is_some_and(|h| Some(*h) != except))
            .map(|o| o.id)
            .collect()
    }

    /// Puts `alpha` in the library as `name`. A name that had other pixels
    /// is rebound on every clay that used it, so their records made with
    /// the old image report stale_context (rebase takes the new one).
    fn install_alpha(&mut self, name: &str, alpha: Alpha, how: &str) -> ToolResult {
        self.install_alpha_with(name, alpha, how, None)
    }

    /// `install_alpha`, its reply naming a VDM stamp's capture (`vdm`).
    fn install_alpha_with(&mut self, name: &str, alpha: Alpha, how: &str, vdm: Option<Value>) -> ToolResult {
        if !self.scene.alphas.contains_key(name) && self.scene.alphas.len() >= MAX_ALPHAS {
            return Err(format!("the scene holds at most {MAX_ALPHAS} alphas; delete one first"));
        }
        let rebind = self.clays_binding(name, Some(alpha.hash()));
        // Rebinding changes what a rebase reads: unchecked ledgers settle
        // first. Every store must take the image before any changes.
        self.settle_clays(rebind.clone())?;
        // A rebase would hand the bound records (live strokes settled) the
        // new kind: a VDM under a name image strokes use, or an image under
        // a VDM's, could never be rebased, so it is refused.
        let vdm_kind = alpha.vdm().is_some();
        for &id in &rebind {
            let Some(o) = self.scene.get(id) else { continue };
            let Some(clay) = o.clay.as_ref() else { continue };
            let mut clash = false;
            let mut check = |r: &crate::brush::resolve::Resolved| clash |= r.params.alpha.as_ref().is_some_and(|a| a.name.as_deref() == Some(name)) && (r.params.brush == crate::sculpt::Brush::Vdm) != vdm_kind;
            ledger::alpha_strokes(clay, &mut check);
            // Archived layers' records read the name as they made it too.
            ledger::archived_alpha_strokes(clay, &mut check);
            if clash {
                let (now, other) = if vdm_kind { ("an image", "a VDM stamp") } else { ("a VDM stamp", "an image") };
                return Err(format!("{}'s strokes use alpha {name} as {now}; it cannot become {other} (use another name, or delete those strokes)", o.name));
            }
        }
        let mut stores = Vec::new();
        for &id in &rebind {
            let clay = self.scene.get(id).and_then(|o| o.clay.as_ref()).ok_or("object vanished")?;
            let mut store = clay.sculpt.as_ref().map(|s| s.data().alphas.clone()).unwrap_or_default();
            let (names, hashes) = ledger::alphas_in_use(clay);
            store.prune(&names, &hashes);
            store.keep(Some(name), &alpha).map_err(|e| format!("{}: {e}", self.scene.get(id).map_or("", |o| o.name.as_str())))?;
            stores.push((id, store));
        }
        let mut stale = 0;
        for (id, store) in stores {
            let clay = self.scene.get_mut(id).and_then(|o| o.clay.as_mut()).expect("checked");
            ledger::set_alphas(clay, store);
            let ctx = <Detail as ledger::Grid>::context(clay);
            stale += ledger::ledger::<Detail>(clay).records().iter().filter(|r| r.stale(ctx, clay)).count();
        }
        let replaced = self.scene.alphas.get(name).map(|old| old.hash() != alpha.hash());
        let (w, h) = alpha.size();
        let hash = alpha.hash();
        self.scene.alphas.insert(name.to_string(), alpha);
        let mut reply = json!({ "name": name, "hash": hash, "w": w, "h": h, "bits": 16, "from": how, "replaced": replaced, "stale_records": stale });
        if let Some(v) = vdm {
            reply["vdm"] = v;
        }
        Ok(text(reply))
    }

    /// The image an authored alpha names: the library's (by name, or by
    /// hash), else the one clay `id` keeps.
    pub(super) fn find_alpha(&self, id: u64, a: &AlphaRef) -> Result<Alpha, String> {
        let store = self.scene.get(id).and_then(|o| o.clay.as_ref()).and_then(|c| c.sculpt.as_ref()).map(|s| &s.data().alphas);
        let by_hash = |h| self.scene.alphas.values().find(|x| x.hash() == h).or_else(|| store.and_then(|s| s.images.get(&h))).cloned();
        match (&a.name, a.hash) {
            (Some(n), want) => {
                let found = self.scene.alphas.get(n).cloned().or_else(|| store.and_then(|s| s.names.get(n)).and_then(|h| by_hash(*h)));
                let found = found.ok_or_else(|| format!("no alpha named {n}; alpha import adds one (alpha list shows them)"))?;
                match want {
                    Some(h) if h != found.hash() => Err(format!("alpha {n} is {}, not {h}", found.hash())),
                    _ => Ok(found),
                }
            }
            (None, Some(h)) => by_hash(h).ok_or_else(|| format!("no alpha with hash {h} in the scene or on this clay")),
            (None, None) => Err("an alpha needs name (from alpha import) or hash".into()),
        }
    }

    /// The clay's alpha store with the images `strokes` use (bound to the
    /// names they give); None when no stroke uses one.
    pub(super) fn alphas_for(&self, id: u64, strokes: &[(StrokeSpec, Resolved)]) -> Result<Option<AlphaStore>, String> {
        let used: Vec<&crate::brush::alpha::AlphaParams> = strokes.iter().filter_map(|(_, r)| r.params.alpha.as_ref()).collect();
        if used.is_empty() {
            return Ok(None);
        }
        let clay = self.scene.get(id).and_then(|o| o.clay.as_ref()).ok_or("not a clay object")?;
        let mut store = clay.sculpt.as_ref().map(|s| s.data().alphas.clone()).unwrap_or_default();
        // Images no record uses any more (deleted, squashed or rebased
        // strokes) make room first.
        let (names, hashes) = ledger::alphas_in_use(clay);
        store.prune(&names, &hashes);
        for (_, r) in strokes {
            let Some(a) = &r.params.alpha else { continue };
            let image = self.find_alpha(id, &AlphaRef { name: None, hash: Some(a.hash), ..AlphaRef::default() })?;
            let label = a.name.clone().unwrap_or_else(|| a.hash.to_string());
            match (r.params.brush == crate::sculpt::Brush::Vdm, image.vdm().is_some()) {
                (true, false) => return Err(format!("alpha {label} is an image; brush vdm stamps a VDM (alpha create_vdm makes one)")),
                (false, true) => return Err(format!("alpha {label} is a VDM stamp: use it with brush vdm")),
                _ => {}
            }
        }
        for a in used {
            let image = self.find_alpha(id, &AlphaRef { name: None, hash: Some(a.hash), ..AlphaRef::default() })?;
            store.keep(a.name.as_deref(), &image)?;
        }
        Ok(Some(store))
    }

    /// `alpha create_vdm`: a VDM stamp captured from a clay (as sculpted,
    /// rest pose) around `center` (see `brush::vdm`), and where it stood.
    fn vdm_from_clay(&mut self, args: &Value) -> Result<(Alpha, Value), String> {
        let id = self.resolve(&args["target"])?;
        let o = self.scene.get(id).ok_or("object vanished")?;
        if o.shape != crate::scene::Shape::Clay {
            return Err(format!("{} is not a clay object: create_vdm captures clay", o.name));
        }
        let (to_local, radius_scale) = self.clay_space(id, args)?;
        let vec = |k: &str| -> Result<Option<Vec3>, String> {
            match &args[k] {
                Value::Null => Ok(None),
                v => serde_json::from_value::<[f32; 3]>(v.clone()).map(|a| Some(Vec3::from(a))).map_err(|_| format!("{k} must be [x, y, z]")),
            }
        };
        let num = |k: &str| -> Result<Option<f32>, String> {
            match &args[k] {
                Value::Null => Ok(None),
                v => v.as_f64().map(|x| Some(x as f32)).ok_or(format!("{k} must be a number")),
            }
        };
        let center = vec("center")?.ok_or("create_vdm needs center: [x, y, z], the stamp's base point on the clay (it moves onto the surface along normal)")?;
        let radius = num("radius")?.ok_or("create_vdm needs radius: half the stamp's width (world units)")?;
        let above = num("above")?.unwrap_or(radius * 2.0);
        let below = num("below")?.unwrap_or(radius * 0.5);
        // Unchecked ledgers settle first: the capture reads the clay as
        // sculpted.
        self.settle_clays([id])?;
        let clay = self.scene.get(id).and_then(|o| o.clay.as_ref()).ok_or("the clay has no blobs yet")?;
        let normal_m = to_local.inverse().transpose();
        let capture = crate::brush::vdm::Capture {
            center: to_local.transform_point3(center),
            normal: vec("normal")?.map(|n| normal_m.transform_vector3(n)),
            tangent: vec("tangent")?.map(|t| to_local.transform_vector3(t)),
            radius: radius * radius_scale,
            above: above * radius_scale,
            below: below * radius_scale,
            cell: num("cell")?.map(|c| c * radius_scale),
        };
        let started = std::time::Instant::now();
        let got = crate::brush::vdm::capture(clay, &capture)?;
        let v = &got.vdm;
        let r = |x: f32| ((x as f64) * 1e6).round() / 1e6;
        let at = json!({
            "base": got.base.to_array().map(r),
            "normal": got.normal.to_array().map(r),
            "radius": r(v.radius),
            "above": r(v.above),
            "below": r(v.below),
            "cell": r(v.cell()),
            "bricks": v.grid.grid().brick_count(),
            "space": "the clay's own (object) units",
            "capture_ms": (started.elapsed().as_secs_f64() * 1e4).round() / 10.0,
        });
        Ok((Alpha::from_vdm(got.vdm)?, at))
    }

    /// `alpha from_view`: a height map of a clay as seen from a named view
    /// (orthographic, world units): the nearest surface is white, misses
    /// black. Rasterized from the rest mesh.
    fn alpha_from_view(&mut self, args: &Value) -> Result<Alpha, String> {
        let id = self.resolve(&args["target"])?;
        self.meshes.sync(&self.scene);
        let mesh = self.meshes.whole(id).ok_or("from_view captures a clay object with a mesh")?.clone();
        let size = args["size"].as_u64().unwrap_or(256);
        if !(16..=MAX_SIDE as u64).contains(&size) {
            return Err(format!("size must be 16 to {MAX_SIDE} pixels"));
        }
        let size = size as usize;
        let view = args["view"].as_str().unwrap_or("front");
        // World point -> (horizontal, vertical, nearness to the viewer).
        let to_view: fn(Vec3) -> Vec3 = match view {
            "front" => |p| Vec3::new(p.x, p.y, p.z),
            "back" => |p| Vec3::new(p.x, p.y, -p.z),
            "right" => |p| Vec3::new(p.z, p.y, p.x),
            "left" => |p| Vec3::new(p.z, p.y, -p.x),
            "top" => |p| Vec3::new(p.x, p.z, p.y),
            "bottom" => |p| Vec3::new(p.x, p.z, -p.y),
            other => return Err(format!("unknown view {other}; use front, back, right, left, top or bottom")),
        };
        let world = self.scene.world_matrix(id);
        let pts: Vec<Vec3> = mesh.vertices.iter().map(|v| to_view(world.transform_point3(Vec3::from(v.pos)))).collect();
        let (lo, hi) = pts.iter().fold((Vec3::splat(f32::MAX), Vec3::splat(f32::MIN)), |(a, b), p| (a.min(*p), b.max(*p)));
        if lo.x > hi.x {
            return Err("the clay has no surface to capture".into());
        }
        let number = |k: &str| args[k].as_f64().map(|v| v as f32).filter(|v| v.is_finite());
        let center = match args["center"].as_array() {
            Some(c) if c.len() == 2 => glam::Vec2::new(c[0].as_f64().unwrap_or(0.0) as f32, c[1].as_f64().unwrap_or(0.0) as f32),
            Some(_) => return Err("center is [horizontal, vertical] in the view (world units)".into()),
            None => glam::Vec2::new(lo.x + hi.x, lo.y + hi.y) * 0.5,
        };
        let extent = number("extent").unwrap_or((hi.x - lo.x).max(hi.y - lo.y) * 0.525);
        if !(extent > 0.0) || !center.is_finite() {
            return Err("extent must be above 0 (half the captured square, world units)".into());
        }
        // Pixel (column, row) of a view point.
        let px = |p: Vec3| glam::Vec2::new((p.x - center.x) / extent * 0.5 + 0.5, 0.5 - (p.y - center.y) / extent * 0.5) * size as f32;
        let mut near = vec![f32::MIN; size * size];
        for t in mesh.indices.chunks_exact(3) {
            let (a, b, c) = (pts[t[0] as usize], pts[t[1] as usize], pts[t[2] as usize]);
            let (pa, pb, pc) = (px(a), px(b), px(c));
            let area = (pb - pa).perp_dot(pc - pa);
            if area.abs() < 1e-12 {
                continue;
            }
            let (mn, mx) = (pa.min(pb).min(pc), pa.max(pb).max(pc));
            let (x0, x1) = ((mn.x - 0.5).ceil().max(0.0) as usize, ((mx.x - 0.5).floor().min(size as f32 - 1.0)).max(-1.0));
            let (y0, y1) = ((mn.y - 0.5).ceil().max(0.0) as usize, ((mx.y - 0.5).floor().min(size as f32 - 1.0)).max(-1.0));
            if x1 < 0.0 || y1 < 0.0 {
                continue;
            }
            for y in y0..=y1 as usize {
                for x in x0..=x1 as usize {
                    let q = glam::Vec2::new(x as f32 + 0.5, y as f32 + 0.5);
                    let (w0, w1) = ((pc - pb).perp_dot(q - pb) / area, (pa - pc).perp_dot(q - pc) / area);
                    let w2 = 1.0 - w0 - w1;
                    if w0 < 0.0 || w1 < 0.0 || w2 < 0.0 {
                        continue;
                    }
                    let z = a.z * w0 + b.z * w1 + c.z * w2;
                    let cell = &mut near[x + y * size];
                    *cell = cell.max(z);
                }
            }
        }
        let hit: Vec<f32> = near.iter().copied().filter(|z| *z > f32::MIN).collect();
        if hit.is_empty() {
            return Err("the clay is not in that part of the view; leave center and extent out to frame it".into());
        }
        let (zlo, zhi) = hit.iter().fold((f32::MAX, f32::MIN), |(a, b), z| (a.min(*z), b.max(*z)));
        let span = (zhi - zlo).max(1e-9);
        let data = near.iter().map(|z| if *z > f32::MIN { ((z - zlo) / span * 65535.0).round().clamp(0.0, 65535.0) as u16 } else { 0 }).collect();
        Alpha::new(size as u32, size as u32, data)
    }

    /// Posable symmetry: for a point in the call's space on the clay as
    /// posed at `posed`, the map from the call's space back to its rest
    /// pose (object space), through the skin weights of the nearest posed
    /// vertex (inverse linear blend skinning). A clay that is not posed
    /// there maps by `to_local` alone.
    pub(super) fn unposer(&self, id: u64, posed: Option<f32>, to_local: Mat4) -> Result<Unposer, String> {
        let o = self.scene.get(id).ok_or("object vanished")?;
        let rig = match &o.rig {
            Some(r) if r.is_posed(posed) => r,
            _ => return Ok(Box::new(move |_| Some(to_local))),
        };
        let (mesh, skin) = self.meshes.rest(id).ok_or("clay has no mesh yet")?;
        let skin = skin.filter(|s| s.joints.len() == mesh.vertices.len()).ok_or("the clay's skin weights are not built yet; try again once its mesh is")?;
        let mats = rig.skin_matrices(posed);
        let blend = move |j: [u16; 4], w: [f32; 4]| {
            let mut m = Mat4::ZERO;
            for k in 0..4 {
                if w[k] > 0.0 {
                    m += mats.get(j[k] as usize).copied().unwrap_or(Mat4::IDENTITY) * w[k];
                }
            }
            m
        };
        let (joints, weights) = (skin.joints.clone(), skin.weights.clone());
        let (kept, pts) = posed_points(mesh.vertices.iter().map(|v| Vec3::from(v.pos)), |i| blend(joints[i], weights[i]));
        let near = Nearest::new(pts);
        Ok(Box::new(move |p| {
            let q = to_local.transform_point3(p);
            let i = kept[near.find(q)?];
            let inv = blend(joints[i], weights[i]).inverse() * to_local;
            inv.is_finite().then_some(inv)
        }))
    }
}

/// Posable symmetry's map from a posed point (call's space) to the rest
/// pose (object space); `None` off the posed clay.
pub(super) type Unposer = Box<dyn Fn(Vec3) -> Option<Mat4>>;

/// The `name` argument, checked.
fn alpha_name(args: &Value) -> Result<String, String> {
    let name = args["name"].as_str().map(str::trim).ok_or("name is required: the alpha's name in the scene")?;
    if !name_ok(name) {
        return Err(format!("alpha names are 1 to {} characters", crate::brush::alpha::MAX_NAME));
    }
    Ok(name.to_string())
}

/// A file's bytes, refused past `MAX_FILE_BYTES` before they are read.
fn read_bounded(path: &str) -> Result<Vec<u8>, String> {
    use std::io::Read;
    let file = std::fs::File::open(path).map_err(|e| format!("cannot read {path}: {e}"))?;
    let len = file.metadata().map_err(|e| e.to_string())?.len();
    if len > MAX_FILE_BYTES {
        return Err(format!("{path} is {len} bytes; alpha images are at most {} MiB", MAX_FILE_BYTES >> 20));
    }
    let mut bytes = Vec::with_capacity(len as usize);
    file.take(MAX_FILE_BYTES + 1).read_to_end(&mut bytes).map_err(|e| e.to_string())?;
    if bytes.len() as u64 > MAX_FILE_BYTES {
        return Err(format!("{path} grew past {} MiB while it was read", MAX_FILE_BYTES >> 20));
    }
    Ok(bytes)
}

/// The vertices a posed point can unpose through, as (their indices,
/// their posed positions): a vertex whose skin blend (`blend(i)`) is zero,
/// singular or not finite has no way back to the rest pose, so it is left
/// out rather than refusing the strokes that land nearest to it.
fn posed_points(rest: impl Iterator<Item = Vec3>, blend: impl Fn(usize) -> Mat4) -> (Vec<usize>, Vec<Vec3>) {
    rest.enumerate()
        .filter_map(|(i, v)| {
            let m = blend(i);
            let det = m.determinant();
            (m.is_finite() && det.is_finite() && det.abs() > 1e-12).then(|| (i, m.transform_point3(v)))
        })
        .unzip()
}

/// Nearest-point queries on a fixed set of points (a uniform grid).
struct Nearest {
    pts: Vec<Vec3>,
    /// The grid's origin (the points' low corner) and its far corner:
    /// cells count from `lo`, so every key stays small.
    lo: Vec3,
    hi: Vec3,
    cell: f32,
    grid: HashMap<IVec3, Vec<usize>>,
    /// Farther than this from every point is off the clay.
    reach: f32,
}

impl Nearest {
    fn new(pts: Vec<Vec3>) -> Nearest {
        let (lo, hi) = pts.iter().fold((Vec3::splat(f32::MAX), Vec3::splat(f32::MIN)), |(a, b), p| (a.min(*p), b.max(*p)));
        let diag = if pts.is_empty() { 1.0 } else { (hi - lo).length().max(1e-6) };
        let cell = diag / 64.0;
        let mut grid: HashMap<IVec3, Vec<usize>> = HashMap::new();
        for (i, p) in pts.iter().enumerate() {
            grid.entry(((*p - lo) / cell).floor().as_ivec3()).or_default().push(i);
        }
        Nearest { pts, lo, hi, cell, grid, reach: diag * 0.05 }
    }

    /// The nearest point's index, within `reach`. A query outside the
    /// points' box grown by `reach` (or not finite) is off the clay, so
    /// the cell coordinates below stay within a few cells of the grid.
    fn find(&self, q: Vec3) -> Option<usize> {
        if self.pts.is_empty() || !q.is_finite() || q.cmplt(self.lo - self.reach).any() || q.cmpgt(self.hi + self.reach).any() {
            return None;
        }
        let c = ((q - self.lo) / self.cell).floor().as_ivec3();
        let rings = (self.reach / self.cell).ceil() as i32 + 1;
        let mut best: Option<(f32, usize)> = None;
        for r in 0..=rings {
            if best.is_some_and(|(d, _)| d < ((r - 1).max(0) as f32 * self.cell).powi(2)) {
                break;
            }
            for z in -r..=r {
                for y in -r..=r {
                    for x in -r..=r {
                        if x.abs().max(y.abs()).max(z.abs()) != r {
                            continue;
                        }
                        for &i in self.grid.get(&(c + IVec3::new(x, y, z))).into_iter().flatten() {
                            let d = self.pts[i].distance_squared(q);
                            if best.is_none_or(|(b, j)| d < b || (d == b && i < j)) {
                                best = Some((d, i));
                            }
                        }
                    }
                }
            }
        }
        best.filter(|(d, _)| *d <= self.reach * self.reach).map(|(_, i)| i)
    }
}

#[cfg(test)]
mod tests;
