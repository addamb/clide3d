//! Garment creation/drape and animation are atomic cancellable scene jobs.
use super::{ClideApp, jobs::Started};
use crate::{
    cloth::{self, Cloth, Pattern},
    scene::Shape,
};
use serde_json::{Value, json};
impl ClideApp {
    pub(super) fn cloth_tool(&mut self, args: &Value) -> Result<Started, String> {
        let allowed = [
            "target",
            "parent",
            "name",
            "color",
            "material",
            "seconds",
            "frames",
            "fps",
            "clip",
            "pattern",
            "length",
            "fit",
            "flare",
            "gather",
            "resolution",
            "thickness",
            "fabric",
            "bone",
            "center",
            "radius",
            "points",
            "pins",
            "belt",
            "neckline",
            "sleeves",
            "subdivision",
        ];
        for key in args.as_object().ok_or("cloth expects an object")?.keys() {
            if !allowed.contains(&key.as_str()) {
                return Err(format!("unknown cloth key {key}"));
            }
        }
        let target = args.get("target").map(|v| self.resolve(v)).transpose()?;
        let parent = match target {
            Some(id) => self
                .scene
                .get(id)
                .and_then(|o| o.parent)
                .ok_or("cloth target needs a parent body")?,
            None => self.resolve(&args["parent"])?,
        };
        let body = self.scene.get(parent).ok_or("body vanished")?;
        let clay = body.clay.clone().ok_or("cloth parent must be clay")?;
        let mut rig = body.rig.clone();
        if let Some(clip) = args["clip"].as_str() {
            let r = rig.as_mut().ok_or("animation needs a rig")?;
            if !r.clips.iter().any(|c| c.name == clip) {
                return Err(format!("no clip {clip}"));
            }
            r.active = Some(clip.into());
        }
        let (body_mesh, skin) = self.meshes.rest(parent).ok_or("body mesh not ready")?;
        let (body_mesh, skin) = (body_mesh.clone(), skin.cloned());
        let existing = target
            .map(|id| {
                self.scene
                    .get(id)
                    .and_then(|o| o.cloth.clone())
                    .ok_or("target is not cloth")
            })
            .transpose()?;
        let pattern_keys = [
            "pattern",
            "length",
            "fit",
            "flare",
            "gather",
            "resolution",
            "thickness",
            "fabric",
            "bone",
            "center",
            "radius",
            "points",
            "pins",
            "belt",
            "neckline",
            "sleeves",
            "subdivision",
        ];
        let mut params = existing
            .as_ref()
            .map_or(serde_json::to_value(Pattern::default()).unwrap(), |c| {
                serde_json::to_value(&c.settings).unwrap()
            });
        let mut regenerate = existing.is_none();
        for key in pattern_keys {
            if let Some(value) = args.get(key) {
                params[key] = value.clone();
                regenerate = true;
            }
        }
        if existing.is_none() && args.get("length").is_none() {
            if params["pattern"] == "shirt" {
                params["length"] = json!("hip");
            }
            if params["pattern"] == "tube"
                && let Some(bone) = params["bone"].as_str().and_then(|name| {
                    rig.as_ref()
                        .and_then(|r| r.bones.iter().find(|b| b.name == name))
                })
            {
                params["length"] = json!(bone.head.distance(bone.tail));
            }
        }
        let settings: Pattern = serde_json::from_value(params).map_err(|e| e.to_string())?;
        settings.validate()?;
        let seconds = args["seconds"].as_f64().unwrap_or(2.0) as f32;
        if !seconds.is_finite() || !(0.05..=20.0).contains(&seconds) {
            return Err("seconds must be 0.05–20".into());
        }
        let frames = args
            .get("frames")
            .map(|v| serde_json::from_value::<Vec<f32>>(v.clone()).map_err(|e| e.to_string()))
            .transpose()?;
        let fps = args["fps"].as_f64().unwrap_or(self.timeline.fps as f64) as f32;
        let color = super::tools::vec3(args, "color")?;
        if color.is_some_and(|c| !c.is_finite()) {
            return Err("color must be finite".into());
        }
        if let Some(name) = args["material"].as_str()
            && crate::material::Material::find(&self.scene.materials, name).is_none()
        {
            return Err(format!("unknown material {name}"));
        }
        let before = self.scene.clone();
        let properties = args.clone();
        Ok(Started::Job(
            "Drape cloth".into(),
            Box::new(move |progress| {
                let mut garment = if regenerate {
                    Cloth::generate(settings, &body_mesh, rig.as_ref())?
                } else {
                    existing.expect("existing")
                };
                if frames.is_none() || regenerate {
                    cloth::drape(
                        &mut garment,
                        &clay,
                        &body_mesh,
                        skin.as_deref(),
                        rig.as_ref(),
                        seconds,
                        progress,
                    )?;
                }
                if let Some(frames) = frames {
                    cloth::simulate(
                        &mut garment,
                        &clay,
                        &body_mesh,
                        skin.as_deref().ok_or("simulate needs a skinned body")?,
                        rig.as_ref().ok_or("simulate needs a rig")?,
                        &frames,
                        fps,
                        progress,
                    )?;
                }
                progress.check()?;
                Ok(Box::new(move |app: &mut ClideApp| {
                    if app.scene != before {
                        return Err("scene changed during cloth job; run cloth again".into());
                    }
                    let id = target.unwrap_or_else(|| app.scene.add(Shape::Cloth));
                    let o = app.scene.get_mut(id).ok_or("garment vanished")?;
                    o.parent = Some(parent);
                    if o.material.is_none() || properties.get("material").is_some() {
                        o.material = Some(
                            properties["material"]
                                .as_str()
                                .unwrap_or(if garment.settings.fabric == "leather" {
                                    "leather"
                                } else {
                                    "cloth"
                                })
                                .into(),
                        );
                    }
                    if let Some(name) = properties["name"].as_str() {
                        o.name = name.into();
                    }
                    if let Some(color) = color {
                        o.color = color.clamp(glam::Vec3::ZERO, glam::Vec3::ONE).into();
                    }
                    let reply = json!({"target":id,"parent":parent,"pattern":garment.settings,"vertices":garment.positions.len(),"surface_triangles":garment.triangles.len(),"pins":garment.pins.len(),"cached_frames":garment.cache.len(),"skinned":!garment.joints.is_empty()});
                    o.cloth = Some(garment);
                    o.bake = None;
                    app.selection = vec![id];
                    if let Some(r) = rig
                        && let Some(parent) = app.scene.get_mut(parent)
                        && let Some(live) = parent.rig.as_mut()
                    {
                        live.active = r.active;
                    }
                    Ok(super::tools::text(reply))
                }) as super::jobs::Finish)
            }),
        ))
    }
}
