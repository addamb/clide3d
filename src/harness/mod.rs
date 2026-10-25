//! Verification harness for the sculpt work (docs/SCULPT_PLAN.md, Stage
//! 0): geometry measures (`geom`), image goldens (`image`, `CLIDE_GPU=1`),
//! perf assertions logged to target/perf.json (`perf`), the meshing
//! baselines and findings they back (`baseline`), and the sculpt timing
//! report (`timing`).
//!
//! It lives in the crate rather than `tests/`: clide3d is a binary crate,
//! so integration tests could not reach `Clay` or the renderer. Goldens
//! are still kept in `tests/golden/`.

pub mod geom;
pub mod image;
pub mod perf;

mod baseline;
pub(crate) mod live;
mod timing;

use serde_json::json;

use crate::clay::Clay;

/// A clay sphere of `radius` at the origin, meshed `res` cells across.
pub fn sphere(radius: f32, res: u32) -> Clay {
    serde_json::from_value(json!({ "resolution": res, "blobs": [{ "type": "sphere", "center": [0, 0, 0], "radius": radius }] })).expect("sphere clay")
}

/// A blobby standing figure about 1.8 units tall (the sculpt timing
/// bench's subject). `foot` adds rounded-box feet, which make it a
/// hard-surface clay (see `Clay::mesh_resolution`).
pub fn figure(res: u32, foot: bool) -> Clay {
    let mut blobs = vec![
        json!({"type":"ellipsoid","center":[0,1.2,0],"radii":[0.25,0.35,0.18],"name":"torso","blend":0.08}),
        json!({"type":"sphere","center":[0,1.72,0],"radius":0.13,"name":"head","blend":0.05}),
        json!({"type":"capsule","a":[0,1.5,0],"b":[0,1.62,0],"radius":0.06,"name":"neck","blend":0.04}),
        json!({"type":"capsule","a":[0.25,1.45,0],"b":[0.5,1.1,0],"radius":0.06,"radius_b":0.045,"mirror":true,"name":"arm","blend":0.04}),
        json!({"type":"capsule","a":[0.5,1.1,0],"b":[0.62,0.85,0.08],"radius":0.045,"radius_b":0.035,"mirror":true,"name":"fore","blend":0.03}),
        json!({"type":"capsule","a":[0.12,0.9,0],"b":[0.14,0.48,0.02],"radius":0.09,"radius_b":0.06,"mirror":true,"name":"thigh","blend":0.05}),
        json!({"type":"capsule","a":[0.14,0.48,0.02],"b":[0.14,0.08,0],"radius":0.06,"radius_b":0.04,"mirror":true,"name":"shin","blend":0.04}),
        json!({"type":"sphere","center":[0.05,1.75,0.11],"radius":0.025,"mirror":true,"op":"carve","name":"eye","blend":0.01}),
        json!({"type":"ellipsoid","center":[0,1.7,0.13],"radii":[0.02,0.035,0.03],"name":"nose","blend":0.02}),
        json!({"type":"ellipsoid","center":[0,1.0,0],"radii":[0.22,0.15,0.16],"name":"hips","blend":0.08}),
    ];
    if foot {
        blobs.push(json!({"type":"box","center":[0.14,0.03,0.05],"size":[0.09,0.06,0.22],"round":0.02,"mirror":true,"name":"foot","blend":0.02}));
    }
    serde_json::from_value(json!({ "resolution": res, "blobs": blobs })).expect("figure clay")
}
