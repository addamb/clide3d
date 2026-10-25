//! `render_film`: film-quality pictures from the path tracer (see
//! `film/mod.rs`), framed like `render_views`.

use std::time::Duration;

use glam::Vec3;
use serde_json::{Value, json};

use super::ClideApp;
use super::eyes::Shot;
use crate::film::passes::{NO_DEPTH, Pass, Passes};
use crate::outputs::{Format, Kind};

impl ClideApp {
    /// Starts a film render job; `reply` gets the picture.
    pub(super) fn render_film(&mut self, args: &Value) -> Result<super::jobs::Started, String> {
        let resolved = crate::film::look::resolved_args(args)?;
        let args = &resolved;
        let shutter = Shutter::parse(&args["motion_blur"], args["frame"].as_f64().map(|f| f as f32))?;
        let passes = PassRequest::parse(args, self.scene_sphere().1)?;
        if passes.is_some() && shutter.is_some() {
            return Err("passes need a still picture; leave out motion_blur".into());
        }
        // time_limit covers the whole call, mesh building included: agent
        // connections often give up after about a minute.
        let started = std::time::Instant::now();
        let keep = (self.meshes.frame, self.meshes.show_baked, self.meshes.show_fine);
        // The rest pose unless a frame is asked for (like render_views).
        self.set_frame(args["frame"].as_f64().map(|f| f as f32));
        if let Some(b) = args["baked"].as_bool() {
            self.meshes.show_baked = b;
        }
        // Film shows the finest detail there is unless told otherwise. Fine
        // meshes still building are waited for in the job, not here.
        self.meshes.show_fine = args["fine"].as_bool().unwrap_or(true);
        self.meshes.sync_shown(&self.scene);
        let job = (|| {
            let mut job = self.prepare_film(args, started)?;
            if let Some(shutter) = shutter {
                let mut temporal = Vec::new();
                for i in 0..shutter.steps {
                    let u = (i as f32 + 0.5)/shutter.steps as f32;
                    let frame = shutter.frame + shutter.open + (shutter.close-shutter.open)*u;
                    self.set_frame(Some(frame)); self.meshes.sync_shown(&self.scene);
                    let mut sample_args = args.clone();
                    if let Some(end) = shutter.end_position {
                        let start = job.shots[0].1.eye;
                        sample_args["camera"]["position"] = json!(start.lerp(end,u).to_array());
                    }
                    if let Some(end) = shutter.end_target {
                        let start = job.shots[0].1.target;
                        sample_args["camera"]["target"] = json!(start.lerp(end,u).to_array());
                    }
                    let mut slice = self.prepare_film(&sample_args, started)?;
                    // Fix framing throughout the shutter (otherwise scene bounds
                    // would track moving objects and erase their screen motion).
                    for ((_,cam),(_,base)) in slice.shots.iter_mut().zip(&job.shots) {
                        *cam = *base;
                        if let Some(end) = shutter.end_position { cam.eye = base.eye.lerp(end,u); }
                        if let Some(end) = shutter.end_target { cam.target = base.target.lerp(end,u); }
                        if cam.eye.distance_squared(cam.target) < 1e-10 { return Err("shutter camera position and target must differ".into()); }
                    }
                    let total = job.settings.samples.max(shutter.steps);
                    slice.settings.samples = total/shutter.steps + u32::from(i < total%shutter.steps);
                    temporal.push(slice);
                }
                job.temporal = temporal;
                let total_limit = Duration::from_secs_f64(args["time_limit"].as_f64().unwrap_or(90.0).clamp(1.0,600.0));
                job.settings.time_limit = total_limit.saturating_sub(started.elapsed());
            }
            Ok::<_,String>(job)
        })();
        self.set_frame(keep.0);
        self.meshes.show_baked = keep.1;
        self.meshes.show_fine = keep.2;
        self.meshes.sync_shown(&self.scene);
        let mut job = job?;
        if let Some(req) = &passes {
            req.check_views(job.shots.iter().map(|s| s.0.as_str()))?;
        }
        job.passes = passes;
        // For `set_display lighting: "film"`.
        self.last_film_args = Some(resolved.clone());
        let label = format!("Film render ({} view{})", job.shots.len(), if job.shots.len() == 1 { "" } else { "s" });
        Ok(super::jobs::Started::Job(label, Box::new(move |progress| job.run(progress))))
    }

    /// Checks a disk export without reading or changing the editor scene.
    pub(super) fn check_export(&mut self, args: &Value) -> Result<super::jobs::Started, String> {
        let path = std::path::PathBuf::from(args["path"].as_str().ok_or("path is required")?);
        if !path.is_absolute() || path.extension().and_then(|s| s.to_str()) != Some("glb") {
            return Err("path must be an absolute .glb path".into());
        }
        let views = match args.get("views") {
            None | Some(Value::Null) => vec![json!("three_quarter")],
            Some(Value::Array(v)) if !v.is_empty() && v.len() <= 4 => v.clone(),
            _ => return Err("views must contain 1 to 4 views".into()),
        };
        for v in &views { Shot::framing(v, Vec3::ZERO, 1.0)?; }
        let animation = match args.get("animation") {
            None | Some(Value::Null) => None,
            Some(Value::String(s)) => Some(s.clone()),
            _ => return Err("animation must be a clip name".into()),
        };
        let frame = match args.get("frame") {
            None | Some(Value::Null) => None,
            Some(v) => Some(v.as_f64().filter(|f| f.is_finite() && (*f as f32).is_finite()).ok_or("frame must be finite")? as f32),
        };
        let size = args["size"].as_u64().unwrap_or(512).clamp(64, 1600) as usize;
        let limit = args["time_limit"].as_f64().unwrap_or(40.0);
        if !limit.is_finite() { return Err("time_limit must be finite".into()); }
        let settings = crate::film::Settings {
            width: size, height: size,
            samples: args["samples"].as_u64().unwrap_or(32).clamp(1, 4096) as u32,
            bounces: 4, time_limit: Duration::from_secs_f64(limit.clamp(1.0, 600.0)),
            floor: true, exposure: 1.0, denoise: true,
        };
        let save = save_png_path(args)?;
        let mut weights = std::collections::BTreeMap::new();
        for (k, v) in args["morph_weights"].as_object().into_iter().flatten() {
            let w = v.as_f64().filter(|w| w.is_finite()).ok_or("morph_weights are numbers by target name")? as f32;
            if w.abs() > crate::glb_read::MAX_MORPH_WEIGHT {
                return Err(format!("morph_weights {k}: {w} is outside -{0}..{0}", crate::glb_read::MAX_MORPH_WEIGHT));
            }
            if weights.len() >= 64 { return Err("at most 64 morph_weights".into()); }
            weights.insert(k.clone(), w);
        }
        Ok(super::jobs::Started::Job(format!("Check export {}", path.display()), Box::new(move |progress| {
            let start = std::time::Instant::now();
            progress.stage("reading exported GLB", 0.0);
            progress.check()?;
            let loaded = crate::glb_read::read(&path, animation.as_deref(), frame, &weights)?;
            progress.check()?;
            let (lo, hi) = loaded.bounds;
            let center = (lo + hi) * 0.5;
            let radius = ((hi - lo).length() * 0.5).max(0.01);
            let shots = views.iter().map(|v| Shot::framing(v, center, radius).map(|s| (s.name.clone(), s.film_camera()))).collect::<Result<Vec<_>, _>>()?;
            let mut summary = loaded.summary;
            summary["path"] = json!(path);
            let settings = crate::film::Settings { time_limit: settings.time_limit.saturating_sub(start.elapsed()), ..settings };
            FilmJob { temporal: Vec::new(), look: std::sync::Arc::new(crate::film::Look::default()), fine_errors: Value::Null, fine: Vec::new(), fine_by: start, items: loaded.items, fibers: Vec::new(), shots, settings, save, summary: Some(summary), passes: None }.run(progress)
        })))
    }

    /// Everything a film render needs, gathered on the window's thread.
    fn prepare_film(&mut self, args: &Value, started: std::time::Instant) -> Result<FilmJob, String> {
        let views: Vec<Value> = match args.get("views") {
            Some(Value::Array(v)) if !v.is_empty() => v.clone(),
            None | Some(Value::Null) => vec![json!("three_quarter")],
            _ => return Err("views must be a non-empty list".into()),
        };
        if views.len() > 4 {
            return Err("at most 4 views per call (film renders are slow)".into());
        }
        let size = args["size"].as_u64().unwrap_or(640).clamp(64, 1600) as usize;
        let total_limit = args["time_limit"].as_f64().unwrap_or(90.0).clamp(1.0, 600.0);
        // What is left after building meshes; the views share it evenly.
        let mut look = crate::film::Look::parse(args)?;
        if !args["camera"]["dof"]["focus_object"].is_null() {
            if !args["camera"]["dof"]["focus_distance"].is_null() || !args["camera"]["dof"]["focus_point"].is_null() { return Err("use only one focus target".into()); }
            let id = self.resolve(&args["camera"]["dof"]["focus_object"])?;
            let eval = self.scene.evaluate();
            let world = eval.get(&id).ok_or("focus object has no transform")?.0;
            let object = self.scene.get(id).ok_or("focus object not found")?;
            let local_center = self.meshes.get(object).map_or(Vec3::ZERO, |mesh| {
                let (lo,hi) = mesh.vertices.iter().fold((Vec3::splat(f32::MAX),Vec3::splat(f32::MIN)), |(lo,hi),v| (lo.min(Vec3::from(v.pos)),hi.max(Vec3::from(v.pos))));
                if mesh.vertices.is_empty() { Vec3::ZERO } else { (lo+hi)*0.5 }
            });
            look.camera.focus_point = Some(world.transform_point3(local_center));
        }
        let left = (total_limit - started.elapsed().as_secs_f64()).max(1.0);
        let sensor = &args["camera"]["sensor_size_mm"];
        let sensor_aspect = sensor[0].as_f64().zip(sensor[1].as_f64()).map_or(1.0, |(w,h)|w/h);
        let aspect = args["aspect"].as_f64().unwrap_or(sensor_aspect);
        if !(0.25..=4.0).contains(&aspect) {
            return Err("aspect must be 0.25..4 (width / height)".into());
        }
        let settings = crate::film::Settings {
            width: size,
            height: ((size as f64 / aspect).round() as usize).clamp(64, 1600),
            samples: args["samples"].as_u64().unwrap_or(128).clamp(1, 4096) as u32,
            bounces: args["bounces"].as_u64().unwrap_or(4).clamp(1, 12) as u32,
            time_limit: Duration::from_secs_f64(left),
            floor: args["floor"].as_bool().unwrap_or(true),
            exposure: args["exposure"].as_f64().unwrap_or(1.0).clamp(0.05, 20.0) as f32,
            denoise: args["denoise"].as_bool().unwrap_or(true),
        };
        let (mut center, mut radius) = self.scene_sphere();
        if let Some(c) = args.get("center").filter(|v| !v.is_null()) {
            let c: [f32; 3] = serde_json::from_value(c.clone()).map_err(|_| "center must be [x, y, z]")?;
            center = Vec3::from(c);
        }
        if let Some(r) = args["radius"].as_f64() {
            radius = (r as f32).clamp(0.01, 1000.0);
        }
        let eval = self.scene.evaluate();
        // Every visible mesh, scattered copies included; lights carried by
        // objects (torches, candles, copies of them) join the lighting.
        let draws = crate::envkit::scatter::draws(&self.scene, &eval);
        // Each item's object, for fine meshes still building.
        let mut owners = Vec::new();
        let mut items: Vec<crate::film::Item> = draws
            .iter()
            .filter(|d| d.object.shape.is_mesh() && d.object.shape != crate::scene::Shape::Fire)
            .filter_map(|d| {
                let mesh = self.meshes.get(d.object)?.clone();
                owners.push(Some(d.object));
                Some(crate::film::Item { mesh, world: d.world, tint: d.object.color, surface: self.scene.surface_of(d.object), material: self.scene.material_of(d.object) })
            })
            .collect();
        if args["object_lights"].as_bool().unwrap_or(true) {
            let time = self.scene.frame.unwrap_or(0.0) / self.timeline.fps.max(1.0);
            let lit = crate::envkit::light::film_lights(&self.scene, &eval, time);
            if lit.len() > MAX_OBJECT_LIGHTS {
                return Err(format!("{} objects give off light; film renders take at most {MAX_OBJECT_LIGHTS}", lit.len()));
            }
            look.lights.extend(lit);
        }
        for d in &draws {
            if let Some(v) = d.object.fire.as_ref().and_then(|f| f.volume(d.world, self.scene.frame)) {
                if args["object_lights"].as_bool().unwrap_or(true) { look.lights.extend(v.lights()); }
                look.volumes.push(v);
                if let Some(f)=d.object.fire.as_ref() {
                    let material=crate::material::Material::default();
                    items.push(crate::film::Item{mesh:std::sync::Arc::new(f.ember_mesh(self.scene.frame)),world:d.world,tint:[1.0;3],surface:material.surface(),material});
                    owners.push(None);
                }
            }
        }
        let look = std::sync::Arc::new(look);
        // Hair objects render as strands, not cards.
        let mut fibers = Vec::new();
        for o in self.scene.objects.iter().filter(|o| o.shape == crate::scene::Shape::Hair) {
            let (Some(&(world, true)), Some(hair)) = (eval.get(&o.id), o.hair.as_ref()) else { continue };
            let scale = world.to_scale_rotation_translation().0.abs().max_element();
            let tint = Vec3::from(o.color).powf(2.2);
            for s in hair.strands() {
                fibers.push(crate::film::Fiber {
                    points: s.points.iter().map(|p| world.transform_point3(*p)).collect(),
                    root_width: s.root_width * scale,
                    tip_width: s.tip_width * scale,
                    root_color: s.root_color * tint,
                    tip_color: s.tip_color * tint,
                });
            }
        }
        let (items, owners): (Vec<crate::film::Item>, Vec<_>) = items.into_iter().zip(owners).filter(|(i, _)| i.mesh.textures.as_ref().is_none_or(|t| !t.alpha)).unzip();
        let mut fine: Vec<FineWait> = Vec::new();
        for (i, o) in owners.iter().enumerate() {
            let Some(o) = o else { continue };
            if let Some(w) = fine.iter_mut().find(|w| w.id == o.id) {
                w.items.push(i);
            } else if let Some((ticket, seconds, pose)) = self.meshes.fine_building(o) {
                fine.push(FineWait { id: o.id, name: o.name.clone(), items: vec![i], ticket, started: std::time::Instant::now() - Duration::from_secs_f32(seconds), pose });
            }
        }
        let shots = views.iter().map(|v| {
            let s = Shot::framing(v, center, radius)?;
            Ok((s.name.clone(), look.camera.apply(s.film_camera())?))
        }).collect::<Result<Vec<_>, String>>()?;
        let save = save_png_path(args)?;
        let fine_wait = args["fine_wait"].as_f64().unwrap_or((total_limit * 0.5).min(30.0)).clamp(0.0, 600.0);
        let fine_by = started + Duration::from_secs_f64(fine_wait);
        Ok(FilmJob { temporal: Vec::new(), look, fine_errors: self.fine_errors_json(), fine, fine_by, items, fibers, shots, settings, save, summary: None, passes: None })
    }
}

fn save_png_path(args: &Value) -> Result<Option<std::path::PathBuf>, String> {
    let Some(value) = args.get("save_path").filter(|v| !v.is_null()) else { return Ok(None) };
    let path = std::path::PathBuf::from(value.as_str().ok_or("save_path must be a string")?);
    if !path.is_absolute() || path.extension().and_then(|s| s.to_str()) != Some("png") {
        return Err("save_path must be an absolute .png path".into());
    }
    if path.exists() && !std::fs::read(&path).is_ok_and(|b| b.starts_with(b"\x89PNG")) {
        return Err(format!("{} exists and is not a PNG; refusing to overwrite", path.display()));
    }
    Ok(Some(path))
}

/// Most lights from objects in one film render (each costs a little at
/// every shading point, even when not picked).
const MAX_OBJECT_LIGHTS: usize = 2048;

/// Stratified shutter quadrature over evaluated animation, measured in frames.
struct Shutter { frame: f32, open: f32, close: f32, steps: u32, end_position: Option<Vec3>, end_target: Option<Vec3> }
impl Shutter {
    fn parse(v: &Value, frame: Option<f32>) -> Result<Option<Self>,String> {
        use crate::film::lighting::{number,vector};
        if v.is_null() || v == &json!(false) { return Ok(None); }
        if !v.is_object() && v != &json!(true) { return Err("motion_blur must be true, false or an object".into()); }
        let frame = frame.filter(|f| f.is_finite()).ok_or("motion_blur needs an animation frame")?;
        let open = number(v,"open",-0.25,-10.0,10.0)?;
        let close = number(v,"close",0.25,-10.0,10.0)?;
        let steps = number(v,"steps",8.0,2.0,16.0)?;
        if close <= open || steps.fract() != 0.0 { return Err("shutter close must exceed open; steps must be an integer 2..16".into()); }
        let end_position = if v["camera_end_position"].is_null() { None } else { Some(vector(v,"camera_end_position",None)?) };
        let end_target = if v["camera_end_target"].is_null() { None } else { Some(vector(v,"camera_end_target",None)?) };
        Ok(Some(Self { frame, open, close, steps: steps as u32, end_position, end_target }))
    }
}

/// A film render, ready to run off the window's thread.
struct FilmJob {
    temporal: Vec<FilmJob>,
    look: std::sync::Arc<crate::film::Look>,
    /// Clay drawn at its regular mesh because its fine mesh failed.
    fine_errors: serde_json::Value,
    /// Fine meshes still building, waited for until `fine_by`.
    fine: Vec<FineWait>,
    fine_by: std::time::Instant,
    items: Vec<crate::film::Item>,
    fibers: Vec<crate::film::Fiber>,
    shots: Vec<(String, crate::film::Camera)>,
    settings: crate::film::Settings,
    save: Option<std::path::PathBuf>,
    summary: Option<Value>,
    /// Render passes to write too (still pictures only).
    passes: Option<PassRequest>,
}

/// A fine mesh still building when the render was asked for, and the
/// items (one object, scattered copies too) that show it.
struct FineWait {
    id: u64,
    name: String,
    items: Vec<usize>,
    ticket: crate::mesh::FineTicket,
    started: std::time::Instant,
    /// Skin matrices when the object is posed.
    pose: Option<Vec<glam::Mat4>>,
}

impl FilmJob {
    /// Puts in the fine meshes built by `fine_by` (shutter slices too);
    /// the rest keep the regular mesh and are listed in `fine_errors`.
    fn take_fine(&mut self, progress: &crate::progress::Progress) -> Result<(), String> {
        let waits: Vec<FineWait> = std::mem::take(&mut self.fine);
        let mut slices: Vec<(Vec<FineWait>, &mut Vec<crate::film::Item>)> = self.temporal.iter_mut().map(|s| (std::mem::take(&mut s.fine), &mut s.items)).collect();
        slices.push((waits, &mut self.items));
        let total = slices.iter().map(|s| s.0.len()).sum::<usize>();
        let mut done = 0;
        let mut errors = std::collections::BTreeMap::new();
        for (waits, items) in slices {
            for w in waits {
                progress.stage(&format!("waiting for the fine mesh of {}", w.name), done as f32 / total as f32);
                done += 1;
                // Short waits, so a cancelled render stops promptly.
                let result = loop {
                    progress.check()?;
                    let now = std::time::Instant::now();
                    if let Some(r) = w.ticket.wait_until(self.fine_by.min(now + Duration::from_millis(200))) {
                        break Some(r);
                    }
                    if now >= self.fine_by {
                        break None;
                    }
                };
                match result {
                    Some(Ok((mesh, skin))) => {
                        let mesh = match (&w.pose, skin) {
                            (Some(pose), Some(skin)) => std::sync::Arc::new(crate::rig::pose_mesh(&mesh, &skin, pose)),
                            _ => mesh,
                        };
                        for &i in &w.items {
                            items[i].mesh = mesh.clone();
                        }
                    }
                    Some(Err(why)) => { errors.insert(w.name, why); }
                    None => { errors.insert(w.name, format!("fine mesh still building ({:.0} s so far); render again to use it", w.started.elapsed().as_secs_f32())); }
                }
            }
        }
        if !errors.is_empty() {
            if !self.fine_errors.is_object() {
                self.fine_errors = json!({});
            }
            let list = self.fine_errors.as_object_mut().expect("an object");
            list.extend(errors.into_iter().map(|(k, v)| (k, Value::from(v))));
        }
        Ok(())
    }

    fn run(mut self, progress: &crate::progress::Progress) -> Result<super::jobs::Finish, String> {
        let start = std::time::Instant::now();
        self.take_fine(progress)?;
        let triangles = self.items.iter().map(|i| i.mesh.indices.len() / 3).sum::<usize>();
        let lights = self.look.lights.len();
        // Displacing materials become real geometry, diced for these cameras.
        let mut items = std::mem::take(&mut self.items);
        let cams: Vec<crate::film::Camera> = self.shots.iter().map(|s| s.1).collect();
        if items.iter().any(|i| i.material.detail_displace > 0.0 || i.mesh.materials.iter().any(|m| m.detail_displace > 0.0)) {
            progress.stage("dicing displaced surfaces", 0.0);
        }
        let diced = if self.temporal.is_empty() { crate::film::dice::displace(&mut items, &cams, self.settings.height) } else { 0 };
        let triangles = triangles + diced;
        progress.stage("building the ray tree", 0.0);
        // One ray tree for every view; its time counts against the limit.
        progress.check()?;
        let mut mixed = false;
        let mut passes = Vec::new();
        let images = if self.temporal.is_empty() {
            let meshes = crate::film::Meshes::new(items);
            let settings = crate::film::Settings { time_limit: self.settings.time_limit.saturating_sub(start.elapsed()), ..self.settings };
            let images = crate::film::render_views_with_look(&meshes, &self.fibers, &cams, settings, self.look.clone(), progress);
            if let Some(req) = &self.passes {
                for (i, cam) in cams.iter().enumerate() {
                    progress.check()?;
                    progress.stage(&format!("render passes, view {}/{}", i + 1, cams.len()), 0.97);
                    let t = std::time::Instant::now();
                    let p = crate::film::passes::render(&meshes, &self.fibers, cam, self.settings, self.look.clone(), &req.list, req.options, progress);
                    passes.push((p, t.elapsed().as_secs_f32()));
                }
            }
            images
        } else {
            let mut accumulated: Vec<crate::film::Image> = Vec::new();
            let n = self.temporal.len();
            // Each shutter slice is its own render: say so if they differ.
            for (i,mut slice) in std::mem::take(&mut self.temporal).into_iter().enumerate() {
                progress.check()?;
                progress.stage(&format!("shutter sample {}/{}",i+1,n), i as f32/n as f32);
                let cams: Vec<_> = slice.shots.iter().map(|s| s.1).collect();
                crate::film::dice::displace(&mut slice.items, &cams, slice.settings.height);
                let meshes = crate::film::Meshes::new(slice.items);
                slice.settings.time_limit = self.settings.time_limit.saturating_sub(start.elapsed())/(n-i) as u32;
                let images = crate::film::render_views_with_look(&meshes,&slice.fibers,&cams,slice.settings,slice.look,progress);
                if accumulated.is_empty() {
                    accumulated = images;
                } else {
                    for (a,b) in accumulated.iter_mut().zip(images) {
                        mixed |= a.traced.used() != b.traced.used();
                        for (c,d) in a.linear.iter_mut().zip(b.linear) { *c += d; }
                        a.samples += b.samples; a.seconds += b.seconds;
                    }
                }
            }
            for image in &mut accumulated {
                for c in &mut image.linear { *c /= n as f32; }
                image.pixels = self.look.finish(image.linear.clone(),image.width,image.height,self.settings.exposure);
            }
            accumulated
        };
        progress.check()?;
        let mut tiles = Vec::new();
        let mut stats = Vec::new();
        let (device, warning) = device_reply(images.first().map(|i| &i.traced), self.look.device, mixed);
        for ((name, _), img) in self.shots.iter().zip(images) {
            stats.push(json!({ "view": name, "samples": img.samples, "seconds": (img.seconds * 10.0).round() / 10.0, "device": img.traced.used() }));
            let pixels = img.pixels.iter().map(|p| egui::Color32::from_rgb(p[0], p[1], p[2])).collect();
            tiles.push(egui::ColorImage::new([img.width, img.height], pixels));
        }
        let cols = (tiles.len() as f32).sqrt().ceil() as usize;
        let sheet = super::eyes::contact_sheet(&tiles, cols);
        let saved = match &self.save {
            Some(path) => {
                std::fs::write(path, super::tools::encode_png_bytes(&sheet)?).map_err(|e| format!("{}: {e}", path.display()))?;
                Some(path.display().to_string())
            }
            None => None,
        };
        progress.check()?;
        let mut pass_sheet = None;
        let pass_reply = match &self.passes {
            Some(req) => {
                let names: Vec<String> = self.shots.iter().map(|s| s.0.clone()).collect();
                let (reply, sheet) = req.write(&passes, &names)?;
                pass_sheet = Some(sheet);
                Some(reply)
            }
            None => None,
        };
        let mut summary = self.summary.unwrap_or_else(|| json!({}));
        let render = json!({
            "views": stats,
            "layout": format!("{cols} per row"),
            "triangles": triangles,
            "seconds": (start.elapsed().as_secs_f32() * 10.0).round() / 10.0,
            "displaced_triangles": diced,
            "hair_strands": self.fibers.len(),
            "lights": lights,
            "fine_mesh_errors": self.fine_errors,
            "saved": saved,
            "device": device,
        });
        summary.as_object_mut().expect("summary is an object").extend(render.as_object().unwrap().clone());
        if let Some(reply) = pass_reply {
            summary.as_object_mut().expect("summary is an object").extend(reply.as_object().expect("an object").clone());
        }
        if let Some(w) = warning {
            match summary["warnings"].as_array_mut() {
                Some(list) => list.push(w.into()),
                None => summary["warnings"] = json!([w]),
            }
        }
        let mut out = super::tools::text(summary);
        out.extend(super::tools::encode_png(&super::tools::downscale(&sheet, 1600))?);
        if let Some(sheet) = pass_sheet {
            out.extend(super::tools::encode_png(&super::tools::downscale(&sheet, 1600))?);
        }
        Ok(Box::new(move |_| Ok(out)))
    }
}

/// `render_film passes`: which passes, where, how written and sampled.
struct PassRequest {
    list: Vec<Pass>,
    format: Format,
    dir: std::path::PathBuf,
    options: crate::film::passes::Options,
}

impl PassRequest {
    /// `passes`, `passes_dir`, `passes_format`, `pass_samples`,
    /// `ao_distance` (default a quarter of `radius`, the framing's) and
    /// `normal_space`.
    fn parse(args: &Value, radius: f32) -> Result<Option<PassRequest>, String> {
        let list = match &args["passes"] {
            Value::Null => return Ok(None),
            Value::Array(list) if !list.is_empty() && list.len() <= Pass::ALL.len() => list,
            _ => return Err(format!("passes must list some of {}", Pass::ALL.map(Pass::name).join(", "))),
        };
        let mut passes = Vec::new();
        for p in list {
            let pass = p.as_str().and_then(Pass::parse).ok_or_else(|| format!("unknown pass {p}: use {}", Pass::ALL.map(Pass::name).join(", ")))?;
            if passes.contains(&pass) {
                return Err(format!("pass {} is listed twice", pass.name()));
            }
            passes.push(pass);
        }
        if args["passes_dir"].is_null() {
            return Err("passes_dir (an absolute folder) is required with passes".into());
        }
        let format = Format::parse(&args["passes_format"], "passes_format", Format::Png)?;
        let samples = args["pass_samples"].as_u64().unwrap_or(8);
        if !(1..=64).contains(&samples) {
            return Err("pass_samples must be 1..64".into());
        }
        let radius = args["radius"].as_f64().map_or(radius, |r| r as f32).clamp(0.01, 1000.0);
        let ao_distance = match args["ao_distance"].as_f64() {
            Some(d) if d.is_finite() && d > 0.0 && d <= 1000.0 => d as f32,
            Some(_) => return Err("ao_distance must be 0..1000 (world units)".into()),
            None => radius * 0.25,
        };
        let camera_normals = match args["normal_space"].as_str() {
            None | Some("world") => false,
            Some("camera") => true,
            Some(_) => return Err("normal_space must be world or camera".into()),
        };
        let dir = crate::outputs::folder(&args["passes_dir"], "passes_dir")?;
        Ok(Some(PassRequest { list: passes, format, dir, options: crate::film::passes::Options { samples: samples as u32, ao_rays: 8, ao_distance, camera_normals } }))
    }

    /// Refuses views whose pass files (or reply entries) would be the same:
    /// two views of one name, or names that make the same file stem.
    fn check_views<'a>(&self, names: impl Iterator<Item = &'a str>) -> Result<(), String> {
        let mut seen: Vec<(String, &str)> = Vec::new();
        for name in names {
            let stem = crate::outputs::file_stem(name);
            if let Some((_, other)) = seen.iter().find(|(s, _)| *s == stem) {
                return Err(format!("views {other:?} and {name:?} would write the same pass files ({stem}_*): give the views distinct names"));
            }
            seen.push((stem, name));
        }
        Ok(())
    }

    /// The 16-bit code of a depth `d` hit between `near` and `far`: near
    /// 65535, far 1 (0 is the background, `NO_DEPTH`).
    fn depth_code(d: f32, near: f32, far: f32) -> u16 {
        if d >= NO_DEPTH {
            0
        } else if far > near {
            1 + (((far - d) / (far - near)).clamp(0.0, 1.0) * 65534.0).round() as u16
        } else {
            65535
        }
    }

    /// Writes every view's passes (on the job's thread): the reply's
    /// `passes` ({name: path} for one view, {name: {view: path}} for more)
    /// and `passes_info`, and a sheet of the first view's passes.
    fn write(&self, views: &[(Passes, f32)], names: &[String]) -> Result<(Value, egui::ColorImage), String> {
        crate::outputs::make_folder(&self.dir)?;
        let mut paths = serde_json::Map::new();
        let mut info = Vec::new();
        for ((p, seconds), view) in views.iter().zip(names) {
            let (w, h) = (p.width, p.height);
            let range = p.depth_range();
            for &pass in &self.list {
                let file = if views.len() == 1 { format!("{}.{}", pass.name(), self.format.ext()) } else { format!("{}_{}.{}", crate::outputs::file_stem(view), pass.name(), self.format.ext()) };
                let path = self.dir.join(file);
                crate::outputs::writable(&path, Kind::of(self.format))?;
                let gray16 = |v: &[f32]| v.iter().map(|x| (x.clamp(0.0, 1.0) * 65535.0).round() as u16).collect::<Vec<_>>();
                let rgb = |v: &[Vec3]| -> Vec<(&str, Vec<f32>)> { vec![("R", v.iter().map(|c| c.x).collect()), ("G", v.iter().map(|c| c.y).collect()), ("B", v.iter().map(|c| c.z).collect())] };
                match self.format {
                    Format::Exr => {
                        let channels = match pass {
                            Pass::Depth => vec![("Z", p.depth.clone())],
                            Pass::Normal => rgb(&p.normal),
                            Pass::Albedo => rgb(&p.albedo),
                            Pass::Emission => rgb(&p.emission),
                            Pass::Ao => vec![("Y", p.ao.clone())],
                            Pass::Shadow => vec![("Y", p.shadow.clone())],
                            Pass::Mask => vec![("Y", p.mask.clone())],
                        };
                        crate::outputs::write_exr(&path, w, h, channels)?;
                    }
                    Format::Png => {
                        let bytes = match pass {
                            Pass::Depth => {
                                let (near, far) = range.unwrap_or((0.0, 1.0));
                                let codes: Vec<u16> = p.depth.iter().map(|d| Self::depth_code(*d, near, far)).collect();
                                crate::outputs::png16(w, h, 1, &codes)?
                            }
                            Pass::Normal => {
                                let codes: Vec<u16> = p.normal.iter().flat_map(|n| if *n == Vec3::ZERO { [0; 3] } else { (*n * 0.5 + 0.5).to_array().map(|v| (v.clamp(0.0, 1.0) * 65535.0).round() as u16) }).collect();
                                crate::outputs::png16(w, h, 3, &codes)?
                            }
                            Pass::Albedo | Pass::Emission => {
                                let v = if pass == Pass::Albedo { &p.albedo } else { &p.emission };
                                let bytes: Vec<u8> = v.iter().flat_map(|c| crate::film::passes::srgb8(*c)).collect();
                                crate::outputs::png8(w, h, 3, &bytes)?
                            }
                            Pass::Ao => crate::outputs::png16(w, h, 1, &gray16(&p.ao))?,
                            Pass::Shadow => crate::outputs::png16(w, h, 1, &gray16(&p.shadow))?,
                            Pass::Mask => crate::outputs::png16(w, h, 1, &gray16(&p.mask))?,
                        };
                        std::fs::write(&path, bytes).map_err(|e| format!("could not write {}: {e}", path.display()))?;
                    }
                }
                let path = json!(path.display().to_string());
                if views.len() == 1 {
                    paths.insert(pass.name().into(), path);
                } else {
                    paths.entry(pass.name()).or_insert_with(|| json!({}))[view] = path;
                }
            }
            info.push(json!({ "view": view, "depth_range": range.map(|(a, b)| [a, b]), "seconds": (seconds * 10.0).round() / 10.0 }));
        }
        let decode = match self.format {
            Format::Png => json!({
                "depth": "16-bit gray: code 0 = background; a hit's depth = depth_range[1] - (code - 1) / 65534 * (depth_range[1] - depth_range[0]) (near = 65535, far = 1; when the range is one value every hit is 65535); depth is the distance along the camera's view axis",
                "normal": "16-bit RGB = 0.5 + 0.5 * normal, background 0",
                "albedo": "8-bit sRGB, times coverage (the floor's too: its gray 0.42)", "emission": "8-bit sRGB, times coverage",
                "ao": "16-bit gray, 1 = open", "shadow": "16-bit gray, 1 = in full shadow", "mask": "16-bit gray coverage of objects (not floor)",
            }),
            Format::Exr => json!({
                "depth": format!("channel Z, the distance along the camera's view axis; background {NO_DEPTH:e}"),
                "normal": "channels R, G, B = x, y, z", "albedo": "linear R, G, B times coverage (the floor's too: its gray 0.42)", "emission": "linear R, G, B times coverage",
                "ao": "channel Y, 1 = open", "shadow": "channel Y, 1 = in full shadow", "mask": "channel Y, coverage of objects (not floor)",
            }),
        };
        let o = self.options;
        let reply = json!({
            "passes": paths,
            "passes_info": { "format": self.format.ext(), "views": info, "normal_space": if o.camera_normals { "camera (x right, y up, z toward the camera)" } else { "world" }, "samples": o.samples, "ao_distance": o.ao_distance, "decode": decode, "image": "The second image: the first view's passes in the order asked, 4 per row" },
        });
        // The first view's passes side by side, for looking at.
        let first = &views.first().ok_or("no view")?.0;
        let tiles: Vec<egui::ColorImage> = self.list.iter().map(|&pass| {
            let pixels = first.preview(pass).into_iter().map(|c| egui::Color32::from_rgb(c[0], c[1], c[2])).collect();
            egui::ColorImage::new([first.width, first.height], pixels)
        }).collect();
        let cols = tiles.len().min(4);
        Ok((reply, super::eyes::contact_sheet(&tiles, cols)))
    }
}

/// The reply's `device`: which tracer made the pictures (each view also
/// lists its own), the GPU's name, and why not the GPU when the CPU traced;
/// `mixed` when shutter slices were traced on different devices. Also a
/// warning when the GPU was asked for by name and not used.
fn device_reply(traced: Option<&crate::film::Traced>, asked: crate::film::look::Device, mixed: bool) -> (Value, Option<String>) {
    let traced = traced.cloned().unwrap_or_default();
    let mut device = traced.json();
    device["asked"] = json!(match asked {
        crate::film::look::Device::Auto => "auto",
        crate::film::look::Device::Gpu => "gpu",
        crate::film::look::Device::Cpu => "cpu",
    });
    if mixed {
        device["mixed"] = json!(true);
    }
    let warning = match &traced {
        crate::film::Traced::Cpu { reason } if asked == crate::film::look::Device::Gpu => {
            Some(format!("device: gpu was asked for but the CPU traced this render ({})", reason.as_deref().unwrap_or("no reason given")))
        }
        _ => None,
    };
    (device, warning)
}

#[cfg(test)]
mod pass_tests {
    use super::*;

    /// Hits encode 1..=65535 (near white, the farthest 1), the background
    /// 0, and the reply's formula reads a hit back.
    #[test]
    fn depth_codes_keep_the_farthest_hit_off_the_background() {
        let (near, far) = (2.0, 6.0);
        assert_eq!(PassRequest::depth_code(near, near, far), 65535);
        assert_eq!(PassRequest::depth_code(far, near, far), 1);
        assert_eq!(PassRequest::depth_code(NO_DEPTH, near, far), 0);
        assert_eq!(PassRequest::depth_code(3.0, 3.0, 3.0), 65535);
        for d in [2.0f32, 2.5, 4.0, 5.99, 6.0] {
            let code = PassRequest::depth_code(d, near, far) as f32;
            let back = far - (code - 1.0) / 65534.0 * (far - near);
            assert!((back - d).abs() <= (far - near) / 65534.0, "{d} -> {code} -> {back}");
        }
    }

    /// Views whose files would collide are refused before anything renders.
    #[test]
    fn views_with_one_file_stem_are_refused() {
        let req = PassRequest { list: vec![Pass::Depth], format: Format::Png, dir: std::env::temp_dir(), options: crate::film::passes::Options { samples: 1, ao_rays: 1, ao_distance: 1.0, camera_normals: false } };
        req.check_views(["front", "side"].into_iter()).unwrap();
        assert!(req.check_views(["front", "front"].into_iter()).is_err());
        assert!(req.check_views(["my view", "my_view"].into_iter()).unwrap_err().contains("distinct names"));
    }
}

#[cfg(test)]
mod shutter_tests {
    use super::*;
    #[test]
    fn shutter_averages_moving_geometry_before_tone_and_glare() {
        use crate::film::{Camera,Item,Look,Meshes,Settings,look::Device,render_views_with_look};
        use glam::Mat4;
        use std::sync::Arc;
        let mut look=Look::parse(&json!({"lights":"none","background":true,"bloom":{"strength":0.25,"threshold":0},"tonemap":"filmic","grain":0.01})).unwrap();
        look.device=Device::Cpu;
        look.environment=Arc::new(crate::film::lighting::Environment::map(1,1,vec![Vec3::ZERO],0.0));
        let look=Arc::new(look);
        let cam=Camera{eye:Vec3::Z*3.0,target:Vec3::ZERO,up:Vec3::Y,fov_deg:35.0,ortho:None};
        let settings=Settings{width:32,height:24,samples:16,bounces:1,time_limit:Duration::from_secs(30),floor:false,exposure:1.0,denoise:false};
        let make=|x:f32| {
            let material=crate::material::Material{emission:8.0,..Default::default()};
            vec![Item{mesh:Arc::new(crate::mesh::Mesh::build(crate::scene::Shape::Sphere)),world:Mat4::from_scale_rotation_translation(Vec3::splat(0.4),glam::Quat::IDENTITY,Vec3::X*x),tint:[1.0,0.4,0.2],surface:material.surface(),material}]
        };
        let job=|x:f32| FilmJob{temporal:Vec::new(),look:look.clone(),fine_errors:Value::Null,fine:Vec::new(),fine_by:std::time::Instant::now(),items:make(x),fibers:Vec::new(),shots:vec![("motion".into(),cam)],settings,save:None,summary:None,passes:None};
        let mut expected=vec![Vec3::ZERO;32*24];
        let mut ldr=vec![[0.0f32;3];32*24];
        for x in [-0.45,0.45] {
            let meshes=Meshes::new(make(x));
            let im=render_views_with_look(&meshes,&[],&[cam],settings,look.clone(),&Default::default()).pop().unwrap();
            for ((a,b),(c,d)) in expected.iter_mut().zip(im.linear).zip(ldr.iter_mut().zip(im.pixels)) { *a+=b*0.5; for k in 0..3 {c[k]+=d[k] as f32*0.5;} }
        }
        let expected=look.finish(expected,32,24,1.0);
        // A nonlinear shoulder/glare/grain makes LDR averaging observably wrong.
        assert!(expected.iter().zip(ldr).any(|(a,b)|(a[0] as f32-b[0]).abs()>10.0));
        let output=std::env::temp_dir().join(format!("maya-lens-shutter-{}.png",std::process::id()));
        let mut main=job(0.0); main.temporal=vec![job(-0.45),job(0.45)]; main.save=Some(output.clone());
        let _finish=main.run(&Default::default()).unwrap();
        let actual=image::open(&output).unwrap().to_rgb8();
        assert_eq!(actual.as_raw(),&expected.iter().flatten().copied().collect::<Vec<_>>());
        std::fs::remove_file(output).unwrap();
    }

    /// The job puts in fine meshes built in time (posed copies and every
    /// item of the object), and lists the late and the failed.
    #[test]
    fn film_job_takes_fine_meshes_ready_by_its_deadline() {
        use crate::film::{Item, Look, Settings};
        use crate::mesh::{FineTicket, Mesh};
        use crate::scene::Shape;
        use std::sync::Arc;
        let regular = Arc::new(Mesh::build(Shape::Sphere));
        let fine = Arc::new(Mesh::build(Shape::Cube));
        let item = || Item { mesh: regular.clone(), world: glam::Mat4::IDENTITY, tint: [1.0; 3], surface: Default::default(), material: Default::default() };
        let now = std::time::Instant::now();
        let wait = |id, name: &str, items, ticket| FineWait { id, name: name.into(), items, ticket, started: now - Duration::from_secs(12), pose: None };
        let settings = Settings { width: 8, height: 8, samples: 1, bounces: 1, time_limit: Duration::from_secs(1), floor: false, exposure: 1.0, denoise: false };
        let mut job = FilmJob { temporal: Vec::new(), look: Arc::new(Look::default()), fine_errors: Value::Null, fine: vec![
            wait(1, "ready", vec![0, 2], FineTicket::done(Ok((fine.clone(), None)))),
            wait(2, "late", vec![1], FineTicket::default()),
            wait(3, "failed", vec![3], FineTicket::done(Err("too fine".into()))),
        ], fine_by: now, items: vec![item(), item(), item(), item()], fibers: Vec::new(), shots: Vec::new(), settings, save: None, summary: None, passes: None };
        job.take_fine(&Default::default()).unwrap();
        let shown: Vec<bool> = job.items.iter().map(|i| Arc::ptr_eq(&i.mesh, &fine)).collect();
        assert_eq!(shown, [true, false, true, false]);
        assert!(job.fine_errors["late"].as_str().unwrap().starts_with("fine mesh still building (12 s so far)"));
        assert_eq!(job.fine_errors["failed"], "too fine");
        assert!(job.fine_errors.get("ready").is_none());
    }

    #[test]
    fn reply_names_the_device_and_warns_when_gpu_is_refused() {
        use crate::film::{Traced, look::Device};
        let refused = Traced::Cpu { reason: Some("triangles: 5.10 GB is more than one GPU buffer holds (4.00 GB)".into()) };
        let (device, warning) = device_reply(Some(&refused), Device::Gpu, false);
        assert_eq!(device["used"], "cpu");
        assert_eq!(device["asked"], "gpu");
        assert!(device["gpu"].is_null());
        assert!(device["fallback_reason"].as_str().unwrap().starts_with("triangles"));
        assert!(device.get("mixed").is_none());
        assert!(warning.unwrap().contains("triangles: 5.10 GB"));
        // Auto falling back is reported but is no warning.
        assert!(device_reply(Some(&refused), Device::Auto, false).1.is_none());
        let gpu = Traced::Gpu { name: "Test GPU (Metal)".into(), warning: None };
        let (device, warning) = device_reply(Some(&gpu), Device::Gpu, true);
        assert_eq!((device["used"].as_str(), device["gpu"].as_str(), device["mixed"].as_bool()), (Some("gpu"), Some("Test GPU (Metal)"), Some(true)));
        assert!(device["fallback_reason"].is_null() && warning.is_none());
        let (device, warning) = device_reply(Some(&Traced::Cpu { reason: None }), Device::Cpu, false);
        assert_eq!((device["used"].as_str(), device["fallback_reason"].is_null(), warning), (Some("cpu"), true, None));
    }

    #[test]
    fn shutter_validation_and_strata() {
        assert!(Shutter::parse(&json!(true),None).is_err());
        for v in [json!({"open":1,"close":0}),json!({"steps":2.5}),json!({"steps":100})] { assert!(Shutter::parse(&v,Some(10.0)).is_err()); }
        let shutter=Shutter::parse(&json!(true),Some(10.0)).unwrap().unwrap();
        let times:Vec<_>=(0..shutter.steps).map(|i|shutter.frame+shutter.open+(shutter.close-shutter.open)*(i as f32+0.5)/shutter.steps as f32).collect();
        assert!(times[0]>9.75 && times[7]<10.25);
        assert!((times.iter().sum::<f32>()/8.0-10.0).abs()<1e-6);
    }
}
