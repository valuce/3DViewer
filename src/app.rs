//! Application state: loading, camera input, overlays and the 3D view.

use crate::camera::{OrbitCamera, ViewPreset};
use crate::i18n::tr;
use crate::loaders;
use crate::render::{FrameParams, LineVertex, Renderer};
use crate::scene::{Aabb, Scene, Topology};
use crate::settings::{DisplayMode, Settings};
use crate::tools::{Measure, Tool};
use glam::Vec3;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Instant;

pub struct MeshInfo {
    pub name: String,
    pub verts: usize,
    pub tris: usize,
    pub topology: Topology,
    pub material: Option<String>,
    pub uv: bool,
    pub colors: bool,
    pub had_normals: bool,
}

pub struct MatInfo {
    pub name: String,
    pub base_color: [f32; 4],
    pub metallic: f32,
    pub roughness: f32,
    pub maps: Vec<&'static str>,
    pub blend: bool,
    pub double_sided: bool,
}

/// Everything the info panel shows; the CPU scene itself is dropped after upload.
pub struct Summary {
    pub path: PathBuf,
    pub format: String,
    pub file_size: u64,
    pub load_ms: f64,
    pub upload_ms: f64,
    pub vertices: usize,
    pub triangles: usize,
    pub points: usize,
    pub lines: usize,
    pub meshes: Vec<MeshInfo>,
    pub materials: Vec<MatInfo>,
    pub textures: Vec<(String, u32, u32)>,
    pub bounds: Aabb,
    pub info: crate::scene::SceneInfo,
    pub gpu_bytes: u64,
}

impl Summary {
    fn new(path: &Path, s: &Scene) -> Self {
        let mut points = 0;
        let mut lines = 0;
        for m in &s.meshes {
            match m.topology {
                Topology::Points => points += m.positions.len(),
                Topology::Lines => lines += m.indices.len() / 2,
                Topology::Triangles => {}
            }
        }
        Self {
            path: path.to_path_buf(),
            format: s.info.format.clone(),
            file_size: s.info.file_size,
            load_ms: s.info.load_ms,
            upload_ms: 0.0,
            vertices: s.vertex_count(),
            triangles: s.triangle_count(),
            points,
            lines,
            meshes: s
                .meshes
                .iter()
                .map(|m| MeshInfo {
                    name: m.name.clone(),
                    verts: m.positions.len(),
                    tris: m.triangle_count(),
                    topology: m.topology,
                    material: m.material.and_then(|i| s.materials.get(i)).map(|x| x.name.clone()),
                    uv: !m.uvs.is_empty(),
                    colors: !m.colors.is_empty(),
                    had_normals: m.had_normals,
                })
                .collect(),
            materials: s
                .materials
                .iter()
                .map(|m| {
                    let mut maps = Vec::new();
                    for (t, n) in [
                        (m.base_color_tex, tr("цвет", "colour")),
                        (m.normal_tex, tr("нормали", "normal")),
                        (m.mr_tex, tr("металл/шерох.", "metal/rough")),
                        (m.occlusion_tex, "AO"),
                        (m.emissive_tex, tr("свечение", "emissive")),
                    ] {
                        if t.is_some() {
                            maps.push(n);
                        }
                    }
                    MatInfo {
                        name: m.name.clone(),
                        base_color: m.base_color,
                        metallic: m.metallic,
                        roughness: m.roughness,
                        maps,
                        blend: m.alpha_mode == crate::scene::AlphaMode::Blend,
                        double_sided: m.double_sided,
                    }
                })
                .collect(),
            textures: s.textures.iter().map(|t| (t.name.clone(), t.width, t.height)).collect(),
            bounds: s.bounds(),
            info: s.info.clone(),
            gpu_bytes: 0,
        }
    }
}

struct Loading {
    path: PathBuf,
    rx: mpsc::Receiver<anyhow::Result<Scene>>,
    started: Instant,
    keep_camera: bool,
}

pub struct ViewerApp {
    pub renderer: Option<Renderer>,
    pub settings: Settings,
    pub camera: OrbitCamera,
    pub summary: Option<Summary>,
    loading: Option<Loading>,
    pub error: Option<String>,
    pub show_help: bool,
    pub show_view_settings: bool,
    pub hovered_mesh: Option<usize>,
    /// Multiply materials by vertex colours (off for FBX/OBJ where they are usually masks).
    pub vertex_colors: bool,
    pub player: Option<crate::player::Player>,
    /// Current file is a DCC/game format (vertex colours often hold masks).
    pub dcc_format: bool,
    pub show_assoc: bool,
    pub show_background: bool,
    logo: Option<egui::TextureHandle>,
    pub assoc_status: Option<String>,
    /// Association table shown in the window (refreshed when it opens and after Apply).
    pub assoc_rows: Vec<crate::assoc::ExtState>,
    /// Extensions ticked in the associations window.
    pub assoc_sel: Vec<&'static str>,
    pub updater: crate::update::Updater,
    pub show_update: bool,
    /// A newer release was found by the automatic check and not dismissed yet.
    pub update_banner: bool,
    /// Start the freshly installed version when this one exits.
    pub restart_after_exit: bool,
    applied_highlight: Option<usize>,
    pub last_frame_ms: f32,
    pub startup_ms: Option<f64>,
    pending_open: Option<PathBuf>,
    pub tool: Tool,
    pub slice: crate::tools::Slice,
    pub measures: Vec<Measure>,
    /// Points of the measurement being placed.
    pub pending: Vec<Vec3>,
    /// Surface point under the cursor while measuring.
    hover_pick: Option<Vec3>,
    /// First-person speed in model units per second.
    pub fly_speed: f32,
    /// Environment currently on the GPU (preset, custom file).
    env_uploaded: Option<(crate::render::env::EnvPreset, Option<PathBuf>)>,
    pub env_error: Option<String>,
    env_previews: Vec<(crate::render::env::EnvPreset, egui::TextureHandle)>,
}

pub fn process_start() -> Instant {
    static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    *START.get_or_init(Instant::now)
}

impl ViewerApp {
    pub fn new(cc: &eframe::CreationContext, initial: Option<PathBuf>) -> Self {
        let settings: Settings = cc.storage.and_then(|s| eframe::get_value(s, eframe::APP_KEY)).unwrap_or_default();
        crate::i18n::set_lang(settings.language.unwrap_or_else(crate::i18n::detect));
        let mut fonts = egui::FontDefinitions::default();
        egui_phosphor::add_to_fonts(&mut fonts, egui_phosphor::Variant::Regular);
        cc.egui_ctx.set_fonts(fonts);
        let renderer = cc.wgpu_render_state.as_ref().map(Renderer::new);
        let mut visuals = egui::Visuals::dark();
        visuals.panel_fill = egui::Color32::from_rgb(30, 31, 36);
        visuals.window_fill = egui::Color32::from_rgb(34, 35, 41);
        cc.egui_ctx.set_visuals(visuals);
        let mut camera = OrbitCamera::default();
        camera.ortho = settings.ortho;
        camera.fov_y = settings.fov_deg.to_radians();
        let mut app = Self {
            renderer,
            settings,
            camera,
            summary: None,
            loading: None,
            error: None,
            show_help: false,
            show_view_settings: false,
            hovered_mesh: None,
            vertex_colors: true,
            player: None,
            dcc_format: false,
            show_assoc: false,
            show_background: false,
            logo: image::load_from_memory(include_bytes!("../assets/icon_256.png")).ok().map(|i| {
                let i = i.into_rgba8();
                let ci = egui::ColorImage::from_rgba_unmultiplied([i.width() as usize, i.height() as usize], &i);
                cc.egui_ctx.load_texture("logo", ci, egui::TextureOptions::LINEAR)
            }),
            assoc_status: None,
            assoc_rows: Vec::new(),
            assoc_sel: Vec::new(),
            updater: Default::default(),
            show_update: false,
            update_banner: false,
            restart_after_exit: false,
            applied_highlight: None,
            last_frame_ms: 0.0,
            startup_ms: None,
            pending_open: initial,
            tool: Tool::Orbit,
            slice: Default::default(),
            measures: Vec::new(),
            pending: Vec::new(),
            hover_pick: None,
            fly_speed: 1.0,
            env_uploaded: None,
            env_error: None,
            env_previews: Vec::new(),
        };
        app.auto_update_check(&cc.egui_ctx);
        app
    }

    /// Checks GitHub for a new release in the background, at most once a day.
    fn auto_update_check(&mut self, ctx: &egui::Context) {
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        if self.settings.auto_update && now.saturating_sub(self.settings.last_update_check) >= 24 * 3600 {
            self.settings.last_update_check = now;
            self.updater.check(ctx, true);
        }
    }

    fn poll_update(&mut self) {
        if self.updater.poll()
            && self.updater.silent
            && let crate::update::State::Available(rel) = &self.updater.state
        {
            self.update_banner = self.settings.skipped_version.as_deref() != Some(rel.version.to_string().as_str());
        }
    }

    pub fn open_assoc_window(&mut self) {
        self.assoc_rows = crate::assoc::query(crate::loaders::SUPPORTED_EXTENSIONS);
        self.assoc_sel = self.assoc_rows.iter().filter(|r| r.linked).map(|r| r.ext).collect();
        if self.assoc_sel.is_empty() {
            // Nothing linked yet: start with every format ticked.
            self.assoc_sel = crate::loaders::SUPPORTED_EXTENSIONS.to_vec();
        }
        self.assoc_status = None;
        self.show_assoc = true;
    }

    /// The model to reopen after a restart into the new version.
    pub fn current_path(&self) -> Option<PathBuf> {
        self.summary.as_ref().map(|s| s.path.clone())
    }

    pub fn is_loading(&self) -> Option<(&Path, f32)> {
        self.loading.as_ref().map(|l| (l.path.as_path(), l.started.elapsed().as_secs_f32()))
    }

    pub fn open(&mut self, ctx: &egui::Context, path: PathBuf) {
        self.start_load(ctx, path, false);
    }

    pub fn reload(&mut self, ctx: &egui::Context) {
        if let Some(p) = self.summary.as_ref().map(|s| s.path.clone()) {
            self.start_load(ctx, p, true);
        }
    }

    fn start_load(&mut self, ctx: &egui::Context, path: PathBuf, keep_camera: bool) {
        let (tx, rx) = mpsc::channel();
        let p = path.clone();
        let ctx2 = ctx.clone();
        std::thread::Builder::new()
            .name("loader".into())
            .spawn(move || {
                let r = loaders::load_file(&p);
                let _ = tx.send(r);
                ctx2.request_repaint();
            })
            .expect("spawn loader thread");
        if let Some(r) = self.renderer.as_mut() {
            r.warm_up();
        }
        self.error = None;
        self.loading = Some(Loading { path, rx, started: Instant::now(), keep_camera });
    }

    pub fn open_dialog(&mut self, ctx: &egui::Context) {
        let mut dlg = rfd::FileDialog::new().add_filter(tr("3D-модели", "3D models"), loaders::SUPPORTED_EXTENSIONS);
        for (name, exts) in [
            ("glTF / GLB", &["gltf", "glb"][..]),
            ("FBX", &["fbx"]),
            ("Wavefront OBJ", &["obj"]),
            ("STL", &["stl"]),
            ("PLY", &["ply"]),
            ("3MF", &["3mf"]),
            ("OFF", &["off"]),
            ("3DS", &["3ds"]),
            ("COLLADA", &["dae"]),
            ("Blender", &["blend"]),
        ] {
            dlg = dlg.add_filter(name, exts);
        }
        if let Some(dir) = self.summary.as_ref().and_then(|s| s.path.parent()) {
            dlg = dlg.set_directory(dir);
        }
        if let Some(p) = dlg.pick_file() {
            self.open(ctx, p);
        }
    }

    /// Open the previous/next supported file in the current file's folder.
    pub fn browse(&mut self, ctx: &egui::Context, step: isize) {
        let Some(cur) = self.summary.as_ref().map(|s| s.path.clone()).or_else(|| self.loading.as_ref().map(|l| l.path.clone())) else {
            return;
        };
        let Some(dir) = cur.parent() else { return };
        let Ok(rd) = std::fs::read_dir(dir) else { return };
        let mut files: Vec<PathBuf> = rd.flatten().map(|e| e.path()).filter(|p| p.is_file() && loaders::is_supported(p)).collect();
        if files.is_empty() {
            return;
        }
        files.sort_by_key(|p| p.file_name().map(|f| f.to_string_lossy().to_lowercase()));
        let i = files.iter().position(|p| p == &cur).unwrap_or(0) as isize;
        let n = files.len() as isize;
        let next = files[((i + step) % n + n) as usize % files.len()].clone();
        if next != cur {
            self.open(ctx, next);
        }
    }

    fn poll_loading(&mut self, ctx: &egui::Context) {
        let Some(l) = &self.loading else { return };
        match l.rx.try_recv() {
            Ok(result) => {
                let l = self.loading.take().unwrap();
                match result {
                    Ok(scene) => self.install_scene(ctx, &l.path, scene, l.keep_camera),
                    Err(e) => self.error = Some(format!("{}: {e:#}", l.path.file_name().unwrap_or_default().to_string_lossy())),
                }
            }
            Err(mpsc::TryRecvError::Empty) => {
                // Keep the spinner animating.
                ctx.request_repaint_after(std::time::Duration::from_millis(100));
            }
            Err(mpsc::TryRecvError::Disconnected) => {
                self.loading = None;
                self.error = Some(tr("загрузчик аварийно завершился", "loader crashed").into());
            }
        }
    }

    fn install_scene(&mut self, ctx: &egui::Context, path: &Path, scene: Scene, keep_camera: bool) {
        let mut summary = Summary::new(path, &scene);
        let t = Instant::now();
        if let Some(r) = self.renderer.as_mut() {
            r.upload(&scene);
            summary.gpu_bytes = r.scene.as_ref().map(|s| s.gpu_bytes).unwrap_or(0);
        }
        self.player = crate::player::Player::new(&scene);
        summary.upload_ms = t.elapsed().as_secs_f64() * 1000.0;
        drop(scene);
        if !keep_camera {
            self.dcc_format = is_dcc_format(&summary.format);
            self.vertex_colors = if self.dcc_format { self.settings.vcolor_dcc } else { self.settings.vcolor_default };
            self.camera.z_up = summary.info.up_axis_z;
            self.camera.set_preset(ViewPreset::Iso);
            self.camera.frame(&summary.bounds.view_bounds());
            self.measures.clear();
            self.pending.clear();
            self.fly_speed = default_fly_speed(summary.bounds.view_bounds().radius(), summary.info.unit_m.or(self.settings.assumed_unit_m));
        }
        self.camera.scene_radius = summary.bounds.view_bounds().radius();
        self.settings.push_recent(path.to_path_buf());
        let name = path.file_name().unwrap_or_default().to_string_lossy();
        ctx.send_viewport_cmd(egui::ViewportCommand::Title(format!("{name} — 3DViewer")));
        self.hovered_mesh = None;
        self.applied_highlight = None;
        self.summary = Some(summary);
    }

    pub fn close_model(&mut self, ctx: &egui::Context) {
        self.summary = None;
        self.player = None;
        self.measures.clear();
        self.pending.clear();
        if let Some(r) = self.renderer.as_mut() {
            r.clear_scene();
        }
        ctx.send_viewport_cmd(egui::ViewportCommand::Title("3DViewer".into()));
    }

    pub fn set_vertex_colors(&mut self, v: bool) {
        self.vertex_colors = v;
        if self.dcc_format {
            self.settings.vcolor_dcc = v;
        } else {
            self.settings.vcolor_default = v;
        }
    }

    pub fn set_mesh_visible(&mut self, i: usize, v: bool) {
        if let Some(m) = self.renderer.as_mut().and_then(|r| r.scene.as_mut()).and_then(|s| s.meshes.get_mut(i)) {
            m.visible = v;
        }
    }

    pub fn mesh_visible(&self, i: usize) -> bool {
        self.renderer.as_ref().and_then(|r| r.scene.as_ref()).and_then(|s| s.meshes.get(i)).is_none_or(|m| m.visible)
    }

    pub fn frame_model(&mut self) {
        if let Some(s) = &self.summary {
            self.camera.frame(&s.bounds.view_bounds());
        }
    }

    fn handle_keys(&mut self, ctx: &egui::Context) {
        if ctx.egui_wants_keyboard_input() {
            return;
        }
        use egui::Key;
        let (pressed, ctrl) = ctx.input(|i| {
            let keys: Vec<Key> = i
                .events
                .iter()
                .filter_map(|e| match e {
                    egui::Event::Key { key, pressed: true, repeat: false, .. } => Some(*key),
                    _ => None,
                })
                .collect();
            (keys, i.modifiers.command)
        });
        for k in pressed {
            let fly_key = matches!(
                k,
                Key::W
                    | Key::A
                    | Key::S
                    | Key::D
                    | Key::Q
                    | Key::E
                    | Key::C
                    | Key::ArrowLeft
                    | Key::ArrowRight
                    | Key::ArrowUp
                    | Key::ArrowDown
            );
            if self.tool == Tool::Fly && fly_key && !ctrl {
                continue;
            }
            let s = &mut self.settings;
            match (k, ctrl) {
                (Key::O, true) => self.open_dialog(ctx),
                (Key::Num1, true) => self.camera.set_preset(ViewPreset::Front),
                (Key::Num3, true) => self.camera.set_preset(ViewPreset::Right),
                (Key::Num7, true) => self.camera.set_preset(ViewPreset::Top),
                (Key::Num9, true) => self.camera.set_preset(ViewPreset::Bottom),
                (Key::Num1, false) => s.mode = DisplayMode::Shaded,
                (Key::Num2, false) => s.mode = DisplayMode::Clay,
                (Key::Num3, false) => s.mode = DisplayMode::Unlit,
                (Key::Num4, false) => s.mode = DisplayMode::Normals,
                (Key::Num5, false) => s.mode = DisplayMode::UvChecker,
                (Key::Num6, false) => s.mode = DisplayMode::VertexColors,
                (Key::Num7, false) => s.mode = DisplayMode::Wireframe,
                (Key::Num8, false) => s.mode = DisplayMode::Points,
                (Key::W, false) => s.wire_overlay = !s.wire_overlay,
                (Key::G, false) => s.show_grid = !s.show_grid,
                (Key::X, false) => s.show_axes = !s.show_axes,
                (Key::B, false) => s.show_bbox = !s.show_bbox,
                (Key::T, false) => s.textures = !s.textures,
                (Key::V, false) => {
                    let v = !self.vertex_colors;
                    self.set_vertex_colors(v);
                }
                (Key::L, false) => s.flat_shading = !s.flat_shading,
                (Key::C, false) => s.backface_culling = !s.backface_culling,
                (Key::I, false) => s.show_info = !s.show_info,
                (Key::P, false) => {
                    self.toggle_ortho();
                }
                (Key::U, false) => self.camera.z_up = !self.camera.z_up,
                (Key::F, false) => self.frame_model(),
                (Key::R, false) => {
                    self.camera.set_preset(ViewPreset::Iso);
                    self.frame_model();
                }
                (Key::F5, _) => self.reload(ctx),
                (Key::Space, false) => {
                    if let Some(p) = self.player.as_mut() {
                        p.toggle();
                    }
                }
                (Key::PageDown, _) | (Key::ArrowRight, false) => self.browse(ctx, 1),
                (Key::PageUp, _) | (Key::ArrowLeft, false) => self.browse(ctx, -1),
                (Key::F1, _) => self.show_help = !self.show_help,
                (Key::F11, _) => {
                    let fs = ctx.input(|i| i.viewport().fullscreen.unwrap_or(false));
                    ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(!fs));
                }
                (Key::Escape, _) => {
                    // Leave the active tool first, then the section view, then close popups.
                    if self.tool != Tool::Orbit {
                        self.set_tool(Tool::Orbit);
                    } else if self.slice.enabled {
                        self.slice.enabled = false;
                    } else {
                        self.show_help = false;
                        self.error = None;
                    }
                }
                _ => {}
            }
        }
    }

    fn handle_drops(&mut self, ctx: &egui::Context) {
        let dropped: Vec<PathBuf> =
            ctx.input(|i| i.raw.dropped_files.iter().map(|f| f.path().to_path_buf()).filter(|p| !p.as_os_str().is_empty()).collect());
        if let Some(p) = dropped.into_iter().next() {
            self.open(ctx, p);
        }
    }

    /// Overlay line lists: depth-tested (grid, axes, bounding box) and always on top
    /// (a faint copy of the axes, so the pivot shows through the model).
    fn build_overlay(&self) -> (Vec<LineVertex>, Vec<LineVertex>) {
        let mut v = Vec::new();
        let mut top = Vec::new();
        let Some(s) = &self.summary else {
            if self.settings.show_grid {
                grid(&mut v, Vec3::ZERO, 1.0, self.camera.z_up, self.settings.bg_luma() > 0.55);
            }
            return (v, top);
        };
        let b = s.bounds;
        if !b.is_valid() {
            return (v, top);
        }
        let z_up = self.camera.z_up;
        let up = if z_up { Vec3::Z } else { Vec3::Y };
        let (ax, ay) = if z_up { (Vec3::X, Vec3::Y) } else { (Vec3::X, Vec3::Z) };
        if self.settings.show_grid {
            let (center, extent) = if b.pivot_near() {
                // World grid through the origin (the pivot), wide enough for the model,
                // so an off-centre pivot or a model floating above it is visible.
                let reach = [b.min, b.max].iter().flat_map(|p| [p.dot(ax).abs(), p.dot(ay).abs()]).fold(0.0f32, f32::max);
                (Vec3::ZERO, reach.max(b.radius()))
            } else {
                // Pivot far away: the grid sits under the model instead.
                let c = b.center();
                let size = b.size();
                let horiz = size.dot(ax).max(size.dot(ay));
                (c - up * (c.dot(up) - b.min.dot(up)), horiz.max(b.radius() * 0.5))
            };
            grid(&mut v, center, extent, z_up, self.settings.bg_luma() > 0.55);
        }
        if self.settings.show_axes {
            let len = b.radius() * 0.6;
            let o = Vec3::ZERO;
            for (axis, col) in [(Vec3::X, [0.95, 0.3, 0.3, 0.9]), (Vec3::Y, [0.4, 0.9, 0.35, 0.9]), (Vec3::Z, [0.35, 0.55, 1.0, 0.9])] {
                v.push(LineVertex { pos: o.to_array(), color: col });
                v.push(LineVertex { pos: (o + axis * len).to_array(), color: col });
                let faint = [col[0], col[1], col[2], 0.5];
                top.push(LineVertex { pos: o.to_array(), color: faint });
                top.push(LineVertex { pos: (o + axis * len).to_array(), color: faint });
            }
        }
        if self.settings.show_bbox {
            let col = [1.0, 0.85, 0.2, 0.9];
            let (mn, mx) = (b.min, b.max);
            let corner = |i: usize| {
                Vec3::new(if i & 1 != 0 { mx.x } else { mn.x }, if i & 2 != 0 { mx.y } else { mn.y }, if i & 4 != 0 { mx.z } else { mn.z })
            };
            for (a, bb) in [(0, 1), (2, 3), (4, 5), (6, 7), (0, 2), (1, 3), (4, 6), (5, 7), (0, 4), (1, 5), (2, 6), (3, 7)] {
                v.push(LineVertex { pos: corner(a).to_array(), color: col });
                v.push(LineVertex { pos: corner(bb).to_array(), color: col });
            }
        }
        if self.slice.plane(&b).is_some() {
            let o = self.slice.outline(&b);
            let col = [1.0, 0.72, 0.25, 0.85];
            for i in 0..4 {
                top.push(LineVertex { pos: o[i].to_array(), color: col });
                top.push(LineVertex { pos: o[(i + 1) % 4].to_array(), color: col });
            }
        }
        (v, top)
    }

    fn frame_params(&self, aspect: f32) -> FrameParams {
        let cam = &self.camera;
        let (right, up, fwd) = cam.basis();
        let light = (-fwd * 1.0 + up * 0.55 - right * 0.45).normalize();
        let s = &self.settings;
        let (top, bottom) = if s.bg_gradient { (s.bg_top, s.bg_bottom) } else { (s.bg_top, s.bg_top) };
        let (overlay, overlay_top) = self.build_overlay();
        let clip = self.summary.as_ref().and_then(|sm| self.slice.plane(&sm.bounds));
        let env_on = s.env != crate::render::env::EnvPreset::Basic;
        FrameParams {
            view_proj: cam.view_proj(aspect),
            eye: cam.eye(),
            back_dir: cam.back_dir(),
            ortho: cam.ortho,
            light_dir: light,
            light_intensity: self.settings.light_intensity,
            up: cam.up(),
            mode: self.settings.mode,
            textures: self.settings.textures,
            flat: self.settings.flat_shading,
            vertex_colors: self.vertex_colors,
            // The inside of a section must stay visible.
            cull: self.settings.backface_culling && clip.is_none(),
            wire_overlay: self.settings.wire_overlay,
            point_size: self.settings.point_size,
            bg_top: top,
            bg_bottom: bottom,
            overlay,
            overlay_top,
            clip,
            env: crate::render::EnvParams {
                lighting: env_on,
                intensity: s.env_intensity,
                rotation: s.env_rotation_deg.to_radians(),
                background: env_on && s.env_background,
                blur: s.env_blur,
            },
        }
    }

    /// The 3D viewport: input handling, rendering and on-screen overlays.
    pub fn view_3d(&mut self, ui: &mut egui::Ui, frame: &eframe::Frame) {
        let rect = ui.available_rect_before_wrap();
        let resp = ui.allocate_rect(rect, egui::Sense::click_and_drag());
        let ctx = ui.ctx().clone();
        let dt = ctx.input(|i| i.stable_dt).min(0.1);
        if self.camera.update(dt) {
            ctx.request_repaint();
        }

        // Camera input.
        let shift = ctx.input(|i| i.modifiers.shift);
        let d = resp.drag_delta();
        if self.tool == Tool::Fly {
            if resp.dragged_by(egui::PointerButton::Primary) || resp.dragged_by(egui::PointerButton::Secondary) {
                self.camera.look(d.x, d.y);
            } else if resp.dragged_by(egui::PointerButton::Middle) {
                self.camera.pan(d.x, d.y, rect.height());
            }
            if resp.hovered() {
                let scroll = ctx.input(|i| i.smooth_scroll_delta.y);
                if scroll != 0.0 {
                    let r = self.camera.scene_radius;
                    self.fly_speed = (self.fly_speed * (scroll * 0.004).exp()).clamp(r * 1e-4, r * 100.0);
                }
            }
            self.fly_keys(&ctx, dt);
        } else {
            if resp.dragged_by(egui::PointerButton::Primary) && !shift {
                self.camera.orbit(d.x, d.y);
            } else if resp.dragged_by(egui::PointerButton::Secondary)
                || resp.dragged_by(egui::PointerButton::Middle)
                || (resp.dragged_by(egui::PointerButton::Primary) && shift)
            {
                self.camera.pan(d.x, d.y, rect.height());
            }
            if resp.hovered() {
                let scroll = ctx.input(|i| i.smooth_scroll_delta.y + (i.zoom_delta() - 1.0) * 400.0);
                if scroll != 0.0 {
                    self.camera.zoom(scroll);
                }
            }
            // While measuring, clicks place points.
            if resp.double_clicked() && !self.tool.measuring() {
                self.frame_model();
            }
        }

        // Highlight follows the mesh hovered in the info panel.
        if self.hovered_mesh != self.applied_highlight {
            if let Some(r) = self.renderer.as_mut() {
                r.set_highlight(self.hovered_mesh);
            }
            self.applied_highlight = self.hovered_mesh;
        }

        if let Some(p) = self.player.as_mut() {
            if p.tick(dt) {
                ctx.request_repaint();
            }
            if let Some(r) = self.renderer.as_mut() {
                p.apply(r);
            }
        }

        let ppp = ctx.pixels_per_point();
        let size = [(rect.width() * ppp).round().max(1.0) as u32, (rect.height() * ppp).round().max(1.0) as u32];
        let params = self.frame_params(rect.width() / rect.height().max(1.0));
        let t = Instant::now();
        if let (Some(r), Some(rs)) = (self.renderer.as_mut(), frame.wgpu_render_state()) {
            let mut er = rs.renderer.write();
            let tex = r.render(&mut er, size, &params);
            drop(er);
            ui.painter().image(tex, rect, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), egui::Color32::WHITE);
        }
        self.last_frame_ms = t.elapsed().as_secs_f32() * 1000.0;

        if self.tool.measuring() {
            self.measure_input(&ctx, &resp, rect, size, &params);
        }
        self.draw_measures(ui, rect);
        self.draw_pivot(ui, rect);
        self.draw_gizmo(ui, rect);
        self.draw_view_messages(ui, rect);
        self.tool_panels(&ctx, rect);
    }

    /// A ring at the model's pivot (the file origin), visible through the model.
    fn draw_pivot(&self, ui: &egui::Ui, rect: egui::Rect) {
        let Some(s) = &self.summary else { return };
        if !self.settings.show_axes || !s.bounds.pivot_near() {
            return;
        }
        let clip = self.camera.view_proj(rect.width() / rect.height().max(1.0)) * glam::Vec4::W;
        if clip.w <= 1e-6 {
            return;
        }
        let ndc = clip.truncate() / clip.w;
        if ndc.x.abs() > 1.0 || ndc.y.abs() > 1.0 || !(0.0..=1.0).contains(&ndc.z) {
            return;
        }
        let p = egui::pos2(rect.left() + (ndc.x * 0.5 + 0.5) * rect.width(), rect.top() + (0.5 - ndc.y * 0.5) * rect.height());
        let painter = ui.painter();
        let gold = egui::Color32::from_rgb(255, 205, 60);
        painter.circle_stroke(p, 6.5, egui::Stroke::new(3.5, egui::Color32::from_black_alpha(150)));
        painter.circle_stroke(p, 6.5, egui::Stroke::new(1.6, gold));
        painter.circle_filled(p, 1.8, gold);
    }

    fn draw_view_messages(&mut self, ui: &mut egui::Ui, rect: egui::Rect) {
        let p = ui.painter().clone();
        let hovering_files = ui.ctx().input(|i| !i.raw.hovered_files.is_empty());
        if hovering_files {
            p.rect_filled(rect.shrink(12.0), 12.0, egui::Color32::from_rgba_unmultiplied(80, 140, 255, 40));
            p.text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                tr("Отпустите файл, чтобы открыть", "Drop the file to open it"),
                egui::FontId::proportional(22.0),
                egui::Color32::WHITE,
            );
        } else if self.summary.is_none() && self.loading.is_none() {
            if let Some(logo) = &self.logo {
                let r = egui::Rect::from_center_size(rect.center() - egui::vec2(0.0, 92.0), egui::vec2(112.0, 112.0));
                p.image(logo.id(), r, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), egui::Color32::WHITE);
            }
            p.text(
                rect.center() - egui::vec2(0.0, 14.0),
                egui::Align2::CENTER_CENTER,
                tr("Перетащите модель сюда или нажмите Ctrl+O", "Drop a model here or press Ctrl+O"),
                egui::FontId::proportional(20.0),
                if self.settings.bg_luma() > 0.55 { egui::Color32::from_gray(40) } else { egui::Color32::from_gray(200) },
            );
            p.text(
                rect.center() + egui::vec2(0.0, 16.0),
                egui::Align2::CENTER_CENTER,
                "glTF · GLB · FBX · OBJ · DAE · 3DS · BLEND · STL · PLY · 3MF · OFF",
                egui::FontId::proportional(14.0),
                if self.settings.bg_luma() > 0.55 { egui::Color32::from_gray(90) } else { egui::Color32::from_gray(130) },
            );
        }
        if let Some((path, secs)) = self.is_loading() {
            let name = path.file_name().unwrap_or_default().to_string_lossy().into_owned();
            let r = egui::Rect::from_center_size(rect.center(), egui::vec2(360.0, 64.0));
            p.rect_filled(r, 10.0, egui::Color32::from_black_alpha(170));
            let mut child = ui.new_child(egui::UiBuilder::new().max_rect(r.shrink(14.0)));
            child.horizontal_centered(|ui| {
                ui.add(egui::Spinner::new().size(26.0));
                ui.vertical(|ui| {
                    ui.label(egui::RichText::new(format!("{} {name}", tr("Загрузка", "Loading"))).strong());
                    ui.label(format!("{secs:.1} {}", tr("с", "s")));
                });
            });
        }
        if let Some(err) = self.error.clone() {
            let r = egui::Rect::from_min_size(rect.left_top() + egui::vec2(12.0, 12.0), egui::vec2((rect.width() - 24.0).min(640.0), 58.0));
            p.rect_filled(r, 8.0, egui::Color32::from_rgba_unmultiplied(120, 30, 30, 230));
            let mut child = ui.new_child(egui::UiBuilder::new().max_rect(r.shrink(10.0)));
            child.horizontal(|ui| {
                ui.label(egui::RichText::new(format!("{} {err}", tr("Не удалось открыть:", "Cannot open:"))).color(egui::Color32::WHITE));
                if ui.button("×").on_hover_text(tr("закрыть (Esc)", "close (Esc)")).clicked() {
                    self.error = None;
                }
            });
        }
    }
}

/// Square grid around `center` on the ground plane with "nice" spacing. When the grid
/// lies in the ground plane through the origin, the two lines through the origin are
/// drawn in the axis colours (X red, Y green, Z blue) so the pivot is easy to find.
fn grid(v: &mut Vec<LineVertex>, center: Vec3, extent: f32, z_up: bool, dark_lines: bool) {
    let target = (extent * 1.6).max(1e-6);
    let raw = target / 16.0;
    let mag = 10f32.powf(raw.log10().floor());
    let step = [1.0, 2.0, 5.0, 10.0].iter().map(|m| m * mag).find(|s| *s >= raw).unwrap_or(10.0 * mag);
    let n = (target / step).ceil() as i32;
    let (ax, ay) = if z_up { (Vec3::X, Vec3::Y) } else { (Vec3::X, Vec3::Z) };
    let cx = (center.dot(ax) / step).round() * step;
    let cy = (center.dot(ay) / step).round() * step;
    let up = if z_up { Vec3::Z } else { Vec3::Y };
    let h = center.dot(up);
    let base = ax * cx + ay * cy + up * h;
    let half = n as f32 * step;
    let through_origin = h == 0.0;
    let axis_col = |dir: Vec3| -> [f32; 4] {
        if dir == Vec3::X {
            [0.95, 0.3, 0.3, 0.7]
        } else if dir == Vec3::Y {
            [0.4, 0.9, 0.35, 0.7]
        } else {
            [0.35, 0.55, 1.0, 0.7]
        }
    };
    for i in -n..=n {
        let major = i % 5 == 0;
        let a = if major { 0.28 } else { 0.12 };
        let col = if dark_lines { [0.1, 0.11, 0.13, a * 1.6] } else { [0.75, 0.78, 0.85, a] };
        let o = i as f32 * step;
        for (dir, perp) in [(ax, ay), (ay, ax)] {
            // This line runs along `dir` at `perp` coordinate (centre + o).
            let on_axis = through_origin && (base.dot(perp) + o).abs() < step * 1e-3;
            let col = if on_axis { axis_col(dir) } else { col };
            v.push(LineVertex { pos: (base + perp * o - dir * half).to_array(), color: col });
            v.push(LineVertex { pos: (base + perp * o + dir * half).to_array(), color: col });
        }
    }
}

impl eframe::App for ViewerApp {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if let Some(p) = self.pending_open.take() {
            self.open(ctx, p);
        }
        self.poll_loading(ctx);
        self.poll_update();
        self.sync_environment();
        self.camera.fov_y = self.settings.fov_deg.to_radians();
    }

    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.handle_drops(&ctx);
        self.handle_keys(&ctx);
        self.ui_panels(ui);
        egui::CentralPanel::no_frame().show(ui, |ui| self.view_3d(ui, frame));
        self.ui_windows(ui.ctx());
        if self.startup_ms.is_none() {
            self.startup_ms = Some(process_start().elapsed().as_secs_f64() * 1000.0);
        }
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        eframe::set_value(storage, eframe::APP_KEY, &self.settings);
    }

    fn on_exit(&mut self) {
        // Runs after `save`, so the new instance reads up-to-date settings.
        if self.restart_after_exit
            && let crate::update::State::Installed(_, exe) = &self.updater.state
        {
            let mut cmd = std::process::Command::new(exe);
            if let Some(p) = self.current_path() {
                cmd.arg(p);
            }
            let _ = cmd.spawn();
        }
    }
}

/// Walking speed when a model opens: a quarter of the scene radius per second, capped
/// at a brisk 5 m/s when the unit size is known (big locations), at least 5 cm/s.
fn default_fly_speed(radius: f32, unit_m: Option<f64>) -> f32 {
    let speed = radius * 0.25;
    match unit_m {
        Some(u) if u > 0.0 => {
            let metres = (speed as f64 * u).clamp(0.05, 5.0);
            (metres / u) as f32
        }
        _ => speed,
    }
}

/// Formats whose vertex colours are usually masks rather than colours.
pub fn is_dcc_format(format: &str) -> bool {
    ["FBX", "Wavefront", "COLLADA", "3DS", "Blender"].iter().any(|p| format.starts_with(p))
}

// ---- Viewport tools, environment and the view gizmo ----

impl ViewerApp {
    pub fn set_tool(&mut self, t: Tool) {
        if self.tool == t {
            return;
        }
        self.pending.clear();
        self.hover_pick = None;
        self.tool = t;
        self.camera.fly = t == Tool::Fly;
        if t == Tool::Fly && self.camera.ortho {
            // Walking needs perspective.
            self.camera.ortho = false;
            self.settings.ortho = false;
        }
    }

    pub fn toggle_ortho(&mut self) {
        self.settings.ortho = !self.settings.ortho;
        self.camera.ortho = self.settings.ortho;
        if self.camera.ortho && self.tool == Tool::Fly {
            self.set_tool(Tool::Orbit);
        }
    }

    /// Metres per model unit: from the file, else the user's assumption.
    pub fn unit_m(&self) -> Option<f64> {
        self.summary.as_ref().and_then(|s| s.info.unit_m).or(self.settings.assumed_unit_m)
    }

    /// True when the file itself says how big one unit is.
    pub fn file_unit_known(&self) -> bool {
        self.summary.as_ref().is_some_and(|s| s.info.unit_m.is_some())
    }

    /// Uploads the selected environment map when the selection changed.
    fn sync_environment(&mut self) {
        use crate::render::env::{EnvMap, EnvPreset};
        let s = &self.settings;
        let want = (s.env, if s.env == EnvPreset::Custom { s.env_hdr.clone() } else { None });
        if self.env_uploaded.as_ref() == Some(&want) {
            return;
        }
        let Some(r) = self.renderer.as_mut() else { return };
        let map = match want.0 {
            EnvPreset::Basic => None,
            EnvPreset::Custom => match want.1.as_deref() {
                Some(path) => match EnvMap::load_hdr(path) {
                    Ok(m) => {
                        self.env_error = None;
                        Some(m)
                    }
                    Err(e) => {
                        self.env_error = Some(format!("{e:#}"));
                        None
                    }
                },
                None => None,
            },
            p => EnvMap::preset(p),
        };
        r.set_environment(map.as_ref());
        self.env_uploaded = Some(want);
    }

    /// Small pictures of the built-in environments for the background window.
    pub fn env_previews(&mut self, ctx: &egui::Context) -> Vec<(crate::render::env::EnvPreset, egui::TextureId)> {
        use crate::render::env::{EnvMap, EnvPreset};
        if self.env_previews.is_empty() {
            for p in EnvPreset::BUILT_IN {
                if let Some(m) = EnvMap::preset(p) {
                    let (w, h) = (96, 48);
                    let img = egui::ColorImage::from_rgba_unmultiplied([w, h], &m.preview(w, h));
                    self.env_previews.push((p, ctx.load_texture(format!("env {p:?}"), img, egui::TextureOptions::LINEAR)));
                }
            }
        }
        self.env_previews.iter().map(|(p, t)| (*p, t.id())).collect()
    }

    /// WASD / arrows move, Q/E go down/up, Shift runs.
    fn fly_keys(&mut self, ctx: &egui::Context, dt: f32) {
        if ctx.egui_wants_keyboard_input() {
            return;
        }
        use egui::Key;
        let (fwd, right, up, fast) = ctx.input(|i| {
            let axis = |pos: &[Key], neg: &[Key]| {
                f32::from(u8::from(pos.iter().any(|k| i.key_down(*k)))) - f32::from(u8::from(neg.iter().any(|k| i.key_down(*k))))
            };
            (
                axis(&[Key::W, Key::ArrowUp], &[Key::S, Key::ArrowDown]),
                axis(&[Key::D, Key::ArrowRight], &[Key::A, Key::ArrowLeft]),
                axis(&[Key::E], &[Key::Q]),
                i.modifiers.shift,
            )
        });
        let len = (fwd * fwd + right * right + up * up).sqrt();
        if len > 0.0 {
            let v = self.fly_speed * if fast { 4.0 } else { 1.0 } * dt / len;
            self.camera.fly_move(right * v, up * v, fwd * v);
            ctx.request_repaint();
        }
    }

    /// Picks the surface under the cursor; left click places a point, right click
    /// removes the last one.
    fn measure_input(&mut self, ctx: &egui::Context, resp: &egui::Response, rect: egui::Rect, size: [u32; 2], params: &FrameParams) {
        let ppp = ctx.pixels_per_point();
        self.hover_pick = match (resp.hover_pos(), self.renderer.as_mut()) {
            (Some(p), Some(r)) => r.pick(params, size, [(p.x - rect.left()) * ppp, (p.y - rect.top()) * ppp]),
            _ => None,
        };
        if resp.hovered() {
            ctx.set_cursor_icon(egui::CursorIcon::Crosshair);
        }
        if resp.clicked_by(egui::PointerButton::Primary)
            && let Some(p) = self.hover_pick
        {
            self.pending.push(p);
            if self.pending.len() >= self.tool.points_needed() {
                self.measures.push(Measure { points: std::mem::take(&mut self.pending) });
            }
        }
        if resp.clicked_by(egui::PointerButton::Secondary) {
            self.pending.pop();
        }
    }

    /// Measurement lines, points and labels drawn over the 3D view.
    fn draw_measures(&self, ui: &egui::Ui, rect: egui::Rect) {
        let measuring = self.tool.measuring();
        if self.measures.is_empty() && self.pending.is_empty() && !(measuring && self.hover_pick.is_some()) {
            return;
        }
        let vp = self.camera.view_proj(rect.width() / rect.height().max(1.0));
        let project = |p: Vec3| -> Option<egui::Pos2> {
            let c = vp * p.extend(1.0);
            if c.w <= 1e-6 {
                return None;
            }
            let n = c.truncate() / c.w;
            Some(egui::pos2(rect.left() + (n.x * 0.5 + 0.5) * rect.width(), rect.top() + (0.5 - n.y * 0.5) * rect.height()))
        };
        let painter = ui.painter_at(rect);
        let shadow = egui::Color32::from_black_alpha(150);
        let line = |a: egui::Pos2, b: egui::Pos2, col: egui::Color32| {
            painter.line_segment([a, b], egui::Stroke::new(4.0, shadow));
            painter.line_segment([a, b], egui::Stroke::new(2.0, col));
        };
        let dot = |p: egui::Pos2, col: egui::Color32| {
            painter.circle_filled(p, 5.0, shadow);
            painter.circle_filled(p, 3.2, col);
        };
        let label = |at: egui::Pos2, text: String| {
            let g = painter.layout_no_wrap(text, egui::FontId::proportional(13.0), egui::Color32::WHITE);
            let r = egui::Rect::from_center_size(at, g.size() + egui::vec2(10.0, 6.0));
            painter.rect_filled(r, 4.0, egui::Color32::from_black_alpha(200));
            painter.galley(r.min + egui::vec2(5.0, 3.0), g, egui::Color32::WHITE);
        };
        let unit = self.unit_m();
        let ruler = egui::Color32::from_rgb(255, 210, 80);
        let protractor = egui::Color32::from_rgb(110, 220, 255);
        let draw_angle = |a: Vec3, b: Vec3, c: Vec3, col: egui::Color32, with_label: bool| {
            let (Some(pa), Some(pb), Some(pc)) = (project(a), project(b), project(c)) else { return };
            line(pb, pa, col);
            line(pb, pc, col);
            let (u, v) = ((a - b).normalize_or_zero(), (c - b).normalize_or_zero());
            let deg = crate::tools::angle_deg(a, b, c);
            let r = 0.3 * (a - b).length().min((c - b).length());
            if deg > 0.5 && deg < 179.5 {
                let arc: Vec<egui::Pos2> =
                    (0..=24).filter_map(|i| project(b + (u.lerp(v, i as f32 / 24.0)).normalize_or_zero() * r)).collect();
                for w in arc.windows(2) {
                    painter.line_segment([w[0], w[1]], egui::Stroke::new(2.0, col));
                }
            }
            for p in [pa, pb, pc] {
                dot(p, col);
            }
            if with_label {
                let bis = (u + v).normalize_or(u);
                let at = project(b + bis * r * 1.7).unwrap_or(pb + egui::vec2(0.0, -18.0));
                label(at, format!("{deg:.1}°"));
            }
        };
        for m in &self.measures {
            match m.points.as_slice() {
                [a, b] => {
                    if let (Some(pa), Some(pb)) = (project(*a), project(*b)) {
                        line(pa, pb, ruler);
                        dot(pa, ruler);
                        dot(pb, ruler);
                        label(pa + (pb - pa) * 0.5, crate::tools::format_length(m.length(), unit));
                    }
                }
                [a, b, c] => draw_angle(*a, *b, *c, protractor, true),
                _ => {}
            }
        }
        // The measurement being placed, with a live preview to the cursor.
        let col = if self.tool == Tool::Angle { protractor } else { ruler };
        let hover = if measuring { self.hover_pick } else { None };
        match (self.pending.as_slice(), hover) {
            ([a], Some(h)) if self.tool == Tool::Distance => {
                if let (Some(pa), Some(ph)) = (project(*a), project(h)) {
                    line(pa, ph, col.gamma_multiply(0.8));
                    dot(pa, col);
                    label(pa + (ph - pa) * 0.5, crate::tools::format_length(a.distance(h), unit));
                }
            }
            ([a], Some(h)) => {
                if let (Some(pa), Some(ph)) = (project(*a), project(h)) {
                    line(pa, ph, col.gamma_multiply(0.8));
                    dot(pa, col);
                }
            }
            ([a, b], Some(h)) => draw_angle(*a, *b, h, col.gamma_multiply(0.85), true),
            (pts, _) => {
                for p in pts.iter().filter_map(|p| project(*p)) {
                    dot(p, col);
                }
            }
        }
        if let Some(ph) = hover.and_then(project) {
            painter.circle_stroke(ph, 7.0, egui::Stroke::new(3.0, shadow));
            painter.circle_stroke(ph, 7.0, egui::Stroke::new(1.5, col));
        }
    }

    /// Clickable orientation gizmo (like Unity's): an axis handle turns the camera to look
    /// along that axis, the centre cube and the caption toggle perspective/orthographic.
    fn draw_gizmo(&mut self, ui: &mut egui::Ui, rect: egui::Rect) {
        struct Handle {
            dir: Vec3,
            depth: f32,
            pos: egui::Pos2,
            col: egui::Color32,
            name: &'static str,
        }
        let view = self.camera.view();
        let c = egui::pos2(rect.left() + 52.0, rect.bottom() - 64.0);
        let len = 30.0;
        let mut hs: Vec<Handle> = Vec::with_capacity(6);
        for (axis, col, name) in [
            (Vec3::X, egui::Color32::from_rgb(235, 80, 80), "X"),
            (Vec3::Y, egui::Color32::from_rgb(110, 220, 90), "Y"),
            (Vec3::Z, egui::Color32::from_rgb(90, 140, 255), "Z"),
        ] {
            for positive in [true, false] {
                let dir = if positive { axis } else { -axis };
                let v = view.transform_vector3(dir);
                hs.push(Handle {
                    dir,
                    depth: v.z,
                    pos: c + egui::vec2(v.x, -v.y) * len,
                    col: if positive { col } else { egui::Color32::from_gray(150) },
                    name: if positive { name } else { "" },
                });
            }
        }
        // Far handles first so near ones are drawn (and hit) on top.
        hs.sort_by(|a, b| a.depth.total_cmp(&b.depth));
        let radius = |h: &Handle| if h.name.is_empty() { 5.5 } else { 8.0 };
        // Handles that point straight at or away from the viewer sit under the cube.
        let visible = |h: &Handle| (h.pos - c).length() >= 6.0;

        let area = egui::Rect::from_center_size(c, egui::vec2(92.0, 92.0));
        let resp = ui.interact(area, ui.make_persistent_id("view_gizmo"), egui::Sense::click());
        let cube = egui::Rect::from_center_size(c, egui::vec2(13.0, 13.0));
        let caption_pos = c + egui::vec2(0.0, 50.0);
        let caption = ui.interact(
            egui::Rect::from_center_size(caption_pos, egui::vec2(60.0, 16.0)),
            ui.make_persistent_id("view_gizmo_caption"),
            egui::Sense::click(),
        );

        #[derive(Clone, Copy, PartialEq)]
        enum Hit {
            Cube,
            Axis(usize),
        }
        let hit = resp.hover_pos().and_then(|p| {
            if cube.expand(2.0).contains(p) {
                return Some(Hit::Cube);
            }
            hs.iter().enumerate().rev().find(|(_, h)| visible(h) && h.pos.distance(p) <= radius(h) + 2.0).map(|(i, _)| Hit::Axis(i))
        });

        let p = ui.painter();
        p.circle_filled(c, 42.0, egui::Color32::from_black_alpha(if resp.hovered() { 85 } else { 55 }));
        let draw_handle = |i: usize, h: &Handle| {
            if !visible(h) {
                return;
            }
            let hovered = hit == Some(Hit::Axis(i));
            let col = if hovered { h.col.lerp_to_gamma(egui::Color32::WHITE, 0.35) } else { h.col };
            if !h.name.is_empty() {
                p.line_segment([c, h.pos], egui::Stroke::new(2.5, col));
            }
            p.circle_filled(h.pos, radius(h), col);
            if hovered {
                p.circle_stroke(h.pos, radius(h) + 1.5, egui::Stroke::new(1.5, egui::Color32::WHITE));
            }
            if !h.name.is_empty() {
                p.text(h.pos, egui::Align2::CENTER_CENTER, h.name, egui::FontId::proportional(10.0), egui::Color32::BLACK);
            }
        };
        for (i, h) in hs.iter().enumerate().filter(|(_, h)| h.depth < 0.0) {
            draw_handle(i, h);
        }
        let cube_hot = hit == Some(Hit::Cube) || caption.hovered();
        p.rect_filled(cube, 2.0, if cube_hot { egui::Color32::WHITE } else { egui::Color32::from_gray(200) });
        p.rect_stroke(cube, 2.0, egui::Stroke::new(1.0, egui::Color32::from_black_alpha(170)), egui::StrokeKind::Outside);
        for (i, h) in hs.iter().enumerate().filter(|(_, h)| h.depth >= 0.0) {
            draw_handle(i, h);
        }
        let text = if self.camera.ortho { tr("Орто", "Ortho") } else { tr("Персп.", "Persp") };
        let text_col = if cube_hot { egui::Color32::WHITE } else { egui::Color32::from_gray(185) };
        p.text(caption_pos, egui::Align2::CENTER_CENTER, text, egui::FontId::proportional(11.0), text_col);

        if hit.is_some() || caption.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        let tip = match hit {
            Some(Hit::Cube) => Some(tr("Перспектива / ортогональная проекция", "Perspective / orthographic").to_string()),
            Some(Hit::Axis(i)) => {
                let d = hs[i].dir;
                let name = if d.x.abs() > 0.5 {
                    "X"
                } else if d.y.abs() > 0.5 {
                    "Y"
                } else {
                    "Z"
                };
                let sign = if d.x + d.y + d.z > 0.0 { "+" } else { "−" };
                Some(format!("{} {sign}{name}", tr("Вид с", "View from")))
            }
            None => None,
        };
        let clicked = resp.clicked();
        if let Some(t) = tip {
            resp.on_hover_text_at_pointer(t);
        }
        if caption.clicked() || (clicked && hit == Some(Hit::Cube)) {
            self.toggle_ortho();
        } else if clicked && let Some(Hit::Axis(i)) = hit {
            let (yaw, pitch) = self.camera.angles_for_back_dir(hs[i].dir);
            self.camera.animate_to(yaw, pitch);
            ui.ctx().request_repaint();
        }
    }
}
