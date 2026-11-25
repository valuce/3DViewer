#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod camera;
mod loaders;
mod render;
mod scene;
mod settings;
mod ui;

use std::sync::Arc;

fn main() -> eframe::Result {
    app::process_start();
    let args: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();

    // `viewer3d --info file` prints a summary without opening a window (handy for scripts).
    if args.first().is_some_and(|a| a == "--info") {
        for p in &args[1..] {
            let p = std::path::Path::new(p);
            match loaders::load_file(p) {
                Ok(s) => println!(
                    "{}: {} | {} meshes, {} verts, {} tris, {} materials, {} textures | {:.1} ms",
                    p.display(),
                    s.info.format,
                    s.meshes.len(),
                    s.vertex_count(),
                    s.triangle_count(),
                    s.materials.len(),
                    s.textures.len(),
                    s.info.load_ms
                ),
                Err(e) => eprintln!("{}: error: {e:#}", p.display()),
            }
        }
        return Ok(());
    }
    let initial = args.first().map(std::path::PathBuf::from);

    let mut wgpu_setup = egui_wgpu::WgpuSetupCreateNew::without_display_handle();
    wgpu_setup.device_descriptor = Arc::new(|adapter| wgpu::DeviceDescriptor {
        label: Some("viewer device"),
        required_features: adapter.features() & wgpu::Features::POLYGON_MODE_LINE,
        required_limits: adapter.limits(),
        ..Default::default()
    });
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("3DViewer")
            .with_inner_size([1280.0, 800.0])
            .with_min_inner_size([480.0, 320.0])
            .with_drag_and_drop(true),
        wgpu_options: egui_wgpu::WgpuConfiguration { wgpu_setup: egui_wgpu::WgpuSetup::CreateNew(wgpu_setup), ..Default::default() },
        depth_buffer: 0,
        multisampling: 0,
        ..Default::default()
    };
    eframe::run_native("3DViewer", options, Box::new(move |cc| Ok(Box::new(app::ViewerApp::new(cc, initial)))))
}
