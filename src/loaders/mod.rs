//! File format loaders. Every loader returns a format-neutral [`Scene`].

pub mod blend;
pub mod dae;
pub mod draco;
pub mod fbx;
pub mod gltf;
pub mod image;
pub mod max3ds;
pub mod off;
pub mod ply;
pub mod stl;
pub mod threemf;

use crate::scene::Scene;
use anyhow::{Context, Result, bail};
use std::path::Path;
use std::time::Instant;

/// Lower-case extensions this viewer can open.
pub const SUPPORTED_EXTENSIONS: &[&str] = &["glb", "gltf", "obj", "fbx", "stl", "ply", "3mf", "off", "3ds", "dae", "blend"];

pub fn is_supported(path: &Path) -> bool {
    extension(path).is_some_and(|e| SUPPORTED_EXTENSIONS.contains(&e.as_str()))
}

pub fn extension(path: &Path) -> Option<String> {
    path.extension().map(|e| e.to_string_lossy().to_ascii_lowercase())
}

/// Read a whole file through a memory map (fast for big binary meshes).
pub fn map_file(path: &Path) -> Result<memmap2::Mmap> {
    let file = std::fs::File::open(path).with_context(|| format!("cannot open {}", path.display()))?;
    // SAFETY: the file is only read; concurrent external modification is an accepted risk for a viewer.
    let map = unsafe { memmap2::Mmap::map(&file) }.with_context(|| format!("cannot map {}", path.display()))?;
    Ok(map)
}

/// Load any supported file. Panics inside third-party parsers are converted into errors.
pub fn load_file(path: &Path) -> Result<Scene> {
    let start = Instant::now();
    let path_owned = path.to_path_buf();
    let result = std::panic::catch_unwind(move || load_file_inner(&path_owned));
    let mut scene = match result {
        Ok(r) => r?,
        Err(p) => {
            let msg = p
                .downcast_ref::<&str>()
                .map(|s| s.to_string())
                .or_else(|| p.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "unknown error".into());
            bail!("parser crashed: {msg}");
        }
    };
    scene.finalize();
    scene.info.file_size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    scene.info.load_ms = start.elapsed().as_secs_f64() * 1000.0;
    Ok(scene)
}

fn load_file_inner(path: &Path) -> Result<Scene> {
    let ext = extension(path).unwrap_or_default();
    let dir = path.parent();
    match ext.as_str() {
        "stl" => stl::parse_stl(&map_file(path)?),
        "ply" => ply::parse_ply(&map_file(path)?, dir),
        "off" => {
            let data = map_file(path)?;
            off::parse_off(&String::from_utf8_lossy(&data))
        }
        "3mf" => threemf::parse_3mf(&map_file(path)?),
        "3ds" => max3ds::parse_3ds(&map_file(path)?, dir),
        "glb" | "gltf" => gltf::load_gltf(path),
        "fbx" | "obj" => fbx::load_ufbx(path),
        "dae" => dae::load_dae(path),
        "blend" => blend::parse_blend(&map_file(path)?),
        _ => bail!("unsupported file type: .{ext}"),
    }
}

/// Little helpers shared by the binary parsers.
pub(crate) fn read_f32_le(b: &[u8], off: usize) -> f32 {
    f32::from_le_bytes([b[off], b[off + 1], b[off + 2], b[off + 3]])
}
pub(crate) fn read_u32_le(b: &[u8], off: usize) -> u32 {
    u32::from_le_bytes([b[off], b[off + 1], b[off + 2], b[off + 3]])
}
