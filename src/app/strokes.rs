//! The ledger tools (docs/SCULPT_PLAN.md 4.2): `stroke_log` lists a clay's
//! stroke records, `stroke_edit` disables, deletes, replaces, moves,
//! scales (Adjust Last), rebases, replays or squashes them, `sculpt_diff`
//! measures what changed between two points of a ledger, `probe_clay`
//! reads the field at points, and `history` lists and jumps the undo
//! history (and runs QuickSave). The editor's controls call these same
//! handlers.

use std::time::Instant;

use glam::{Mat4, Vec3};
use serde_json::{Value, json};

use super::ClideApp;
use super::jobs::Started;
use super::tools::text;
use crate::clay::{Clay, Detail, Field};
use crate::ledger::{self, Edit, Grid};
use crate::mcp::ToolResult;
use crate::groups::Groups;
use crate::mask::SculptMask;
use crate::paint::Paint;
use crate::scene::Shape;

/// Most records one `stroke_log` page lists.
const MAX_PAGE: usize = 200;

fn r3(v: Vec3) -> [f32; 3] {
    let r = |x: f32| (x * 1e5).round() / 1e5;
    [r(v.x), r(v.y), r(v.z)]
}

fn ids(args: &Value) -> Result<Vec<u64>, String> {
    let mut out = Vec::new();
    for v in args["ids"].as_array().into_iter().flatten() {
        out.push(v.as_u64().ok_or("ids must be record ids (whole numbers; see stroke_log)")?);
    }
    if let Some(id) = args["id"].as_u64() {
        out.push(id);
    }
    Ok(out)
}

impl ClideApp {
    pub(super) fn clay_of(&self, args: &Value) -> Result<(u64, &Clay), String> {
        let id = self.resolve(&args["target"])?;
        let o = self.scene.get(id).ok_or("object vanished")?;
        if o.shape != Shape::Clay {
            return Err(format!("{} is not a clay object", o.name));
        }
        Ok((id, o.clay.as_ref().ok_or("the clay has no blobs yet")?))
    }

    /// `stroke_log`: a layer's records, a page at a time.
    pub(super) fn stroke_log(&self, args: &Value) -> ToolResult {
        let (id, clay) = self.clay_of(args)?;
        match args["layer"].as_str().unwrap_or("base") {
            "mask" => Ok(text(log::<SculptMask>(id, clay, args)?)),
            "groups" => Ok(text(log::<Groups>(id, clay, args)?)),
            name if name.starts_with("archived:") => {
                // A baked or merged layer's records, as they were.
                let n = &name["archived:".len()..];
                let a = clay.sculpt.as_ref().and_then(|s| s.data().archived.iter().rev().find(|a| a.name == n)).ok_or_else(|| format!("no archived layer {n} (sculpt_layer list shows them)"))?;
                let one = |r: Value| r;
                let mut records: Vec<Value> = a.strokes.records().iter().enumerate().map(|(i, r)| one(ledger::record_json(r, i + 1, false, false))).collect();
                records.extend(a.paint_strokes.records().iter().enumerate().map(|(i, r)| ledger::record_json(r, i + 1, false, false)));
                let total = records.len();
                records.truncate(MAX_PAGE);
                Ok(text(json!({ "target": id, "layer": { "name": name, "into": a.into, "archived": true }, "records": records, "total": total })))
            }
            name => match crate::layers::kind_of(clay, name)? {
                crate::layers::Kind::Sculpt => Ok(text(log::<Detail>(id, &*crate::layers::view_for::<Detail>(clay, name)?, args)?)),
                crate::layers::Kind::Paint => Ok(text(log::<Paint>(id, &*crate::layers::view_for::<Paint>(clay, name)?, args)?)),
            },
        }
    }

    /// `stroke_edit`: one edit of a layer's ledger, replayed as a job
    /// (Adjust Last answers at once).
    pub(super) fn stroke_edit(&mut self, args: &Value, posed: Option<f32>) -> Result<Started, String> {
        let (id, _) = self.clay_of(args)?;
        let layer = args["layer"].as_str().unwrap_or("base");
        let action = args["action"].as_str().ok_or("action is required: disable, enable, delete, replace, move, scale, rebase, replay or squash")?;
        if action == "scale" && args["last"].as_bool().unwrap_or(false) {
            if !args["layer"].is_null() && self.scene.get(id).and_then(|o| o.clay.as_ref()).is_none_or(|c| crate::layers::kind_of(c, layer).ok() != Some(crate::layers::Kind::Sculpt)) {
                return Err("Adjust Last scales sculpt strokes (a sculpt layer)".into());
            }
            return Ok(Started::Now(self.adjust_last(id, args)));
        }
        let mut alphas = None;
        let edit = match action {
            "disable" | "enable" => Edit::Enable(ids(args)?, action == "enable"),
            "delete" => Edit::Delete(ids(args)?),
            "move" => Edit::Move(ids(args)?, args["to_seq"].as_u64().ok_or("move needs to_seq: the position (1-based) to move the records to")? as usize),
            "scale" => Edit::Scale(ids(args)?, args["factor"].as_f64().ok_or("scale needs factor 0 to 2 (or last: true with factor for the newest stroke)")? as f32),
            "rebase" => Edit::Rebase,
            "replay" => Edit::Replay(args["from_seq"].as_u64().map_or(0, |s| s.max(1) as usize - 1)),
            "squash" => Edit::Squash(ids(args)?),
            "replace" => {
                let rid = *ids(args)?.first().ok_or("replace needs id (or ids: [id]): the record to replace")?;
                let spec = args.get("spec").filter(|v| !v.is_null()).ok_or("replace needs spec: the new stroke (a sculpt or paint stroke, in the call's space)")?;
                let (to_local, radius_scale) = self.clay_space(id, args)?;
                // Unchecked ledgers settle first: a posable stroke unposes
                // through the clay's surface as sculpted.
                self.settle_clays([id])?;
                self.meshes.sync(&self.scene);
                if layer == "groups" {
                    return Err("group strokes are replaced by painting again (group {actions: [{paint: {id, strokes}}]}); disable or delete this one".into());
                }
                let clay = self.scene.get(id).and_then(|o| o.clay.as_ref()).ok_or("not a clay object")?;
                let brush = match layer {
                    "mask" => Some(crate::sculpt::Brush::MaskPaint),
                    name => match crate::layers::kind_of(clay, name)? {
                        crate::layers::Kind::Paint => Some(crate::sculpt::Brush::Paint),
                        crate::layers::Kind::Sculpt => None,
                    },
                };
                let mut spec = crate::brush::preset::merge(&self.scene.brushes, None, spec)?;
                // Detail levels as `sculpt` fills them (the spec's level, else
                // the clay's active one), so a record's stored spec sent
                // again does what it did there.
                if brush.is_none() {
                    crate::levels::fill(clay, &mut spec, None)?;
                }
                // Posable symmetry as in sculpt: the stroke's frame (or the
                // editor's), and the way back to the rest pose. Paint refuses
                // it, as the paint tool does.
                let unpose = match brush {
                    None => self.posable_unposer(id, std::slice::from_mut(&mut spec), posed, to_local)?,
                    Some(_) => None,
                };
                let strokes = self.resolve_strokes_posed(id, args, std::slice::from_ref(&spec), brush, to_local, radius_scale, unpose.as_ref().map(|f| f as crate::brush::resolve::Unpose))?;
                alphas = self.alphas_for(id, &strokes)?;
                let (spec, resolved) = strokes.into_iter().next().expect("one stroke");
                // `sculpt` grows a detail region of the stroke's cell over it
                // first (Sculptris Pro, a relayout record of its own); a
                // replacement in the ledger cannot, so it is refused rather
                // than written at the levels already there.
                if brush.is_none() {
                    let clay = self.scene.get(id).and_then(|o| o.clay.as_ref()).ok_or("not a clay object")?;
                    if crate::levels::grows_regions(clay, spec.cell, &resolved) {
                        return Err(format!("the new stroke reaches past the clay's detail regions of cell {}: replace cannot grow them (sculpt does, as a record of its own); disable this record and sculpt the stroke again", spec.cell.unwrap_or_default()));
                    }
                }
                Edit::Replace(rid, Box::new(spec), Box::new(resolved))
            }
            other => return Err(format!("unknown action {other}; use disable, enable, delete, replace, move, scale, rebase, replay or squash")),
        };
        let clay = self.scene.get(id).and_then(|o| o.clay.as_ref()).ok_or("not a clay object")?;
        match layer {
            "mask" => self.edit_job::<SculptMask>(id, "mask", action, edit, alphas),
            "groups" => self.edit_job::<Groups>(id, "groups", action, edit, alphas),
            name => match crate::layers::kind_of(clay, name)? {
                crate::layers::Kind::Sculpt => self.edit_job::<Detail>(id, name, action, edit, alphas),
                crate::layers::Kind::Paint => self.edit_job::<Paint>(id, name, action, edit, alphas),
            },
        }
    }

    /// `alphas`: the clay's alpha store with the images a replacing stroke
    /// uses.
    fn edit_job<G: crate::layers::Layered>(&mut self, id: u64, layer: &str, action: &str, edit: Edit, alphas: Option<crate::brush::alpha::AlphaStore>) -> Result<Started, String> {
        let o = self.scene.get(id).ok_or("object vanished")?;
        let before = o.clay.clone().unwrap_or_default();
        let layer = layer.to_string();
        crate::layers::view_for::<G>(&before, &layer)?;
        let name = o.name.clone();
        let cache = self.replay_cache.clone();
        let action = action.to_string();
        Ok(Started::Job(
            format!("{action} strokes on {name}"),
            Box::new(move |progress| {
                progress.set(0.05);
                let t = Instant::now();
                // Unchecked ledgers settle first (both: paint reads the
                // sculpted surface), on a copy: `before` is what the clay
                // must still hold when the edit installs.
                let mut settled = before.clone();
                ledger::settle_clay(&mut settled, Some(&cache))?;
                if let Some(store) = alphas {
                    ledger::set_alphas(&mut settled, store);
                }
                // A replacing stroke reads the mask and groups as they are now.
                let mut edit = edit;
                if let Edit::Replace(_, _, stroke) = &mut edit
                    && !G::LAYER.eq("mask")
                {
                    ledger::freeze(&mut settled, stroke)?;
                }
                let e = {
                    let v = crate::layers::view_for::<G>(&settled, &layer)?;
                    ledger::edit::<G>(&v, &ledger::ledger::<G>(&v), edit, Some(&cache))?
                };
                let ms = t.elapsed().as_secs_f64() * 1000.0;
                Ok(Box::new(move |app: &mut ClideApp| {
                    let o = app.scene.get_mut(id).ok_or("the object was deleted meanwhile")?;
                    let cur = o.clay.as_mut().ok_or("the object is no longer clay")?;
                    let state = |c: &Clay| (c.sculpt.as_ref().and_then(|_| crate::layers::ledger_of::<G>(c, &layer).ok()).map(|l| l.head()), crate::layers::grid_of::<G>(c, &layer).ok().flatten(), crate::layers::context_of::<G>(c, &layer).ok());
                    if state(cur) != state(&before) {
                        return Err(format!("the {layer} layer, what it reads or the blobs changed meanwhile; nothing was changed, send the edit again"));
                    }
                    let (head, total) = (e.ledger.head(), e.ledger.len());
                    // A replay that found mismatches is a check: the clay
                    // keeps its grid (rebase takes the replayed results).
                    let keep = action == "replay" && !e.mismatched.is_empty();
                    if !keep {
                        crate::layers::install::<G>(cur, &layer, e.ledger, e.grid);
                        ledger::set_alphas(cur, settled.sculpt.as_ref().map(|s| s.data().alphas.clone()).unwrap_or_default());
                        ledger::merge_frozen(cur, &settled);
                    }
                    ledger::adopt_settled(cur, &before, &settled);
                    let mut out = json!({
                        "target": id,
                        "layer": layer,
                        "action": action,
                        "replayed": e.replayed,
                        "from_seq": e.from + 1,
                        "records": total,
                        "head": head,
                        "diff_stats": e.stats,
                        "timing_ms": (ms * 10.0).round() / 10.0,
                        "step": app.history.current() + 1,
                    });
                    if action == "replay" || action == "rebase" {
                        out["mismatched"] = json!(e.mismatched);
                    }
                    if keep {
                        out["kept"] = json!("the clay's own grid: replaying the records gave another result; rebase takes the replayed results");
                        out["step"] = Value::Null;
                    }
                    Ok(text(out))
                }) as super::jobs::Finish)
            }),
        ))
    }

    /// Adjust Last: scales the newest stroke this session made on the clay.
    fn adjust_last(&mut self, id: u64, args: &Value) -> ToolResult {
        let factor = args["factor"].as_f64().ok_or("scale needs factor 0 to 2")? as f32;
        let last = self.last_deltas.get(&id).cloned().ok_or("no last stroke on this clay in this session; scale a record by id: stroke_edit {action: scale, ids, factor}")?;
        let clay = self.scene.get(id).and_then(|o| o.clay.as_ref()).ok_or("not a clay object")?;
        let (l, grid, record, tag) = ledger::adjust_last(&*crate::layers::view_for::<Detail>(clay, &last.layer).map_err(|e| format!("stale last: {e}"))?, &last, factor)?;
        let head = l.head();
        let clay = self.scene.get_mut(id).and_then(|o| o.clay.as_mut()).expect("checked");
        crate::layers::install::<Detail>(clay, &last.layer, l, grid);
        self.last_deltas.insert(id, tag);
        Ok(text(json!({ "target": id, "layer": last.layer, "record": record.id, "made_by": last.source, "factor": factor, "stats": record.stats, "head": head, "step": self.history.current() + 1 })))
    }

    /// `sculpt_diff`: how the base layer differs between two points. A
    /// point in the ledger (a seq or {id}) is replayed as a job.
    pub(super) fn sculpt_diff(&mut self, args: &Value) -> Result<Started, String> {
        let (id, clay) = self.clay_of(args)?;
        let whole = clay.clone();
        // The sculpt layer measured (base by default), seen through its view.
        let layer = args["layer"].as_str().unwrap_or("base").to_string();
        if crate::layers::kind_of(&whole, &layer)? != crate::layers::Kind::Sculpt {
            return Err(format!("{layer} is a paint layer; sculpt_diff measures sculpt layers"));
        }
        let clay = crate::layers::view_for::<Detail>(&whole, &layer)?.into_owned();
        let l = ledger::ledger::<Detail>(&clay);
        /// A point to compare: a grid at hand, or the records to replay.
        enum At {
            Grid(Option<Detail>),
            Replay(usize),
        }
        let at = |v: &Value| -> Result<At, String> {
            match v {
                Value::Null => Ok(At::Grid(clay.detail.clone())),
                Value::String(s) if s == "current" => Ok(At::Grid(clay.detail.clone())),
                Value::String(s) if s == "blobs" => Ok(At::Grid(None)),
                Value::Number(n) => {
                    let seq = n.as_u64().ok_or("a seq is a whole number")? as usize;
                    if seq > l.len() {
                        return Err(format!("no seq {seq}: the layer has {} records", l.len()));
                    }
                    Ok(At::Replay(seq))
                }
                Value::Object(o) if o.get("id").is_some_and(Value::is_u64) => {
                    let rid = o["id"].as_u64().expect("checked");
                    let i = l.position(rid).ok_or_else(|| format!("no record {rid}"))?;
                    Ok(At::Replay(i + 1))
                }
                Value::String(s) if s.starts_with("morph:") => {
                    // The morph's grid of the same layer.
                    let n = &s["morph:".len()..];
                    let m = whole.sculpt.as_ref().and_then(|st| st.data().morphs.iter().find(|m| m.name == n)).ok_or_else(|| format!("no morph {n} (morph list)"))?;
                    if layer == "base" {
                        Ok(At::Grid(m.base.clone()))
                    } else {
                        Ok(At::Grid(m.layers.iter().find(|x| x.name == layer).ok_or_else(|| format!("morph {n} has no layer {layer}"))?.detail.clone()))
                    }
                }
                other => Err(format!("from/to must be a seq (number), {{id}}, \"blobs\", \"current\" or \"morph:NAME\", not {other}")),
            }
        };
        let from = at(args.get("from").ok_or("from is required: a seq, {id}, or \"blobs\"")?)?;
        let to = at(&args["to"])?;
        let (to_local, scale) = self.clay_space(id, args)?;
        let region = match args.get("region").filter(|r| !r.is_null()) {
            Some(r) => {
                let c: [f32; 3] = serde_json::from_value(r["center"].clone()).map_err(|_| "region needs center [x, y, z] and radius")?;
                let rad = r["radius"].as_f64().ok_or("region needs center [x, y, z] and radius")? as f32;
                Some((to_local.transform_point3(Vec3::from(c)), rad * scale))
            }
            None => None,
        };
        let bins = args["histogram_bins"].as_u64().unwrap_or(10).clamp(1, 64) as usize;
        let diff = move |clay: &Clay, from: Option<Detail>, to: Option<Detail>| text(diff_json(clay, from.as_ref(), to.as_ref(), region, bins, to_local.inverse(), scale));
        let (from, to) = match (from, to) {
            (At::Grid(f), At::Grid(t)) => return Ok(Started::Now(Ok(diff(&clay, f, t)))),
            pair => pair,
        };
        // Replays can run long (a loaded ledger has no checkpoints yet), so
        // they run off the UI thread; the diff is of the clay as it was.
        let cache = self.replay_cache.clone();
        let name = self.scene.get(id).map_or_else(String::new, |o| o.name.clone());
        Ok(Started::Job(
            format!("diff strokes on {name}"),
            Box::new(move |progress| {
                progress.set(0.05);
                let grid = |a: At| match a {
                    At::Grid(g) => Ok(g),
                    At::Replay(n) => ledger::replay(&l, &clay, n, Some(&cache), |_, _| {}),
                };
                let from = grid(from)?;
                progress.set(0.5);
                let out = diff(&clay, from, grid(to)?);
                Ok(Box::new(move |_app: &mut ClideApp| Ok(out)) as super::jobs::Finish)
            }),
        ))
    }

    /// `probe_clay`: the field at points, along rays or from a view.
    pub(super) fn probe_clay(&mut self, args: &Value) -> ToolResult {
        let (id, clay) = self.clay_of(args)?;
        let clay = clay.clone();
        let (to_local, scale) = self.clay_space(id, args)?;
        let world = to_local.inverse();
        let mut points: Vec<(Value, Option<Vec3>)> = Vec::new();
        for p in args["points"].as_array().into_iter().flatten() {
            let v: [f32; 3] = serde_json::from_value(p.clone()).map_err(|_| "points must be [x, y, z] lists")?;
            points.push((json!({ "from": v }), Some(to_local.transform_point3(Vec3::from(v)))));
        }
        let mut rays: Vec<(Vec3, Vec3)> = Vec::new();
        for r in args["rays"].as_array().into_iter().flatten() {
            let v = |k: &str| -> Result<Vec3, String> { serde_json::from_value::<[f32; 3]>(r[k].clone()).map(Vec3::from).map_err(|_| format!("each ray needs {k}: [x, y, z]")) };
            rays.push((v("origin")?, v("direction")?.normalize_or_zero()));
        }
        if let Some(screen) = args.get("screen").filter(|s| !s.is_null()) {
            let view = screen["view"].as_str().ok_or("screen needs view (front, back, left, right, top, bottom) and at")?;
            let far = self.scene_sphere().1 * 4.0 + 10.0;
            for a in screen["at"].as_array().into_iter().flatten() {
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
        if !rays.is_empty() {
            self.meshes.sync(&self.scene);
            let cast = self.clay_caster(id)?;
            // Named-view rays are world rays; the call's space maps them.
            let m = if args["screen"].is_null() { to_local } else { self.scene.world_matrix(id).inverse() };
            for (o, d) in rays {
                let hit = cast(m.transform_point3(o), m.transform_vector3(d).normalize_or_zero()).map(|(q, _)| q);
                points.push((json!({ "origin": r3(o), "direction": r3(d) }), hit));
            }
        }
        if points.is_empty() {
            return Err("pass points, rays, or screen {view, at}".into());
        }
        if points.len() > 256 {
            return Err("at most 256 probes per call".into());
        }
        let probes: Vec<Value> = points.into_iter().map(|(mut v, p)| {
            match p {
                Some(p) => probe(&clay, p, world, scale, &mut v),
                None => v["hit"] = json!(false),
            }
            v
        }).collect();
        Ok(text(json!({ "target": id, "probes": probes, "units": "distance and points in the call's space; layer_offsets in millimetres of it" })))
    }

    /// `history`: list, goto, mark, label, quicksave.
    pub(super) fn history_tool(&mut self, args: &Value) -> ToolResult {
        let index = |app: &ClideApp| -> Result<u64, String> {
            match (args["index"].as_u64(), args["mark"].as_str()) {
                (Some(i), _) => Ok(i),
                (None, Some(m)) => app.history.find_mark(m).ok_or_else(|| format!("no mark {m}")),
                (None, None) => Err("pass index (a step number from history list) or mark".into()),
            }
        };
        match args["action"].as_str().unwrap_or("list") {
            "list" => {
                let mut out = self.history.list();
                let q = &self.quicksave;
                out["quicksave"] = json!({ "enabled": q.enabled, "dir": q.dir, "every_s": super::quicksave::EVERY.as_secs(), "keep": super::quicksave::KEEP, "last": q.last_path, "pending": q.pending });
                Ok(text(out))
            }
            "goto" => {
                let n = index(self)?;
                self.history_goto(n)?;
                Ok(text(json!({ "current": self.history.current(), "next": "The scene is that snapshot now; redo or history goto moves forward again until the next edit." })))
            }
            "mark" | "label" => {
                let n = match index(self) {
                    Ok(n) => n,
                    Err(_) if args["index"].is_null() && args["action"] == "mark" => self.history.current(),
                    Err(e) => return Err(e),
                };
                let name = args["name"].as_str().map(|s| s.chars().take(120).collect::<String>());
                if args["action"] == "mark" {
                    self.history.mark(n, name)?;
                } else {
                    self.history.label(n, name.ok_or("label needs name")?)?;
                }
                Ok(text(json!({ "step": n })))
            }
            "quicksave" => {
                if let Some(on) = args["enabled"].as_bool() {
                    self.quicksave.enabled = on;
                }
                let path = args["now"].as_bool().unwrap_or(args["enabled"].is_null()).then(|| self.quicksave.start(self.scene.clone()));
                Ok(text(json!({ "enabled": self.quicksave.enabled, "dir": self.quicksave.dir, "written": path, "every_s": super::quicksave::EVERY.as_secs(), "keep": super::quicksave::KEEP })))
            }
            other => Err(format!("unknown action {other}; use list, goto, mark, label or quicksave")),
        }
    }
}

/// `stroke_log` for one layer.
fn log<G: Grid>(id: u64, clay: &Clay, args: &Value) -> Result<Value, String> {
    let l = ledger::ledger::<G>(clay);
    let ctx = G::context(clay);
    let full = args["full"].as_bool().unwrap_or(false);
    let limit = args["limit"].as_u64().map_or(if full { 20 } else { 50 }, |v| v as usize).clamp(1, MAX_PAGE);
    let wanted = ids(args)?;
    let from = args["from_seq"].as_u64().map_or(1, |s| s.max(1) as usize);
    let records: Vec<Value> = l
        .records()
        .iter()
        .enumerate()
        .filter(|(i, r)| if wanted.is_empty() { i + 1 >= from } else { wanted.contains(&r.id) })
        .take(limit)
        .map(|(i, r)| ledger::record_json(r, i + 1, r.stale(ctx, clay), full))
        .collect();
    let last = records.last().and_then(|r| r["seq"].as_u64()).unwrap_or(0) as usize;
    Ok(json!({
        "target": id,
        "layer": ledger::layer_json::<G>(clay),
        "records": records,
        "total": l.len(),
        "head": clay.sculpt.is_some().then(|| l.head()),
        "next_from_seq": (wanted.is_empty() && last < l.len()).then_some(last + 1),
    }))
}

/// One probe: the field, its normal and curvature, the layer offsets and
/// the owning blob at a clay-space point; written into `v` in the call's
/// space (`world` maps clay space there, lengths divide by `scale`).
fn probe(clay: &Clay, p: Vec3, world: Mat4, scale: f32, v: &mut Value) {
    let cell = clay.detail.as_ref().map(Detail::voxel).or(clay.detail_cell).or_else(|| clay.cell_size()).unwrap_or(0.01);
    let field = Field::new(clay, cell);
    let list = field.near(p - cell * 4.0, p + cell * 4.0);
    let f = |q: Vec3| field.sample(q, &list).0;
    let d = f(p);
    let e = cell;
    let (dx, dy, dz) = (Vec3::X * e, Vec3::Y * e, Vec3::Z * e);
    let grad = Vec3::new(f(p + dx) - f(p - dx), f(p + dy) - f(p - dy), f(p + dz) - f(p - dz)) / (2.0 * e);
    let lap = (f(p + dx) + f(p - dx) + f(p + dy) + f(p - dy) + f(p + dz) + f(p - dz) - 6.0 * d) / (e * e);
    let n = grad.normalize_or_zero();
    let surface = p - n * d;
    v["hit"] = json!(true);
    v["point"] = json!(r3(world.transform_point3(surface)));
    v["distance"] = json!((d / scale * 1e6).round() / 1e6);
    v["inside"] = json!(d < 0.0);
    v["normal"] = json!(r3(world.inverse().transpose().transform_vector3(n).normalize_or_zero()));
    // Mean curvature (1 / radius; positive on bumps), from the Laplacian
    // of a distance field.
    v["curvature"] = json!(((lap * 0.5 * scale) * 1e4).round() / 1e4);
    v["mask"] = json!(clay.sculpt_mask.as_ref().map_or(0.0, |m| (m.grid().value(surface) * 1e4).round() / 1e4));
    let group = clay.groups.as_ref().map(|g| g.label(surface)).filter(|g| *g != 0);
    v["group"] = json!(group);
    if let (Some(g), Some(id)) = (&clay.groups, group) {
        v["group_visible"] = json!(g.data().info.get(&id).is_none_or(|i| i.visible));
    }
    let offset = clay.detail.as_ref().map_or(0.0, |dg| dg.grid().offset(surface));
    let mut offsets = serde_json::Map::new();
    offsets.insert("base".into(), json!((offset / scale * 1e6).round() / 1e3));
    // Each sculpt layer: what it adds there (its grid times its amount).
    for l in crate::layers::extra(clay).iter().filter(|l| l.kind == crate::layers::Kind::Sculpt) {
        let o = if l.enabled { l.amount * l.detail.as_ref().map_or(0.0, |d| d.grid().offset(surface)) } else { 0.0 };
        offsets.insert(l.name.clone(), json!((o / scale * 1e6).round() / 1e3));
    }
    v["layer_offsets"] = Value::Object(offsets);
    v["blob_owner"] = json!(field.owner_name(surface));
}

/// `sculpt_diff`'s numbers between two base grids.
fn diff_json(clay: &Clay, from: Option<&Detail>, to: Option<&Detail>, region: Option<(Vec3, f32)>, bins: usize, world: Mat4, scale: f32) -> Value {
    let mut changes: Vec<(Vec3, f32, f32)> = Vec::new();
    let bricks = ledger::detail_changes(from, to, |p, cell, u, v| {
        if region.is_none_or(|(c, r)| p.distance(c) <= r) {
            changes.push((p, cell, v - u));
        }
    });
    let n = changes.len();
    let max = changes.iter().fold(0.0f32, |m, c| m.max(c.2.abs()));
    let mean = if n > 0 { changes.iter().map(|c| c.2.abs() as f64).sum::<f64>() / n as f64 } else { 0.0 };
    let rms = if n > 0 { (changes.iter().map(|c| (c.2 as f64).powi(2)).sum::<f64>() / n as f64).sqrt() } else { 0.0 };
    let mut histogram = vec![0usize; bins];
    for c in &changes {
        let k = if max > 0.0 { ((c.2.abs() / max) * bins as f32) as usize } else { 0 };
        histogram[k.min(bins - 1)] += 1;
    }
    // Area: changed points within a cell of the new surface; a band two
    // cells thick holds about two of them per surface cell.
    let after = Clay { detail: to.cloned(), ..clay.clone() };
    let mut area = 0.0f64;
    if n > 0 && n <= 2_000_000 {
        let cell = changes[0].1;
        let field = Field::new(&after, cell);
        for (p, c, _) in &changes {
            let list = field.near(*p - c * 2.0, *p + c * 2.0);
            if field.sample(*p, &list).0.abs() <= *c {
                area += (*c as f64).powi(2) * 0.5;
            }
        }
    }
    let volume = <Detail as Grid>::stats(clay, from, to, true).volume_delta.unwrap_or(0.0);
    let (lo, hi) = changes.iter().fold((Vec3::splat(f32::MAX), Vec3::splat(f32::MIN)), |(a, b), c| (a.min(c.0), b.max(c.0)));
    let s = scale as f64;
    json!({
        "changed_points": n,
        "bricks": bricks,
        "max_disp": max / scale,
        "mean_disp": mean / s,
        "rms": rms / s,
        "volume_delta": volume as f64 / s.powi(3),
        "area_changed": area / s.powi(2),
        "bbox": (n > 0).then(|| [r3(world.transform_point3(lo)), r3(world.transform_point3(hi))]),
        "histogram": { "bins": histogram, "max": max / scale },
    })
}
