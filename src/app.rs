//! Application state: loading, camera input, overlays and the 3D view.

use crate::camera::{OrbitCamera, ViewPreset};
use crate::i18n::tr;
use crate::loaders;
use crate::render::{FrameParams, LineVertex, Renderer};
use crate::scene::{Aabb, Scene, Topology};
use crate::settings::{DisplayMode, Settings};
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
    pub assoc_status: Option<String>,
    applied_highlight: Option<usize>,
    pub last_frame_ms: f32,
    pub startup_ms: Option<f64>,
    pending_open: Option<PathBuf>,
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
        let camera = OrbitCamera { ortho: settings.ortho, fov_y: settings.fov_deg.to_radians(), ..Default::default() };
        Self {
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
            assoc_status: None,
            applied_highlight: None,
            last_frame_ms: 0.0,
            startup_ms: None,
            pending_open: initial,
        }
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
            self.camera.frame(&summary.bounds);
        }
        self.camera.scene_radius = summary.bounds.radius();
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
            self.camera.frame(&s.bounds);
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
                    s.ortho = !s.ortho;
                    self.camera.ortho = s.ortho;
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
                    self.show_help = false;
                    self.error = None;
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

    /// Grid, axes and bounding box line list.
    fn build_overlay(&self) -> Vec<LineVertex> {
        let mut v = Vec::new();
        let Some(s) = &self.summary else {
            if self.settings.show_grid {
                grid(&mut v, Vec3::ZERO, 1.0, self.camera.z_up, self.settings.bg_luma() > 0.55);
            }
            return v;
        };
        let b = s.bounds;
        if !b.is_valid() {
            return v;
        }
        let z_up = self.camera.z_up;
        let up = if z_up { Vec3::Z } else { Vec3::Y };
        let floor = b.min.dot(up);
        let c = b.center();
        let center = c - up * (c.dot(up) - floor);
        let size = b.size();
        let horiz = if z_up { size.x.max(size.y) } else { size.x.max(size.z) };
        if self.settings.show_grid {
            grid(&mut v, center, horiz.max(b.radius() * 0.5), z_up, self.settings.bg_luma() > 0.55);
        }
        if self.settings.show_axes {
            let len = b.radius() * 0.6;
            let o = Vec3::ZERO;
            for (axis, col) in [(Vec3::X, [0.95, 0.3, 0.3, 0.9]), (Vec3::Y, [0.4, 0.9, 0.35, 0.9]), (Vec3::Z, [0.35, 0.55, 1.0, 0.9])] {
                v.push(LineVertex { pos: o.to_array(), color: col });
                v.push(LineVertex { pos: (o + axis * len).to_array(), color: col });
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
        v
    }

    fn frame_params(&self, aspect: f32) -> FrameParams {
        let cam = &self.camera;
        let (right, up, fwd) = cam.basis();
        let light = (-fwd * 1.0 + up * 0.55 - right * 0.45).normalize();
        let s = &self.settings;
        let (top, bottom) = if s.bg_gradient { (s.bg_top, s.bg_bottom) } else { (s.bg_top, s.bg_top) };
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
            cull: self.settings.backface_culling,
            wire_overlay: self.settings.wire_overlay,
            point_size: self.settings.point_size,
            bg_top: top,
            bg_bottom: bottom,
            overlay: self.build_overlay(),
        }
    }

    /// The 3D viewport: input handling, rendering and on-screen overlays.
    pub fn view_3d(&mut self, ui: &mut egui::Ui, frame: &eframe::Frame) {
        let rect = ui.available_rect_before_wrap();
        let resp = ui.allocate_rect(rect, egui::Sense::click_and_drag());
        let ctx = ui.ctx().clone();

        // Camera input.
        let shift = ctx.input(|i| i.modifiers.shift);
        let d = resp.drag_delta();
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
        if resp.double_clicked() {
            self.frame_model();
        }

        // Highlight follows the mesh hovered in the info panel.
        if self.hovered_mesh != self.applied_highlight {
            if let Some(r) = self.renderer.as_mut() {
                r.set_highlight(self.hovered_mesh);
            }
            self.applied_highlight = self.hovered_mesh;
        }

        if let Some(p) = self.player.as_mut() {
            let dt = ctx.input(|i| i.stable_dt).min(0.1);
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

        self.draw_gizmo(ui, rect);
        self.draw_view_messages(ui, rect);
    }

    fn draw_gizmo(&self, ui: &egui::Ui, rect: egui::Rect) {
        let view = self.camera.view();
        let c = egui::pos2(rect.left() + 46.0, rect.bottom() - 46.0);
        let len = 30.0;
        let mut axes: Vec<(f32, egui::Vec2, egui::Color32, &str)> = [
            (Vec3::X, egui::Color32::from_rgb(235, 80, 80), "X"),
            (Vec3::Y, egui::Color32::from_rgb(110, 220, 90), "Y"),
            (Vec3::Z, egui::Color32::from_rgb(90, 140, 255), "Z"),
        ]
        .into_iter()
        .map(|(a, col, n)| {
            let v = view.transform_vector3(a);
            (v.z, egui::vec2(v.x, -v.y) * len, col, n)
        })
        .collect();
        axes.sort_by(|a, b| a.0.total_cmp(&b.0));
        let p = ui.painter();
        p.circle_filled(c, 40.0, egui::Color32::from_black_alpha(60));
        for (_, d, col, n) in axes {
            p.line_segment([c, c + d], egui::Stroke::new(2.5, col));
            p.circle_filled(c + d, 7.5, col);
            p.text(c + d, egui::Align2::CENTER_CENTER, n, egui::FontId::proportional(10.0), egui::Color32::BLACK);
        }
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
            p.text(
                rect.center() - egui::vec2(0.0, 14.0),
                egui::Align2::CENTER_CENTER,
                tr("Перетащите модель сюда или нажмите Ctrl+O", "Drop a model here or press Ctrl+O"),
                egui::FontId::proportional(20.0),
                egui::Color32::from_gray(200),
            );
            p.text(
                rect.center() + egui::vec2(0.0, 16.0),
                egui::Align2::CENTER_CENTER,
                "glTF · GLB · FBX · OBJ · DAE · 3DS · BLEND · STL · PLY · 3MF · OFF",
                egui::FontId::proportional(14.0),
                egui::Color32::from_gray(130),
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

/// Square grid around `center` on the ground plane with "nice" spacing.
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
    for i in -n..=n {
        let major = i % 5 == 0;
        let a = if major { 0.28 } else { 0.12 };
        let col = if dark_lines { [0.1, 0.11, 0.13, a * 1.6] } else { [0.75, 0.78, 0.85, a] };
        let o = i as f32 * step;
        for (dir, perp) in [(ax, ay), (ay, ax)] {
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
}

/// Formats whose vertex colours are usually masks rather than colours.
pub fn is_dcc_format(format: &str) -> bool {
    ["FBX", "Wavefront", "COLLADA", "3DS", "Blender"].iter().any(|p| format.starts_with(p))
}
