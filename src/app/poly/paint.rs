//! `paint` on a Poly (docs/QUALITY_PLAN.md Stage 4b): strokes resolved
//! as clay paint strokes are (frames, snapping, pressure, symmetry, on
//! the Poly's shown level), laid into its UDIM tiles and kept as records
//! (`crate::poly::texture::paint`).


use glam::Vec3;
use serde_json::{Value, json};

use super::super::ClideApp;
use super::super::jobs::{Finish, Started};
use super::super::tools::{object_json_in, text};
use crate::poly::texture::paint::{Surface, paint_poly_with};
use crate::poly::texture::{Channel, DEFAULT_RESOLUTION, check_resolution};

/// Most strokes one call may paint.
const MAX_STROKES: usize = 64;

impl ClideApp {
    /// `paint` with a Poly target: clear, then strokes (see the module
    /// doc). Refused before anything changes: no UVs, a channel strokes do
    /// not paint, a resolution past 8192 or unlike the channel's, alphas,
    /// masks, layers and fills (clay paint's), or too many strokes.
    /// A background job (it may paint millions of texels): the strokes
    /// resolve here, the texels on the job's thread, and the Poly takes
    /// them if it is as the call found it.
    pub(in crate::app) fn poly_paint(&mut self, id: u64, args: &Value) -> Result<Started, String> {
        let o = self.scene.get(id).ok_or("object vanished")?;
        let name = o.name.clone();
        let poly = o.poly.clone().ok_or_else(|| format!("{name} is not a Poly object"))?;
        for key in ["layer", "fill", "fill_strength", "label"] {
            if args.get(key).is_some_and(|v| !v.is_null()) {
                return Err(format!("{key} is for clay paint; a Poly paints its UDIM tiles (channel, resolution, strokes, clear)"));
            }
        }
        let mut notes: Vec<String> = Vec::new();
        match args.get("pressure_source").filter(|v| !v.is_null()) {
            None => {}
            Some(v) if matches!(v.as_str(), Some("touch_force" | "nsevent" | "velocity")) => notes.push("pressure_source is not kept on a Poly's paint records".into()),
            Some(_) => return Err("pressure_source must be touch_force, nsevent or velocity".into()),
        }
        if poly.uv.is_none() {
            return Err("painting a Poly needs UVs: run poly uv first (udim: true spreads them over tiles)".into());
        }
        let channel = match args.get("channel").filter(|v| !v.is_null()) {
            None => Channel::Color,
            Some(v) => v.as_str().and_then(Channel::parse).filter(|c| c.paintable()).ok_or("on a Poly, channel is color, roughness or mask (normal and ao come from bake udim: true)")?,
        };
        let had = poly.paint.as_ref().and_then(|p| p.textures.resolution(channel));
        let resolution = match args.get("resolution").filter(|v| !v.is_null()) {
            None => had.unwrap_or(DEFAULT_RESOLUTION),
            Some(v) => {
                let r = v.as_u64().filter(|r| *r <= u32::MAX as u64).ok_or("resolution must be a whole number of texels per tile side")? as u32;
                check_resolution(r)?;
                r
            }
        };
        let value = match args.get("value").filter(|v| !v.is_null()) {
            None => None,
            Some(v) => Some(v.as_f64().filter(|x| (0.0..=1.0).contains(x)).ok_or("value must be 0-1 (the roughness or mask a stroke lays)")? as f32),
        };
        if channel == Channel::Color && value.is_some() {
            return Err("value is for the roughness and mask channels; color strokes take color".into());
        }
        let clear = args["clear"].as_bool().unwrap_or(false);
        let strokes = args["strokes"].as_array().map(Vec::as_slice).unwrap_or_default();
        if strokes.len() > MAX_STROKES {
            return Err(format!("at most {MAX_STROKES} strokes per call"));
        }
        if let Some(r) = had
            && r != resolution
            && !clear
        {
            return Err(format!("the {} channel is {r} texels a tile; paint it at that resolution (or clear: true first)", channel.name()));
        }
        let mut raws = strokes.to_vec();
        for (n, raw) in raws.iter_mut().enumerate() {
            for key in ["mask", "alpha"] {
                if raw.get(key).is_some_and(|v| !v.is_null()) {
                    return Err(format!("stroke {n}: {key} is not supported on a Poly yet (clay paint takes it)"));
                }
            }
            if channel.scalar() {
                if value.is_none() && !raw["erase"].as_bool().unwrap_or(false) {
                    return Err(format!("stroke {n}: a {} stroke needs value (0-1) on the call, or erase: true", channel.name()));
                }
                // The scalar rides as the stroke's color (the resolver
                // needs one); the record keeps the value.
                if raw.get("color").is_none_or(Value::is_null) {
                    raw["color"] = json!([1.0, 1.0, 1.0]);
                }
            }
        }
        let (to_local, radius_scale) = self.clay_space(id, args)?;
        self.meshes.sync(&self.scene);
        let surface = Surface::new(&poly, poly.level)?;
        let mesh = self.meshes.get(self.scene.get(id).ok_or("object vanished")?).cloned().ok_or("the Poly has no mesh yet")?;
        let caster = self.poly_caster(id)?;
        // Strokes snap to the shown level and screen rays hit its drawn
        // mesh (the same surface), in its object space.
        let snap = |p: Vec3| surface.snap(p);
        let cast = |o: Vec3, d: Vec3| caster(o, d);
        let center = {
            let (lo, hi) = mesh.bounds();
            (!mesh.vertices.is_empty()).then_some((lo + hi) * 0.5)
        };
        let ctx = crate::brush::resolve::ResolveCtx {
            to_local,
            radius_scale,
            world_to_local: self.scene.world_matrix(id).inverse(),
            snap: Some(&snap),
            cast: Some(&cast),
            view_far: self.scene_sphere().1 * 4.0 + 10.0,
            snap_call: args["snap"].as_bool(),
            snap_single: true,
            alpha: None,
            unpose: None,
            levels: None,
            center,
        };
        let resolved = crate::sculpt::prepare(&raws, Some(crate::sculpt::Brush::Paint), &ctx)?;
        let mut strokes = Vec::with_capacity(resolved.len());
        for (n, (spec, r)) in resolved.into_iter().enumerate() {
            if spec.brush != crate::sculpt::Brush::Paint {
                return Err(format!("stroke {n}: paint strokes take no brush"));
            }
            strokes.push(r);
        }
        let before = poly.clone();
        let label = format!("Paint {name}");
        Ok(Started::Job(label, Box::new(move |progress| {
            let painted = paint_poly_with(&before, &surface, channel, resolution, value, clear, strokes, Some(progress))?;
            let (next, touched, ids) = (painted.poly, painted.touched, painted.ids);
            let t = next.paint.as_ref().map(|p| &p.textures);
            let mut reply = json!({
                "target": id,
                "channel": channel.name(),
                "resolution": resolution,
                "texels_painted": touched,
                "record_ids": ids,
                "records": next.paint.as_ref().map_or(0, |p| p.records.len()),
                "tiles": t.map(|t| t.tiles(channel)).unwrap_or_default(),
                "pages": t.map_or(0, |t| t.pages()),
                "texture_mb": t.map_or(0.0, |t| (t.bytes() as f64 / 1048576.0 * 10.0).round() / 10.0),
                "hash": next.hash(),
                "notes": notes,
            });
            Ok(Box::new(move |app: &mut ClideApp| {
                if app.scene.get(id).and_then(|o| o.poly.as_ref()) != Some(&before) {
                    return Err("the Poly changed while it was painting; nothing was changed, send the call again".into());
                }
                reply["object"] = object_json_in(&app.scene, id);
                reply["step"] = json!(app.history.current() + 1);
                app.scene.get_mut(id).expect("checked").poly = Some(next);
                Ok(text(reply))
            }) as Finish)
        })))
    }
}
