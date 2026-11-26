//! COLLADA (.dae) loader with its own quick-xml based parser.
//!
//! The XML is read into a compact borrowed DOM (big numeric arrays stay slices of the input),
//! then interpreted: `library_geometries` meshes (triangles, polylist, polygons, trifans,
//! tristrips, lines, linestrips), the visual scene node hierarchy (matrix / translate / rotate /
//! scale / lookat, instance_node, skin and morph controllers shown in bind pose),
//! profile_COMMON materials with textures, and asset metadata (up axis, unit, authoring tool).
//! Node transforms are baked into vertices.

use super::image::{ImageSource, decode_parallel, resolve_texture_path};
use crate::scene::{AlphaMode, Material, Mesh, Scene, Topology};
use anyhow::{Result, bail};
use glam::{Mat4, Vec3};
use quick_xml::Reader;
use quick_xml::events::Event;
use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::hash::{BuildHasherDefault, Hash, Hasher};
use std::path::Path;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

const NONE: u32 = u32::MAX;
const MAX_WARNINGS: usize = 50;

pub fn load_dae(path: &Path) -> Result<Scene> {
    let data = super::map_file(path)?;
    let text = decode_text(&data);
    parse_dae(&text, path.parent())
}

/// Parse a COLLADA document. `dir` is used to resolve relative texture paths.
pub fn parse_dae(xml: &str, dir: Option<&Path>) -> Result<Scene> {
    let xml = xml.strip_prefix('\u{feff}').unwrap_or(xml);
    let doc = Doc::parse(xml)?;
    if doc.name(0) != "COLLADA" {
        bail!("not a COLLADA document (root element is <{}>)", doc.name(0));
    }
    let mut ld = Loader {
        ctx: Ctx::new(&doc, dir),
        scene: Scene::default(),
        materials: HashMap::new(),
        images: HashMap::new(),
        jobs: Vec::new(),
        insts: Vec::new(),
        controllers: HashSet::new(),
        nodes: 0,
        warned: HashSet::new(),
        suppressed: 0,
        linear_colors: false,
    };
    ld.run()?;
    Ok(ld.scene)
}

/// UTF-8 (with or without BOM) or UTF-16 with BOM.
fn decode_text(data: &[u8]) -> Cow<'_, str> {
    let utf16 = |be: bool| {
        let units: Vec<u16> = data[2..]
            .chunks_exact(2)
            .map(|c| if be { u16::from_be_bytes([c[0], c[1]]) } else { u16::from_le_bytes([c[0], c[1]]) })
            .collect();
        Cow::Owned(String::from_utf16_lossy(&units))
    };
    match data {
        [0xEF, 0xBB, 0xBF, rest @ ..] => String::from_utf8_lossy(rest),
        [0xFF, 0xFE, ..] => utf16(false),
        [0xFE, 0xFF, ..] => utf16(true),
        _ => String::from_utf8_lossy(data),
    }
}

// ---------------------------------------------------------------------------------------------
// Minimal DOM
// ---------------------------------------------------------------------------------------------

struct El<'a> {
    name: Cow<'a, str>,
    attrs: Vec<(Cow<'a, str>, Cow<'a, str>)>,
    /// Direct text content (leading whitespace-only pieces dropped).
    text: Cow<'a, str>,
    first: u32,
    next: u32,
    /// Elements are stored in document order, so descendants are `idx + 1 .. end`.
    end: u32,
}

struct Doc<'a> {
    els: Vec<El<'a>>,
}

/// Turn a string borrowed from a quick-xml event back into a slice of the input (no copy).
fn rebase<'a>(src: &'a str, s: &str) -> Cow<'a, str> {
    let (base, start) = (src.as_ptr() as usize, s.as_ptr() as usize);
    if start >= base && start + s.len() <= base + src.len() {
        if let Some(r) = src.get(start - base..start - base + s.len()) {
            return Cow::Borrowed(r);
        }
    }
    Cow::Owned(s.to_string())
}

fn append_text<'a>(text: &mut Cow<'a, str>, piece: Cow<'a, str>) {
    if text.is_empty() {
        if !piece.trim_ascii().is_empty() {
            *text = piece;
        }
    } else {
        text.to_mut().push_str(&piece);
    }
}

impl<'a> Doc<'a> {
    fn parse(xml: &'a str) -> Result<Self> {
        let mut r = Reader::from_str(xml);
        let mut els: Vec<El<'a>> = Vec::new();
        let mut stack: Vec<(u32, u32)> = Vec::new(); // (element, last child)
        loop {
            let ev = match r.read_event() {
                Ok(ev) => ev,
                Err(e) => bail!("COLLADA XML error near byte {}: {e}", r.buffer_position()),
            };
            match ev {
                Event::Start(ref e) | Event::Empty(ref e) => {
                    let idx = els.len() as u32;
                    let attrs = e
                        .attributes()
                        .flatten()
                        .map(|a| {
                            let k = rebase(xml, a.key.local_name().as_ref());
                            let v = if a.value.contains('&') {
                                let v = a.normalized_value(quick_xml::XmlVersion::Implicit1_0);
                                Cow::Owned(v.map(|v| v.into_owned()).unwrap_or_else(|_| a.value.to_string()))
                            } else {
                                rebase(xml, &a.value)
                            };
                            (k, v)
                        })
                        .collect();
                    let name = rebase(xml, e.local_name().as_ref());
                    els.push(El { name, attrs, text: Cow::Borrowed(""), first: NONE, next: NONE, end: idx + 1 });
                    match stack.last_mut() {
                        Some(top) => {
                            if top.1 == NONE {
                                els[top.0 as usize].first = idx;
                            } else {
                                els[top.1 as usize].next = idx;
                            }
                            top.1 = idx;
                        }
                        None if idx > 0 => bail!("COLLADA XML error: more than one root element"),
                        None => {}
                    }
                    if matches!(ev, Event::Start(_)) {
                        stack.push((idx, NONE));
                    }
                }
                Event::End(_) => {
                    if let Some((i, _)) = stack.pop() {
                        els[i as usize].end = els.len() as u32;
                    }
                }
                Event::Text(t) => {
                    if let Some(&(i, _)) = stack.last() {
                        append_text(&mut els[i as usize].text, t.into_inner());
                    }
                }
                Event::CData(t) => {
                    if let Some(&(i, _)) = stack.last() {
                        append_text(&mut els[i as usize].text, t.into_inner());
                    }
                }
                Event::GeneralRef(g) => {
                    if let Some(&(i, _)) = stack.last() {
                        let ch = g.resolve_char_ref().ok().flatten();
                        let raw = g.into_inner();
                        let piece = match (ch, raw.as_ref()) {
                            (Some(c), _) => c.to_string(),
                            (None, "amp") => "&".into(),
                            (None, "lt") => "<".into(),
                            (None, "gt") => ">".into(),
                            (None, "quot") => "\"".into(),
                            (None, "apos") => "'".into(),
                            (None, other) => format!("&{other};"),
                        };
                        append_text(&mut els[i as usize].text, Cow::Owned(piece));
                    }
                }
                Event::Eof => break,
                _ => {}
            }
        }
        if let Some(&(i, _)) = stack.last() {
            bail!("COLLADA XML error: unexpected end of file inside <{}>", els[i as usize].name);
        }
        if els.is_empty() {
            bail!("COLLADA XML error: document has no root element");
        }
        Ok(Doc { els })
    }

    fn name(&self, n: u32) -> &str {
        &self.els[n as usize].name
    }

    fn text(&self, n: u32) -> &str {
        &self.els[n as usize].text
    }

    fn attr(&self, n: u32, key: &str) -> Option<&str> {
        self.els[n as usize].attrs.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_ref())
    }

    fn attr_num<T: std::str::FromStr>(&self, n: u32, key: &str) -> Option<T> {
        self.attr(n, key).and_then(|v| v.trim().parse().ok())
    }

    fn children(&self, n: u32) -> impl Iterator<Item = u32> + '_ {
        let first = self.els[n as usize].first;
        std::iter::successors((first != NONE).then_some(first), move |&c| {
            let nx = self.els[c as usize].next;
            (nx != NONE).then_some(nx)
        })
    }

    fn child(&self, n: u32, name: &str) -> Option<u32> {
        self.children(n).find(|&c| self.name(c) == name)
    }

    fn kids<'s>(&'s self, n: u32, name: &'s str) -> impl Iterator<Item = u32> + 's {
        self.children(n).filter(move |&c| self.name(c) == name)
    }

    fn descendants(&self, n: u32) -> std::ops::Range<u32> {
        n + 1..self.els[n as usize].end
    }

    /// Small whitespace separated float list (transforms, colours).
    fn floats(&self, n: u32) -> Vec<f32> {
        self.text(n).split_ascii_whitespace().map(|t| t.parse().unwrap_or(0.0)).collect()
    }

    fn display_name(&self, n: u32) -> Option<&str> {
        self.attr(n, "name").or_else(|| self.attr(n, "id")).filter(|s| !s.is_empty())
    }
}

/// Parse a big whitespace separated number list, splitting it across threads when large.
fn parse_list<T: std::str::FromStr + Copy + Send>(s: &str, bad: T) -> Vec<T> {
    const PAR_MIN: usize = 4 << 20;
    let parse = move |c: &str| c.split_ascii_whitespace().map(|t| t.parse().unwrap_or(bad)).collect::<Vec<T>>();
    if s.len() < PAR_MIN {
        return parse(s);
    }
    let threads = std::thread::available_parallelism().map(|c| c.get()).unwrap_or(4).min(s.len() >> 20).max(1);
    let b = s.as_bytes();
    let mut cuts = vec![0usize];
    for i in 1..threads {
        let mut p = (s.len() * i / threads).max(*cuts.last().unwrap_or(&0));
        // Advance to an ASCII whitespace byte: always a char boundary and a token boundary.
        while p < b.len() && !b[p].is_ascii_whitespace() {
            p += 1;
        }
        cuts.push(p);
    }
    cuts.push(s.len());
    let parts: Vec<Vec<T>> = std::thread::scope(|sc| {
        let handles: Vec<_> = cuts
            .windows(2)
            .map(|w| {
                let chunk = &s[w[0]..w[1]];
                sc.spawn(move || parse(chunk))
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap_or_default()).collect()
    });
    let mut out = Vec::with_capacity(parts.iter().map(Vec::len).sum());
    for p in parts {
        out.extend_from_slice(&p);
    }
    out
}

/// COLLADA matrices are written row-major.
fn row_major(v: &[f32]) -> Option<Mat4> {
    (v.len() >= 16).then(|| Mat4::from_cols_slice(&v[..16]).transpose())
}

fn srgb_to_linear(c: f32) -> f32 {
    if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
}

fn color4(v: &[f32]) -> [f32; 4] {
    let g = |i: usize| v.get(i).copied().unwrap_or(0.0);
    [g(0), g(1), g(2), v.get(3).copied().unwrap_or(1.0)]
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
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

/// `file:///C:/a%20b.png` -> `C:/a b.png`; plain relative paths are percent-decoded too.
fn uri_to_path(s: &str) -> String {
    let s = s.trim();
    let rest = s.strip_prefix("file://").or_else(|| s.strip_prefix("file:")).unwrap_or(s);
    let mut p = percent_decode(rest);
    let b = p.as_bytes();
    if b.len() >= 3 && b[0] == b'/' && b[1].is_ascii_alphabetic() && b[2] == b':' {
        p.remove(0);
    }
    p
}

fn hex_bytes(s: &str) -> Vec<u8> {
    let val = |c: u8| (c as char).to_digit(16).unwrap_or(0) as u8;
    let digits: Vec<u8> = s.bytes().filter(u8::is_ascii_hexdigit).collect();
    digits.chunks_exact(2).map(|c| (val(c[0]) << 4) | val(c[1])).collect()
}

// ---------------------------------------------------------------------------------------------
// Shared read-only context
// ---------------------------------------------------------------------------------------------

struct Ctx<'d> {
    doc: &'d Doc<'d>,
    ids: HashMap<&'d str, u32>,
    dir: Option<&'d Path>,
}

impl<'d> Ctx<'d> {
    fn new(doc: &'d Doc<'d>, dir: Option<&'d Path>) -> Self {
        let mut ids = HashMap::new();
        for (i, el) in doc.els.iter().enumerate() {
            if let Some((_, v)) = el.attrs.iter().find(|(k, _)| k == "id") {
                ids.entry(v.as_ref()).or_insert(i as u32);
            }
        }
        Self { doc, ids, dir }
    }

    /// Resolve `#id` (or a bare id). References into other documents are not supported.
    fn by_url(&self, url: &str) -> Option<u32> {
        let url = url.trim();
        let id = match url.strip_prefix('#') {
            Some(id) => id,
            None if url.contains('#') => return None,
            None => url,
        };
        self.ids.get(id).copied()
    }

    fn by_url_named(&self, url: &str, name: &str) -> Option<u32> {
        self.by_url(url).filter(|&e| self.doc.name(e) == name)
    }
}

// ---------------------------------------------------------------------------------------------
// Geometry
// ---------------------------------------------------------------------------------------------

#[derive(Clone)]
struct GeomPrim {
    mesh: Mesh,
    symbol: Option<String>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Triangles,
    Polylist,
    Polygons,
    Fans,
    Strips,
    Lines,
    LineStrips,
}

struct Accessor {
    array: u32,
    count: usize,
    stride: usize,
    offset: usize,
    comps: Vec<usize>,
}

fn accessor(ctx: &Ctx, source: u32) -> Option<Accessor> {
    let doc = ctx.doc;
    let is_array = |e: &u32| matches!(doc.name(*e), "float_array" | "int_array");
    let acc = doc.child(source, "technique_common").and_then(|t| doc.child(t, "accessor"))?;
    let array = doc.attr(acc, "source").and_then(|u| ctx.by_url(u)).filter(is_array).or_else(|| doc.children(source).find(is_array))?;
    let stride = doc.attr_num(acc, "stride").unwrap_or(1usize).max(1);
    if stride > 1024 {
        return None;
    }
    let params: Vec<u32> = doc.kids(acc, "param").collect();
    let mut comps: Vec<usize> =
        params.iter().enumerate().filter(|(_, p)| doc.attr(**p, "name").is_some_and(|n| !n.is_empty())).map(|(i, _)| i).collect();
    if comps.is_empty() {
        comps = (0..if params.is_empty() { stride } else { params.len().min(1024) }).collect();
    }
    Some(Accessor {
        array,
        count: doc.attr_num(acc, "count").unwrap_or(0),
        stride,
        offset: doc.attr_num(acc, "offset").unwrap_or(0),
        comps,
    })
}

/// Accessor bound to its parsed array; `count` is clamped so every read is in bounds.
struct View<'x> {
    data: &'x [f32],
    count: usize,
    stride: usize,
    offset: usize,
    comps: Vec<usize>,
}

impl<'x> View<'x> {
    fn new(data: &'x [f32], a: &Accessor) -> Self {
        let maxc = a.comps.iter().copied().max().unwrap_or(0);
        let need = a.offset.saturating_add(maxc);
        let avail = if data.len() > need { (data.len() - need - 1) / a.stride + 1 } else { 0 };
        let count = if a.count == 0 { avail } else { a.count.min(avail) };
        Self { data, count, stride: a.stride, offset: a.offset, comps: a.comps.clone() }
    }

    fn get(&self, i: usize, k: usize) -> f32 {
        match self.comps.get(k) {
            Some(&c) => self.data[self.offset + i * self.stride + c],
            None => 0.0,
        }
    }
}

struct Attr<'x> {
    off: usize,
    view: View<'x>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct Key([u32; 4]);

impl Hash for Key {
    fn hash<H: Hasher>(&self, h: &mut H) {
        h.write_u64(self.0[0] as u64 | (self.0[1] as u64) << 32);
        h.write_u64(self.0[2] as u64 | (self.0[3] as u64) << 32);
    }
}

/// FxHash-style hasher for the corner dedup map (std SipHash is slow for millions of keys).
#[derive(Default)]
struct Fx(u64);

impl Hasher for Fx {
    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.write_u64(b as u64);
        }
    }
    fn write_u64(&mut self, w: u64) {
        self.0 = (self.0.rotate_left(5) ^ w).wrapping_mul(0xf135_7aea_2e62_a9c5);
    }
    fn finish(&self) -> u64 {
        self.0.rotate_left(26)
    }
}

struct Builder<'x> {
    idx: &'x [u32],
    stride: usize,
    pos: Attr<'x>,
    nrm: Option<Attr<'x>>,
    uv: Option<Attr<'x>>,
    col: Option<Attr<'x>>,
    /// Fast path when every used input shares one offset: remap by that index.
    remap: Option<Vec<u32>>,
    map: HashMap<Key, u32, BuildHasherDefault<Fx>>,
    mesh: Mesh,
    bad: usize,
}

impl Builder<'_> {
    /// Output vertex for a corner, or NONE when one of its indices is out of range.
    fn vertex(&mut self, corner: usize) -> u32 {
        let b = corner * self.stride;
        let at = |a: &Option<Attr>| a.as_ref().map_or(0, |a| self.idx[b + a.off]);
        let key = Key([self.idx[b + self.pos.off], at(&self.nrm), at(&self.uv), at(&self.col)]);
        if let Some(r) = &self.remap {
            match r.get(key.0[0] as usize) {
                Some(&v) if v != NONE - 1 => return v,
                Some(_) => {}
                None => return NONE,
            }
        } else if let Some(&v) = self.map.get(&key) {
            return v;
        }
        let ok = |a: &Option<Attr>, i: u32| a.as_ref().is_none_or(|a| (i as usize) < a.view.count);
        let valid =
            (key.0[0] as usize) < self.pos.view.count && ok(&self.nrm, key.0[1]) && ok(&self.uv, key.0[2]) && ok(&self.col, key.0[3]);
        let v = if valid {
            let m = &mut self.mesh;
            let i = m.positions.len() as u32;
            let (p, k) = (&self.pos.view, key.0[0] as usize);
            m.positions.push([p.get(k, 0), p.get(k, 1), p.get(k, 2)]);
            if let Some(a) = &self.nrm {
                let (n, k) = (&a.view, key.0[1] as usize);
                m.normals.push([n.get(k, 0), n.get(k, 1), n.get(k, 2)]);
            }
            if let Some(a) = &self.uv {
                let (t, k) = (&a.view, key.0[2] as usize);
                m.uvs.push([t.get(k, 0), 1.0 - t.get(k, 1)]);
            }
            if let Some(a) = &self.col {
                let (c, k) = (&a.view, key.0[3] as usize);
                let u = |x: f32| (x.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
                let alpha = if c.comps.len() >= 4 { u(c.get(k, 3)) } else { 255 };
                m.colors.push([u(c.get(k, 0)), u(c.get(k, 1)), u(c.get(k, 2)), alpha]);
            }
            i
        } else {
            NONE
        };
        match &mut self.remap {
            Some(r) => r[key.0[0] as usize] = v,
            None => {
                self.map.insert(key, v);
            }
        }
        v
    }

    fn push(&mut self, corners: &[usize]) {
        let mut v = [0u32; 3];
        for (o, &c) in v.iter_mut().zip(corners) {
            *o = self.vertex(c);
        }
        let v = &v[..corners.len()];
        if v.contains(&NONE) {
            self.bad += 1;
        } else {
            self.mesh.indices.extend_from_slice(v);
        }
    }

    fn polygon(&mut self, start: usize, n: usize) {
        match n {
            0..=2 => {}
            3 => self.push(&[start, start + 1, start + 2]),
            4 => {
                let v = [self.vertex(start), self.vertex(start + 1), self.vertex(start + 2), self.vertex(start + 3)];
                if v.contains(&NONE) {
                    self.bad += 1;
                    return;
                }
                let p = v.map(|i| Vec3::from(self.mesh.positions[i as usize]));
                self.mesh.indices.extend(split_quad(&p).iter().flatten().map(|&i| v[i]));
            }
            _ => {
                let vs: Vec<u32> = (start..start + n).map(|c| self.vertex(c)).collect();
                if vs.contains(&NONE) {
                    self.bad += 1;
                    return;
                }
                let pts: Vec<Vec3> = vs.iter().map(|&i| Vec3::from(self.mesh.positions[i as usize])).collect();
                let tris = if n <= 64 { ear_clip(&pts) } else { None };
                match tris {
                    Some(t) => self.mesh.indices.extend(t.iter().flatten().map(|&i| vs[i])),
                    None => self.mesh.indices.extend((2..n).flat_map(|i| [vs[0], vs[i - 1], vs[i]])),
                }
            }
        }
    }
}

/// Split a quad along the diagonal that keeps both triangles facing the quad normal.
fn split_quad(p: &[Vec3; 4]) -> [[usize; 3]; 2] {
    let n = (p[2] - p[0]).cross(p[3] - p[1]);
    let fan_ok = (p[1] - p[0]).cross(p[2] - p[0]).dot(n) > 0.0 && (p[2] - p[0]).cross(p[3] - p[0]).dot(n) > 0.0;
    if fan_ok || n == Vec3::ZERO { [[0, 1, 2], [0, 2, 3]] } else { [[1, 2, 3], [1, 3, 0]] }
}

/// Ear clipping in the polygon plane; None when the polygon is degenerate.
fn ear_clip(pts: &[Vec3]) -> Option<Vec<[usize; 3]>> {
    let n = pts.len();
    let mut nrm = Vec3::ZERO;
    for i in 0..n {
        nrm += pts[i].cross(pts[(i + 1) % n]);
    }
    let nrm = nrm.try_normalize()?;
    let u = nrm.any_orthonormal_vector();
    let v = nrm.cross(u);
    let q: Vec<[f32; 2]> = pts.iter().map(|p| [p.dot(u), p.dot(v)]).collect();
    let cross = |o: [f32; 2], a: [f32; 2], b: [f32; 2]| (a[0] - o[0]) * (b[1] - o[1]) - (a[1] - o[1]) * (b[0] - o[0]);
    let mut rem: Vec<usize> = (0..n).collect();
    let mut out = Vec::with_capacity(n - 2);
    let (mut i, mut misses) = (0usize, 0usize);
    while rem.len() > 3 {
        let m = rem.len();
        let (ia, ib, ic) = (rem[(i + m - 1) % m], rem[i], rem[(i + 1) % m]);
        let (a, b, c) = (q[ia], q[ib], q[ic]);
        let ear = cross(a, b, c) > 0.0
            && !rem.iter().any(|&j| {
                j != ia && j != ib && j != ic && {
                    let p = q[j];
                    cross(a, b, p) >= 0.0 && cross(b, c, p) >= 0.0 && cross(c, a, p) >= 0.0
                }
            });
        if ear {
            out.push([ia, ib, ic]);
            rem.remove(i);
            if i >= rem.len() {
                i = 0;
            }
            misses = 0;
        } else {
            i = (i + 1) % m;
            misses += 1;
            if misses > m {
                return None;
            }
        }
    }
    out.push([rem[0], rem[1], rem[2]]);
    Some(out)
}

struct Input<'d> {
    semantic: &'d str,
    source: u32,
    offset: usize,
    set: u32,
}

fn build_geometry(ctx: &Ctx, geom: u32, warn: &mut Vec<String>) -> Vec<GeomPrim> {
    let doc = ctx.doc;
    let gname = doc.display_name(geom).unwrap_or("geometry");
    let Some(mesh_el) = doc.child(geom, "mesh").or_else(|| doc.child(geom, "convex_mesh")) else {
        if let Some(c) = doc.children(geom).find(|&c| matches!(doc.name(c), "spline" | "brep" | "convex_mesh")) {
            warn.push(format!("{gname}: <{}> geometry is not supported", doc.name(c)));
        }
        return Vec::new();
    };
    let mut arrays: HashMap<u32, Vec<f32>> = HashMap::new();
    let mut out = Vec::new();
    for prim in doc.children(mesh_el) {
        let kind = match doc.name(prim) {
            "triangles" => Kind::Triangles,
            "polylist" => Kind::Polylist,
            "polygons" => Kind::Polygons,
            "trifans" => Kind::Fans,
            "tristrips" => Kind::Strips,
            "lines" => Kind::Lines,
            "linestrips" => Kind::LineStrips,
            _ => continue,
        };
        if let Some(p) = build_prim(ctx, gname, prim, kind, &mut arrays, warn) {
            out.push(p);
        }
    }
    out
}

fn build_prim(
    ctx: &Ctx,
    gname: &str,
    prim: u32,
    kind: Kind,
    arrays: &mut HashMap<u32, Vec<f32>>,
    warn: &mut Vec<String>,
) -> Option<GeomPrim> {
    let doc = ctx.doc;
    let pname = doc.name(prim);

    // Inputs; VERTEX expands to every input of the <vertices> element at the same offset.
    let mut inputs: Vec<Input> = Vec::new();
    let mut stride = 1usize;
    for inp in doc.kids(prim, "input") {
        let semantic = doc.attr(inp, "semantic").unwrap_or("");
        let offset = doc.attr_num(inp, "offset").unwrap_or(0usize);
        let set = doc.attr_num(inp, "set").unwrap_or(0u32);
        stride = stride.max(offset.saturating_add(1));
        let url = doc.attr(inp, "source").unwrap_or("");
        let target = ctx.by_url(url);
        match (semantic, target) {
            ("VERTEX", Some(v)) if doc.name(v) == "vertices" => {
                for vi in doc.kids(v, "input") {
                    if let Some(source) = doc.attr(vi, "source").and_then(|u| ctx.by_url_named(u, "source")) {
                        let semantic = doc.attr(vi, "semantic").unwrap_or("");
                        let set = doc.attr_num(vi, "set").unwrap_or(set);
                        inputs.push(Input { semantic, source, offset, set });
                    }
                }
            }
            ("VERTEX", Some(s)) if doc.name(s) == "source" => inputs.push(Input { semantic: "POSITION", source: s, offset, set }),
            (_, Some(s)) if doc.name(s) == "source" => inputs.push(Input { semantic, source: s, offset, set }),
            _ => warn.push(format!("{gname}: unresolved {semantic} input '{url}'")),
        }
    }
    if stride > 256 {
        warn.push(format!("{gname}: <{pname}> input offsets are implausible, skipped"));
        return None;
    }
    let pick = |sem: &str| inputs.iter().filter(|i| i.semantic == sem).min_by_key(|i| i.set);
    let Some(pos_in) = pick("POSITION") else {
        warn.push(format!("{gname}: <{pname}> has no POSITION input, skipped"));
        return None;
    };
    let Some(pos_acc) = accessor(ctx, pos_in.source) else {
        warn.push(format!("{gname}: POSITION source has no readable float data"));
        return None;
    };
    let opt = |i: Option<&Input>| i.and_then(|i| accessor(ctx, i.source).map(|a| (i.offset, a)));
    let nrm_acc = opt(pick("NORMAL"));
    let uv_acc = opt(pick("TEXCOORD"));
    let col_acc = opt(pick("COLOR"));
    for a in [Some(&pos_acc), nrm_acc.as_ref().map(|x| &x.1), uv_acc.as_ref().map(|x| &x.1), col_acc.as_ref().map(|x| &x.1)]
        .into_iter()
        .flatten()
    {
        arrays.entry(a.array).or_insert_with(|| parse_list(doc.text(a.array), 0.0f32));
    }

    // Index data: one <p> for triangles/lines/polylist, one <p> per polygon/fan/strip otherwise.
    let mut idx: Vec<u32> = Vec::new();
    let mut groups: Vec<(usize, usize)> = Vec::new(); // (first corner, corner count)
    let mut holes = false;
    for c in doc.children(prim) {
        let p = match doc.name(c) {
            "p" => c,
            "ph" if kind == Kind::Polygons => {
                holes |= doc.child(c, "h").is_some();
                match doc.child(c, "p") {
                    Some(p) => p,
                    None => continue,
                }
            }
            _ => continue,
        };
        let start = idx.len();
        let v = parse_list::<u32>(doc.text(p), NONE);
        if idx.is_empty() {
            idx = v;
        } else {
            idx.extend_from_slice(&v);
        }
        let n = idx.len() - start;
        if n % stride != 0 {
            warn.push(format!("{gname}: <{pname}> index count is not a multiple of the input count"));
            idx.truncate(start + n / stride * stride);
        }
        groups.push((start / stride, (idx.len() - start) / stride));
    }
    if holes {
        warn.push(format!("{gname}: polygon holes are ignored"));
    }
    let ncorners = idx.len() / stride;
    if kind == Kind::Polylist {
        groups.clear();
        let vcount = doc.child(prim, "vcount").map(|v| parse_list::<u32>(doc.text(v), 0)).unwrap_or_default();
        let mut s = 0usize;
        for n in vcount {
            let n = n as usize;
            if s + n > ncorners {
                warn.push(format!("{gname}: <vcount> refers past the end of the index data"));
                break;
            }
            groups.push((s, n));
            s += n;
        }
    }
    if ncorners == 0 {
        return None;
    }

    let view = |a: &Accessor| View::new(&arrays[&a.array], a);
    let attr = |x: &Option<(usize, Accessor)>| x.as_ref().map(|(off, a)| Attr { off: *off, view: view(a) });
    let pos = Attr { off: pos_in.offset, view: view(&pos_acc) };
    let (nrm, uv, col) = (attr(&nrm_acc), attr(&uv_acc), attr(&col_acc));
    let single = [&nrm, &uv, &col].iter().all(|a| a.as_ref().is_none_or(|a| a.off == pos.off));
    let topology = if matches!(kind, Kind::Lines | Kind::LineStrips) { Topology::Lines } else { Topology::Triangles };
    let mut b = Builder {
        idx: &idx,
        stride,
        remap: single.then(|| vec![NONE - 1; pos.view.count]),
        map: HashMap::default(),
        mesh: Mesh::new(gname, topology),
        bad: 0,
        pos,
        nrm,
        uv,
        col,
    };
    b.mesh.indices.reserve(ncorners);
    for &(s, n) in &groups {
        match kind {
            Kind::Triangles => (0..n / 3).for_each(|t| b.push(&[s + 3 * t, s + 3 * t + 1, s + 3 * t + 2])),
            Kind::Lines => (0..n / 2).for_each(|t| b.push(&[s + 2 * t, s + 2 * t + 1])),
            Kind::Polylist | Kind::Polygons => b.polygon(s, n),
            Kind::Fans => (2..n).for_each(|i| b.push(&[s, s + i - 1, s + i])),
            Kind::Strips => (2..n)
                .for_each(|i| if i % 2 == 0 { b.push(&[s + i - 2, s + i - 1, s + i]) } else { b.push(&[s + i - 1, s + i - 2, s + i]) }),
            Kind::LineStrips => (1..n).for_each(|i| b.push(&[s + i - 1, s + i])),
        }
    }
    if b.bad > 0 {
        warn.push(format!("{gname}: {} primitives with out-of-range indices skipped", b.bad));
    }
    if b.mesh.indices.is_empty() {
        return None;
    }
    let mut mesh = b.mesh;
    mesh.had_normals = !mesh.normals.is_empty();
    Some(GeomPrim { mesh, symbol: doc.attr(prim, "material").map(String::from) })
}

fn build_parallel(ctx: &Ctx, geoms: &[u32]) -> Vec<(Vec<GeomPrim>, Vec<String>)> {
    let n = geoms.len();
    let build = |g: u32| {
        let mut w = Vec::new();
        let r = build_geometry(ctx, g, &mut w);
        (r, w)
    };
    let threads = std::thread::available_parallelism().map(|c| c.get()).unwrap_or(4).min(n);
    if threads <= 1 {
        return geoms.iter().map(|&g| build(g)).collect();
    }
    let out: Vec<Mutex<Option<(Vec<GeomPrim>, Vec<String>)>>> = (0..n).map(|_| Mutex::new(None)).collect();
    let next = AtomicUsize::new(0);
    std::thread::scope(|s| {
        for _ in 0..threads {
            s.spawn(|| {
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    if i >= n {
                        break;
                    }
                    let r = build(geoms[i]);
                    if let Ok(mut slot) = out[i].lock() {
                        *slot = Some(r);
                    }
                }
            });
        }
    });
    out.into_iter().map(|m| m.into_inner().ok().flatten().unwrap_or_default()).collect()
}

// ---------------------------------------------------------------------------------------------
// Scene assembly
// ---------------------------------------------------------------------------------------------

struct Inst {
    geom: u32,
    world: Mat4,
    /// (material symbol, material url)
    bind: Vec<(String, String)>,
    name: String,
}

struct Loader<'d> {
    ctx: Ctx<'d>,
    scene: Scene,
    /// material element -> scene material index
    materials: HashMap<u32, usize>,
    /// image element -> decode job slot
    images: HashMap<u32, Option<usize>>,
    jobs: Vec<(String, ImageSource)>,
    insts: Vec<Inst>,
    controllers: HashSet<u32>,
    nodes: usize,
    warned: HashSet<String>,
    suppressed: usize,
    /// Blender writes linear colour values; other exporters write sRGB.
    linear_colors: bool,
}

impl Loader<'_> {
    fn warn(&mut self, msg: impl Into<String>) {
        let msg = msg.into();
        if self.warned.contains(&msg) {
            return;
        }
        if self.scene.info.warnings.len() >= MAX_WARNINGS {
            self.suppressed += 1;
            return;
        }
        self.warned.insert(msg.clone());
        self.scene.info.warnings.push(msg);
    }

    fn run(&mut self) -> Result<()> {
        let doc = self.ctx.doc;
        let up = self.read_asset();
        let vs = doc
            .child(0, "scene")
            .and_then(|s| doc.child(s, "instance_visual_scene"))
            .and_then(|i| doc.attr(i, "url"))
            .and_then(|u| self.ctx.by_url_named(u, "visual_scene"))
            .or_else(|| doc.kids(0, "library_visual_scenes").find_map(|l| doc.child(l, "visual_scene")));
        if let Some(vs) = vs {
            for n in doc.kids(vs, "node") {
                self.visit(n, up, 0);
            }
        }
        if self.insts.is_empty() {
            let geoms: Vec<u32> = doc.kids(0, "library_geometries").flat_map(|l| doc.kids(l, "geometry")).collect();
            if vs.is_some() && !geoms.is_empty() {
                self.warn("visual scene instantiates no geometry; showing every geometry once");
            }
            for geom in geoms {
                self.insts.push(Inst { geom, world: up, bind: Vec::new(), name: String::new() });
            }
        }
        self.instantiate();
        self.decode_textures();
        self.read_animations();
        self.scene.info.nodes = self.nodes;
        if self.suppressed > 0 {
            let n = self.suppressed;
            self.scene.info.warnings.push(format!("... {n} more warnings"));
        }
        if self.scene.meshes.is_empty() {
            bail!("COLLADA file contains no drawable geometry");
        }
        Ok(())
    }

    /// Asset metadata; returns the root transform (X_UP is rotated to Y-up).
    fn read_asset(&mut self) -> Mat4 {
        let doc = self.ctx.doc;
        let info = &mut self.scene.info;
        info.format = match doc.attr(0, "version") {
            Some(v) => format!("COLLADA {v}"),
            None => "COLLADA".into(),
        };
        let Some(asset) = doc.child(0, "asset") else { return Mat4::IDENTITY };
        let tool = doc
            .kids(asset, "contributor")
            .filter_map(|c| doc.child(c, "authoring_tool"))
            .map(|t| doc.text(t).trim())
            .find(|t| !t.is_empty());
        if let Some(t) = tool {
            info.extra.push(("authoring tool".into(), t.to_string()));
            self.linear_colors = t.contains("Blender");
        }
        // COLLADA defaults to one metre per unit.
        info.unit_m = Some(1.0);
        if let Some(u) = doc.child(asset, "unit") {
            let name = doc.attr(u, "name").unwrap_or("meter");
            let meter = doc.attr(u, "meter").unwrap_or("1");
            info.extra.push(("unit".into(), format!("{name} ({meter} m)")));
            if let Some(m) = meter.trim().parse::<f64>().ok().filter(|m| m.is_finite() && *m > 0.0) {
                info.unit_m = Some(m);
            }
        }
        match doc.child(asset, "up_axis").map(|a| doc.text(a).trim()) {
            Some("Z_UP") => info.up_axis_z = true,
            Some("X_UP") => {
                self.warn("X_UP model rotated to Y-up");
                return Mat4::from_rotation_z(std::f32::consts::FRAC_PI_2);
            }
            _ => {}
        }
        Mat4::IDENTITY
    }

    fn visit(&mut self, node: u32, parent: Mat4, depth: u32) {
        let doc = self.ctx.doc;
        if depth > 64 {
            self.warn("node hierarchy too deep (instance_node cycle?), truncated");
            return;
        }
        self.nodes += 1;
        // Transform elements compose in document order.
        let mut m = parent;
        for c in doc.children(node) {
            let v = || doc.floats(c);
            let t = match doc.name(c) {
                "matrix" => row_major(&v()),
                "translate" => match v()[..] {
                    [x, y, z, ..] => Some(Mat4::from_translation(Vec3::new(x, y, z))),
                    _ => None,
                },
                "rotate" => match v()[..] {
                    [x, y, z, a, ..] => Vec3::new(x, y, z).try_normalize().map(|ax| Mat4::from_axis_angle(ax, a.to_radians())),
                    _ => None,
                },
                "scale" => match v()[..] {
                    [x, y, z, ..] => Some(Mat4::from_scale(Vec3::new(x, y, z))),
                    _ => None,
                },
                "lookat" => match v()[..] {
                    [ex, ey, ez, ix, iy, iz, ux, uy, uz, ..] => {
                        // Inverse of a right-handed view matrix: the node looks down its -Z axis.
                        let eye = Vec3::new(ex, ey, ez);
                        let z = (eye - Vec3::new(ix, iy, iz)).try_normalize();
                        let x = z.and_then(|z| Vec3::new(ux, uy, uz).cross(z).try_normalize());
                        z.zip(x).map(|(z, x)| Mat4::from_cols(x.extend(0.0), z.cross(x).extend(0.0), z.extend(0.0), eye.extend(1.0)))
                    }
                    _ => None,
                },
                "skew" => {
                    self.warn("<skew> transforms are ignored");
                    None
                }
                _ => None,
            };
            if let Some(t) = t {
                m *= t;
            }
        }
        let name = doc.display_name(node).unwrap_or("").to_string();
        for c in doc.children(node) {
            match doc.name(c) {
                "instance_geometry" => {
                    let url = doc.attr(c, "url").unwrap_or("");
                    match self.ctx.by_url_named(url, "geometry") {
                        Some(geom) => {
                            let bind = self.bindings(c);
                            self.insts.push(Inst { geom, world: m, bind, name: name.clone() });
                        }
                        None => self.warn(format!("node '{name}': geometry '{url}' not found")),
                    }
                }
                "instance_controller" => {
                    let url = doc.attr(c, "url").unwrap_or("");
                    let resolved = self.ctx.by_url_named(url, "controller").and_then(|ct| self.resolve_controller(ct, 0));
                    match resolved {
                        // Bind pose. node * bind_shape_matrix matches what Blender and the
                        // common DCC exporters expect when joints are left at their bind pose.
                        Some((geom, bsm)) => {
                            let bind = self.bindings(c);
                            self.insts.push(Inst { geom, world: m * bsm, bind, name: name.clone() });
                        }
                        None => self.warn(format!("node '{name}': controller '{url}' could not be resolved")),
                    }
                }
                "instance_node" => {
                    let url = doc.attr(c, "url").unwrap_or("");
                    match self.ctx.by_url_named(url, "node") {
                        Some(t) => self.visit(t, m, depth + 1),
                        None => self.warn(format!("instance_node '{url}' not found (external references are not supported)")),
                    }
                }
                "node" => self.visit(c, m, depth + 1),
                "instance_camera" => self.scene.info.cameras += 1,
                "instance_light" => self.scene.info.lights += 1,
                _ => {}
            }
        }
    }

    fn bindings(&self, inst: u32) -> Vec<(String, String)> {
        let doc = self.ctx.doc;
        let Some(bm) = doc.child(inst, "bind_material") else { return Vec::new() };
        doc.descendants(bm)
            .filter(|&e| doc.name(e) == "instance_material")
            .filter_map(|e| Some((doc.attr(e, "symbol")?.to_string(), doc.attr(e, "target")?.to_string())))
            .collect()
    }

    /// Controller -> (base geometry, bind shape matrix). Skins may wrap morphs.
    fn resolve_controller(&mut self, ctrl: u32, depth: u32) -> Option<(u32, Mat4)> {
        let doc = self.ctx.doc;
        if depth > 8 {
            return None;
        }
        let first = self.controllers.insert(ctrl);
        if let Some(skin) = doc.child(ctrl, "skin") {
            let bsm = doc.child(skin, "bind_shape_matrix").and_then(|b| row_major(&doc.floats(b))).unwrap_or(Mat4::IDENTITY);
            if first {
                self.scene.info.skins += 1;
                let joints = doc
                    .child(skin, "joints")
                    .and_then(|j| doc.kids(j, "input").find(|&i| doc.attr(i, "semantic") == Some("JOINT")))
                    .and_then(|i| doc.attr(i, "source"))
                    .and_then(|u| self.ctx.by_url_named(u, "source"))
                    .and_then(|s| doc.children(s).find(|&a| matches!(doc.name(a), "Name_array" | "IDREF_array")))
                    .map(|a| doc.text(a).split_ascii_whitespace().count())
                    .unwrap_or(0);
                self.scene.info.bones += joints;
            }
            let src = doc.attr(skin, "source").and_then(|u| self.ctx.by_url(u))?;
            return match doc.name(src) {
                "geometry" => Some((src, bsm)),
                "controller" => self.resolve_controller(src, depth + 1).map(|(g, m)| (g, bsm * m)),
                _ => None,
            };
        }
        if let Some(morph) = doc.child(ctrl, "morph") {
            if first {
                let targets = doc
                    .child(morph, "targets")
                    .and_then(|t| doc.kids(t, "input").find(|&i| doc.attr(i, "semantic") == Some("MORPH_TARGET")))
                    .and_then(|i| doc.attr(i, "source"))
                    .and_then(|u| self.ctx.by_url_named(u, "source"))
                    .and_then(|s| doc.children(s).find(|&a| matches!(doc.name(a), "IDREF_array" | "Name_array")))
                    .map(|a| doc.text(a).split_ascii_whitespace().count())
                    .unwrap_or(0);
                self.scene.info.morph_targets += targets;
            }
            let src = doc.attr(morph, "source").and_then(|u| self.ctx.by_url(u))?;
            return match doc.name(src) {
                "geometry" => Some((src, Mat4::IDENTITY)),
                "controller" => self.resolve_controller(src, depth + 1),
                _ => None,
            };
        }
        None
    }

    /// Build each referenced geometry once (in parallel), then bake one copy per instance.
    fn instantiate(&mut self) {
        let insts = std::mem::take(&mut self.insts);
        let mut uses: HashMap<u32, usize> = HashMap::new();
        let mut unique = Vec::new();
        for i in &insts {
            let c = uses.entry(i.geom).or_insert(0);
            if *c == 0 {
                unique.push(i.geom);
            }
            *c += 1;
        }
        let built = build_parallel(&self.ctx, &unique);
        let mut store: HashMap<u32, Vec<GeomPrim>> = HashMap::new();
        for (&g, (prims, warns)) in unique.iter().zip(built) {
            for w in warns {
                self.warn(w);
            }
            store.insert(g, prims);
        }
        for inst in insts {
            let left = uses.get_mut(&inst.geom).map(|c| {
                *c -= 1;
                *c
            });
            let prims = if left == Some(0) {
                store.remove(&inst.geom).unwrap_or_default()
            } else {
                store.get(&inst.geom).cloned().unwrap_or_default()
            };
            let n = prims.len();
            for (i, p) in prims.into_iter().enumerate() {
                let mut mesh = p.mesh;
                let base = if inst.name.is_empty() { std::mem::take(&mut mesh.name) } else { inst.name.clone() };
                mesh.name = if n > 1 { format!("{base} [{i}]") } else { base };
                mesh.material = self.resolve_material(&inst.bind, p.symbol.as_deref());
                mesh.transform(&inst.world);
                self.scene.meshes.push(mesh);
            }
        }
    }

    fn resolve_material(&mut self, bind: &[(String, String)], symbol: Option<&str>) -> Option<usize> {
        let sym = symbol?;
        let doc = self.ctx.doc;
        let el = bind
            .iter()
            .find(|(s, _)| s == sym)
            .and_then(|(_, t)| self.ctx.by_url_named(t, "material"))
            .or_else(|| self.ctx.by_url_named(sym, "material"))
            .or_else(|| doc.kids(0, "library_materials").flat_map(|l| doc.kids(l, "material")).find(|&m| doc.attr(m, "name") == Some(sym)));
        match el {
            Some(e) => Some(self.material(e)),
            None => {
                self.warn(format!("material symbol '{sym}' is not bound to a material"));
                None
            }
        }
    }

    fn material(&mut self, el: u32) -> usize {
        if let Some(&i) = self.materials.get(&el) {
            return i;
        }
        let m = self.build_material(el);
        self.scene.materials.push(m);
        let i = self.scene.materials.len() - 1;
        self.materials.insert(el, i);
        i
    }

    fn build_material(&mut self, mat: u32) -> Material {
        let doc = self.ctx.doc;
        let name = doc.display_name(mat).unwrap_or("material").to_string();
        let mut out = Material { name: name.clone(), ..Default::default() };
        let effect = doc.child(mat, "instance_effect").and_then(|ie| doc.attr(ie, "url")).and_then(|u| self.ctx.by_url_named(u, "effect"));
        let Some(effect) = effect else {
            self.warn(format!("material '{name}': effect not found"));
            return out;
        };
        let Some(profile) = doc.child(effect, "profile_COMMON") else {
            self.warn(format!("material '{name}': no profile_COMMON effect, using default look"));
            return out;
        };
        let shader = doc
            .child(profile, "technique")
            .and_then(|t| doc.children(t).find(|&c| matches!(doc.name(c), "phong" | "lambert" | "blinn" | "constant")));
        let Some(shader) = shader else { return out };
        let kind = doc.name(shader);
        let param = |n: &str| doc.child(shader, n);

        if let Some(d) = param("diffuse") {
            let (c, t) = self.color_or_texture(effect, d);
            if let Some(c) = c {
                let lin = |x: f32| if self.linear_colors { x } else { srgb_to_linear(x) };
                out.base_color = [lin(c[0]), lin(c[1]), lin(c[2]), 1.0];
            }
            if t.is_some() {
                out.base_color = [1.0; 4];
                out.base_color_tex = t;
            }
        }
        if let Some(e) = param("emission") {
            let (c, t) = self.color_or_texture(effect, e);
            if let Some(c) = c {
                let lin = |x: f32| if self.linear_colors { x } else { srgb_to_linear(x) };
                out.emissive = [lin(c[0]), lin(c[1]), lin(c[2])];
            }
            if t.is_some() {
                out.emissive = [1.0; 3];
                out.emissive_tex = t;
            }
        }
        if kind == "constant" {
            // Unlit: show the emission colour as the surface colour.
            let e = out.emissive;
            out.base_color = [e[0], e[1], e[2], 1.0];
            out.base_color_tex = out.emissive_tex.take();
            out.emissive = [0.0; 3];
        }
        if matches!(kind, "phong" | "blinn") {
            let s = param("shininess").and_then(|s| doc.child(s, "float")).and_then(|f| doc.floats(f).first().copied());
            if let Some(s) = s.filter(|&s| s > 1.0) {
                out.roughness = (2.0 / (s + 2.0)).sqrt().clamp(0.05, 1.0);
            }
        }

        // Transparency (A_ONE default). Modes per COLLADA spec; missing parts default like most importers.
        let transparent = param("transparent");
        let transparency = param("transparency").and_then(|t| doc.child(t, "float")).and_then(|f| doc.floats(f).first().copied());
        let mut alpha = 1.0f32;
        let mut blend = false;
        if transparent.is_some() || transparency.is_some() {
            let f = transparency.unwrap_or(1.0);
            let (tc, tt) = match transparent {
                Some(t) => self.color_or_texture(effect, t),
                None => (None, None),
            };
            if tt.is_some() {
                blend = true;
            } else {
                let c = tc.unwrap_or([1.0; 4]);
                let lum = 0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2];
                alpha = match transparent.and_then(|t| doc.attr(t, "opaque")).unwrap_or("A_ONE") {
                    "RGB_ZERO" => 1.0 - lum * f,
                    "A_ZERO" => 1.0 - c[3] * f,
                    "RGB_ONE" => lum * f,
                    _ => c[3] * f,
                };
            }
        }
        let mut alpha = alpha.clamp(0.0, 1.0);
        if alpha <= 0.001 {
            self.warn(format!("material '{name}': fully transparent, assuming inverted transparency (shown opaque)"));
            alpha = 1.0;
        }
        out.base_color[3] = alpha;
        if alpha < 0.999 || blend {
            out.alpha_mode = AlphaMode::Blend;
        }

        // Vendor extras: <double_sided>1</double_sided>, FCOLLADA/OpenCOLLADA <bump> normal maps.
        for d in doc.descendants(effect) {
            match doc.name(d) {
                "double_sided" if matches!(doc.text(d).trim(), "1" | "true") => out.double_sided = true,
                "bump" if out.normal_tex.is_none() => {
                    let normal_map = doc.attr(d, "bumptype").is_none_or(|t| t.eq_ignore_ascii_case("NORMALMAP"));
                    if let Some(t) = doc.child(d, "texture").filter(|_| normal_map) {
                        out.normal_tex = doc.attr(t, "texture").and_then(|s| self.texture_slot(effect, s));
                    }
                }
                _ => {}
            }
        }
        out
    }

    /// `<color>`, `<texture>` or `<param ref>` child of a shading parameter.
    fn color_or_texture(&mut self, effect: u32, el: u32) -> (Option<[f32; 4]>, Option<usize>) {
        let doc = self.ctx.doc;
        if let Some(c) = doc.child(el, "color") {
            return (Some(color4(&doc.floats(c))), None);
        }
        if let Some(t) = doc.child(el, "texture") {
            let slot = doc.attr(t, "texture").and_then(|s| self.texture_slot(effect, s));
            return (None, slot);
        }
        let from_param = doc
            .child(el, "param")
            .and_then(|p| doc.attr(p, "ref"))
            .and_then(|r| newparam(doc, effect, r))
            .and_then(|np| doc.children(np).find(|&c| matches!(doc.name(c), "float4" | "float3" | "color")));
        (from_param.map(|f| color4(&doc.floats(f))), None)
    }

    fn texture_slot(&mut self, effect: u32, name: &str) -> Option<usize> {
        match find_image(&self.ctx, effect, name, 0) {
            Some(img) => self.image_slot(img),
            None => {
                self.warn(format!("texture '{name}' could not be resolved to an image"));
                None
            }
        }
    }

    fn image_slot(&mut self, img: u32) -> Option<usize> {
        if let Some(&s) = self.images.get(&img) {
            return s;
        }
        let s = self.image_job(img);
        self.images.insert(img, s);
        s
    }

    fn image_job(&mut self, img: u32) -> Option<usize> {
        let doc = self.ctx.doc;
        let id = doc.display_name(img).unwrap_or("image").to_string();
        let init = doc.child(img, "init_from");
        if let Some(h) = init.and_then(|i| doc.child(i, "hex")).or_else(|| doc.child(img, "data")) {
            self.jobs.push((id, ImageSource::Bytes(hex_bytes(doc.text(h)))));
            return Some(self.jobs.len() - 1);
        }
        let raw = init.map(|i| doc.child(i, "ref").map_or_else(|| doc.text(i), |r| doc.text(r))).unwrap_or("").trim();
        if raw.is_empty() {
            self.warn(format!("image '{id}' has no file reference"));
            return None;
        }
        let path = uri_to_path(raw);
        match resolve_texture_path(self.ctx.dir, &[path.as_str(), raw]) {
            Some(p) => {
                self.jobs.push((path, ImageSource::File(p)));
                Some(self.jobs.len() - 1)
            }
            None => {
                self.warn(format!("texture not found: {path}"));
                None
            }
        }
    }

    fn decode_textures(&mut self) {
        let jobs = std::mem::take(&mut self.jobs);
        let names: Vec<String> = jobs.iter().map(|(n, _)| n.clone()).collect();
        let mut slot_tex = Vec::with_capacity(jobs.len());
        for (i, r) in decode_parallel(jobs).into_iter().enumerate() {
            match r {
                Ok(t) => {
                    slot_tex.push(Some(self.scene.textures.len()));
                    self.scene.textures.push(t);
                }
                Err(e) => {
                    self.warn(format!("texture '{}': {e}", names[i]));
                    slot_tex.push(None);
                }
            }
        }
        for m in &mut self.scene.materials {
            for t in [&mut m.base_color_tex, &mut m.normal_tex, &mut m.emissive_tex, &mut m.mr_tex, &mut m.occlusion_tex] {
                *t = t.and_then(|s| slot_tex.get(s).copied().flatten());
            }
        }
    }

    fn read_animations(&mut self) {
        let doc = self.ctx.doc;
        let clips: Vec<String> = doc
            .kids(0, "library_animation_clips")
            .flat_map(|l| doc.kids(l, "animation_clip"))
            .map(|c| {
                let name = doc.display_name(c).unwrap_or("clip");
                match (doc.attr_num::<f32>(c, "start"), doc.attr_num::<f32>(c, "end")) {
                    (Some(s), Some(e)) => format!("{name} ({:.2} s)", e - s),
                    _ => name.to_string(),
                }
            })
            .collect();
        self.scene.info.animations = if clips.is_empty() {
            doc.kids(0, "library_animations")
                .flat_map(|l| doc.kids(l, "animation"))
                .map(|a| doc.display_name(a).unwrap_or("animation").to_string())
                .collect()
        } else {
            clips
        };
    }
}

fn newparam(doc: &Doc, effect: u32, sid: &str) -> Option<u32> {
    doc.descendants(effect).find(|&d| matches!(doc.name(d), "newparam" | "setparam") && doc.attr(d, "sid") == Some(sid))
}

/// `<texture texture=...>`: sampler2D newparam -> surface newparam -> image (1.4),
/// sampler2D -> instance_image (1.5), or an image id used directly.
fn find_image(ctx: &Ctx, effect: u32, name: &str, depth: u32) -> Option<u32> {
    let doc = ctx.doc;
    if depth > 4 {
        return None;
    }
    if let Some(np) = newparam(doc, effect, name) {
        for c in doc.children(np) {
            match doc.name(c) {
                "sampler2D" => {
                    if let Some(img) =
                        doc.child(c, "instance_image").and_then(|i| doc.attr(i, "url")).and_then(|u| ctx.by_url_named(u, "image"))
                    {
                        return Some(img);
                    }
                    if let Some(src) = doc.child(c, "source") {
                        return find_image(ctx, effect, doc.text(src).trim(), depth + 1);
                    }
                }
                "surface" => {
                    let init = doc.child(c, "init_from")?;
                    let id = doc.child(init, "ref").map_or_else(|| doc.text(init), |r| doc.text(r)).trim();
                    return ctx.by_url_named(id, "image");
                }
                _ => {}
            }
        }
    }
    ctx.by_url_named(name, "image")
        .or_else(|| doc.kids(0, "library_images").flat_map(|l| doc.kids(l, "image")).find(|&i| doc.attr(i, "name") == Some(name)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: [f32; 3], b: [f32; 3]) -> bool {
        a.iter().zip(b).all(|(x, y)| (x - y).abs() < 1e-5)
    }

    const POS_SRC: &str = r##"<source id="pos"><float_array id="pos-a" count="12">0 0 0 1 0 0 1 1 0 0 1 0</float_array>
      <technique_common><accessor source="#pos-a" count="4" stride="3">
        <param name="X" type="float"/><param name="Y" type="float"/><param name="Z" type="float"/>
      </accessor></technique_common></source>
     <vertices id="v"><input semantic="POSITION" source="#pos"/></vertices>"##;

    fn doc_with(asset: &str, libs: &str, scene: &str) -> String {
        format!(
            r#"<?xml version="1.0" encoding="utf-8"?>
<COLLADA xmlns="http://www.collada.org/2005/11/COLLADASchema" version="1.4.1">
 <asset>{asset}</asset>
 {libs}
 {scene}
</COLLADA>"#
        )
    }

    fn geom(id: &str, prims: &str) -> String {
        format!(r#"<geometry id="{id}" name="{id}"><mesh>{POS_SRC}{prims}</mesh></geometry>"#)
    }

    #[test]
    fn triangles_shared_offsets_and_matrix() {
        let libs = r##"<library_geometries><geometry id="g" name="tri"><mesh>
     <source id="pos"><float_array id="pos-a" count="9">0 0 0 1 0 0 0 1 0</float_array>
      <technique_common><accessor source="#pos-a" count="3" stride="3">
        <param name="X" type="float"/><param name="Y" type="float"/><param name="Z" type="float"/>
      </accessor></technique_common></source>
     <source id="nrm"><float_array id="nrm-a" count="3">0 0 1</float_array>
      <technique_common><accessor source="#nrm-a" count="1" stride="3">
        <param name="X" type="float"/><param name="Y" type="float"/><param name="Z" type="float"/>
      </accessor></technique_common></source>
     <source id="uv"><float_array id="uv-a" count="6">0 0 1 0 0 1</float_array>
      <technique_common><accessor source="#uv-a" count="3" stride="2">
        <param name="S" type="float"/><param name="T" type="float"/>
      </accessor></technique_common></source>
     <vertices id="v"><input semantic="POSITION" source="#pos"/></vertices>
     <triangles count="2">
      <input semantic="VERTEX" source="#v" offset="0"/>
      <input semantic="NORMAL" source="#nrm" offset="1"/>
      <input semantic="TEXCOORD" source="#uv" offset="0" set="0"/>
      <p>0 0 1 0 2 0  2 0 1 0 0 0</p>
     </triangles>
    </mesh></geometry></library_geometries>
    <library_visual_scenes><visual_scene id="s"><node id="n" name="Tri">
      <matrix>1 0 0 10  0 1 0 20  0 0 1 30  0 0 0 1</matrix>
      <instance_geometry url="#g"/>
    </node></visual_scene></library_visual_scenes>"##;
        let mut s =
            parse_dae(&doc_with("<up_axis>Y_UP</up_axis>", libs, r##"<scene><instance_visual_scene url="#s"/></scene>"##), None).unwrap();
        s.finalize();
        assert_eq!(s.meshes.len(), 1);
        let m = &s.meshes[0];
        assert_eq!(m.name, "Tri");
        assert_eq!(m.positions.len(), 3, "corners deduplicated by index tuple");
        assert_eq!(m.indices.len(), 6);
        assert!(close(m.positions[1], [11.0, 20.0, 30.0]), "{:?}", m.positions[1]);
        assert_eq!(m.uvs[1], [1.0, 1.0], "V flipped");
        assert!(m.had_normals && close(m.normals[0], [0.0, 0.0, 1.0]));
        assert!(!s.info.up_axis_z);
        assert_eq!(s.info.nodes, 1);
        assert_eq!(s.info.format, "COLLADA 1.4.1");
    }

    #[test]
    fn polylist_quad_is_two_triangles() {
        let g = geom(
            "q",
            r##"<polylist count="1"><input semantic="VERTEX" source="#v" offset="0"/><vcount>4</vcount><p>0 1 2 3</p></polylist>"##,
        );
        // No visual scene: every geometry is instantiated once.
        let s = parse_dae(&doc_with("", &format!("<library_geometries>{g}</library_geometries>"), ""), None).unwrap();
        assert_eq!(s.meshes.len(), 1);
        assert_eq!(s.meshes[0].positions.len(), 4);
        assert_eq!(s.meshes[0].triangle_count(), 2);
        assert!(s.meshes[0].normals.is_empty() && !s.meshes[0].had_normals);
    }

    #[test]
    fn concave_polygon_is_ear_clipped() {
        // L-shaped hexagon: a fan from vertex 0 would leave the polygon.
        let libs = r##"<library_geometries><geometry id="l"><mesh>
     <source id="pos"><float_array id="pos-a" count="18">0 0 0  2 0 0  2 1 0  1 1 0  1 2 0  0 2 0</float_array>
      <technique_common><accessor source="#pos-a" count="6" stride="3">
        <param name="X" type="float"/><param name="Y" type="float"/><param name="Z" type="float"/>
      </accessor></technique_common></source>
     <vertices id="v"><input semantic="POSITION" source="#pos"/></vertices>
     <polygons count="1"><input semantic="VERTEX" source="#v" offset="0"/><p>0 1 2 3 4 5</p></polygons>
    </mesh></geometry></library_geometries>"##;
        let s = parse_dae(&doc_with("", libs, ""), None).unwrap();
        let m = &s.meshes[0];
        assert_eq!(m.triangle_count(), 4);
        let area: f32 = m
            .indices
            .chunks_exact(3)
            .map(|t| {
                let p = |i: u32| Vec3::from(m.positions[i as usize]);
                (p(t[1]) - p(t[0])).cross(p(t[2]) - p(t[0])).z * 0.5
            })
            .sum();
        assert!((area - 3.0).abs() < 1e-5, "area {area}");
    }

    #[test]
    fn strips_fans_lines() {
        let prims = r##"<tristrips count="1"><input semantic="VERTEX" source="#v" offset="0"/><p>0 1 3 2</p></tristrips>
          <trifans count="1"><input semantic="VERTEX" source="#v" offset="0"/><p>0 1 2 3</p></trifans>
          <linestrips count="1"><input semantic="VERTEX" source="#v" offset="0"/><p>0 1 2 3</p></linestrips>"##;
        let s = parse_dae(&doc_with("", &format!("<library_geometries>{}</library_geometries>", geom("g", prims)), ""), None).unwrap();
        assert_eq!(s.meshes.len(), 3);
        assert_eq!(s.meshes[0].indices, vec![0, 1, 2, 2, 1, 3]);
        assert_eq!(s.meshes[1].triangle_count(), 2);
        assert_eq!(s.meshes[2].topology, Topology::Lines);
        assert_eq!(s.meshes[2].indices.len(), 6);
        assert_eq!(s.meshes[0].name, "g [0]");
    }

    #[test]
    fn z_up_unit_and_tool() {
        let g = geom("g", r##"<triangles count="1"><input semantic="VERTEX" source="#v" offset="0"/><p>0 1 2</p></triangles>"##);
        let asset = r#"<contributor><authoring_tool>TestTool 1.0</authoring_tool></contributor>
          <unit name="centimeter" meter="0.01"/><up_axis>Z_UP</up_axis>"#;
        let s = parse_dae(&doc_with(asset, &format!("<library_geometries>{g}</library_geometries>"), ""), None).unwrap();
        assert!(s.info.up_axis_z);
        assert!(s.info.extra.contains(&("authoring tool".into(), "TestTool 1.0".into())));
        assert!(s.info.extra.contains(&("unit".into(), "centimeter (0.01 m)".into())));
        assert_eq!(s.meshes[0].positions[1], [1.0, 0.0, 0.0], "Z_UP keeps coordinates");
    }

    #[test]
    fn x_up_is_rotated_to_y_up() {
        let g = geom("g", r##"<triangles count="1"><input semantic="VERTEX" source="#v" offset="0"/><p>0 1 2</p></triangles>"##);
        let s =
            parse_dae(&doc_with("<up_axis>X_UP</up_axis>", &format!("<library_geometries>{g}</library_geometries>"), ""), None).unwrap();
        assert!(close(s.meshes[0].positions[1], [0.0, 1.0, 0.0]), "{:?}", s.meshes[0].positions[1]);
        assert!(!s.info.warnings.is_empty());
    }

    const MAT_LIBS: &str = r##"
     <library_effects><effect id="fx"><profile_COMMON><technique sid="common"><phong>
       <emission><color>0 0 0 1</color></emission>
       <diffuse><color sid="diffuse">0.5 0 1 1</color></diffuse>
       <shininess><float>50</float></shininess>
       <transparency><float>0.5</float></transparency>
     </phong></technique>
     <extra><technique profile="GOOGLEEARTH"><double_sided>1</double_sided></technique></extra>
     </profile_COMMON></effect></library_effects>
     <library_materials><material id="redMat" name="Red"><instance_effect url="#fx"/></material></library_materials>"##;

    #[test]
    fn material_bound_through_bind_material() {
        let g =
            geom("g", r##"<triangles count="1" material="sym"><input semantic="VERTEX" source="#v" offset="0"/><p>0 1 2</p></triangles>"##);
        let libs = format!(
            r##"{MAT_LIBS}<library_geometries>{g}</library_geometries>
     <library_visual_scenes><visual_scene id="s"><node name="A">
       <translate>1 2 3</translate>
       <instance_geometry url="#g"><bind_material><technique_common>
         <instance_material symbol="sym" target="#redMat"/>
       </technique_common></bind_material></instance_geometry>
     </node></visual_scene></library_visual_scenes>"##
        );
        let s = parse_dae(&doc_with("", &libs, r##"<scene><instance_visual_scene url="#s"/></scene>"##), None).unwrap();
        assert_eq!(s.materials.len(), 1);
        let m = &s.materials[0];
        assert_eq!(s.meshes[0].material, Some(0));
        assert_eq!(m.name, "Red");
        assert!((m.base_color[0] - 0.21404).abs() < 1e-4, "sRGB 0.5 -> linear, got {}", m.base_color[0]);
        assert!((m.base_color[2] - 1.0).abs() < 1e-6);
        assert!((m.base_color[3] - 0.5).abs() < 1e-6);
        assert_eq!(m.alpha_mode, AlphaMode::Blend);
        assert!(m.double_sided);
        assert!(m.roughness < 0.25);
        assert_eq!(s.meshes[0].positions[0], [1.0, 2.0, 3.0]);
    }

    #[test]
    fn unbound_symbol_falls_back_to_material_id() {
        let g = geom(
            "g",
            r##"<triangles count="1" material="redMat"><input semantic="VERTEX" source="#v" offset="0"/><p>0 1 2</p></triangles>"##,
        );
        let s = parse_dae(&doc_with("", &format!("{MAT_LIBS}<library_geometries>{g}</library_geometries>"), ""), None).unwrap();
        assert_eq!(s.meshes[0].material, Some(0));
    }

    #[test]
    fn transforms_compose_in_document_order() {
        let g = geom("g", r##"<triangles count="1"><input semantic="VERTEX" source="#v" offset="0"/><p>0 1 2</p></triangles>"##);
        let libs = format!(
            r##"<library_geometries>{g}</library_geometries>
     <library_nodes><node id="lib"><scale>2 2 2</scale><instance_geometry url="#g"/></node></library_nodes>
     <library_visual_scenes><visual_scene id="s"><node>
       <translate>10 0 0</translate><rotate>0 0 1 90</rotate>
       <instance_node url="#lib"/>
     </node></visual_scene></library_visual_scenes>"##
        );
        let s = parse_dae(&doc_with("", &libs, ""), None).unwrap();
        // (1,0,0) -> scale 2 -> (2,0,0) -> rotate 90 about Z -> (0,2,0) -> translate -> (10,2,0)
        assert!(close(s.meshes[0].positions[1], [10.0, 2.0, 0.0]), "{:?}", s.meshes[0].positions[1]);
        assert_eq!(s.info.nodes, 2);
    }

    #[test]
    fn skin_controller_uses_bind_shape_matrix() {
        let g = geom("g", r##"<triangles count="1"><input semantic="VERTEX" source="#v" offset="0"/><p>0 1 2</p></triangles>"##);
        let libs = format!(
            r##"<library_geometries>{g}</library_geometries>
     <library_controllers><controller id="c"><skin source="#g">
       <bind_shape_matrix>1 0 0 0 0 1 0 0 0 0 1 1 0 0 0 1</bind_shape_matrix>
       <source id="j"><Name_array id="ja" count="2">A B</Name_array></source>
       <joints><input semantic="JOINT" source="#j"/></joints>
     </skin></controller></library_controllers>
     <library_visual_scenes><visual_scene id="s"><node><translate>1 2 0</translate>
       <instance_controller url="#c"/></node></visual_scene></library_visual_scenes>"##
        );
        let s = parse_dae(&doc_with("", &libs, ""), None).unwrap();
        assert!(close(s.meshes[0].positions[0], [1.0, 2.0, 1.0]), "{:?}", s.meshes[0].positions[0]);
        assert_eq!((s.info.skins, s.info.bones), (1, 2));
    }

    #[test]
    fn texture_through_sampler_and_surface() {
        let dir = std::env::temp_dir().join(format!("viewer3d_dae_test_{}", std::process::id()));
        std::fs::create_dir_all(dir.join("tex")).unwrap();
        image::RgbaImage::from_pixel(2, 2, image::Rgba([255, 0, 0, 255])).save(dir.join("tex").join("my tex.png")).unwrap();
        let g =
            geom("g", r##"<triangles count="1" material="m"><input semantic="VERTEX" source="#v" offset="0"/><p>0 1 2</p></triangles>"##);
        let libs = format!(
            r##"<library_images><image id="img"><init_from>tex/my%20tex.png</init_from></image>
         <image id="gone"><init_from>file:///C:/nowhere/missing.png</init_from></image></library_images>
     <library_effects><effect id="fx"><profile_COMMON>
       <newparam sid="surf"><surface type="2D"><init_from>img</init_from></surface></newparam>
       <newparam sid="samp"><sampler2D><source>surf</source></sampler2D></newparam>
       <technique sid="common"><lambert>
         <diffuse><texture texture="samp" texcoord="UVMap"/></diffuse>
         <emission><texture texture="gone" texcoord="UVMap"/></emission>
       </lambert></technique></profile_COMMON></effect></library_effects>
     <library_materials><material id="m"><instance_effect url="#fx"/></material></library_materials>
     <library_geometries>{g}</library_geometries>"##
        );
        let s = parse_dae(&doc_with("", &libs, ""), Some(&dir)).unwrap();
        std::fs::remove_dir_all(&dir).ok();
        assert_eq!(s.textures.len(), 1);
        assert_eq!((s.textures[0].width, s.textures[0].height), (2, 2));
        assert_eq!(s.materials[0].base_color_tex, Some(0));
        assert_eq!(s.materials[0].base_color, [1.0; 4]);
        assert_eq!(s.materials[0].emissive_tex, None);
        assert!(s.info.warnings.iter().any(|w| w.contains("missing.png")), "{:?}", s.info.warnings);
    }

    #[test]
    fn bad_indices_are_skipped_with_warning() {
        let g = geom("g", r##"<triangles count="2"><input semantic="VERTEX" source="#v" offset="0"/><p>0 1 2 0 2 99</p></triangles>"##);
        let s = parse_dae(&doc_with("", &format!("<library_geometries>{g}</library_geometries>"), ""), None).unwrap();
        assert_eq!(s.meshes[0].triangle_count(), 1);
        assert!(s.info.warnings.iter().any(|w| w.contains("out-of-range")));
    }

    #[test]
    fn broken_input_is_error() {
        assert!(parse_dae("<COLLADA><library_geometries></COLLADA>", None).is_err(), "mismatched tags");
        assert!(parse_dae("<COLLADA><asset>", None).is_err(), "truncated");
        assert!(parse_dae("", None).is_err(), "empty");
        assert!(parse_dae("<html><body/></html>", None).is_err(), "not COLLADA");
        let err = parse_dae(&doc_with("", "<library_geometries/>", ""), None).unwrap_err();
        assert!(err.to_string().contains("no drawable geometry"));
        let g = geom("g", r##"<triangles count="1"><input semantic="VERTEX" source="#nope" offset="0"/><p>0 1 2</p></triangles>"##);
        assert!(parse_dae(&doc_with("", &format!("<library_geometries>{g}</library_geometries>"), ""), None).is_err());
    }

    #[test]
    fn lookat_places_and_orients_node() {
        let g = geom("g", r##"<triangles count="1"><input semantic="VERTEX" source="#v" offset="0"/><p>0 1 2</p></triangles>"##);
        let libs = format!(
            r##"<library_geometries>{g}</library_geometries>
     <library_visual_scenes><visual_scene id="s"><node><lookat>5 0 0  0 0 0  0 1 0</lookat>
       <instance_geometry url="#g"/></node></visual_scene></library_visual_scenes>"##
        );
        let s = parse_dae(&doc_with("", &libs, ""), None).unwrap();
        // Node -Z points from the eye at the origin, so local +X maps to world -Z.
        assert!(close(s.meshes[0].positions[1], [5.0, 0.0, -1.0]), "{:?}", s.meshes[0].positions[1]);
    }

    #[test]
    fn uri_and_number_helpers() {
        assert_eq!(uri_to_path("file:///C:/tex/a%20b.png"), "C:/tex/a b.png");
        assert_eq!(uri_to_path("file:///home/u/a.png"), "/home/u/a.png");
        assert_eq!(uri_to_path("./maps/x.jpg"), "./maps/x.jpg");
        let big: String = (0..2_000_000).map(|i| format!("{} ", i % 1000)).collect();
        let v = parse_list::<u32>(&big, NONE);
        assert_eq!(v.len(), 2_000_000);
        assert!(v.iter().enumerate().all(|(i, &x)| x == (i % 1000) as u32));
        assert_eq!(parse_list::<f32>("1 -1.#IND 2", 0.0), vec![1.0, 0.0, 2.0]);
    }
}
