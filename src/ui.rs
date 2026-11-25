//! Menus, toolbar, info panel, status bar and auxiliary windows.

use crate::app::ViewerApp;
use crate::camera::ViewPreset;
use crate::scene::Topology;
use crate::settings::DisplayMode;

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
    if f >= 1024.0 * 1024.0 * 1024.0 {
        format!("{:.2} ГБ", f / 1024.0 / 1024.0 / 1024.0)
    } else if f >= 1024.0 * 1024.0 {
        format!("{:.1} МБ", f / 1024.0 / 1024.0)
    } else if f >= 1024.0 {
        format!("{:.1} КБ", f / 1024.0)
    } else {
        format!("{n} Б")
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

const VIEWS: &[(&str, ViewPreset, &str)] = &[
    ("Спереди", ViewPreset::Front, "Ctrl+1"),
    ("Сзади", ViewPreset::Back, ""),
    ("Справа", ViewPreset::Right, "Ctrl+3"),
    ("Слева", ViewPreset::Left, ""),
    ("Сверху", ViewPreset::Top, "Ctrl+7"),
    ("Снизу", ViewPreset::Bottom, "Ctrl+9"),
    ("Изометрия", ViewPreset::Iso, "R"),
];

const HELP: &[(&str, &str)] = &[
    ("ЛКМ", "вращать камеру"),
    ("ПКМ / СКМ / Shift+ЛКМ", "сдвигать камеру"),
    ("Колесо", "приближать"),
    ("Двойной клик, F", "показать модель целиком"),
    ("R", "сбросить вид"),
    ("1 … 8", "режимы отображения"),
    ("W", "рёбра поверх модели"),
    ("T", "текстуры вкл/выкл"),
    ("V", "цвета вершин в материале"),
    ("L", "плоское затенение"),
    ("C", "отсечение задних граней"),
    ("G / X / B", "сетка / оси / габариты"),
    ("P", "перспектива / ортогональная проекция"),
    ("U", "ось вверх Y / Z"),
    ("Ctrl+1 / 3 / 7 / 9", "вид спереди / справа / сверху / снизу"),
    ("PgUp / PgDn, ← / →", "предыдущий / следующий файл в папке"),
    ("Ctrl+O", "открыть файл"),
    ("F5", "перезагрузить файл"),
    ("I", "панель информации"),
    ("F11", "полный экран"),
    ("F1", "эта справка"),
];

impl ViewerApp {
    pub fn ui_panels(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        egui::Panel::top("menu").show(ui, |ui| {
            egui::MenuBar::new().ui(ui, |ui| {
                self.menu_file(ui, &ctx);
                self.menu_view(ui);
                ui.menu_button("Справка", |ui| {
                    if ui.button("Горячие клавиши   F1").clicked() {
                        self.show_help = true;
                        ui.close();
                    }
                });
            });
        });
        egui::Panel::top("toolbar").show(ui, |ui| self.toolbar(ui));
        egui::Panel::bottom("status").show(ui, |ui| self.status_bar(ui));
        if self.settings.show_info {
            egui::Panel::right("info").resizable(true).default_size(310.0).min_size(220.0).show(ui, |ui| {
                egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| self.info_panel(ui));
            });
        }
    }

    fn menu_file(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        ui.menu_button("Файл", |ui| {
            if ui.button("Открыть…   Ctrl+O").clicked() {
                ui.close();
                self.open_dialog(ctx);
            }
            ui.menu_button("Недавние", |ui| {
                if self.settings.recent.is_empty() {
                    ui.label("пусто");
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
                    if ui.button("Очистить список").clicked() {
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
            if ui.add_enabled(has, egui::Button::new("Перезагрузить   F5")).clicked() {
                ui.close();
                self.reload(ctx);
            }
            if ui.add_enabled(has, egui::Button::new("Предыдущий в папке   PgUp")).clicked() {
                ui.close();
                self.browse(ctx, -1);
            }
            if ui.add_enabled(has, egui::Button::new("Следующий в папке   PgDn")).clicked() {
                ui.close();
                self.browse(ctx, 1);
            }
            if ui.add_enabled(has, egui::Button::new("Показать в проводнике")).clicked() {
                ui.close();
                if let Some(p) = self.summary.as_ref().map(|s| s.path.clone()) {
                    let mut arg = std::ffi::OsString::from("/select,");
                    arg.push(p.as_os_str());
                    let _ = std::process::Command::new("explorer").arg(arg).spawn();
                }
            }
            if ui.add_enabled(has, egui::Button::new("Закрыть модель")).clicked() {
                ui.close();
                self.close_model(ctx);
            }
            ui.separator();
            if ui.button("Выход").clicked() {
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        });
    }

    fn menu_view(&mut self, ui: &mut egui::Ui) {
        ui.menu_button("Вид", |ui| {
            for (i, m) in DisplayMode::ALL.iter().enumerate() {
                ui.radio_value(&mut self.settings.mode, *m, format!("{}   {}", m.label(), i + 1));
            }
            ui.separator();
            let s = &mut self.settings;
            ui.checkbox(&mut s.wire_overlay, "Рёбра поверх модели   W");
            ui.checkbox(&mut s.textures, "Текстуры   T");
            ui.checkbox(&mut s.flat_shading, "Плоское затенение   L");
            ui.checkbox(&mut self.vertex_colors, "Цвета вершин в материале   V");
            ui.checkbox(&mut s.backface_culling, "Отсекать задние грани   C");
            ui.separator();
            ui.checkbox(&mut s.show_grid, "Сетка   G");
            ui.checkbox(&mut s.show_axes, "Оси   X");
            ui.checkbox(&mut s.show_bbox, "Габаритный бокс   B");
            ui.checkbox(&mut s.show_info, "Панель информации   I");
            ui.separator();
            if ui.checkbox(&mut s.ortho, "Ортогональная проекция   P").changed() {
                self.camera.ortho = self.settings.ortho;
            }
            ui.checkbox(&mut self.camera.z_up, "Ось Z вверх   U");
            ui.menu_button("Камера", |ui| self.camera_menu(ui));
            ui.separator();
            if ui.button("Настройки вида…").clicked() {
                self.show_view_settings = true;
                ui.close();
            }
        });
    }

    fn camera_menu(&mut self, ui: &mut egui::Ui) {
        for (name, p, key) in VIEWS {
            let label = if key.is_empty() { name.to_string() } else { format!("{name}   {key}") };
            if ui.button(label).clicked() {
                self.camera.set_preset(*p);
                if *p == ViewPreset::Iso {
                    self.frame_model();
                }
                ui.close();
            }
        }
        ui.separator();
        if ui.button("Показать целиком   F").clicked() {
            self.frame_model();
            ui.close();
        }
    }

    fn toolbar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            for (i, m) in DisplayMode::ALL.iter().enumerate() {
                let sel = self.settings.mode == *m;
                if ui.selectable_label(sel, m.short_label()).on_hover_text(format!("{} (клавиша {})", m.label(), i + 1)).clicked() {
                    self.settings.mode = *m;
                }
            }
            ui.separator();
            let s = &mut self.settings;
            ui.toggle_value(&mut s.wire_overlay, "Рёбра").on_hover_text("рёбра поверх модели (W)");
            ui.toggle_value(&mut s.textures, "Текстуры").on_hover_text("T");
            ui.toggle_value(&mut s.flat_shading, "Плоско").on_hover_text("плоское затенение (L)");
            ui.toggle_value(&mut self.vertex_colors, "Цв. вершин").on_hover_text("умножать материал на цвета вершин (V)");
            ui.toggle_value(&mut s.show_grid, "Сетка").on_hover_text("G");
            ui.toggle_value(&mut s.show_axes, "Оси").on_hover_text("X");
            ui.toggle_value(&mut s.show_bbox, "Габариты").on_hover_text("B");
            ui.separator();
            if ui.toggle_value(&mut s.ortho, "Орто").on_hover_text("ортогональная проекция (P)").changed() {
                self.camera.ortho = self.settings.ortho;
            }
            ui.menu_button("Камера", |ui| self.camera_menu(ui));
            if ui.button("Целиком").on_hover_text("показать модель целиком (F)").clicked() {
                self.frame_model();
            }
            ui.separator();
            ui.toggle_value(&mut self.settings.show_info, "Инфо").on_hover_text("панель информации (I)");
        });
    }

    fn status_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if let Some(s) = &self.summary {
                ui.label(egui::RichText::new(s.path.file_name().unwrap_or_default().to_string_lossy()).strong());
                ui.separator();
                ui.label(format!("вершин {}", group(s.vertices)));
                ui.label(format!("треугольников {}", group(s.triangles)));
                ui.separator();
                ui.label(format!("загрузка {:.0} мс + GPU {:.0} мс", s.load_ms, s.upload_ms));
            } else {
                ui.label("модель не открыта");
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if let Some(r) = &self.renderer {
                    ui.label(egui::RichText::new(&r.adapter_info).weak());
                    ui.separator();
                    ui.label(egui::RichText::new(format!("MSAA {}×", r.sample_count)).weak());
                }
                ui.separator();
                ui.label(egui::RichText::new(format!("кадр {:.2} мс", self.last_frame_ms)).weak());
            });
        });
    }

    fn info_panel(&mut self, ui: &mut egui::Ui) {
        let Some(s) = &self.summary else {
            ui.add_space(8.0);
            ui.label("Откройте модель, чтобы увидеть информацию.");
            return;
        };
        let mut copy = None;
        let row = |ui: &mut egui::Ui, k: &str, v: String| {
            ui.label(egui::RichText::new(k).weak());
            ui.label(v);
            ui.end_row();
        };

        egui::CollapsingHeader::new("Файл").default_open(true).show(ui, |ui| {
            egui::Grid::new("file").num_columns(2).spacing([10.0, 3.0]).show(ui, |ui| {
                row(ui, "Имя", s.path.file_name().unwrap_or_default().to_string_lossy().into_owned());
                row(ui, "Формат", s.format.clone());
                row(ui, "Размер", bytes(s.file_size));
                row(ui, "Чтение", format!("{:.1} мс", s.load_ms));
                row(ui, "Выгрузка в GPU", format!("{:.1} мс", s.upload_ms));
                row(ui, "Память GPU", bytes(s.gpu_bytes));
                for (k, v) in &s.info.extra {
                    row(ui, k, v.clone());
                }
            });
            ui.label(egui::RichText::new(s.path.display().to_string()).small().weak());
        });

        egui::CollapsingHeader::new("Геометрия").default_open(true).show(ui, |ui| {
            egui::Grid::new("geo").num_columns(2).spacing([10.0, 3.0]).show(ui, |ui| {
                row(ui, "Вершины", group(s.vertices));
                row(ui, "Треугольники", group(s.triangles));
                if s.points > 0 {
                    row(ui, "Точки", group(s.points));
                }
                if s.lines > 0 {
                    row(ui, "Линии", group(s.lines));
                }
                row(ui, "Меши", group(s.meshes.len()));
                row(ui, "Материалы", group(s.materials.len()));
                row(ui, "Текстуры", group(s.textures.len()));
                let uv = s.meshes.iter().filter(|m| m.uv).count();
                let col = s.meshes.iter().filter(|m| m.colors).count();
                let nrm = s.meshes.iter().filter(|m| m.had_normals).count();
                let n = s.meshes.len();
                row(ui, "UV", format!("{uv} из {n} мешей"));
                row(ui, "Нормали в файле", format!("{nrm} из {n}"));
                row(ui, "Цвета вершин", format!("{col} из {n}"));
            });
        });

        egui::CollapsingHeader::new("Габариты").default_open(true).show(ui, |ui| {
            let b = s.bounds;
            let sz = b.size();
            egui::Grid::new("bounds").num_columns(2).spacing([10.0, 3.0]).show(ui, |ui| {
                row(ui, "Размер X", fnum(sz.x));
                row(ui, "Размер Y", fnum(sz.y));
                row(ui, "Размер Z", fnum(sz.z));
                row(ui, "Мин", format!("{}, {}, {}", fnum(b.min.x), fnum(b.min.y), fnum(b.min.z)));
                row(ui, "Макс", format!("{}, {}, {}", fnum(b.max.x), fnum(b.max.y), fnum(b.max.z)));
                let c = b.center();
                row(ui, "Центр", format!("{}, {}, {}", fnum(c.x), fnum(c.y), fnum(c.z)));
                row(ui, "Ось вверх", if self.camera.z_up { "Z".into() } else { "Y".into() });
            });
        });

        let i = &s.info;
        if i.nodes + i.skins + i.bones + i.cameras + i.lights + i.morph_targets + i.animations.len() > 0 {
            egui::CollapsingHeader::new("Сцена").default_open(true).show(ui, |ui| {
                egui::Grid::new("scene").num_columns(2).spacing([10.0, 3.0]).show(ui, |ui| {
                    row(ui, "Узлы", group(i.nodes));
                    if i.skins > 0 {
                        row(ui, "Скины", group(i.skins));
                    }
                    if i.bones > 0 {
                        row(ui, "Кости", group(i.bones));
                    }
                    if i.morph_targets > 0 {
                        row(ui, "Морфы", group(i.morph_targets));
                    }
                    if i.cameras > 0 {
                        row(ui, "Камеры", group(i.cameras));
                    }
                    if i.lights > 0 {
                        row(ui, "Источники света", group(i.lights));
                    }
                    row(ui, "Анимации", group(i.animations.len()));
                });
                for a in &i.animations {
                    ui.label(format!("• {a}"));
                }
            });
        }

        let mut hovered = None;
        let mut vis_changes = Vec::new();
        egui::CollapsingHeader::new(format!("Меши ({})", s.meshes.len())).default_open(s.meshes.len() <= 30).show(ui, |ui| {
            ui.horizontal(|ui| {
                if ui.small_button("Показать все").clicked() {
                    vis_changes.extend((0..s.meshes.len()).map(|i| (i, true)));
                }
                if ui.small_button("Скрыть все").clicked() {
                    vis_changes.extend((0..s.meshes.len()).map(|i| (i, false)));
                }
            });
            for (idx, m) in s.meshes.iter().enumerate().take(2000) {
                let mut vis = self.mesh_visible(idx);
                let kind = match m.topology {
                    Topology::Triangles => format!("{} тр.", group(m.tris)),
                    Topology::Lines => "линии".into(),
                    Topology::Points => format!("{} точ.", group(m.verts)),
                };
                let r = ui.checkbox(&mut vis, format!("{}  · {kind}", m.name));
                let r = r.on_hover_ui(|ui| {
                    ui.label(format!("вершин: {}", group(m.verts)));
                    ui.label(format!("материал: {}", m.material.as_deref().unwrap_or("—")));
                    ui.label(format!(
                        "UV: {}  цвета: {}  нормали: {}",
                        yes(m.uv),
                        yes(m.colors),
                        if m.had_normals { "из файла" } else { "вычислены" }
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
                ui.label(format!("… ещё {}", s.meshes.len() - 2000));
            }
        });

        if !s.materials.is_empty() {
            egui::CollapsingHeader::new(format!("Материалы ({})", s.materials.len())).default_open(s.materials.len() <= 12).show(
                ui,
                |ui| {
                    for m in s.materials.iter().take(500) {
                        ui.horizontal(|ui| {
                            let c = m.base_color;
                            let to_srgb = |x: f32| {
                                (if x <= 0.0031308 { x * 12.92 } else { 1.055 * x.powf(1.0 / 2.4) - 0.055 } * 255.0).clamp(0.0, 255.0) as u8
                            };
                            let (r, _) = ui.allocate_exact_size(egui::vec2(14.0, 14.0), egui::Sense::hover());
                            ui.painter().rect_filled(r, 3.0, egui::Color32::from_rgb(to_srgb(c[0]), to_srgb(c[1]), to_srgb(c[2])));
                            ui.label(&m.name).on_hover_ui(|ui| {
                                ui.label(format!("цвет: {:.2} {:.2} {:.2} α {:.2}", c[0], c[1], c[2], c[3]));
                                ui.label(format!("металл {:.2}  шероховатость {:.2}", m.metallic, m.roughness));
                                if m.blend {
                                    ui.label("прозрачный");
                                }
                                if m.double_sided {
                                    ui.label("двусторонний");
                                }
                            });
                            if !m.maps.is_empty() {
                                ui.label(egui::RichText::new(m.maps.join(", ")).small().weak());
                            }
                        });
                    }
                },
            );
        }

        if !s.textures.is_empty() {
            egui::CollapsingHeader::new(format!("Текстуры ({})", s.textures.len())).default_open(s.textures.len() <= 12).show(ui, |ui| {
                for (name, w, h) in &s.textures {
                    ui.label(format!("{name}  ·  {w}×{h}"));
                }
            });
        }

        if !s.info.warnings.is_empty() {
            egui::CollapsingHeader::new(
                egui::RichText::new(format!("Предупреждения ({})", s.info.warnings.len())).color(egui::Color32::from_rgb(240, 190, 80)),
            )
            .default_open(true)
            .show(ui, |ui| {
                for w in s.info.warnings.iter().take(200) {
                    ui.label(egui::RichText::new(w).small());
                }
            });
        }

        ui.add_space(6.0);
        if ui.button("Копировать сводку").clicked() {
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
        egui::Window::new("Горячие клавиши").open(&mut open).resizable(false).collapsible(false).show(ctx, |ui| {
            egui::Grid::new("help").num_columns(2).spacing([16.0, 4.0]).show(ui, |ui| {
                for (k, v) in HELP {
                    ui.label(egui::RichText::new(*k).strong());
                    ui.label(*v);
                    ui.end_row();
                }
            });
            if let Some(ms) = self.startup_ms {
                ui.separator();
                ui.label(egui::RichText::new(format!("Запуск до первого кадра: {ms:.0} мс")).weak());
            }
        });
        self.show_help = open;

        let mut open = self.show_view_settings;
        egui::Window::new("Настройки вида").open(&mut open).resizable(false).collapsible(false).show(ctx, |ui| {
            let s = &mut self.settings;
            egui::Grid::new("vs").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
                ui.label("Угол обзора");
                ui.add(egui::Slider::new(&mut s.fov_deg, 15.0..=100.0).suffix("°"));
                ui.end_row();
                ui.label("Яркость света");
                ui.add(egui::Slider::new(&mut s.light_intensity, 0.0..=3.0));
                ui.end_row();
                ui.label("Размер точек");
                ui.add(egui::Slider::new(&mut s.point_size, 1.0..=12.0).suffix(" px"));
                ui.end_row();
                ui.label("Фон");
                ui.horizontal(|ui| {
                    ui.color_edit_button_srgb(&mut s.background);
                    ui.checkbox(&mut s.gradient_background, "градиент");
                });
                ui.end_row();
            });
            if ui.button("Сбросить").clicked() {
                let recent = std::mem::take(&mut s.recent);
                *s = crate::settings::Settings { recent, ..Default::default() };
            }
        });
        self.show_view_settings = open;
    }
}

fn yes(b: bool) -> &'static str {
    if b { "да" } else { "нет" }
}

fn summary_text(s: &crate::app::Summary) -> String {
    let sz = s.bounds.size();
    let mut t = format!(
        "{}\nФормат: {}\nРазмер: {}\nВершины: {}\nТреугольники: {}\nМеши: {}\nМатериалы: {}\nТекстуры: {}\nГабариты: {} × {} × {}\n",
        s.path.display(),
        s.format,
        bytes(s.file_size),
        s.vertices,
        s.triangles,
        s.meshes.len(),
        s.materials.len(),
        s.textures.len(),
        fnum(sz.x),
        fnum(sz.y),
        fnum(sz.z)
    );
    if !s.info.animations.is_empty() {
        t += &format!("Анимации: {}\n", s.info.animations.join(", "));
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
