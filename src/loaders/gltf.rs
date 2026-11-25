//! glTF 2.0 / GLB loader. Node transforms are baked into vertices.

use super::image::{ImageSource, decode_parallel};
use crate::scene::{AlphaMode, Material, Mesh, Scene, Topology};
use anyhow::{Result, anyhow, bail};
use base64::Engine;
use glam::Mat4;
use std::path::Path;

pub fn load_gltf(path: &Path) -> Result<Scene> {
    let data = super::map_file(path)?;
    load_gltf_slice(&data, path.parent())
}

pub fn load_gltf_slice(data: &[u8], dir: Option<&Path>) -> Result<Scene> {
    let mut scene = Scene::default();
    let gltf = match gltf::Gltf::from_slice(data) {
        Ok(g) => g,
        Err(gltf::Error::Validation(errs)) => {
            scene.info.warnings.push(format!("file failed validation ({} issues), loaded anyway", errs.len()));
            gltf::Gltf::from_slice_without_validation(data).map_err(|e| anyhow!("glTF: {e}"))?
        }
        Err(e) => bail!("glTF: {e}"),
    };
    let gltf::Gltf { document: doc, mut blob } = gltf;
    scene.info.format = if data.starts_with(b"glTF") { "glTF binary (GLB)".into() } else { "glTF (JSON)".into() };

    for ext in doc.extensions_required() {
        if matches!(ext, "KHR_draco_mesh_compression" | "EXT_meshopt_compression" | "KHR_mesh_quantization" | "KHR_texture_basisu") {
            scene.info.warnings.push(format!("required extension {ext} is not supported; geometry/textures may be missing"));
        }
    }

    // Buffers.
    let mut buffers: Vec<Vec<u8>> = Vec::new();
    for b in doc.buffers() {
        let bytes = match b.source() {
            gltf::buffer::Source::Bin => blob.take().unwrap_or_default(),
            gltf::buffer::Source::Uri(uri) => match load_uri(uri, dir) {
                Ok(v) => v,
                Err(e) => {
                    scene.info.warnings.push(format!("buffer {}: {e}", b.index()));
                    Vec::new()
                }
            },
        };
        if bytes.len() < b.length() {
            scene.info.warnings.push(format!("buffer {} is shorter than declared", b.index()));
        }
        buffers.push(bytes);
    }

    // Images (decoded in parallel).
    let mut jobs = Vec::new();
    for img in doc.images() {
        let mut name = img.name().map(String::from).unwrap_or_else(|| format!("image {}", img.index()));
        let src = match img.source() {
            gltf::image::Source::View { view, .. } => {
                let buf = &buffers[view.buffer().index()];
                let bytes = buf.get(view.offset()..view.offset() + view.length()).unwrap_or(&[]).to_vec();
                ImageSource::Bytes(bytes)
            }
            gltf::image::Source::Uri { uri, .. } => {
                if uri.starts_with("data:") {
                    ImageSource::Bytes(load_uri(uri, dir).unwrap_or_default())
                } else {
                    if img.name().is_none() {
                        name = percent_decode(uri);
                    }
                    ImageSource::File(dir.map(|d| d.join(percent_decode(uri))).unwrap_or_else(|| percent_decode(uri).into()))
                }
            }
        };
        jobs.push((name, src));
    }
    let mut image_map: Vec<Option<usize>> = Vec::new();
    for (i, r) in decode_parallel(jobs).into_iter().enumerate() {
        match r {
            Ok(t) => {
                image_map.push(Some(scene.textures.len()));
                scene.textures.push(t);
            }
            Err(e) => {
                scene.info.warnings.push(format!("image {i}: {e}"));
                image_map.push(None);
            }
        }
    }
    let tex_of = |t: gltf::Texture| image_map.get(t.source().index()).copied().flatten();

    // Materials.
    let mut uv_warned = false;
    for m in doc.materials() {
        let pbr = m.pbr_metallic_roughness();
        let strength = m.emissive_strength().unwrap_or(1.0);
        let e = m.emissive_factor();
        let mut check_uv = |set: u32| {
            if set != 0 && !uv_warned {
                uv_warned = true;
                scene.info.warnings.push("textures using TEXCOORD_1+ are shown with TEXCOORD_0".into());
            }
        };
        if let Some(t) = pbr.base_color_texture() {
            check_uv(t.tex_coord());
        }
        scene.materials.push(Material {
            name: m.name().map(String::from).unwrap_or_else(|| format!("material {}", m.index().unwrap_or(0))),
            base_color: pbr.base_color_factor(),
            metallic: pbr.metallic_factor(),
            roughness: pbr.roughness_factor(),
            emissive: [e[0] * strength, e[1] * strength, e[2] * strength],
            base_color_tex: pbr.base_color_texture().and_then(|t| tex_of(t.texture())),
            mr_tex: pbr.metallic_roughness_texture().and_then(|t| tex_of(t.texture())),
            normal_tex: m.normal_texture().and_then(|t| tex_of(t.texture())),
            occlusion_tex: m.occlusion_texture().and_then(|t| tex_of(t.texture())),
            emissive_tex: m.emissive_texture().and_then(|t| tex_of(t.texture())),
            double_sided: m.double_sided(),
            alpha_mode: match m.alpha_mode() {
                gltf::material::AlphaMode::Opaque => AlphaMode::Opaque,
                gltf::material::AlphaMode::Mask => AlphaMode::Mask,
                gltf::material::AlphaMode::Blend => AlphaMode::Blend,
            },
            alpha_cutoff: m.alpha_cutoff().unwrap_or(0.5),
        });
    }

    // Geometry.
    let mut ctx = Ctx { buffers: &buffers, scene: &mut scene, morph: 0 };
    match doc.default_scene().or_else(|| doc.scenes().next()) {
        Some(sc) => {
            for node in sc.nodes() {
                ctx.visit(&node, Mat4::IDENTITY, 0);
            }
        }
        None => {
            for mesh in doc.meshes() {
                ctx.add_mesh(&mesh, Mat4::IDENTITY, None);
            }
        }
    }
    let morph = ctx.morph;

    // Info.
    let info = &mut scene.info;
    info.morph_targets = morph;
    info.nodes = doc.nodes().count();
    info.skins = doc.skins().count();
    info.bones = doc.skins().map(|s| s.joints().count()).sum();
    info.cameras = doc.cameras().count();
    info.lights = doc.lights().map(|l| l.count()).unwrap_or(0);
    for a in doc.animations() {
        let dur = a
            .samplers()
            .filter_map(|s| s.input().max())
            .filter_map(|v| v.as_array().and_then(|a| a.first()).and_then(|x| x.as_f64()))
            .fold(0.0f64, f64::max);
        let name = a.name().map(String::from).unwrap_or_else(|| format!("animation {}", a.index()));
        info.animations.push(format!("{name} ({dur:.2} s)"));
    }
    let asset = &doc.as_json().asset;
    if let Some(g) = &asset.generator {
        info.extra.push(("generator".into(), g.clone()));
    }
    info.extra.push(("glTF version".into(), asset.version.clone()));
    let used: Vec<&str> = doc.extensions_used().collect();
    if !used.is_empty() {
        info.extra.push(("extensions".into(), used.join(", ")));
    }
    if scene.meshes.is_empty() {
        bail!("glTF contains no drawable geometry");
    }
    Ok(scene)
}

struct Ctx<'a> {
    buffers: &'a [Vec<u8>],
    scene: &'a mut Scene,
    morph: usize,
}

impl Ctx<'_> {
    fn visit(&mut self, node: &gltf::Node, parent: Mat4, depth: u32) {
        if depth > 256 {
            return;
        }
        let m = parent * Mat4::from_cols_array_2d(&node.transform().matrix());
        if let Some(mesh) = node.mesh() {
            self.add_mesh(&mesh, m, node.name());
        }
        for c in node.children() {
            self.visit(&c, m, depth + 1);
        }
    }

    fn add_mesh(&mut self, mesh: &gltf::Mesh, m: Mat4, node_name: Option<&str>) {
        let base_name = mesh.name().or(node_name).map(String::from).unwrap_or_else(|| format!("mesh {}", mesh.index()));
        let nprims = mesh.primitives().len();
        for (pi, prim) in mesh.primitives().enumerate() {
            let name = if nprims > 1 { format!("{base_name} [{pi}]") } else { base_name.clone() };
            let buffers = self.buffers;
            let reader = prim.reader(|b| buffers.get(b.index()).map(|v| v.as_slice()).filter(|s| !s.is_empty()));
            let Some(pos) = reader.read_positions() else {
                self.scene.info.warnings.push(format!("{name}: no POSITION data (compressed?)"));
                continue;
            };
            use gltf::mesh::Mode;
            let topology = match prim.mode() {
                Mode::Points => Topology::Points,
                Mode::Lines | Mode::LineLoop | Mode::LineStrip => Topology::Lines,
                _ => Topology::Triangles,
            };
            let mut out = Mesh::new(name, topology);
            out.positions = pos.collect();
            let n = out.positions.len();
            if let Some(nr) = reader.read_normals() {
                out.normals = nr.collect();
                out.had_normals = out.normals.len() == n;
            }
            if let Some(t) = reader.read_tex_coords(0) {
                out.uvs = t.into_f32().collect();
            }
            if let Some(c) = reader.read_colors(0) {
                out.colors = c.into_rgba_u8().collect();
            }
            let idx: Vec<u32> = match reader.read_indices() {
                Some(i) => i.into_u32().collect(),
                None => (0..n as u32).collect(),
            };
            out.indices = match prim.mode() {
                Mode::Triangles | Mode::Lines => idx,
                Mode::TriangleStrip => (2..idx.len())
                    .flat_map(|i| if i % 2 == 0 { [idx[i - 2], idx[i - 1], idx[i]] } else { [idx[i - 1], idx[i - 2], idx[i]] })
                    .collect(),
                Mode::TriangleFan => (2..idx.len()).flat_map(|i| [idx[0], idx[i - 1], idx[i]]).collect(),
                Mode::LineStrip => idx.windows(2).flatten().copied().collect(),
                Mode::LineLoop => {
                    let mut v: Vec<u32> = idx.windows(2).flatten().copied().collect();
                    if idx.len() > 2 {
                        v.extend_from_slice(&[idx[idx.len() - 1], idx[0]]);
                    }
                    v
                }
                Mode::Points => Vec::new(),
            };
            out.material = prim.material().index();
            self.morph += prim.morph_targets().len();
            out.transform(&m);
            self.scene.meshes.push(out);
        }
    }
}

fn load_uri(uri: &str, dir: Option<&Path>) -> Result<Vec<u8>> {
    if let Some(rest) = uri.strip_prefix("data:") {
        let (_, payload) = rest.split_once(";base64,").ok_or_else(|| anyhow!("unsupported data URI"))?;
        return base64::engine::general_purpose::STANDARD.decode(payload).map_err(|e| anyhow!("bad base64: {e}"));
    }
    let rel = percent_decode(uri);
    let p = dir.map(|d| d.join(&rel)).unwrap_or_else(|| rel.into());
    std::fs::read(&p).map_err(|e| anyhow!("{}: {e}", p.display()))
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Some(v) = s.get(i + 1..i + 3).and_then(|h| u8::from_str_radix(h, 16).ok()) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Minimal GLB: one triangle, positions only.
    fn tiny_glb() -> Vec<u8> {
        let mut bin = Vec::new();
        for v in [0.0f32, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0] {
            bin.extend_from_slice(&v.to_le_bytes());
        }
        let json = r#"{"asset":{"version":"2.0","generator":"test"},"scene":0,"scenes":[{"nodes":[0]}],
"nodes":[{"mesh":0,"translation":[0,0,5]}],"meshes":[{"name":"tri","primitives":[{"attributes":{"POSITION":0}}]}],
"accessors":[{"bufferView":0,"componentType":5126,"count":3,"type":"VEC3","min":[0,0,0],"max":[1,1,0]}],
"bufferViews":[{"buffer":0,"byteLength":36}],"buffers":[{"byteLength":36}]}"#;
        let mut j = json.as_bytes().to_vec();
        while j.len() % 4 != 0 {
            j.push(b' ');
        }
        let total = 12 + 8 + j.len() + 8 + bin.len();
        let mut d = Vec::new();
        d.extend_from_slice(b"glTF");
        d.extend_from_slice(&2u32.to_le_bytes());
        d.extend_from_slice(&(total as u32).to_le_bytes());
        d.extend_from_slice(&(j.len() as u32).to_le_bytes());
        d.extend_from_slice(b"JSON");
        d.extend_from_slice(&j);
        d.extend_from_slice(&(bin.len() as u32).to_le_bytes());
        d.extend_from_slice(b"BIN\0");
        d.extend_from_slice(&bin);
        d
    }

    #[test]
    fn glb_triangle_with_node_transform() {
        let mut s = load_gltf_slice(&tiny_glb(), None).unwrap();
        s.finalize();
        assert_eq!(s.meshes.len(), 1);
        assert_eq!(s.meshes[0].name, "tri");
        assert_eq!(s.meshes[0].positions[1], [1.0, 0.0, 5.0]);
        assert_eq!(s.meshes[0].indices, vec![0, 1, 2]);
        assert_eq!(s.meshes[0].normals.len(), 3);
        assert!(!s.meshes[0].had_normals);
        assert_eq!(s.info.format, "glTF binary (GLB)");
        assert!(s.info.extra.iter().any(|(k, v)| k == "generator" && v == "test"));
    }

    #[test]
    fn percent_decoding() {
        assert_eq!(percent_decode("my%20tex.png"), "my tex.png");
        assert_eq!(percent_decode("a%2"), "a%2");
    }

    #[test]
    fn garbage_is_error() {
        assert!(load_gltf_slice(b"nope", None).is_err());
    }
}
