//! Blender `.blend` loader.
//!
//! A .blend file is a dump of Blender's in-memory structs plus a description of
//! their layout (SDNA) written by the same build, so everything here is read by
//! field *name* through the file's own SDNA and works across versions (2.8x .. 5.x):
//! - gzip (2.x) and zstd (3.0+, seekable multi-frame, decoded in parallel) compression;
//! - the legacy 12-byte header and the 5.x 17-byte header with 64-bit block lengths;
//! - pointers resolved per data-block (5.x reuses "old" addresses between IDs);
//! - the active scene's collection tree, viewport-hidden objects and collections,
//!   collection instances, parent chains (object and bone parents);
//! - mesh geometry in the legacy MVert/MPoly/MLoop layout, CustomData attributes
//!   (3.5 .. 4.x) and 5.x AttributeStorage: UVs, material indices, flat/smooth
//!   faces with sharp edges (and pre-4.1 auto smooth);
//! - material colours from the Principled BSDF node, else the viewport colour.
//!
//! Modifiers, shape keys, custom normals, textures and non-mesh objects are not evaluated.

use crate::scene::{AlphaMode, Material, Mesh, Scene, Topology};
use anyhow::{Context, Result, anyhow, bail};
use glam::{Mat3, Mat4, Quat, Vec3};
use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::hash::{BuildHasherDefault, Hasher};
use std::io::Read;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Tiny Fx-style hasher: keys are plain integers (old pointers, block indices).
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

// Object types (DNA_object_types.h).
const OB_EMPTY: i64 = 0;
const OB_MESH: i64 = 1;
const OB_CURVES_LEGACY: i64 = 2;
const OB_SURF: i64 = 3;
const OB_FONT: i64 = 4;
const OB_MBALL: i64 = 5;
const OB_LAMP: i64 = 10;
const OB_CAMERA: i64 = 11;
const OB_ARMATURE: i64 = 25;
/// `Object.restrictflag` / `visibility_flag`: "Disable in Viewports".
const OB_HIDE_VIEWPORT: i64 = 1;
/// `Collection.flag`: "Disable in Viewports".
const COLLECTION_HIDE_VIEWPORT: i64 = 1;
/// `Object.transflag`: instances a collection.
const OB_DUPLICOLLECTION: i64 = 1 << 8;
const PARBONE: i64 = 7;
// Legacy mesh flags.
const ME_SMOOTH: i64 = 1;
const ME_SHARP: i64 = 1 << 9;
const ME_AUTOSMOOTH: i64 = 1 << 5;
// CustomData layer types (DNA_customdata_types.h).
const CD_MVERT: i64 = 0;
const CD_MEDGE: i64 = 3;
const CD_PROP_INT32: i64 = 11;
const CD_MLOOPUV: i64 = 16;
const CD_MPOLY: i64 = 25;
const CD_MLOOP: i64 = 26;
const CD_CUSTOMLOOPNORMAL: i64 = 41;
const CD_PROP_FLOAT3: i64 = 48;
const CD_PROP_FLOAT2: i64 = 49;
const CD_PROP_BOOL: i64 = 50;
/// `bNodeSocket.type` of colour sockets.
const SOCK_RGBA: i64 = 2;
// Attribute domains.
const DOM_POINT: u8 = 0;
const DOM_EDGE: u8 = 1;
const DOM_FACE: u8 = 2;
const DOM_CORNER: u8 = 3;

pub fn parse_blend(data: &[u8]) -> Result<Scene> {
    let (raw, compression) = decompress(data)?;
    let mut warnings = Vec::new();
    let f = Blend::parse(&raw, &mut warnings)?;
    let mut scene = load_scene(&f, &mut warnings)?;
    let info = &mut scene.info;
    info.format = format!("Blender {}", version_string(f.version));
    info.up_axis_z = true;
    let sub = f.global().and_then(|g| g.int("subversion")).map(|s| format!(" (subversion {s})")).unwrap_or_default();
    info.extra.insert(0, ("Blender file version".into(), format!("{}{sub}", version_string(f.version))));
    info.extra.insert(1, ("compression".into(), compression.into()));
    if f.ptr_size == 4 {
        info.extra.push(("pointer size".into(), "32-bit".into()));
    }
    info.warnings.extend(warnings);
    Ok(scene)
}

fn version_string(v: u32) -> String {
    let (major, minor) = (v / 100, v % 100);
    if major >= 3 { format!("{major}.{minor}") } else { format!("{major}.{minor:02}") }
}

// ---------------------------------------------------------------------------
// Decompression
// ---------------------------------------------------------------------------

const GZIP_MAGIC: [u8; 2] = [0x1f, 0x8b];
const ZSTD_MAGIC: [u8; 4] = [0x28, 0xb5, 0x2f, 0xfd];

fn decompress(data: &[u8]) -> Result<(Cow<'_, [u8]>, &'static str)> {
    if data.starts_with(&GZIP_MAGIC) {
        let mut out = Vec::with_capacity(data.len().saturating_mul(4));
        flate2::read::MultiGzDecoder::new(data).read_to_end(&mut out).context("gzip decompression failed")?;
        Ok((Cow::Owned(out), "gzip"))
    } else if data.starts_with(&ZSTD_MAGIC) {
        let out = match zstd_seek_table(data) {
            Some(frames) => zstd_parallel(data, &frames).or_else(|_| zstd_sequential(data))?,
            None => zstd_sequential(data)?,
        };
        Ok((Cow::Owned(out), "zstd"))
    } else {
        Ok((Cow::Borrowed(data), "none"))
    }
}

/// One frame of a seekable zstd stream: compressed byte range and decompressed size.
#[derive(Clone, Copy, Debug, PartialEq)]
struct ZFrame {
    src: usize,
    src_len: usize,
    dst_len: usize,
}

/// Frames listed in the seek table Blender appends to zstd files (a skippable frame
/// ending with the seekable-format footer). `None` if there is no valid table.
fn zstd_seek_table(data: &[u8]) -> Option<Vec<ZFrame>> {
    let n = data.len();
    let footer = data.get(n.checked_sub(9)?..)?;
    if rd_u32(footer, 5)? != 0x8F92_EAB1 {
        return None;
    }
    let count = rd_u32(footer, 0)? as usize;
    let entry = if footer[4] & 0x80 != 0 { 12 } else { 8 };
    let table_len = count.checked_mul(entry)?.checked_add(9)?;
    let skip = n.checked_sub(table_len.checked_add(8)?)?;
    if rd_u32(data, skip)? & 0xFFFF_FFF0 != 0x184D_2A50 || rd_u32(data, skip + 4)? as usize != table_len {
        return None;
    }
    let mut frames = Vec::with_capacity(count);
    let mut src = 0usize;
    for i in 0..count {
        let e = skip + 8 + i * entry;
        let (src_len, dst_len) = (rd_u32(data, e)? as usize, rd_u32(data, e + 4)? as usize);
        frames.push(ZFrame { src, src_len, dst_len });
        src = src.checked_add(src_len)?;
    }
    (src == skip && !frames.is_empty()).then_some(frames)
}

/// Decode independent frames on all cores straight into their slots of the output.
fn zstd_parallel(data: &[u8], frames: &[ZFrame]) -> Result<Vec<u8>> {
    use ruzstd::decoding::FrameDecoder;
    let total = frames.iter().try_fold(0usize, |a, f| a.checked_add(f.dst_len)).ok_or_else(|| anyhow!("zstd size overflow"))?;
    // A seek table claiming absurd sizes (corrupt file) must not trigger a huge allocation.
    if total / 64 > data.len().max(1 << 20) {
        bail!("zstd seek table sizes are implausible");
    }
    let mut out = vec![0u8; total];
    let mut jobs: Vec<(&[u8], &mut [u8])> = Vec::with_capacity(frames.len());
    let mut rest = &mut out[..];
    for f in frames {
        let (dst, tail) = rest.split_at_mut(f.dst_len);
        let src = data.get(f.src..f.src + f.src_len).ok_or_else(|| anyhow!("zstd frame out of range"))?;
        jobs.push((src, dst));
        rest = tail;
    }
    let jobs = Mutex::new(jobs);
    let error: Mutex<Option<String>> = Mutex::new(None);
    let threads = std::thread::available_parallelism().map(|c| c.get()).unwrap_or(4).min(frames.len());
    std::thread::scope(|s| {
        for _ in 0..threads {
            s.spawn(|| {
                let mut dec = FrameDecoder::new();
                loop {
                    let Some((src, dst)) = jobs.lock().unwrap().pop() else { break };
                    match dec.decode_all(src, dst) {
                        Ok(n) if n == dst.len() => {}
                        Ok(n) => *error.lock().unwrap() = Some(format!("zstd frame size mismatch ({n} != {})", dst.len())),
                        Err(e) => *error.lock().unwrap() = Some(format!("zstd: {e}")),
                    }
                }
            });
        }
    });
    if let Some(e) = error.into_inner().unwrap() {
        bail!(e);
    }
    Ok(out)
}

/// Decode all frames one after another (no seek table), skipping skippable frames.
fn zstd_sequential(data: &[u8]) -> Result<Vec<u8>> {
    use ruzstd::decoding::errors::{FrameDecoderError, ReadFrameHeaderError};
    use ruzstd::decoding::{BlockDecodingStrategy, FrameDecoder};
    let mut input = data;
    let mut out = Vec::with_capacity(data.len().saturating_mul(5));
    let mut dec = FrameDecoder::new();
    while !input.is_empty() {
        match dec.reset(&mut input) {
            Ok(()) => {}
            Err(FrameDecoderError::ReadFrameHeaderError(ReadFrameHeaderError::SkipFrame { length, .. })) => {
                input = input.get(length as usize..).ok_or_else(|| anyhow!("truncated zstd skippable frame"))?;
                continue;
            }
            // Trailing garbage after at least one good frame is ignored.
            Err(_) if !out.is_empty() => break,
            Err(e) => bail!("zstd decompression failed: {e}"),
        }
        loop {
            let done = dec
                .decode_blocks(&mut input, BlockDecodingStrategy::UptoBytes(4 << 20))
                .map_err(|e| anyhow!("zstd decompression failed: {e}"))?;
            dec.collect_to_writer(&mut out)?;
            if done {
                break;
            }
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Little-endian readers (all bounds-checked)
// ---------------------------------------------------------------------------

fn rd_u16(d: &[u8], o: usize) -> Option<u16> {
    d.get(o..o.checked_add(2)?).map(|b| u16::from_le_bytes([b[0], b[1]]))
}
fn rd_u32(d: &[u8], o: usize) -> Option<u32> {
    d.get(o..o.checked_add(4)?).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}
fn rd_u64(d: &[u8], o: usize) -> Option<u64> {
    d.get(o..o.checked_add(8)?).map(|b| u64::from_le_bytes(b.try_into().unwrap()))
}
fn rd_f32(d: &[u8], o: usize) -> Option<f32> {
    rd_u32(d, o).map(f32::from_bits)
}
fn cstr(b: &[u8]) -> String {
    let end = b.iter().position(|&c| c == 0).unwrap_or(b.len());
    String::from_utf8_lossy(&b[..end]).into_owned()
}

// ---------------------------------------------------------------------------
// Header and blocks
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq)]
enum BHeadKind {
    /// code, len, old (4), sdna, nr
    Small4,
    /// code, len, old (8), sdna, nr
    Small8,
    /// 5.x: code, sdna, old (8), len (i64), nr (i64)
    Large8,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Header {
    ptr_size: usize,
    version: u32,
    bhead: BHeadKind,
    len: usize,
}

fn parse_header(d: &[u8]) -> Result<Header> {
    if d.len() < 12 || &d[..7] != b"BLENDER" {
        bail!("not a Blender file (missing BLENDER header)");
    }
    let digits = |s: &[u8]| -> Option<u32> {
        if s.is_empty() || !s.iter().all(u8::is_ascii_digit) {
            return None;
        }
        std::str::from_utf8(s).ok()?.parse().ok()
    };
    let endian = |c: u8| -> Result<()> {
        match c {
            b'v' => Ok(()),
            b'V' => bail!("big-endian .blend files are not supported"),
            _ => bail!("bad endianness marker in .blend header"),
        }
    };
    if d[7] == b'_' || d[7] == b'-' {
        // Legacy: "BLENDER" + pointer size + endianness + 3-digit version.
        let ptr_size = if d[7] == b'_' { 4 } else { 8 };
        endian(d[8])?;
        let version = digits(&d[9..12]).ok_or_else(|| anyhow!("bad version in .blend header"))?;
        let bhead = if ptr_size == 4 { BHeadKind::Small4 } else { BHeadKind::Small8 };
        return Ok(Header { ptr_size, version, bhead, len: 12 });
    }
    // 5.x: "BLENDER" + header size (2 digits) + '-' + file format version (2 digits)
    // + endianness + 4-digit version, e.g. "BLENDER17-01v0501". Always 64-bit pointers.
    let len = digits(&d[7..9]).ok_or_else(|| anyhow!("unrecognised .blend header"))? as usize;
    if len < 17 || d.len() < len || d[9] != b'-' {
        bail!("unrecognised .blend header");
    }
    let format = digits(&d[10..12]).ok_or_else(|| anyhow!("bad format version in .blend header"))?;
    endian(d[12])?;
    let version = digits(&d[13..17]).ok_or_else(|| anyhow!("bad version in .blend header"))?;
    if format != 1 {
        bail!("unsupported .blend file format version {format} (Blender {})", version_string(version));
    }
    Ok(Header { ptr_size: 8, version, bhead: BHeadKind::Large8, len })
}

#[derive(Clone, Debug)]
struct Block {
    code: [u8; 4],
    sdna: u32,
    old: u64,
    start: usize,
    len: usize,
    /// Index of the ID block this block belongs to (the last ID block before it).
    owner: u32,
}

fn is_id_code(code: &[u8; 4]) -> bool {
    code[0] != 0 && code[2] == 0 && code[3] == 0
}

fn parse_blocks(d: &[u8], h: &Header, warnings: &mut Vec<String>) -> Vec<Block> {
    let bh = match h.bhead {
        BHeadKind::Small4 => 20,
        BHeadKind::Small8 => 24,
        BHeadKind::Large8 => 32,
    };
    let mut blocks = Vec::new();
    let mut pos = h.len;
    let mut owner = u32::MAX;
    loop {
        let Some(hd) = d.get(pos..pos + bh) else {
            warnings.push("file is truncated (no end block)".into());
            break;
        };
        let code = [hd[0], hd[1], hd[2], hd[3]];
        let u32_at = |o| rd_u32(hd, o).unwrap_or(0);
        let (sdna, old, len, _count) = match h.bhead {
            BHeadKind::Large8 => {
                (u32_at(4), rd_u64(hd, 8).unwrap_or(0), rd_u64(hd, 16).unwrap_or(0) as i64, rd_u64(hd, 24).unwrap_or(0) as i64)
            }
            BHeadKind::Small8 => (u32_at(16), rd_u64(hd, 8).unwrap_or(0), u32_at(4) as i32 as i64, u32_at(20) as i32 as i64),
            BHeadKind::Small4 => (u32_at(12), u32_at(8) as u64, u32_at(4) as i32 as i64, u32_at(16) as i32 as i64),
        };
        pos += bh;
        if &code == b"ENDB" {
            break;
        }
        let end = usize::try_from(len).ok().and_then(|l| pos.checked_add(l)).filter(|&e| e <= d.len());
        let Some(end) = end else {
            warnings.push("file is truncated (last block incomplete)".into());
            break;
        };
        let idx = blocks.len() as u32;
        if is_id_code(&code) {
            owner = idx;
        }
        blocks.push(Block { code, sdna, old, start: pos, len: end - pos, owner });
        pos = end;
    }
    blocks
}

// ---------------------------------------------------------------------------
// SDNA
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Prim {
    Other,
    I8,
    U8,
    I16,
    U16,
    I32,
    U32,
    I64,
    U64,
    F32,
    F64,
}

impl Prim {
    fn from_type(t: &str) -> Prim {
        match t {
            "char" | "uchar" | "uint8_t" => Prim::U8,
            "int8_t" => Prim::I8,
            "short" | "int16_t" => Prim::I16,
            "ushort" | "uint16_t" => Prim::U16,
            "int" | "int32_t" => Prim::I32,
            "uint" | "uint32_t" => Prim::U32,
            "int64_t" | "long" => Prim::I64,
            "uint64_t" | "ulong" => Prim::U64,
            "float" => Prim::F32,
            "double" => Prim::F64,
            _ => Prim::Other,
        }
    }
    fn size(self) -> usize {
        match self {
            Prim::Other => 0,
            Prim::I8 | Prim::U8 => 1,
            Prim::I16 | Prim::U16 => 2,
            Prim::I32 | Prim::U32 | Prim::F32 => 4,
            Prim::I64 | Prim::U64 | Prim::F64 => 8,
        }
    }
    fn read_i64(self, b: &[u8]) -> Option<i64> {
        let b = b.get(..self.size().max(1))?;
        Some(match self {
            Prim::I8 => b[0] as i8 as i64,
            Prim::U8 => b[0] as i64,
            Prim::I16 => i16::from_le_bytes([b[0], b[1]]) as i64,
            Prim::U16 => u16::from_le_bytes([b[0], b[1]]) as i64,
            Prim::I32 => i32::from_le_bytes(b.try_into().ok()?) as i64,
            Prim::U32 => u32::from_le_bytes(b.try_into().ok()?) as i64,
            Prim::I64 | Prim::U64 => i64::from_le_bytes(b.try_into().ok()?),
            Prim::F32 => f32::from_le_bytes(b.try_into().ok()?) as i64,
            Prim::F64 => f64::from_le_bytes(b.try_into().ok()?) as i64,
            Prim::Other => return None,
        })
    }
    fn read_f64(self, b: &[u8]) -> Option<f64> {
        match self {
            Prim::F32 => Some(f32::from_le_bytes(b.get(..4)?.try_into().ok()?) as f64),
            Prim::F64 => Some(f64::from_le_bytes(b.get(..8)?.try_into().ok()?)),
            _ => self.read_i64(b).map(|v| v as f64),
        }
    }
}

#[derive(Clone, Debug)]
struct Field {
    name: String,
    ty: u32,
    offset: usize,
    size: usize,
    ptr: bool,
    array: usize,
}

#[derive(Clone, Debug)]
struct StructDef {
    size: usize,
    fields: Vec<Field>,
}

struct Sdna {
    types: Vec<String>,
    prims: Vec<Prim>,
    structs: Vec<StructDef>,
    /// Type index -> struct index (`u32::MAX` for non-struct types).
    type_struct: Vec<u32>,
    by_name: HashMap<String, u32>,
}

/// Splits an SDNA member name such as `*next`, `co[3]`, `mat[4][4]` or `(*func)()`
/// into (base name, is pointer, number of array elements). Matches Blender's own
/// rules: a leading `*` or `(*` makes a pointer, every `[n]` multiplies the count.
fn parse_member_name(s: &str) -> (&str, bool, usize) {
    let ptr = s.starts_with('*') || s.starts_with("(*");
    let rest = s.trim_start_matches(['*', '(']);
    let base = &rest[..rest.find([')', '[']).unwrap_or(rest.len())];
    let mut array = 1usize;
    let mut tail = s;
    while let Some(open) = tail.find('[') {
        let Some(close) = tail[open..].find(']') else { break };
        if let Ok(n) = tail[open + 1..open + close].trim().parse::<usize>() {
            array = array.saturating_mul(n);
        }
        tail = &tail[open + close + 1..];
    }
    (base, ptr, array)
}

struct Cursor<'a> {
    b: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn tag(&mut self, t: &[u8; 4]) -> Result<()> {
        if self.b.get(self.pos..self.pos + 4) != Some(&t[..]) {
            bail!("SDNA: expected '{}' at offset {}", String::from_utf8_lossy(t), self.pos);
        }
        self.pos += 4;
        Ok(())
    }
    fn u32(&mut self) -> Result<u32> {
        let v = rd_u32(self.b, self.pos).ok_or_else(|| anyhow!("SDNA truncated"))?;
        self.pos += 4;
        Ok(v)
    }
    fn u16(&mut self) -> Result<u16> {
        let v = rd_u16(self.b, self.pos).ok_or_else(|| anyhow!("SDNA truncated"))?;
        self.pos += 2;
        Ok(v)
    }
    fn cstr(&mut self) -> Result<&'a str> {
        let rest = self.b.get(self.pos..).ok_or_else(|| anyhow!("SDNA truncated"))?;
        let n = rest.iter().position(|&c| c == 0).ok_or_else(|| anyhow!("SDNA: unterminated name"))?;
        self.pos += n + 1;
        Ok(std::str::from_utf8(&rest[..n]).unwrap_or(""))
    }
    fn align4(&mut self) {
        self.pos = (self.pos + 3) & !3;
    }
    /// A count read from the file, sanity-limited by the remaining bytes.
    fn count(&mut self, min_item: usize) -> Result<usize> {
        let n = self.u32()? as usize;
        if n.saturating_mul(min_item) > self.b.len().saturating_sub(self.pos) {
            bail!("SDNA: implausible count {n}");
        }
        Ok(n)
    }
}

fn parse_sdna(b: &[u8], ptr_size: usize) -> Result<Sdna> {
    let mut c = Cursor { b, pos: 0 };
    c.tag(b"SDNA")?;
    c.tag(b"NAME")?;
    let n = c.count(1)?;
    let names: Vec<&str> = (0..n).map(|_| c.cstr()).collect::<Result<_>>()?;
    c.align4();
    c.tag(b"TYPE")?;
    let n = c.count(1)?;
    let types: Vec<String> = (0..n).map(|_| c.cstr().map(str::to_string)).collect::<Result<_>>()?;
    c.align4();
    c.tag(b"TLEN")?;
    let tlen: Vec<usize> = (0..types.len()).map(|_| c.u16().map(|v| v as usize)).collect::<Result<_>>()?;
    c.align4();
    c.tag(b"STRC")?;
    let n = c.count(4)?;
    let mut structs = Vec::with_capacity(n);
    let mut type_struct = vec![u32::MAX; types.len()];
    let mut by_name = HashMap::with_capacity(n);
    for i in 0..n {
        let ty = c.u16()? as usize;
        let nf = c.u16()? as usize;
        let mut fields = Vec::with_capacity(nf);
        let mut offset = 0usize;
        for _ in 0..nf {
            let (ft, fname) = (c.u16()? as usize, c.u16()? as usize);
            let (Some(fname), Some(&flen)) = (names.get(fname), tlen.get(ft)) else { bail!("SDNA: bad member index") };
            let (base, ptr, array) = parse_member_name(fname);
            let size = if ptr { ptr_size } else { flen }.saturating_mul(array);
            fields.push(Field { name: base.to_string(), ty: ft as u32, offset, size, ptr, array });
            offset = offset.saturating_add(size);
        }
        let Some(&size) = tlen.get(ty) else { bail!("SDNA: bad struct type index") };
        type_struct[ty] = i as u32;
        by_name.insert(types[ty].clone(), i as u32);
        structs.push(StructDef { size, fields });
    }
    let prims = types.iter().map(|t| Prim::from_type(t)).collect();
    Ok(Sdna { types, prims, structs, type_struct, by_name })
}

// ---------------------------------------------------------------------------
// File with pointer resolution and struct views
// ---------------------------------------------------------------------------

struct Blend<'a> {
    data: &'a [u8],
    ptr_size: usize,
    version: u32,
    blocks: Vec<Block>,
    /// Old pointer -> block (first occurrence; ID blocks take priority).
    by_ptr: FxMap<u64, u32>,
    /// (owner, old pointer) -> block, only for pointers that occur more than once.
    dups: FxMap<(u32, u64), u32>,
    sdna: Sdna,
}

impl<'a> Blend<'a> {
    fn parse(data: &'a [u8], warnings: &mut Vec<String>) -> Result<Self> {
        let h = parse_header(data)?;
        let blocks = parse_blocks(data, &h, warnings);
        let dna = blocks.iter().find(|b| &b.code == b"DNA1").ok_or_else(|| anyhow!("file has no SDNA block"))?;
        let sdna = parse_sdna(&data[dna.start..dna.start + dna.len], h.ptr_size).context("cannot read SDNA")?;
        let mut by_ptr: FxMap<u64, u32> = FxMap::default();
        by_ptr.reserve(blocks.len());
        let mut dups: FxMap<(u32, u64), u32> = FxMap::default();
        for (i, b) in blocks.iter().enumerate() {
            if b.old == 0 {
                continue;
            }
            let i = i as u32;
            match by_ptr.entry(b.old) {
                std::collections::hash_map::Entry::Vacant(e) => {
                    e.insert(i);
                }
                std::collections::hash_map::Entry::Occupied(mut e) => {
                    let prev = *e.get();
                    if is_id_code(&b.code) && !is_id_code(&blocks[prev as usize].code) {
                        e.insert(i);
                        dups.entry((blocks[prev as usize].owner, b.old)).or_insert(prev);
                    } else {
                        dups.entry((b.owner, b.old)).or_insert(i);
                    }
                }
            }
        }
        Ok(Blend { data, ptr_size: h.ptr_size, version: h.version, blocks, by_ptr, dups, sdna })
    }

    fn code(&self, b: u32) -> &[u8; 4] {
        &self.blocks[b as usize].code
    }

    fn bytes(&self, b: u32) -> &'a [u8] {
        let b = &self.blocks[b as usize];
        &self.data[b.start..b.start + b.len]
    }

    /// Resolve an old pointer that was read from block `from`. Data pointers are
    /// looked up within the same ID first (5.x reuses addresses between IDs).
    fn resolve(&self, from: u32, ptr: u64) -> Option<u32> {
        if ptr == 0 {
            return None;
        }
        let first = *self.by_ptr.get(&ptr)?;
        let owner = self.blocks.get(from as usize).map_or(u32::MAX, |b| b.owner);
        if self.blocks[first as usize].owner == owner || self.dups.is_empty() {
            return Some(first);
        }
        Some(self.dups.get(&(owner, ptr)).copied().unwrap_or(first))
    }

    fn read_ptr(&self, d: &[u8], off: usize) -> Option<u64> {
        if self.ptr_size == 8 { rd_u64(d, off) } else { rd_u32(d, off).map(u64::from) }
    }

    fn struct_id(&self, name: &str) -> Option<u32> {
        self.sdna.by_name.get(name).copied()
    }

    /// View of struct `st` at absolute offset `off`, which must lie inside block `block`.
    fn view(&self, block: u32, off: usize, st: u32) -> Option<View<'_>> {
        let b = self.blocks.get(block as usize)?;
        let size = self.sdna.structs.get(st as usize)?.size;
        (off >= b.start && off.checked_add(size)? <= b.start + b.len).then_some(View { f: self, block, off, st })
    }

    /// View of a block's first element using the struct type recorded in its header.
    fn block_view(&self, block: u32) -> Option<View<'_>> {
        let b = self.blocks.get(block as usize)?;
        self.view(block, b.start, b.sdna)
    }

    /// All elements of an array of struct `st` stored in `block`.
    fn array(&self, block: u32, st: u32) -> impl Iterator<Item = View<'_>> {
        let b = &self.blocks[block as usize];
        let size = self.sdna.structs.get(st as usize).map_or(0, |s| s.size);
        let n = b.len.checked_div(size).unwrap_or(0);
        (0..n).filter_map(move |i| self.view(block, b.start + i * size, st))
    }

    fn global(&self) -> Option<View<'_>> {
        let g = self.blocks.iter().position(|b| &b.code == b"GLOB")? as u32;
        let st = self.struct_id("FileGlobal")?;
        self.view(g, self.blocks[g as usize].start, st)
    }
}

/// A struct instance inside the file, read by field name through the SDNA.
#[derive(Clone, Copy)]
struct View<'f> {
    f: &'f Blend<'f>,
    block: u32,
    off: usize,
    st: u32,
}

impl<'f> View<'f> {
    fn def(&self) -> &'f StructDef {
        &self.f.sdna.structs[self.st as usize]
    }

    fn field(&self, name: &str) -> Option<&'f Field> {
        self.def().fields.iter().find(|fl| fl.name == name)
    }

    fn raw(&self, fl: &Field) -> Option<&'f [u8]> {
        if fl.offset.checked_add(fl.size)? > self.def().size {
            return None;
        }
        self.f.data.get(self.off + fl.offset..self.off + fl.offset + fl.size)
    }

    fn int(&self, name: &str) -> Option<i64> {
        let fl = self.field(name).filter(|fl| !fl.ptr)?;
        self.f.sdna.prims[fl.ty as usize].read_i64(self.raw(fl)?)
    }

    fn float(&self, name: &str) -> Option<f32> {
        let fl = self.field(name).filter(|fl| !fl.ptr)?;
        self.f.sdna.prims[fl.ty as usize].read_f64(self.raw(fl)?).map(|v| v as f32)
    }

    fn floats<const N: usize>(&self, name: &str) -> Option<[f32; N]> {
        let fl = self.field(name).filter(|fl| !fl.ptr && fl.array >= N && self.f.sdna.prims[fl.ty as usize] == Prim::F32)?;
        let b = self.raw(fl)?;
        let mut out = [0.0f32; N];
        for (i, v) in out.iter_mut().enumerate() {
            *v = rd_f32(b, i * 4)?;
        }
        Some(out)
    }

    fn ptr(&self, name: &str) -> u64 {
        self.field(name).filter(|fl| fl.ptr).and_then(|fl| self.f.read_ptr(self.raw(fl)?, 0)).unwrap_or(0)
    }

    fn string(&self, name: &str) -> String {
        self.field(name).filter(|fl| !fl.ptr).and_then(|fl| self.raw(fl)).map(cstr).unwrap_or_default()
    }

    /// Embedded struct member.
    fn sub(&self, name: &str) -> Option<View<'f>> {
        let fl = self.field(name).filter(|fl| !fl.ptr)?;
        let st = *self.f.sdna.type_struct.get(fl.ty as usize)?;
        if st == u32::MAX {
            return None;
        }
        self.f.view(self.block, self.off + fl.offset, st)
    }

    /// Block a pointer member points to.
    fn target(&self, name: &str) -> Option<u32> {
        self.f.resolve(self.block, self.ptr(name))
    }

    /// Follow a typed pointer member. Generic `ID *` / `void *` use the target block's struct.
    fn deref(&self, name: &str) -> Option<View<'f>> {
        let fl = self.field(name).filter(|fl| fl.ptr)?;
        let b = self.target(name)?;
        let st = self.f.sdna.type_struct.get(fl.ty as usize).copied().unwrap_or(u32::MAX);
        if st == u32::MAX || self.f.sdna.types[fl.ty as usize] == "ID" {
            return self.f.block_view(b);
        }
        self.f.view(b, self.f.blocks[b as usize].start, st)
    }

    /// Follow a pointer member, reading the target as struct `st_name`.
    fn deref_as(&self, name: &str, st_name: &str) -> Option<View<'f>> {
        let b = self.target(name)?;
        self.f.view(b, self.f.blocks[b as usize].start, self.f.struct_id(st_name)?)
    }

    /// NUL-terminated string a `char *` member points to.
    fn string_ptr(&self, name: &str) -> Option<String> {
        self.target(name).map(|b| cstr(self.f.bytes(b)))
    }

    /// Elements of a `ListBase` member, following `next` pointers.
    fn list(&self, name: &str, elem: &str) -> Vec<View<'f>> {
        let (Some(lb), Some(st)) = (self.sub(name), self.f.struct_id(elem)) else { return Vec::new() };
        let mut out = Vec::new();
        let mut seen = HashSet::new();
        let (mut p, mut from) = (lb.ptr("first"), self.block);
        while let Some(b) = self.f.resolve(from, p) {
            if !seen.insert(b) {
                break;
            }
            let Some(v) = self.f.view(b, self.f.blocks[b as usize].start, st) else { break };
            p = v.ptr("next");
            from = b;
            out.push(v);
        }
        out
    }

    /// `ID.name` without the two-letter type prefix.
    fn id_name(&self) -> String {
        let n = self.sub("id").map(|id| id.string("name")).unwrap_or_default();
        n.get(2..).unwrap_or("").to_string()
    }
}

// ---------------------------------------------------------------------------
// Scene traversal and object transforms
// ---------------------------------------------------------------------------

#[derive(Default)]
struct Stats {
    objects: usize,
    hidden_objects: usize,
    hidden_collections: usize,
    curves: usize,
    metaballs: usize,
    other: usize,
    linked: usize,
    modifiers: usize,
    instances: usize,
    cameras: usize,
    lights: usize,
}

struct Inst {
    ob: u32,
    mesh: u32,
    world: Mat4,
}

struct Walker<'f> {
    f: &'f Blend<'f>,
    worlds: HashMap<u32, Mat4>,
    top_objects: HashSet<u32>,
    stats: Stats,
    insts: Vec<Inst>,
}

impl<'f> Walker<'f> {
    fn walk_collection(&mut self, coll: View<'f>, inst: Option<Mat4>, depth: u32, colls: &mut HashSet<u32>, objs: &mut HashSet<u32>) {
        if depth > 32 || !colls.insert(coll.block) {
            return;
        }
        if coll.int("flag").unwrap_or(0) & COLLECTION_HIDE_VIEWPORT != 0 {
            self.stats.hidden_collections += 1;
            return;
        }
        for co in coll.list("gobject", "CollectionObject") {
            if let Some(b) = co.target("ob") {
                self.visit_block(b, inst, depth, objs);
            }
        }
        for cc in coll.list("children", "CollectionChild") {
            match cc.target("collection") {
                Some(b) if self.f.code(b) == b"GR\0\0" => {
                    if let Some(c) = self.f.block_view(b) {
                        self.walk_collection(c, inst, depth + 1, colls, objs);
                    }
                }
                Some(_) => self.stats.linked += 1,
                None => {}
            }
        }
    }

    fn visit_block(&mut self, b: u32, inst: Option<Mat4>, depth: u32, objs: &mut HashSet<u32>) {
        if self.f.code(b) != b"OB\0\0" {
            // Linked objects are stored as ID placeholders without data.
            self.stats.linked += 1;
            return;
        }
        if !objs.insert(b) {
            return;
        }
        if let Some(ob) = self.f.block_view(b) {
            self.visit_object(ob, inst, depth);
        }
    }

    fn visit_object(&mut self, ob: View<'f>, inst: Option<Mat4>, depth: u32) {
        let restrict = ob.int("restrictflag").or_else(|| ob.int("visibility_flag")).unwrap_or(0);
        if restrict & OB_HIDE_VIEWPORT != 0 {
            self.stats.hidden_objects += 1;
            return;
        }
        self.stats.objects += 1;
        let world = inst.unwrap_or(Mat4::IDENTITY) * self.world(ob, 0);
        match ob.int("type").unwrap_or(-1) {
            OB_MESH => match ob.target("data") {
                Some(me) if self.f.code(me) == b"ME\0\0" => {
                    if ob.sub("modifiers").is_some_and(|l| l.ptr("first") != 0) {
                        self.stats.modifiers += 1;
                    }
                    self.insts.push(Inst { ob: ob.block, mesh: me, world });
                }
                _ => self.stats.linked += 1,
            },
            OB_CURVES_LEGACY | OB_SURF | OB_FONT => self.stats.curves += 1,
            OB_MBALL => self.stats.metaballs += 1,
            OB_LAMP => self.stats.lights += 1,
            OB_CAMERA => self.stats.cameras += 1,
            OB_EMPTY | OB_ARMATURE => {}
            _ => self.stats.other += 1,
        }
        if ob.int("transflag").unwrap_or(0) & OB_DUPLICOLLECTION != 0 && depth < 8 {
            let coll = ob.target("instance_collection").or_else(|| ob.target("dup_group"));
            if let Some(c) = coll.filter(|&c| self.f.code(c) == b"GR\0\0").and_then(|c| self.f.block_view(c)) {
                self.stats.instances += 1;
                let ofs = coll_offset(&c);
                let m = world * Mat4::from_translation(-ofs);
                self.walk_collection(c, Some(m), depth + 1, &mut HashSet::new(), &mut HashSet::new());
            }
        }
    }

    /// World matrix of an object (without collection-instance transforms).
    fn world(&mut self, ob: View<'f>, depth: u32) -> Mat4 {
        if let Some(m) = self.worlds.get(&ob.block) {
            return *m;
        }
        // Older files store the evaluated matrix; newer ones only keep it at runtime.
        let stored = ob
            .floats::<16>("object_to_world")
            .or_else(|| ob.floats::<16>("obmat"))
            .map(|a| Mat4::from_cols_array(&a))
            .filter(|m| m.is_finite() && *m != Mat4::ZERO);
        let m = match stored {
            Some(m) => m,
            None => {
                let local = local_matrix(&ob);
                let parent = ob.target("parent").filter(|&p| p != ob.block && self.f.code(p) == b"OB\0\0");
                match parent.and_then(|p| self.f.block_view(p)) {
                    Some(p) if depth < 64 => {
                        let pw = self.world(p, depth + 1);
                        let mut pm = pw;
                        if ob.int("partype").unwrap_or(0) & 15 == PARBONE
                            && let Some(bm) = bone_matrix(&p, &ob.string("parsubstr"))
                        {
                            pm = pw * bm;
                        }
                        let pinv = ob.floats::<16>("parentinv").map(|a| Mat4::from_cols_array(&a)).unwrap_or(Mat4::IDENTITY);
                        pm * pinv * local
                    }
                    _ => local,
                }
            }
        };
        self.worlds.insert(ob.block, m);
        m
    }
}

fn coll_offset(c: &View) -> Vec3 {
    c.floats::<3>("instance_offset").or_else(|| c.floats::<3>("dupli_ofs")).map(Vec3::from).unwrap_or(Vec3::ZERO)
}

/// Bone parent: the bone's pose matrix moved to the bone tail (as Blender does).
fn bone_matrix(arm: &View, bone: &str) -> Option<Mat4> {
    let pose = arm.deref("pose")?;
    let ch = pose.list("chanbase", "bPoseChannel").into_iter().find(|c| c.string("name") == bone)?;
    let mut m = Mat4::from_cols_array(&ch.floats::<16>("pose_mat")?);
    if let Some(t) = ch.floats::<3>("pose_tail") {
        m.w_axis = Vec3::from(t).extend(1.0);
    }
    m.is_finite().then_some(m)
}

/// loc / rot / scale (plus delta transforms) as `T * R * S`.
fn local_matrix(ob: &View) -> Mat4 {
    let v3 = |n: &str, d: Vec3| ob.floats::<3>(n).map(Vec3::from).unwrap_or(d);
    let loc = v3("loc", Vec3::ZERO) + v3("dloc", Vec3::ZERO);
    let scale = ob.floats::<3>("scale").or_else(|| ob.floats::<3>("size")).map(Vec3::from).unwrap_or(Vec3::ONE) * v3("dscale", Vec3::ONE);
    let rot = match ob.int("rotmode").unwrap_or(1) {
        0 => {
            let q = |n: &str| ob.floats::<4>(n).map(quat_wxyz).unwrap_or(Quat::IDENTITY);
            Mat3::from_quat((q("dquat") * q("quat")).normalize())
        }
        -1 => {
            let aa = |axis: &str, angle: &str| {
                let a = v3(axis, Vec3::Y);
                let ang = ob.float(angle).unwrap_or(0.0);
                if a.length_squared() > 1e-12 && ang.is_finite() { Mat3::from_axis_angle(a.normalize(), ang) } else { Mat3::IDENTITY }
            };
            aa("drotAxis", "drotAngle") * aa("rotAxis", "rotAngle")
        }
        mode => euler(v3("drot", Vec3::ZERO), mode) * euler(v3("rot", Vec3::ZERO), mode),
    };
    let m = Mat4::from_translation(loc) * Mat4::from_mat3(rot) * Mat4::from_scale(scale);
    if m.is_finite() { m } else { Mat4::IDENTITY }
}

/// Blender stores quaternions as (w, x, y, z).
fn quat_wxyz(q: [f32; 4]) -> Quat {
    let q = Quat::from_xyzw(q[1], q[2], q[3], q[0]);
    if q.length_squared() > 1e-12 && q.is_finite() { q.normalize() } else { Quat::IDENTITY }
}

/// Euler rotation in one of Blender's orders; the name lists the axes in the order
/// they are applied (XYZ = X first), i.e. `R = R_last * R_middle * R_first`.
fn euler(e: Vec3, mode: i64) -> Mat3 {
    let (x, y, z) = (Mat3::from_rotation_x(e.x), Mat3::from_rotation_y(e.y), Mat3::from_rotation_z(e.z));
    match mode {
        2 => y * z * x, // XZY
        3 => z * x * y, // YXZ
        4 => x * z * y, // YZX
        5 => y * x * z, // ZXY
        6 => x * y * z, // ZYX
        _ => z * y * x, // XYZ
    }
}

fn active_scene<'f>(f: &'f Blend<'f>) -> Option<View<'f>> {
    let cur = f.global().and_then(|g| g.target("curscene")).filter(|&b| f.code(b) == b"SC\0\0");
    let b = cur.or_else(|| f.blocks.iter().position(|b| &b.code == b"SC\0\0").map(|i| i as u32))?;
    f.block_view(b)
}

// ---------------------------------------------------------------------------
// Mesh data
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Bool,
    Int32,
    Float2,
    Float3,
    Other,
}

/// One attribute / CustomData layer of a mesh, normalised across storage layouts.
struct Attr<'f> {
    name: String,
    domain: u8,
    kind: Kind,
    /// CustomData layer type, -1 for 5.x AttributeStorage.
    cd_type: i64,
    data: &'f [u8],
    /// One value for the whole domain (5.x single-value storage).
    single: bool,
    active_rnd: i64,
}

impl Attr<'_> {
    fn elem_size(&self) -> usize {
        match self.kind {
            Kind::Bool => 1,
            Kind::Int32 => 4,
            Kind::Float2 => 8,
            Kind::Float3 => 12,
            Kind::Other => 0,
        }
    }
    /// Raw bytes of `n` elements (`None` if the data is too short).
    fn elems(&self, n: usize) -> Option<Vec<&[u8]>> {
        let s = self.elem_size();
        if s == 0 {
            return None;
        }
        if self.single {
            let v = self.data.get(..s)?;
            return Some(vec![v; n]);
        }
        let need = n.checked_mul(s)?;
        Some(self.data.get(..need)?.chunks_exact(s).collect())
    }
    fn f32x3(&self, n: usize) -> Option<Vec<[f32; 3]>> {
        (self.kind == Kind::Float3).then_some(())?;
        if !self.single {
            let b = self.data.get(..n.checked_mul(12)?)?;
            return Some(b.as_chunks::<12>().0.iter().map(|c| [f32_at(c, 0), f32_at(c, 4), f32_at(c, 8)]).collect());
        }
        self.elems(n).map(|v| v.into_iter().map(|c| [f32_at(c, 0), f32_at(c, 4), f32_at(c, 8)]).collect())
    }
    fn f32x2(&self, n: usize) -> Option<Vec<[f32; 2]>> {
        (self.kind == Kind::Float2).then_some(())?;
        if !self.single {
            let b = self.data.get(..n.checked_mul(8)?)?;
            return Some(b.as_chunks::<8>().0.iter().map(|c| [f32_at(c, 0), f32_at(c, 4)]).collect());
        }
        self.elems(n).map(|v| v.into_iter().map(|c| [f32_at(c, 0), f32_at(c, 4)]).collect())
    }
    fn i32s(&self, n: usize) -> Option<Vec<i32>> {
        (self.kind == Kind::Int32).then_some(())?;
        if !self.single {
            let b = self.data.get(..n.checked_mul(4)?)?;
            return Some(b.as_chunks::<4>().0.iter().map(|c| i32::from_le_bytes(*c)).collect());
        }
        self.elems(n).map(|v| v.into_iter().map(|c| i32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect())
    }
    fn bools(&self, n: usize) -> Option<Vec<bool>> {
        (self.kind == Kind::Bool).then_some(())?;
        self.elems(n).map(|v| v.into_iter().map(|c| c[0] != 0).collect())
    }
}

fn f32_at(c: &[u8], o: usize) -> f32 {
    f32::from_le_bytes([c[o], c[o + 1], c[o + 2], c[o + 3]])
}

fn mesh_attrs<'f>(me: &View<'f>) -> Vec<Attr<'f>> {
    let f = me.f;
    let mut out = Vec::new();
    // CustomData layers (2.8 .. 4.x; 5.x keeps the structs but leaves them empty).
    let domains = [
        (DOM_POINT, ["vdata", "vert_data"]),
        (DOM_EDGE, ["edata", "edge_data"]),
        (DOM_FACE, ["pdata", "face_data"]),
        (DOM_CORNER, ["ldata", "corner_data"]),
    ];
    if let Some(layer_st) = f.struct_id("CustomDataLayer") {
        for (domain, names) in domains {
            let Some(cd) = names.iter().find_map(|n| me.sub(n)) else { continue };
            let n = cd.int("totlayer").unwrap_or(0).max(0) as usize;
            let Some(b) = cd.target("layers") else { continue };
            for layer in f.array(b, layer_st).take(n) {
                let cd_type = layer.int("type").unwrap_or(-1);
                let kind = match cd_type {
                    CD_PROP_FLOAT3 => Kind::Float3,
                    CD_PROP_FLOAT2 => Kind::Float2,
                    CD_PROP_INT32 => Kind::Int32,
                    CD_PROP_BOOL => Kind::Bool,
                    _ => Kind::Other,
                };
                out.push(Attr {
                    name: layer.string("name"),
                    domain,
                    kind,
                    cd_type,
                    data: layer.target("data").map(|b| f.bytes(b)).unwrap_or(&[]),
                    single: false,
                    active_rnd: layer.int("active_rnd").unwrap_or(0),
                });
            }
        }
    }
    // 5.x AttributeStorage.
    if let (Some(stg), Some(attr_st)) = (me.sub("attribute_storage"), f.struct_id("Attribute")) {
        let n = stg.int("dna_attributes_num").unwrap_or(0).max(0) as usize;
        if let Some(b) = stg.target("dna_attributes") {
            for a in f.array(b, attr_st).take(n) {
                let single = a.int("storage_type") == Some(1);
                let holder = a.deref_as("data", if single { "AttributeSingle" } else { "AttributeArray" });
                let data = holder.and_then(|h| h.target("data")).map(|b| f.bytes(b)).unwrap_or(&[]);
                let kind = match a.int("data_type").unwrap_or(-1) {
                    0 => Kind::Bool,
                    3 => Kind::Int32,
                    6 => Kind::Float2,
                    7 => Kind::Float3,
                    _ => Kind::Other,
                };
                out.push(Attr {
                    name: a.string_ptr("name").unwrap_or_default(),
                    domain: a.int("domain").unwrap_or(-1) as u8,
                    kind,
                    cd_type: -1,
                    data,
                    single,
                    active_rnd: 0,
                });
            }
        }
    }
    out
}

fn find_attr<'a, 'f>(attrs: &'a [Attr<'f>], domain: u8, name: &str, kind: Kind) -> Option<&'a Attr<'f>> {
    attrs.iter().find(|a| a.domain == domain && a.kind == kind && a.name == name)
}

/// Legacy struct array (MVert, MPoly, MLoop, MEdge, MLoopUV) from a mesh pointer or a CustomData layer.
struct Legacy<'f> {
    data: &'f [u8],
    stride: usize,
    st: u32,
}

impl<'f> Legacy<'f> {
    fn new(me: &View<'f>, ptr_name: &str, attrs: &[Attr<'f>], domain: u8, cd_type: i64, st_name: &str) -> Option<Self> {
        let f = me.f;
        let st = f.struct_id(st_name)?;
        let stride = f.sdna.structs[st as usize].size;
        let data = me
            .target(ptr_name)
            .map(|b| f.bytes(b))
            .or_else(|| attrs.iter().find(|a| a.domain == domain && a.cd_type == cd_type).map(|a| a.data))?;
        (stride > 0).then_some(Legacy { data, stride, st })
    }
    /// Element count that fits the data.
    fn len(&self) -> usize {
        self.data.len() / self.stride
    }
    /// (offset, prim) of a field.
    fn field(&self, f: &Blend, name: &str) -> Option<(usize, Prim, usize)> {
        let fl = f.sdna.structs[self.st as usize].fields.iter().find(|fl| fl.name == name && !fl.ptr)?;
        Some((fl.offset, f.sdna.prims[fl.ty as usize], fl.array))
    }
    fn ints(&self, f: &Blend, name: &str, n: usize) -> Option<Vec<i64>> {
        let (off, p, _) = self.field(f, name)?;
        (n <= self.len()).then_some(())?;
        (0..n).map(|i| p.read_i64(self.data.get(i * self.stride + off..)?)).collect()
    }
    fn floats<const N: usize>(&self, f: &Blend, name: &str, n: usize) -> Option<Vec<[f32; N]>> {
        let (off, p, arr) = self.field(f, name)?;
        (p == Prim::F32 && arr >= N && n <= self.len()).then_some(())?;
        (0..n)
            .map(|i| {
                let base = i * self.stride + off;
                let mut v = [0.0f32; N];
                for (k, x) in v.iter_mut().enumerate() {
                    *x = rd_f32(self.data, base + k * 4)?;
                }
                Some(v)
            })
            .collect()
    }
}

struct MeshData {
    positions: Vec<[f32; 3]>,
    /// (first corner, corner count) per face.
    faces: Vec<[u32; 2]>,
    corner_verts: Vec<u32>,
    corner_edges: Option<Vec<u32>>,
    edges: usize,
    uvs: Option<Vec<[f32; 2]>>,
    mat_index: Option<Vec<i32>>,
    /// Per-face flat shading; `None` = all smooth.
    flat: Option<Vec<bool>>,
    sharp_edges: Option<Vec<bool>>,
    /// Pre-4.1 auto smooth angle (radians).
    auto_smooth: Option<f32>,
    slots: usize,
    custom_normals: bool,
}

fn extract_mesh(me: &View, version: u32) -> std::result::Result<MeshData, String> {
    let f = me.f;
    let count = |names: &[&str]| names.iter().find_map(|n| me.int(n)).unwrap_or(0).max(0) as usize;
    let nv = count(&["totvert", "verts_num"]);
    let ne = count(&["totedge", "edges_num"]);
    let nf = count(&["totpoly", "faces_num"]);
    let nc = count(&["totloop", "corners_num"]);
    if [nv, ne, nf, nc].iter().any(|&n| n > f.data.len()) {
        return Err("element counts exceed the file size".into());
    }
    let attrs = mesh_attrs(me);

    // Positions: "position" attribute (3.5+) or MVert.co.
    let positions = match find_attr(&attrs, DOM_POINT, "position", Kind::Float3) {
        Some(a) => a.f32x3(nv),
        None => Legacy::new(me, "mvert", &attrs, DOM_POINT, CD_MVERT, "MVert").and_then(|l| l.floats::<3>(f, "co", nv)),
    }
    .ok_or("vertex positions missing")?;

    // Faces: offset array (3.6+) or MPoly; MPoly also carries material index and smooth flag.
    let mut faces = Vec::new();
    let mut legacy_mat = None;
    let mut legacy_flag = None;
    let offsets = me.target("face_offset_indices").or_else(|| me.target("poly_offset_indices"));
    if let Some(b) = offsets.filter(|_| nf > 0) {
        let d = f.bytes(b);
        if d.len() / 4 <= nf {
            return Err("face offsets truncated".into());
        }
        let off: Vec<i32> = d[..(nf + 1) * 4].as_chunks::<4>().0.iter().map(|c| i32::from_le_bytes(*c)).collect();
        for w in off.windows(2) {
            let (s, e) = (w[0].max(0) as u32, w[1].max(0) as u32);
            faces.push([s, e.saturating_sub(s)]);
        }
    } else if nf > 0 {
        let mp = Legacy::new(me, "mpoly", &attrs, DOM_FACE, CD_MPOLY, "MPoly").ok_or("faces missing")?;
        let start = mp.ints(f, "loopstart", nf).ok_or("faces truncated")?;
        let len = mp.ints(f, "totloop", nf).ok_or("faces truncated")?;
        faces = start.iter().zip(&len).map(|(&s, &l)| [s.clamp(0, u32::MAX as i64) as u32, l.clamp(0, u32::MAX as i64) as u32]).collect();
        legacy_mat = mp.ints(f, "mat_nr", nf);
        legacy_flag = mp.ints(f, "flag", nf);
    }

    // Corners: ".corner_vert" / ".corner_edge" (3.6+) or MLoop.v / MLoop.e.
    let mloop = Legacy::new(me, "mloop", &attrs, DOM_CORNER, CD_MLOOP, "MLoop");
    let to_u32 = |v: Vec<i32>| v.into_iter().map(|x| x as u32).collect::<Vec<u32>>();
    let corner_verts = match find_attr(&attrs, DOM_CORNER, ".corner_vert", Kind::Int32) {
        Some(a) => a.i32s(nc).map(to_u32),
        None => mloop.as_ref().and_then(|l| l.ints(f, "v", nc)).map(|v| v.into_iter().map(|x| x as u32).collect()),
    }
    .ok_or("face corners missing")?;
    let corner_edges = match find_attr(&attrs, DOM_CORNER, ".corner_edge", Kind::Int32) {
        Some(a) => a.i32s(nc).map(to_u32),
        None => mloop.as_ref().and_then(|l| l.ints(f, "e", nc)).map(|v| v.into_iter().map(|x| x as u32).collect()),
    };

    // UVs: float2 corner attributes (3.5+), preferring the render-active map; else MLoopUV.
    let uv_layers: Vec<&Attr> =
        attrs.iter().filter(|a| a.domain == DOM_CORNER && a.kind == Kind::Float2 && !a.name.starts_with('.')).collect();
    let uvs = if !uv_layers.is_empty() {
        let wanted = me.string_ptr("default_uv_map_attribute").or_else(|| me.string_ptr("active_uv_map_attribute"));
        let by_name = wanted.and_then(|w| uv_layers.iter().find(|a| a.name == w));
        let by_index = uv_layers.iter().filter(|a| a.cd_type >= 0).nth(uv_layers[0].active_rnd.max(0) as usize);
        by_name.or(by_index).unwrap_or(&uv_layers[0]).f32x2(nc)
    } else {
        let mloopuv: Vec<&Attr> = attrs.iter().filter(|a| a.domain == DOM_CORNER && a.cd_type == CD_MLOOPUV).collect();
        let st = f.struct_id("MLoopUV");
        let pick = mloopuv.get(mloopuv.first().map_or(0, |a| a.active_rnd.max(0) as usize)).or(mloopuv.first());
        match (pick, st) {
            (Some(a), Some(st)) => {
                let l = Legacy { data: a.data, stride: f.sdna.structs[st as usize].size, st };
                l.floats::<2>(f, "uv", nc)
            }
            _ => Legacy::new(me, "mloopuv", &[], DOM_CORNER, -2, "MLoopUV").and_then(|l| l.floats::<2>(f, "uv", nc)),
        }
    };

    let mat_index = find_attr(&attrs, DOM_FACE, "material_index", Kind::Int32)
        .and_then(|a| a.i32s(nf))
        .or_else(|| legacy_mat.map(|v| v.into_iter().map(|x| x as i32).collect()));
    let flat = find_attr(&attrs, DOM_FACE, "sharp_face", Kind::Bool)
        .and_then(|a| a.bools(nf))
        .or_else(|| legacy_flag.map(|v| v.into_iter().map(|x| x & ME_SMOOTH == 0).collect()))
        .filter(|v| v.iter().any(|&x| x));

    // Before 4.1 sharp edges only split normals with auto smooth enabled.
    let auto_smooth = (version < 401 && me.int("flag").unwrap_or(0) & ME_AUTOSMOOTH != 0)
        .then(|| me.float("smoothresh").filter(|a| a.is_finite()).unwrap_or(std::f32::consts::FRAC_PI_6));
    let sharp_edges = if version >= 401 || auto_smooth.is_some() {
        find_attr(&attrs, DOM_EDGE, "sharp_edge", Kind::Bool).and_then(|a| a.bools(ne)).or_else(|| {
            let medge = Legacy::new(me, "medge", &attrs, DOM_EDGE, CD_MEDGE, "MEdge")?;
            Some(medge.ints(f, "flag", ne)?.into_iter().map(|x| x & ME_SHARP != 0).collect())
        })
    } else {
        None
    }
    .filter(|v: &Vec<bool>| v.iter().any(|&x| x));

    let custom_normals = attrs.iter().any(|a| a.cd_type == CD_CUSTOMLOOPNORMAL || a.name == "custom_normal");
    let slots = me.int("totcol").unwrap_or(0).clamp(0, 32767) as usize;
    Ok(MeshData {
        positions,
        faces,
        corner_verts,
        corner_edges,
        edges: ne,
        uvs,
        mat_index,
        flat,
        sharp_edges,
        auto_smooth,
        slots,
        custom_normals,
    })
}

// ---------------------------------------------------------------------------
// Triangulation and normals
// ---------------------------------------------------------------------------

fn uf_find(p: &mut [u32], mut x: usize) -> usize {
    while p[x] as usize != x {
        let g = p[p[x] as usize];
        p[x] = g;
        x = g as usize;
    }
    x
}

fn uf_union(p: &mut [u32], a: usize, b: usize) {
    let (ra, rb) = (uf_find(p, a), uf_find(p, b));
    if ra != rb {
        p[ra.max(rb)] = ra.min(rb) as u32;
    }
}

/// Built geometry of one Blender mesh: one output mesh per used material slot.
struct Built {
    parts: Vec<(usize, Mesh)>,
    bad_faces: usize,
    custom_normals: bool,
}

/// Fan-triangulate, split corners into smooth fans (separated by flat faces, sharp
/// and non-manifold edges, and the auto-smooth angle) and give each fan an
/// angle-weighted normal, then deduplicate output vertices by (fan, UV).
fn build_mesh(md: &MeshData) -> Built {
    let nv = md.positions.len();
    let nc = md.corner_verts.len();
    let cv = &md.corner_verts;
    let mut bad = 0usize;
    let valid: Vec<bool> = md
        .faces
        .iter()
        .map(|&[s, l]| {
            let (s, l) = (s as usize, l as usize);
            let ok = l >= 3 && s + l <= nc && cv[s..s + l].iter().all(|&v| (v as usize) < nv);
            bad += usize::from(!ok);
            ok
        })
        .collect();
    let pos = |c: usize| Vec3::from(md.positions[cv[c] as usize]);
    let fnorm: Vec<Vec3> = md
        .faces
        .iter()
        .zip(&valid)
        .map(|(&[s, l], &ok)| {
            if !ok {
                return Vec3::Z;
            }
            let (s, l) = (s as usize, l as usize);
            let p0 = pos(s);
            let mut n = Vec3::ZERO;
            for k in 1..l - 1 {
                n += (pos(s + k) - p0).cross(pos(s + k + 1) - p0);
            }
            n.try_normalize().unwrap_or(Vec3::Z)
        })
        .collect();
    let is_flat = |fi: usize| md.flat.as_ref().is_some_and(|v| v.get(fi).copied().unwrap_or(false));

    let mut parent: Vec<u32> = (0..nc as u32).collect();
    if (0..md.faces.len()).any(|fi| valid[fi] && !is_flat(fi)) {
        link_smooth_fans(md, &valid, &fnorm, &is_flat, &mut parent);
    }

    // Angle-weighted normal per fan (flat-face corners are fans of one).
    let mut acc = vec![Vec3::ZERO; nc];
    for (fi, &[s, l]) in md.faces.iter().enumerate() {
        if !valid[fi] {
            continue;
        }
        let (s, l) = (s as usize, l as usize);
        for k in 0..l {
            let c = s + k;
            let p = pos(c);
            let a = (pos(s + (k + l - 1) % l) - p).normalize_or_zero();
            let b = (pos(s + (k + 1) % l) - p).normalize_or_zero();
            let angle = a.dot(b).clamp(-1.0, 1.0).acos();
            let r = uf_find(&mut parent, c);
            acc[r] += fnorm[fi] * angle;
        }
    }

    let nslots = md.slots.max(1);
    let mut parts: Vec<Option<Mesh>> = (0..nslots).map(|_| None).collect();
    // Output vertex records chained per fan root: (slot, index, uv bits, next).
    let mut head = vec![u32::MAX; nc];
    let mut recs: Vec<(u32, u32, [u32; 2], u32)> = Vec::with_capacity(nc.min(nv * 2 + 16));
    let mut fan = Vec::new();
    for (fi, &[s, l]) in md.faces.iter().enumerate() {
        if !valid[fi] {
            continue;
        }
        let (s, l) = (s as usize, l as usize);
        let slot = md.mat_index.as_ref().and_then(|m| m.get(fi)).map_or(0, |&m| m.clamp(0, nslots as i32 - 1) as usize);
        let mesh = parts[slot].get_or_insert_with(|| Mesh::new("", Topology::Triangles));
        fan.clear();
        for c in s..s + l {
            let r = uf_find(&mut parent, c);
            let uv = md.uvs.as_ref().map(|u| u[c]);
            let key = uv.map_or([0; 2], |u| [u[0].to_bits(), u[1].to_bits()]);
            let mut i = head[r];
            let mut found = u32::MAX;
            while i != u32::MAX {
                let rec = recs[i as usize];
                if rec.0 == slot as u32 && rec.2 == key {
                    found = rec.1;
                    break;
                }
                i = rec.3;
            }
            if found == u32::MAX {
                found = mesh.positions.len() as u32;
                mesh.positions.push(md.positions[cv[c] as usize]);
                mesh.normals.push(acc[r].try_normalize().unwrap_or(fnorm[fi]).to_array());
                if let Some(u) = uv {
                    mesh.uvs.push([u[0], 1.0 - u[1]]);
                }
                recs.push((slot as u32, found, key, head[r]));
                head[r] = recs.len() as u32 - 1;
            }
            fan.push(found);
        }
        for k in 1..l - 1 {
            mesh.indices.extend_from_slice(&[fan[0], fan[k], fan[k + 1]]);
        }
    }
    let parts = parts.into_iter().enumerate().filter_map(|(i, m)| m.map(|m| (i, m))).collect();
    Built { parts, bad_faces: bad, custom_normals: md.custom_normals }
}

/// Union the corners of neighbouring smooth faces around shared, smooth, manifold edges.
fn link_smooth_fans(md: &MeshData, valid: &[bool], fnorm: &[Vec3], is_flat: &dyn Fn(usize) -> bool, parent: &mut [u32]) {
    let nc = md.corner_verts.len();
    let cv = &md.corner_verts;
    let mut corner_face = vec![u32::MAX; nc];
    for (fi, &[s, l]) in md.faces.iter().enumerate() {
        if valid[fi] {
            corner_face[s as usize..(s + l) as usize].fill(fi as u32);
        }
    }
    let next = |c: usize| {
        let [s, l] = md.faces[corner_face[c] as usize];
        if c + 1 == (s + l) as usize { s as usize } else { c + 1 }
    };
    // Edge id per corner (edge from the corner to the next one): Blender's own edge
    // indices when present (needed for sharp edges), else ids from vertex pairs.
    // An edge count far above the corner count (loose edges, corrupt data) is not
    // worth a per-edge table.
    let real = md.corner_edges.as_ref().filter(|ce| {
        ce.len() == nc && md.edges <= nc * 2 + 1024 && (0..nc).all(|c| corner_face[c] == u32::MAX || (ce[c] as usize) < md.edges)
    });
    let (edge_of, nedges): (Cow<[u32]>, usize) = match real {
        Some(ce) => (Cow::Borrowed(&ce[..]), md.edges),
        None => {
            let mut ids: FxMap<(u32, u32), u32> = FxMap::default();
            let mut e = vec![u32::MAX; nc];
            for c in 0..nc {
                if corner_face[c] == u32::MAX {
                    continue;
                }
                let (a, b) = (cv[c], cv[next(c)]);
                let n = ids.len() as u32;
                e[c] = *ids.entry((a.min(b), a.max(b))).or_insert(n);
            }
            let n = ids.len();
            (Cow::Owned(e), n)
        }
    };
    // (first corner, second corner, use count) per edge.
    let mut uses = vec![(u32::MAX, u32::MAX, 0u8); nedges];
    for c in 0..nc {
        if corner_face[c] == u32::MAX {
            continue;
        }
        let u = &mut uses[edge_of[c] as usize];
        match u.2 {
            0 => u.0 = c as u32,
            1 => u.1 = c as u32,
            _ => {}
        }
        u.2 = u.2.saturating_add(1);
    }
    let cos_limit = md.auto_smooth.map(f32::cos);
    let sharp = if real.is_some() { md.sharp_edges.as_ref() } else { None };
    for (e, &(c1, c2, n)) in uses.iter().enumerate() {
        if n != 2 || sharp.is_some_and(|s| s.get(e).copied().unwrap_or(false)) {
            continue;
        }
        let (c1, c2) = (c1 as usize, c2 as usize);
        let (f1, f2) = (corner_face[c1] as usize, corner_face[c2] as usize);
        if f1 == f2 || is_flat(f1) || is_flat(f2) {
            continue;
        }
        if cos_limit.is_some_and(|cl| fnorm[f1].dot(fnorm[f2]) < cl) {
            continue;
        }
        let (a0, a1, b0, b1) = (c1, next(c1), c2, next(c2));
        if cv[a0] == cv[b1] && cv[a1] == cv[b0] {
            uf_union(parent, a0, b1);
            uf_union(parent, a1, b0);
        } else if cv[a0] == cv[b0] && cv[a1] == cv[b1] {
            uf_union(parent, a0, b0);
            uf_union(parent, a1, b1);
        }
    }
}

// ---------------------------------------------------------------------------
// Materials
// ---------------------------------------------------------------------------

/// Material data-block per slot, honouring object-level slot overrides (`matbits`).
fn material_slots(ob: &View, me: &View) -> Vec<Option<u32>> {
    let f = me.f;
    let ptrs = |v: &View, n: usize| -> Vec<Option<u32>> {
        let Some(b) = v.target("mat") else { return vec![None; n] };
        let d = f.bytes(b);
        (0..n).map(|i| f.read_ptr(d, i * f.ptr_size).and_then(|p| f.resolve(b, p)).filter(|&m| f.code(m) == b"MA\0\0")).collect()
    };
    let n = me.int("totcol").unwrap_or(0).clamp(0, 32767) as usize;
    let me_mats = ptrs(me, n);
    let ob_n = ob.int("totcol").unwrap_or(0).clamp(0, 32767) as usize;
    let ob_mats = ptrs(ob, ob_n);
    let bits = ob.target("matbits").map(|b| f.bytes(b)).unwrap_or(&[]);
    (0..n).map(|i| if bits.get(i).is_some_and(|&x| x != 0) && i < ob_n { ob_mats[i].or(me_mats[i]) } else { me_mats[i] }).collect()
}

#[derive(Default)]
struct Principled {
    base: Option<[f32; 4]>,
    metallic: Option<f32>,
    roughness: Option<f32>,
    alpha: Option<f32>,
    emission: Option<[f32; 4]>,
    emission_strength: Option<f32>,
}

/// Material plus whether its node tree samples image textures.
fn read_material(ma: &View) -> (Material, bool) {
    let mut m = Material { name: ma.id_name(), ..Default::default() };
    if let (Some(r), Some(g), Some(b)) = (ma.float("r"), ma.float("g"), ma.float("b")) {
        let a = ma.float("a").or_else(|| ma.float("alpha")).unwrap_or(1.0);
        m.base_color = [r, g, b, a];
    }
    if let Some(v) = ma.float("metallic") {
        m.metallic = v;
    }
    if let Some(v) = ma.float("roughness") {
        m.roughness = v;
    }
    let (p, textured) = principled(ma);
    if let Some(p) = p {
        if let Some(c) = p.base {
            m.base_color = [c[0], c[1], c[2], m.base_color[3]];
        }
        if let Some(a) = p.alpha {
            m.base_color[3] = a;
        }
        m.metallic = p.metallic.unwrap_or(m.metallic);
        m.roughness = p.roughness.unwrap_or(m.roughness);
        let strength = p.emission_strength.unwrap_or(1.0);
        if let Some(e) = p.emission.filter(|_| strength > 0.0) {
            m.emissive = [e[0] * strength, e[1] * strength, e[2] * strength];
        }
    }
    let clean = |v: f32, d: f32| if v.is_finite() { v.clamp(0.0, 1.0) } else { d };
    m.base_color = m.base_color.map(|c| clean(c, 0.8));
    m.metallic = clean(m.metallic, 0.0);
    m.roughness = clean(m.roughness, 0.6);
    m.emissive = m.emissive.map(|c| if c.is_finite() { c.max(0.0) } else { 0.0 });
    if m.base_color[3] < 1.0 {
        m.alpha_mode = AlphaMode::Blend;
    }
    (m, textured)
}

/// Unlinked input defaults of the Principled BSDF feeding the material output.
fn principled(ma: &View) -> (Option<Principled>, bool) {
    // Before 5.0 a material with "Use Nodes" off renders its viewport colour.
    if ma.f.version < 500 && ma.int("use_nodes") == Some(0) {
        return (None, false);
    }
    let Some(nt) = ma.deref("nodetree") else { return (None, false) };
    let nodes = nt.list("nodes", "bNode");
    let textured = nodes.iter().any(|n| n.string("idname") == "ShaderNodeTexImage");
    let bsdfs: Vec<&View> = nodes.iter().filter(|n| n.string("idname") == "ShaderNodeBsdfPrincipled").collect();
    let Some(&&first) = bsdfs.first() else { return (None, textured) };
    let old = |v: &View| ma.f.blocks[v.block as usize].old;
    let mut node = first;
    if bsdfs.len() > 1 {
        let outputs: HashSet<u64> = nodes.iter().filter(|n| n.string("idname") == "ShaderNodeOutputMaterial").map(old).collect();
        let links = nt.list("links", "bNodeLink");
        if let Some(&&b) = bsdfs.iter().find(|b| links.iter().any(|l| l.ptr("fromnode") == old(b) && outputs.contains(&l.ptr("tonode")))) {
            node = b;
        }
    }
    let mut p = Principled::default();
    for s in node.list("inputs", "bNodeSocket") {
        let (id, name) = (s.string("identifier"), s.string("name"));
        let is = |k: &str| id == k || name == k;
        let rgba = || s.deref_as("default_value", "bNodeSocketValueRGBA").and_then(|v| v.floats::<4>("value"));
        let float = || s.deref_as("default_value", "bNodeSocketValueFloat").and_then(|v| v.float("value"));
        if is("Base Color") {
            p.base = rgba();
        } else if is("Metallic") {
            p.metallic = float();
        } else if is("Roughness") {
            p.roughness = float();
        } else if is("Alpha") {
            p.alpha = float();
        } else if is("Emission Color") || (is("Emission") && s.int("type") == Some(SOCK_RGBA)) {
            p.emission = rgba();
        } else if is("Emission Strength") {
            p.emission_strength = float();
        }
    }
    (Some(p), textured)
}

// ---------------------------------------------------------------------------
// Scene assembly
// ---------------------------------------------------------------------------

fn load_scene(f: &Blend, warnings: &mut Vec<String>) -> Result<Scene> {
    let mut w = Walker { f, worlds: HashMap::new(), top_objects: HashSet::new(), stats: Stats::default(), insts: Vec::new() };
    let mut scene = Scene::default();
    let sc = active_scene(f);
    if let Some(sc) = sc {
        scene.info.extra.push(("scene".into(), sc.id_name()));
        if let Some(unit) = sc.sub("unit")
            && let Some(s) = unit.float("scale_length").filter(|s| (s - 1.0).abs() > 1e-6 && *s > 0.0)
        {
            scene.info.extra.push(("unit scale".into(), format!("{s}")));
        }
        let mut objs = std::mem::take(&mut w.top_objects);
        if let Some(master) = sc.deref("master_collection") {
            w.walk_collection(master, None, 0, &mut HashSet::new(), &mut objs);
        } else {
            // 2.7x: objects are listed as scene bases.
            for base in sc.list("base", "Base") {
                if let Some(b) = base.target("object") {
                    w.visit_block(b, None, 0, &mut objs);
                }
            }
        }
        w.top_objects = objs;
    }
    let has_mesh_objects = f
        .blocks
        .iter()
        .enumerate()
        .any(|(i, b)| &b.code == b"OB\0\0" && f.block_view(i as u32).and_then(|o| o.int("type")) == Some(OB_MESH));
    if w.insts.is_empty() && has_mesh_objects {
        // No scene (e.g. a library file written by `bpy.data.libraries.write`) or the
        // scene shows no meshes: show every object stored in the file.
        if sc.is_some() {
            warnings.push("the active scene shows no meshes; showing all objects in the file".into());
        }
        w.stats = Stats::default();
        let mut objs = HashSet::new();
        for i in 0..f.blocks.len() as u32 {
            if f.code(i) == b"OB\0\0" {
                w.visit_block(i, None, 0, &mut objs);
            }
        }
    }

    // Build every distinct mesh once, in parallel.
    let mut mesh_ids: Vec<u32> = Vec::new();
    let mut mesh_slot: HashMap<u32, usize> = HashMap::new();
    let mut uses: Vec<usize> = Vec::new();
    for inst in &w.insts {
        let i = *mesh_slot.entry(inst.mesh).or_insert_with(|| {
            mesh_ids.push(inst.mesh);
            uses.push(0);
            mesh_ids.len() - 1
        });
        uses[i] += 1;
    }
    let results: Vec<Mutex<Option<std::result::Result<Built, String>>>> = (0..mesh_ids.len()).map(|_| Mutex::new(None)).collect();
    let next = AtomicUsize::new(0);
    let threads = std::thread::available_parallelism().map(|c| c.get()).unwrap_or(4).min(mesh_ids.len().max(1));
    std::thread::scope(|s| {
        for _ in 0..threads {
            s.spawn(|| {
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    if i >= mesh_ids.len() {
                        break;
                    }
                    let r = f.block_view(mesh_ids[i]).ok_or_else(|| "unreadable mesh block".to_string());
                    let r = r.and_then(|me| extract_mesh(&me, f.version)).map(|md| build_mesh(&md));
                    *results[i].lock().unwrap() = Some(r);
                }
            });
        }
    });
    let mut built: Vec<Option<Built>> = Vec::with_capacity(results.len());
    let (mut bad_faces, mut custom_normals, mut faceless, mut shape_keys) = (0usize, 0usize, 0usize, 0usize);
    for (i, r) in results.into_iter().enumerate() {
        match r.into_inner().unwrap() {
            Some(Ok(b)) => {
                bad_faces += b.bad_faces;
                custom_normals += usize::from(b.custom_normals);
                faceless += usize::from(b.parts.is_empty());
                shape_keys += usize::from(f.block_view(mesh_ids[i]).is_some_and(|me| me.ptr("key") != 0));
                built.push(Some(b));
            }
            Some(Err(e)) => {
                let name = f.block_view(mesh_ids[i]).map(|v| v.id_name()).unwrap_or_default();
                warnings.push(format!("mesh '{name}': {e}"));
                built.push(None);
            }
            None => built.push(None),
        }
    }

    // Instantiate per object: material slots, names, world transforms.
    let mut mat_index: HashMap<u32, usize> = HashMap::new();
    let mut textured = 0usize;
    for inst in &w.insts {
        let mi = mesh_slot[&inst.mesh];
        uses[mi] -= 1;
        let last_use = uses[mi] == 0;
        let Some(geo) = built[mi].as_mut() else { continue };
        let (Some(ob), Some(me)) = (f.block_view(inst.ob), f.block_view(inst.mesh)) else { continue };
        let slots = material_slots(&ob, &me);
        let ob_name = Some(ob.id_name()).filter(|n| !n.is_empty()).unwrap_or_else(|| me.id_name());
        let multi = geo.parts.len() > 1;
        let parts: Vec<(usize, Mesh)> = if last_use { std::mem::take(&mut geo.parts) } else { geo.parts.clone() };
        for (slot, mut mesh) in parts {
            let mat = slots.get(slot).copied().flatten().and_then(|b| {
                if let Some(&i) = mat_index.get(&b) {
                    return Some(i);
                }
                let (m, tex) = read_material(&f.block_view(b)?);
                textured += usize::from(tex);
                scene.materials.push(m);
                mat_index.insert(b, scene.materials.len() - 1);
                Some(scene.materials.len() - 1)
            });
            mesh.name = match (multi, mat) {
                (true, Some(i)) => format!("{ob_name} [{}]", scene.materials[i].name),
                (true, None) => format!("{ob_name} [slot {}]", slot + 1),
                _ => ob_name.clone(),
            };
            mesh.material = mat;
            mesh.transform(&inst.world);
            scene.meshes.push(mesh);
        }
    }

    let st = &w.stats;
    let info = &mut scene.info;
    info.nodes = st.objects;
    info.cameras = st.cameras;
    info.lights = st.lights;
    if st.instances > 0 {
        info.extra.push(("collection instances".into(), st.instances.to_string()));
    }
    if st.hidden_objects + st.hidden_collections > 0 {
        info.extra.push(("hidden (not shown)".into(), format!("{} objects, {} collections", st.hidden_objects, st.hidden_collections)));
    }
    if st.modifiers > 0 {
        warnings.push(format!("modifiers are not applied ({} objects have modifiers)", st.modifiers));
    }
    if st.curves > 0 {
        warnings.push(format!("{} curve/text objects skipped", st.curves));
    }
    if st.metaballs > 0 {
        warnings.push(format!("{} metaball objects skipped", st.metaballs));
    }
    if st.other > 0 {
        warnings.push(format!("{} other non-mesh objects skipped (curves, point clouds, volumes, grease pencil ...)", st.other));
    }
    if st.linked > 0 {
        warnings.push(format!("{} objects use linked library data that is not stored in this file", st.linked));
    }
    if bad_faces > 0 {
        warnings.push(format!("{bad_faces} faces with invalid indices skipped"));
    }
    if faceless > 0 {
        warnings.push(format!("{faceless} meshes without faces (only vertices/edges) are not shown"));
    }
    if shape_keys > 0 {
        warnings.push(format!("shape keys are not applied ({shape_keys} meshes, basis shape shown)"));
    }
    if custom_normals > 0 {
        warnings.push(format!("custom split normals are ignored ({custom_normals} meshes)"));
    }
    if textured > 0 {
        warnings.push(format!("image textures are not loaded ({textured} materials use them)"));
    }
    if scene.meshes.is_empty() {
        let why = if warnings.is_empty() { String::new() } else { format!(" ({})", warnings.join("; ")) };
        bail!("file contains no mesh objects{why}");
    }
    Ok(scene)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn member_names() {
        assert_eq!(parse_member_name("*next"), ("next", true, 1));
        assert_eq!(parse_member_name("**mat"), ("mat", true, 1));
        assert_eq!(parse_member_name("co[3]"), ("co", false, 3));
        assert_eq!(parse_member_name("mat[4][4]"), ("mat", false, 16));
        assert_eq!(parse_member_name("(*func)()"), ("func", true, 1));
        assert_eq!(parse_member_name("*mtex[18]"), ("mtex", true, 18));
        assert_eq!(parse_member_name("name[258]"), ("name", false, 258));
        assert_eq!(parse_member_name("totvert"), ("totvert", false, 1));
    }

    #[test]
    fn headers() {
        let h = parse_header(b"BLENDER-v405REND").unwrap();
        assert_eq!((h.ptr_size, h.version, h.bhead, h.len), (8, 405, BHeadKind::Small8, 12));
        let h = parse_header(b"BLENDER_v279REND").unwrap();
        assert_eq!((h.ptr_size, h.version, h.bhead), (4, 279, BHeadKind::Small4));
        let h = parse_header(b"BLENDER17-01v0501REND").unwrap();
        assert_eq!((h.ptr_size, h.version, h.bhead, h.len), (8, 501, BHeadKind::Large8, 17));
        assert!(parse_header(b"BLENDER-V405REND").unwrap_err().to_string().contains("big-endian"));
        assert!(parse_header(b"BLENDER17-02v0600REND").is_err());
        assert!(parse_header(b"NOTBLENDERFILE").is_err());
        assert_eq!(version_string(280), "2.80");
        assert_eq!(version_string(405), "4.5");
        assert_eq!(version_string(501), "5.1");
    }

    #[test]
    fn euler_orders() {
        // XYZ applies X first: rotating +Y by 90 deg about X gives +Z, then about Z leaves +Z.
        let m = euler(Vec3::new(90f32.to_radians(), 0.0, 90f32.to_radians()), 1);
        assert!((m * Vec3::Y - Vec3::Z).length() < 1e-5);
        // ZXY applies Z first: +X -> +Y (Z), -> +Z (X), Y leaves +Z... then about Y: +Z -> +X.
        let m = euler(Vec3::new(90f32.to_radians(), 90f32.to_radians(), 90f32.to_radians()), 5);
        let expect = Mat3::from_rotation_y(90f32.to_radians())
            * Mat3::from_rotation_x(90f32.to_radians())
            * Mat3::from_rotation_z(90f32.to_radians());
        assert!((m * Vec3::X - expect * Vec3::X).length() < 1e-5);
        assert!((quat_wxyz([1.0, 0.0, 0.0, 0.0]) - Quat::IDENTITY).length() < 1e-6);
    }

    #[test]
    fn seek_table() {
        // Two fake frames (3 + 5 bytes) followed by a seek table and footer.
        let mut d = vec![0x28, 0xb5, 0x2f, 0xfd, 0, 0, 0, 1, 2, 3, 4];
        d.extend_from_slice(&0x184D_2A5Eu32.to_le_bytes());
        d.extend_from_slice(&(2u32 * 8 + 9).to_le_bytes());
        for (c, u) in [(3u32, 10u32), (8, 20)] {
            d.extend_from_slice(&c.to_le_bytes());
            d.extend_from_slice(&u.to_le_bytes());
        }
        d.extend_from_slice(&2u32.to_le_bytes());
        d.push(0);
        d.extend_from_slice(&0x8F92_EAB1u32.to_le_bytes());
        let t = zstd_seek_table(&d).unwrap();
        assert_eq!(t, vec![ZFrame { src: 0, src_len: 3, dst_len: 10 }, ZFrame { src: 3, src_len: 8, dst_len: 20 }]);
        assert!(zstd_seek_table(&d[..d.len() - 1]).is_none());
    }

    #[test]
    fn fans_split_at_flat_faces_and_seams() {
        // Two quads sharing an edge, both smooth: the shared edge vertices are merged.
        let md = MeshData {
            positions: vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [1.0, 1.0, 0.0], [0.0, 1.0, 0.0], [2.0, 0.0, 0.5], [2.0, 1.0, 0.5]],
            faces: vec![[0, 4], [4, 4]],
            corner_verts: vec![0, 1, 2, 3, 1, 4, 5, 2],
            corner_edges: None,
            edges: 0,
            uvs: None,
            mat_index: Some(vec![0, 1]),
            flat: None,
            sharp_edges: None,
            auto_smooth: None,
            slots: 2,
            custom_normals: false,
        };
        let b = build_mesh(&md);
        assert_eq!(b.parts.len(), 2);
        assert_eq!(b.parts.iter().map(|(_, m)| m.indices.len()).sum::<usize>(), 12);
        // The shared vertices get the same averaged normal in both material parts.
        let n0 = b.parts[0].1.normals[1];
        let n1 = b.parts[1].1.normals[0];
        assert!((Vec3::from(n0) - Vec3::from(n1)).length() < 1e-6);
        assert!(Vec3::from(n0).z < 0.999);
        // Flat faces keep their own face normals.
        let b = build_mesh(&MeshData { flat: Some(vec![true, true]), mat_index: None, slots: 1, ..md });
        assert_eq!(b.parts[0].1.positions.len(), 8);
        assert!((Vec3::from(b.parts[0].1.normals[0]) - Vec3::Z).length() < 1e-6);
    }

    /// Files generated by Blender 4.5 / 5.1 (zstd-compressed scenes; see tests/data);
    /// skipped when missing. Scene: flat cube (ZXY euler), linked duplicate with an
    /// object-level material, smooth UV sphere (quaternion) parented to a rotated and
    /// scaled empty with a parent inverse, monkey (axis-angle, two material slots,
    /// subdivision modifier), a cone instanced through a collection, a hidden object,
    /// a hidden collection, a camera, a light and a text object.
    #[test]
    fn blender_test_files() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data");
        for (name, version) in [("blend45_scene.blend", "4.5"), ("blend51_scene.blend", "5.1")] {
            let Ok(data) = std::fs::read(dir.join(name)) else {
                eprintln!("skipping {name}: not found");
                continue;
            };
            // The sequential zstd path must agree with the parallel seek-table one.
            let frames = zstd_seek_table(&data).expect("Blender writes a seek table");
            assert_eq!(zstd_parallel(&data, &frames).unwrap(), zstd_sequential(&data).unwrap(), "{name}");
            let s = parse_blend(&data).unwrap_or_else(|e| panic!("{name}: {e:#}"));
            assert_eq!(s.info.format, format!("Blender {version}"));
            assert!(s.info.extra.iter().any(|(k, v)| k == "compression" && v == "zstd"));
            assert_eq!((s.info.cameras, s.info.lights), (1, 1), "{name}");
            // Cube, linked cube, sphere, monkey (2 slots), instanced cone.
            assert_eq!(s.meshes.len(), 6, "{name}: {:?}", s.meshes.iter().map(|m| &m.name).collect::<Vec<_>>());
            assert_eq!(s.triangle_count(), 12 + 12 + 960 + 968 + 62, "{name}");
            assert_eq!(s.materials.len(), 5, "{name}");
            let b = s.bounds();
            let (lo, hi) = (Vec3::new(-4.277, -9.0, -2.0), Vec3::new(4.669, 7.409, 3.996));
            assert!((b.min - lo).abs().max_element() < 0.01 && (b.max - hi).abs().max_element() < 0.01, "{name}: {b:?}");
            let mat = |n: &str| s.materials.iter().find(|m| m.name == n).unwrap_or_else(|| panic!("{name}: no material {n}"));
            assert!((mat("Red").base_color[0] - 0.8).abs() < 1e-4 && mat("Red").base_color[1] < 0.06);
            // Colour only set on the Principled BSDF node.
            assert!((mat("Green").base_color[1] - 0.6).abs() < 1e-4);
            assert!((mat("Gold").metallic - 1.0).abs() < 1e-4);
            // The linked duplicate uses an object-level material override.
            let linked = s.meshes.iter().find(|m| m.name == "CubeLinked").unwrap();
            assert_eq!(s.materials[linked.material.unwrap()].name, "Blue");
            let cube = s.meshes.iter().find(|m| m.name == "Cube").unwrap();
            assert_eq!(cube.positions.len(), 24, "flat cube splits per face");
            assert_eq!(cube.uvs.len(), 24);
            let c = cube.positions.iter().map(|p| Vec3::from(*p)).sum::<Vec3>() / 24.0;
            for (p, n) in cube.positions.iter().zip(&cube.normals) {
                assert!(Vec3::from(*n).dot(Vec3::from(*p) - c) > 0.5, "{name}: cube normals point outwards");
            }
            let sphere = s.meshes.iter().find(|m| m.name == "Sphere").unwrap();
            assert!(sphere.positions.len() < 600, "{name}: smooth sphere shares vertices ({})", sphere.positions.len());
            let monkey: usize = s.meshes.iter().filter(|m| m.name.starts_with("Suzanne")).map(|m| m.triangle_count()).sum();
            assert_eq!(monkey, 968);
            assert!(s.info.warnings.iter().any(|w| w.contains("modifiers")), "{name}: {:?}", s.info.warnings);
            assert!(s.info.warnings.iter().any(|w| w.contains("1 curve/text")), "{name}: {:?}", s.info.warnings);
        }
        // gzip-compressed library file without a scene (written by `bpy.data.libraries.write`):
        // falls back to all objects in the file.
        let name = "blend45_small_gz.blend";
        if let Ok(data) = std::fs::read(dir.join(name)) {
            let s = parse_blend(&data).unwrap_or_else(|e| panic!("{name}: {e:#}"));
            assert!(s.info.extra.iter().any(|(k, v)| k == "compression" && v == "gzip"));
            assert_eq!(s.meshes.len(), 3, "{name}");
            assert_eq!(s.triangle_count(), 12 + 968, "{name}");
        }
    }
}
