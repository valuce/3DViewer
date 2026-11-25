//! Image decoding helpers (parallel, RGBA8 output).

use crate::scene::TextureData;
use anyhow::{Result, anyhow};
use std::path::Path;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

pub fn decode(name: &str, bytes: &[u8]) -> Result<TextureData> {
    let img = image::load_from_memory(bytes).map_err(|e| anyhow!("{e}"))?;
    let rgba = img.into_rgba8();
    Ok(TextureData { name: name.to_string(), width: rgba.width(), height: rgba.height(), rgba: rgba.into_raw() })
}

/// Where image bytes come from.
pub enum ImageSource {
    Bytes(Vec<u8>),
    File(std::path::PathBuf),
}

/// Decode many images on all cores. Output order matches input order.
pub fn decode_parallel(jobs: Vec<(String, ImageSource)>) -> Vec<Result<TextureData>> {
    let n = jobs.len();
    if n == 0 {
        return Vec::new();
    }
    let results: Vec<Mutex<Option<Result<TextureData>>>> = (0..n).map(|_| Mutex::new(None)).collect();
    let next = AtomicUsize::new(0);
    let threads = std::thread::available_parallelism().map(|c| c.get()).unwrap_or(4).min(n);
    std::thread::scope(|s| {
        for _ in 0..threads {
            s.spawn(|| {
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    if i >= n {
                        break;
                    }
                    let (name, src) = &jobs[i];
                    let r = match src {
                        ImageSource::Bytes(b) => decode(name, b),
                        ImageSource::File(p) => {
                            std::fs::read(p).map_err(|e| anyhow!("{}: {e}", p.display())).and_then(|b| decode(name, &b))
                        }
                    };
                    *results[i].lock().unwrap() = Some(r);
                }
            });
        }
    });
    results.into_iter().map(|m| m.into_inner().unwrap().unwrap_or_else(|| Err(anyhow!("not decoded")))).collect()
}

/// Resolve a texture path referenced by a model file: absolute, relative to the model,
/// or just the file name next to the model.
pub fn resolve_texture_path(dir: Option<&Path>, candidates: &[&str]) -> Option<std::path::PathBuf> {
    for c in candidates.iter().filter(|c| !c.is_empty()) {
        let c = c.replace('\\', "/");
        let p = Path::new(&c);
        if p.is_absolute() && p.is_file() {
            return Some(p.to_path_buf());
        }
        if let Some(d) = dir {
            let j = d.join(p);
            if j.is_file() {
                return Some(j);
            }
            if let Some(f) = p.file_name() {
                for sub in ["", "textures", "Textures", "tex", "images"] {
                    let j = d.join(sub).join(f);
                    if j.is_file() {
                        return Some(j);
                    }
                }
            }
        }
    }
    None
}
