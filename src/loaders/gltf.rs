//! glTF 2.0 / GLB loader.
//!
//! Static scenes are baked into world space. Scenes with animations, skins or morph
//! targets keep node-space meshes plus [`AnimData`] so they can be played back.
//! Supports quantized / normalized / sparse accessors (KHR_mesh_quantization),
//! EXT_meshopt_compression, KHR_draco_mesh_compression and KHR_texture_transform.

use super::image::{ImageSource, decode_parallel};
use crate::anim::{AnimData, Clip, Interp, Node, Skin, Track, TrackValues, Trs};
use crate::scene::{AlphaMode, Material, Mesh, MorphTarget, Scene, Topology};
use anyhow::{Result, anyhow, bail};
use base64::Engine;
use glam::{Mat4, Quat, Vec3};
use gltf::accessor::DataType;
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
        if matches!(ext, "KHR_texture_basisu" | "EXT_texture_avif") {
            scene.info.warnings.push(format!("required extension {ext} is not supported; textures may be missing"));
        }
    }

    // Buffers.
    let mut buffers: Vec<Vec<u8>> = Vec::new();
    for b in doc.buffers() {
        let mut bytes = match b.source() {
            gltf::buffer::Source::Bin => blob.take().unwrap_or_default(),
            gltf::buffer::Source::Uri(uri) => match load_uri(uri, dir) {
                Ok(v) => v,
                Err(e) => {
                    scene.info.warnings.push(format!("buffer {}: {e}", b.index()));
                    Vec::new()
                }
            },
        };
        let is_fallback = b.extension_value("EXT_meshopt_compression").is_some();
        if bytes.len() < b.length() {
            if !is_fallback {
                scene.info.warnings.push(format!("buffer {} is shorter than declared", b.index()));
            }
            bytes.resize(b.length(), 0);
        }
        buffers.push(bytes);
    }
    decode_meshopt(&doc, &mut buffers, &mut scene.info.warnings);

    // Images (decoded in parallel).
    let mut jobs = Vec::new();
    for img in doc.images() {
        let mut name = img.name().map(String::from).unwrap_or_else(|| format!("image {}", img.index()));
        let src = match img.source() {
            gltf::image::Source::View { view, .. } => {
                let buf = &buffers[view.buffer().index()];
                ImageSource::Bytes(buf.get(view.offset()..view.offset() + view.length()).unwrap_or(&[]).to_vec())
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

    // Materials (+ per-material UV transform taken from the base colour texture).
    let mut uv_xform: Vec<Option<[f32; 6]>> = Vec::new();
    let mut uv_warned = false;
    for m in doc.materials() {
        let pbr = m.pbr_metallic_roughness();
        let strength = m.emissive_strength().unwrap_or(1.0);
        let e = m.emissive_factor();
        if let Some(t) = pbr.base_color_texture() {
            if t.tex_coord() != 0 && !uv_warned {
                uv_warned = true;
                scene.info.warnings.push("textures using TEXCOORD_1+ are shown with TEXCOORD_0".into());
            }
            uv_xform.push(t.texture_transform().map(|x| {
                let (o, r, s) = (x.offset(), x.rotation(), x.scale());
                let (sn, cs) = r.sin_cos();
                // uv' = T * R * S * uv  (KHR_texture_transform)
                [cs * s[0], sn * s[0], -sn * s[1], cs * s[1], o[0], o[1]]
            }));
        } else {
            uv_xform.push(None);
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

    let animated =
        doc.animations().len() > 0 || doc.skins().len() > 0 || doc.meshes().any(|m| m.primitives().any(|p| p.morph_targets().len() > 0));
    let views: Vec<(usize, usize, usize)> = doc.views().map(|v| (v.buffer().index(), v.offset(), v.length())).collect();
    let mut ctx = Ctx { buffers: &buffers, views: &views, scene: &mut scene, uv_xform: &uv_xform, animated, morph: 0 };
    let roots: Vec<gltf::Node> = match doc.default_scene().or_else(|| doc.scenes().next()) {
        Some(sc) => sc.nodes().collect(),
        None => Vec::new(),
    };
    if roots.is_empty() && doc.scenes().len() == 0 {
        for mesh in doc.meshes() {
            ctx.add_mesh(&mesh, Mat4::IDENTITY, None, None);
        }
    } else {
        for node in &roots {
            ctx.visit(node, Mat4::IDENTITY, 0);
        }
    }
    let morph = ctx.morph;

    if animated {
        scene.anim = Some(build_anim(&doc, &buffers));
    }

    // Info.
    let info = &mut scene.info;
    info.morph_targets = morph;
    info.nodes = doc.nodes().count();
    info.skins = doc.skins().count();
    info.bones = doc.skins().map(|s| s.joints().count()).sum();
    info.cameras = doc.cameras().count();
    info.lights = doc.lights().map(|l| l.count()).unwrap_or(0);
    if let Some(a) = &scene.anim {
        for c in &a.clips {
            info.animations.push(format!("{} ({:.2} s)", c.name, c.duration));
        }
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

// ---------------------------------------------------------------------------
// Accessors

fn comp_size(dt: DataType) -> usize {
    match dt {
        DataType::I8 | DataType::U8 => 1,
        DataType::I16 | DataType::U16 => 2,
        DataType::U32 | DataType::F32 => 4,
    }
}

fn read_comp(b: &[u8], dt: DataType, norm: bool) -> f32 {
    match dt {
        DataType::I8 => {
            let v = b[0] as i8 as f32;
            if norm { (v / 127.0).max(-1.0) } else { v }
        }
        DataType::U8 => {
            let v = b[0] as f32;
            if norm { v / 255.0 } else { v }
        }
        DataType::I16 => {
            let v = i16::from_le_bytes([b[0], b[1]]) as f32;
            if norm { (v / 32767.0).max(-1.0) } else { v }
        }
        DataType::U16 => {
            let v = u16::from_le_bytes([b[0], b[1]]) as f32;
            if norm { v / 65535.0 } else { v }
        }
        DataType::U32 => u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as f32,
        DataType::F32 => f32::from_le_bytes([b[0], b[1], b[2], b[3]]),
    }
}

/// Read any accessor as a flat `count × components` f32 array (handles stride,
/// normalized integers, sparse substitution). Out-of-range data reads as zero.
pub(crate) fn read_f32(acc: &gltf::Accessor, bufs: &[Vec<u8>]) -> Vec<f32> {
    let comps = acc.dimensions().multiplicity();
    let count = acc.count();
    let dt = acc.data_type();
    let cs = comp_size(dt);
    let norm = acc.normalized();
    let mut out = vec![0f32; count.saturating_mul(comps).min(1 << 31)];
    if let Some(view) = acc.view() {
        if let Some(buf) = bufs.get(view.buffer().index()) {
            let stride = view.stride().unwrap_or(cs * comps);
            let base = view.offset() + acc.offset();
            if dt == DataType::F32 && stride == 4 * comps && base + count * stride <= buf.len() {
                for (o, ch) in out.iter_mut().zip(buf[base..base + count * stride].chunks_exact(4)) {
                    *o = f32::from_le_bytes([ch[0], ch[1], ch[2], ch[3]]);
                }
            } else {
                for i in 0..count {
                    for c in 0..comps {
                        let o = base + i * stride + c * cs;
                        if o + cs <= buf.len() {
                            out[i * comps + c] = read_comp(&buf[o..], dt, norm);
                        }
                    }
                }
            }
        }
    }
    if let Some(sp) = acc.sparse() {
        let iv = sp.indices();
        let vv = sp.values();
        let it = match iv.index_type() {
            gltf::accessor::sparse::IndexType::U8 => DataType::U8,
            gltf::accessor::sparse::IndexType::U16 => DataType::U16,
            gltf::accessor::sparse::IndexType::U32 => DataType::U32,
        };
        let (ib, vb) = (bufs.get(iv.view().buffer().index()), bufs.get(vv.view().buffer().index()));
        if let (Some(ib), Some(vb)) = (ib, vb) {
            let ibase = iv.view().offset() + iv.offset();
            let vbase = vv.view().offset() + vv.offset();
            for k in 0..sp.count() {
                let io = ibase + k * comp_size(it);
                if io + comp_size(it) > ib.len() {
                    break;
                }
                let idx = read_comp(&ib[io..], it, false) as usize;
                for c in 0..comps {
                    let vo = vbase + (k * comps + c) * cs;
                    if idx < count && vo + cs <= vb.len() {
                        out[idx * comps + c] = read_comp(&vb[vo..], dt, norm);
                    }
                }
            }
        }
    }
    out
}

fn read_u32(acc: &gltf::Accessor, bufs: &[Vec<u8>]) -> Vec<u32> {
    // Indices are never normalized; u32 values above 2^24 would lose precision via f32.
    let dt = acc.data_type();
    if let (Some(view), None) = (acc.view(), acc.sparse()) {
        if let Some(buf) = bufs.get(view.buffer().index()) {
            let cs = comp_size(dt);
            let stride = view.stride().unwrap_or(cs);
            let base = view.offset() + acc.offset();
            return (0..acc.count())
                .map(|i| {
                    let o = base + i * stride;
                    match (dt, buf.get(o..o + cs)) {
                        (DataType::U8, Some(b)) => b[0] as u32,
                        (DataType::U16, Some(b)) => u16::from_le_bytes([b[0], b[1]]) as u32,
                        (DataType::U32, Some(b)) => u32::from_le_bytes([b[0], b[1], b[2], b[3]]),
                        _ => 0,
                    }
                })
                .collect();
        }
    }
    read_f32(acc, bufs).into_iter().map(|v| v as u32).collect()
}

fn chunk3(v: Vec<f32>) -> Vec<[f32; 3]> {
    v.chunks_exact(3).map(|c| [c[0], c[1], c[2]]).collect()
}

/// Decode EXT_meshopt_compression buffer views in place into their fallback buffers.
fn decode_meshopt(doc: &gltf::Document, buffers: &mut [Vec<u8>], warnings: &mut Vec<String>) {
    for view in doc.views() {
        let Some(ext) = view.extension_value("EXT_meshopt_compression") else { continue };
        let num = |k: &str| ext.get(k).and_then(|v| v.as_u64()).unwrap_or(0) as usize;
        let text = |k: &str, d: &'static str| ext.get(k).and_then(|v| v.as_str()).unwrap_or(d).to_string();
        let (sb, off, len, stride, count) = (num("buffer"), num("byteOffset"), num("byteLength"), num("byteStride"), num("count"));
        let (mode, filter) = (text("mode", "ATTRIBUTES"), text("filter", "NONE"));
        let Some(src) = buffers.get(sb).and_then(|b| b.get(off..off + len)).map(|s| s.to_vec()) else {
            warnings.push(format!("meshopt view {}: source out of range", view.index()));
            continue;
        };
        if stride == 0 || count == 0 {
            continue;
        }
        let mut dst = vec![0u8; count * stride];
        // SAFETY: dst holds count*stride bytes; src slice length is passed explicitly.
        let rc = unsafe {
            use meshopt::ffi::*;
            match mode.as_str() {
                "ATTRIBUTES" => meshopt_decodeVertexBuffer(dst.as_mut_ptr().cast(), count, stride, src.as_ptr(), src.len()),
                "TRIANGLES" => meshopt_decodeIndexBuffer(dst.as_mut_ptr().cast(), count, stride, src.as_ptr(), src.len()),
                "INDICES" => meshopt_decodeIndexSequence(dst.as_mut_ptr().cast(), count, stride, src.as_ptr(), src.len()),
                _ => -1,
            }
        };
        if rc != 0 {
            warnings.push(format!("meshopt view {}: decode failed ({rc})", view.index()));
            continue;
        }
        unsafe {
            use meshopt::ffi::*;
            match filter.as_str() {
                "OCTAHEDRAL" if stride == 4 || stride == 8 => meshopt_decodeFilterOct(dst.as_mut_ptr().cast(), count, stride),
                "QUATERNION" if stride == 8 => meshopt_decodeFilterQuat(dst.as_mut_ptr().cast(), count, stride),
                "EXPONENTIAL" if stride % 4 == 0 => meshopt_decodeFilterExp(dst.as_mut_ptr().cast(), count, stride),
                "COLOR" if stride == 4 || stride == 8 => meshopt_decodeFilterColor(dst.as_mut_ptr().cast(), count, stride),
                _ => {}
            }
        }
        let tb = view.buffer().index();
        let to = view.offset();
        if let Some(target) = buffers.get_mut(tb) {
            if target.len() < to + dst.len() {
                target.resize(to + dst.len(), 0);
            }
            target[to..to + dst.len()].copy_from_slice(&dst);
        }
    }
}

// ---------------------------------------------------------------------------
// Geometry

struct Ctx<'a> {
    buffers: &'a [Vec<u8>],
    /// (buffer, offset, length) per bufferView.
    views: &'a [(usize, usize, usize)],
    scene: &'a mut Scene,
    uv_xform: &'a [Option<[f32; 6]>],
    animated: bool,
    morph: usize,
}

impl Ctx<'_> {
    fn visit(&mut self, node: &gltf::Node, parent: Mat4, depth: u32) {
        if depth > 256 {
            return;
        }
        let m = parent * Mat4::from_cols_array_2d(&node.transform().matrix());
        if let Some(mesh) = node.mesh() {
            self.add_mesh(&mesh, m, Some(node), node.name());
        }
        for c in node.children() {
            self.visit(&c, m, depth + 1);
        }
    }

    fn add_mesh(&mut self, mesh: &gltf::Mesh, world: Mat4, node: Option<&gltf::Node>, node_name: Option<&str>) {
        let base_name = mesh.name().or(node_name).map(String::from).unwrap_or_else(|| format!("mesh {}", mesh.index()));
        let nprims = mesh.primitives().len();
        let default_weights: Vec<f32> = node.and_then(|n| n.weights()).or_else(|| mesh.weights()).map(|w| w.to_vec()).unwrap_or_default();
        for (pi, prim) in mesh.primitives().enumerate() {
            let name = if nprims > 1 { format!("{base_name} [{pi}]") } else { base_name.clone() };
            let Some(mut out) = self.read_primitive(&prim, name) else { continue };
            if let Some(x) = prim.material().index().and_then(|i| self.uv_xform.get(i).copied().flatten()) {
                for uv in &mut out.uvs {
                    let (u, v) = (uv[0], uv[1]);
                    *uv = [x[0] * u + x[2] * v + x[4], x[1] * u + x[3] * v + x[5]];
                }
            }
            self.morph += prim.morph_targets().len();
            if self.animated {
                out.node = node.map(|n| n.index());
                out.skin = node.and_then(|n| n.skin()).map(|s| s.index());
                out.morph_weights = default_weights.clone();
                if out.node.is_none() {
                    out.transform(&world);
                }
            } else {
                out.transform(&world);
            }
            self.scene.meshes.push(out);
        }
    }

    fn read_primitive(&mut self, prim: &gltf::Primitive, name: String) -> Option<Mesh> {
        use gltf::mesh::Mode;
        let bufs = self.buffers;
        let topology = match prim.mode() {
            Mode::Points => Topology::Points,
            Mode::Lines | Mode::LineLoop | Mode::LineStrip => Topology::Lines,
            _ => Topology::Triangles,
        };
        let mut out = Mesh::new(name, topology);
        let idx: Vec<u32>;

        if let Some(draco) = prim.extension_value("KHR_draco_mesh_compression") {
            // Ids of Draco attributes whose glTF accessor is normalized.
            let attrs = draco.get("attributes").and_then(|a| a.as_object());
            let normalized: Vec<u32> = prim
                .attributes()
                .filter(|(_, acc)| acc.normalized())
                .filter_map(|(sem, _)| {
                    let name = match sem {
                        gltf::Semantic::Colors(n) => format!("COLOR_{n}"),
                        gltf::Semantic::TexCoords(n) => format!("TEXCOORD_{n}"),
                        gltf::Semantic::Weights(n) => format!("WEIGHTS_{n}"),
                        gltf::Semantic::Normals => "NORMAL".into(),
                        gltf::Semantic::Positions => "POSITION".into(),
                        _ => return None,
                    };
                    attrs?.get(&name)?.as_u64().map(|v| v as u32)
                })
                .collect();
            match self.read_draco(draco, &normalized, &mut out) {
                Ok(i) => idx = i,
                Err(e) => {
                    self.scene.info.warnings.push(format!("{}: Draco decode failed: {e}", out.name));
                    return None;
                }
            }
        } else {
            let Some(pos) = prim.get(&gltf::Semantic::Positions) else {
                self.scene.info.warnings.push(format!("{}: no POSITION attribute", out.name));
                return None;
            };
            out.positions = chunk3(read_f32(&pos, bufs));
            let n = out.positions.len();
            if let Some(a) = prim.get(&gltf::Semantic::Normals) {
                out.normals = chunk3(read_f32(&a, bufs));
                out.had_normals = out.normals.len() == n;
            }
            if let Some(a) = prim.get(&gltf::Semantic::TexCoords(0)) {
                out.uvs = read_f32(&a, bufs).chunks_exact(2).map(|c| [c[0], c[1]]).collect();
            }
            if let Some(a) = prim.get(&gltf::Semantic::Colors(0)) {
                out.colors = colors_from(&read_f32(&a, bufs), a.dimensions().multiplicity());
            }
            if self.animated {
                if let (Some(j), Some(w)) = (prim.get(&gltf::Semantic::Joints(0)), prim.get(&gltf::Semantic::Weights(0))) {
                    out.joints = read_f32(&j, bufs).chunks_exact(4).map(|c| [c[0] as u16, c[1] as u16, c[2] as u16, c[3] as u16]).collect();
                    out.weights = read_f32(&w, bufs).chunks_exact(4).map(|c| [c[0], c[1], c[2], c[3]]).collect();
                }
                for t in prim.morph_targets() {
                    out.morph_targets.push(MorphTarget {
                        positions: t.positions().map(|a| chunk3(read_f32(&a, bufs))).unwrap_or_else(|| vec![[0.0; 3]; n]),
                        normals: t.normals().map(|a| chunk3(read_f32(&a, bufs))).unwrap_or_default(),
                    });
                }
            }
            idx = match prim.indices() {
                Some(i) => read_u32(&i, bufs),
                None => (0..n as u32).collect(),
            };
        }

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
        Some(out)
    }

    fn read_draco(&mut self, ext: &gltf::json::Value, normalized: &[u32], out: &mut Mesh) -> Result<Vec<u32>> {
        let view_idx = ext.get("bufferView").and_then(|v| v.as_u64()).ok_or_else(|| anyhow!("no bufferView"))? as usize;
        let bytes = self.view_bytes(view_idx).ok_or_else(|| anyhow!("bufferView {view_idx} out of range"))?;
        let mesh = super::draco::decode_with_normalized(&bytes, normalized)?;
        let attrs = ext.get("attributes").and_then(|a| a.as_object()).ok_or_else(|| anyhow!("no attributes"))?;
        let find = |name: &str| -> Option<&super::draco::DracoAttribute> {
            let id = attrs.get(name)?.as_u64()? as u32;
            mesh.attribute(id)
        };
        let n = mesh.num_points;
        let vec3 = |a: &super::draco::DracoAttribute| -> Vec<[f32; 3]> {
            a.data.chunks_exact(a.components.max(1)).map(|c| [c[0], *c.get(1).unwrap_or(&0.0), *c.get(2).unwrap_or(&0.0)]).collect()
        };
        let pos = find("POSITION").ok_or_else(|| anyhow!("no POSITION"))?;
        out.positions = vec3(pos);
        if let Some(a) = find("NORMAL") {
            out.normals = vec3(a);
            out.had_normals = out.normals.len() == n;
        }
        if let Some(a) = find("TEXCOORD_0") {
            out.uvs = a.data.chunks_exact(a.components.max(1)).map(|c| [c[0], *c.get(1).unwrap_or(&0.0)]).collect();
        }
        if let Some(a) = find("COLOR_0") {
            out.colors = colors_from(&a.data, a.components);
        }
        if self.animated {
            if let (Some(j), Some(w)) = (find("JOINTS_0"), find("WEIGHTS_0")) {
                out.joints = j.data.chunks_exact(4).map(|c| [c[0] as u16, c[1] as u16, c[2] as u16, c[3] as u16]).collect();
                out.weights = w.data.chunks_exact(4).map(|c| [c[0], c[1], c[2], c[3]]).collect();
            }
        }
        Ok(if mesh.indices.is_empty() { (0..n as u32).collect() } else { mesh.indices })
    }

    fn view_bytes(&self, view_idx: usize) -> Option<Vec<u8>> {
        let (b, o, l) = *self.views.get(view_idx)?;
        self.buffers.get(b)?.get(o..o + l).map(|s| s.to_vec())
    }
}

fn colors_from(v: &[f32], comps: usize) -> Vec<[u8; 4]> {
    let b = |x: f32| (x * 255.0).round().clamp(0.0, 255.0) as u8;
    match comps {
        3 => v.chunks_exact(3).map(|c| [b(c[0]), b(c[1]), b(c[2]), 255]).collect(),
        4 => v.chunks_exact(4).map(|c| [b(c[0]), b(c[1]), b(c[2]), b(c[3])]).collect(),
        _ => Vec::new(),
    }
}

// ---------------------------------------------------------------------------
// Animation

fn build_anim(doc: &gltf::Document, bufs: &[Vec<u8>]) -> AnimData {
    let mut a = AnimData::default();
    for n in doc.nodes() {
        let (t, r, s) = n.transform().decomposed();
        a.nodes.push(Node {
            name: n.name().map(String::from).unwrap_or_else(|| format!("node {}", n.index())),
            parent: None,
            rest: Trs { t: Vec3::from(t), r: Quat::from_array(r).normalize(), s: Vec3::from(s) },
            post: None,
        });
    }
    for n in doc.nodes() {
        for c in n.children() {
            if let Some(child) = a.nodes.get_mut(c.index()) {
                child.parent = Some(n.index());
            }
        }
    }
    for skin in doc.skins() {
        let joints: Vec<usize> = skin.joints().map(|j| j.index()).collect();
        let inverse_bind = match skin.inverse_bind_matrices() {
            Some(acc) => read_f32(&acc, bufs).chunks_exact(16).map(Mat4::from_cols_slice).collect(),
            None => vec![Mat4::IDENTITY; joints.len()],
        };
        a.skins.push(Skin { joints, inverse_bind });
    }
    for (ai, anim) in doc.animations().enumerate() {
        let mut clip =
            Clip { name: anim.name().map(String::from).unwrap_or_else(|| format!("animation {ai}")), duration: 0.0, tracks: Vec::new() };
        for ch in anim.channels() {
            let s = ch.sampler();
            let times = read_f32(&s.input(), bufs);
            let raw = read_f32(&s.output(), bufs);
            let interp = match s.interpolation() {
                gltf::animation::Interpolation::Step => Interp::Step,
                gltf::animation::Interpolation::Linear => Interp::Linear,
                gltf::animation::Interpolation::CubicSpline => Interp::Cubic,
            };
            use gltf::animation::Property;
            let values = match ch.target().property() {
                Property::Translation => TrackValues::Translation(raw.chunks_exact(3).map(Vec3::from_slice).collect()),
                Property::Scale => TrackValues::Scale(raw.chunks_exact(3).map(Vec3::from_slice).collect()),
                Property::Rotation => TrackValues::Rotation(raw.chunks_exact(4).map(|c| Quat::from_xyzw(c[0], c[1], c[2], c[3])).collect()),
                Property::MorphTargetWeights => TrackValues::Weights(raw),
            };
            if let Some(&last) = times.last() {
                clip.duration = clip.duration.max(last);
            }
            clip.tracks.push(Track { node: ch.target().node().index(), interp, times, values });
        }
        a.clips.push(clip);
    }
    a
}

// ---------------------------------------------------------------------------

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

    fn glb(json: &str, bin: &[u8]) -> Vec<u8> {
        let mut j = json.as_bytes().to_vec();
        while j.len() % 4 != 0 {
            j.push(b' ');
        }
        let mut b = bin.to_vec();
        while b.len() % 4 != 0 {
            b.push(0);
        }
        let total = 12 + 8 + j.len() + 8 + b.len();
        let mut d = Vec::new();
        d.extend_from_slice(b"glTF");
        d.extend_from_slice(&2u32.to_le_bytes());
        d.extend_from_slice(&(total as u32).to_le_bytes());
        d.extend_from_slice(&(j.len() as u32).to_le_bytes());
        d.extend_from_slice(b"JSON");
        d.extend_from_slice(&j);
        d.extend_from_slice(&(b.len() as u32).to_le_bytes());
        d.extend_from_slice(b"BIN\0");
        d.extend_from_slice(&b);
        d
    }

    fn f32s(v: &[f32]) -> Vec<u8> {
        v.iter().flat_map(|x| x.to_le_bytes()).collect()
    }

    #[test]
    fn glb_triangle_with_node_transform() {
        let bin = f32s(&[0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0]);
        let json = r#"{"asset":{"version":"2.0","generator":"test"},"scene":0,"scenes":[{"nodes":[0]}],
"nodes":[{"mesh":0,"translation":[0,0,5]}],"meshes":[{"name":"tri","primitives":[{"attributes":{"POSITION":0}}]}],
"accessors":[{"bufferView":0,"componentType":5126,"count":3,"type":"VEC3","min":[0,0,0],"max":[1,1,0]}],
"bufferViews":[{"buffer":0,"byteLength":36}],"buffers":[{"byteLength":36}]}"#;
        let mut s = load_gltf_slice(&glb(json, &bin), None).unwrap();
        s.finalize();
        assert_eq!(s.meshes.len(), 1);
        assert_eq!(s.meshes[0].name, "tri");
        assert_eq!(s.meshes[0].positions[1], [1.0, 0.0, 5.0]);
        assert_eq!(s.meshes[0].indices, vec![0, 1, 2]);
        assert!(!s.meshes[0].had_normals);
        assert!(s.anim.is_none());
        assert!(s.info.extra.iter().any(|(k, v)| k == "generator" && v == "test"));
    }

    #[test]
    fn quantized_positions_are_dequantized() {
        // i16 normalized positions (KHR_mesh_quantization): 32767 -> 1.0
        let mut bin = Vec::new();
        for v in [0i16, 0, 0, 0, 32767, 0, 0, 0, 0, 32767, 0, 0] {
            bin.extend_from_slice(&v.to_le_bytes());
        }
        let json = r#"{"asset":{"version":"2.0"},"extensionsUsed":["KHR_mesh_quantization"],"extensionsRequired":["KHR_mesh_quantization"],
"scene":0,"scenes":[{"nodes":[0]}],"nodes":[{"mesh":0}],"meshes":[{"primitives":[{"attributes":{"POSITION":0}}]}],
"accessors":[{"bufferView":0,"componentType":5122,"normalized":true,"count":3,"type":"VEC3","min":[0,0,0],"max":[1,1,0]}],
"bufferViews":[{"buffer":0,"byteLength":24,"byteStride":8}],"buffers":[{"byteLength":24}]}"#;
        let s = load_gltf_slice(&glb(json, &bin), None).unwrap();
        assert!((s.meshes[0].positions[1][0] - 1.0).abs() < 1e-6);
        assert!((s.meshes[0].positions[2][1] - 1.0).abs() < 1e-6);
    }

    #[test]
    fn animated_node_produces_clip() {
        let mut bin = f32s(&[0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0]); // 36 bytes positions
        bin.extend(f32s(&[0.0, 1.0])); // times @36
        bin.extend(f32s(&[0.0, 0.0, 0.0, 3.0, 0.0, 0.0])); // translations @44
        let json = r#"{"asset":{"version":"2.0"},"scene":0,"scenes":[{"nodes":[0]}],"nodes":[{"mesh":0,"name":"mover"}],
"meshes":[{"primitives":[{"attributes":{"POSITION":0}}]}],
"accessors":[{"bufferView":0,"componentType":5126,"count":3,"type":"VEC3","min":[0,0,0],"max":[1,1,0]},
 {"bufferView":1,"componentType":5126,"count":2,"type":"SCALAR","min":[0],"max":[1]},
 {"bufferView":2,"componentType":5126,"count":2,"type":"VEC3"}],
"bufferViews":[{"buffer":0,"byteLength":36},{"buffer":0,"byteOffset":36,"byteLength":8},{"buffer":0,"byteOffset":44,"byteLength":24}],
"animations":[{"name":"slide","channels":[{"sampler":0,"target":{"node":0,"path":"translation"}}],"samplers":[{"input":1,"output":2}]}],
"buffers":[{"byteLength":68}]}"#;
        let mut s = load_gltf_slice(&glb(json, &bin), None).unwrap();
        s.finalize();
        let a = s.anim.as_ref().unwrap();
        assert_eq!(a.clips[0].name, "slide");
        assert!((a.clips[0].duration - 1.0).abs() < 1e-6);
        assert_eq!(s.meshes[0].node, Some(0));
        let pose = crate::anim::evaluate(a, &[0], 1.0);
        assert!((pose.worlds[0].transform_point3(Vec3::ZERO).x - 3.0).abs() < 1e-5);
        assert!(s.info.animations[0].starts_with("slide"));
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
