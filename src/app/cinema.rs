//! `render_film`: film-quality pictures from the path tracer (see
//! `film/mod.rs`), framed like `render_views`.

use std::time::Duration;

use glam::Vec3;
use serde_json::{Value, json};

use super::ClideApp;
use super::eyes::Shot;

impl ClideApp {
    /// Starts a film render job; `reply` gets the picture.
    pub(super) fn render_film(&mut self, args: &Value) -> Result<super::jobs::Started, String> {
        let resolved = crate::film::look::resolved_args(args)?;
        let args = &resolved;
        let shutter = Shutter::parse(&args["motion_blur"], args["frame"].as_f64().map(|f| f as f32))?;
        // time_limit covers the whole call, mesh building included: agent
        // connections often give up after about a minute.
        let started = std::time::Instant::now();
        let keep = (self.meshes.frame, self.meshes.show_baked, self.meshes.show_fine);
        // The rest pose unless a frame is asked for (like render_views).
        self.set_frame(args["frame"].as_f64().map(|f| f as f32));
        if let Some(b) = args["baked"].as_bool() {
            self.meshes.show_baked = b;
        }
        // Film shows the finest detail there is unless told otherwise.
        self.meshes.show_fine = args["fine"].as_bool().unwrap_or(true);
        self.meshes.sync(&self.scene);
        let job = (|| {
            let mut job = self.prepare_film(args, started)?;
            if let Some(shutter) = shutter {
                let mut temporal = Vec::new();
                for i in 0..shutter.steps {
                    let u = (i as f32 + 0.5)/shutter.steps as f32;
                    let frame = shutter.frame + shutter.open + (shutter.close-shutter.open)*u;
                    self.set_frame(Some(frame)); self.meshes.sync(&self.scene);
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
        self.meshes.sync(&self.scene);
        let job = job?;
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
        Ok(super::jobs::Started::Job(format!("Check export {}", path.display()), Box::new(move |progress| {
            let start = std::time::Instant::now();
            progress.stage("reading exported GLB", 0.0);
            progress.check()?;
            let loaded = crate::glb_read::read(&path, animation.as_deref(), frame)?;
            progress.check()?;
            let (lo, hi) = loaded.bounds;
            let center = (lo + hi) * 0.5;
            let radius = ((hi - lo).length() * 0.5).max(0.01);
            let shots = views.iter().map(|v| Shot::framing(v, center, radius).map(|s| (s.name.clone(), s.film_camera()))).collect::<Result<Vec<_>, _>>()?;
            let mut summary = loaded.summary;
            summary["path"] = json!(path);
            let settings = crate::film::Settings { time_limit: settings.time_limit.saturating_sub(start.elapsed()), ..settings };
            FilmJob { temporal: Vec::new(), look: std::sync::Arc::new(crate::film::Look::default()), fine_errors: Value::Null, items: loaded.items, fibers: Vec::new(), shots, settings, save, summary: Some(summary) }.run(progress)
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
        let mut items: Vec<crate::film::Item> = draws
            .iter()
            .filter(|d| d.object.shape.is_mesh() && d.object.shape != crate::scene::Shape::Fire)
            .filter_map(|d| {
                let mesh = self.meshes.get(d.object)?.clone();
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
        let items: Vec<crate::film::Item> = items.into_iter().filter(|i| i.mesh.textures.as_ref().is_none_or(|t| !t.alpha)).collect();
        let shots = views.iter().map(|v| {
            let s = Shot::framing(v, center, radius)?;
            Ok((s.name.clone(), look.camera.apply(s.film_camera())?))
        }).collect::<Result<Vec<_>, String>>()?;
        let save = save_png_path(args)?;
        Ok(FilmJob { temporal: Vec::new(), look, fine_errors: self.fine_errors_json(), items, fibers, shots, settings, save, summary: None })
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
    items: Vec<crate::film::Item>,
    fibers: Vec<crate::film::Fiber>,
    shots: Vec<(String, crate::film::Camera)>,
    settings: crate::film::Settings,
    save: Option<std::path::PathBuf>,
    summary: Option<Value>,
}

impl FilmJob {
    fn run(mut self, progress: &crate::progress::Progress) -> Result<super::jobs::Finish, String> {
        let start = std::time::Instant::now();
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
        let images = if self.temporal.is_empty() {
            let meshes = crate::film::Meshes::new(items);
            let settings = crate::film::Settings { time_limit: self.settings.time_limit.saturating_sub(start.elapsed()), ..self.settings };
            crate::film::render_views_with_look(&meshes, &self.fibers, &cams, settings, self.look.clone(), progress)
        } else {
            let mut accumulated: Vec<crate::film::Image> = Vec::new();
            let n = self.temporal.len();
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
        for ((name, _), img) in self.shots.iter().zip(images) {
            stats.push(json!({ "view": name, "samples": img.samples, "seconds": (img.seconds * 10.0).round() / 10.0 }));
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
        });
        summary.as_object_mut().expect("summary is an object").extend(render.as_object().unwrap().clone());
        let mut out = super::tools::text(summary);
        out.extend(super::tools::encode_png(&super::tools::downscale(&sheet, 1600))?);
        Ok(Box::new(move |_| Ok(out)))
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
        let job=|x:f32| FilmJob{temporal:Vec::new(),look:look.clone(),fine_errors:Value::Null,items:make(x),fibers:Vec::new(),shots:vec![("motion".into(),cam)],settings,save:None,summary:None};
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
