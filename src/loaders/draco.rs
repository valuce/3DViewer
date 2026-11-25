//! Draco geometry decompression for glTF `KHR_draco_mesh_compression`.
//!
//! A thin wrapper over the pure-Rust [`draco_core`] decoder, which matches Google's Draco 1.5.x
//! bit for bit: EdgeBreaker (standard and valence) and sequential connectivity, KD-tree and
//! sequential point clouds, every prediction scheme (parallelogram, constrained
//! multi-parallelogram, portable tex-coords, octahedral normals, ...) and the legacy 1.x/2.0
//! bitstreams. No C++ and no runtime DLLs.
//!
//! The result is format-neutral: per-point `f32` channels keyed by the Draco attribute unique id,
//! which is the number glTF stores in `extensions.KHR_draco_mesh_compression.attributes`
//! (e.g. `{"POSITION": 0, "NORMAL": 1, "TEXCOORD_0": 2}`).

// A small public API for the glTF loader, which need not use every entry point.

use anyhow::{Result, anyhow, bail};
use draco_core::{DataType, DecoderBuffer, Mesh, MeshDecoder};

/// One decoded vertex attribute.
#[derive(Debug, Clone)]
pub struct DracoAttribute {
    /// Draco unique id: the value glTF maps the semantic to in the extension's `attributes`.
    pub unique_id: u32,
    /// Components per point (3 for POSITION, 2 for TEXCOORD_n, 4 for JOINTS_n, ...).
    pub components: usize,
    /// `num_points * components` values, point-major. Quantized float attributes are dequantized
    /// and octahedral normals expanded back to unit vectors. Integer attributes keep their integer
    /// value (JOINTS_n come back as 0.0, 1.0, 2.0, ...) unless they are normalized, in which case
    /// they are mapped to `[0, 1]` (unsigned) or `[-1, 1]` (signed) as glTF specifies.
    pub data: Vec<f32>,
}

/// A decoded Draco mesh or point cloud.
#[derive(Debug, Clone, Default)]
pub struct DracoMesh {
    /// Number of decoded points (vertices). It can differ a little from the vertex count of the
    /// original mesh; glTF writers store the decoded count in the primitive's accessors.
    pub num_points: usize,
    /// Triangle list, three point indices per face, all `< num_points`. Empty for point clouds.
    pub indices: Vec<u32>,
    /// Attributes in stream order. Attributes of types glTF cannot express (64-bit integers,
    /// booleans) are left out.
    pub attributes: Vec<DracoAttribute>,
}

impl DracoMesh {
    /// The attribute with the given unique id (the value in the glTF extension's `attributes`).
    pub fn attribute(&self, unique_id: u32) -> Option<&DracoAttribute> {
        self.attributes.iter().find(|a| a.unique_id == unique_id)
    }
}

/// Decode a Draco stream: the bytes of the bufferView the glTF extension points at.
///
/// Integer attributes are normalized only when the stream flags them as normalized, which is what
/// Google's decoder does. Blender sets that flag, but gltf-pipeline does not: a normalized
/// `UNSIGNED_SHORT` COLOR_0 it compressed comes back here as 0..65535. A glTF loader should
/// therefore call [`decode_with_normalized`] with the ids whose accessor says `normalized: true`.
#[allow(dead_code)] // used by tests; the glTF loader passes normalized ids
pub fn decode(bytes: &[u8]) -> Result<DracoMesh> {
    decode_with_normalized(bytes, &[])
}

/// Like [`decode`], but also normalizes the integer attributes whose unique id is in
/// `normalized_ids`, whether or not the stream flags them. Pass the ids of the glTF attributes
/// whose accessor has `normalized: true`. Float attributes are never rescaled.
pub fn decode_with_normalized(bytes: &[u8], normalized_ids: &[u32]) -> Result<DracoMesh> {
    if !bytes.starts_with(b"DRACO") {
        bail!("not a Draco stream");
    }
    let mut mesh = Mesh::new();
    MeshDecoder::new().decode(&mut DecoderBuffer::new(bytes), &mut mesh).map_err(|e| anyhow!("Draco: {e}"))?;

    let num_points = mesh.num_points();
    if u32::try_from(num_points).is_err() {
        bail!("Draco: too many points ({num_points})");
    }

    let indices: Vec<u32> = mesh.faces().iter().flatten().map(|p| p.0).collect();
    if let Some(&bad) = indices.iter().find(|&&i| i as usize >= num_points) {
        bail!("Draco: face index {bad} out of range ({num_points} points)");
    }

    let num_attributes = mesh.num_attributes().max(0);
    let mut attributes = Vec::with_capacity(num_attributes as usize);
    for id in 0..num_attributes {
        let att = mesh.try_attribute(id).map_err(|e| anyhow!("Draco: {e}"))?;
        let components = usize::from(att.num_components());
        let data_type = att.data_type();
        if components == 0 || !is_supported(data_type) {
            continue;
        }
        // Follows the point-to-value map, widens the stored type and zero-fills missing values.
        let mut data = att.read_f32s(num_points, components);
        if att.normalized() || normalized_ids.contains(&att.unique_id()) {
            normalize(&mut data, data_type);
        }
        attributes.push(DracoAttribute { unique_id: att.unique_id(), components, data });
    }
    Ok(DracoMesh { num_points, indices, attributes })
}

/// Types `PointAttribute::read_f32s` widens correctly (it yields zeros for the rest).
fn is_supported(ty: DataType) -> bool {
    matches!(
        ty,
        DataType::Int8
            | DataType::Uint8
            | DataType::Int16
            | DataType::Uint16
            | DataType::Int32
            | DataType::Uint32
            | DataType::Float32
            | DataType::Float64
    )
}

/// Map integer values to `[0, 1]` / `[-1, 1]` the way glTF defines normalized accessors.
fn normalize(data: &mut [f32], ty: DataType) {
    let max = match ty {
        DataType::Int8 => i8::MAX as f32,
        DataType::Uint8 => u8::MAX as f32,
        DataType::Int16 => i16::MAX as f32,
        DataType::Uint16 => u16::MAX as f32,
        DataType::Int32 => i32::MAX as f32,
        DataType::Uint32 => u32::MAX as f32,
        _ => return,
    };
    for v in data {
        *v = (*v / max).max(-1.0);
    }
}

#[cfg(test)]
mod tests {
    //! Always-run tests use the small files in `tests/data/`:
    //! - `draco_blender45.glb`: Blender 4.5 glTF export, Draco level 6 (EdgeBreaker valence).
    //!   A UV sphere with per-corner COLOR_0 (normalized u16) and Suzanne skinned to three bones
    //!   (JOINTS_0 u8, WEIGHTS_0).
    //! - `draco_pointcloud.drc`, `draco_intmesh.drc`: Google's encoder (npm `draco3d` 1.5.9), see
    //!   the individual tests.
    //!
    //! The other tests need generated files in `target/testdata/draco/` (or `$DRACO_TESTDATA`) and
    //! are skipped when it is missing:
    //! - `b45_*.glb` / `b51_*.glb`: the same scene from Blender 4.5 / 5.1, exported plain
    //!   (`*_plain.glb`) and with Draco levels 0 (sequential), 3, 6, 10 and a low-quantization
    //!   variant (`export_draco_mesh_compression_enable=True, export_draco_mesh_compression_level=N`).
    //! - `gp_*.glb`: `npx gltf-pipeline -i b45_plain.glb -o gp_x.glb -d` with compression levels
    //!   0, 7 (default), 10 and `--draco.unifiedQuantization`.
    //! - `ref/*.ref.json`: every Draco stream above (and `*.drc`) decoded by Google's decoder
    //!   (`draco3d` 1.5.9, `GetAttributeFloatForAllPoints`), for an exact comparison.
    use super::*;
    use serde_json::Value;
    use std::path::{Path, PathBuf};

    fn manifest_path(rel: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join(rel)
    }

    fn read_bundled(name: &str) -> Vec<u8> {
        let path = manifest_path("tests/data").join(name);
        std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
    }

    fn testdata_dir() -> Option<PathBuf> {
        let dir = std::env::var_os("DRACO_TESTDATA").map(PathBuf::from).unwrap_or_else(|| manifest_path("target/testdata/draco"));
        if dir.is_dir() {
            Some(dir)
        } else {
            eprintln!("skipping: {} not found", dir.display());
            None
        }
    }

    fn files_in(dir: &Path, pred: impl Fn(&str) -> bool) -> Vec<PathBuf> {
        let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
            .unwrap()
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.file_name().and_then(|n| n.to_str()).is_some_and(&pred))
            .collect();
        files.sort();
        files
    }

    // ---- A minimal GLB reader, independent of the gltf crate. ----

    struct Glb {
        json: Value,
        bin: Vec<u8>,
    }

    fn parse_glb(bytes: &[u8]) -> Glb {
        assert_eq!(&bytes[..4], b"glTF");
        let (mut json, mut bin, mut pos) = (Value::Null, Vec::new(), 12);
        while pos + 8 <= bytes.len() {
            let len = u32::from_le_bytes(bytes[pos..pos + 4].try_into().unwrap()) as usize;
            let chunk = &bytes[pos + 8..pos + 8 + len];
            match &bytes[pos + 4..pos + 8] {
                b"JSON" => json = serde_json::from_slice(chunk).unwrap(),
                b"BIN\0" => bin = chunk.to_vec(),
                _ => {}
            }
            pos += 8 + len;
        }
        Glb { json, bin }
    }

    fn components_of(ty: &str) -> usize {
        match ty {
            "SCALAR" => 1,
            "VEC2" => 2,
            "VEC3" => 3,
            "VEC4" | "MAT2" => 4,
            "MAT3" => 9,
            "MAT4" => 16,
            _ => panic!("accessor type {ty}"),
        }
    }

    fn uint(v: &Value) -> usize {
        v.as_u64().unwrap_or_else(|| panic!("expected an integer, got {v}")) as usize
    }

    impl Glb {
        fn view(&self, index: usize) -> (&[u8], usize) {
            let v = &self.json["bufferViews"][index];
            let off = v["byteOffset"].as_u64().unwrap_or(0) as usize;
            (&self.bin[off..off + uint(&v["byteLength"])], v["byteStride"].as_u64().unwrap_or(0) as usize)
        }

        fn accessor(&self, index: usize) -> &Value {
            &self.json["accessors"][index]
        }

        /// Read an accessor as floats, applying `normalized` as a glTF loader would.
        fn read(&self, index: usize) -> (Vec<f32>, usize) {
            let a = self.accessor(index);
            let comps = components_of(a["type"].as_str().unwrap());
            let ty = uint(&a["componentType"]);
            let size = match ty {
                5120 | 5121 => 1,
                5122 | 5123 => 2,
                _ => 4,
            };
            let norm = a["normalized"].as_bool().unwrap_or(false);
            let (view, stride) = self.view(uint(&a["bufferView"]));
            let data = &view[a["byteOffset"].as_u64().unwrap_or(0) as usize..];
            let stride = if stride == 0 { size * comps } else { stride };
            let mut out = Vec::new();
            for i in 0..uint(&a["count"]) {
                for c in 0..comps {
                    let b = &data[i * stride + c * size..];
                    let (v, max) = match ty {
                        5120 => (b[0] as i8 as f32, 127.0),
                        5121 => (b[0] as f32, 255.0),
                        5122 => (i16::from_le_bytes([b[0], b[1]]) as f32, 32767.0),
                        5123 => (u16::from_le_bytes([b[0], b[1]]) as f32, 65535.0),
                        5125 => (u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as f32, 1.0),
                        5126 => (f32::from_le_bytes([b[0], b[1], b[2], b[3]]), 1.0),
                        _ => panic!("componentType {ty}"),
                    };
                    out.push(if norm { (v / max).max(-1.0) } else { v });
                }
            }
            (out, comps)
        }
    }

    /// One Draco-compressed primitive of a GLB.
    struct DracoPrim<'a> {
        glb: &'a Glb,
        mesh_index: usize,
        prim_index: usize,
        mesh_name: String,
        prim: &'a Value,
        stream: &'a [u8],
        /// glTF semantic -> Draco unique id, from the extension.
        ids: Vec<(String, u32)>,
    }

    fn draco_prims(glb: &Glb) -> Vec<DracoPrim<'_>> {
        let mut out = Vec::new();
        for (mesh_index, mesh) in glb.json["meshes"].as_array().unwrap().iter().enumerate() {
            for (prim_index, prim) in mesh["primitives"].as_array().unwrap().iter().enumerate() {
                let ext = &prim["extensions"]["KHR_draco_mesh_compression"];
                if ext.is_null() {
                    continue;
                }
                let ids = ext["attributes"].as_object().unwrap().iter().map(|(k, v)| (k.clone(), uint(v) as u32)).collect();
                out.push(DracoPrim {
                    glb,
                    mesh_index,
                    prim_index,
                    mesh_name: mesh["name"].as_str().unwrap_or("").to_string(),
                    prim,
                    stream: glb.view(uint(&ext["bufferView"])).0,
                    ids,
                });
            }
        }
        out
    }

    impl DracoPrim<'_> {
        fn accessor(&self, semantic: &str) -> &Value {
            self.glb.accessor(uint(&self.prim["attributes"][semantic]))
        }

        fn id(&self, semantic: &str) -> u32 {
            self.ids.iter().find(|(s, _)| s == semantic).unwrap_or_else(|| panic!("no {semantic}")).1
        }

        /// Decode as a glTF loader should: normalize what the accessors call normalized.
        fn decode(&self) -> DracoMesh {
            let normalized: Vec<u32> =
                self.ids.iter().filter(|(sem, _)| self.accessor(sem)["normalized"].as_bool().unwrap_or(false)).map(|&(_, id)| id).collect();
            decode_with_normalized(self.stream, &normalized).unwrap_or_else(|e| panic!("{}: {e}", self.mesh_name))
        }
    }

    fn channel_range(att: &DracoAttribute, c: usize) -> (f32, f32) {
        att.data.iter().skip(c).step_by(att.components).fold((f32::MAX, f32::MIN), |(lo, hi), &v| (lo.min(v), hi.max(v)))
    }

    /// Check a decoded primitive against what its glTF JSON promises.
    fn check_against_gltf(p: &DracoPrim, m: &DracoMesh) {
        let what = &p.mesh_name;
        assert_eq!(m.num_points, uint(&p.accessor("POSITION")["count"]), "{what}: point count");
        if let Some(i) = p.prim["indices"].as_u64() {
            assert_eq!(m.indices.len(), uint(&p.glb.accessor(i as usize)["count"]), "{what}: index count");
        }
        assert!(m.indices.iter().all(|&i| (i as usize) < m.num_points), "{what}: index range");
        for (sem, id) in &p.ids {
            let acc = p.accessor(sem);
            let att = m.attribute(*id).unwrap_or_else(|| panic!("{what}: no attribute {id} for {sem}"));
            assert_eq!(att.components, components_of(acc["type"].as_str().unwrap()), "{what} {sem}: components");
            assert_eq!(att.data.len(), m.num_points * att.components, "{what} {sem}: length");
            assert!(att.data.iter().all(|v| v.is_finite()), "{what} {sem}: non-finite values");
            if let (Some(min), Some(max)) = (acc["min"].as_array(), acc["max"].as_array()) {
                for c in 0..att.components {
                    let (lo, hi) = (min[c].as_f64().unwrap() as f32, max[c].as_f64().unwrap() as f32);
                    let tol = (hi - lo).abs() * 2e-3 + 1e-5;
                    let (dlo, dhi) = channel_range(att, c);
                    assert!(dlo >= lo - tol && dhi <= hi + tol, "{what} {sem}[{c}]: decoded {dlo}..{dhi}, accessor {lo}..{hi}");
                }
            }
            for v in att.data.chunks(att.components) {
                if sem == "NORMAL" {
                    let len = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
                    assert!((len - 1.0).abs() < 0.02, "{what}: normal length {len}");
                } else if sem.starts_with("JOINTS_") {
                    assert!(v.iter().all(|j| *j >= 0.0 && j.fract() == 0.0), "{what}: joints {v:?}");
                } else if sem.starts_with("WEIGHTS_") {
                    let sum: f32 = v.iter().sum();
                    assert!((sum - 1.0).abs() < 0.03, "{what}: weights {v:?}");
                } else if sem.starts_with("COLOR_") {
                    assert!(v.iter().all(|c| (-1e-6..=1.0 + 1e-6).contains(c)), "{what}: color {v:?}");
                }
            }
        }
    }

    // ---- Always-run tests on the bundled files. ----

    #[test]
    fn bundled_blender_glb() {
        let bytes = read_bundled("draco_blender45.glb");
        let glb = parse_glb(&bytes);
        let prims = draco_prims(&glb);
        assert_eq!(prims.len(), 2);
        for p in &prims {
            check_against_gltf(p, &p.decode());
        }

        let sphere = prims.iter().find(|p| p.mesh_name == "Sphere").unwrap();
        let m = decode(sphere.stream).unwrap();
        assert_eq!((m.num_points, m.indices.len() / 3), (4502, 2208));
        // Blender flags COLOR_0 as normalized in the stream, so plain `decode` normalizes it too.
        let color = m.attribute(sphere.id("COLOR_0")).unwrap();
        assert!(color.data.iter().all(|v| (0.0..=1.0).contains(v)));
        assert!(color.data.chunks(4).all(|c| c[3] == 1.0));

        let suzanne = prims.iter().find(|p| p.mesh_name == "Suzanne").unwrap();
        let m = decode(suzanne.stream).unwrap();
        assert_eq!((m.num_points, m.indices.len() / 3), (1966, 968));
        let joints = m.attribute(suzanne.id("JOINTS_0")).unwrap();
        assert!(joints.data.iter().all(|&j| j == 0.0 || j == 1.0 || j == 2.0));
        assert!(joints.data.contains(&2.0));
    }

    /// Google's encoder, KD-tree point cloud (speed 6) of 500 points: POSITION f32 (12-bit
    /// quantization), COLOR u8x4 normalized, GENERIC u16 = the point's original index.
    #[test]
    fn bundled_point_cloud() {
        let m = decode(&read_bundled("draco_pointcloud.drc")).unwrap();
        assert_eq!(m.num_points, 500);
        assert!(m.indices.is_empty());
        let (pos, color, ids) = (m.attribute(0).unwrap(), m.attribute(1).unwrap(), m.attribute(2).unwrap());
        assert_eq!((pos.components, color.components, ids.components), (3, 4, 1));

        // The KD-tree reorders points, so check each point against the generator's formula.
        let mut seen = vec![false; 500];
        for p in 0..500 {
            let k = ids.data[p];
            assert!(k.fract() == 0.0 && (0.0..500.0).contains(&k), "id {k}");
            let k = k as usize;
            seen[k] = true;
            let (t, a) = (k as f64 / 500.0, k as f64 * 2.39996323);
            let want = [t.sqrt() * a.cos() * 3.0, (t - 0.5) * 2.0, t.sqrt() * a.sin() * 3.0 + 10.0];
            for c in 0..3 {
                assert!((pos.data[p * 3 + c] as f64 - want[c]).abs() < 4e-3, "point {k}: {:?} vs {want:?}", &pos.data[p * 3..p * 3 + 3]);
            }
            let want = [(k * 7) & 255, (k * 13) & 255, (k * 29) & 255, 255].map(|v| v as f32 / 255.0);
            assert_eq!(&color.data[p * 4..p * 4 + 4], &want);
        }
        assert!(seen.iter().all(|&s| s));
    }

    /// Google's encoder, EdgeBreaker (speed 3), KHR_mesh_quantization-style integer attributes on
    /// a 41x41 grid: POSITION f32 (14-bit), NORMAL i8 normalized, TEXCOORD u16 normalized,
    /// GENERIC u8x4 (not normalized) = (x % 4, y % 3, 0, 7).
    #[test]
    fn bundled_integer_mesh() {
        let m = decode(&read_bundled("draco_intmesh.drc")).unwrap();
        assert_eq!((m.num_points, m.indices.len()), (1681, 3200 * 3));
        let att = |id| m.attribute(id).unwrap();
        let (pos, nrm, uv, gen_) = (att(0), att(1), att(2), att(3));
        assert_eq!((pos.components, nrm.components, uv.components, gen_.components), (3, 3, 2, 4));
        for p in 0..m.num_points {
            let (u, v) = (uv.data[p * 2], uv.data[p * 2 + 1]);
            let (x, y) = ((u * 40.0).round(), (v * 40.0).round());
            assert!((u * 40.0 - x).abs() < 1e-3 && (v * 40.0 - y).abs() < 1e-3, "uv {u} {v}");
            let (u, v) = (x / 40.0, y / 40.0);
            let h = 0.3 * (u * 6.0).sin() * (v * 5.0).cos();
            let want = [u * 4.0 - 2.0, h, v * 4.0 - 2.0];
            for c in 0..3 {
                assert!((pos.data[p * 3 + c] - want[c]).abs() < 1e-3, "pos {:?} vs {want:?}", &pos.data[p * 3..p * 3 + 3]);
            }
            let n = &nrm.data[p * 3..p * 3 + 3];
            let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
            assert!((len - 1.0).abs() < 0.02 && n[1] > 0.0, "normal {n:?}");
            assert_eq!(&gen_.data[p * 4..p * 4 + 4], &[x % 4.0, y % 3.0, 0.0, 7.0]);
        }
        // The triangles tile the 4x4 square exactly once, all facing up (+Y).
        let mut area = 0.0;
        for t in m.indices.chunks(3) {
            let [a, b, c] = [t[0], t[1], t[2]].map(|i| (pos.data[i as usize * 3], pos.data[i as usize * 3 + 2]));
            let signed = 0.5 * ((b.0 - a.0) * (c.1 - a.1) - (c.0 - a.0) * (b.1 - a.1));
            assert!(signed < 0.0, "triangle winding");
            area -= signed;
        }
        assert!((area - 16.0).abs() < 1e-3, "area {area}");
    }

    #[test]
    fn garbage_is_an_error() {
        let mut noise = vec![0u8; 4096];
        let mut s = 0x2545_f491_4f6c_dd1du64;
        for b in &mut noise {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            *b = s as u8;
        }
        let mut draco_noise = b"DRACO".to_vec();
        draco_noise.extend_from_slice(&noise);
        let mut header_only = b"DRACO\x02\x02\x01\x00\x00\x00".to_vec();
        let cases: [&[u8]; 6] = [b"", b"DRACO", &header_only, &noise, &draco_noise, b"glTF\x02\x00\x00\x00"];
        for case in cases {
            assert!(decode(case).is_err(), "{} bytes decoded", case.len());
        }
        header_only.extend_from_slice(&[0xff; 64]);
        assert!(decode(&header_only).is_err());
    }

    /// Truncated and bit-flipped streams must fail cleanly (or decode to something consistent),
    /// never panic: release builds abort on panic.
    #[test]
    fn corrupted_streams_do_not_panic() {
        let glb_bytes = read_bundled("draco_blender45.glb");
        let glb = parse_glb(&glb_bytes);
        let mut streams: Vec<Vec<u8>> = draco_prims(&glb).iter().map(|p| p.stream.to_vec()).collect();
        streams.push(read_bundled("draco_pointcloud.drc"));
        streams.push(read_bundled("draco_intmesh.drc"));

        let check = |bytes: &[u8]| {
            if let Ok(m) = decode(bytes) {
                assert!(m.indices.iter().all(|&i| (i as usize) < m.num_points));
                assert!(m.attributes.iter().all(|a| a.data.len() == m.num_points * a.components));
            }
        };
        let mut s = 0x9e37_79b9_7f4a_7c15u64;
        for stream in &streams {
            let step = (stream.len() / 300).max(1);
            for len in (0..stream.len()).step_by(step) {
                check(&stream[..len]);
            }
            for _ in 0..400 {
                s = s.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                let mut bad = stream.clone();
                // Mostly the headers and early data, where the sizes and modes live.
                let at = if s & 1 == 0 { (s >> 8) as usize % bad.len().min(64) } else { (s >> 8) as usize % bad.len() };
                bad[at] ^= 1 << ((s >> 40) % 8);
                check(&bad);
            }
        }
    }

    // ---- Tests on generated files (skipped when missing). ----

    /// Decoded geometry matches the uncompressed export of the same scene. A decoded point
    /// "matches" an original vertex when every attribute is within quantization tolerance
    /// (quantization can make several originals match). Checked: every decoded point matches some
    /// original, every original is matched, the triangle counts and surface areas agree, and every
    /// triangle on either side has a counterpart on the other whose corners match in the same
    /// cyclic order (so connectivity and winding survive).
    #[test]
    fn exports_match_uncompressed() {
        let Some(dir) = testdata_dir() else { return };
        let files = files_in(&dir, |n| n.ends_with(".glb") && n.contains("_draco"));
        assert!(!files.is_empty());
        for file in files {
            let name = file.file_name().unwrap().to_string_lossy().into_owned();
            let plain_name = if name.starts_with("b51_") { "b51_plain.glb" } else { "b45_plain.glb" };
            let plain = parse_glb(&std::fs::read(dir.join(plain_name)).unwrap());
            let bytes = std::fs::read(&file).unwrap();
            let glb = parse_glb(&bytes);
            let prims = draco_prims(&glb);
            assert_eq!(prims.len(), 2, "{name}");
            for p in &prims {
                let m = p.decode();
                check_against_gltf(p, &m);
                let plain_prim = &plain.json["meshes"][p.mesh_index]["primitives"][p.prim_index];
                compare_with_plain(&format!("{name} {}", p.mesh_name), p, &m, &plain, plain_prim);
            }
            eprintln!("{name}: ok");
        }
    }

    /// One attribute of a primitive, decoded and from the uncompressed export.
    struct Channel<'a> {
        normal: bool,
        decoded: &'a [f32],
        plain: Vec<f32>,
        comps: usize,
        tol: f32,
    }

    impl Channel<'_> {
        /// `1 - cos(angle)` for normals, the largest component difference otherwise.
        fn distance(&self, i: usize, j: usize) -> f32 {
            let a = &self.decoded[i * self.comps..(i + 1) * self.comps];
            let b = &self.plain[j * self.comps..(j + 1) * self.comps];
            if self.normal {
                1.0 - a.iter().zip(b).map(|(x, y)| x * y).sum::<f32>()
            } else {
                a.iter().zip(b).map(|(x, y)| (x - y).abs()).fold(0.0, f32::max)
            }
        }
    }

    fn compare_with_plain(what: &str, p: &DracoPrim, m: &DracoMesh, plain: &Glb, plain_prim: &Value) {
        let mut channels = Vec::new();
        for (sem, id) in &p.ids {
            let (values, comps) = plain.read(uint(&plain_prim["attributes"][sem.as_str()]));
            let att = m.attribute(*id).unwrap();
            assert_eq!(att.components, comps, "{what} {sem}");
            let tol = match sem.as_str() {
                "POSITION" => 0.01, // 10-bit quantization of a ~2.5 unit box in the low-quality files
                "NORMAL" => 0.02,
                s if s.starts_with("JOINTS_") => 0.0,
                _ => 0.01,
            };
            channels.push(Channel { normal: sem == "NORMAL", decoded: &att.data, plain: values, comps, tol });
        }
        let pos = &channels[p.ids.iter().position(|(s, _)| s == "POSITION").unwrap()];
        let n_plain = pos.plain.len() / 3;

        // Plain vertices sorted by x, for a window search around each decoded position.
        let mut by_x: Vec<usize> = (0..n_plain).collect();
        by_x.sort_by(|&a, &b| pos.plain[a * 3].total_cmp(&pos.plain[b * 3]));

        // matches[i]: the original vertices decoded point i matches; matched_by[j]: the reverse.
        let mut matches: Vec<Vec<usize>> = vec![Vec::new(); m.num_points];
        let mut matched_by: Vec<Vec<usize>> = vec![Vec::new(); n_plain];
        for i in 0..m.num_points {
            let x = pos.decoded[i * 3];
            let start = by_x.partition_point(|&j| pos.plain[j * 3] < x - pos.tol);
            for &j in by_x[start..].iter().take_while(|&&j| pos.plain[j * 3] <= x + pos.tol) {
                if channels.iter().all(|ch| ch.distance(i, j) <= ch.tol) {
                    matches[i].push(j);
                    matched_by[j].push(i);
                }
            }
            assert!(!matches[i].is_empty(), "{what}: decoded point {i} matches no original vertex");
        }
        let missing = matched_by.iter().filter(|v| v.is_empty()).count();
        assert_eq!(missing, 0, "{what}: {missing} original vertices missing");

        let (plain_idx, _) = plain.read(uint(&plain_prim["indices"]));
        let plain_idx: Vec<u32> = plain_idx.iter().map(|&i| i as u32).collect();
        assert_eq!(m.indices.len(), plain_idx.len(), "{what}: triangle count");
        let area = |pos: &[f32], idx: &[u32]| -> f64 {
            let p = |i: u32| [0, 1, 2].map(|c| pos[i as usize * 3 + c] as f64);
            idx.chunks(3)
                .map(|t| {
                    let (a, b, c) = (p(t[0]), p(t[1]), p(t[2]));
                    let (u, v) = ([b[0] - a[0], b[1] - a[1], b[2] - a[2]], [c[0] - a[0], c[1] - a[1], c[2] - a[2]]);
                    let n = [u[1] * v[2] - u[2] * v[1], u[2] * v[0] - u[0] * v[2], u[0] * v[1] - u[1] * v[0]];
                    0.5 * (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt()
                })
                .sum()
        };
        let (got_area, want_area) = (area(pos.decoded, &m.indices), area(&pos.plain, &plain_idx));
        assert!((got_area / want_area - 1.0).abs() < 0.01, "{what}: surface area {got_area} vs {want_area}");

        // Every triangle of `tris` has one in `other` whose corners, in the same cyclic order,
        // are among the matches of its own corners.
        let all_have_counterpart = |tris: &[u32], other: &[u32], matches: &[Vec<usize>], n_other: usize| {
            let mut incident: Vec<Vec<usize>> = vec![Vec::new(); n_other];
            for (t, tri) in other.chunks(3).enumerate() {
                for &v in tri {
                    incident[v as usize].push(t);
                }
            }
            tris.chunks(3).all(|tri| {
                let [a, b, c] = [tri[0], tri[1], tri[2]].map(|v| &matches[v as usize]);
                a.iter().any(|&va| {
                    incident[va].iter().any(|&t| {
                        let o = &other[t * 3..t * 3 + 3];
                        let r = o.iter().position(|&v| v as usize == va).unwrap();
                        b.contains(&(o[(r + 1) % 3] as usize)) && c.contains(&(o[(r + 2) % 3] as usize))
                    })
                })
            })
        };
        assert!(all_have_counterpart(&m.indices, &plain_idx, &matches, n_plain), "{what}: a decoded triangle is not in the original");
        assert!(all_have_counterpart(&plain_idx, &m.indices, &matched_by, m.num_points), "{what}: an original triangle is missing");
    }

    /// Bit-exact agreement with Google's decoder (draco3d 1.5.9) on every generated stream.
    #[test]
    fn matches_google_decoder() {
        let Some(dir) = testdata_dir() else { return };
        let refs = files_in(&dir.join("ref"), |n| n.ends_with(".ref.json"));
        if refs.is_empty() {
            eprintln!("skipping: no reference files in {}", dir.join("ref").display());
            return;
        }
        for path in refs {
            let file_name = path.file_name().unwrap().to_string_lossy().into_owned();
            let stem = file_name.trim_end_matches(".ref.json");
            let reference: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
            let m = match stem.split_once('.') {
                Some((base, tag)) => {
                    let bytes = std::fs::read(dir.join(format!("{base}.glb"))).unwrap();
                    let glb = parse_glb(&bytes);
                    let prims = draco_prims(&glb);
                    let p = prims.iter().find(|p| format!("m{}p{}", p.mesh_index, p.prim_index) == tag).unwrap();
                    decode(p.stream)
                }
                None => decode(&std::fs::read(dir.join(format!("{stem}.drc"))).unwrap()),
            }
            .unwrap_or_else(|e| panic!("{stem}: {e}"));

            assert_eq!(m.num_points, uint(&reference["num_points"]), "{stem}: points");
            let want_idx: Vec<u32> = reference["indices"].as_array().unwrap().iter().map(|v| uint(v) as u32).collect();
            assert!(m.indices == want_idx, "{stem}: indices differ");
            let want_atts = reference["attributes"].as_array().unwrap();
            assert_eq!(m.attributes.len(), want_atts.len(), "{stem}: attribute count");
            for (att, want) in m.attributes.iter().zip(want_atts) {
                assert_eq!(att.unique_id, uint(&want["unique_id"]) as u32, "{stem}");
                assert_eq!(att.components, uint(&want["components"]), "{stem}");
                let values = want["data"].as_array().unwrap();
                assert_eq!(att.data.len(), values.len(), "{stem}: attribute {} length", att.unique_id);
                for (k, (&got, want)) in att.data.iter().zip(values).enumerate() {
                    let want = want.as_f64().unwrap();
                    assert!(
                        (got as f64 - want).abs() <= 1e-6 * want.abs().max(1.0),
                        "{stem}: attribute {} value {k}: {got} vs {want}",
                        att.unique_id
                    );
                }
            }
            eprintln!("{stem}: identical ({} points, {} triangles)", m.num_points, m.indices.len() / 3);
        }
    }
}
