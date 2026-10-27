//! `surface_point`: where a surface is. Agents place eyes, paint and
//! sculpt strokes on surfaces they cannot see exactly; this answers "what
//! do I hit looking from the front at (x, y)?", "where does this ray hit?"
//! and "what is the nearest clay surface to this point?". Paint and sculpt
//! use the same snapping with `snap: true`.

use glam::{Mat4, Vec3};
use serde_json::{Value, json};

use super::ClideApp;
use crate::mcp::ToolResult;
use crate::scene::Shape;
use crate::surface::Surface;

/// Most rays, `at` points (per page) and snapped points in one call.
pub(super) const MAX_PROBES: usize = 1024;

impl ClideApp {
    pub(super) fn surface_point(&mut self, args: &Value) -> ToolResult {
        // Counts first: refused whole, never cut short.
        let count = |k: &str| args[k].as_array().map_or(0, Vec::len);
        for k in ["rays", "points"] {
            if count(k) > MAX_PROBES {
                return Err(format!("{} {k} given; at most {MAX_PROBES} per call: send them in batches of {MAX_PROBES}", count(k)));
            }
        }
        let whole = |k: &str| -> Result<Option<usize>, String> {
            match args.get(k).filter(|v| !v.is_null()) {
                None => Ok(None),
                Some(v) => v.as_u64().map(|n| Some(n.min(usize::MAX as u64) as usize)).ok_or(format!("{k} must be a whole number")),
            }
        };
        let (offset, limit) = (whole("offset")?.unwrap_or(0), whole("limit")?.unwrap_or(MAX_PROBES));
        if !(1..=MAX_PROBES).contains(&limit) {
            return Err(format!("limit must be 1 to {MAX_PROBES}"));
        }
        let all_at = args["at"].as_array().map(Vec::as_slice).unwrap_or_default();
        if !all_at.is_empty() && !args["view"].is_string() {
            return Err("at needs view: front, back, right, left, top or bottom (at is [horizontal, vertical] on that view)".into());
        }
        if offset > 0 && offset >= all_at.len() {
            return Err(format!("offset {offset} is past the {} at points", all_at.len()));
        }
        // One page of `at`: the reply says where the next one starts.
        let at = &all_at[offset.min(all_at.len())..(offset.saturating_add(limit)).min(all_at.len())];
        let next_offset = (offset + at.len() < all_at.len()).then_some(offset + at.len());
        self.meshes.sync(&self.scene);
        let target = match args.get("target").filter(|t| !t.is_null()) {
            Some(t) => Some(self.resolve(t)?),
            None => None,
        };
        let mut out = Vec::new();
        // Rays: given directly, or straight in from a view at screen coordinates.
        let mut rays: Vec<(Vec3, Vec3)> = Vec::with_capacity(count("rays") + at.len());
        for r in args["rays"].as_array().into_iter().flatten() {
            let v = |k: &str| -> Result<Vec3, String> { serde_json::from_value::<[f32; 3]>(r[k].clone()).map(Vec3::from).map_err(|_| format!("each ray needs {k}: [x, y, z]")) };
            rays.push((v("origin")?, v("direction")?.normalize_or_zero()));
        }
        if let Some(view) = args["view"].as_str() {
            let far = self.scene_sphere().1 * 4.0 + 10.0;
            for a in at {
                let [h, v]: [f32; 2] = serde_json::from_value(a.clone()).map_err(|_| "at must be a list of [horizontal, vertical] pairs")?;
                rays.push(match view {
                    "front" => (Vec3::new(h, v, far), Vec3::NEG_Z),
                    "back" => (Vec3::new(h, v, -far), Vec3::Z),
                    "right" => (Vec3::new(far, v, h), Vec3::NEG_X),
                    "left" => (Vec3::new(-far, v, h), Vec3::X),
                    "top" => (Vec3::new(h, far, v), Vec3::NEG_Y),
                    "bottom" => (Vec3::new(h, -far, v), Vec3::Y),
                    other => return Err(format!("unknown view {other}; use front, back, right, left, top or bottom")),
                });
            }
        }
        for (origin, dir) in rays {
            out.push(self.cast(target, origin, dir));
        }
        // Points: snap to the nearest surface of one clay object.
        let points = args["points"].as_array().map(Vec::as_slice).unwrap_or_default();
        if !points.is_empty() {
            let id = target.ok_or("points need a target clay object")?;
            let o = self.scene.get(id).ok_or("object vanished")?;
            if o.shape != Shape::Clay {
                return Err(format!("{} is not a clay object; points snap to clay only (use rays for others)", o.name));
            }
            let world = self.scene.world_matrix(id);
            let snap = self.snapper(id)?;
            for p in points {
                let p: [f32; 3] = serde_json::from_value(p.clone()).map_err(|_| "points must be [x, y, z] lists")?;
                let local = world.inverse().transform_point3(Vec3::from(p));
                out.push(match snap(local) {
                    Some((q, n)) => {
                        // The persistent mask and polygroup there (clay space).
                        let clay = o.clay.as_ref();
                        let mask = clay.and_then(|c| c.sculpt_mask.as_ref()).map_or(0.0, |m| m.grid().value(q));
                        let group = clay.and_then(|c| c.groups.as_ref()).map(|g| g.label(q)).filter(|g| *g != 0);
                        let (q, n) = (world.transform_point3(q), world.inverse().transpose().transform_vector3(n).normalize_or_zero());
                        json!({ "from": p, "hit": true, "object": o.name, "point": r3(q), "normal": r3(n), "distance": round(q.distance(Vec3::from(p))), "mask": round(mask), "group": group })
                    }
                    None => json!({ "from": p, "hit": false }),
                });
            }
        }
        if out.is_empty() {
            return Err("pass rays, view + at, or points".into());
        }
        let mut reply = json!({ "results": out });
        if !all_at.is_empty() && args["view"].is_string() {
            reply["at_offset"] = json!(offset);
            reply["at_total"] = json!(all_at.len());
            reply["next_offset"] = json!(next_offset);
        }
        Ok(super::tools::text(reply))
    }

    /// First visible mesh a world ray hits (only `target` and what is under
    /// it, if given), as drawn now.
    fn cast(&self, target: Option<u64>, origin: Vec3, dir: Vec3) -> Value {
        let mut best: Option<(f32, u64, Vec3)> = None;
        let under = target.map(|t| {
            let mut ids = vec![t];
            ids.extend(self.scene.descendants(t));
            ids
        });
        // Reference image planes are not geometry: rays pass through them.
        for o in self.scene.geometry() {
            if !self.scene.is_visible(o.id) || under.as_ref().is_some_and(|u| !u.contains(&o.id)) {
                continue;
            }
            let Some(mesh) = self.meshes.get(o) else { continue };
            let world: Mat4 = self.scene.world_matrix(o.id);
            let inv = world.inverse();
            let (lo, ld) = (inv.transform_point3(origin), inv.transform_vector3(dir));
            if let Some((t, tri)) = self.meshes.ray_hit(mesh, lo, ld).map(|h| (h.t, h.tri))
                && best.is_none_or(|b| t < b.0)
            {
                let p = |i: u32| Vec3::from(mesh.vertices[i as usize].pos);
                let ix = &mesh.indices[tri * 3..tri * 3 + 3];
                let n = (p(ix[1]) - p(ix[0])).cross(p(ix[2]) - p(ix[0]));
                let mut n = inv.transpose().transform_vector3(n).normalize_or_zero();
                if n.dot(dir) > 0.0 {
                    n = -n;
                }
                best = Some((t, o.id, n));
            }
        }
        match best {
            Some((t, id, n)) => {
                let q = origin + dir * t;
                let name = self.scene.get(id).map(|o| o.name.clone()).unwrap_or_default();
                json!({ "origin": r3(origin), "direction": r3(dir), "hit": true, "object": name, "point": r3(q), "normal": r3(n) })
            }
            None => json!({ "origin": r3(origin), "direction": r3(dir), "hit": false }),
        }
    }

    /// Snaps clay-space points onto a clay object's surface.
    pub(super) fn snapper(&self, id: u64) -> Result<impl Fn(Vec3) -> Option<(Vec3, Vec3)> + '_, String> {
        let clay = self.scene.get(id).and_then(|o| o.clay.as_ref()).ok_or("not a clay object")?;
        let mesh = self.meshes.whole(id).ok_or("clay has no mesh yet")?;
        let s = Surface::new(clay, mesh).ok_or("clay is empty")?;
        Ok(move |p| s.snap(p))
    }

    /// Casts clay-space rays at a clay object's mesh: (hit point, normal
    /// facing the ray). Screen-frame strokes start here.
    pub(super) fn clay_caster(&self, id: u64) -> Result<impl Fn(Vec3, Vec3) -> Option<(Vec3, Vec3)> + Send + Sync + 'static, String> {
        let mesh = self.meshes.whole(id).ok_or("clay has no mesh yet")?.clone();
        // Through a tree: the brute-force hit, bit for bit (see `mesh::pick`).
        let bvh = self.meshes.bvh(&mesh);
        Ok(move |origin: Vec3, dir: Vec3| cast(&mesh, &bvh, origin, dir))
    }
}

impl ClideApp {
    /// Casts object-space rays at a Poly's drawn level (its shown level):
    /// what `paint` on a Poly resolves screen strokes on.
    pub(super) fn poly_caster(&self, id: u64) -> Result<impl Fn(Vec3, Vec3) -> Option<(Vec3, Vec3)> + Send + Sync + 'static, String> {
        let o = self.scene.get(id).filter(|o| o.poly.is_some()).ok_or("not a Poly object")?;
        let mesh = self.meshes.get(o).ok_or("the Poly has no mesh yet")?.clone();
        let bvh = self.meshes.bvh(&mesh);
        Ok(move |origin: Vec3, dir: Vec3| cast(&mesh, &bvh, origin, dir))
    }
}

/// Where a clay-space ray first meets `mesh`: the point, and the face
/// normal turned toward the ray.
pub(super) fn cast(mesh: &crate::mesh::Mesh, bvh: &crate::mesh::MeshBvh, origin: Vec3, dir: Vec3) -> Option<(Vec3, Vec3)> {
    let (t, tri) = bvh.hit(mesh, origin, dir).map(|h| (h.t, h.tri))?;
    let p = |i: u32| Vec3::from(mesh.vertices[i as usize].pos);
    let ix = &mesh.indices[tri * 3..tri * 3 + 3];
    let n = (p(ix[1]) - p(ix[0])).cross(p(ix[2]) - p(ix[0])).normalize_or_zero();
    Some((origin + dir * t, if n.dot(dir) > 0.0 { -n } else { n }))
}

fn round(v: f32) -> f32 {
    (v * 1e4).round() / 1e4
}

fn r3(v: Vec3) -> [f32; 3] {
    [round(v.x), round(v.y), round(v.z)]
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::super::ClideApp;
    use super::super::recorder::tests::{frame, mcp, ok, settle};

    /// Defect 7 of the quality plan: `surface_point` cut `points` to 256
    /// without a word. Up to 1024 rays and points each are answered, more
    /// are refused naming the bound, and a long `at` list pages with
    /// offset / limit to the same answers as one call. (Long replies need
    /// verbose: true past the transport's 8k summary, as for every tool.)
    #[test]
    fn probe_limits_refuse_not_truncate() {
        let Some(rs) = crate::render::test_support::headless() else {
            eprintln!("skipped: no GPU adapter for the editor's render state");
            return;
        };
        let (mut app, tx) = ClideApp::headless(rs);
        let ctx = egui::Context::default();
        let (app, ctx, tx) = (&mut app, &ctx, &tx);
        frame(app, ctx, Vec::new());
        ok(app, ctx, tx, "create_object", json!({ "shape": "Clay", "name": "Ball", "resolution": 48, "blobs": [{ "type": "sphere", "center": [0, 0, 0], "radius": 0.5 }] }));
        settle(app, ctx);
        // Points on a sphere around the ball (golden spiral).
        let around = |n: usize| -> Vec<[f32; 3]> {
            (0..n).map(|i| {
                let y = 1.0 - 2.0 * (i as f32 + 0.5) / n as f32;
                let a = i as f32 * 2.399_963;
                let r = (1.0 - y * y).sqrt();
                [r * a.cos() * 0.6, y * 0.6, r * a.sin() * 0.6]
            }).collect()
        };
        let r = ok(app, ctx, tx, "surface_point", json!({ "target": "Ball", "points": around(300), "verbose": true }));
        assert_eq!(r["results"].as_array().unwrap().len(), 300, "all 300, not 256");
        assert!(r["results"].as_array().unwrap().iter().all(|h| h["hit"] == true));
        let r = ok(app, ctx, tx, "surface_point", json!({ "target": "Ball", "points": around(1024), "verbose": true }));
        assert_eq!(r["results"].as_array().unwrap().len(), 1024);
        for (key, value) in [("points", json!(around(1025))), ("rays", json!(vec![json!({ "origin": [0, 0, 2], "direction": [0, 0, -1] }); 1025]))] {
            let (v, err) = mcp(app, ctx, tx, "surface_point", json!({ "target": "Ball", key: value }));
            assert!(err && v.as_str().is_some_and(|m| m.contains("1025") && m.contains("1024")), "{key}: {v}");
        }
        // A 300-point view grid: one call, or pages of 128.
        let at: Vec<[f32; 2]> = (0..300).map(|i| [(i % 20) as f32 * 0.04 - 0.38, (i / 20) as f32 * 0.05 - 0.37]).collect();
        let whole = ok(app, ctx, tx, "surface_point", json!({ "target": "Ball", "view": "front", "at": at, "verbose": true }));
        assert_eq!((whole["results"].as_array().unwrap().len(), &whole["next_offset"]), (300, &Value::Null));
        let (mut paged, mut offset) = (Vec::new(), Some(0u64));
        while let Some(o) = offset {
            let page = ok(app, ctx, tx, "surface_point", json!({ "target": "Ball", "view": "front", "at": at, "offset": o, "limit": 128, "verbose": true }));
            assert_eq!(page["at_total"], 300);
            paged.extend(page["results"].as_array().unwrap().iter().cloned());
            offset = page["next_offset"].as_u64();
        }
        assert_eq!(Value::from(paged), whole["results"]);
        // at without a view says so, paged or not.
        for page in [json!({}), json!({ "offset": 128 })] {
            let mut a = json!({ "target": "Ball", "at": at });
            a.as_object_mut().unwrap().extend(page.as_object().unwrap().clone());
            let (v, err) = mcp(app, ctx, tx, "surface_point", a);
            assert!(err && v.as_str().is_some_and(|m| m.contains("at needs view")), "{page}: {v}");
        }
        for bad in [json!({ "offset": 300 }), json!({ "limit": 0 }), json!({ "limit": 1025 }), json!({ "offset": -1 })] {
            let mut a = json!({ "target": "Ball", "view": "front", "at": at });
            a.as_object_mut().unwrap().extend(bad.as_object().unwrap().clone());
            let (v, err) = mcp(app, ctx, tx, "surface_point", a);
            assert!(err, "{bad}: {v}");
        }
    }
}
