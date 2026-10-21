//! Clide3D: a small Maya-style 3D scene editor.
//!
//! Layout: `app` owns the UI and editor state, `scene` is the saved data,
//! `mesh` builds primitive shapes (and ray picking), `camera` is the orbit
//! camera, `render` draws the 3D viewport with wgpu inside egui, and `mcp`
//! is the built-in MCP server that lets AI agents drive the editor.

#![recursion_limit = "256"]

mod adaptive;
mod app;
mod bake;
mod camera;
mod clay;
mod cloth;
#[cfg(test)]
mod cloth_checks;
mod detail;
#[cfg(test)]
mod detail_checks;
mod damage;
#[cfg(test)]
mod damage_checks;
#[cfg(test)]
mod paint_checks;
#[cfg(test)]
mod closeup_checks;
mod envkit;
mod eyeball;
mod face;
mod face_hair;
mod fbx;
mod film;
mod fire;
#[cfg(test)]
mod fire_checks;
mod gltf;
mod export_atlas;
mod glb_read;
mod hair;
mod human;
mod kitbash;
mod material;
mod mcp;
mod mesh;
mod mesh_shading;
mod material_management;
mod replies;
mod paint;
mod pose;
#[cfg(test)]
mod pose_tests;
mod progress;
mod render;
mod rig;
mod scene;
mod sculpt;
#[cfg(test)]
mod shape_checks;
mod smart;
#[cfg(test)]
mod smart_check;
#[cfg(test)]
mod hard_checks;
mod surface;
#[cfg(test)]
mod texture_check;
mod weather;

/// macOS "App Nap" slows down apps whose windows are hidden, and agents
/// drive the editor while it sits behind other windows: opt out for the
/// whole run (the returned token must stay alive).
#[cfg(target_os = "macos")]
fn no_app_nap() -> objc2::rc::Retained<objc2::runtime::ProtocolObject<dyn objc2::runtime::NSObjectProtocol>> {
    use objc2_foundation::{NSActivityOptions, NSProcessInfo, NSString};
    NSProcessInfo::processInfo().beginActivityWithOptions_reason(NSActivityOptions::UserInitiatedAllowingIdleSystemSleep, &NSString::from_str("AI agents work in the editor in the background"))
}

fn main() -> eframe::Result {
    #[cfg(target_os = "macos")]
    let _awake = no_app_nap();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1400.0, 900.0])
            .with_title("Clide3D"),
        renderer: eframe::Renderer::Wgpu,
        // The 3D viewport needs a depth buffer; MSAA smooths edges.
        depth_buffer: 24,
        multisampling: render::MSAA as u16,
        ..Default::default()
    };
    eframe::run_native(
        "clide3d",
        options,
        Box::new(|cc| Ok(Box::new(app::ClideApp::new(cc)))),
    )
}
