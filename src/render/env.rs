//! Image-based lighting: equirectangular environment maps (built-in procedural ones or a
//! user `.hdr`), their spherical-harmonics irradiance and a mip chain for glossy
//! reflections.
//!
//! Directions are in a canonical Y-up frame; the shader maps world space into it
//! (Z-up models) and applies the user rotation around the up axis.

use anyhow::{Context, Result};
use glam::Vec3;
use serde::{Deserialize, Serialize};
use std::f32::consts::PI;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum EnvPreset {
    /// Simple sky/ground ambient (no environment map).
    #[default]
    Basic,
    Studio,
    Sky,
    Sunset,
    Overcast,
    /// The user's own `.hdr` file.
    Custom,
}

impl EnvPreset {
    pub const BUILT_IN: [EnvPreset; 4] = [EnvPreset::Studio, EnvPreset::Sky, EnvPreset::Sunset, EnvPreset::Overcast];

    pub fn label(self) -> &'static str {
        use crate::i18n::tr;
        match self {
            EnvPreset::Basic => tr("Простой свет", "Basic light"),
            EnvPreset::Studio => tr("Студия", "Studio"),
            EnvPreset::Sky => tr("Небо", "Sky"),
            EnvPreset::Sunset => tr("Закат", "Sunset"),
            EnvPreset::Overcast => tr("Пасмурно", "Overcast"),
            EnvPreset::Custom => tr("Свой HDRI", "Custom HDRI"),
        }
    }
}

/// Linear HDR equirectangular image, row 0 at the zenith.
#[derive(Clone)]
pub struct EnvMap {
    pub width: usize,
    pub height: usize,
    pub pixels: Vec<[f32; 3]>,
}

/// Unit direction (canonical Y-up) for an equirect texel centre.
fn texel_dir(x: usize, y: usize, w: usize, h: usize) -> Vec3 {
    let u = (x as f32 + 0.5) / w as f32;
    let v = (y as f32 + 0.5) / h as f32;
    let phi = (u - 0.5) * 2.0 * PI;
    let theta = v * PI;
    Vec3::new(theta.sin() * phi.sin(), theta.cos(), -theta.sin() * phi.cos())
}

fn smooth(e0: f32, e1: f32, x: f32) -> f32 {
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

fn mix(a: Vec3, b: Vec3, t: f32) -> Vec3 {
    a + (b - a) * t
}

fn sun(d: Vec3, dir: Vec3, color: Vec3, size_deg: f32, power: f32) -> Vec3 {
    let c = d.dot(dir).clamp(-1.0, 1.0).acos().to_degrees();
    let disc = 1.0 - smooth(size_deg * 0.6, size_deg, c);
    let glow = (-c / (size_deg * 6.0)).exp() * 0.08;
    color * (disc * power + glow * power * 0.15)
}

fn studio(d: Vec3) -> Vec3 {
    let base = mix(Vec3::splat(0.035), Vec3::splat(0.09), smooth(-0.3, 0.8, d.y));
    // Soft boxes: a big one overhead, a key strip front-left and a rim strip behind.
    let softbox = |center: Vec3, half_w: f32, half_h: f32, up: Vec3, power: f32| {
        let c = center.normalize();
        let right = up.cross(c).normalize();
        let top = c.cross(right);
        let along = d.dot(c);
        if along <= 0.0 {
            return Vec3::ZERO;
        }
        let p = d / along - c;
        let x = p.dot(right).abs();
        let y = p.dot(top).abs();
        let edge = (1.0 - smooth(half_w * 0.8, half_w, x)) * (1.0 - smooth(half_h * 0.8, half_h, y));
        Vec3::splat(power * edge)
    };
    base + softbox(Vec3::Y, 0.55, 0.55, Vec3::Z, 7.0)
        + softbox(Vec3::new(-0.8, 0.45, 0.6), 0.18, 0.55, Vec3::Y, 9.0)
        + softbox(Vec3::new(0.7, 0.35, -0.7), 0.12, 0.6, Vec3::Y, 5.0)
        + softbox(Vec3::new(0.9, 0.1, 0.5), 0.35, 0.2, Vec3::Y, 1.2)
}

fn sky(d: Vec3) -> Vec3 {
    let zenith = Vec3::new(0.18, 0.36, 0.85) * 1.1;
    let horizon = Vec3::new(0.75, 0.85, 1.0) * 1.6;
    let ground = Vec3::new(0.30, 0.27, 0.22) * 0.55;
    let up = d.y;
    let c = if up >= 0.0 { mix(horizon, zenith, up.powf(0.45)) } else { mix(horizon * 0.6, ground, smooth(0.0, 0.15, -up)) };
    let sun_dir = Vec3::new(0.45, 0.62, 0.64).normalize();
    c + if up > -0.02 { sun(d, sun_dir, Vec3::new(1.0, 0.95, 0.85), 1.6, 300.0) } else { Vec3::ZERO }
}

fn sunset(d: Vec3) -> Vec3 {
    let zenith = Vec3::new(0.10, 0.12, 0.35);
    let mid = Vec3::new(0.65, 0.35, 0.45);
    let horizon = Vec3::new(1.6, 0.7, 0.3);
    let ground = Vec3::new(0.12, 0.08, 0.07);
    let up = d.y;
    let sun_dir = Vec3::new(-0.2, 0.06, -0.98).normalize();
    // Warmer towards the sun.
    let towards = d.dot(Vec3::new(sun_dir.x, 0.0, sun_dir.z).normalize()).max(0.0);
    let c = if up >= 0.0 {
        let h = mix(horizon * (0.6 + 0.6 * towards), mid, smooth(0.0, 0.25, up));
        mix(h, zenith, smooth(0.2, 0.9, up))
    } else {
        mix(horizon * 0.35, ground, smooth(0.0, 0.1, -up))
    };
    c + sun(d, sun_dir, Vec3::new(1.0, 0.55, 0.25), 2.2, 120.0)
}

fn overcast(d: Vec3) -> Vec3 {
    let up = d.y;
    if up >= 0.0 {
        mix(Vec3::splat(0.75), Vec3::splat(1.15), up.powf(0.6))
    } else {
        mix(Vec3::splat(0.5), Vec3::splat(0.22), smooth(0.0, 0.3, -up))
    }
}

impl EnvMap {
    pub fn preset(p: EnvPreset) -> Option<EnvMap> {
        let f: fn(Vec3) -> Vec3 = match p {
            EnvPreset::Studio => studio,
            EnvPreset::Sky => sky,
            EnvPreset::Sunset => sunset,
            EnvPreset::Overcast => overcast,
            EnvPreset::Basic | EnvPreset::Custom => return None,
        };
        let (w, h) = (512, 256);
        let mut pixels = Vec::with_capacity(w * h);
        for y in 0..h {
            for x in 0..w {
                // 2×2 supersampling keeps the sun and box edges clean.
                let mut acc = Vec3::ZERO;
                for (ox, oy) in [(0.25, 0.25), (0.75, 0.25), (0.25, 0.75), (0.75, 0.75)] {
                    let u = (x as f32 + ox) / w as f32;
                    let v = (y as f32 + oy) / h as f32;
                    let phi = (u - 0.5) * 2.0 * PI;
                    let theta = v * PI;
                    acc += f(Vec3::new(theta.sin() * phi.sin(), theta.cos(), -theta.sin() * phi.cos()));
                }
                pixels.push((acc * 0.25).to_array());
            }
        }
        Some(EnvMap { width: w, height: h, pixels })
    }

    /// Loads a Radiance `.hdr` equirectangular image (downscaled to at most 2048 wide).
    pub fn load_hdr(path: &std::path::Path) -> Result<EnvMap> {
        let img = image::open(path).with_context(|| format!("cannot read {}", path.display()))?;
        let mut img = img.into_rgb32f();
        if img.width() > 2048 {
            let h = (2048 * img.height() / img.width()).max(1);
            img = image::imageops::resize(&img, 2048, h, image::imageops::FilterType::Triangle);
        }
        let (w, h) = (img.width() as usize, img.height() as usize);
        anyhow::ensure!(w >= 4 && h >= 2, "image is too small");
        let pixels = img.pixels().map(|p| [p.0[0].max(0.0), p.0[1].max(0.0), p.0[2].max(0.0)]).collect();
        Ok(EnvMap { width: w, height: h, pixels })
    }

    /// Irradiance as 9 spherical-harmonics coefficients (already convolved with the
    /// cosine lobe, so E(n) = Σ c_i · Y_i(n)).
    pub fn irradiance_sh(&self) -> [Vec3; 9] {
        let (w, h) = (self.width, self.height);
        // A coarse grid is plenty for 9 coefficients.
        let step = (w / 256).max(1);
        let mut c = [Vec3::ZERO; 9];
        let mut total = 0.0;
        for y in (0..h).step_by(step) {
            let theta = (y as f32 + 0.5) / h as f32 * PI;
            let weight = theta.sin();
            for x in (0..w).step_by(step) {
                let d = texel_dir(x, y, w, h);
                let l = Vec3::from(self.pixels[y * w + x]);
                for (ci, yi) in c.iter_mut().zip(sh_basis(d)) {
                    *ci += l * (yi * weight);
                }
                total += weight;
            }
        }
        let norm = 4.0 * PI / total;
        // Band factors of the clamped cosine: π, 2π/3, π/4.
        let band = [PI, 2.0 * PI / 3.0, 2.0 * PI / 3.0, 2.0 * PI / 3.0, PI / 4.0, PI / 4.0, PI / 4.0, PI / 4.0, PI / 4.0];
        let mut out = [Vec3::ZERO; 9];
        for i in 0..9 {
            out[i] = c[i] * norm * band[i];
        }
        out
    }

    /// RGBA16F mip chain (box filtered), level 0 first, ready for upload.
    pub fn mip_chain_f16(&self) -> (Vec<u8>, u32) {
        let mut levels: Vec<(usize, usize, Vec<[f32; 3]>)> = vec![(self.width, self.height, self.pixels.clone())];
        loop {
            let (w, h, src) = levels.last().unwrap();
            if *w <= 1 && *h <= 1 {
                break;
            }
            let (nw, nh) = ((*w / 2).max(1), (*h / 2).max(1));
            let mut dst = Vec::with_capacity(nw * nh);
            for y in 0..nh {
                for x in 0..nw {
                    let mut acc = [0.0f32; 3];
                    let mut n = 0.0;
                    for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                        let sx = (x * 2 + dx).min(w - 1);
                        let sy = (y * 2 + dy).min(h - 1);
                        let p = src[sy * w + sx];
                        acc[0] += p[0];
                        acc[1] += p[1];
                        acc[2] += p[2];
                        n += 1.0;
                    }
                    dst.push([acc[0] / n, acc[1] / n, acc[2] / n]);
                }
            }
            levels.push((nw, nh, dst));
        }
        let mut bytes = Vec::new();
        for (_, _, px) in &levels {
            for p in px {
                for v in [p[0], p[1], p[2], 1.0] {
                    bytes.extend_from_slice(&half::f16::from_f32(v.min(60000.0)).to_le_bytes());
                }
            }
        }
        (bytes, levels.len() as u32)
    }

    /// Small tonemapped RGBA preview for the UI.
    pub fn preview(&self, w: usize, h: usize) -> Vec<u8> {
        let mut out = Vec::with_capacity(w * h * 4);
        for y in 0..h {
            for x in 0..w {
                let sx = x * self.width / w;
                let sy = y * self.height / h;
                let p = Vec3::from(self.pixels[sy * self.width + sx]);
                let t = p / (p + Vec3::ONE); // Reinhard
                for c in t.to_array() {
                    out.push((c.powf(1.0 / 2.2) * 255.0).round().clamp(0.0, 255.0) as u8);
                }
                out.push(255);
            }
        }
        out
    }
}

/// Real spherical-harmonics basis up to band 2.
fn sh_basis(d: Vec3) -> [f32; 9] {
    let (x, y, z) = (d.x, d.y, d.z);
    [
        0.282095,
        0.488603 * y,
        0.488603 * z,
        0.488603 * x,
        1.092548 * x * y,
        1.092548 * y * z,
        0.315392 * (3.0 * z * z - 1.0),
        1.092548 * x * z,
        0.546274 * (x * x - y * y),
    ]
}

/// Evaluates SH irradiance (used by tests; the shader has its own copy).
#[cfg(test)]
fn eval_sh(c: &[Vec3; 9], n: Vec3) -> Vec3 {
    c.iter().zip(sh_basis(n)).map(|(c, y)| *c * y).sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uniform_white_environment_gives_pi_irradiance() {
        let env = EnvMap { width: 64, height: 32, pixels: vec![[1.0; 3]; 64 * 32] };
        let sh = env.irradiance_sh();
        for n in [Vec3::X, Vec3::Y, -Vec3::Z, Vec3::new(1.0, 1.0, 1.0).normalize()] {
            let e = eval_sh(&sh, n);
            assert!((e.x - PI).abs() < 0.05, "{n}: {e}");
        }
    }

    #[test]
    fn sky_is_brighter_from_above() {
        let env = EnvMap::preset(EnvPreset::Overcast).unwrap();
        let sh = env.irradiance_sh();
        assert!(eval_sh(&sh, Vec3::Y).x > eval_sh(&sh, -Vec3::Y).x * 1.5);
    }

    #[test]
    fn mip_chain_ends_at_one_texel() {
        let env = EnvMap { width: 8, height: 4, pixels: vec![[0.5; 3]; 32] };
        let (bytes, levels) = env.mip_chain_f16();
        assert_eq!(levels, 4); // 8x4, 4x2, 2x1, 1x1
        assert_eq!(bytes.len(), (32 + 8 + 2 + 1) * 8);
    }

    #[test]
    fn texel_directions_cover_the_sphere() {
        assert!(texel_dir(0, 0, 64, 32).y > 0.99);
        assert!(texel_dir(0, 31, 64, 32).y < -0.99);
        // u = 0.5 looks down -Z.
        assert!(texel_dir(32, 16, 64, 32).z < -0.95);
    }

    #[test]
    fn loads_a_radiance_hdr_file() {
        let (w, h) = (64u32, 32u32);
        let px: Vec<image::Rgb<f32>> = (0..w * h).map(|i| image::Rgb([i as f32 / 100.0, 0.5, 2.0])).collect();
        let path = std::env::temp_dir().join(format!("viewer3d-test-{}.hdr", std::process::id()));
        {
            let f = std::io::BufWriter::new(std::fs::File::create(&path).unwrap());
            image::codecs::hdr::HdrEncoder::new(f).encode(&px, w as usize, h as usize).unwrap();
        }
        let env = EnvMap::load_hdr(&path).unwrap();
        let _ = std::fs::remove_file(&path);
        assert_eq!((env.width, env.height), (64, 32));
        assert!((env.pixels[0][2] - 2.0).abs() < 0.05);
        assert!(EnvMap::load_hdr(std::path::Path::new("no-such-file.hdr")).is_err());
    }

    #[test]
    fn all_presets_build() {
        for p in EnvPreset::BUILT_IN {
            let e = EnvMap::preset(p).unwrap();
            assert!(e.pixels.iter().all(|p| p.iter().all(|v| v.is_finite() && *v >= 0.0)));
        }
    }
}
