//! The `kitbash` and `detail_pass` tools on a scene and its meshes (the
//! editor runs them at rest; checks and tests call them directly).

use std::collections::HashMap;

use glam::{Mat4, Vec3};
use serde_json::{Value, json};

use super::passes::{self, Ctx, Settings};
use super::place::{self, Placement, frame, on_surface};
use super::surface::{Hit, Surface};
use super::{CATALOG, Kind, Part};
use crate::mesh::MeshLib;
use crate::scene::{Scene, Shape};

/// `kitbash` arguments that are part settings when given at the top level.
const PART_KEYS: [&str; 16] = ["style", "size", "count", "radius", "sag", "clips", "ends", "text", "lit", "glow", "paint", "color", "materials", "detail", "seed", "kind"];

pub fn resolve(scene: &Scene, v: &Value) -> Result<u64, String> {
    let found = match v {
        Value::Number(n) => n.as_u64().and_then(|id| scene.get(id)),
        Value::String(name) => scene.objects.iter().find(|o| &o.name == name),
        _ => return Err("objects are ids or names".into()),
    };
    found.map(|o| o.id).ok_or_else(|| format!("no object {v}"))
}

fn targets(scene: &Scene, args: &Value) -> Result<Vec<u64>, String> {
    let mut out = Vec::new();
    if let Some(t) = args.get("target").filter(|t| !t.is_null()) {
        out.push(resolve(scene, t)?);
    }
    for t in args["targets"].as_array().into_iter().flatten() {
        out.push(resolve(scene, t)?);
    }
    if out.is_empty() {
        return Err("target (or targets) is required: the object(s) to put parts on".into());
    }
    // A kitbash group (or any group) stands for the parts under it: parts
    // can go on other parts (bolts on a plate, a latch on a hatch).
    let mut expanded = Vec::new();
    for &id in &out {
        let o = scene.get(id).expect("resolved");
        if o.shape == Shape::Group {
            let inner: Vec<u64> = scene.descendants(id).into_iter().filter(|&d| scene.get(d).is_some_and(|x| (x.shape.is_mesh() && x.shape != Shape::Fire) || x.shape == Shape::Scatter)).collect();
            if inner.is_empty() {
                return Err(format!("{} holds nothing to put parts on", o.name));
            }
            expanded.extend(inner);
            continue;
        }
        if !(o.shape.is_mesh() || o.shape == Shape::Scatter) || o.shape == Shape::Fire {
            return Err(format!("{} has no surface to put parts on", o.name));
        }
        expanded.push(id);
    }
    let mut out = expanded;
    let mut seen = std::collections::HashSet::new();
    out.retain(|id| seen.insert(*id));
    Ok(out)
}

fn vec3(v: &Value, name: &str) -> Result<Vec3, String> {
    let a: [f32; 3] = serde_json::from_value(v.clone()).map_err(|_| format!("{name} must be [x, y, z]"))?;
    let p = Vec3::from(a);
    if !p.is_finite() {
        return Err(format!("{name} must be finite"));
    }
    Ok(p)
}

fn points(v: &Value, name: &str) -> Result<Vec<Vec3>, String> {
    v.as_array().ok_or_else(|| format!("{name} must be a list of [x, y, z]"))?.iter().map(|p| vec3(p, name)).collect()
}

/// The surface owners: the targets, plus (for mirroring) the asset's
/// other objects near the mirrored targets.
fn surface_for(scene: &Scene, meshes: &MeshLib, ids: &[u64], mirror: Option<f32>) -> Surface {
    let mut all: Vec<u64> = ids.to_vec();
    if let Some(x0) = mirror {
        let eval = scene.evaluate();
        let bounds = |id: u64| -> Option<(Vec3, Vec3)> {
            let o = scene.get(id)?;
            let (lo, hi) = meshes.bounds(o)?;
            let w = eval.get(&id)?.0;
            let mut acc: Option<(Vec3, Vec3)> = None;
            for i in 0..8 {
                let c = Vec3::new(if i & 1 == 0 { lo.x } else { hi.x }, if i & 2 == 0 { lo.y } else { hi.y }, if i & 4 == 0 { lo.z } else { hi.z });
                let p = w.transform_point3(c);
                acc = Some(acc.map_or((p, p), |(a, b)| (a.min(p), b.max(p))));
            }
            acc
        };
        let want: Vec<(Vec3, Vec3)> = ids.iter().filter_map(|&id| bounds(id)).map(|(lo, hi)| {
            let (a, b) = (Vec3::new(2.0 * x0 - hi.x, lo.y, lo.z), Vec3::new(2.0 * x0 - lo.x, hi.y, hi.z));
            let pad = (hi - lo).length() * 0.05;
            (a - Vec3::splat(pad), b + Vec3::splat(pad))
        }).collect();
        for o in &scene.objects {
            if all.contains(&o.id) || o.kitbash.is_some() || !o.shape.is_mesh() || o.shape == Shape::Fire || !eval.get(&o.id).is_some_and(|e| e.1) {
                continue;
            }
            if let Some((lo, hi)) = bounds(o.id)
                && want.iter().any(|(a, b)| lo.cmple(*b).all() && hi.cmpge(*a).all())
            {
                all.push(o.id);
            }
        }
    }
    Surface::new(place::owners(scene, meshes, &all))
}

fn nearest(s: &Surface, p: Vec3, normal: Option<Vec3>) -> Option<Hit> {
    match normal {
        Some(n) => s.snap(p, n, s.diag.max(1e-3) * 0.1),
        None => s.nearest(p),
    }
}

/// A part from `part` (a kind name or settings) and top-level part keys.
fn part_from(args: &Value) -> Result<Part, String> {
    let mut v = match args.get("part") {
        Some(Value::String(k)) => json!({ "kind": k }),
        Some(Value::Object(o)) => Value::Object(o.clone()),
        _ => return Err(format!("part is required: a kind ({}) or {{kind, style, size, ...}}", Kind::all_names())),
    };
    for k in PART_KEYS {
        if let Some(x) = args.get(k).filter(|x| !x.is_null()) && v.get(k).is_none() {
            v[k] = x.clone();
        }
    }
    let p: Part = serde_json::from_value(v).map_err(|e| format!("part: {e} (kinds: {})", Kind::all_names()))?;
    Ok(p)
}

/// Scales an unsized part to the asset (unit = asset / 6.5 m).
fn scaled(mut p: Part, unit: f32) -> Part {
    let info = p.kind.info();
    if p.size.is_none() && !info.real_size && !info.between_points {
        p.size = Some((Vec3::from(info.size) * unit).to_array());
    }
    if p.radius.is_none() && (info.between_points || p.kind == Kind::Seam) {
        p.radius = Some(p.tube_radius() * unit);
    }
    p
}

/// `kitbash`: place, remove, list or catalog.
pub fn kitbash(scene: &mut Scene, meshes: &mut MeshLib, args: &Value) -> Result<Value, String> {
    match args["action"].as_str().unwrap_or("place") {
        "catalog" => Ok(catalog()),
        "list" => Ok(list(scene, meshes)),
        "remove" => {
            let tag = args["group"].as_str().ok_or("group is required (the tag a kitbash call returned)")?;
            let (objects, blobs) = place::remove(scene, tag);
            if objects == 0 && blobs == 0 {
                return Err(format!("no kitbash group {tag}; groups: {:?}", place::groups(scene).keys().collect::<Vec<_>>()));
            }
            Ok(json!({ "removed_group": tag, "objects_removed": objects, "blobs_removed": blobs }))
        }
        "place" => place_parts(scene, meshes, args),
        "span" | "reattach" => span_parts(scene, args),
        other => Err(format!("unknown action {other}; use place, remove, span, list or catalog")),
    }
}

fn catalog() -> Value {
    json!({
        "parts": CATALOG.iter().map(|i| json!({ "kind": i.name, "styles": i.styles, "size": i.size, "between_points": i.between_points, "real_size": i.real_size, "about": i.about })).collect::<Vec<_>>(),
        "sizes": "meters [along X, out of the surface, across], at a 6.5 m asset; unsized parts scale with the asset they go on",
        "roles": super::geo::Role::ALL.map(|r| r.name()),
    })
}

fn list(scene: &Scene, meshes: &MeshLib) -> Value {
    let eval = scene.evaluate();
    let draws = crate::envkit::scatter::draws(scene, &eval);
    let tri = |o: &crate::scene::Object| meshes.get(o).map_or(0, |m| m.indices.len() / 3);
    let mut out = serde_json::Map::new();
    for (tag, (objects, copies)) in place::groups(scene) {
        let ids: std::collections::HashSet<u64> = scene.objects.iter().filter(|o| o.kitbash.as_deref() == Some(tag.as_str())).map(|o| o.id).collect();
        let mut drawn = 0;
        for d in &draws {
            let mine = if d.scattered { scene.objects.iter().any(|s| ids.contains(&s.id) && s.scatter.as_ref().is_some_and(|sc| sc.sources.contains(&d.object.id))) } else { ids.contains(&d.object.id) };
            if mine {
                drawn += tri(d.object);
            }
        }
        out.insert(tag, json!({ "objects": objects, "copies": copies, "triangles_drawn": drawn }));
    }
    let sources: usize = scene.objects.iter().filter(|o| o.kitbash.as_deref() == Some(place::LIBRARY_TAG) && o.shape == Shape::Part).map(tri).sum();
    json!({ "groups": out, "library_unique_triangles": sources })
}

fn place_parts(scene: &mut Scene, meshes: &mut MeshLib, args: &Value) -> Result<Value, String> {
    let mut ids = targets(scene, args)?;
    let linked_skipped: Vec<_> = ids.iter().copied().filter(|id|scene.get(*id).is_some_and(|o|o.link.is_some())).collect();
    ids.retain(|id|!linked_skipped.contains(id));
    if ids.is_empty() {return Err("linked mirror targets inherit detail from their source; target the source or unlink first".into());}
    meshes.sync(scene);
    let (asset, unit) = place::asset_scale(scene, meshes, ids[0]);
    let mut part = scaled(part_from(args)?, unit);
    part.sanitize();
    part.validate().or_else(|e| if part.kind.info().between_points && part.points.len() < 2 { Ok(()) } else { Err(e) })?;
    let mirror = args["mirror"].as_bool().unwrap_or(false);
    let x0 = args["mirror_x"].as_f64().unwrap_or(0.0) as f32;
    let s = surface_for(scene, meshes, &ids, mirror.then_some(x0));
    if s.is_empty() {
        return Err("the targets have no surface".into());
    }
    let normal = args.get("normal").filter(|v| !v.is_null()).map(|v| vec3(v, "normal")).transpose()?;
    let rotate = args["rotate"].as_f64().unwrap_or(0.0) as f32;
    let lift = args["offset"].as_f64().unwrap_or(0.0) as f32;
    let mut list: Vec<Placement> = Vec::new();
    let mut misses = 0;
    let mut slid = 0.0f32;
    let mut reasons: std::collections::BTreeMap<&str, usize> = std::collections::BTreeMap::new();
    let mut notes = serde_json::Map::new();
    if part.kind.info().between_points {
        // Between points: from/to (and via) or points, ends snapped onto
        // the surface (their connectors and eyes sit on it).
        let mut pts = Vec::new();
        if let Some(f) = args.get("from").filter(|v| !v.is_null()) {
            pts.push(vec3(f, "from")?);
            pts.extend(args.get("via").map(|v| points(v, "via")).transpose()?.unwrap_or_default());
            pts.push(vec3(args.get("to").ok_or("to is required with from")?, "to")?);
        } else if let Some(p) = args.get("points").filter(|v| !v.is_null()) {
            pts = points(p, "points")?;
        } else if !part.points.is_empty() {
            pts = part.points.iter().map(|p| Vec3::from(*p)).collect();
        }
        if pts.len() < 2 {
            return Err(format!("{} runs between points: give from and to (world), or points", part.kind.name()));
        }
        let snap_ends = args["snap"].as_bool().unwrap_or(true);
        let (mut first_hit, mut last_hit) = (None, None);
        if snap_ends {
            let n = pts.len();
            for (i, slot) in [(0usize, &mut first_hit), (n - 1, &mut last_hit)] {
                if let Some(h) = nearest(&s, pts[i], normal) {
                    pts[i] = h.p;
                    *slot = Some(h);
                }
            }
        }
        let last_hit = last_hit.or_else(||nearest(&s,pts[pts.len()-1],None));
        let rigid = matches!(part.kind,Kind::Piston|Kind::Pipe);
        let clearance = part.tube_radius()*1.5 + 0.001;
        if !rigid && args["route"].as_bool().unwrap_or(true) {
            let sag = part.sag.unwrap_or(0.06);
            let (routed, contacts) = routed_points(&s, &pts, sag, clearance);
            pts = routed;
            part.sag = Some(0.0);
            if crossing_count(&s,&pts,0.0)>0 {notes.insert("warning".into(),json!("some routed segments still intersect a target; add via points outside the obstruction"));}
            notes.insert("route".into(),json!({"surface_contacts":contacts,"points":pts.len(),"sag":sag,"clearance":clearance}));
        } else if rigid {
            let hits = crossing_count(&s,&pts,clearance);
            if hits>0 {notes.insert("warning".into(),json!("straight rigid span intersects a target surface; offset the mounts using snap:false, or use a routed hose"));}
        }
        part.points = pts.iter().map(|p| p.to_array()).collect();
        place::check_part(&part)?;
        let h = first_hit.or_else(|| nearest(&s, pts[0], None)).ok_or("no surface near the first point")?;
        let owner = s.owner_of(h.tri).id;
        let first_bone = end_bone(scene, &s, &h);
        // Two-bone parts: ends on different bones of one rig (chest ->
        // hand) ride their own bones and are solved again in every pose.
        let span = match args.get("bones").filter(|v| !v.is_null()) {
            Some(v) => {
                let b: [String; 2] = serde_json::from_value(v.clone()).map_err(|_| "bones must be two bone names [first end, last end]")?;
                Some(b)
            }
            None if args["two_bone"].as_bool().unwrap_or(true) => {
                let l = last_hit.or_else(|| nearest(&s, pts[pts.len() - 1], None));
                match (&first_bone, l.and_then(|l| end_bone(scene, &s, &l))) {
                    (Some((ra, a)), Some((rb, b))) if ra == &rb && *a != b => Some([a.clone(), b]),
                    _ => None,
                }
            }
            None => None,
        };
        if let Some(bones) = span {
            if !super::span::can_span(part.kind) {
                return Err(format!("{} cannot run between two bones; use piston, cable, hose, corrugated_tube, pipe or strap", part.kind.name()));
            }
            notes.insert("two_bone".into(), json!(bones));
            part.span = Some(super::Span { bones, rest_length: None, half: None });
        }
        let attach_bone = match (&part.span, s.bone_at(&h)) {
            (Some(sp), Some(_)) => Some(sp.bones[0].clone()),
            (_, b) => b,
        };
        list.push(Placement { bone: attach_bone, part: part.clone(), frame: Mat4::IDENTITY, owner, sink: 0.0, priority: 2 });
    } else {
        // Where: at (one point), points, a path with spacing, an array, a
        // grid, or along the nearest hard edge.
        let mut spots: Vec<Vec3> = Vec::new();
        if let Some(a) = args.get("at").filter(|v| !v.is_null()) {
            spots.push(vec3(a, "at")?);
        }
        if let Some(p) = args.get("points").filter(|v| !v.is_null()) {
            spots.extend(points(p, "points")?);
        }
        let mut path_line: Vec<Vec3> = Vec::new();
        if let Some(p) = args.get("path").filter(|v| !v.is_null()) {
            let path = points(p, "path")?;
            let spacing = args["spacing"].as_f64().map(|v| v as f32).unwrap_or(part.extent() * 4.0).max(1e-4);
            spots.extend(along_path(&path, spacing));
            path_line = path;
        }
        let orient_path = match args["orient"].as_str() {
            None | Some("surface") => false,
            Some("path") if path_line.len() >= 2 => true,
            Some("path") => return Err("orient path needs a path".into()),
            Some(o) => return Err(format!("unknown orient {o}; use surface or path")),
        };
        if spots.is_empty() {
            return Err("give at, points or path (world points on or near the target)".into());
        }
        spots.truncate(4096);
        let first = nearest(&s, spots[0], normal).ok_or("no surface near the point")?;
        if let Some(arr) = args.get("array").filter(|v| v.is_object()) {
            let count = arr["count"].as_u64().unwrap_or(2).clamp(1, 512) as usize;
            if let Some(step) = arr.get("step").filter(|v| !v.is_null()) {
                let step = vec3(step, "array.step")?;
                spots = (0..count).map(|i| spots[0] + step * i as f32).collect();
            } else if let Some(to) = arr.get("to").filter(|v| !v.is_null()) {
                let to = vec3(to, "array.to")?;
                spots = (0..count).map(|i| spots[0].lerp(to, if count == 1 { 0.0 } else { i as f32 / (count - 1) as f32 })).collect();
            } else if let Some(grid) = arr.get("grid").filter(|v| !v.is_null()) {
                let g: [u32; 2] = serde_json::from_value(grid.clone()).map_err(|_| "array.grid must be [columns, rows]")?;
                let sp: [f32; 2] = arr.get("spacing").map(|v| serde_json::from_value(v.clone())).transpose().map_err(|_| "array.spacing must be [along, across]")?.unwrap_or([part.extent() * 2.0; 2]);
                let along = args.get("along").and_then(|v| vec3(v, "along").ok()).unwrap_or_else(|| place::default_along(first.n));
                let fr = frame(first.p, first.n, along, 1.0);
                let (cx, cz) = (g[0].clamp(1, 64) as usize, g[1].clamp(1, 64) as usize);
                spots = (0..cz).flat_map(|j| (0..cx).map(move |i| (i, j))).map(|(i, j)| {
                    let x = (i as f32 - (cx as f32 - 1.0) * 0.5) * sp[0];
                    let z = (j as f32 - (cz as f32 - 1.0) * 0.5) * sp[1];
                    fr.transform_point3(Vec3::new(x, 0.0, z))
                }).collect();
            } else {
                return Err("array needs step, to, or grid (with spacing)".into());
            }
        }
        let edge = args["along"].as_str() == Some("edge") || args["edge"].as_bool().unwrap_or(false);
        let mut edge_dir = None;
        if edge {
            let owner_index = s.tri_owner[first.tri as usize].0;
            let an = s.analyze(&[owner_index], 4.0, 25.0);
            let best = an.edges.iter().min_by(|a, b| seg_dist(first.p, a.a, a.b).total_cmp(&seg_dist(first.p, b.a, b.b)));
            if let Some(e) = best {
                let dir = (e.b - e.a).normalize_or(Vec3::X);
                edge_dir = Some(dir);
                if let Some(count) = args["count_along"].as_u64().or(args["edge_count"].as_u64()) {
                    // Spread copies along the edge, set in from it as the first point is.
                    let t0 = (first.p - e.a).dot(dir);
                    let off = first.p - (e.a + dir * t0);
                    let len = e.a.distance(e.b);
                    let n = count.clamp(1, 512) as usize;
                    let margin = (len * 0.08).min(part.extent() * 2.0);
                    spots = (0..n).map(|i| e.a + dir * (margin + (len - 2.0 * margin) * if n == 1 { 0.5 } else { i as f32 / (n - 1) as f32 }) + off).collect();
                }
            }
        }
        for p in spots {
            let Some(h) = nearest(&s, p, normal.or(Some(first.n))).filter(|h| h.p.distance(p) < s.diag * 0.25) else {
                misses += 1;
                *reasons.entry("no surface near the point").or_insert(0usize) += 1;
                continue;
            };
            let along = match (edge_dir, args.get("along")) {
                (Some(d), _) => d,
                _ if orient_path => path_tangent(&path_line, p),
                (None, Some(v)) if v.is_array() => vec3(v, "along")?,
                _ => place::default_along(h.n),
            };
            let along = glam::Quat::from_axis_angle(h.n, rotate.to_radians()) * along;
            let max_sink = args["max_sink"].as_f64().unwrap_or(0.5) as f32;
            // Overhanging an edge or a curve: slide inward to the nearest
            // spot within a footprint radius where it fits.
            let mut placed = on_surface(&s, &h, part.clone(), along, max_sink);
            if placed.is_err() && args["slide"].as_bool().unwrap_or(true) {
                let r = part.footprint().map_or(0.0, |(hx, hz, _)| hx.hypot(hz));
                let (tu, tv) = super::geo::plane_axes(h.n);
                'slide: for k in 1..=4 {
                    let d = r * k as f32 * 0.25;
                    for i in 0..12 {
                        let a = i as f32 / 12.0 * std::f32::consts::TAU;
                        let q = h.p + (tu * a.cos() + tv * a.sin()) * d;
                        let Some(h2) = s.snap(q, h.n, r + 1e-3).filter(|h2| h2.n.dot(h.n) > 0.95) else { continue };
                        if let Ok(pl) = on_surface(&s, &h2, part.clone(), along, max_sink) {
                            slid = slid.max(d);
                            placed = Ok(pl);
                            break 'slide;
                        }
                    }
                }
            }
            match placed {
                Ok(mut pl) => {
                    if args.get("paint").is_none() && part.paint.is_none() {
                        let (m, c) = s.paint_at(&h);
                        pl.part.paint = Some(m);
                        pl.part.color = Some(c.map(|x| (x * 20.0).round() / 20.0));
                    }
                    if lift != 0.0 {
                        pl.frame = Mat4::from_translation(h.n * lift) * pl.frame;
                    }
                    list.push(pl);
                }
                Err(why) => {
                    misses += 1;
                    *reasons.entry(why).or_insert(0usize) += 1;
                }
            }
        }
    }
    let mut mirrored_n = 0;
    if mirror {
        let eligible: Vec<_> = list.iter().filter(|p|!linked_source(scene,p.owner)).cloned().collect();
        let (copies, missed) = place::mirrored(&eligible, &s, x0, false);
        mirrored_n = copies.len();
        misses += missed;
        list.extend(copies);
    }
    if list.is_empty() {
        return Err(format!("nothing placed ({misses} spots): {reasons:?}"));
    }
    let tag = match args["group"].as_str() {
        Some(t) if !t.is_empty() => t.to_string(),
        _ => place::new_tag(scene, &format!("kb_{}", part.kind.name())),
    };
    let mut cache = HashMap::new();
    let (drawn, unique) = place::triangles(&list, &scene.materials, &mut cache);
    let sinks: Vec<f32> = list.iter().map(|p| p.sink).collect();
    let written = place::write(scene, &tag, &list);
    notes.insert("max_sink".into(), json!(sinks.iter().copied().fold(0.0, f32::max)));
    let mut out = report(scene, &tag, &written, list.len(), drawn, unique, asset, unit);
    out["linked_mirror_targets_skipped"] = json!(linked_skipped);
    out["mirror_policy"] = json!("linked copies inherit parts automatically; explicit mirroring skips their sources");
    out["mirrored"] = json!(mirrored_n);
    out["missed"] = json!(misses);
    if slid > 0.0 {
        out["slid"] = json!((slid * 1000.0).round() / 1000.0);
    }
    out["part"] = serde_json::to_value(&list[0].part).unwrap_or_default();
    for (k, v) in notes {
        out[k] = v;
    }
    Ok(out)
}

/// The rig and bone that carry a surface point: the bone moving it on
/// rigged clay, or the bone the hit object rides.
fn end_bone(scene: &Scene, s: &Surface, h: &Hit) -> Option<(u64, String)> {
    let o = scene.get(s.owner_of(h.tri).id)?;
    if o.rig.as_ref().is_some_and(|r| !r.bones.is_empty()) {
        return Some((o.id, s.bone_at(h)?));
    }
    let mut cur = Some(o);
    while let Some(x) = cur {
        if let (Some(b), Some(p)) = (&x.bone, x.parent)
            && scene.get(p).and_then(|p| p.rig.as_ref()).is_some_and(|r| r.bone_index(b).is_some())
        {
            return Some((p, b.clone()));
        }
        cur = x.parent.and_then(|p| scene.get(p));
    }
    None
}

/// `kitbash action: "span"`: makes parts between points (a group's, or
/// one object) two-bone parts: `bones` [first end, last end], or for each
/// end the nearest bone of the rig the part hangs on.
fn span_parts(scene: &mut Scene, args: &Value) -> Result<Value, String> {
    let mut ids: Vec<u64> = Vec::new();
    if let Some(tag) = args["group"].as_str() {
        ids.extend(scene.objects.iter().filter(|o| o.kitbash.as_deref() == Some(tag) && o.part.is_some()).map(|o| o.id));
    }
    if let Some(t) = args.get("target").filter(|t| !t.is_null()) {
        let id = resolve(scene, t)?;
        ids.push(id);
        ids.extend(scene.descendants(id));
    }
    ids.retain(|&id| scene.get(id).is_some_and(|o| o.part.as_ref().is_some_and(|p| p.kind.info().between_points)));
    if ids.is_empty() {
        return Err("give group (a kitbash tag) or target holding parts between points (piston, cable, hose...)".into());
    }
    let given: Option<[String; 2]> = args.get("bones").filter(|v| !v.is_null()).map(|v| serde_json::from_value(v.clone()).map_err(|_| "bones must be two bone names [first end, last end]")).transpose()?;
    let off = args["two_bone"].as_bool() == Some(false);
    let mut done = Vec::new();
    for id in ids {
        let o = scene.get(id).expect("listed");
        let mut part = o.part.clone().expect("listed");
        let world = scene.world_matrix(id);
        let from=args.get("from").filter(|v|!v.is_null()).map(|v|vec3(v,"from")).transpose()?;
        let to=args.get("to").filter(|v|!v.is_null()).map(|v|vec3(v,"to")).transpose()?;
        if part.points.len()<2 {continue;}
        if from.is_some() || to.is_some() {
            let old:Vec<_>=part.points.iter().map(|p|world.transform_point3(Vec3::from(*p))).collect();
            let a=from.unwrap_or(old[0])-old[0];let b=to.unwrap_or(*old.last().expect("points"))-*old.last().expect("points");
            let weights=super::span::blend_weights(&old);
            part.points=old.iter().zip(weights).map(|(p,w)|world.inverse().transform_point3(*p+a.lerp(b,w)).to_array()).collect();
        }
        if !super::span::can_span(part.kind) {
            continue;
        }
        let h = super::span::hang(scene, o).ok_or_else(|| format!("{} does not hang on a rig", o.name))?;
        let rig = scene.get(h.rig).and_then(|r| r.rig.as_ref()).expect("hang found it");
        let bones = if off {
            None
        } else if let Some(b) = &given {
            for n in b {
                if rig.bone_index(n).is_none() {
                    return Err(format!("the rig has no bone {n}"));
                }
            }
            Some(b.clone())
        } else {
            let end = |i: usize| h.to_rig.transform_point3(Vec3::from(part.points[i]));
            let a = super::span::nearest_bone(rig, end(0)).ok_or("the rig has no deform bones")?;
            let b = super::span::nearest_bone(rig, end(part.points.len() - 1)).ok_or("the rig has no deform bones")?;
            Some([a, b])
        };
        let name = o.name.clone();
        let p = scene.get_mut(id).expect("listed").part.as_mut().expect("listed");
        p.points = part.points;
        p.span = bones.clone().map(|bones| super::Span { bones, rest_length: None, half: None });
        done.push(json!({ "object": name, "bones": bones }));
    }
    Ok(json!({ "parts": done }))
}

fn seg_dist(p: Vec3, a: Vec3, b: Vec3) -> f32 {
    let d = b - a;
    let t = ((p - a).dot(d) / d.length_squared().max(1e-12)).clamp(0.0, 1.0);
    p.distance(a + d * t)
}

/// The direction of the polyline segment nearest `p`.
fn path_tangent(path: &[Vec3], p: Vec3) -> Vec3 {
    path.windows(2).min_by(|a, b| seg_dist(p, a[0], a[1]).total_cmp(&seg_dist(p, b[0], b[1]))).map_or(Vec3::X, |w| (w[1] - w[0]).normalize_or(Vec3::X))
}

/// Points every `spacing` along a polyline.
fn along_path(path: &[Vec3], spacing: f32) -> Vec<Vec3> {
    if path.len() < 2 {
        return path.to_vec();
    }
    let total: f32 = path.windows(2).map(|w| w[0].distance(w[1])).sum();
    let n = ((total / spacing).floor() as usize).clamp(0, 2000);
    let mut out = Vec::with_capacity(n + 1);
    for i in 0..=n {
        let mut d = i as f32 * spacing + (total - n as f32 * spacing) * 0.5;
        for w in path.windows(2) {
            let l = w[0].distance(w[1]);
            if d <= l || std::ptr::eq(w, path.windows(2).last().expect("two points")) {
                out.push(w[0].lerp(w[1], (d / l.max(1e-9)).clamp(0.0, 1.0)));
                break;
            }
            d -= l;
        }
    }
    out
}

#[allow(clippy::too_many_arguments)]
fn report(scene: &Scene, tag: &str, w: &place::Written, placed: usize, drawn: usize, unique: usize, asset: f32, unit: f32) -> Value {
    let name = |id: u64| scene.get(id).map(|o| o.name.clone());
    json!({
        "group": tag,
        "placed": placed,
        "copies": w.copies,
        "objects": w.objects.iter().filter_map(|&i| name(i)).collect::<Vec<_>>(),
        "sources": w.sources.iter().filter_map(|&i| name(i)).collect::<Vec<_>>(),
        "attached": w.attached.iter().map(|(g, p, b)| json!({ "group": g, "parent": p, "bone": b })).collect::<Vec<_>>(),
        "triangles_drawn": drawn,
        "unique_triangles": unique,
        "asset_size": (asset * 1000.0).round() / 1000.0,
        "unit": (unit * 10000.0).round() / 10000.0,
        "remove_with": { "action": "remove", "group": tag },
    })
}

/// `detail_pass`: one auto-detail pass over the targets.
pub fn detail_pass(scene: &mut Scene, meshes: &mut MeshLib, args: &Value) -> Result<Value, String> {
    let mut ids = targets(scene, args)?;
    let linked_skipped: Vec<_> = ids.iter().copied().filter(|id|scene.get(*id).is_some_and(|o|o.link.is_some())).collect();
    ids.retain(|id|!linked_skipped.contains(id));
    if ids.is_empty() {return Err("linked mirror targets inherit detail from their source; target the source or unlink first".into());}
    let mut settings_v = args.clone();
    if let Some(o) = settings_v.as_object_mut() {
        for k in ["target", "targets", "group", "mirror", "mirror_x", "budget", "verbose", "detail"] {
            o.remove(k);
        }
    }
    let mut settings: Settings = serde_json::from_value(settings_v).map_err(|e| format!("detail_pass: {e}"))?;
    if !(settings.scale.is_finite() && settings.scale > 0.0) {
        return Err("scale must be positive".into());
    }
    meshes.sync(scene);
    settings.sanitize(place::asset_scale(scene, meshes, ids[0]).1)?;
    let keep_frame = scene.frame.take();
    let mirror = args["mirror"].as_bool().unwrap_or(false);
    let x0 = args["mirror_x"].as_f64().unwrap_or(0.0) as f32;
    let s = surface_for(scene, meshes, &ids, mirror.then_some(x0));
    if s.is_empty() {
        scene.frame = keep_frame;
        return Err("the targets have no surface".into());
    }
    let (asset, unit) = place::asset_scale(scene, meshes, ids[0]);
    let target_owners: Vec<u32> = s.owners.iter().enumerate().filter(|(_, o)| ids.contains(&o.id)).map(|(i, _)| i as u32).collect();
    let started = std::time::Instant::now();
    let analysis = s.analyze(&target_owners, 4.0, 25.0);
    // Parts already placed stay clear, except the ones being detailed.
    let mut keep_clear = place::existing_parts(scene, &ids);
    for o in &s.owners {
        keep_clear.extend(o.keep_clear.iter().copied());
    }
    // The rig the targets belong to, and the bone each rides.
    let eval = scene.evaluate();
    let mut rig = None;
    let mut owner_bone = HashMap::new();
    for o in &s.owners {
        let Some(obj) = scene.get(o.id) else { continue };
        if let Some(r) = obj.rig.as_ref().filter(|r| !r.bones.is_empty()) {
            rig.get_or_insert((r.bones.clone(), eval.get(&obj.id).map_or(Mat4::IDENTITY, |e| e.0)));
        }
        if let (Some(b), Some(p)) = (&obj.bone, obj.parent)
            && let Some(r) = scene.get(p).and_then(|p| p.rig.as_ref())
        {
            owner_bone.insert(o.id, b.clone());
            rig.get_or_insert((r.bones.clone(), eval.get(&p).map_or(Mat4::IDENTITY, |e| e.0)));
        }
    }
    let ctx = Ctx { surface: &s, analysis: &analysis, targets: target_owners, unit, keep_clear, rig, owner_bone, mirror_x: x0 };
    let mut out = passes::run(&ctx, &settings);
    let analysis_s = started.elapsed().as_secs_f32();
    let tag = match args["group"].as_str() {
        Some(t) if !t.is_empty() => t.to_string(),
        _ => place::new_tag(scene, &format!("kb_{:?}", settings.pass).to_lowercase()),
    };
    let mut list = std::mem::take(&mut out.placements);
    let mut mirrored_n = 0;
    let mut mirror_missed = 0;
    if mirror {
        list.retain(|p| {
            let x = if p.part.points.is_empty() { p.origin().x } else { p.part.points[0][0] };
            linked_source(scene,p.owner) || x >= x0 - p.radius() * 0.5
        });
        let eligible: Vec<_> = list.iter().filter(|p|!linked_source(scene,p.owner)).cloned().collect();
        let (copies, missed) = place::mirrored(&eligible, &s, x0, true);
        mirrored_n = copies.len();
        mirror_missed = missed;
        list.extend(copies);
    }
    let mut dropped = 0;
    if let Some(b) = args["budget"].as_u64() {
        dropped = place::fit_budget(&mut list, b as usize, &scene.materials, settings.seed);
    }
    // Engraved seams go into the clay targets themselves.
    let mut blobs = 0;
    let mut engrave_skipped = 0;
    for (i, (owner, pts, n, width, depth)) in out.engraves.iter().enumerate() {
        let is_clay = scene.get(*owner).is_some_and(|o| o.shape == Shape::Clay);
        let to_local = eval.get(owner).map_or(Mat4::IDENTITY, |e| e.0).inverse();
        let blob = is_clay.then(|| passes::band_blob(&format!("{tag}/seam{i}"), pts, *n, *width, *depth, to_local)).flatten();
        match (blob, scene.get_mut(*owner).and_then(|o| o.clay.as_mut())) {
            (Some(b), Some(c)) if c.blobs.len() < crate::clay::MAX_BLOBS => {
                c.blobs.push(b);
                blobs += 1;
            }
            _ => engrave_skipped += 1,
        }
    }
    let mut cache = HashMap::new();
    let (drawn, unique) = place::triangles(&list, &scene.materials, &mut cache);
    let written = place::write(scene, &tag, &list);
    scene.frame = keep_frame;
    let mut rep = report(scene, &tag, &written, list.len(), drawn, unique, asset, unit);
    rep["pass"] = json!(format!("{:?}", settings.pass).to_lowercase());
    rep["linked_mirror_targets_skipped"] = json!(linked_skipped);
    rep["mirror_policy"] = json!("linked copies inherit parts automatically; explicit mirroring skips their sources");
    rep["mirrored"] = json!(mirrored_n);
    if mirror_missed > 0 {
        rep["mirror_missed"] = json!(mirror_missed);
    }
    if dropped > 0 {
        rep["dropped_for_budget"] = json!(dropped);
    }
    if blobs > 0 || engrave_skipped > 0 {
        rep["engraved_seams"] = json!(blobs);
        rep["engrave_skipped"] = json!(engrave_skipped);
    }
    rep["flat_regions"] = json!(analysis.regions.iter().filter(|r| r.area > (0.1 * unit).powi(2)).count());
    rep["hard_edges"] = json!(analysis.edges.len());
    rep["analysis_seconds"] = json!((analysis_s * 10.0).round() / 10.0);
    for (k, v) in out.notes {
        rep[k] = v;
    }
    Ok(rep)
}

fn linked_source(scene:&Scene,id:u64)->bool {
    scene.objects.iter().any(|o|o.link.as_ref().is_some_and(|l|l.source==id))
}

fn crossing_count(surface:&Surface,points:&[Vec3],clearance:f32)->usize {
    points.windows(2).flat_map(|p|(1..16).map(move |i|p[0].lerp(p[1],i as f32/16.0))).filter(|p|surface.nearest(*p).is_some_and(|h|(*p-h.p).dot(h.n)<clearance)).count()
}

/// Sag independently between authored clips, project samples outside the
/// surface, then relax tangents while retaining the authored endpoints.
fn routed_points(surface:&Surface,points:&[Vec3],sag:f32,clearance:f32)->(Vec<Vec3>,usize) {
    let mut result=Vec::new();let mut contacts=0;
    for segment in points.windows(2) {
        let n=((segment[0].distance(segment[1])/clearance.max(0.005)).ceil() as usize).clamp(1,(63/(points.len()-1)).clamp(1,32));
        let start=result.len();
        for i in 0..=n {
            let t=i as f32/n as f32;
            let p=segment[0].lerp(segment[1],t)-Vec3::Y*(sag*segment[0].distance(segment[1])*4.0*t*(1.0-t));
            result.push(p);
        }
        if crossing_count(surface,&result[start..],0.0)>0 && n>=6 {
            let a=surface.nearest(segment[0]).map_or(segment[0],|h|if h.p.distance(segment[0])<clearance*4.0{segment[0]+h.n*clearance*3.0}else{segment[0]});
            let b=surface.nearest(segment[1]).map_or(segment[1],|h|if h.p.distance(segment[1])<clearance*4.0{segment[1]+h.n*clearance*3.0}else{segment[1]});
            let mut best=None;
            for direction in [Vec3::NEG_Y,Vec3::Y,Vec3::Z,Vec3::NEG_Z,Vec3::X,Vec3::NEG_X] {
                for factor in [0.35,0.7,1.4,2.8] {
                    let detour=direction*(a.distance(b)*factor+clearance*4.0);
                    let controls=[segment[0],a,a+detour,b+detour,b,segment[1]];
                    let candidate=sample_polyline(&controls,n);
                    let crossed=crossing_count(surface,&candidate,0.0);
                    let length:f32=candidate.windows(2).map(|w|w[0].distance(w[1])).sum();
                    if best.as_ref().is_none_or(|(hits,len,_)|crossed<*hits || (crossed==*hits && length<*len)) {best=Some((crossed,length,candidate));}
                }
            }
            if let Some((_,_,candidate))=best {result.truncate(start);result.extend(candidate);}
        }
        for _ in 0..6 {
            for i in start+1..result.len()-1 {
                if let Some(h)=surface.nearest(result[i]) {
                    let distance=(result[i]-h.p).dot(h.n);
                    if distance<clearance {result[i]=h.p+h.n*clearance;contacts+=1;}
                }
            }
        }
    }
    result.dedup_by(|a,b|a.distance_squared(*b)<1e-12);
    (result,contacts)
}

#[cfg(test)]
mod speed_route_tests {
    use super::*;
    #[test]
    fn hose_routes_preserve_both_ends_and_sag_between_clips() {
        let mut scene=Scene::default();let id=scene.add(Shape::Cube);
        scene.get_mut(id).unwrap().translation=Vec3::new(0.0,-2.0,0.0);
        let mut meshes=MeshLib::new();meshes.sync(&scene);
        let surface=surface_for(&scene,&meshes,&[id],None);
        let points=[Vec3::new(-1.0,0.0,0.0),Vec3::new(0.0,0.0,0.0),Vec3::new(1.0,0.0,0.0)];
        let (route,_)=routed_points(&surface,&points,0.15,0.02);
        assert!(route.len()<=64);assert_eq!(route[0],points[0]);assert_eq!(*route.last().unwrap(),points[2]);
        assert!(route.iter().any(|p|p.y < -0.1));assert!(route.iter().any(|p|p.distance(points[1])<1e-6));
    }
    #[test]
    fn rigid_piston_reports_a_segment_through_the_target() {
        let mut scene=Scene::default();let id=scene.add(Shape::Cube);
        let mut meshes=MeshLib::new();meshes.sync(&scene);
        let response=kitbash(&mut scene,&mut meshes,&json!({"part":"piston","target":id,"from":[-0.5,0.5,0],"to":[0.5,0.5,0],"snap":false,"radius":0.02})).unwrap();
        assert!(response["warning"].as_str().is_some_and(|w|w.contains("intersects")),"{response}");
    }
}

fn sample_polyline(points:&[Vec3],segments:usize)->Vec<Vec3> {
    let lengths:Vec<_>=points.windows(2).map(|w|w[0].distance(w[1])).collect();
    let total:f32=lengths.iter().sum();
    let extra=segments.saturating_sub(lengths.len());
    let mut result=vec![points[0]];
    for (i,length) in lengths.iter().enumerate() {
        let count=1+(extra as f32*length/total.max(1e-9)).floor() as usize;
        for j in 1..=count {result.push(if j==count {points[i+1]} else {points[i].lerp(points[i+1],j as f32/count as f32)});}
    }
    result
}

#[cfg(test)]
mod speed_reattach_tests {
    use super::*;
    #[test]
    fn reattach_moves_both_span_ends_without_replacing_settings() {
        let mut scene=Scene::default();let rig=scene.add(Shape::Clay);
        scene.get_mut(rig).unwrap().rig=Some(serde_json::from_value(json!({"split":false,"bones":[{"name":"chest","head":[0,0,0],"tail":[0,1,0]},{"name":"hand","parent":"chest","head":[1,0,0],"tail":[1,1,0]}]})).unwrap());
        let id=scene.add(Shape::Part);let mut part=Part::new(Kind::Hose);part.points=vec![[0.0,0.5,0.0],[0.5,0.4,0.2],[1.0,0.5,0.0]];part.radius=Some(0.025);
        let o=scene.get_mut(id).unwrap();o.parent=Some(rig);o.bone=Some("chest".into());o.part=Some(part);
        let mut meshes=MeshLib::new();
        kitbash(&mut scene,&mut meshes,&json!({"action":"reattach","target":id,"from":[0,0.7,0.1],"to":[1,0.8,0.3],"bones":["chest","hand"]})).unwrap();
        let p=scene.get(id).unwrap().part.as_ref().unwrap();let w=scene.world_matrix(id);
        assert!(w.transform_point3(Vec3::from(p.points[0])).distance(Vec3::new(0.0,0.7,0.1))<1e-5);
        assert!(w.transform_point3(Vec3::from(*p.points.last().unwrap())).distance(Vec3::new(1.0,0.8,0.3))<1e-5);
        assert_eq!(p.radius,Some(0.025));assert_eq!(p.span.as_ref().unwrap().bones,["chest","hand"]);
    }
}

#[cfg(test)]
mod speed_obstacle_tests {
    use super::*;
    #[test]
    fn hose_routes_around_a_solid_instead_of_through_it() {
        let mut scene=Scene::default();let id=scene.add(Shape::Cube);
        let mut meshes=MeshLib::new();meshes.sync(&scene);
        let surface=surface_for(&scene,&meshes,&[id],None);
        let ends=[Vec3::new(-0.5,0.5,0.0),Vec3::new(0.5,0.5,0.0)];
        let (route,_)=routed_points(&surface,&ends,0.1,0.025);
        assert_eq!(route[0],ends[0]);assert_eq!(*route.last().unwrap(),ends[1]);assert!(route.len()<=64);
        assert_eq!(crossing_count(&surface,&route,0.0),0,"route intersects cube: {route:?}");
    }
}
