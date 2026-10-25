//! The sculpt timing report behind the "Today" column of
//! docs/SCULPT_PLAN.md section 7 (it began as a scratch bench):
//! `cargo test --release sculpt_timing_report -- --ignored --nocapture`.
//! It prints every timing and logs it to target/perf.json. Single runs,
//! not medians: the slow steps take seconds.

use std::time::Instant;

use glam::Vec3;
use serde_json::{Value, json};

use super::{figure, perf};
use crate::clay::Clay;
use crate::sculpt::Stroke;

fn timed<R>(label: &str, extra: impl FnOnce(&R) -> Value, f: impl FnOnce() -> R) -> R {
    let t = Instant::now();
    let r = f();
    let ms = t.elapsed().as_secs_f64() * 1000.0;
    let extra = extra(&r);
    eprintln!("TIMING {label}: {ms:.1} ms {extra}");
    perf::log(&format!("timing: {label}"), ms, extra);
    r
}

fn none<R>(_: &R) -> Value {
    json!({})
}

fn stroke(v: Value) -> Stroke {
    serde_json::from_value(v).expect("stroke")
}

fn tris(m: &crate::mesh::Mesh) -> Value {
    json!({ "triangles": m.indices.len() / 3 })
}

#[test]
#[ignore = "a report, about a minute: cargo test --release sculpt_timing_report -- --ignored --nocapture"]
fn sculpt_timing_report() {
    // Regular meshes; the figure has box feet, so 96 (the default) is
    // floored to 192 (see harness/baseline.rs).
    for res in [64u32, 96, 128, 192, 256, 384, 512] {
        let c = figure(res, true);
        timed(&format!("build_mesh hard figure res {res}"), tris, || c.build_mesh_with(&[], ""));
    }
    for res in [96u32, 128, 192, 256] {
        let c = figure(res, false);
        timed(&format!("build_mesh organic figure res {res}"), tris, || c.build_mesh_with(&[], ""));
    }

    // Dab costs at the mesh cell and at fine detail cells.
    for cell in [None, Some(0.002f32), Some(0.001f32)] {
        let mut c = figure(96, true);
        if let Some(cell) = cell {
            c.set_detail_cell(cell).unwrap();
        }
        let dabs = [
            json!({"brush":"inflate","points":[[0.0,1.25,0.18]],"radius":0.03}),
            json!({"brush":"inflate","points":[[0.0,1.25,0.18]],"radius":0.08}),
            json!({"brush":"smooth","points":[[0.0,1.25,0.18]],"radius":0.05}),
            json!({"brush":"grab","points":[[0.0,1.25,0.18]],"radius":0.05,"delta":[0,0,0.02]}),
            json!({"brush":"crease","points":[[0.0,1.25,0.18]],"radius":0.02,"strength":0.003}),
        ];
        for d in dabs {
            let label = format!("dab {} r={} cell={cell:?}", d["brush"].as_str().unwrap(), d["radius"]);
            let s = stroke(d);
            timed(&label, |r: &Result<crate::sculpt::Report, String>| json!({ "changed_points": r.as_ref().map(|r| r.changed_points).ok() }), || crate::sculpt::apply(&mut c, &s)).unwrap();
        }
        let path: Vec<[f32; 3]> = (0..12).map(|i| [-0.15 + i as f32 * 0.025, 1.3, 0.17]).collect();
        let s = stroke(json!({"brush":"add","points":path,"radius":0.015}));
        timed(&format!("path add 12 points r=0.015 cell={cell:?}"), none, || crate::sculpt::apply(&mut c, &s)).unwrap();
        let bricks = c.detail.as_ref().map_or(0, |d| d.grid().total_bricks());
        timed(&format!("rebuild hard figure res 96 after 6 dabs cell={cell:?}"), |m: &crate::mesh::Mesh| json!({ "triangles": m.indices.len() / 3, "bricks": bricks }), || c.build_mesh_with(&[], ""));
        if cell.is_some() {
            let fine = timed(&format!("build_fine cell={cell:?}"), |m: &Result<crate::mesh::Mesh, String>| json!({ "triangles": m.as_ref().map(|m| m.indices.len() / 3).ok() }), || c.build_fine(&[], ""));
            // Hover picking today: brute-force rays over every triangle.
            if let Ok(m) = fine {
                timed(&format!("brute-force pick on the fine mesh cell={cell:?}"), |h: &Option<(f32, usize)>| json!({ "hit": h.is_some(), "triangles": m.indices.len() / 3 }), || m.ray_hit_triangle(Vec3::new(0.0, 1.25, 2.0), Vec3::NEG_Z));
            }
        }
        let bytes = timed("serialize clay json", |b: &usize| json!({ "bytes": b }), || serde_json::to_vec(&c).unwrap().len());
        perf::log(&format!("saved clay after 6 dabs cell={cell:?}"), 0.0, json!({ "bytes": bytes }));
    }

    // Interactive loop today: a dab then a full remesh (no chunking).
    let mut c = figure(128, false);
    let mut total = 0.0;
    for i in 0..10 {
        let s = stroke(json!({"brush":"inflate","points":[[-0.05 + i as f32 * 0.01, 1.25, 0.178]],"radius":0.03}));
        let t = Instant::now();
        crate::sculpt::apply(&mut c, &s).unwrap();
        let _ = c.build_mesh_with(&[], "");
        total += t.elapsed().as_secs_f64() * 1000.0;
    }
    eprintln!("TIMING dab + full remesh, organic res 128, mean of 10: {:.1} ms", total / 10.0);
    perf::log("timing: dab + full remesh organic res 128 (mean of 10)", total / 10.0, json!({}));
    let mut hard = figure(96, true);
    let s = stroke(json!({"brush":"inflate","points":[[0.0,1.25,0.18]],"radius":0.03}));
    timed("dab + full remesh hard figure res 96 (floored)", none, || {
        crate::sculpt::apply(&mut hard, &s).unwrap();
        hard.build_mesh_with(&[], "")
    });

    // Paint and the shading-only refresh.
    let mut c = figure(128, false);
    let m = c.build_mesh_with(&[], "");
    let ps: crate::paint::PaintStroke = serde_json::from_value(json!({"points":[[0.0,1.25,0.178]],"radius":0.05,"color":[1,0,0]})).unwrap();
    timed("paint dab r=0.05 organic res 128", none, || crate::paint::stroke(&mut c, &ps)).unwrap();
    timed("mesh_shading::refresh organic res 128", |r: &crate::mesh::Mesh| json!({ "vertices": r.vertices.len() }), || crate::mesh_shading::refresh(&c, &[], "", &m, &[]));
    timed("brute-force pick organic res 128", |_: &Option<(f32, usize)>| tris(&m), || m.ray_hit_triangle(Vec3::new(0.0, 1.25, 2.0), Vec3::NEG_Z));

    // The Human form: the costliest field.
    let h: Clay = serde_json::from_value(json!({"blobs":[{"type":"human"}]})).unwrap();
    timed("human build res 96 (cold, loads the base)", tris, || h.build_mesh_with(&[], ""));
    timed("human build res 96 (warm)", tris, || h.build_mesh_with(&[], ""));
    let mut h2 = h.clone();
    h2.resolution = 192;
    timed("human build res 192", tris, || h2.build_mesh_with(&[], ""));
    let mut h3 = h.clone();
    let s = stroke(json!({"brush":"inflate","points":[[0.0,1.3,0.12]],"radius":0.04}));
    timed("human dab inflate r=0.04", none, || crate::sculpt::apply(&mut h3, &s)).unwrap();
    timed("human rebuild res 96 after a dab", tris, || h3.build_mesh_with(&[], ""));
}
