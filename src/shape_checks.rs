//! Reproducible visual checks, using the production film renderer.
//! `cargo test --release shape_film_checks -- --ignored --nocapture`
use std::sync::Arc;
use std::time::Duration;

use glam::{Mat4, Vec3};
use serde_json::{Value, json};

use crate::{
    clay::Clay,
    film,
    material::Material,
    scene::{Scene, Shape},
    sculpt,
};

fn clay(blobs: Value, cell: f32) -> Clay {
    serde_json::from_value(json!({"blobs":blobs,"resolution":192,"detail_cell":cell})).unwrap()
}

fn stroke(c: &mut Clay, value: Value) {
    let s: sculpt::Stroke = serde_json::from_value(value).unwrap();
    sculpt::apply(c, &s).unwrap();
}

fn pair(name: &str, before: Clay, after: Clay, target: Vec3, radius: f32, yaw: f32) {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("shape_check");
    std::fs::create_dir_all(&dir).unwrap();
    let material = Material::preset("clay").unwrap();
    let angle = yaw.to_radians();
    let cam = film::Camera {
        eye: target + Vec3::new(angle.sin(), 0.15, angle.cos()).normalize() * radius * 3.8,
        target,
        up: Vec3::Y,
        fov_deg: 30.0,
        ortho: None,
    };
    let settings = film::Settings {
        width: 384,
        height: 384,
        samples: 48,
        bounces: 4,
        time_limit: Duration::from_secs(120),
        floor: false,
        exposure: 1.0,
        denoise: true,
    };
    let mut images = Vec::new();
    for (label, c) in [("before", before), ("after", after)] {
        let mut scene = Scene::default();
        let id = scene.add(Shape::Clay);
        let o = scene.get_mut(id).unwrap();
        o.name = name.into();
        o.clay = Some(c.clone());
        std::fs::write(dir.join(format!("{name}_{label}.json")), serde_json::to_vec(&scene).unwrap()).unwrap();
        let mesh = c.build_fine(&[], "clay").unwrap();
        let item = film::Item {
            mesh: Arc::new(mesh),
            world: Mat4::IDENTITY,
            tint: [1.0; 3],
            surface: material.surface(),
            material: material.clone(),
        };
        let img = film::render(vec![item], &[], &cam, settings, &Default::default());
        assert_eq!(img.samples, settings.samples);
        images.push(img);
    }
    let mut pixels = Vec::with_capacity(768 * 384 * 3);
    for y in 0..384 {
        for img in &images {
            for rgb in &img.pixels[y * 384..(y + 1) * 384] {
                pixels.extend(rgb);
            }
        }
    }
    image::save_buffer(dir.join(format!("{name}.png")), &pixels, 768, 384, image::ColorType::Rgb8).unwrap();
    eprintln!("{name}: saved before (left) / after (right)");
}

#[test]
#[ignore]
fn shape_film_checks() {
    // A cupped ear with a thin upper tip, rather than a solid sphere.
    let ear = clay(
        json!([
            {"type":"ellipsoid","name":"ear","center":[0,0,0],"radii":[0.038,0.078,0.018],"blend":0,"color":[0.76,0.52,0.40]},
            {"type":"ellipsoid","center":[0,-0.008,0.017],"radii":[0.024,0.05,0.012],"op":"carve","blend":0.004}
        ]),
        0.001,
    );
    let mut stretched = ear.clone();
    stroke(
        &mut stretched,
        json!({"brush":"stretch","points":[[0.007,0.062,0.006]],"radius":0.035,"delta":[0.04,0.085,0]}),
    );
    pair("ear_stretch", ear, stretched, Vec3::new(0.012, 0.045, 0.0), 0.145, 15.0);

    // Stylized head with soft lids and lips; sharp crease strokes articulate
    // their meeting lines without replacing the underlying anatomy blobs.
    let face = clay(
        json!([
            {"type":"ellipsoid","name":"head","center":[0,0,0],"radii":[0.11,0.15,0.09],"blend":0.005,"color":[0.76,0.52,0.40]},
            {"type":"ellipsoid","center":[0.046,0.036,0.081],"radii":[0.027,0.014,0.012],"blend":0.01,"mirror":true,"color":[0.76,0.52,0.40]},
            {"type":"ellipsoid","center":[0,-0.044,0.086],"radii":[0.039,0.012,0.014],"blend":0.008,"color":[0.76,0.48,0.39]},
            {"type":"ellipsoid","center":[0,-0.061,0.085],"radii":[0.036,0.011,0.013],"blend":0.008,"color":[0.76,0.48,0.39]},
            {"type":"capsule","a":[0,0.03,0.085],"b":[0,-0.015,0.101],"radius":0.013,"radius_b":0.018,"blend":0.012,"color":[0.76,0.52,0.40]}
        ]),
        0.001,
    );
    let mut crisp = face.clone();
    stroke(
        &mut crisp,
        json!({"brush":"crease","points":[[0.025,0.035,0.092],[0.046,0.028,0.096],[0.068,0.035,0.089]],"radius":0.005,"strength":0.003,"normal":[0,0,1],"mirror":false}),
    );
    // Mirrors are normally dispatched by the app; this direct check calls
    // apply explicitly for both sides.
    stroke(
        &mut crisp,
        json!({"brush":"crease","points":[[-0.025,0.035,0.092],[-0.046,0.028,0.096],[-0.068,0.035,0.089]],"radius":0.005,"strength":0.003,"normal":[0,0,1]}),
    );
    stroke(
        &mut crisp,
        json!({"brush":"crease","points":[[-0.032,-0.053,0.092],[0,-0.053,0.099],[0.032,-0.053,0.092]],"radius":0.004,"strength":0.003,"normal":[0,0,1]}),
    );
    pair("face_creases", face, crisp, Vec3::ZERO, 0.17, 0.0);

    let tunic = clay(
        json!([
            {"type":"ellipsoid","name":"body","center":[0,0,0],"radii":[0.16,0.25,0.09],"blend":0,"color":[0.76,0.52,0.40]},
            {"type":"sphere","center":[0.055,0.08,0.084],"radius":0.014,"blend":0.005,"mirror":true},
            {"type":"sphere","center":[0,-0.06,0.09],"radius":0.012,"op":"carve","blend":0.003},
            {"type":"box","name":"tunic","center":[0,0,0],"size":[0.40,0.44,0.3],"op":"cover","offset":0.004,"blend":0.003,"color":[0.18,0.36,0.26],"material":"cloth"}
        ]),
        0.002,
    );
    let mut loose = tunic.clone();
    loose.blobs.last_mut().unwrap().smooth = 0.035;
    pair("tunic_smooth", tunic, loose, Vec3::ZERO, 0.29, 12.0);

    let sleeve = clay(
        json!([
            {"type":"capsule","name":"sleeve","a":[0,-0.14,0],"b":[0,0.14,0],"radius":0.055,"radius_b":0.073,"blend":0,"color":[0.18,0.36,0.26],"material":"cloth"}
        ]),
        0.0015,
    );
    let mut folds = sleeve.clone();
    stroke(
        &mut folds,
        json!({"brush":"noise","pattern":"folds","points":[[0,-0.14,0.05],[0,0.14,0.065]],"radius":0.13,"scale":0.035,"strength":0.8,"normal":[0,-1,0],"mask":{"blobs":["sleeve"]}}),
    );
    pair("sleeve_folds", sleeve, folds, Vec3::ZERO, 0.23, 10.0);

    let tail = clay(
        json!([
            {"type":"capsule","a":[0,0.2,0],"b":[0,-0.2,0],"radius":0.025,"radius_b":0.012,"blend":0,"color":[0.26,0.13,0.065]}
        ]),
        0.001,
    );
    let braid = clay(
        json!([
            {"type":"chain","name":"braid","points":[[0,0.2,0],[0.02,0.05,0],[0,-0.2,0]],"size":[0.04,0.075,0.026],"tilt":40,"taper":0.45,"blend":0,"color":[0.26,0.13,0.065],"material":"hair"}
        ]),
        0.001,
    );
    pair("chain_braid", tail, braid, Vec3::ZERO, 0.25, 15.0);
}
