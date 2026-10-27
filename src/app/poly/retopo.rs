//! The `retopo` tool (docs/QUALITY_PLAN.md Stage 3b): field-aligned quad
//! retopology of a clay into a new Poly object (`crate::poly::retopo`),
//! run as a job.
//!
//! Every size an agent sends is checked from the arguments before
//! anything is allocated: faces 500-200,000, at most 64 guides of 2-256
//! points, 16 density regions, and the input the faces need (8 points per
//! face, at most 2,000,000). Guides and density regions come in world
//! space at rest and are kept in the clay's own space in `made_by`, so
//! `poly rebuild` runs the same call again on the same clay.

use std::sync::Arc;

use glam::{Mat4, Vec3};
use serde_json::{Value, json};

use super::super::ClideApp;
use super::super::jobs::{Finish, Started};
use super::super::tools::{object_json_in, text};
use crate::poly::retopo::{self, Args, Density, Symmetry};

pub const MIN_FACES: usize = 500;
pub const MAX_FACES: usize = 200_000;
pub const DEFAULT_FACES: usize = 8000;
pub const MAX_GUIDES: usize = 64;
pub const MAX_GUIDE_POINTS: usize = 256;
pub const MAX_DENSITY: usize = 16;

/// A `retopo` call, checked, in the clay's own space.
#[derive(Clone, Debug)]
pub(in crate::app) struct Request {
    pub args: Args,
    /// Fit the fine (detail) surface, not the rest mesh.
    pub fine: bool,
    pub name: Option<String>,
    pub hide: bool,
    pub images: bool,
}

fn round(x: f32) -> f64 {
    (x as f64 * 1e4).round() / 1e4
}

/// A number argument in `lo..=hi` (absent: `default`).
fn number(args: &Value, key: &str, lo: f32, hi: f32, default: f32) -> Result<f32, String> {
    match args.get(key).filter(|v| !v.is_null()) {
        None => Ok(default),
        Some(v) => v.as_f64().map(|x| x as f32).filter(|x| x.is_finite() && (lo..=hi).contains(x)).ok_or_else(|| format!("{key} must be a number from {lo} to {hi}")),
    }
}

fn boolean(args: &Value, key: &str, default: bool) -> Result<bool, String> {
    match args.get(key).filter(|v| !v.is_null()) {
        None => Ok(default),
        Some(v) => v.as_bool().ok_or_else(|| format!("{key} must be true or false")),
    }
}

/// A point [x, y, z], finite and within the coordinate bound.
fn point(v: &Value, what: &str) -> Result<Vec3, String> {
    let a: [f32; 3] = serde_json::from_value(v.clone()).map_err(|_| format!("{what} must be [x, y, z]"))?;
    let p = Vec3::from(a);
    if !p.is_finite() || p.abs().max_element() > crate::mesh_asset::MAX_COORD {
        return Err(format!("{what} must be finite and within {}", crate::mesh_asset::MAX_COORD));
    }
    Ok(p)
}

impl Request {
    /// The call's arguments, bounded before anything is allocated; guides
    /// and density regions (world space at rest) moved into the clay's own
    /// space by the inverse of `world`.
    pub fn parse(args: &Value, world: Mat4) -> Result<Request, String> {
        let faces = match args.get("faces").filter(|v| !v.is_null()) {
            None => DEFAULT_FACES,
            Some(v) => v.as_u64().filter(|n| (MIN_FACES as u64..=MAX_FACES as u64).contains(n)).ok_or(format!("faces must be a whole number from {MIN_FACES} to {MAX_FACES}"))? as usize,
        };
        let input = retopo::input_points(faces);
        if input > retopo::MAX_INPUT {
            return Err(format!("{faces} faces would need {input} input points, over the {} bound", retopo::MAX_INPUT));
        }
        let symmetry = match args.get("symmetry").filter(|v| !v.is_null()) {
            None => Symmetry::Auto,
            Some(v) => match v.as_str() {
                Some("auto") => Symmetry::Auto,
                Some("x") => Symmetry::X,
                Some("none") => Symmetry::Off,
                _ => return Err("symmetry must be x, none or auto (x when the clay is X-symmetric within a cell)".into()),
            },
        };
        let to_local = world.inverse();
        if !to_local.is_finite() {
            return Err("the clay's placement cannot be inverted (a zero scale)".into());
        }
        let guides = match args.get("guides").filter(|v| !v.is_null()) {
            None => Vec::new(),
            Some(v) => {
                let list = v.as_array().ok_or("guides must be a list of polylines ([[x, y, z], ...])")?;
                if list.len() > MAX_GUIDES {
                    return Err(format!("at most {MAX_GUIDES} guides ({} sent)", list.len()));
                }
                let mut out = Vec::with_capacity(list.len());
                for (i, g) in list.iter().enumerate() {
                    let pts = g.as_array().ok_or_else(|| format!("guide {i} must be a list of [x, y, z] points"))?;
                    if !(2..=MAX_GUIDE_POINTS).contains(&pts.len()) {
                        return Err(format!("guide {i} has {} points (2 to {MAX_GUIDE_POINTS})", pts.len()));
                    }
                    out.push(pts.iter().map(|p| point(p, "a guide point").map(|p| to_local.transform_point3(p))).collect::<Result<Vec<_>, _>>()?);
                }
                out
            }
        };
        let density = match args.get("density").filter(|v| !v.is_null()) {
            None => Vec::new(),
            Some(v) => {
                let list = v.as_array().ok_or("density must be a list of {center, radius, scale}")?;
                if list.len() > MAX_DENSITY {
                    return Err(format!("at most {MAX_DENSITY} density regions ({} sent)", list.len()));
                }
                // The clay's own units per world unit (the mean of its axes).
                let unit = (to_local.transform_vector3(Vec3::X).length() + to_local.transform_vector3(Vec3::Y).length() + to_local.transform_vector3(Vec3::Z).length()) / 3.0;
                let mut out = Vec::with_capacity(list.len());
                for d in list {
                    let center = to_local.transform_point3(point(&d["center"], "a density center")?);
                    let radius = d["radius"].as_f64().map(|r| r as f32).filter(|r| r.is_finite() && *r > 0.0 && *r <= crate::mesh_asset::MAX_COORD).ok_or("a density region needs radius, a positive number")?;
                    let scale = number(d, "scale", 0.25, 4.0, 1.0)?;
                    out.push(Density { center, radius: radius * unit, scale });
                }
                out
            }
        };
        let fine = match args.get("source").filter(|v| !v.is_null()).map(|v| v.as_str()) {
            None | Some(Some("rest")) => false,
            Some(Some("fine")) => true,
            _ => return Err("source must be rest or fine".into()),
        };
        let name = match args["name"].as_str().filter(|n| !n.trim().is_empty()) {
            Some(n) if n.chars().count() > 64 => return Err("name is at most 64 characters".into()),
            n => n.map(str::to_string),
        };
        Ok(Request {
            args: Args {
                faces,
                adaptive: number(args, "adaptive", 0.0, 1.0, 0.3)?,
                symmetry,
                guides,
                density,
                keep_creases: number(args, "keep_creases", 0.0, 180.0, 40.0)?,
                keep_group_borders: boolean(args, "keep_group_borders", true)?,
                pure_quads: boolean(args, "pure_quads", true)?,
            },
            fine,
            name,
            hide: boolean(args, "hide_source", true)?,
            images: boolean(args, "images", true)?,
        })
    }

    /// The call as a Poly keeps it (`made_by`): in the clay's own space.
    pub fn made_by(&self, source: u64) -> Value {
        let a = &self.args;
        json!({
            "action": "retopo",
            "source": source,
            "space": "object",
            "faces": a.faces,
            "adaptive": a.adaptive,
            "symmetry": match a.symmetry { Symmetry::Auto => "auto", Symmetry::X => "x", Symmetry::Off => "none" },
            "guides": a.guides.iter().map(|g| g.iter().map(|p| p.to_array()).collect::<Vec<_>>()).collect::<Vec<_>>(),
            "density": a.density.iter().map(|d| json!({ "center": d.center.to_array(), "radius": d.radius, "scale": d.scale })).collect::<Vec<_>>(),
            "keep_creases": a.keep_creases,
            "keep_group_borders": a.keep_group_borders,
            "pure_quads": a.pure_quads,
            "mesh": if self.fine { "fine" } else { "rest" },
        })
    }

    /// The call a Poly's `made_by` keeps, checked again (a file may hold
    /// anything): already in the clay's space.
    pub fn from_made_by(made: &Value) -> Result<Request, String> {
        let mut args = made.clone();
        args["source"] = made.get("mesh").cloned().unwrap_or(Value::Null);
        let mut r = Request::parse(&args, Mat4::IDENTITY).map_err(|e| format!("this cage's made_by cannot be run again: {e}"))?;
        (r.hide, r.name) = (false, None);
        Ok(r)
    }
}

impl ClideApp {
    /// `retopo` (a job): parses the call, then `retopo_job`.
    pub(in crate::app) fn retopo_tool(&mut self, args: &Value) -> Result<Started, String> {
        let source = self.resolve(args.get("target").filter(|v| !v.is_null()).ok_or("target (a Clay object) is required")?)?;
        let o = self.scene.get(source).ok_or("object vanished")?;
        if o.clay.is_none() {
            return Err(format!("{} is not a Clay object (retopo reads a clay's surface; poly from_clay is the quick dense cage)", o.name));
        }
        let request = Request::parse(args, self.scene.world_matrix(source))?;
        self.retopo_job(source, None, request)
    }

    /// Retopology of clay `source` into a new Poly in its place (`into`
    /// None), or into `into`'s cage (`poly rebuild`: levels kept within the
    /// bound, creases when the faces are the same).
    pub(in crate::app) fn retopo_job(&mut self, source: u64, into: Option<u64>, request: Request) -> Result<Started, String> {
        let o = self.scene.get(source).ok_or("object vanished")?;
        let clay_name = o.name.clone();
        if o.clay.is_none() {
            return Err(format!("{} is not a Clay object", o.name));
        }
        // Its surface is the composed field's: unchecked ledgers settle first.
        self.settle_clays([source])?;
        self.meshes.sync(&self.scene);
        let clay = self.scene.get(source).and_then(|o| o.clay.clone()).expect("checked");
        let rest = self.meshes.rest(source).map(|m| m.0.clone()).ok_or("the clay has no mesh yet")?;
        if rest.indices.is_empty() {
            return Err("the clay has no surface".into());
        }
        let mut notes: Vec<String> = Vec::new();
        let composed = crate::layers::composed_clay(&clay);
        let fine = request.fine && composed.detail_cell.is_some();
        if request.fine && !fine {
            notes.push("the clay has no detail regions: its rest mesh was used".into());
        }
        // The input's triangles, from the meshes there are now (bounded
        // before anything is built: the weld reads at most this many).
        let triangles = if fine { composed.fine_estimate(&rest) } else { (rest.indices.len() / 3) as f64 };
        let most = 2 * crate::poly::MAX_CAGE_FACES;
        if triangles > most as f64 {
            return Err(format!("the clay's {} mesh has about {} triangles; retopo reads at most {most} (use source: rest, or larger detail cells)", if fine { "fine" } else { "rest" }, triangles.round()));
        }
        // Guides are laid on the surface every input edge: bounded here
        // too, so an oversized call never starts a job.
        let at = |i: u32| Vec3::from(rest.vertices[i as usize].pos);
        let area: f64 = rest.indices.chunks_exact(3).map(|t| (at(t[1]) - at(t[0])).cross(at(t[2]) - at(t[0])).length() as f64 * 0.5).sum();
        let length = retopo::input_length(area as f32, request.args.faces);
        let samples = retopo::guide_samples(&request.args.guides, length);
        if samples > retopo::MAX_GUIDE_SAMPLES {
            return Err(format!("the guides would lay {samples:.0} points on the surface (one per {length:.4}), over the {} bound: send shorter guides, or guides on this clay", retopo::MAX_GUIDE_SAMPLES));
        }
        let cell = if fine { composed.detail_cell.unwrap_or(0.01) } else { clay.cell_size().unwrap_or(0.01) };
        let library = self.scene.materials.clone();
        let before = self.scene.get(source).cloned();
        let target_before = into.and_then(|i| self.scene.get(i).cloned());
        let made_by = Arc::new(request.made_by(source));
        let name = request.name.clone().unwrap_or_else(|| format!("{clay_name}_retopo"));
        let (hide, images) = (request.hide && into.is_none(), request.images);
        let args = request.args.clone();
        let label = format!("Retopology of {clay_name}");
        Ok(Started::Job(label, Box::new(move |progress| {
            let timer = std::time::Instant::now();
            progress.stage("reading the surface", 0.0);
            let mesh = if fine { Arc::new(composed.build_fine(&library, "")?) } else { rest };
            progress.check()?;
            let points: Vec<Vec3> = mesh.vertices.iter().map(|v| Vec3::from(v.pos)).collect();
            let welded = crate::poly::from_mesh::from_triangles(&points, &mesh.indices, &[], cell * 1e-3)?;
            let (starts, corners) = welded.poly.cage_faces();
            let mut tris = Vec::with_capacity(corners.len());
            for f in starts.windows(2) {
                let face = &corners[f[0] as usize..f[1] as usize];
                for k in 1..face.len() - 1 {
                    tris.push([face[0], face[k], face[k + 1]]);
                }
            }
            let points = welded.poly.cage.to_vec();
            drop(welded);
            progress.check()?;
            let probe = crate::quality::Probe::new(if fine { &composed } else { &clay }).ok_or("the clay is empty")?;
            let groups = clay.groups.clone();
            let label = |p: Vec3| groups.as_ref().map_or(0, |g| g.label(p));
            let input = retopo::Input { points: &points, tris: &tris, surface: &probe, cell, groups: groups.is_some().then_some(&label as &(dyn Fn(Vec3) -> u16 + Sync)) };
            let out = retopo::retopo(&input, &args, progress)?;
            let mut poly = out.poly;
            poly.made_by = Some(made_by);
            notes.extend(out.notes);
            if let Some(old) = target_before.as_ref().and_then(|o| o.poly.as_ref()) {
                super::keep_levels(&mut poly, old, &mut notes);
            }
            let info = poly.info();
            let drawn = images.then(|| Arc::new(crate::mesh::Mesh::poly(&poly)));
            let b = &out.before_split;
            let mut reply = json!({
                "faces": out.shape.faces,
                "quads_pct": out.shape.quads_pct,
                "irregular_pct": out.shape.irregular_pct,
                "before_split": { "faces": b.faces, "quads_pct": b.quads_pct, "triangles": b.triangles, "pentagons": b.pentagons, "larger": b.larger, "irregular_pct": b.irregular_pct, "valence": b.valence },
                "valence": out.shape.valence,
                "edge_cv": out.shape.edge_cv,
                "hausdorff_cells": round(out.hausdorff_cells),
                "mean_distance_cells": round(out.mean_distance_cells),
                "coverage_cells": round(out.coverage_cells),
                "distance_samples": out.samples,
                "symmetric": out.symmetric,
                "symmetric_error": out.symmetric_error,
                "input": { "points": out.input_points, "triangles": out.input_triangles, "crease_edges": out.crease_edges, "mesh": if fine { "fine" } else { "rest" }, "cell": cell },
                "field": { "levels": out.levels, "singularities": out.singularities, "spacing": round(out.spacing) },
                "topology": { "open_edges": info["open_edges"], "non_manifold_edges": info["non_manifold_edges"], "flipped_edges": info["flipped_edges"], "non_manifold_points": info["non_manifold_points"], "genus": info["genus"], "components": info["components"] },
                "levels": poly.level_count(),
                "level": poly.level,
                "hash": info["hash"],
                "timings": out.timings.iter().map(|(k, t)| json!({ "step": k, "at": round(*t) })).collect::<Vec<_>>(),
            });
            progress.set(0.99);
            Ok(Box::new(move |app: &mut ClideApp| {
                if app.scene.get(source) != before.as_ref() || into.and_then(|i| app.scene.get(i)) != target_before.as_ref() {
                    return Err("the clay or the Poly changed meanwhile; nothing was changed, send the call again".into());
                }
                let id = match into {
                    Some(id) => {
                        app.scene.get_mut(id).expect("checked").poly = Some(poly);
                        id
                    }
                    None => app.place_poly(before.as_ref().expect("checked"), name, poly, hide, &mut notes),
                };
                reply["object"] = object_json_in(&app.scene, id);
                reply["seconds"] = json!(round(timer.elapsed().as_secs_f32()));
                let mut image = None;
                if let Some(mesh) = drawn {
                    match app.wire_sheet(id, mesh) {
                        Ok(img) => image = Some(img),
                        Err(e) => notes.push(format!("no wireframe image: {e}")),
                    }
                }
                reply["notes"] = json!(notes);
                let mut result = text(reply);
                if let Some(img) = image {
                    result.extend(img);
                }
                Ok(result)
            }) as Finish)
        })))
    }
}

/// What the Retopologize… dialog holds (editor state).
#[derive(Clone, Debug)]
pub(in crate::app) struct RetopoDialog {
    pub target: u64,
    pub faces: usize,
    pub adaptive: f32,
    pub symmetry: Symmetry,
    pub keep_creases: f32,
    pub pure_quads: bool,
}

impl RetopoDialog {
    pub fn new(target: u64) -> RetopoDialog {
        RetopoDialog { target, faces: DEFAULT_FACES, adaptive: 0.3, symmetry: Symmetry::Auto, keep_creases: 40.0, pure_quads: true }
    }

    /// The `retopo` request the dialog sends: what an agent would.
    pub fn request(&self) -> Value {
        json!({
            "target": self.target,
            "faces": self.faces,
            "adaptive": self.adaptive,
            "symmetry": match self.symmetry { Symmetry::Auto => "auto", Symmetry::X => "x", Symmetry::Off => "none" },
            "keep_creases": self.keep_creases,
            "pure_quads": self.pure_quads,
        })
    }
}

impl ClideApp {
    pub(in crate::app) fn open_retopo_dialog(&mut self) {
        let Some(id) = self.lead().filter(|id| self.scene.get(*id).is_some_and(|o| o.clay.is_some())) else { return };
        self.retopo_dialog = Some(RetopoDialog::new(id));
    }

    /// The Retopologize… window; Retopologize (or Enter) sends `retopo`.
    pub(in crate::app) fn retopo_dialog_window(&mut self, ctx: &egui::Context) {
        let Some(mut d) = self.retopo_dialog.clone() else { return };
        let mut open = true;
        let mut send = false;
        let name = self.scene.get(d.target).map(|o| o.name.clone()).unwrap_or_default();
        egui::Window::new("Retopologize").open(&mut open).collapsible(false).resizable(false).show(ctx, |ui| {
            ui.label(format!("{name}: a clean quad cage (a new Poly object)"));
            ui.add(egui::Slider::new(&mut d.faces, MIN_FACES..=MAX_FACES).logarithmic(true).text("Quads"));
            ui.add(egui::Slider::new(&mut d.adaptive, 0.0..=1.0).text("Adaptive"));
            ui.add(egui::Slider::new(&mut d.keep_creases, 0.0..=180.0).text("Crease angle"));
            ui.horizontal(|ui| {
                ui.label("Symmetry");
                ui.selectable_value(&mut d.symmetry, Symmetry::Auto, "Auto");
                ui.selectable_value(&mut d.symmetry, Symmetry::X, "X");
                ui.selectable_value(&mut d.symmetry, Symmetry::Off, "None");
            });
            ui.checkbox(&mut d.pure_quads, "All quads");
            send = ui.button("Retopologize").clicked() || ui.input(|i| i.key_pressed(egui::Key::Enter));
        });
        if send {
            self.send_ui_call("retopo", d.request());
            self.retopo_dialog = None;
        } else {
            self.retopo_dialog = open.then_some(d);
        }
    }
}
