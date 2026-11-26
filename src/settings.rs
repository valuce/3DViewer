//! Persisted viewer settings.

use crate::i18n::{Lang, tr};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DisplayMode {
    Shaded,
    Clay,
    Unlit,
    Normals,
    UvChecker,
    VertexColors,
    Wireframe,
    Points,
}

impl DisplayMode {
    pub const ALL: [DisplayMode; 8] = [
        DisplayMode::Shaded,
        DisplayMode::Clay,
        DisplayMode::Unlit,
        DisplayMode::Normals,
        DisplayMode::UvChecker,
        DisplayMode::VertexColors,
        DisplayMode::Wireframe,
        DisplayMode::Points,
    ];

    pub fn label(self) -> &'static str {
        match self {
            DisplayMode::Shaded => tr("Материалы", "Materials"),
            DisplayMode::Clay => tr("Глина", "Clay"),
            DisplayMode::Unlit => tr("Без освещения", "Unlit"),
            DisplayMode::Normals => tr("Нормали", "Normals"),
            DisplayMode::UvChecker => tr("UV-шахматка", "UV checker"),
            DisplayMode::VertexColors => tr("Цвета вершин", "Vertex colours"),
            DisplayMode::Wireframe => tr("Каркас", "Wireframe"),
            DisplayMode::Points => tr("Точки", "Points"),
        }
    }

    pub fn hint(self) -> &'static str {
        match self {
            DisplayMode::Shaded => tr("PBR-материалы, текстуры и освещение", "PBR materials, textures and lighting"),
            DisplayMode::Clay => tr("Однотонная поверхность — видно форму", "Plain surface that shows the shape"),
            DisplayMode::Unlit => tr("Цвета и текстуры без света", "Colours and textures without lighting"),
            DisplayMode::Normals => tr("Направления нормалей цветом", "Normal directions as colours"),
            DisplayMode::UvChecker => tr("Шахматка по UV — видно растяжения", "Checker by UV to reveal stretching"),
            DisplayMode::VertexColors => tr("Только цвета вершин", "Vertex colours only"),
            DisplayMode::Wireframe => tr("Рёбра без невидимых линий", "Edges with hidden lines removed"),
            DisplayMode::Points => tr("Вершины точками", "Vertices as points"),
        }
    }

    pub fn icon(self) -> &'static str {
        use egui_phosphor::regular as i;
        match self {
            DisplayMode::Shaded => i::CUBE,
            DisplayMode::Clay => i::SPHERE,
            DisplayMode::Unlit => i::SUN_DIM,
            DisplayMode::Normals => i::COMPASS,
            DisplayMode::UvChecker => i::CHECKERBOARD,
            DisplayMode::VertexColors => i::PALETTE,
            DisplayMode::Wireframe => i::CUBE_TRANSPARENT,
            DisplayMode::Points => i::DOTS_NINE,
        }
    }

    /// Value passed to the shader.
    pub fn shader_id(self) -> u32 {
        self as u32
    }
}

/// Background presets: (name ru, name en, top, bottom, gradient).
pub const BG_PRESETS: &[(&str, &str, [u8; 3], [u8; 3], bool)] = &[
    ("Графит", "Graphite", [52, 55, 63], [21, 22, 25], true),
    ("Ночь", "Night", [28, 36, 58], [8, 10, 16], true),
    ("Студия", "Studio", [205, 208, 214], [120, 124, 132], true),
    ("Светлый", "Light", [244, 245, 247], [214, 217, 222], true),
    ("Серый", "Grey", [96, 98, 104], [96, 98, 104], false),
    ("Чёрный", "Black", [0, 0, 0], [0, 0, 0], false),
    ("Белый", "White", [255, 255, 255], [255, 255, 255], false),
    ("Хромакей", "Chroma green", [0, 177, 64], [0, 177, 64], false),
];

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub mode: DisplayMode,
    pub wire_overlay: bool,
    pub textures: bool,
    pub flat_shading: bool,
    pub backface_culling: bool,
    pub show_grid: bool,
    pub show_axes: bool,
    pub show_bbox: bool,
    pub show_info: bool,
    pub ortho: bool,
    pub fov_deg: f32,
    pub point_size: f32,
    pub bg_top: [u8; 3],
    pub bg_bottom: [u8; 3],
    pub bg_gradient: bool,
    pub light_intensity: f32,
    /// Vertex colours for formats where they are usually real colours (glTF, PLY, OFF, STL, 3MF).
    pub vcolor_default: bool,
    /// Vertex colours for DCC / game formats where they are often masks (FBX, OBJ, DAE, 3DS, BLEND).
    pub vcolor_dcc: bool,
    /// None = follow the system language.
    pub language: Option<Lang>,
    pub recent: Vec<PathBuf>,
    /// Look for a new release on start-up (at most once a day).
    pub auto_update: bool,
    /// Unix time of the last automatic update check.
    pub last_update_check: u64,
    /// Release the user chose to skip; automatic checks stay quiet about it.
    pub skipped_version: Option<String>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            mode: DisplayMode::Shaded,
            wire_overlay: false,
            textures: true,
            flat_shading: false,
            backface_culling: false,
            show_grid: true,
            show_axes: true,
            show_bbox: false,
            show_info: true,
            ortho: false,
            fov_deg: 45.0,
            point_size: 3.0,
            bg_top: BG_PRESETS[0].2,
            bg_bottom: BG_PRESETS[0].3,
            bg_gradient: true,
            light_intensity: 1.0,
            vcolor_default: true,
            vcolor_dcc: false,
            language: None,
            recent: Vec::new(),
            auto_update: true,
            last_update_check: 0,
            skipped_version: None,
        }
    }
}

impl Settings {
    pub fn push_recent(&mut self, p: PathBuf) {
        self.recent.retain(|r| r != &p);
        self.recent.insert(0, p);
        self.recent.truncate(12);
    }

    /// Perceived brightness of the background (0..1), used to pick grid / text colours.
    pub fn bg_luma(&self) -> f32 {
        let l = |c: [u8; 3]| (0.299 * c[0] as f32 + 0.587 * c[1] as f32 + 0.114 * c[2] as f32) / 255.0;
        if self.bg_gradient { (l(self.bg_top) + l(self.bg_bottom)) * 0.5 } else { l(self.bg_top) }
    }
}
