//! Agent tools for game-ready output: `bake` turns clay into a simple
//! textured mesh (see `bake.rs`), shown in place of the clay with
//! `set_display baked` or `render_views baked`, and used by both exports.

use std::time::Instant;

use serde_json::{Value, json};

use super::ClideApp;
use super::jobs::Started;
use crate::bake::{Settings, TEXTURE_SIZES};
use crate::mcp::ToolResult;
use crate::scene::Shape;

impl ClideApp {
    /// `bake`: set an object's bake settings (or turn baking off) and bake,
    /// as a background job; `reply` gets the counts and the textures.
    pub(super) fn bake_tool(&mut self, args: &Value) -> Result<Started, String> {
        let id = self.resolve(&args["target"])?;
        let o = self.scene.get(id).ok_or("object vanished")?;
        if !matches!(o.shape, Shape::Clay | Shape::Cloth) {
            return Err(format!("{} is not a clay object", o.name));
        }
        let name = o.name.clone();
        if args["off"].as_bool().unwrap_or(false) {
            self.scene.get_mut(id).expect("checked").bake = None;
            return Ok(Started::Now(Ok(super::tools::text(json!({ "target": id, "bake": null })))));
        }
        // A face head bakes at 4K by default so pores and lines survive.
        let face = o.bake.is_none() && o.clay.as_ref().is_some_and(|c| c.face().is_some());
        let mut s = o.bake.unwrap_or_default();
        if face {
            s.texture = 4096;
        }
        if let Some(t) = args["triangles"].as_u64() {
            s.triangles = t.min(u32::MAX as u64) as u32;
        }
        if let Some(t) = args["texture"].as_u64() {
            if !TEXTURE_SIZES.contains(&(t as u32)) {
                return Err(format!("texture must be one of {TEXTURE_SIZES:?}"));
            }
            s.texture = t as u32;
        }
        if let Some(d) = args["region_density"].as_f64() {
            s.region_density = d as f32;
        }
        if let Some(e) = args["exact"].as_bool() {
            s.exact = e;
        }
        s.sanitize();
        self.scene.get_mut(id).expect("checked").bake = Some(s);
        let show = args["show"].as_bool();
        let start = Instant::now();
        self.meshes.sync(&self.scene);
        let Some(job) = self.meshes.bake_job(&self.scene, id, s)? else {
            return Ok(Started::Now(self.bake_reply(id, s, false, start, show)));
        };
        Ok(Started::Job(format!("Bake {name}"), Box::new(move |progress| {
            let (mesh, skin) = job.run(progress)?;
            Ok(Box::new(move |app: &mut ClideApp| {
                if !app.meshes.install_bake(&job, mesh, skin) {
                    return Err("the clay changed while it was baking; run bake again".into());
                }
                app.bake_reply(job.id, job.settings, true, start, show)
            }) as super::jobs::Finish)
        })))
    }

    /// The `bake` reply: counts and an image of the three textures.
    fn bake_reply(&mut self, id: u64, s: Settings, fresh: bool, start: Instant, show: Option<bool>) -> ToolResult {
        if let Some(show) = show {
            self.meshes.show_baked = show;
        }
        self.meshes.sync(&self.scene);
        let (mesh, skin) = self.meshes.baked(id).ok_or("the bake is gone (the clay changed); run bake again")?;
        let (rest, _) = self.meshes.rest(id).ok_or("not a clay object")?;
        let tex = mesh.textures.clone().ok_or("baked meshes have textures")?;
        let mut out = super::tools::text(json!({
            "target": id,
            "bake": s,
            "rebaked": fresh,
            "seconds": (start.elapsed().as_secs_f32() * 10.0).round() / 10.0,
            "triangles": mesh.indices.len() / 3,
            "vertices": mesh.vertices.len(),
            "clay_triangles": rest.indices.len() / 3,
            "note": if self.scene.get(id).is_some_and(|o|o.shape==Shape::Cloth) { "Pattern UVs and skin are preserved; fine folds and weave are baked into the normal map. Lower cloth resolution if seams prevent your triangle budget." } else { "A bake cannot have more triangles than the clay mesh (clay_triangles); raise the clay resolution for a bigger budget." },
            "skinned": skin.is_some(),
            "shown_in_viewport": self.meshes.show_baked,
            "image": "The baked textures: color | normal map (bumps; flat = light blue) | AO, roughness, metallic as red, green, blue.",
        }));
        let tile = |data: &[u8]| {
            let n = tex.size as usize;
            let pixels = data.chunks_exact(4).map(|p| egui::Color32::from_rgb(p[0], p[1], p[2])).collect();
            super::tools::downscale(&egui::ColorImage::new([n, n], pixels), 512)
        };
        let sheet = super::eyes::contact_sheet(&[tile(&tex.color), tile(&tex.normal), tile(&tex.orm)], 3);
        out.extend(super::tools::encode_png(&sheet)?);
        Ok(out)
    }

}
