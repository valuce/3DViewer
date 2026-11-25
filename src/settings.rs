//! Persisted viewer settings.

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
            DisplayMode::Shaded => "Материалы",
            DisplayMode::Clay => "Глина",
            DisplayMode::Unlit => "Без света",
            DisplayMode::Normals => "Нормали",
            DisplayMode::UvChecker => "UV-сетка",
            DisplayMode::VertexColors => "Цвета вершин",
            DisplayMode::Wireframe => "Каркас",
            DisplayMode::Points => "Точки",
        }
    }

    /// Compact label for the toolbar.
    pub fn short_label(self) -> &'static str {
        match self {
            DisplayMode::Shaded => "Материалы",
            DisplayMode::Clay => "Глина",
            DisplayMode::Unlit => "Без света",
            DisplayMode::Normals => "Нормали",
            DisplayMode::UvChecker => "UV",
            DisplayMode::VertexColors => "Цвета",
            DisplayMode::Wireframe => "Каркас",
            DisplayMode::Points => "Точки",
        }
    }

    /// Value passed to the shader.
    pub fn shader_id(self) -> u32 {
        self as u32
    }
}

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
    pub background: [u8; 3],
    pub gradient_background: bool,
    pub light_intensity: f32,
    pub recent: Vec<PathBuf>,
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
            background: [38, 40, 46],
            gradient_background: true,
            light_intensity: 1.0,
            recent: Vec::new(),
        }
    }
}

impl Settings {
    pub fn push_recent(&mut self, p: PathBuf) {
        self.recent.retain(|r| r != &p);
        self.recent.insert(0, p);
        self.recent.truncate(12);
    }
}
