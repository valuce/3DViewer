#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod anim;
mod app;
mod assoc;
mod camera;
mod i18n;
mod loaders;
mod net;
mod player;
mod render;
mod scene;
mod settings;
mod tools;
mod ui;
mod update;

use std::sync::Arc;

fn window_icon() -> egui::IconData {
    match image::load_from_memory(include_bytes!("../assets/icon_64.png")) {
        Ok(img) => {
            let img = img.into_rgba8();
            egui::IconData { width: img.width(), height: img.height(), rgba: img.into_raw() }
        }
        Err(_) => egui::IconData::default(),
    }
}

fn main() -> eframe::Result {
    app::process_start();
    let args: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    if args.first().is_some_and(|a| a.to_str().is_some_and(|a| a.starts_with("--"))) {
        // Release builds use the GUI subsystem; reuse the parent console for CLI output.
        // SAFETY: plain Win32 call.
        unsafe {
            windows_sys::Win32::System::Console::AttachConsole(windows_sys::Win32::System::Console::ATTACH_PARENT_PROCESS);
        }
    }

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
            if let Ok(s) = loaders::load_file(p) {
                if let Some(a) = &s.anim {
                    for c in &a.clips {
                        let kinds: Vec<String> = c
                            .tracks
                            .iter()
                            .map(|t| {
                                let k = match &t.values {
                                    anim::TrackValues::Translation(_) => "T",
                                    anim::TrackValues::Rotation(_) => "R",
                                    anim::TrackValues::Scale(_) => "S",
                                    anim::TrackValues::Weights(_) => "W",
                                };
                                format!("{}:{k}{}", a.nodes[t.node].name, t.times.len())
                            })
                            .collect();
                        println!("  clip '{}' {:.2}s: {}", c.name, c.duration, kinds.join(" "));
                    }
                    for m in &s.meshes {
                        println!(
                            "  mesh '{}' node {:?} skin {:?} morphs {} joints {}",
                            m.name,
                            m.node,
                            m.skin,
                            m.morph_targets.len(),
                            m.joints.len()
                        );
                    }
                }
            }
        }
        return Ok(());
    }
    // Used by the updater to check that a downloaded build starts and reports its version.
    if args.first().is_some_and(|a| a == "--version") {
        println!("3DViewer {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    // `--register` links every format, `--register glb stl` only the listed ones.
    if args.first().is_some_and(|a| a == "--register" || a == "--unregister") {
        let r = if args[0] == "--register" {
            let wanted: Vec<String> = args[1..].iter().map(|a| a.to_string_lossy().trim_start_matches('.').to_ascii_lowercase()).collect();
            let exts: Vec<&str> =
                loaders::SUPPORTED_EXTENSIONS.iter().copied().filter(|e| wanted.is_empty() || wanted.iter().any(|w| w == e)).collect();
            if exts.is_empty() {
                eprintln!("error: none of the given extensions is supported ({})", loaders::SUPPORTED_EXTENSIONS.join(" "));
                return Ok(());
            }
            assoc::register(&exts).map(|n| println!("linked {} ({n} open in 3DViewer by default)", exts.join(" ")))
        } else {
            assoc::unregister(loaders::SUPPORTED_EXTENSIONS).map(|_| println!("unregistered"))
        };
        if let Err(e) = r {
            eprintln!("error: {e:#}");
        }
        return Ok(());
    }
    let initial = args.first().map(std::path::PathBuf::from);
    update::cleanup_leftovers();
    update::sync_installed_version();

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
            .with_drag_and_drop(true)
            .with_icon(Arc::new(window_icon())),
        wgpu_options: egui_wgpu::WgpuConfiguration { wgpu_setup: egui_wgpu::WgpuSetup::CreateNew(wgpu_setup), ..Default::default() },
        depth_buffer: 0,
        multisampling: 0,
        ..Default::default()
    };
    eframe::run_native("3DViewer", options, Box::new(move |cc| Ok(Box::new(app::ViewerApp::new(cc, initial)))))
}
