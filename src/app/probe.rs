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

impl ClideApp {
    pub(super) fn surface_point(&mut self, args: &Value) -> ToolResult {
        self.meshes.sync(&self.scene);
        let target = match args.get("target").filter(|t| !t.is_null()) {
            Some(t) => Some(self.resolve(t)?),
            None => None,
        };
        let mut out = Vec::new();
        // Rays: given directly, or straight in from a view at screen coordinates.
        let mut rays: Vec<(Vec3, Vec3)> = Vec::new();
        for r in args["rays"].as_array().into_iter().flatten() {
            let v = |k: &str| -> Result<Vec3, String> { serde_json::from_value::<[f32; 3]>(r[k].clone()).map(Vec3::from).map_err(|_| format!("each ray needs {k}: [x, y, z]")) };
            rays.push((v("origin")?, v("direction")?.normalize_or_zero()));
        }
        if let Some(view) = args["view"].as_str() {
            let far = self.scene_sphere().1 * 4.0 + 10.0;
            for a in args["at"].as_array().into_iter().flatten() {
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
        if rays.len() > 256 {
            return Err("at most 256 rays per call".into());
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
            for p in points.iter().take(256) {
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
        Ok(super::tools::text(json!({ "results": out })))
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
        for o in &self.scene.objects {
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
