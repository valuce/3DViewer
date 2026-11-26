//! Menus, icon toolbar, info panel, animation timeline, status bar and windows.

use crate::app::ViewerApp;
use crate::camera::ViewPreset;
use crate::i18n::{Lang, tr};
use crate::scene::Topology;
use crate::settings::{BG_PRESETS, DisplayMode};
use crate::tools::Tool;
use egui_phosphor::regular as icon;

/// "1234567" -> "1 234 567".
pub fn group(n: usize) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push('\u{202F}');
        }
        out.push(c);
    }
    out
}

pub fn bytes(n: u64) -> String {
    let f = n as f64;
    let (k, m, g) = (1024.0, 1024.0 * 1024.0, 1024.0 * 1024.0 * 1024.0);
    if f >= g {
        format!("{:.2} {}", f / g, tr("ГБ", "GB"))
    } else if f >= m {
        format!("{:.1} {}", f / m, tr("МБ", "MB"))
    } else if f >= k {
        format!("{:.1} {}", f / k, tr("КБ", "KB"))
    } else {
        format!("{n} {}", tr("Б", "B"))
    }
}

fn fnum(x: f32) -> String {
    let a = x.abs();
    if a >= 1000.0 {
        format!("{x:.0}")
    } else if a >= 10.0 {
        format!("{x:.2}")
    } else {
        format!("{x:.4}")
    }
}

fn views() -> [(&'static str, ViewPreset, &'static str); 7] {
    [
        (tr("Спереди", "Front"), ViewPreset::Front, "Ctrl+1"),
        (tr("Сзади", "Back"), ViewPreset::Back, ""),
        (tr("Справа", "Right"), ViewPreset::Right, "Ctrl+3"),
        (tr("Слева", "Left"), ViewPreset::Left, ""),
        (tr("Сверху", "Top"), ViewPreset::Top, "Ctrl+7"),
        (tr("Снизу", "Bottom"), ViewPreset::Bottom, "Ctrl+9"),
        (tr("Изометрия", "Isometric"), ViewPreset::Iso, "R"),
    ]
}

fn help_rows() -> Vec<(&'static str, &'static str)> {
    vec![
        (tr("ЛКМ", "LMB"), tr("вращать камеру", "orbit")),
        (tr("ПКМ / СКМ / Shift+ЛКМ", "RMB / MMB / Shift+LMB"), tr("сдвигать камеру", "pan")),
        (tr("Колесо", "Wheel"), tr("приближать", "zoom")),
        (tr("Двойной клик, F", "Double click, F"), tr("показать модель целиком", "frame the model")),
        ("R", tr("сбросить вид", "reset view")),
        ("1 … 8", tr("режимы отображения", "display modes")),
        ("W", tr("рёбра поверх модели", "edges over the model")),
        ("T", tr("текстуры", "textures")),
        ("L", tr("плоское затенение", "flat shading")),
        ("V", tr("цвета вершин в материале", "vertex colours in materials")),
        ("C", tr("отсечение задних граней", "back-face culling")),
        ("G / X / B", tr("сетка / оси / габариты", "grid / axes / bounds")),
        ("P", tr("перспектива / ортогональная проекция", "perspective / orthographic")),
        ("U", tr("ось вверх Y / Z", "up axis Y / Z")),
        ("Ctrl+1 / 3 / 7 / 9", tr("вид спереди / справа / сверху / снизу", "front / right / top / bottom view")),
        (tr("Пробел", "Space"), tr("пуск / пауза анимации", "play / pause animation")),
        ("PgUp / PgDn, ← / →", tr("предыдущий / следующий файл в папке", "previous / next file in the folder")),
        ("Ctrl+O", tr("открыть файл", "open a file")),
        ("F5", tr("перезагрузить файл", "reload the file")),
        ("I", tr("панель информации", "info panel")),
        ("F11", tr("полный экран", "full screen")),
        (
            tr("Клик по гизмо", "Gizmo click"),
            tr("вид вдоль оси; кубик — перспектива / орто", "view along an axis; cube — perspective / ortho"),
        ),
        (
            tr("Прогулка", "Walk"),
            tr(
                "WASD / стрелки, Q/E — вниз/вверх, Shift — бегом, колесо — скорость",
                "WASD / arrows, Q/E — down/up, Shift — run, wheel — speed",
            ),
        ),
        (tr("Линейка, угол", "Ruler, angle"), tr("клик — точка, ПКМ — убрать последнюю", "click — point, RMB — remove the last one")),
        ("Esc", tr("выйти из инструмента, затем выключить срез", "leave the tool, then turn the section off")),
        ("F1", tr("эта справка", "this help")),
    ]
}

/// Icon toggle with a tooltip: "title — hint (key)".
fn tool_toggle(ui: &mut egui::Ui, on: &mut bool, glyph: &str, title: &str, hint: &str, key: &str) -> bool {
    let r = ui.selectable_label(*on, egui::RichText::new(glyph).size(18.0)).on_hover_ui(|ui| tip(ui, title, hint, key));
    if r.clicked() {
        *on = !*on;
        return true;
    }
    false
}

fn tool_button(ui: &mut egui::Ui, glyph: &str, title: &str, hint: &str, key: &str) -> bool {
    ui.add(egui::Button::new(egui::RichText::new(glyph).size(18.0)).frame(false)).on_hover_ui(|ui| tip(ui, title, hint, key)).clicked()
}

fn tip(ui: &mut egui::Ui, title: &str, hint: &str, key: &str) {
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(title).strong());
        if !key.is_empty() {
            ui.label(egui::RichText::new(key).weak().monospace());
        }
    });
    if !hint.is_empty() {
        ui.label(hint);
    }
}

impl ViewerApp {
    pub fn ui_panels(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        egui::Panel::top("menu").show(ui, |ui| {
            egui::MenuBar::new().ui(ui, |ui| {
                self.menu_file(ui, &ctx);
                self.menu_view(ui);
                ui.menu_button(tr("Справка", "Help"), |ui| {
                    if ui.button(format!("{}  {}   F1", icon::KEYBOARD, tr("Горячие клавиши", "Keyboard shortcuts"))).clicked()
                    {
                        self.show_help = true;
                        ui.close();
                    }
                    if ui.button(format!("{}  {}", icon::ARROW_CIRCLE_UP, tr("Проверить обновления…", "Check for updates…"))).clicked()
                    {
                        self.open_update_window(&ctx);
                        ui.close();
                    }
                    if ui.button(format!("{}  {}", icon::GITHUB_LOGO, tr("Страница проекта", "Project page"))).clicked() {
                        open_url(&format!("https://github.com/{}", crate::update::REPO));
                        ui.close();
                    }
                });
            });
        });
        egui::Panel::top("toolbar").show(ui, |ui| self.toolbar(ui, &ctx));
        self.update_banner_panel(ui, &ctx);
        egui::Panel::bottom("status").show(ui, |ui| self.status_bar(ui));
        if self.player.is_some() {
            egui::Panel::bottom("timeline").show(ui, |ui| self.timeline(ui));
        }
        if self.settings.show_info {
            egui::Panel::right("info").resizable(true).default_size(310.0).min_size(220.0).show(ui, |ui| {
                egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| self.info_panel(ui));
            });
        }
    }

    fn menu_file(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        ui.menu_button(tr("Файл", "File"), |ui| {
            if ui.button(format!("{}  {}   Ctrl+O", icon::FOLDER_OPEN, tr("Открыть…", "Open…"))).clicked() {
                ui.close();
                self.open_dialog(ctx);
            }
            ui.menu_button(format!("{}  {}", icon::CLOCK_COUNTER_CLOCKWISE, tr("Недавние", "Recent")), |ui| {
                if self.settings.recent.is_empty() {
                    ui.label(tr("пусто", "empty"));
                }
                let mut open = None;
                for p in &self.settings.recent {
                    let name = p.file_name().unwrap_or_default().to_string_lossy();
                    if ui.button(name).on_hover_text(p.display().to_string()).clicked() {
                        open = Some(p.clone());
                    }
                }
                if !self.settings.recent.is_empty() {
                    ui.separator();
                    if ui.button(tr("Очистить список", "Clear list")).clicked() {
                        self.settings.recent.clear();
                        ui.close();
                    }
                }
                if let Some(p) = open {
                    ui.close();
                    self.open(ctx, p);
                }
            });
            ui.separator();
            let has = self.summary.is_some();
            if ui
                .add_enabled(has, egui::Button::new(format!("{}  {}   F5", icon::ARROW_CLOCKWISE, tr("Перезагрузить", "Reload"))))
                .clicked()
            {
                ui.close();
                self.reload(ctx);
            }
            if ui
                .add_enabled(
                    has,
                    egui::Button::new(format!("{}  {}   PgUp", icon::CARET_LEFT, tr("Предыдущий в папке", "Previous in folder"))),
                )
                .clicked()
            {
                ui.close();
                self.browse(ctx, -1);
            }
            if ui
                .add_enabled(has, egui::Button::new(format!("{}  {}   PgDn", icon::CARET_RIGHT, tr("Следующий в папке", "Next in folder"))))
                .clicked()
            {
                ui.close();
                self.browse(ctx, 1);
            }
            if ui
                .add_enabled(has, egui::Button::new(format!("{}  {}", icon::FOLDER_OPEN, tr("Показать в проводнике", "Show in Explorer"))))
                .clicked()
            {
                ui.close();
                if let Some(p) = self.summary.as_ref().map(|s| s.path.clone()) {
                    let mut arg = std::ffi::OsString::from("/select,");
                    arg.push(p.as_os_str());
                    let _ = std::process::Command::new("explorer").arg(arg).spawn();
                }
            }
            if ui.add_enabled(has, egui::Button::new(format!("{}  {}", icon::X, tr("Закрыть модель", "Close model")))).clicked()
            {
                ui.close();
                self.close_model(ctx);
            }
            ui.separator();
            if ui.button(format!("{}  {}", icon::LINK, tr("Ассоциации файлов…", "File associations…"))).clicked() {
                self.open_assoc_window();
                ui.close();
            }
            ui.separator();
            if ui.button(tr("Выход", "Exit")).clicked() {
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        });
    }

    fn menu_view(&mut self, ui: &mut egui::Ui) {
        ui.menu_button(tr("Вид", "View"), |ui| {
            for (i, m) in DisplayMode::ALL.iter().enumerate() {
                ui.radio_value(&mut self.settings.mode, *m, format!("{}  {}   {}", m.icon(), m.label(), i + 1));
            }
            ui.separator();
            let mut vc = self.vertex_colors;
            let s = &mut self.settings;
            ui.checkbox(&mut s.wire_overlay, format!("{}   W", tr("Рёбра поверх модели", "Edges over the model")));
            ui.checkbox(&mut s.textures, format!("{}   T", tr("Текстуры", "Textures")));
            ui.checkbox(&mut s.flat_shading, format!("{}   L", tr("Плоское затенение", "Flat shading")));
            ui.checkbox(&mut s.backface_culling, format!("{}   C", tr("Отсекать задние грани", "Back-face culling")));
            if ui.checkbox(&mut vc, format!("{}   V", tr("Цвета вершин в материале", "Vertex colours in materials"))).changed()
            {
                self.set_vertex_colors(vc);
            }
            ui.separator();
            let s = &mut self.settings;
            ui.checkbox(&mut s.show_grid, format!("{}   G", tr("Сетка", "Grid")));
            ui.checkbox(&mut s.show_axes, format!("{}   X", tr("Оси", "Axes")));
            ui.checkbox(&mut s.show_bbox, format!("{}   B", tr("Габаритный бокс", "Bounding box")));
            ui.checkbox(&mut s.show_info, format!("{}   I", tr("Панель информации", "Info panel")));
            ui.separator();
            if ui.checkbox(&mut s.ortho, format!("{}   P", tr("Ортогональная проекция", "Orthographic"))).changed() {
                self.camera.ortho = self.settings.ortho;
                if self.camera.ortho && self.tool == Tool::Fly {
                    self.set_tool(Tool::Orbit);
                }
            }
            ui.checkbox(&mut self.camera.z_up, format!("{}   U", tr("Ось Z вверх", "Z axis up")));
            ui.menu_button(format!("{}  {}", icon::CAMERA, tr("Камера", "Camera")), |ui| self.camera_menu(ui));
            ui.separator();
            if ui.button(format!("{}  {}", icon::PAINT_BUCKET, tr("Фон…", "Background…"))).clicked() {
                self.show_background = true;
                ui.close();
            }
            if ui.button(format!("{}  {}", icon::SLIDERS_HORIZONTAL, tr("Настройки…", "Settings…"))).clicked() {
                self.show_view_settings = true;
                ui.close();
            }
            ui.menu_button(format!("{}  {}", icon::TRANSLATE, tr("Язык / Language", "Language / Язык")), |ui| {
                self.language_menu(ui)
            });
        });
    }

    fn language_menu(&mut self, ui: &mut egui::Ui) {
        let mut choice = self.settings.language;
        ui.radio_value(&mut choice, None, tr("Как в системе", "System default"));
        ui.radio_value(&mut choice, Some(Lang::Ru), "Русский");
        ui.radio_value(&mut choice, Some(Lang::En), "English");
        if choice != self.settings.language {
            self.settings.language = choice;
            crate::i18n::set_lang(choice.unwrap_or_else(crate::i18n::detect));
        }
    }

    fn camera_menu(&mut self, ui: &mut egui::Ui) {
        for (name, p, key) in views() {
            let label = if key.is_empty() { name.to_string() } else { format!("{name}   {key}") };
            if ui.button(label).clicked() {
                self.camera.set_preset(p);
                if p == ViewPreset::Iso {
                    self.frame_model();
                }
                ui.close();
            }
        }
        ui.separator();
        if ui.button(format!("{}   F", tr("Показать целиком", "Frame model"))).clicked() {
            self.frame_model();
            ui.close();
        }
    }

    fn toolbar(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            if tool_button(ui, icon::FOLDER_OPEN, tr("Открыть", "Open"), tr("Открыть модель", "Open a model"), "Ctrl+O") {
                self.open_dialog(ctx);
            }
            ui.separator();
            for (i, m) in DisplayMode::ALL.iter().enumerate() {
                let sel = self.settings.mode == *m;
                let key = format!("{}", i + 1);
                let r = ui.selectable_label(sel, egui::RichText::new(m.icon()).size(18.0)).on_hover_ui(|ui| tip(ui, m.label(), m.hint(), &key));
                if r.clicked() {
                    self.settings.mode = *m;
                }
            }
            ui.separator();
            let s = &mut self.settings;
            tool_toggle(ui, &mut s.wire_overlay, icon::POLYGON, tr("Рёбра", "Edges"), tr("Каркас поверх модели", "Wireframe over the model"), "W");
            tool_toggle(ui, &mut s.textures, icon::IMAGE, tr("Текстуры", "Textures"), tr("Показывать текстуры материалов", "Show material textures"), "T");
            tool_toggle(ui, &mut s.flat_shading, icon::DIAMOND, tr("Плоское затенение", "Flat shading"), tr("Грани без сглаживания", "Faceted look"), "L");
            let mut vc = self.vertex_colors;
            let vc_hint = tr(
                "Умножать материал на цвета вершин. В FBX/OBJ/DAE/3DS/BLEND они часто служебные маски, поэтому там выключено по умолчанию; выбор запоминается",
                "Multiply materials by vertex colours. In FBX/OBJ/DAE/3DS/BLEND they are often masks, so this is off there by default; the choice is remembered",
            );
            if tool_toggle(ui, &mut vc, icon::DROP_HALF, tr("Цвета вершин", "Vertex colours"), vc_hint, "V") {
                self.set_vertex_colors(vc);
            }
            let s = &mut self.settings;
            tool_toggle(ui, &mut s.backface_culling, icon::EYE_SLASH, tr("Отсечение", "Culling"), tr("Скрывать задние грани", "Hide back faces"), "C");
            ui.separator();
            tool_toggle(ui, &mut s.show_grid, icon::GRID_FOUR, tr("Сетка", "Grid"), tr("Сетка пола под моделью", "Floor grid under the model"), "G");
            tool_toggle(ui, &mut s.show_axes, icon::ARROWS_OUT_CARDINAL, tr("Оси", "Axes"), tr("Оси X/Y/Z в начале координат", "X/Y/Z axes at the origin"), "X");
            tool_toggle(ui, &mut s.show_bbox, icon::BOUNDING_BOX, tr("Габариты", "Bounds"), tr("Габаритный бокс модели", "Model bounding box"), "B");
            ui.separator();
            if tool_toggle(ui, &mut s.ortho, icon::PERSPECTIVE, tr("Ортогональная проекция", "Orthographic"), tr("Без перспективы", "No perspective"), "P") {
                self.camera.ortho = self.settings.ortho;
                if self.camera.ortho && self.tool == Tool::Fly {
                    self.set_tool(Tool::Orbit);
                }
            }
            ui.menu_button(egui::RichText::new(icon::CAMERA).size(18.0), |ui| self.camera_menu(ui))
                .response
                .on_hover_ui(|ui| tip(ui, tr("Камера", "Camera"), tr("Стандартные виды", "Standard views"), ""));
            if tool_button(ui, icon::CORNERS_OUT, tr("Показать целиком", "Frame model"), "", "F") {
                self.frame_model();
            }
            ui.separator();
            for (t, glyph, title, hint) in [
                (Tool::Orbit, icon::CURSOR, tr("Вращение", "Orbit"), tr("Обычный режим: вращать, сдвигать, приближать", "Default: orbit, pan, zoom")),
                (Tool::Fly, icon::PERSON_SIMPLE_WALK, tr("Прогулка", "Walk"), tr("Ходить по сцене: WASD и мышь", "Walk through the scene with WASD and the mouse")),
                (Tool::Distance, icon::RULER, tr("Линейка", "Ruler"), tr("Расстояние между двумя точками на модели", "Distance between two points on the model")),
                (Tool::Angle, icon::ANGLE, tr("Угол", "Angle"), tr("Угол по трём точкам: начало, вершина, конец", "Angle through three points: start, vertex, end")),
            ] {
                let key = if t == Tool::Orbit { "Esc" } else { "" };
                let r = ui.selectable_label(self.tool == t, egui::RichText::new(glyph).size(18.0)).on_hover_ui(|ui| tip(ui, title, hint, key));
                if r.clicked() {
                    self.set_tool(if self.tool == t { Tool::Orbit } else { t });
                }
            }
            let mut sl = self.slice.enabled;
            if tool_toggle(ui, &mut sl, icon::KNIFE, tr("Срез", "Section"), tr("Разрезать модель плоскостью и заглянуть внутрь", "Cut the model with a plane to look inside"), "") {
                self.slice.enabled = sl;
            }
            ui.separator();
            let mut bg = self.show_background;
            if tool_toggle(ui, &mut bg, icon::PAINT_BUCKET, tr("Фон", "Background"), tr("Цвет и градиент фона", "Background colour and gradient"), "") {
                self.show_background = bg;
            }
            let mut st = self.show_view_settings;
            if tool_toggle(ui, &mut st, icon::SLIDERS_HORIZONTAL, tr("Настройки", "Settings"), tr("Свет, угол обзора, точки, язык", "Light, field of view, points, language"), "") {
                self.show_view_settings = st;
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let mut help = self.show_help;
                if tool_toggle(ui, &mut help, icon::QUESTION, tr("Справка", "Help"), tr("Горячие клавиши", "Keyboard shortcuts"), "F1") {
                    self.show_help = help;
                }
                tool_toggle(ui, &mut self.settings.show_info, icon::INFO, tr("Информация", "Info"), tr("Панель информации о модели", "Model info panel"), "I");
            });
        });
    }

    fn timeline(&mut self, ui: &mut egui::Ui) {
        let Some(p) = self.player.as_mut() else { return };
        ui.horizontal(|ui| {
            let glyph = if p.playing { icon::PAUSE } else { icon::PLAY };
            if tool_button(ui, glyph, tr("Пуск / пауза", "Play / pause"), "", tr("Пробел", "Space")) {
                p.toggle();
            }
            let name = |c: Option<usize>| match c {
                None => tr("Все клипы", "All clips").to_string(),
                Some(i) => p.data.clips.get(i).map(|c| c.name.clone()).unwrap_or_default(),
            };
            let mut sel = p.clip;
            egui::ComboBox::from_id_salt("clip").selected_text(name(sel)).width(160.0).show_ui(ui, |ui| {
                if p.data.clips.len() > 1 {
                    ui.selectable_value(&mut sel, None, name(None));
                }
                for i in 0..p.data.clips.len() {
                    ui.selectable_value(&mut sel, Some(i), name(Some(i)));
                }
            });
            if sel != p.clip {
                p.set_clip(sel);
            }
            let dur = p.duration().max(1e-3);
            let mut t = p.time;
            ui.spacing_mut().slider_width = (ui.available_width() - 240.0).max(60.0);
            if ui.add(egui::Slider::new(&mut t, 0.0..=dur).show_value(false)).changed() {
                p.seek(t);
            }
            ui.label(egui::RichText::new(format!("{:.2} / {:.2} {}", p.time, dur, tr("с", "s"))).monospace());
            let mut speed = p.speed;
            egui::ComboBox::from_id_salt("speed").selected_text(format!("{speed}×")).width(56.0).show_ui(ui, |ui| {
                for v in [0.1, 0.25, 0.5, 1.0, 1.5, 2.0, 4.0] {
                    ui.selectable_value(&mut speed, v, format!("{v}×"));
                }
            });
            p.speed = speed;
            tool_toggle(ui, &mut p.looping, icon::REPEAT, tr("Повтор", "Loop"), "", "");
        });
    }

    fn status_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if let Some(s) = &self.summary {
                ui.label(egui::RichText::new(s.path.file_name().unwrap_or_default().to_string_lossy()).strong());
                ui.separator();
                ui.label(format!("{} {}", tr("вершин", "vertices"), group(s.vertices)));
                ui.label(format!("{} {}", tr("треугольников", "triangles"), group(s.triangles)));
                ui.separator();
                ui.label(format!(
                    "{} {:.0} {} + GPU {:.0} {}",
                    tr("загрузка", "load"),
                    s.load_ms,
                    tr("мс", "ms"),
                    s.upload_ms,
                    tr("мс", "ms")
                ));
            } else {
                ui.label(tr("модель не открыта", "no model open"));
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if let Some(r) = &self.renderer {
                    ui.label(egui::RichText::new(&r.adapter_info).weak());
                    ui.separator();
                    ui.label(egui::RichText::new(format!("MSAA {}×", r.sample_count)).weak());
                }
                ui.separator();
                ui.label(egui::RichText::new(format!("{} {:.2} {}", tr("кадр", "frame"), self.last_frame_ms, tr("мс", "ms"))).weak());
            });
        });
    }

    fn info_panel(&mut self, ui: &mut egui::Ui) {
        let Some(s) = &self.summary else {
            ui.add_space(8.0);
            ui.label(tr("Откройте модель, чтобы увидеть информацию.", "Open a model to see its details."));
            return;
        };
        let mut copy = None;
        let row = |ui: &mut egui::Ui, k: &str, v: String| {
            ui.label(egui::RichText::new(k).weak());
            if v.chars().count() > 42 {
                let short: String = v.chars().take(40).collect();
                ui.label(format!("{short}…")).on_hover_text(v);
            } else {
                ui.label(v);
            }
            ui.end_row();
        };

        egui::CollapsingHeader::new(format!("{}  {}", icon::FILE_ARROW_DOWN, tr("Файл", "File")))
            .id_salt("h_file")
            .default_open(true)
            .show(ui, |ui| {
                egui::Grid::new("file").num_columns(2).spacing([10.0, 3.0]).show(ui, |ui| {
                    row(ui, tr("Имя", "Name"), s.path.file_name().unwrap_or_default().to_string_lossy().into_owned());
                    row(ui, tr("Формат", "Format"), s.format.clone());
                    row(ui, tr("Размер", "Size"), bytes(s.file_size));
                    row(ui, tr("Чтение", "Parsing"), format!("{:.1} {}", s.load_ms, tr("мс", "ms")));
                    row(ui, tr("Выгрузка в GPU", "GPU upload"), format!("{:.1} {}", s.upload_ms, tr("мс", "ms")));
                    row(ui, tr("Память GPU", "GPU memory"), bytes(s.gpu_bytes));
                    for (k, v) in &s.info.extra {
                        row(ui, k, v.clone());
                    }
                });
                ui.add(egui::Label::new(egui::RichText::new(s.path.display().to_string()).small().weak()).wrap());
            });

        egui::CollapsingHeader::new(format!("{}  {}", icon::TRIANGLE, tr("Геометрия", "Geometry")))
            .id_salt("h_geo")
            .default_open(true)
            .show(ui, |ui| {
                egui::Grid::new("geo").num_columns(2).spacing([10.0, 3.0]).show(ui, |ui| {
                    row(ui, tr("Вершины", "Vertices"), group(s.vertices));
                    row(ui, tr("Треугольники", "Triangles"), group(s.triangles));
                    if s.points > 0 {
                        row(ui, tr("Точки", "Points"), group(s.points));
                    }
                    if s.lines > 0 {
                        row(ui, tr("Линии", "Lines"), group(s.lines));
                    }
                    row(ui, tr("Меши", "Meshes"), group(s.meshes.len()));
                    row(ui, tr("Материалы", "Materials"), group(s.materials.len()));
                    row(ui, tr("Текстуры", "Textures"), group(s.textures.len()));
                    let uv = s.meshes.iter().filter(|m| m.uv).count();
                    let col = s.meshes.iter().filter(|m| m.colors).count();
                    let nrm = s.meshes.iter().filter(|m| m.had_normals).count();
                    let n = s.meshes.len();
                    let of = tr("из", "of");
                    row(ui, "UV", format!("{uv} {of} {n}"));
                    row(ui, tr("Нормали в файле", "Normals in file"), format!("{nrm} {of} {n}"));
                    row(ui, tr("Цвета вершин", "Vertex colours"), format!("{col} {of} {n}"));
                });
            });

        egui::CollapsingHeader::new(format!("{}  {}", icon::BOUNDING_BOX, tr("Габариты", "Bounds")))
            .id_salt("h_bounds")
            .default_open(true)
            .show(ui, |ui| {
                let b = s.bounds;
                let sz = b.size();
                egui::Grid::new("bounds").num_columns(2).spacing([10.0, 3.0]).show(ui, |ui| {
                    row(ui, tr("Размер X", "Size X"), fnum(sz.x));
                    row(ui, tr("Размер Y", "Size Y"), fnum(sz.y));
                    row(ui, tr("Размер Z", "Size Z"), fnum(sz.z));
                    row(ui, tr("Мин", "Min"), format!("{}, {}, {}", fnum(b.min.x), fnum(b.min.y), fnum(b.min.z)));
                    row(ui, tr("Макс", "Max"), format!("{}, {}, {}", fnum(b.max.x), fnum(b.max.y), fnum(b.max.z)));
                    let c = b.center();
                    row(ui, tr("Центр", "Centre"), format!("{}, {}, {}", fnum(c.x), fnum(c.y), fnum(c.z)));
                    ui.label(egui::RichText::new(tr("Пивот", "Pivot")).weak()).on_hover_text(pivot_hint());
                    let (text, far) = pivot_text(&b, self.camera.z_up);
                    let value = if far {
                        egui::RichText::new(text).color(egui::Color32::from_rgb(240, 170, 80))
                    } else {
                        egui::RichText::new(text)
                    };
                    ui.label(value).on_hover_text(pivot_hint());
                    ui.end_row();
                    row(ui, tr("Ось вверх", "Up axis"), if self.camera.z_up { "Z".into() } else { "Y".into() });
                });
            });

        let i = &s.info;
        if i.nodes + i.skins + i.bones + i.cameras + i.lights + i.morph_targets + i.animations.len() > 0 {
            egui::CollapsingHeader::new(format!("{}  {}", icon::TREE_STRUCTURE, tr("Сцена", "Scene")))
                .id_salt("h_scene")
                .default_open(true)
                .show(ui, |ui| {
                    egui::Grid::new("scene").num_columns(2).spacing([10.0, 3.0]).show(ui, |ui| {
                        row(ui, tr("Узлы", "Nodes"), group(i.nodes));
                        if i.skins > 0 {
                            row(ui, tr("Скины", "Skins"), group(i.skins));
                        }
                        if i.bones > 0 {
                            row(ui, tr("Кости", "Bones"), group(i.bones));
                        }
                        if i.morph_targets > 0 {
                            row(ui, tr("Морфы", "Morph targets"), group(i.morph_targets));
                        }
                        if i.cameras > 0 {
                            row(ui, tr("Камеры", "Cameras"), group(i.cameras));
                        }
                        if i.lights > 0 {
                            row(ui, tr("Источники света", "Lights"), group(i.lights));
                        }
                        row(ui, tr("Анимации", "Animations"), group(i.animations.len()));
                    });
                    for a in &i.animations {
                        ui.label(format!("{}  {a}", icon::FILM_STRIP));
                    }
                });
        }

        let mut hovered = None;
        let mut vis_changes = Vec::new();
        egui::CollapsingHeader::new(format!("{}  {} ({})", icon::STACK, tr("Меши", "Meshes"), s.meshes.len()))
            .id_salt("h_meshes")
            .default_open(s.meshes.len() <= 30)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    if ui.small_button(format!("{} {}", icon::EYE, tr("Показать все", "Show all"))).clicked() {
                        vis_changes.extend((0..s.meshes.len()).map(|i| (i, true)));
                    }
                    if ui.small_button(format!("{} {}", icon::EYE_SLASH, tr("Скрыть все", "Hide all"))).clicked() {
                        vis_changes.extend((0..s.meshes.len()).map(|i| (i, false)));
                    }
                });
                for (idx, m) in s.meshes.iter().enumerate().take(2000) {
                    let mut vis = self.mesh_visible(idx);
                    let kind = match m.topology {
                        Topology::Triangles => format!("{} {}", group(m.tris), tr("тр.", "tris")),
                        Topology::Lines => tr("линии", "lines").into(),
                        Topology::Points => format!("{} {}", group(m.verts), tr("точ.", "pts")),
                    };
                    let r = ui.checkbox(&mut vis, format!("{}  · {kind}", m.name));
                    let r = r.on_hover_ui(|ui| {
                        ui.label(format!("{}: {}", tr("вершин", "vertices"), group(m.verts)));
                        ui.label(format!("{}: {}", tr("материал", "material"), m.material.as_deref().unwrap_or("—")));
                        ui.label(format!(
                            "UV: {}  {}: {}  {}: {}",
                            yes(m.uv),
                            tr("цвета", "colours"),
                            yes(m.colors),
                            tr("нормали", "normals"),
                            if m.had_normals { tr("из файла", "from file") } else { tr("вычислены", "computed") }
                        ));
                    });
                    if r.hovered() {
                        hovered = Some(idx);
                    }
                    if r.changed() {
                        vis_changes.push((idx, vis));
                    }
                }
                if s.meshes.len() > 2000 {
                    ui.label(format!("… {} {}", tr("ещё", "and"), s.meshes.len() - 2000));
                }
            });

        if !s.materials.is_empty() {
            egui::CollapsingHeader::new(format!("{}  {} ({})", icon::PALETTE, tr("Материалы", "Materials"), s.materials.len()))
                .id_salt("h_mats")
                .default_open(s.materials.len() <= 12)
                .show(ui, |ui| {
                    for m in s.materials.iter().take(500) {
                        ui.horizontal(|ui| {
                            let c = m.base_color;
                            let to_srgb = |x: f32| {
                                (if x <= 0.0031308 { x * 12.92 } else { 1.055 * x.powf(1.0 / 2.4) - 0.055 } * 255.0).clamp(0.0, 255.0) as u8
                            };
                            let (r, _) = ui.allocate_exact_size(egui::vec2(14.0, 14.0), egui::Sense::hover());
                            ui.painter().rect_filled(r, 3.0, egui::Color32::from_rgb(to_srgb(c[0]), to_srgb(c[1]), to_srgb(c[2])));
                            ui.label(&m.name).on_hover_ui(|ui| {
                                ui.label(format!("{}: {:.2} {:.2} {:.2} α {:.2}", tr("цвет", "colour"), c[0], c[1], c[2], c[3]));
                                ui.label(format!(
                                    "{} {:.2}  {} {:.2}",
                                    tr("металл", "metallic"),
                                    m.metallic,
                                    tr("шероховатость", "roughness"),
                                    m.roughness
                                ));
                                if m.blend {
                                    ui.label(tr("прозрачный", "transparent"));
                                }
                                if m.double_sided {
                                    ui.label(tr("двусторонний", "double-sided"));
                                }
                            });
                            if !m.maps.is_empty() {
                                ui.label(egui::RichText::new(m.maps.join(", ")).small().weak());
                            }
                        });
                    }
                });
        }

        if !s.textures.is_empty() {
            egui::CollapsingHeader::new(format!("{}  {} ({})", icon::IMAGE, tr("Текстуры", "Textures"), s.textures.len()))
                .id_salt("h_tex")
                .default_open(s.textures.len() <= 12)
                .show(ui, |ui| {
                    for (name, w, h) in &s.textures {
                        ui.label(format!("{name}  ·  {w}×{h}"));
                    }
                });
        }

        if !s.info.warnings.is_empty() {
            egui::CollapsingHeader::new(
                egui::RichText::new(format!("{}  {} ({})", icon::WARNING, tr("Предупреждения", "Warnings"), s.info.warnings.len()))
                    .color(egui::Color32::from_rgb(240, 190, 80)),
            )
            .id_salt("h_warn")
            .default_open(true)
            .show(ui, |ui| {
                for w in s.info.warnings.iter().take(200) {
                    ui.label(egui::RichText::new(w).small());
                }
            });
        }

        ui.add_space(6.0);
        if ui.button(format!("{}  {}", icon::COPY, tr("Копировать сводку", "Copy summary"))).clicked() {
            copy = Some(summary_text(s));
        }

        self.hovered_mesh = hovered;
        for (i, v) in vis_changes {
            self.set_mesh_visible(i, v);
        }
        if let Some(t) = copy {
            ui.ctx().copy_text(t);
        }
    }

    pub fn ui_windows(&mut self, ctx: &egui::Context) {
        let mut open = self.show_help;
        egui::Window::new(tr("Горячие клавиши", "Keyboard shortcuts")).open(&mut open).resizable(false).collapsible(false).show(
            ctx,
            |ui| {
                egui::Grid::new("help").num_columns(2).spacing([16.0, 4.0]).show(ui, |ui| {
                    for (k, v) in help_rows() {
                        ui.label(egui::RichText::new(k).strong());
                        ui.label(v);
                        ui.end_row();
                    }
                });
                if let Some(ms) = self.startup_ms {
                    ui.separator();
                    ui.label(
                        egui::RichText::new(format!(
                            "{}: {ms:.0} {}",
                            tr("Запуск до первого кадра", "Startup to first frame"),
                            tr("мс", "ms")
                        ))
                        .weak(),
                    );
                }
                ui.label(egui::RichText::new(format!("3DViewer {}", env!("CARGO_PKG_VERSION"))).weak());
            },
        );
        self.show_help = open;

        let mut open = self.show_background;
        let previews = if open { self.env_previews(ctx) } else { Vec::new() };
        let mut pick_hdr = false;
        egui::Window::new(tr("Фон и окружение", "Background & environment")).open(&mut open).resizable(false).collapsible(false).show(
            ctx,
            |ui| {
                pick_hdr = self.environment_section(ui, &previews);
                ui.separator();
                let s = &mut self.settings;
                ui.label(egui::RichText::new(tr("Цвет фона", "Background colour")).strong());
                ui.horizontal_wrapped(|ui| {
                    for (ru, en, top, bottom, grad) in BG_PRESETS {
                        let (r, resp) = ui.allocate_exact_size(egui::vec2(44.0, 30.0), egui::Sense::click());
                        let p = ui.painter();
                        let c = |x: [u8; 3]| egui::Color32::from_rgb(x[0], x[1], x[2]);
                        let mut mesh = egui::Mesh::default();
                        let (ct, cb) = (c(*top), if *grad { c(*bottom) } else { c(*top) });
                        mesh.colored_vertex(r.left_top(), ct);
                        mesh.colored_vertex(r.right_top(), ct);
                        mesh.colored_vertex(r.right_bottom(), cb);
                        mesh.colored_vertex(r.left_bottom(), cb);
                        mesh.add_triangle(0, 1, 2);
                        mesh.add_triangle(0, 2, 3);
                        p.add(mesh);
                        let active = s.bg_top == *top && (!*grad || s.bg_bottom == *bottom) && s.bg_gradient == *grad;
                        let stroke = if active {
                            egui::Stroke::new(2.0, egui::Color32::from_rgb(90, 160, 255))
                        } else {
                            egui::Stroke::new(1.0, egui::Color32::from_gray(90))
                        };
                        p.rect_stroke(r, 3.0, stroke, egui::StrokeKind::Inside);
                        if resp.on_hover_text(tr(ru, en)).clicked() {
                            s.bg_top = *top;
                            s.bg_bottom = *bottom;
                            s.bg_gradient = *grad;
                        }
                    }
                });
                ui.separator();
                ui.horizontal(|ui| {
                    ui.radio_value(&mut s.bg_gradient, false, tr("Сплошной", "Solid"));
                    ui.radio_value(&mut s.bg_gradient, true, tr("Градиент", "Gradient"));
                });
                egui::Grid::new("bgc").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
                    ui.label(if s.bg_gradient { tr("Верх", "Top") } else { tr("Цвет", "Colour") });
                    ui.color_edit_button_srgb(&mut s.bg_top);
                    ui.end_row();
                    if s.bg_gradient {
                        ui.label(tr("Низ", "Bottom"));
                        ui.color_edit_button_srgb(&mut s.bg_bottom);
                        ui.end_row();
                        ui.label("");
                        if ui.button(format!("{} {}", icon::ARROWS_CLOCKWISE, tr("Поменять местами", "Swap"))).clicked() {
                            std::mem::swap(&mut s.bg_top, &mut s.bg_bottom);
                        }
                        ui.end_row();
                    }
                });
            },
        );
        self.show_background = open;
        if pick_hdr && let Some(path) = rfd::FileDialog::new().add_filter("HDRI (Radiance .hdr)", &["hdr"]).pick_file() {
            self.settings.env_hdr = Some(path);
            self.settings.env = crate::render::env::EnvPreset::Custom;
        }

        let mut open = self.show_view_settings;
        egui::Window::new(tr("Настройки", "Settings")).open(&mut open).resizable(false).collapsible(false).show(ctx, |ui| {
            let s = &mut self.settings;
            egui::Grid::new("vs").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
                ui.label(tr("Угол обзора", "Field of view"));
                ui.add(egui::Slider::new(&mut s.fov_deg, 15.0..=100.0).suffix("°"));
                ui.end_row();
                ui.label(tr("Яркость света", "Light intensity"));
                ui.add(egui::Slider::new(&mut s.light_intensity, 0.0..=3.0));
                ui.end_row();
                ui.label(tr("Размер точек", "Point size"));
                ui.add(egui::Slider::new(&mut s.point_size, 1.0..=12.0).suffix(" px"));
                ui.end_row();
                ui.label(tr("Цвета вершин", "Vertex colours"));
                ui.vertical(|ui| {
                    ui.checkbox(&mut s.vcolor_default, tr("glTF, PLY, STL, 3MF, OFF", "glTF, PLY, STL, 3MF, OFF"));
                    ui.checkbox(&mut s.vcolor_dcc, tr("FBX, OBJ, DAE, 3DS, BLEND", "FBX, OBJ, DAE, 3DS, BLEND"));
                });
                ui.end_row();
                ui.label(tr("Язык", "Language"));
                ui.horizontal(|ui| {
                    let mut choice = s.language;
                    ui.radio_value(&mut choice, None, tr("системный", "system"));
                    ui.radio_value(&mut choice, Some(Lang::Ru), "Русский");
                    ui.radio_value(&mut choice, Some(Lang::En), "English");
                    if choice != s.language {
                        s.language = choice;
                        crate::i18n::set_lang(choice.unwrap_or_else(crate::i18n::detect));
                    }
                });
                ui.end_row();
                ui.label(tr("Обновления", "Updates"));
                ui.checkbox(&mut s.auto_update, tr("Проверять при запуске (раз в сутки)", "Check on start-up (once a day)"));
                ui.end_row();
            });
            if ui.button(format!("{} {}", icon::ARROW_COUNTER_CLOCKWISE, tr("Сбросить всё", "Reset all"))).clicked() {
                let recent = std::mem::take(&mut s.recent);
                let (language, last_update_check, skipped_version) = (s.language, s.last_update_check, s.skipped_version.take());
                *s = crate::settings::Settings { recent, language, last_update_check, skipped_version, ..Default::default() };
            }
        });
        self.show_view_settings = open;

        self.assoc_window(ctx);
        self.update_window(ctx);
    }
}

fn pivot_hint() -> &'static str {
    tr(
        "Где лежит начало координат файла (точка привязки модели) внутри габаритов по каждой оси: 0% — минимум, 50% — середина, 100% — максимум. Сетка и оси проходят через эту точку, если она рядом с моделью.",
        "Where the file origin (the model's pivot) sits inside the bounds on each axis: 0% = min, 50% = middle, 100% = max. The grid and axes go through this point when it is near the model.",
    )
}

/// "X 50% · Y 0% · Z 50% (bottom centre)"; the flag is set when the pivot is far away.
fn pivot_text(b: &crate::scene::Aabb, z_up: bool) -> (String, bool) {
    if !b.pivot_near() {
        return (format!("{}: {}", tr("далеко от модели", "far from the model"), fnum(b.distance_to_origin())), true);
    }
    let f = b.pivot_fraction();
    let pct = |v: f32| (v * 100.0).round() as i32;
    let (px, py, pz) = (pct(f.x), pct(f.y), pct(f.z));
    let (up, horiz) = if z_up { (pz, [px, py]) } else { (py, [px, pz]) };
    let word = if [px, py, pz].iter().any(|p| !(0..=100).contains(p)) {
        Some(tr("вне габаритов", "outside the bounds"))
    } else if horiz == [50, 50] {
        match up {
            0 => Some(tr("снизу по центру", "bottom centre")),
            50 => Some(tr("в центре", "centre")),
            100 => Some(tr("сверху по центру", "top centre")),
            _ => None,
        }
    } else {
        None
    };
    let mut s = format!("X {px}% · Y {py}% · Z {pz}%");
    if let Some(w) = word {
        s.push_str(&format!(" ({w})"));
    }
    (s, false)
}

fn yes(b: bool) -> &'static str {
    if b { tr("да", "yes") } else { tr("нет", "no") }
}

fn summary_text(s: &crate::app::Summary) -> String {
    let sz = s.bounds.size();
    let mut t = format!(
        "{}\n{}: {}\n{}: {}\n{}: {}\n{}: {}\n{}: {}\n{}: {}\n{}: {}\n{}: {} × {} × {}\n",
        s.path.display(),
        tr("Формат", "Format"),
        s.format,
        tr("Размер", "Size"),
        bytes(s.file_size),
        tr("Вершины", "Vertices"),
        s.vertices,
        tr("Треугольники", "Triangles"),
        s.triangles,
        tr("Меши", "Meshes"),
        s.meshes.len(),
        tr("Материалы", "Materials"),
        s.materials.len(),
        tr("Текстуры", "Textures"),
        s.textures.len(),
        tr("Габариты", "Bounds"),
        fnum(sz.x),
        fnum(sz.y),
        fnum(sz.z)
    );
    t += &format!("{}: {}\n", tr("Пивот", "Pivot"), pivot_text(&s.bounds, s.info.up_axis_z).0);
    if !s.info.animations.is_empty() {
        t += &format!("{}: {}\n", tr("Анимации", "Animations"), s.info.animations.join(", "));
    }
    t
}

#[cfg(test)]
mod tests {
    #[test]
    fn grouping() {
        assert_eq!(super::group(1234567), "1\u{202F}234\u{202F}567");
        assert_eq!(super::group(12), "12");
    }
}

const OK_GREEN: egui::Color32 = egui::Color32::from_rgb(110, 200, 120);
const WARN_ORANGE: egui::Color32 = egui::Color32::from_rgb(240, 170, 80);

fn open_url(url: &str) {
    let _ = std::process::Command::new("explorer").arg(url).spawn();
}

/// Release notes are Markdown; show them as plain text without the markup noise.
/// Arrows are missing from egui's default font, so they come from the icon font.
fn plain_notes(md: &str) -> String {
    md.lines()
        .map(|l| {
            let l = l.trim_end().trim_start_matches('#').trim_start();
            let l = match l.strip_prefix("- ").or_else(|| l.strip_prefix("* ")) {
                Some(rest) => format!("•  {rest}"),
                None => l.to_string(),
            };
            l.replace("**", "").replace('`', "").replace('→', icon::ARROW_RIGHT).replace('←', icon::ARROW_LEFT)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

impl ViewerApp {
    pub fn open_update_window(&mut self, ctx: &egui::Context) {
        use crate::update::State;
        self.show_update = true;
        self.update_banner = false;
        if matches!(self.updater.state, State::Idle | State::UpToDate(_) | State::Failed(_)) {
            self.updater.check(ctx, false);
        }
    }

    fn update_banner_panel(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        if !self.update_banner {
            return;
        }
        let crate::update::State::Available(rel) = &self.updater.state else { return };
        let v = rel.version;
        egui::Panel::top("update_banner").show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new(format!(
                        "{}  {} {v}",
                        icon::ARROW_CIRCLE_UP,
                        tr("Доступна новая версия", "New version available:")
                    ))
                    .color(egui::Color32::from_rgb(120, 180, 255)),
                );
                if ui.button(tr("Подробнее…", "Details…")).clicked() {
                    self.open_update_window(ctx);
                }
                if ui.small_button(icon::X).on_hover_text(tr("Скрыть", "Dismiss")).clicked() {
                    self.update_banner = false;
                }
            });
        });
    }

    fn assoc_window(&mut self, ctx: &egui::Context) {
        use crate::loaders::SUPPORTED_EXTENSIONS as ALL;
        let mut open = self.show_assoc;
        egui::Window::new(tr("Ассоциации файлов", "File associations")).open(&mut open).resizable(false).collapsible(false).show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label(tr("Отметьте форматы, которые должен открывать 3DViewer.", "Tick the formats 3DViewer should open."));
                if ui.small_button(icon::ARROWS_CLOCKWISE).on_hover_text(tr("Перечитать из реестра", "Re-read from the registry")).clicked() {
                    self.assoc_rows = crate::assoc::query(ALL);
                }
            });
            ui.add_space(4.0);
            egui::Grid::new("assoc").num_columns(3).spacing([16.0, 4.0]).striped(true).show(ui, |ui| {
                ui.label("");
                ui.label(egui::RichText::new(tr("Формат", "Format")).weak());
                ui.label(egui::RichText::new(tr("Сейчас открывает", "Opens with now")).weak());
                ui.end_row();
                for row in &self.assoc_rows {
                    let mut on = self.assoc_sel.contains(&row.ext);
                    if ui.checkbox(&mut on, egui::RichText::new(format!(".{}", row.ext)).monospace()).changed() {
                        if on {
                            self.assoc_sel.push(row.ext);
                        } else {
                            self.assoc_sel.retain(|e| *e != row.ext);
                        }
                    }
                    ui.label(crate::assoc::format_name(row.ext));
                    if row.is_default {
                        ui.label(egui::RichText::new(format!("{} 3DViewer", icon::CHECK)).color(OK_GREEN));
                    } else if let Some(app) = &row.default_app {
                        ui.label(app);
                    } else {
                        ui.label(egui::RichText::new(tr("не выбрано", "not set")).weak());
                    }
                    ui.end_row();
                }
            });
            ui.horizontal(|ui| {
                if ui.small_button(tr("Все", "All")).clicked() {
                    self.assoc_sel = ALL.to_vec();
                }
                if ui.small_button(tr("Ни одного", "None")).clicked() {
                    self.assoc_sel.clear();
                }
                if ui
                    .small_button(tr("Только свободные", "Only unclaimed"))
                    .on_hover_text(tr("Форматы, которые сейчас не открывает другая программа", "Formats no other program opens right now"))
                    .clicked()
                {
                    self.assoc_sel = self.assoc_rows.iter().filter(|r| r.is_default || r.default_app.is_none()).map(|r| r.ext).collect();
                }
            });
            ui.separator();
            let linked: Vec<&str> = self.assoc_rows.iter().filter(|r| r.linked).map(|r| r.ext).collect();
            let changed = linked.len() != self.assoc_sel.len() || linked.iter().any(|e| !self.assoc_sel.contains(e));
            ui.horizontal(|ui| {
                if ui.add_enabled(changed, egui::Button::new(format!("{} {}", icon::CHECK, tr("Применить", "Apply")))).clicked() {
                    let sel: Vec<&str> = ALL.iter().copied().filter(|e| self.assoc_sel.contains(e)).collect();
                    self.assoc_status = Some(match crate::assoc::apply(&sel, ALL) {
                        Ok(_) if sel.is_empty() => tr("3DViewer убран из ассоциаций", "3DViewer removed from associations").into(),
                        Ok(n) => format!(
                            "{}: {} {}, {} {}",
                            tr("Готово", "Done"),
                            sel.len(),
                            tr("связано", "linked"),
                            n,
                            tr("открываются в 3DViewer по умолчанию", "open in 3DViewer by default")
                        ),
                        Err(e) => format!("{}: {e:#}", tr("Ошибка", "Error")),
                    });
                    self.assoc_rows = crate::assoc::query(ALL);
                }
                if ui
                    .button(format!("{} {}", icon::GEAR, tr("Сделать по умолчанию…", "Make default…")))
                    .on_hover_text(tr("Откроет страницу 3DViewer в «Приложениях по умолчанию» Windows", "Opens 3DViewer's page in Windows Default apps"))
                    .clicked()
                {
                    crate::assoc::open_default_apps_settings();
                }
            });
            let taken: Vec<String> =
                self.assoc_rows.iter().filter(|r| r.linked && !r.is_default && r.default_app.is_some()).map(|r| format!(".{}", r.ext)).collect();
            if !taken.is_empty() {
                ui.label(
                    egui::RichText::new(format!(
                        "{} {}",
                        taken.join(" "),
                        tr(
                            "уже открывает другая программа. Windows не даёт приложениям менять этот выбор: нажмите «Сделать по умолчанию…», назначьте 3DViewer там и обновите таблицу.",
                            "already open in another program. Windows does not let apps change that choice: press \"Make default…\", pick 3DViewer there, then refresh the table."
                        )
                    ))
                    .small(),
                );
            }
            if let Some(st) = &self.assoc_status {
                ui.label(st);
            }
        });
        self.show_assoc = open;
    }

    fn update_window(&mut self, ctx: &egui::Context) {
        use crate::update::State;
        enum Action {
            Check,
            Install,
            Cancel,
            Skip(String),
            Restart,
            Page(String),
        }
        let mut open = self.show_update;
        let mut action = None;
        egui::Window::new(tr("Обновления", "Updates")).open(&mut open).resizable(false).collapsible(false).default_width(400.0).show(
            ctx,
            |ui| {
                ui.label(format!("{}: {}", tr("Установлена версия", "Installed version"), env!("CARGO_PKG_VERSION")));
                ui.add_space(4.0);
                match &self.updater.state {
                    State::Idle => {
                        ui.label(tr("Проверка ещё не выполнялась.", "Not checked yet."));
                    }
                    State::Checking => {
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.label(tr("Проверяю GitHub…", "Checking GitHub…"));
                        });
                    }
                    State::UpToDate(v) => {
                        ui.label(
                            egui::RichText::new(format!(
                                "{} {} ({} {v})",
                                icon::CHECK,
                                tr("Это последняя версия", "You have the latest version"),
                                tr("на GitHub", "on GitHub")
                            ))
                            .color(OK_GREEN),
                        );
                    }
                    State::Available(rel) => {
                        ui.label(egui::RichText::new(format!("{} {}", tr("Доступна версия", "Available version"), rel.version)).strong());
                        if !rel.notes.is_empty() {
                            egui::Frame::group(ui.style()).show(ui, |ui| {
                                egui::ScrollArea::vertical().max_height(180.0).show(ui, |ui| {
                                    ui.set_width(ui.available_width());
                                    ui.label(plain_notes(&rel.notes));
                                });
                            });
                        }
                        ui.horizontal(|ui| {
                            if rel.asset.is_some()
                                && ui
                                    .button(format!("{} {}", icon::DOWNLOAD_SIMPLE, tr("Скачать и установить", "Download and install")))
                                    .clicked()
                            {
                                action = Some(Action::Install);
                            }
                            if ui.button(tr("Страница релиза", "Release page")).clicked() {
                                action = Some(Action::Page(rel.page.clone()));
                            }
                            if ui.button(tr("Пропустить версию", "Skip this version")).clicked() {
                                action = Some(Action::Skip(rel.version.to_string()));
                            }
                        });
                        match &rel.asset {
                            Some(a) => {
                                ui.label(egui::RichText::new(format!("{} · {:.1} MB", a.name, a.size as f64 / 1048576.0)).small().weak())
                            }
                            None => ui.label(
                                egui::RichText::new(tr(
                                    "В релизе нет exe: скачайте вручную со страницы релиза.",
                                    "The release has no exe: download it from the release page.",
                                ))
                                .small(),
                            ),
                        };
                    }
                    State::Downloading(rel, p) => {
                        let done = p.done.load(std::sync::atomic::Ordering::Relaxed);
                        let total = p.total.load(std::sync::atomic::Ordering::Relaxed).max(1);
                        ui.label(format!("{} {}…", tr("Скачиваю и проверяю версию", "Downloading and verifying version"), rel.version));
                        ui.add(egui::ProgressBar::new(done as f32 / total as f32).text(format!(
                            "{:.1} / {:.1} MB",
                            done as f64 / 1048576.0,
                            total as f64 / 1048576.0
                        )));
                        if ui.button(tr("Отмена", "Cancel")).clicked() {
                            action = Some(Action::Cancel);
                        }
                        ui.ctx().request_repaint_after(std::time::Duration::from_millis(100));
                    }
                    State::Installed(rel, _) => {
                        ui.label(
                            egui::RichText::new(format!(
                                "{} {} {} {}",
                                icon::CHECK,
                                tr("Версия", "Version"),
                                rel.version,
                                tr("установлена и проверена.", "is installed and verified.")
                            ))
                            .color(OK_GREEN),
                        );
                        ui.label(tr("Она запустится при следующем старте.", "It runs from the next start."));
                        if ui.button(format!("{} {}", icon::ARROWS_CLOCKWISE, tr("Перезапустить сейчас", "Restart now"))).clicked()
                        {
                            action = Some(Action::Restart);
                        }
                    }
                    State::Failed(e) => {
                        ui.label(egui::RichText::new(format!("{} {e}", icon::WARNING)).color(WARN_ORANGE));
                        ui.horizontal(|ui| {
                            if ui.button(tr("Повторить", "Retry")).clicked() {
                                action = Some(Action::Check);
                            }
                            if ui.button(tr("Страница релизов", "Releases page")).clicked() {
                                action = Some(Action::Page(crate::update::releases_page()));
                            }
                        });
                    }
                }
                ui.separator();
                let idle = !self.updater.busy() && !matches!(self.updater.state, State::Installed(..));
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(
                            idle,
                            egui::Button::new(format!("{} {}", icon::ARROWS_CLOCKWISE, tr("Проверить снова", "Check again"))),
                        )
                        .clicked()
                    {
                        action = Some(Action::Check);
                    }
                    ui.checkbox(&mut self.settings.auto_update, tr("Проверять при запуске", "Check on start-up"));
                });
            },
        );
        match action {
            Some(Action::Check) => self.updater.check(ctx, false),
            Some(Action::Install) => self.updater.install(ctx),
            Some(Action::Cancel) => self.updater.cancel(),
            Some(Action::Skip(v)) => {
                self.settings.skipped_version = Some(v);
                self.update_banner = false;
                open = false;
            }
            Some(Action::Restart) => {
                self.restart_after_exit = true;
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            Some(Action::Page(u)) => open_url(&u),
            None => {}
        }
        self.show_update = open;
    }
}

// ---- Tool panels and the environment section ----

impl ViewerApp {
    /// Environment picker; returns true when the user asked to load a custom `.hdr`.
    fn environment_section(&mut self, ui: &mut egui::Ui, previews: &[(crate::render::env::EnvPreset, egui::TextureId)]) -> bool {
        use crate::render::env::EnvPreset;
        let mut pick = false;
        ui.label(egui::RichText::new(tr("Окружение (HDRI)", "Environment (HDRI)")).strong());
        ui.label(
            egui::RichText::new(tr(
                "Освещение и отражения от панорамы вокруг модели.",
                "Lighting and reflections from a panorama around the model.",
            ))
            .small()
            .weak(),
        );
        let s = &mut self.settings;
        let size = egui::vec2(96.0, 48.0);
        ui.horizontal_wrapped(|ui| {
            let tile = |ui: &mut egui::Ui, selected: bool, content: &mut dyn FnMut(&mut egui::Ui) -> egui::Response, name: &str| {
                ui.vertical(|ui| {
                    let r = content(ui);
                    if selected {
                        ui.painter().rect_stroke(
                            r.rect.expand(1.0),
                            4.0,
                            egui::Stroke::new(2.0, egui::Color32::from_rgb(90, 160, 255)),
                            egui::StrokeKind::Outside,
                        );
                    }
                    ui.label(egui::RichText::new(name).small());
                    r.clicked()
                })
                .inner
            };
            if tile(
                ui,
                s.env == EnvPreset::Basic,
                &mut |ui| ui.add_sized(size, egui::Button::new(icon::LIGHTBULB)),
                EnvPreset::Basic.label(),
            ) {
                s.env = EnvPreset::Basic;
            }
            for (p, tex) in previews {
                if tile(ui, s.env == *p, &mut |ui| ui.add(egui::Button::image(egui::Image::new((*tex, size))).frame(false)), p.label()) {
                    s.env = *p;
                }
            }
            let custom_name = s
                .env_hdr
                .as_ref()
                .and_then(|p| p.file_stem())
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| EnvPreset::Custom.label().to_string());
            if tile(
                ui,
                s.env == EnvPreset::Custom,
                &mut |ui| ui.add_sized(size, egui::Button::new(format!("{} .hdr", icon::FOLDER_OPEN))),
                &custom_name,
            ) {
                if s.env_hdr.is_some() && s.env != EnvPreset::Custom {
                    s.env = EnvPreset::Custom;
                } else {
                    pick = true;
                }
            }
        });
        if s.env != EnvPreset::Basic {
            egui::Grid::new("env").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
                ui.label(tr("Яркость", "Intensity"));
                ui.add(egui::Slider::new(&mut s.env_intensity, 0.0..=4.0));
                ui.end_row();
                ui.label(tr("Поворот", "Rotation"));
                ui.add(egui::Slider::new(&mut s.env_rotation_deg, -180.0..=180.0).suffix("°"));
                ui.end_row();
                ui.label(tr("Фон", "Background"));
                ui.checkbox(&mut s.env_background, tr("показывать окружение", "show the environment"));
                ui.end_row();
                if s.env_background {
                    ui.label(tr("Размытие", "Blur"));
                    ui.add(egui::Slider::new(&mut s.env_blur, 0.0..=1.0));
                    ui.end_row();
                }
            });
            if s.env == EnvPreset::Custom && ui.small_button(tr("Другой файл…", "Another file…")).clicked() {
                pick = true;
            }
        }
        if let Some(e) = &self.env_error {
            ui.label(egui::RichText::new(format!("{} {e}", icon::WARNING)).color(WARN_ORANGE).small());
        }
        pick
    }

    /// Floating panel in the viewport's top-right corner for the active tools.
    pub fn tool_panels(&mut self, ctx: &egui::Context, rect: egui::Rect) {
        let measuring = self.tool.measuring();
        let show_fly = self.tool == Tool::Fly;
        let show_measure = measuring || !self.measures.is_empty();
        if !show_fly && !show_measure && !self.slice.enabled {
            return;
        }
        egui::Area::new(egui::Id::unique("tool_panel"))
            .order(egui::Order::Foreground)
            .pivot(egui::Align2::RIGHT_TOP)
            .fixed_pos(rect.right_top() + egui::vec2(-10.0, 10.0))
            .show(ctx, |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.set_max_width(300.0);
                    let mut first = true;
                    let mut gap = |ui: &mut egui::Ui| {
                        if !std::mem::replace(&mut first, false) {
                            ui.separator();
                        }
                    };
                    if show_fly {
                        gap(ui);
                        self.fly_panel(ui);
                    }
                    if show_measure {
                        gap(ui);
                        self.measure_panel(ui);
                    }
                    if self.slice.enabled {
                        gap(ui);
                        self.slice_panel(ui);
                    }
                });
            });
    }

    fn fly_panel(&mut self, ui: &mut egui::Ui) {
        ui.label(egui::RichText::new(format!("{}  {}", icon::PERSON_SIMPLE_WALK, tr("Прогулка", "Walk"))).strong());
        ui.label(
            egui::RichText::new(tr(
                "WASD или стрелки — идти, Q/E — вниз/вверх, Shift — бегом, мышь с зажатой кнопкой — смотреть, колесо — скорость",
                "WASD or arrows — move, Q/E — down/up, Shift — run, drag the mouse — look, wheel — speed",
            ))
            .small(),
        );
        ui.horizontal(|ui| {
            ui.label(tr("Скорость", "Speed"));
            ui.label(
                egui::RichText::new(format!("{}/{}", crate::tools::format_length(self.fly_speed, self.unit_m()), tr("с", "s"))).strong(),
            );
        });
        ui.label(egui::RichText::new(tr("Esc — выход", "Esc — exit")).small().weak());
    }

    fn measure_panel(&mut self, ui: &mut egui::Ui) {
        let (glyph, title) = match self.tool {
            Tool::Angle => (icon::ANGLE, tr("Угол", "Angle")),
            Tool::Distance => (icon::RULER, tr("Линейка", "Ruler")),
            _ => (icon::RULER, tr("Измерения", "Measurements")),
        };
        ui.label(egui::RichText::new(format!("{glyph}  {title}")).strong());
        let hint = match self.tool {
            Tool::Distance => Some(tr(
                "Кликните две точки на модели. ПКМ — убрать последнюю точку, Esc — выход.",
                "Click two points on the model. RMB removes the last point, Esc exits.",
            )),
            Tool::Angle => Some(tr(
                "Кликните три точки: начало, вершину угла и конец. ПКМ — убрать последнюю, Esc — выход.",
                "Click three points: start, the vertex and the end. RMB removes the last one, Esc exits.",
            )),
            _ => None,
        };
        if let Some(h) = hint {
            ui.label(egui::RichText::new(h).small());
        }
        if self.file_unit_known() {
            if let Some(u) = self.unit_m() {
                ui.label(
                    egui::RichText::new(format!("{}: {}", tr("Единица файла", "File unit"), crate::tools::unit_name(u))).small().weak(),
                );
            }
        } else {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(tr("1 единица файла =", "1 file unit =")).small());
                let current = self.settings.assumed_unit_m;
                let name = |u: Option<f64>| u.map(crate::tools::unit_name).unwrap_or_else(|| tr("не знаю", "unknown").into());
                egui::ComboBox::from_id_salt("unit").selected_text(name(current)).width(90.0).show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.settings.assumed_unit_m, None, name(None));
                    for (m, _, _) in crate::tools::UNIT_CHOICES {
                        ui.selectable_value(&mut self.settings.assumed_unit_m, Some(m), name(Some(m)));
                    }
                });
            });
        }
        let unit = self.unit_m();
        let mut remove = None;
        for (i, m) in self.measures.iter().enumerate() {
            ui.horizontal(|ui| {
                if m.is_angle() {
                    ui.label(egui::RichText::new(icon::ANGLE).color(egui::Color32::from_rgb(110, 220, 255)));
                    ui.label(egui::RichText::new(format!("{:.1}°", m.angle_deg())).strong());
                } else {
                    ui.label(egui::RichText::new(icon::RULER).color(egui::Color32::from_rgb(255, 210, 80)));
                    ui.label(egui::RichText::new(crate::tools::format_length(m.length(), unit)).strong());
                    let d = (m.points[1] - m.points[0]).abs();
                    ui.label(
                        egui::RichText::new(format!(
                            "Δ {} · {} · {}",
                            crate::tools::format_length(d.x, unit),
                            crate::tools::format_length(d.y, unit),
                            crate::tools::format_length(d.z, unit)
                        ))
                        .small()
                        .weak(),
                    );
                }
                if ui.small_button(icon::X).on_hover_text(tr("Удалить", "Delete")).clicked() {
                    remove = Some(i);
                }
            });
        }
        if let Some(i) = remove {
            self.measures.remove(i);
        }
        if !self.measures.is_empty() && ui.small_button(format!("{} {}", icon::TRASH, tr("Очистить все", "Clear all"))).clicked()
        {
            self.measures.clear();
        }
    }

    fn slice_panel(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(format!("{}  {}", icon::KNIFE, tr("Срез", "Section"))).strong());
            for (i, n) in ["X", "Y", "Z"].into_iter().enumerate() {
                ui.selectable_value(&mut self.slice.axis, i, n);
            }
            if ui.button(icon::ARROWS_LEFT_RIGHT).on_hover_text(tr("Оставить другую часть", "Keep the other side")).clicked()
            {
                self.slice.flip = !self.slice.flip;
            }
            if ui.small_button(icon::X).on_hover_text(tr("Выключить срез (Esc)", "Turn the section off (Esc)")).clicked() {
                self.slice.enabled = false;
            }
        });
        ui.spacing_mut().slider_width = 230.0;
        ui.add(egui::Slider::new(&mut self.slice.t, 0.0..=1.0).fixed_decimals(2));
        if let Some(sm) = &self.summary {
            let pos = self.slice.position(&sm.bounds);
            let axis = ["X", "Y", "Z"][self.slice.axis];
            ui.label(egui::RichText::new(format!("{axis} = {}", crate::tools::format_length(pos, self.unit_m()))).small().weak());
        }
    }
}
