//! `poly project` (docs/QUALITY_PLAN.md Stage 3c): a clay's detail onto a
//! Poly's subdivision levels as tangent-space vector displacement
//! (`crate::poly::project`), run as a job.
//!
//! Every size is checked from the arguments before anything is built:
//! levels by the face bound (`check_levels`, from the counts), cone
//! 0-80 degrees, reach a positive length within the coordinate bound
//! (and, on the job's thread before any point moves, within the cage's
//! size).

use std::sync::Arc;

use glam::Vec3;
use serde_json::{Value, json};

use super::super::ClideApp;
use super::super::jobs::{Finish, Started};
use super::super::tools::{object_json_in, text};
use crate::poly::project::{self, Args, DEFAULT_CONE, MAX_CONE, Mode, REACH_EDGES, Target};
use crate::poly::{MAX_LEVELS, MAX_SHOWN_FACES, Poly};

fn round(x: f32) -> f64 {
    (x as f64 * 1e4).round() / 1e4
}

/// A checked `poly project` call (reach None: the default, worked out
/// from the cage on the job's thread).
#[derive(Clone, Debug)]
pub(in crate::app) struct Request {
    pub levels: Option<u8>,
    pub mode: Mode,
    pub cone: f32,
    pub reach: Option<f32>,
}

impl Request {
    pub fn parse(args: &Value) -> Result<Request, String> {
        let levels = match args.get("levels").filter(|v| !v.is_null()) {
            None => None,
            Some(v) => Some(v.as_u64().filter(|n| (1..=MAX_LEVELS as u64).contains(n)).ok_or(format!("levels must be a whole number from 1 to {MAX_LEVELS}"))? as u8),
        };
        let mode = match args.get("mode").filter(|v| !v.is_null()).map(|v| v.as_str()) {
            None | Some(Some("vector")) => Mode::Vector,
            Some(Some("scalar")) => Mode::Scalar,
            _ => return Err("mode must be vector or scalar".into()),
        };
        let cone = match args.get("cone").filter(|v| !v.is_null()) {
            None => DEFAULT_CONE,
            Some(v) => v.as_f64().map(|x| x as f32).filter(|x| x.is_finite() && (0.0..=MAX_CONE).contains(x)).ok_or(format!("cone must be degrees from 0 to {MAX_CONE}"))?,
        };
        let reach = match args.get("reach").filter(|v| !v.is_null()) {
            None => None,
            Some(v) => Some(v.as_f64().map(|x| x as f32).filter(|x| x.is_finite() && *x > 0.0 && *x <= crate::mesh_asset::MAX_COORD).ok_or("reach must be a positive length (the Poly's units)")?),
        };
        Ok(Request { levels, mode, cone, reach })
    }

    /// Levels: as asked, else the levels kept, else as many as keep the
    /// shown level within its bound (at most 4).
    fn levels_for(&self, p: &Poly) -> u8 {
        self.levels.unwrap_or_else(|| if p.level_count() > 0 { p.level_count() } else { (1..=4).rev().find(|&l| p.faces_at(l) <= MAX_SHOWN_FACES).unwrap_or(1) })
    }
}

impl ClideApp {
    /// `poly project` (a job, editing): the source clay settled, then its
    /// composed surface projected onto the target's levels.
    pub(in crate::app) fn poly_project(&mut self, args: &Value) -> Result<Started, String> {
        let request = Request::parse(args)?;
        let (id, p) = self.poly_of(args)?;
        let poly = p.clone();
        let p = &poly;
        let levels = request.levels_for(p);
        // Refused from the counts before anything is built.
        crate::poly::check_levels(p.faces() as u64, p.corners.len() as u64, levels)?;
        let source = match args.get("source").filter(|v| !v.is_null()) {
            Some(v) => self.resolve(v)?,
            None => [p.projected.as_deref(), p.made_by.as_deref()]
                .into_iter()
                .flatten()
                .find_map(|m| m["source"].as_u64().filter(|s| self.scene.get(*s).is_some_and(|o| o.clay.is_some())))
                .ok_or("source (a Clay object) is required: this Poly records no clay it was made from")?,
        };
        let o = self.scene.get(source).ok_or("object vanished")?;
        if o.clay.is_none() {
            return Err(format!("{} is not a Clay object (project reads a clay's surface)", o.name));
        }
        // The composed field is read: unchecked ledgers settle first.
        let unsettled = o.clay.clone();
        self.settle_clays([source])?;
        let settled = self.scene.get(source).and_then(|o| o.clay.as_ref()) != unsettled.as_ref();
        self.meshes.sync(&self.scene);
        let clay = self.scene.get(source).and_then(|o| o.clay.clone()).expect("checked");
        let rest = self.meshes.rest(source).map(|m| m.0.clone());
        let cell = clay.cell_size().ok_or("the clay is empty")?;
        let to_clay = self.scene.world_matrix(source).inverse() * self.scene.world_matrix(id);
        if !to_clay.is_finite() {
            return Err("the clay's placement cannot be inverted (a zero scale)".into());
        }
        let before = self.scene.get(source).cloned();
        let target_before = self.scene.get(id).cloned();
        let label = format!("Project {} onto {}", before.as_ref().map_or("?", |o| o.name.as_str()), target_before.as_ref().map_or("?", |o| o.name.as_str()));
        Ok(Started::Job(label, Box::new(move |progress| {
            let timer = std::time::Instant::now();
            progress.stage("reading the clay", 0.0);
            let composed = crate::layers::composed_clay(&clay);
            let probe = crate::quality::Probe::new(&composed).ok_or("the clay is empty")?;
            let target = Target { surface: &probe, to_clay, cell };
            let edge = project::mean_edge(&poly);
            let reach = request.reach.unwrap_or(REACH_EDGES * edge);
            let (lo, hi) = poly.cage.iter().fold((Vec3::MAX, Vec3::MIN), |(a, b), p| (a.min(p), b.max(p)));
            let size = (hi - lo).length();
            if reach > size {
                return Err(format!("reach {reach} is more than the cage's size ({size:.4}); the default is {REACH_EDGES} cage edges ({:.4})", REACH_EDGES * edge));
            }
            progress.check()?;
            progress.stage("projecting", 0.02);
            let args = Args { levels, mode: request.mode, cone: request.cone, reach };
            let made = project::project(&poly, &target, &args, progress)?;
            let mut next = project::install(&poly, &made)?;
            next.projected = Some(Arc::new(json!({ "source": source, "levels": levels, "mode": request.mode.name(), "cone": request.cone, "reach": reach })));
            progress.stage("measuring", 0.92);
            let r = crate::poly::subdiv::Refiner::new(&next, levels)?;
            let rest_points: Option<Vec<Vec3>> = rest.as_ref().map(|m| m.vertices.iter().map(|v| Vec3::from(v.pos)).collect());
            let m = project::measure(&made.points, &r.top.face_start, &r.top.corners, &r.top.edges, &target, rest_points.as_deref(), progress)?;
            let mut notes: Vec<String> = Vec::new();
            let missed: usize = made.stats.iter().map(|s| s.missed).sum();
            if missed > 0 {
                notes.push(format!("{missed} points found no surface within reach {reach:.4} and kept their subdivided place (a larger reach, or a cage nearer the clay)"));
            }
            let won: usize = made.stats.iter().map(|s| s.scalar_won).sum();
            if won > 0 {
                notes.push(format!("{won} points took the normal's crossing: their nearest point was more than reach / 2 from it (another surface across a gap)"));
            }
            if poly.level_count() > levels {
                notes.push(format!("levels {} to {} keep no displacement (theirs were relative to the old coarser levels)", levels + 1, poly.level_count()));
            }
            let info = next.info();
            let reply = json!({
                "target": id,
                "source": source,
                "settled": settled,
                "levels": levels,
                "level": next.level,
                "level_faces": (0..=next.level_count()).map(|l| next.faces_at(l)).collect::<Vec<_>>(),
                "mode": request.mode.name(),
                "cone": request.cone,
                "reach": round(reach),
                "cell": cell,
                "per_level": made.stats,
                "hausdorff_cells": round(m.hausdorff_cells),
                "mean_distance_cells": round(m.mean_distance_cells),
                "rms_cells": round(m.rms_cells),
                "distance_samples": m.distance_samples,
                "normal_rms_deg": round(m.normal_rms_deg),
                "coverage_cells": m.coverage_cells.map(round),
                "hash": info["hash"],
            });
            progress.set(0.99);
            Ok(Box::new(move |app: &mut ClideApp| {
                if app.scene.get(source) != before.as_ref() || app.scene.get(id) != target_before.as_ref() {
                    return Err("the clay or the Poly changed meanwhile; nothing was changed, send the call again".into());
                }
                app.scene.get_mut(id).expect("checked").poly = Some(next);
                let mut reply = reply;
                reply["object"] = object_json_in(&app.scene, id);
                reply["seconds"] = json!(round(timer.elapsed().as_secs_f32()));
                reply["notes"] = json!(notes);
                Ok(text(reply))
            }) as Finish)
        })))
    }
}

/// What the Project Clay Detail… dialog holds (editor state).
#[derive(Clone, Debug)]
pub(in crate::app) struct ProjectDialog {
    pub target: u64,
    /// The clay: the one the Poly was projected from or made from.
    pub source: Option<u64>,
    pub levels: u8,
    pub mode: Mode,
    pub cone: f32,
}

impl ProjectDialog {
    /// The `poly project` request the dialog sends: what an agent would.
    pub fn request(&self) -> Value {
        let mut r = json!({ "target": self.target, "action": "project", "levels": self.levels, "mode": self.mode.name(), "cone": self.cone });
        if let Some(s) = self.source {
            r["source"] = json!(s);
        }
        r
    }
}

impl ClideApp {
    /// Opens the dialog for the lead Poly.
    pub(in crate::app) fn open_project_dialog(&mut self) {
        let Some((id, p)) = self.lead().and_then(|id| self.scene.get(id).and_then(|o| o.poly.as_ref()).map(|p| (id, p))) else { return };
        let source = [p.projected.as_deref(), p.made_by.as_deref()].into_iter().flatten().find_map(|m| m["source"].as_u64().filter(|s| self.scene.get(*s).is_some_and(|o| o.clay.is_some())));
        let levels = Request { levels: None, mode: Mode::Vector, cone: DEFAULT_CONE, reach: None }.levels_for(p);
        self.project_dialog = Some(ProjectDialog { target: id, source, levels, mode: Mode::Vector, cone: DEFAULT_CONE });
    }

    /// The Project Clay Detail… window; Project (or Enter) sends `poly
    /// project`.
    pub(in crate::app) fn project_dialog_window(&mut self, ctx: &egui::Context) {
        let Some(mut d) = self.project_dialog.clone() else { return };
        let mut open = true;
        let mut send = false;
        let name = |id: u64| self.scene.get(id).map(|o| o.name.clone()).unwrap_or_default();
        let (target, source) = (name(d.target), d.source.map(name));
        egui::Window::new("Project Clay Detail").open(&mut open).collapsible(false).resizable(false).show(ctx, |ui| {
            match &source {
                Some(s) => ui.label(format!("{s}'s detail onto {target}'s levels")),
                None => ui.label(format!("{target} records no clay: select it after the clay and use poly project's source")),
            };
            ui.add(egui::Slider::new(&mut d.levels, 1..=MAX_LEVELS).text("Levels"));
            ui.horizontal(|ui| {
                ui.label("Mode");
                ui.selectable_value(&mut d.mode, Mode::Vector, "Vector");
                ui.selectable_value(&mut d.mode, Mode::Scalar, "Scalar");
            });
            ui.add_enabled(d.mode == Mode::Vector, egui::Slider::new(&mut d.cone, 0.0..=MAX_CONE).text("Cone"));
            send = ui.add_enabled(d.source.is_some(), egui::Button::new("Project")).clicked() || (d.source.is_some() && ui.input(|i| i.key_pressed(egui::Key::Enter)));
        });
        if send {
            self.send_ui_call("poly", d.request());
            self.project_dialog = None;
        } else {
            self.project_dialog = open.then_some(d);
        }
    }
}
