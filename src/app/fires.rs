use super::{ClideApp, jobs::Started};
use crate::{
    fire::{self, Settings},
    scene::Shape,
};
use glam::Vec3;
use serde_json::{Value, json};
impl ClideApp {
    pub(super) fn fire_tool(&mut self, args: &Value) -> Result<Started, String> {
        let target = args
            .get("target")
            .filter(|v| !v.is_null())
            .map(|v| self.resolve(v))
            .transpose()?;
        if target.is_some_and(|id| self.scene.get(id).is_none_or(|o| o.shape != Shape::Fire)) {
            return Err("target must be a Fire object".into());
        }
        let mut settings = if let Some(name) = args["preset"].as_str() {
            Settings::preset(name)?
        } else {
            target
                .and_then(|id| self.scene.get(id))
                .and_then(|o| o.fire.as_ref())
                .map(|f| f.settings.clone())
                .unwrap_or_default()
        };
        if args.get("settings").is_some_and(|v| !v.is_object()) {
            return Err("settings must be an object".into());
        }
        let mut value = serde_json::to_value(&settings).map_err(|e| e.to_string())?;
        if let Some(map) = args["settings"].as_object() {
            for (k, v) in map {
                value[k] = v.clone();
            }
        }
        settings = serde_json::from_value(value).map_err(|e| e.to_string())?;
        let parent = args
            .get("parent")
            .filter(|v| !v.is_null())
            .map(|v| self.resolve(v))
            .transpose()?;
        let emitter = args
            .get("emitter")
            .filter(|v| !v.is_null())
            .map(|v| self.resolve(v))
            .transpose()?;
        if let Some(p) = parent.or(emitter) {
            if target.is_some_and(|id| id == p || self.scene.is_ancestor(id, p)) {
                return Err("fire cannot be parented under itself or its descendant".into());
            }
            if self.scene.ancestors(p).len()
                + 1
                + target.map_or(0, |id| self.scene.subtree_height(id))
                >= crate::scene::MAX_DEPTH
            {
                return Err("fire parent hierarchy is too deep".into());
            }
        }
        if parent.is_some() && emitter.is_some() && parent != emitter {
            return Err(
                "parent must match emitter, whose local space supplies the emission points".into(),
            );
        }
        let translation: Option<Vec3> = args
            .get("translation")
            .map(|v| {
                serde_json::from_value(v.clone())
                    .map_err(|_| "translation must be [x,y,z]".to_string())
            })
            .transpose()?;
        if translation.is_some_and(|v| !v.is_finite()) {
            return Err("translation must be finite".into());
        }
        if let Some(id) = emitter {
            let o = self.scene.get(id).ok_or("emitter vanished")?;
            let mesh = self.meshes.get(o).ok_or("emitter needs a mesh")?;
            // Mesh emitter positions use the mesh's local space. Parent the fire
            // to the emitter, and center it with translation; points are bounded.
            let center = translation
                .or_else(|| {
                    target
                        .and_then(|id| self.scene.get(id))
                        .map(|o| o.translation)
                })
                .unwrap_or(Vec3::ZERO);
            let step = mesh.vertices.len().div_ceil(256).max(1);
            settings.points = mesh
                .vertices
                .iter()
                .step_by(step)
                .map(|v| Vec3::from(v.pos) - center)
                .filter(|p| p.abs().max_element() <= settings.radius * 3.0)
                .collect();
            if settings.points.is_empty() {
                return Err("emitter has no vertices inside radius*3 of translation; increase radius or move emitter center".into());
            }
        }
        settings.validate()?;
        let before = self.scene.clone();
        let base = args["name"].as_str().unwrap_or("Fire");
        let mut name = base.to_string();
        let mut suffix = 1;
        while self.scene.objects.iter().any(|o| o.name == name) {
            name = format!("{base}{suffix}");
            suffix += 1;
        }
        Ok(Started::Job(
            "Simulate fire and smoke".into(),
            Box::new(move |progress| {
                let fire = fire::simulate(settings, progress)?;
                progress.check()?;
                Ok(Box::new(move |app: &mut ClideApp| {
                    if app.scene != before {
                        return Err("scene changed during fire job; run fire again".into());
                    }
                    let id = target.unwrap_or_else(|| app.scene.add(Shape::Fire));
                    let o = app.scene.get_mut(id).ok_or("fire vanished")?;
                    if target.is_none() {
                        o.name = name;
                        o.translation = Vec3::ZERO;
                    }
                    if parent.is_some() || emitter.is_some() {
                        o.parent = parent.or(emitter);
                    }
                    if let Some(t) = translation {
                        o.translation = t;
                    }
                    o.fire = Some(fire);
                    app.meshes.sync(&app.scene);
                    Ok(super::tools::text(
                        json!({"id":id,"shape":"Fire","cached_frames":app.scene.get(id).unwrap().fire.as_ref().unwrap().cache.len(),"next":"playback frame selects cache; render_film traces volume; export_glb embeds flipbook"}),
                    ))
                }))
            }),
        ))
    }
}
