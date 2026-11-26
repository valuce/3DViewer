//! 3MF loader: zip container + model XML (core spec, base materials, colour groups,
//! components, build items with transforms) and the production extension
//! (objects stored in separate model parts, as written by Bambu/Prusa/Orca slicers).

use crate::scene::{Material, Mesh, Scene, Topology};
use anyhow::{Context, Result, anyhow, bail};
use glam::Mat4;
use quick_xml::Reader;
use quick_xml::events::{BytesStart, Event};
use std::collections::HashMap;
use std::io::{Cursor, Read, Seek};

/// Reference to an object, optionally in another model part (production extension).
type Ref = (Option<String>, u32, Mat4);

#[derive(Default)]
struct Object {
    name: String,
    positions: Vec<[f32; 3]>,
    /// (v1, v2, v3, material key)
    triangles: Vec<([u32; 3], Option<(u32, u32)>)>,
    components: Vec<Ref>,
    default_mat: Option<(u32, u32)>,
}

#[derive(Default)]
struct Part {
    objects: HashMap<u32, Object>,
    colors: HashMap<u32, Vec<[f32; 4]>>,
    names: HashMap<(u32, u32), String>,
    items: Vec<Ref>,
    unit: Option<String>,
}

fn norm(p: &str) -> String {
    p.trim_start_matches('/').to_string()
}

pub fn parse_3mf(data: &[u8]) -> Result<Scene> {
    let mut zip = zip::ZipArchive::new(Cursor::new(data)).context("3MF is not a valid zip archive")?;
    let root = norm(&find_model_path(&mut zip).unwrap_or_else(|| "3D/3dmodel.model".to_string()));
    let mut parts: HashMap<String, Part> = HashMap::new();
    let mut level = vec![root.clone()];
    while !level.is_empty() {
        // Read this level's XML sequentially (zip is not shareable), parse in parallel.
        let mut xmls = Vec::new();
        for path in level.drain(..) {
            if parts.contains_key(&path) || xmls.iter().any(|(p, _)| p == &path) {
                continue;
            }
            let mut xml = String::new();
            zip.by_name(&path).with_context(|| format!("3MF has no {path}"))?.read_to_string(&mut xml)?;
            xmls.push((path, xml));
        }
        let parsed = parse_parallel(&xmls);
        for ((path, _), part) in xmls.into_iter().zip(parsed) {
            let part = part?;
            for o in part.objects.values() {
                for (p, _, _) in &o.components {
                    if let Some(p) = p {
                        let p = norm(p);
                        if !parts.contains_key(&p) {
                            level.push(p);
                        }
                    }
                }
            }
            parts.insert(path, part);
        }
    }

    let mut scene = Scene::default();
    scene.info.format = "3MF".into();
    scene.info.up_axis_z = true;
    let rp = &parts[&root];
    if let Some(u) = &rp.unit {
        scene.info.extra.push(("unit".into(), u.clone()));
    }
    // The 3MF default unit is the millimetre.
    scene.info.unit_m = Some(match rp.unit.as_deref().unwrap_or("millimeter") {
        "micron" => 1e-6,
        "centimeter" => 0.01,
        "inch" => 0.0254,
        "foot" => 0.3048,
        "meter" => 1.0,
        _ => 0.001,
    });
    let mut items = rp.items.clone();
    if items.is_empty() {
        // No build section: show every mesh object of the root part once.
        let mut ids: Vec<u32> = rp.objects.keys().copied().collect();
        ids.sort();
        items = ids.into_iter().map(|i| (None, i, Mat4::IDENTITY)).collect();
    }
    let mut st = Inst { parts: &parts, mat_index: HashMap::new(), scene: &mut scene };
    for (p, oid, m) in &items {
        let part = p.as_deref().map(norm).unwrap_or_else(|| root.clone());
        st.instantiate(&part, *oid, *m, 0)?;
    }
    scene.info.nodes = items.len();
    if parts.len() > 1 {
        scene.info.extra.push(("model parts".into(), parts.len().to_string()));
    }
    if scene.meshes.is_empty() {
        bail!("3MF contains no triangles");
    }
    Ok(scene)
}

fn parse_parallel(xmls: &[(String, String)]) -> Vec<Result<Part>> {
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};
    let n = xmls.len();
    let out: Vec<Mutex<Option<Result<Part>>>> = (0..n).map(|_| Mutex::new(None)).collect();
    let next = AtomicUsize::new(0);
    let threads = std::thread::available_parallelism().map(|c| c.get()).unwrap_or(4).min(n.max(1));
    std::thread::scope(|s| {
        for _ in 0..threads {
            s.spawn(|| {
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    if i >= n {
                        break;
                    }
                    *out[i].lock().unwrap() = Some(parse_part(&xmls[i].1));
                }
            });
        }
    });
    out.into_iter().map(|m| m.into_inner().unwrap().unwrap_or_else(|| Err(anyhow!("not parsed")))).collect()
}

fn find_model_path<R: Read + Seek>(zip: &mut zip::ZipArchive<R>) -> Option<String> {
    let mut rels = String::new();
    zip.by_name("_rels/.rels").ok()?.read_to_string(&mut rels).ok()?;
    let mut r = Reader::from_str(&rels);
    loop {
        match r.read_event() {
            Ok(Event::Empty(e)) | Ok(Event::Start(e)) if e.local_name().as_ref() == "Relationship" => {
                let ty = attr(&e, "Type").unwrap_or_default();
                if ty.ends_with("/3dmodel") {
                    return attr(&e, "Target");
                }
            }
            Ok(Event::Eof) | Err(_) => return None,
            _ => {}
        }
    }
}

fn attr(e: &BytesStart, key: &str) -> Option<String> {
    e.attributes().flatten().find(|a| a.key.local_name().as_ref() == key).map(|a| a.value.into_owned())
}

fn attr_parse<T: std::str::FromStr>(e: &BytesStart, key: &str) -> Option<T> {
    e.attributes().flatten().find(|a| a.key.local_name().as_ref() == key).and_then(|a| a.value.trim().parse().ok())
}

fn attr_f32(e: &BytesStart, key: &str) -> f32 {
    attr_parse(e, key).unwrap_or(0.0)
}

fn attr_u32(e: &BytesStart, key: &str) -> Option<u32> {
    attr_parse(e, key)
}

/// 3MF transform: "m00 m01 m02 m10 m11 m12 m20 m21 m22 m30 m31 m32", row-vector convention.
fn parse_transform(s: Option<String>) -> Mat4 {
    let Some(s) = s else { return Mat4::IDENTITY };
    let v: Vec<f32> = s.split_ascii_whitespace().filter_map(|t| t.parse().ok()).collect();
    if v.len() != 12 {
        return Mat4::IDENTITY;
    }
    Mat4::from_cols_array(&[v[0], v[1], v[2], 0.0, v[3], v[4], v[5], 0.0, v[6], v[7], v[8], 0.0, v[9], v[10], v[11], 1.0])
}

fn parse_color(s: &str) -> [f32; 4] {
    let h = s.trim_start_matches('#');
    let byte = |i: usize| u8::from_str_radix(h.get(i..i + 2).unwrap_or("ff"), 16).unwrap_or(255) as f32 / 255.0;
    let srgb = |c: f32| if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) };
    [srgb(byte(0)), srgb(byte(2)), srgb(byte(4)), if h.len() >= 8 { byte(6) } else { 1.0 }]
}

fn parse_part(xml: &str) -> Result<Part> {
    let mut r = Reader::from_str(xml);
    let mut part = Part::default();
    let mut cur_obj: Option<(u32, Object)> = None;
    let mut cur_group: Option<u32> = None;

    loop {
        let ev = r.read_event().map_err(|e| anyhow!("3MF XML error: {e}"))?;
        let (e, is_start) = match &ev {
            Event::Start(e) => (e, true),
            Event::Empty(e) => (e, false),
            Event::End(e) => {
                match e.local_name().as_ref() {
                    "object" => {
                        if let Some((id, o)) = cur_obj.take() {
                            part.objects.insert(id, o);
                        }
                    }
                    "basematerials" | "colorgroup" => cur_group = None,
                    _ => {}
                }
                continue;
            }
            Event::Eof => break,
            _ => continue,
        };
        match e.local_name().as_ref() {
            "model" => part.unit = attr(e, "unit"),
            "basematerials" | "colorgroup" => {
                if let Some(id) = attr_u32(e, "id") {
                    part.colors.insert(id, Vec::new());
                    cur_group = if is_start { Some(id) } else { None };
                }
            }
            "base" | "color" => {
                if let Some(g) = cur_group {
                    let key = if e.local_name().as_ref() == "base" { "displaycolor" } else { "color" };
                    let c = parse_color(&attr(e, key).unwrap_or_else(|| "#CCCCCC".into()));
                    let list = part.colors.entry(g).or_default();
                    if let Some(n) = attr(e, "name") {
                        part.names.insert((g, list.len() as u32), n);
                    }
                    list.push(c);
                }
            }
            "object" => {
                let id = attr_u32(e, "id").unwrap_or(0);
                let o = Object {
                    name: attr(e, "name").unwrap_or_else(|| format!("object {id}")),
                    default_mat: attr_u32(e, "pid").map(|p| (p, attr_u32(e, "pindex").unwrap_or(0))),
                    ..Default::default()
                };
                if is_start {
                    cur_obj = Some((id, o));
                } else {
                    part.objects.insert(id, o);
                }
            }
            "vertex" => {
                if let Some((_, o)) = cur_obj.as_mut() {
                    o.positions.push([attr_f32(e, "x"), attr_f32(e, "y"), attr_f32(e, "z")]);
                }
            }
            "triangle" => {
                if let Some((_, o)) = cur_obj.as_mut() {
                    let v = [attr_u32(e, "v1").unwrap_or(0), attr_u32(e, "v2").unwrap_or(0), attr_u32(e, "v3").unwrap_or(0)];
                    let m = match attr_u32(e, "pid") {
                        Some(p) => Some((p, attr_u32(e, "p1").unwrap_or(0))),
                        None => match (o.default_mat, attr_u32(e, "p1")) {
                            (Some((p, _)), Some(p1)) => Some((p, p1)),
                            (d, _) => d,
                        },
                    };
                    o.triangles.push((v, m));
                }
            }
            "component" => {
                if let Some((_, o)) = cur_obj.as_mut() {
                    if let Some(oid) = attr_u32(e, "objectid") {
                        o.components.push((attr(e, "path"), oid, parse_transform(attr(e, "transform"))));
                    }
                }
            }
            "item" => {
                if let Some(oid) = attr_u32(e, "objectid") {
                    part.items.push((attr(e, "path"), oid, parse_transform(attr(e, "transform"))));
                }
            }
            _ => {}
        }
    }
    Ok(part)
}

struct Inst<'a> {
    parts: &'a HashMap<String, Part>,
    mat_index: HashMap<(String, u32, u32), usize>,
    scene: &'a mut Scene,
}

impl Inst<'_> {
    fn instantiate(&mut self, part_path: &str, oid: u32, m: Mat4, depth: u32) -> Result<()> {
        if depth > 32 {
            bail!("3MF component nesting too deep (cycle?)");
        }
        let Some(part) = self.parts.get(part_path) else {
            self.scene.info.warnings.push(format!("missing model part {part_path}"));
            return Ok(());
        };
        let Some(o) = part.objects.get(&oid) else {
            self.scene.info.warnings.push(format!("missing object {oid} in {part_path}"));
            return Ok(());
        };
        for (p, cid, cm) in &o.components {
            let pp = p.as_deref().map(norm).unwrap_or_else(|| part_path.to_string());
            self.instantiate(&pp, *cid, m * *cm, depth + 1)?;
        }
        if o.triangles.is_empty() {
            return Ok(());
        }
        // Group triangles by material so each group becomes one mesh.
        let mut groups: Vec<(Option<(u32, u32)>, Vec<[u32; 3]>)> = Vec::new();
        for (t, k) in &o.triangles {
            match groups.iter_mut().find(|g| g.0 == *k) {
                Some(g) => g.1.push(*t),
                None => groups.push((*k, vec![*t])),
            }
        }
        let single = groups.len() == 1;
        for (key, tris) in groups {
            let mut mesh = Mesh::new(o.name.clone(), Topology::Triangles);
            if single {
                mesh.positions = o.positions.clone();
                mesh.indices = tris.iter().flatten().copied().collect();
            } else {
                let mut remap = vec![u32::MAX; o.positions.len()];
                for t in &tris {
                    for &v in t {
                        let Some(slot) = remap.get_mut(v as usize) else { continue };
                        if *slot == u32::MAX {
                            *slot = mesh.positions.len() as u32;
                            mesh.positions.push(o.positions[v as usize]);
                        }
                        mesh.indices.push(*slot);
                    }
                }
            }
            if let Some(k) = key {
                if let Some(c) = part.colors.get(&k.0).and_then(|g| g.get(k.1 as usize)) {
                    let scene = &mut *self.scene;
                    let idx = *self.mat_index.entry((part_path.to_string(), k.0, k.1)).or_insert_with(|| {
                        scene.materials.push(Material {
                            name: part.names.get(&k).cloned().unwrap_or_else(|| format!("color {}:{}", k.0, k.1)),
                            base_color: *c,
                            ..Default::default()
                        });
                        scene.materials.len() - 1
                    });
                    mesh.material = Some(idx);
                }
            }
            mesh.transform(&m);
            self.scene.meshes.push(mesh);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn make_3mf(files: &[(&str, &str)]) -> Vec<u8> {
        let mut buf = Cursor::new(Vec::new());
        {
            let mut w = zip::ZipWriter::new(&mut buf);
            let o = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
            w.start_file("_rels/.rels", o).unwrap();
            w.write_all(br#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Target="/3D/3dmodel.model" Id="rel0" Type="http://schemas.microsoft.com/3dmanufacturing/2013/01/3dmodel"/></Relationships>"#).unwrap();
            for (name, body) in files {
                w.start_file(*name, o).unwrap();
                w.write_all(body.as_bytes()).unwrap();
            }
            w.finish().unwrap();
        }
        buf.into_inner()
    }

    const TRI_OBJ: &str = r#"<mesh><vertices><vertex x="0" y="0" z="0"/><vertex x="1" y="0" z="0"/><vertex x="0" y="1" z="0"/></vertices>
   <triangles><triangle v1="0" v2="1" v3="2"/></triangles></mesh>"#;

    #[test]
    fn triangle_with_item_transform_and_color() {
        let model = format!(
            r##"<?xml version="1.0" encoding="UTF-8"?>
<model unit="millimeter" xmlns="http://schemas.microsoft.com/3dmanufacturing/core/2015/02">
 <resources>
  <basematerials id="1"><base name="Red" displaycolor="#FF0000"/></basematerials>
  <object id="2" type="model" pid="1" pindex="0">{TRI_OBJ}</object>
 </resources>
 <build><item objectid="2" transform="1 0 0 0 1 0 0 0 1 10 20 30"/></build>
</model>"##
        );
        let s = parse_3mf(&make_3mf(&[("3D/3dmodel.model", &model)])).unwrap();
        assert_eq!(s.meshes.len(), 1);
        assert_eq!(s.meshes[0].positions[1], [11.0, 20.0, 30.0]);
        assert_eq!(s.materials[0].name, "Red");
        assert!((s.materials[0].base_color[0] - 1.0).abs() < 1e-6);
        assert_eq!(s.info.extra[0], ("unit".to_string(), "millimeter".to_string()));
    }

    #[test]
    fn components_are_instanced() {
        let model = format!(
            r#"<model><resources>
  <object id="1">{TRI_OBJ}</object>
  <object id="2"><components><component objectid="1"/><component objectid="1" transform="1 0 0 0 1 0 0 0 1 5 0 0"/></components></object>
 </resources><build><item objectid="2"/></build></model>"#
        );
        let s = parse_3mf(&make_3mf(&[("3D/3dmodel.model", &model)])).unwrap();
        assert_eq!(s.meshes.len(), 2);
        assert_eq!(s.meshes[1].positions[0], [5.0, 0.0, 0.0]);
    }

    #[test]
    fn production_extension_external_part() {
        let root = r#"<model xmlns:p="http://schemas.microsoft.com/3dmanufacturing/production/2015/06"><resources>
  <object id="3"><components><component p:path="/3D/Objects/object_1.model" objectid="1" transform="1 0 0 0 1 0 0 0 1 0 0 7"/></components></object>
 </resources><build><item objectid="3"/></build></model>"#;
        let obj = format!(r#"<model><resources><object id="1">{TRI_OBJ}</object></resources><build/></model>"#);
        let s = parse_3mf(&make_3mf(&[("3D/3dmodel.model", root), ("3D/Objects/object_1.model", &obj)])).unwrap();
        assert_eq!(s.meshes.len(), 1);
        assert_eq!(s.meshes[0].positions[2], [0.0, 1.0, 7.0]);
    }

    #[test]
    fn not_a_zip_is_error() {
        assert!(parse_3mf(b"definitely not zip").is_err());
    }
}
