//! The `mask` and `group` tools (docs/SCULPT_PLAN.md 4.2): the persistent
//! sculpt mask and polygroups of a clay, edited by lists of actions. Each
//! action is a record on the clay's mask or group ledger (`ledger.rs`,
//! layers "mask" and "groups"), so stroke_log lists them and stroke_edit
//! disables, deletes or replays them. Actions are read here (the call's
//! space and named views into the clay's object space, strokes resolved)
//! and applied on a job thread; the result installs if the layer, the
//! blobs and the sculpted surface did not change meanwhile.
//!
//! The editor's controls send these same calls (the gizmo's masked
//! transform sends `sculpt {brush: transform}`).

use glam::Vec3;
use serde_json::{Value, json};

use super::ClideApp;
use super::jobs::Started;
use super::tools::text;
use crate::brush::spec::{Brush, Camera};
use crate::clay::Clay;
use crate::groups::{GroupOp, Groups};
use crate::ledger::{self, Grid, Meta};
use crate::mask::{MaskOp, Mode, Select, SculptMask};

/// Most actions one call takes.
const MAX_ACTIONS: usize = 64;

/// A mask action ready for the job.
enum MaskStep {
    Strokes(Vec<(crate::brush::spec::StrokeSpec, crate::brush::resolve::Resolved)>),
    Op(Value, MaskOp),
    /// by_groups: resolved on the job's copy (it names the groups by hash).
    ByGroups(Value, Vec<u16>, Mode),
    FromChannel(Value, String, Mode),
}

/// A group action ready for the job.
enum GroupStep {
    Strokes(Vec<(crate::brush::spec::StrokeSpec, crate::brush::resolve::Resolved)>),
    Op(Value, GroupOp),
    /// from_mask: resolved on the job's copy (it names the mask by hash).
    FromMask(Value, f32, Option<u16>),
}

/// An action as (name, arguments): "invert", {"grow": 3}, {"lasso": {..}}.
fn split_action(a: &Value) -> Result<(String, Value), String> {
    match a {
        Value::String(s) => Ok((s.clone(), Value::Null)),
        Value::Object(o) if o.len() == 1 => {
            let (k, v) = o.iter().next().expect("one");
            Ok((k.clone(), v.clone()))
        }
        _ => Err("each action is a name (\"invert\") or an object with one name ({\"grow\": 3}, {\"lasso\": {..}})".into()),
    }
}

fn mode_of(v: &Value) -> Result<Mode, String> {
    match v.get("mode") {
        None | Some(Value::Null) => Ok(Mode::Set),
        Some(m) => serde_json::from_value(m.clone()).map_err(|_| "mode must be set, add or sub".to_string()),
    }
}

fn cells_of(v: &Value, what: &str) -> Result<u32, String> {
    let n = v.as_u64().or_else(|| v["cells"].as_u64()).unwrap_or(1);
    if !(1..=crate::mask::MAX_CELLS as u64).contains(&n) {
        return Err(format!("{what} takes 1 to {} cells", crate::mask::MAX_CELLS));
    }
    Ok(n as u32)
}

fn ids_of(v: &Value) -> Result<Vec<u16>, String> {
    let list = v.get("ids").unwrap_or(v);
    let ids: Vec<u16> = list.as_array().ok_or("pass ids: [group ids]")?.iter().map(|i| i.as_u64().filter(|i| (1..=u16::MAX as u64).contains(i)).map(|i| i as u16).ok_or("group ids are whole numbers 1 to 65535")).collect::<Result<_, _>>()?;
    if ids.is_empty() || ids.len() > crate::groups::MAX_GROUPS {
        return Err(format!("pass 1 to {} group ids", crate::groups::MAX_GROUPS));
    }
    Ok(ids)
}

fn pairs(v: &Value, what: &str) -> Result<Vec<[f32; 2]>, String> {
    let list = v.as_array().ok_or(format!("{what} must be a list of [x, y] pairs"))?;
    if list.len() > crate::mask::MAX_LASSO {
        return Err(format!("{what} takes at most {} points", crate::mask::MAX_LASSO));
    }
    list.iter()
        .map(|p| {
            let a = p.as_array().filter(|a| a.len() == 2).ok_or(format!("each of {what} is [x, y]"))?;
            let (x, y) = (a[0].as_f64().ok_or("numbers")? as f32, a[1].as_f64().ok_or("numbers")? as f32);
            if !(x.is_finite() && y.is_finite()) {
                return Err(format!("{what} must be finite"));
            }
            Ok([x, y])
        })
        .collect()
}

fn vec3_of(v: &Value, key: &str) -> Result<Option<Vec3>, String> {
    super::tools::vec3(v, key)
}

impl ClideApp {
    /// A lasso or rect: its camera (world, as given or from a named view),
    /// stored with the clay's object placement folded in, and its pixels.
    /// The authored form, with the camera filled in, is returned too: sent
    /// again it selects the same, wherever the editor's camera went.
    fn lasso(&self, id: u64, name: &str, v: &Value) -> Result<(Value, MaskOp), String> {
        let mut spec = v.clone();
        let (camera, px) = match (v.get("camera").filter(|c| !c.is_null()), v.get("view").and_then(Value::as_str)) {
            (Some(c), None) => {
                let camera: Camera = serde_json::from_value(c.clone()).map_err(|e| format!("camera: {e}"))?;
                let px = match name {
                    "rect" => rect(pairs(&json!([v["from"].clone(), v["to"].clone()]), "from/to")?),
                    _ => pairs(&v["px"], "px")?,
                };
                (camera, px)
            }
            (None, Some(view)) => {
                // A fixed span: the same at always gives the same record.
                let e = 100.0;
                let (camera, to_px) = crate::mask::named_camera(view, e)?;
                let at = match name {
                    "rect" => rect(pairs(&json!([v["from"].clone(), v["to"].clone()]), "from/to")?),
                    _ => pairs(&v["at"], "at")?,
                };
                // A named view keeps its name in the record's spec; the
                // camera it stands for is fixed (not the editor's).
                (camera, at.iter().map(|a| to_px(*a, e)).collect())
            }
            _ => return Err(format!("{name} takes camera {{view, proj, viewport}} with {}, or view (front, back, right, left, top, bottom) with {}", if name == "rect" { "from and to pixels" } else { "px pixels" }, if name == "rect" { "from and to [h, v] in world units" } else { "at [h, v] points in world units" })),
        };
        if v.get("camera").is_some_and(|c| !c.is_null()) {
            spec["camera"] = serde_json::to_value(camera).map_err(|e| e.to_string())?;
        }
        let through = v["through"].as_bool().unwrap_or(true);
        let local = Camera { view: camera.view * self.scene.world_matrix(id), ..camera };
        let op = MaskOp::Select { select: Select::Lasso { camera: local, px, through }, mode: mode_of(v)? };
        op.validate()?;
        Ok((spec, op))
    }

    /// The mask actions of a call, read and resolved (strokes snapped,
    /// points and directions into the clay's space).
    fn mask_steps(&mut self, id: u64, args: &Value) -> Result<(Vec<MaskStep>, Option<crate::brush::alpha::AlphaStore>), String> {
        let actions = args["actions"].as_array().ok_or("actions must be a list")?;
        if actions.is_empty() || actions.len() > MAX_ACTIONS {
            return Err(format!("pass 1 to {MAX_ACTIONS} actions"));
        }
        let (to_local, radius_scale) = self.clay_space(id, args)?;
        let world = to_local.inverse();
        let clay = self.scene.get(id).and_then(|o| o.clay.clone()).ok_or("not a clay object")?;
        let voxel = crate::mask::voxel_of(&clay)?;
        let mut steps = Vec::new();
        let mut alphas = None;
        // Channels to_channel adds (the cap holds for the whole call, and
        // from_channel can read one written earlier in it).
        let mut added = std::collections::BTreeSet::new();
        for (n, a) in actions.iter().enumerate() {
            let fail = |e: String| format!("action {n}: {e}");
            let (name, v) = split_action(a).map_err(fail)?;
            let spec = a.clone();
            let op = |op: MaskOp| op.validate().map(|()| MaskStep::Op(spec.clone(), op)).map_err(fail);
            let step = match name.as_str() {
                "paint" => {
                    let raws = v.as_array().ok_or("paint takes a list of strokes").map_err(|e| fail(e.into()))?;
                    if raws.is_empty() || raws.len() > 64 {
                        return Err(fail("paint takes 1 to 64 strokes".into()));
                    }
                    let mut raws = raws.iter().map(|r| crate::brush::preset::merge(&self.scene.brushes, None, r)).collect::<Result<Vec<_>, _>>().map_err(fail)?;
                    for r in &mut raws {
                        if r.get("brush").is_some_and(|b| b != "mask_paint") {
                            return Err(fail("mask strokes take brush mask_paint (or none)".into()));
                        }
                        r["brush"] = json!("mask_paint");
                    }
                    self.meshes.sync(&self.scene);
                    let strokes = self.resolve_strokes(id, args, &raws, Some(Brush::MaskPaint), to_local, radius_scale).map_err(fail)?;
                    if let Some(store) = self.alphas_for(id, &strokes).map_err(fail)? {
                        alphas = Some(store);
                    }
                    MaskStep::Strokes(strokes)
                }
                "lasso" | "rect" => {
                    let (spec, op) = self.lasso(id, &name, &v).map_err(fail)?;
                    MaskStep::Op(json!({ name.clone(): spec }), op)
                }
                "invert" => op(MaskOp::Invert)?,
                "clear" => op(MaskOp::Clear)?,
                "all" => op(MaskOp::All)?,
                "grow" => op(MaskOp::Grow { cells: cells_of(&v, "grow").map_err(fail)? })?,
                "shrink" => op(MaskOp::Shrink { cells: cells_of(&v, "shrink").map_err(fail)? })?,
                "blur" => op(MaskOp::Blur { cells: cells_of(&v, "blur").map_err(fail)? })?,
                "sharpen" => op(MaskOp::Sharpen { amount: v.as_f64().or_else(|| v["amount"].as_f64()).unwrap_or(1.0) as f32 })?,
                "by_cavity" | "by_curvature" => {
                    let radius = v["radius"].as_f64().map_or(2.0 * voxel, |r| r as f32 * radius_scale);
                    let select = if name == "by_cavity" { Select::Cavity { radius } } else { Select::Curvature { radius } };
                    op(MaskOp::Select { select, mode: mode_of(&v).map_err(fail)? })?
                }
                "by_normal" => {
                    let dir = vec3_of(&v, "dir").map_err(fail)?.ok_or("by_normal needs dir [x, y, z]").map_err(|e| fail(e.into()))?;
                    let angle = v["angle"].as_f64().unwrap_or(30.0) as f32;
                    op(MaskOp::Select { select: Select::Normal { dir: to_local.transform_vector3(dir).normalize_or_zero(), angle }, mode: mode_of(&v).map_err(fail)? })?
                }
                "by_height" => {
                    let axis = match &v["axis"] {
                        Value::Null => Vec3::Y,
                        Value::String(s) => match s.as_str() {
                            "x" => Vec3::X,
                            "y" => Vec3::Y,
                            "z" => Vec3::Z,
                            _ => return Err(fail("axis is x, y, z or [x, y, z]".into())),
                        },
                        _ => vec3_of(&v, "axis").map_err(fail)?.ok_or("axis").map_err(|e| fail(e.into()))?.normalize_or_zero(),
                    };
                    let (min, max) = (v["min"].as_f64().map_or(f32::MIN, |x| x as f32), v["max"].as_f64().map_or(f32::MAX, |x| x as f32));
                    // A height along a call-space axis, as a linear function of
                    // object-space points.
                    let (along, offset) = (world.transpose().transform_vector3(axis), axis.dot(world.w_axis.truncate()));
                    op(MaskOp::Select { select: Select::Height { along, offset, min: min.max(-1e30), max: max.min(1e30) }, mode: mode_of(&v).map_err(fail)? })?
                }
                "by_blobs" => {
                    let names: Vec<String> = v.get("names").unwrap_or(&v).as_array().ok_or("by_blobs takes names: [blob names]").map_err(|e| fail(e.into()))?.iter().filter_map(|n| n.as_str().map(str::to_string)).collect();
                    op(MaskOp::Select { select: Select::Blobs { names }, mode: mode_of(&v).map_err(fail)? })?
                }
                "by_groups" => MaskStep::ByGroups(spec.clone(), ids_of(&v).map_err(fail)?, mode_of(&v).map_err(fail)?),
                "connected" => {
                    let seed = vec3_of(&v, "seed_point").map_err(fail)?.or(vec3_of(&v, "seed").map_err(fail)?).ok_or("connected needs seed_point [x, y, z]").map_err(|e| fail(e.into()))?;
                    op(MaskOp::Select { select: Select::Connected { seed: to_local.transform_point3(seed) }, mode: mode_of(&v).map_err(fail)? })?
                }
                "from_channel" => {
                    let c = v.as_str().or(v["name"].as_str()).ok_or("from_channel needs name: a mask channel (paint channel)").map_err(|e| fail(e.into()))?;
                    if !clay.masks.contains_key(c) && !added.contains(c) {
                        return Err(fail(format!("no mask channel {c} (has {:?})", clay.masks.keys().chain(&added).collect::<Vec<_>>())));
                    }
                    MaskStep::FromChannel(spec.clone(), c.to_string(), mode_of(&v).map_err(fail)?)
                }
                "to_channel" => {
                    let c = v.as_str().or(v["name"].as_str()).map(str::trim).ok_or("to_channel needs name: the mask channel to write").map_err(|e| fail(e.into()))?;
                    if !clay.masks.contains_key(c) && added.insert(c.to_string()) && clay.masks.len() + added.len() > crate::smart::MAX_CHANNELS {
                        return Err(fail(format!("at most {} mask channels per clay", crate::smart::MAX_CHANNELS)));
                    }
                    op(MaskOp::ToChannel { name: c.to_string() })?
                }
                other => return Err(fail(format!("unknown mask action {other}; use paint, lasso, rect, invert, clear, all, grow, shrink, blur, sharpen, by_cavity, by_curvature, by_normal, by_height, by_blobs, by_groups, connected, from_channel or to_channel"))),
            };
            steps.push(step);
        }
        Ok((steps, alphas))
    }

    /// `mask`: the actions in order, each a record on the mask ledger.
    pub(super) fn mask_tool(&mut self, args: &Value) -> Result<Started, String> {
        let (id, _) = self.clay_of(args)?;
        self.settle_clays([id])?;
        let (steps, alphas) = self.mask_steps(id, args)?;
        let label = args["label"].as_str().map(|l| l.chars().take(120).collect::<String>());
        let o = self.scene.get(id).ok_or("object vanished")?;
        let (before, name) = (o.clay.clone().unwrap_or_default(), o.name.clone());
        self.meshes.sync(&self.scene);
        let mesh = self.meshes.rest(id).map(|m| m.0.clone());
        let cache = self.replay_cache.clone();
        let meta = Meta::by(self.call_source, "mask", label).pressed(args)?;
        Ok(Started::Job(
            format!("Mask {name}"),
            Box::new(move |progress| {
                let mut clay = before.clone();
                ledger::settle_clay(&mut clay, Some(&cache))?;
                if let Some(store) = alphas {
                    ledger::set_alphas(&mut clay, store);
                }
                let mut records = Vec::new();
                let mut channels: Vec<String> = Vec::new();
                let n = steps.len();
                for (k, step) in steps.into_iter().enumerate() {
                    progress.check()?;
                    progress.set(k as f32 / n as f32);
                    let fail = |e: String| format!("action {k}: {e} (no action was applied)");
                    let mut put = |clay: &mut Clay, c: ledger::Committed<SculptMask>, what: &str| {
                        records.push(json!({ "action": k, "what": what, "id": c.record.id, "changed_points": c.record.stats.changed_points }));
                        ledger::install(clay, c.ledger, c.grid);
                    };
                    match step {
                        MaskStep::Strokes(strokes) => {
                            for (spec, s) in strokes {
                                let c = ledger::commit::<SculptMask>(&clay, &ledger::ledger(&clay), &meta, &spec, &s, false, Some(&cache)).map_err(fail)?;
                                put(&mut clay, c, "paint");
                            }
                        }
                        MaskStep::Op(spec, op) => {
                            let what = action_name(&spec);
                            let channel = match &op {
                                MaskOp::ToChannel { name } => Some(name.clone()),
                                _ => None,
                            };
                            let c = ledger::commit_action::<SculptMask>(&clay, &ledger::ledger(&clay), &meta, spec, op, Some(&cache)).map_err(fail)?;
                            put(&mut clay, c, &what);
                            // The mask as it is at this action goes to the
                            // channel (on the copy; Finish copies it over).
                            if let Some(name) = channel {
                                to_channel(&mut clay, &name).map_err(fail)?;
                                channels.push(name);
                            }
                        }
                        MaskStep::ByGroups(spec, ids, mode) => {
                            let groups = clay.groups.clone().ok_or("by_groups: the clay has no polygroups (group from_blobs, from_mask, paint ...)").map_err(|e| fail(e.into()))?;
                            let hash = Groups::hash(Some(&groups));
                            ledger::keep_frozen(&mut clay, None, Some(groups)).map_err(fail)?;
                            let op = MaskOp::Select { select: Select::Groups { ids, groups: hash }, mode };
                            let c = ledger::commit_action::<SculptMask>(&clay, &ledger::ledger(&clay), &meta, spec, op, Some(&cache)).map_err(fail)?;
                            put(&mut clay, c, "by_groups");
                        }
                        MaskStep::FromChannel(spec, name, mode) => {
                            // A painted channel has no ledger: the result is kept whole.
                            let grid = from_channel(&clay, &name, mode).map_err(fail)?;
                            let _ = spec;
                            let c = ledger::commit_grid::<SculptMask>(&clay, &ledger::ledger(&clay), &meta, &format!("from channel {name}"), grid, Some(&cache)).map_err(fail)?;
                            put(&mut clay, c, "from_channel");
                        }
                    }
                }
                let coverage = mesh.as_ref().map(|m| crate::mask::coverage(clay.sculpt_mask.as_ref(), m));
                Ok(Box::new(move |app: &mut ClideApp| {
                    let o = app.scene.get_mut(id).ok_or("the object was deleted meanwhile")?;
                    let cur = o.clay.as_mut().ok_or("the object is no longer clay")?;
                    let head = |c: &Clay| c.sculpt.as_ref().map(|s| s.data().mask.head());
                    if head(cur) != head(&before) || cur.sculpt_mask != before.sculpt_mask || SculptMask::context(cur) != SculptMask::context(&before) {
                        return Err("the clay's mask, blobs or sculpt changed meanwhile; nothing was changed, send the actions again".into());
                    }
                    channels.sort();
                    channels.dedup();
                    if cur.masks.len() + channels.iter().filter(|c| !cur.masks.contains_key(*c)).count() > crate::smart::MAX_CHANNELS {
                        return Err(format!("at most {} mask channels per clay (another call added some meanwhile); nothing was changed", crate::smart::MAX_CHANNELS));
                    }
                    ledger::install(cur, ledger::ledger::<SculptMask>(&clay), clay.sculpt_mask.clone());
                    ledger::set_alphas(cur, clay.sculpt.as_ref().map(|s| s.data().alphas.clone()).unwrap_or_default());
                    ledger::merge_frozen(cur, &clay);
                    ledger::adopt_settled(cur, &before, &clay);
                    for name in channels {
                        if let Some(p) = clay.masks.get(&name) {
                            cur.masks.insert(name, p.clone());
                        }
                    }
                    let mut out = app.mask_json(id);
                    out["record_ids"] = json!(records.iter().map(|r| r["id"].clone()).collect::<Vec<_>>());
                    out["records"] = json!(records);
                    out["coverage"] = json!(coverage.map(|c| (c * 1e4).round() / 1e4));
                    out["step"] = json!(app.history.current() + 1);
                    Ok(text(out))
                }) as super::jobs::Finish)
            }),
        ))
    }

    /// The mask's summary (get_clay, mask replies).
    pub(super) fn mask_json(&self, id: u64) -> Value {
        let Some(clay) = self.scene.get(id).and_then(|o| o.clay.as_ref()) else { return Value::Null };
        let m = clay.sculpt_mask.as_ref();
        let (lo, hi) = m.into_iter().flat_map(|m| m.grid().values().map(|(i, _)| glam::IVec3::from(i).as_vec3() * m.grid().voxel)).fold((Vec3::splat(f32::MAX), Vec3::splat(f32::MIN)), |(a, b), p| (a.min(p), b.max(p)));
        let coverage = self.meshes.rest(id).map(|r| crate::mask::coverage(m, r.0));
        json!({
            "target": id,
            "layer": "mask",
            "coverage": coverage.map(|c| (c * 1e4).round() / 1e4),
            "bricks": m.map_or(0, |m| m.grid().brick_count()),
            "bbox": (lo.x <= hi.x).then(|| [lo.to_array(), hi.to_array()]),
            "cell": m.map(|m| m.grid().voxel),
            "ledger_head": clay.sculpt.as_ref().filter(|s| !s.data().mask.is_empty()).map(|s| s.data().mask.head()),
        })
    }

    /// The group actions of a call, read and resolved.
    fn group_steps(&mut self, id: u64, args: &Value) -> Result<(Vec<GroupStep>, Option<crate::brush::alpha::AlphaStore>), String> {
        let actions = args["actions"].as_array().ok_or("actions must be a list")?;
        if actions.is_empty() || actions.len() > MAX_ACTIONS {
            return Err(format!("pass 1 to {MAX_ACTIONS} actions"));
        }
        let (to_local, radius_scale) = self.clay_space(id, args)?;
        let mut steps = Vec::new();
        let mut alphas = None;
        for (n, a) in actions.iter().enumerate() {
            let fail = |e: String| format!("action {n}: {e}");
            let (name, v) = split_action(a).map_err(fail)?;
            let spec = a.clone();
            let op = |op: GroupOp| op.validate().map(|()| GroupStep::Op(spec.clone(), op)).map_err(fail);
            let step = match name.as_str() {
                "list" => continue,
                "from_blobs" => op(GroupOp::FromBlobs)?,
                "from_normals" => op(GroupOp::FromNormals { angle: v.as_f64().or_else(|| v["angle"].as_f64()).unwrap_or(30.0) as f32 })?,
                "from_mask" => {
                    let id = v["id"].as_u64().map(|i| u16::try_from(i).map_err(|_| "ids are 1 to 65535".to_string())).transpose().map_err(fail)?;
                    GroupStep::FromMask(spec.clone(), v["threshold"].as_f64().unwrap_or(0.5) as f32, id)
                }
                "paint" => {
                    let gid = v["id"].as_u64().filter(|i| (1..=u16::MAX as u64).contains(i)).ok_or("paint needs id: the group (1 to 65535)").map_err(|e| fail(e.into()))? as u16;
                    let raws = v["strokes"].as_array().ok_or("paint needs strokes").map_err(|e| fail(e.into()))?;
                    if raws.is_empty() || raws.len() > 64 {
                        return Err(fail("paint takes 1 to 64 strokes".into()));
                    }
                    let mut raws = raws.iter().map(|r| crate::brush::preset::merge(&self.scene.brushes, None, r)).collect::<Result<Vec<_>, _>>().map_err(fail)?;
                    for r in &mut raws {
                        if r.get("brush").is_some_and(|b| b != "group_paint") {
                            return Err(fail("group strokes take brush group_paint (or none)".into()));
                        }
                        r["brush"] = json!("group_paint");
                    }
                    self.meshes.sync(&self.scene);
                    let mut strokes = self.resolve_strokes(id, args, &raws, Some(Brush::GroupPaint), to_local, radius_scale).map_err(fail)?;
                    for (_, s) in &mut strokes {
                        s.group = Some(gid);
                    }
                    if let Some(store) = self.alphas_for(id, &strokes).map_err(fail)? {
                        alphas = Some(store);
                    }
                    GroupStep::Strokes(strokes)
                }
                "rename" => op(GroupOp::Rename { id: ids_of(&json!([v["id"].clone()])).map_err(fail)?[0], name: v["name"].as_str().unwrap_or_default().to_string() })?,
                "show" => op(GroupOp::Show { ids: ids_of(&v).map_err(fail)? })?,
                "hide" => op(GroupOp::Hide { ids: ids_of(&v).map_err(fail)? })?,
                "isolate" => op(GroupOp::Isolate { ids: ids_of(&v).map_err(fail)? })?,
                "show_all" => op(GroupOp::ShowAll)?,
                "clear" => op(GroupOp::Clear)?,
                other => return Err(fail(format!("unknown group action {other}; use from_blobs, from_mask, from_normals, paint, rename, show, hide, isolate, show_all, clear or list"))),
            };
            steps.push(step);
        }
        Ok((steps, alphas))
    }

    /// `group`: the actions in order, each a record on the group ledger;
    /// replies with the groups (`list` alone answers at once).
    pub(super) fn group_tool(&mut self, args: &Value) -> Result<Started, String> {
        let (id, _) = self.clay_of(args)?;
        self.settle_clays([id])?;
        let (steps, alphas) = self.group_steps(id, args)?;
        if steps.is_empty() {
            return Ok(Started::Now(Ok(text(self.groups_json(id)))));
        }
        let label = args["label"].as_str().map(|l| l.chars().take(120).collect::<String>());
        let o = self.scene.get(id).ok_or("object vanished")?;
        let (before, name) = (o.clay.clone().unwrap_or_default(), o.name.clone());
        let cache = self.replay_cache.clone();
        let meta = Meta::by(self.call_source, "group", label).pressed(args)?;
        Ok(Started::Job(
            format!("Groups {name}"),
            Box::new(move |progress| {
                let mut clay = before.clone();
                ledger::settle_clay(&mut clay, Some(&cache))?;
                if let Some(store) = alphas {
                    ledger::set_alphas(&mut clay, store);
                }
                let mut records = Vec::new();
                let n = steps.len();
                for (k, step) in steps.into_iter().enumerate() {
                    progress.check()?;
                    progress.set(k as f32 / n as f32);
                    let fail = |e: String| format!("action {k}: {e} (no action was applied)");
                    let mut put = |clay: &mut Clay, c: ledger::Committed<Groups>, what: &str| {
                        records.push(json!({ "action": k, "what": what, "id": c.record.id, "changed_points": c.record.stats.changed_points }));
                        ledger::install(clay, c.ledger, c.grid);
                    };
                    match step {
                        GroupStep::Strokes(strokes) => {
                            for (spec, s) in strokes {
                                let c = ledger::commit::<Groups>(&clay, &ledger::ledger(&clay), &meta, &spec, &s, false, Some(&cache)).map_err(fail)?;
                                put(&mut clay, c, "paint");
                            }
                        }
                        GroupStep::Op(spec, op) => {
                            let what = action_name(&spec);
                            let c = ledger::commit_action::<Groups>(&clay, &ledger::ledger(&clay), &meta, spec, op, Some(&cache)).map_err(fail)?;
                            put(&mut clay, c, &what);
                        }
                        GroupStep::FromMask(spec, threshold, gid) => {
                            let mask = clay.sculpt_mask.clone().ok_or("from_mask: the clay has no mask").map_err(|e| fail(e.into()))?;
                            let hash = SculptMask::hash(Some(&mask));
                            ledger::keep_frozen(&mut clay, Some(mask), None).map_err(fail)?;
                            let op = GroupOp::FromMask { threshold, id: gid, mask: hash };
                            let c = ledger::commit_action::<Groups>(&clay, &ledger::ledger(&clay), &meta, spec, op, Some(&cache)).map_err(fail)?;
                            put(&mut clay, c, "from_mask");
                        }
                    }
                }
                Ok(Box::new(move |app: &mut ClideApp| {
                    let o = app.scene.get_mut(id).ok_or("the object was deleted meanwhile")?;
                    let cur = o.clay.as_mut().ok_or("the object is no longer clay")?;
                    let head = |c: &Clay| c.sculpt.as_ref().map(|s| s.data().groups.head());
                    if head(cur) != head(&before) || cur.groups != before.groups || Groups::context(cur) != Groups::context(&before) {
                        return Err("the clay's groups, blobs or sculpt changed meanwhile; nothing was changed, send the actions again".into());
                    }
                    ledger::install(cur, ledger::ledger::<Groups>(&clay), clay.groups.clone());
                    ledger::set_alphas(cur, clay.sculpt.as_ref().map(|s| s.data().alphas.clone()).unwrap_or_default());
                    ledger::merge_frozen(cur, &clay);
                    ledger::adopt_settled(cur, &before, &clay);
                    let mut out = app.groups_json(id);
                    out["record_ids"] = json!(records.iter().map(|r| r["id"].clone()).collect::<Vec<_>>());
                    out["records"] = json!(records);
                    out["step"] = json!(app.history.current() + 1);
                    Ok(text(out))
                }) as super::jobs::Finish)
            }),
        ))
    }

    /// The clay's groups: id, name, visibility and area (on the rest mesh).
    pub(super) fn groups_json(&self, id: u64) -> Value {
        let Some(clay) = self.scene.get(id).and_then(|o| o.clay.as_ref()) else { return Value::Null };
        let Some(g) = &clay.groups else { return json!({ "target": id, "groups": [] }) };
        let mut area: std::collections::BTreeMap<u16, f64> = std::collections::BTreeMap::new();
        if let Some((mesh, _)) = self.meshes.rest(id) {
            let labels: Vec<u16> = mesh.vertices.iter().map(|v| g.label(Vec3::from(v.pos))).collect();
            for t in mesh.indices.chunks_exact(3) {
                let [a, b, c] = [t[0], t[1], t[2]].map(|i| i as usize);
                let p = |i: usize| Vec3::from(mesh.vertices[i].pos);
                let s = ((p(b) - p(a)).cross(p(c) - p(a)).length() / 6.0) as f64;
                for i in [a, b, c] {
                    *area.entry(labels[i]).or_insert(0.0) += s;
                }
            }
        }
        let groups: Vec<Value> = g.data().info.iter().map(|(gid, info)| json!({ "id": gid, "name": info.name, "visible": info.visible, "area": area.get(gid).map(|a| (a * 1e6).round() / 1e6) })).collect();
        json!({ "target": id, "layer": "groups", "groups": groups, "hidden": g.hidden(), "cell": g.data().voxel, "ledger_head": clay.sculpt.as_ref().filter(|s| !s.data().groups.is_empty()).map(|s| s.data().groups.head()) })
    }
}

/// A rect's four corners from its two.
fn rect(c: Vec<[f32; 2]>) -> Vec<[f32; 2]> {
    let ([a, b], [x, y]) = (c[0], c[1]);
    vec![[a, b], [x, b], [x, y], [a, y]]
}

fn action_name(spec: &Value) -> String {
    split_action(spec).map(|(n, _)| n).unwrap_or_default()
}

/// A mask from a smart-material channel's coverage, combined by `mode`.
fn from_channel(clay: &Clay, name: &str, mode: Mode) -> Result<Option<SculptMask>, String> {
    let channel = clay.masks.get(name).ok_or(format!("no mask channel {name}"))?;
    let voxel = crate::mask::voxel_of(clay)?;
    let band = crate::band::Band::of(clay, voxel)?;
    let old = clay.sculpt_mask.clone().unwrap_or_else(|| SculptMask::new(voxel));
    let values: Vec<u8> = band
        .keys
        .iter()
        .enumerate()
        .map(|(k, i)| {
            let s = channel.grid().coverage(band.point(k));
            let m = old.grid().get(*i);
            match mode {
                Mode::Set => crate::mask::q(s),
                Mode::Add => m.max(crate::mask::q(s)),
                Mode::Sub => crate::mask::q(m as f32 / 255.0 * (1.0 - s)),
            }
        })
        .collect();
    Ok(crate::mask::write(old, &band, &values))
}

/// Writes the clay's mask into smart-material channel `name` (white,
/// coverage = mask).
fn to_channel(clay: &mut Clay, name: &str) -> Result<(), String> {
    let voxel = crate::mask::voxel_of(clay)?;
    let mut paint = crate::paint::Paint::new(voxel);
    if let Some(m) = &clay.sculpt_mask {
        let g = paint.grid_mut();
        for (i, v) in m.grid().values() {
            g.set(i, [255, 255, 255, v]);
        }
    }
    if paint.grid().brick_count() > crate::clay::MAX_BRICKS {
        return Err("the mask is too large for a channel".into());
    }
    if !clay.masks.contains_key(name) && clay.masks.len() >= crate::smart::MAX_CHANNELS {
        return Err(format!("at most {} mask channels per clay", crate::smart::MAX_CHANNELS));
    }
    clay.masks.insert(name.to_string(), paint);
    Ok(())
}

#[cfg(test)]
mod tests;
