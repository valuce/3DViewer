//! FBX and OBJ loader built on ufbx. Converts to Y-up, keeps the file's units.

use super::image::{ImageSource, decode_parallel, resolve_texture_path};
use crate::scene::{AlphaMode, Material, Mesh, Scene, Topology};
use anyhow::{Result, anyhow, bail};
use glam::Mat4;
use std::collections::HashMap;
use std::hash::{BuildHasherDefault, Hasher};
use std::path::Path;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Tiny Fx-style hasher: corner keys are plain integers, SipHash is overkill.
#[derive(Default)]
struct FxHasher(u64);
impl Hasher for FxHasher {
    fn finish(&self) -> u64 {
        self.0
    }
    fn write(&mut self, bytes: &[u8]) {
        for b in bytes {
            self.write_u64(*b as u64);
        }
    }
    fn write_u64(&mut self, i: u64) {
        self.0 = (self.0.rotate_left(5) ^ i).wrapping_mul(0x51_7c_c1_b7_27_22_0a_95);
    }
    fn write_u32(&mut self, i: u32) {
        self.write_u64(i as u64);
    }
}
type FxMap<K, V> = HashMap<K, V, BuildHasherDefault<FxHasher>>;

fn mat4(m: &ufbx::Matrix) -> Mat4 {
    Mat4::from_cols_array(&[
        m.m00 as f32,
        m.m10 as f32,
        m.m20 as f32,
        0.0,
        m.m01 as f32,
        m.m11 as f32,
        m.m21 as f32,
        0.0,
        m.m02 as f32,
        m.m12 as f32,
        m.m22 as f32,
        0.0,
        m.m03 as f32,
        m.m13 as f32,
        m.m23 as f32,
        1.0,
    ])
}

fn file_texture(t: &ufbx::Texture) -> &ufbx::Texture {
    if t.type_ == ufbx::TextureType::File { t } else { t.file_textures.first().map(|r| &**r).unwrap_or(t) }
}

pub fn load_ufbx(path: &Path) -> Result<Scene> {
    let path_str = path.to_str().ok_or_else(|| anyhow!("path is not valid UTF-8"))?;
    let opts = ufbx::LoadOpts {
        target_axes: ufbx::CoordinateAxes {
            right: ufbx::CoordinateAxis::PositiveX,
            up: ufbx::CoordinateAxis::PositiveY,
            front: ufbx::CoordinateAxis::PositiveZ,
        },
        generate_missing_normals: true,
        load_external_files: true,
        ignore_missing_external_files: true,
        ..Default::default()
    };
    let t0 = std::time::Instant::now();
    let root = ufbx::load_file(path_str, opts)
        .map_err(|e| anyhow!("{} ({:?}): file is damaged or not an FBX/OBJ model", e.description, e.type_))?;
    if std::env::var_os("VIEWER3D_PROFILE").is_some() {
        eprintln!("ufbx parse: {:.1} ms", t0.elapsed().as_secs_f64() * 1000.0);
    }
    let sc: &ufbx::Scene = &root;
    let dir = path.parent();
    let mut scene = Scene::default();
    let is_obj = sc.metadata.file_format == ufbx::FileFormat::Obj;
    scene.info.format = if is_obj {
        "Wavefront OBJ".into()
    } else {
        format!("FBX {} ({})", sc.metadata.version, if sc.metadata.ascii { "ASCII" } else { "binary" })
    };

    // Textures referenced by materials (deduplicated by element id).
    let mut tex_slot: FxMap<u32, usize> = FxMap::default();
    let mut jobs = Vec::new();
    let mut missing = Vec::new();
    let mut want = |t: &ufbx::Texture, jobs: &mut Vec<(String, ImageSource)>| -> Option<usize> {
        let t = file_texture(t);
        if let Some(&s) = tex_slot.get(&t.element.element_id) {
            return (s != usize::MAX).then_some(s);
        }
        let name = Path::new(&*t.filename.replace('\\', "/"))
            .file_name()
            .map(|f| f.to_string_lossy().into_owned())
            .unwrap_or_else(|| t.element.name.to_string());
        let src = if !t.content.is_empty() {
            Some(ImageSource::Bytes(t.content.to_vec()))
        } else {
            resolve_texture_path(dir, &[&t.absolute_filename, &t.relative_filename, &t.filename]).map(ImageSource::File)
        };
        let slot = match src {
            Some(s) => {
                jobs.push((name, s));
                jobs.len() - 1
            }
            None => {
                if !t.filename.is_empty() {
                    missing.push(t.filename.to_string());
                }
                usize::MAX
            }
        };
        tex_slot.insert(t.element.element_id, slot);
        (slot != usize::MAX).then_some(slot)
    };

    // Materials; scene.materials index == ufbx typed_id.
    for m in sc.materials.iter() {
        let pbr = &m.pbr;
        let map_tex = |mm: &ufbx::MaterialMap,
                       jobs: &mut Vec<(String, ImageSource)>,
                       want: &mut dyn FnMut(&ufbx::Texture, &mut Vec<(String, ImageSource)>) -> Option<usize>| {
            mm.texture.as_ref().filter(|_| mm.texture_enabled || true).and_then(|t| want(t, jobs))
        };
        let bc = if pbr.base_color.has_value {
            pbr.base_color.value_vec4
        } else if m.fbx.diffuse_color.has_value {
            m.fbx.diffuse_color.value_vec4
        } else {
            ufbx::Vec4 { x: 0.8, y: 0.8, z: 0.8, w: 1.0 }
        };
        let bf = if pbr.base_factor.has_value { pbr.base_factor.value_vec4.x as f32 } else { 1.0 };
        let opacity = if pbr.opacity.has_value { pbr.opacity.value_vec4.x as f32 } else { 1.0 };
        let ef = if pbr.emission_factor.has_value { pbr.emission_factor.value_vec4.x as f32 } else { 1.0 };
        let ec = pbr.emission_color.value_vec4;
        let base_color_tex = map_tex(&pbr.base_color, &mut jobs, &mut want).or_else(|| map_tex(&m.fbx.diffuse_color, &mut jobs, &mut want));
        let normal_tex = map_tex(&pbr.normal_map, &mut jobs, &mut want).or_else(|| map_tex(&m.fbx.normal_map, &mut jobs, &mut want));
        let emissive_tex = map_tex(&pbr.emission_color, &mut jobs, &mut want);
        let roughness = if pbr.roughness.has_value { pbr.roughness.value_vec4.x as f32 } else { 0.6 };
        let metallic = if pbr.metalness.has_value { pbr.metalness.value_vec4.x as f32 } else { 0.0 };
        scene.materials.push(Material {
            name: m.element.name.to_string(),
            base_color: [bc.x as f32 * bf, bc.y as f32 * bf, bc.z as f32 * bf, opacity],
            metallic: metallic.clamp(0.0, 1.0),
            roughness: roughness.clamp(0.04, 1.0),
            emissive: [ec.x as f32 * ef, ec.y as f32 * ef, ec.z as f32 * ef],
            base_color_tex,
            normal_tex,
            emissive_tex,
            mr_tex: None,
            occlusion_tex: None,
            double_sided: false,
            alpha_mode: if opacity < 0.999 { AlphaMode::Blend } else { AlphaMode::Opaque },
            alpha_cutoff: 0.5,
        });
    }

    // Decode textures and remap job slots to scene texture indices.
    let mut job_to_tex = Vec::new();
    for (i, r) in decode_parallel(jobs).into_iter().enumerate() {
        match r {
            Ok(t) => {
                job_to_tex.push(Some(scene.textures.len()));
                scene.textures.push(t);
            }
            Err(e) => {
                scene.info.warnings.push(format!("texture {i}: {e}"));
                job_to_tex.push(None);
            }
        }
    }
    for m in &mut scene.materials {
        for t in [&mut m.base_color_tex, &mut m.normal_tex, &mut m.emissive_tex] {
            *t = t.and_then(|j| job_to_tex.get(j).copied().flatten());
        }
    }
    missing.sort();
    missing.dedup();
    for f in missing {
        scene.info.warnings.push(format!("texture not found: {f}"));
    }

    // Geometry: one output mesh per (node, material part), built in parallel per node.
    let mat_names: Vec<String> = scene.materials.iter().map(|m| m.name.clone()).collect();
    let nodes: Vec<&ufbx::Node> = sc.nodes.iter().filter(|n| n.mesh.is_some()).map(|n| &**n).collect();
    let shared = Shared(&nodes);
    let results: Vec<Mutex<Vec<Mesh>>> = (0..nodes.len()).map(|_| Mutex::new(Vec::new())).collect();
    let next = AtomicUsize::new(0);
    let threads = std::thread::available_parallelism().map(|c| c.get()).unwrap_or(4).min(nodes.len().max(1));
    std::thread::scope(|s| {
        for _ in 0..threads {
            s.spawn(|| {
                let shared = &shared;
                let mut tri = Vec::new();
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    if i >= shared.0.len() {
                        break;
                    }
                    let out = build_node(shared.0[i], &mat_names, &mut tri);
                    *results[i].lock().unwrap() = out;
                }
            });
        }
    });
    for r in results {
        scene.meshes.extend(r.into_inner().unwrap());
    }

    let info = &mut scene.info;
    info.nodes = sc.nodes.len();
    info.skins = sc.skin_deformers.len();
    info.bones = sc.bones.len();
    info.cameras = sc.cameras.len();
    info.lights = sc.lights.len();
    info.morph_targets = sc.blend_channels.len();
    for a in sc.anim_stacks.iter() {
        info.animations.push(format!("{} ({:.2} s)", a.element.name, a.time_end - a.time_begin));
    }
    if !sc.metadata.creator.is_empty() {
        info.extra.push(("creator".into(), sc.metadata.creator.to_string()));
    }
    if !is_obj {
        let u = sc.settings.unit_meters;
        let unit = match u {
            x if (x - 0.001).abs() < 1e-9 => "mm".to_string(),
            x if (x - 0.01).abs() < 1e-9 => "cm".to_string(),
            x if (x - 1.0).abs() < 1e-9 => "m".to_string(),
            x if (x - 0.0254).abs() < 1e-9 => "inch".to_string(),
            x => format!("{x} m"),
        };
        info.extra.push(("unit".into(), unit));
        info.extra.push(("original up axis".into(), format!("{:?}", sc.settings.original_axis_up)));
    }
    for w in sc.metadata.warnings.iter() {
        info.warnings.push(w.description.to_string());
    }
    if scene.meshes.is_empty() {
        bail!("file contains no polygon meshes");
    }
    Ok(scene)
}

/// ufbx data is immutable after loading; the wrapper lets worker threads read it.
struct Shared<'a>(&'a [&'a ufbx::Node]);
// SAFETY: the scene is never mutated while workers run and ufbx readers are pure.
unsafe impl Sync for Shared<'_> {}

fn build_node(node: &ufbx::Node, mat_names: &[String], tri: &mut Vec<u32>) -> Vec<Mesh> {
    let mut meshes = Vec::new();
    let Some(mesh) = node.mesh.as_ref() else { return meshes };
    let world = mat4(&node.geometry_to_world);
    let node_name = if node.element.name.is_empty() { mesh.element.name.to_string() } else { node.element.name.to_string() };
    tri.resize(mesh.max_face_triangles * 3, 0u32);
    let all_faces: Vec<u32>;
    let parts: Vec<(usize, &[u32])> = if mesh.material_parts.is_empty() {
        all_faces = (0..mesh.num_faces as u32).collect();
        vec![(usize::MAX, &all_faces[..])]
    } else {
        mesh.material_parts.iter().map(|p| (p.index as usize, p.face_indices.as_ref())).collect()
    };
    let nparts = parts.iter().filter(|(_, f)| !f.is_empty()).count();
    let has_n = mesh.vertex_normal.exists;
    let has_uv = mesh.vertex_uv.exists;
    let has_c = mesh.vertex_color.exists;
    for (mat_slot, faces) in parts {
        if faces.is_empty() {
            continue;
        }
        let material = node.materials.get(mat_slot).or_else(|| mesh.materials.get(mat_slot)).map(|m| m.element.typed_id as usize);
        let name = match (nparts > 1, material) {
            (true, Some(mi)) => format!("{node_name} [{}]", mat_names.get(mi).map(String::as_str).unwrap_or("?")),
            _ => node_name.clone(),
        };
        let mut out = Mesh::new(name, Topology::Triangles);
        out.material = material;
        // Deduplicate corners by attribute *values* (FBX often stores unique
        // per-corner indices for normals/UVs even when the values repeat).
        let mut map: FxMap<[u32; 9], u32> = FxMap::default();
        map.reserve(faces.len() * 2);
        for &fi in faces {
            let face = mesh.faces[fi as usize];
            if face.num_indices < 3 {
                continue;
            }
            let nt = ufbx::triangulate_face(tri, mesh, face) as usize;
            for &ix in &tri[..nt * 3] {
                let ix = ix as usize;
                let p = mesh.vertex_position[ix];
                let p = [p.x as f32, p.y as f32, p.z as f32];
                let n = if has_n {
                    let n = mesh.vertex_normal[ix];
                    [n.x as f32, n.y as f32, n.z as f32]
                } else {
                    [0.0; 3]
                };
                let t = if has_uv {
                    let t = mesh.vertex_uv[ix];
                    [t.x as f32, 1.0 - t.y as f32]
                } else {
                    [0.0; 2]
                };
                let c = if has_c {
                    let c = mesh.vertex_color[ix];
                    let b = |x: f64| (x * 255.0).round().clamp(0.0, 255.0) as u8;
                    [b(c.x), b(c.y), b(c.z), b(c.w)]
                } else {
                    [0; 4]
                };
                let key = [
                    p[0].to_bits(),
                    p[1].to_bits(),
                    p[2].to_bits(),
                    n[0].to_bits(),
                    n[1].to_bits(),
                    n[2].to_bits(),
                    t[0].to_bits(),
                    t[1].to_bits(),
                    u32::from_le_bytes(c),
                ];
                let next = out.positions.len() as u32;
                let v = *map.entry(key).or_insert_with(|| {
                    out.positions.push(p);
                    if has_n {
                        out.normals.push(n);
                    }
                    if has_uv {
                        out.uvs.push(t);
                    }
                    if has_c {
                        out.colors.push(c);
                    }
                    next
                });
                out.indices.push(v);
            }
        }
        if out.indices.is_empty() {
            continue;
        }
        out.had_normals = has_n && !mesh.generated_normals;
        out.transform(&world);
        meshes.push(out);
    }
    meshes
}
