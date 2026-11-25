//! Menus, icon toolbar, info panel, animation timeline, status bar and windows.

use crate::app::ViewerApp;
use crate::camera::ViewPreset;
use crate::i18n::{Lang, tr};
use crate::scene::Topology;
use crate::settings::{BG_PRESETS, DisplayMode};
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
                });
            });
        });
        egui::Panel::top("toolbar").show(ui, |ui| self.toolbar(ui, &ctx));
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
                self.show_assoc = true;
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
            }
            ui.menu_button(egui::RichText::new(icon::CAMERA).size(18.0), |ui| self.camera_menu(ui))
                .response
                .on_hover_ui(|ui| tip(ui, tr("Камера", "Camera"), tr("Стандартные виды", "Standard views"), ""));
            if tool_button(ui, icon::CORNERS_OUT, tr("Показать целиком", "Frame model"), "", "F") {
                self.frame_model();
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
            ui.label(v);
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
                ui.label(egui::RichText::new(s.path.display().to_string()).small().weak());
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
        egui::Window::new(tr("Фон", "Background")).open(&mut open).resizable(false).collapsible(false).show(ctx, |ui| {
            let s = &mut self.settings;
            ui.label(tr("Пресеты", "Presets"));
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
        });
        self.show_background = open;

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
            });
            if ui.button(format!("{} {}", icon::ARROW_COUNTER_CLOCKWISE, tr("Сбросить всё", "Reset all"))).clicked() {
                let recent = std::mem::take(&mut s.recent);
                let language = s.language;
                *s = crate::settings::Settings { recent, language, ..Default::default() };
            }
        });
        self.show_view_settings = open;

        let mut open = self.show_assoc;
        egui::Window::new(tr("Ассоциации файлов", "File associations")).open(&mut open).resizable(false).collapsible(false).show(ctx, |ui| {
            ui.label(tr(
                "Добавляет 3DViewer в «Открыть с помощью» для всех поддерживаемых форматов и делает его программой по умолчанию там, где её ещё нет. Права администратора не нужны.",
                "Adds 3DViewer to \"Open with\" for every supported format and makes it the default where no default exists yet. No admin rights needed.",
            ));
            ui.label(egui::RichText::new(crate::loaders::SUPPORTED_EXTENSIONS.iter().map(|e| format!(".{e}")).collect::<Vec<_>>().join("  ")).monospace().weak());
            ui.label(egui::RichText::new(tr(
                "Windows не даёт программам менять уже выбранное приложение по умолчанию — для этого откройте настройки Windows.",
                "Windows does not let apps override a default you already chose — use Windows settings for that.",
            )).small());
            let registered = crate::assoc::is_registered();
            ui.label(if registered { format!("{} {}", icon::CHECK, tr("Зарегистрировано для этого exe", "Registered for this exe")) } else { tr("Не зарегистрировано", "Not registered").into() });
            ui.horizontal(|ui| {
                if ui.button(format!("{} {}", icon::LINK, tr("Связать", "Associate"))).clicked() {
                    self.assoc_status = Some(match crate::assoc::register(crate::loaders::SUPPORTED_EXTENSIONS) {
                        Ok(n) => format!("{} ({n} {})", tr("Готово", "Done"), tr("по умолчанию", "set as default")),
                        Err(e) => format!("{}: {e:#}", tr("Ошибка", "Error")),
                    });
                }
                if ui.button(format!("{} {}", icon::X, tr("Убрать", "Remove"))).clicked() {
                    self.assoc_status = Some(match crate::assoc::unregister(crate::loaders::SUPPORTED_EXTENSIONS) {
                        Ok(()) => tr("Ассоциации удалены", "Associations removed").into(),
                        Err(e) => format!("{}: {e:#}", tr("Ошибка", "Error")),
                    });
                }
                if ui.button(format!("{} {}", icon::GEAR, tr("Настройки Windows", "Windows settings"))).clicked() {
                    crate::assoc::open_default_apps_settings();
                }
            });
            if let Some(st) = &self.assoc_status {
                ui.label(st);
            }
        });
        self.show_assoc = open;
    }
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
