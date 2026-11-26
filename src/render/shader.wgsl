// All scene shading in one module: meshes (every display mode), wireframe,
// depth-only, points (instanced quads), overlay lines and the background.

struct Frame {
    view_proj: mat4x4<f32>,
    inv_view_proj: mat4x4<f32>,
    cam_pos: vec4<f32>,
    light_dir: vec4<f32>,   // xyz: direction towards the key light, w: intensity
    up: vec4<f32>,          // world up axis
    wire_color: vec4<f32>,
    viewport: vec4<f32>,    // xy: size in px, z: point size in px
    bg_top: vec4<f32>,
    bg_bottom: vec4<f32>,
    clip: vec4<f32>,        // section plane: fragments with dot(p, xyz) > w are cut away
    env: vec4<f32>,         // x: intensity, y: rotation (rad), z: max mip, w: 1 = environment lighting
    env_bg: vec4<f32>,      // x: 1 = environment as background, y: background blur (mip)
    sh: array<vec4<f32>, 9>, // irradiance spherical harmonics
    mode: u32,
    flags: u32,
    _pad0: u32,
    _pad1: u32,
};

struct MaterialU {
    base_color: vec4<f32>,
    emissive: vec4<f32>,
    params: vec4<f32>,      // metallic, roughness, alpha_cutoff, alpha_mode (0 opaque, 1 mask, 2 blend)
    tex: vec4<u32>,         // x: texture presence bitmask
};

struct MeshU {
    model: mat4x4<f32>,
    normal_mat: mat4x4<f32>,
    flags: vec4<u32>,       // x: has_uv, y: has_color, z: highlight, w: has_normals
};

@group(0) @binding(0) var<uniform> frame: Frame;
@group(0) @binding(1) var env_tex: texture_2d<f32>;
@group(0) @binding(2) var env_samp: sampler;

@group(1) @binding(0) var<uniform> mat: MaterialU;
@group(1) @binding(1) var t_base: texture_2d<f32>;
@group(1) @binding(2) var t_normal: texture_2d<f32>;
@group(1) @binding(3) var t_mr: texture_2d<f32>;
@group(1) @binding(4) var t_occ: texture_2d<f32>;
@group(1) @binding(5) var t_emis: texture_2d<f32>;
@group(1) @binding(6) var samp: sampler;

@group(2) @binding(0) var<uniform> meshu: MeshU;

const PI: f32 = 3.14159265;

const MODE_SHADED: u32 = 0u;
const MODE_CLAY: u32 = 1u;
const MODE_UNLIT: u32 = 2u;
const MODE_NORMALS: u32 = 3u;
const MODE_UV: u32 = 4u;
const MODE_VCOLOR: u32 = 5u;

const FLAG_TEXTURES: u32 = 1u;
const FLAG_FLAT: u32 = 2u;
const FLAG_VCOLOR: u32 = 4u;
const FLAG_CLIP: u32 = 8u;

const TEX_BASE: u32 = 1u;
const TEX_NORMAL: u32 = 2u;
const TEX_MR: u32 = 4u;
const TEX_OCC: u32 = 8u;
const TEX_EMIS: u32 = 16u;

struct VIn {
    @location(0) pos: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) uv: vec2<f32>,
    @location(3) color: vec4<f32>,
};

struct VOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) world: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) uv: vec2<f32>,
    @location(3) color: vec4<f32>,
    @location(4) corner: vec2<f32>,
};

@vertex
fn vs_main(v: VIn) -> VOut {
    var o: VOut;
    let wp = meshu.model * vec4<f32>(v.pos, 1.0);
    o.clip = frame.view_proj * wp;
    o.world = wp.xyz;
    o.normal = (meshu.normal_mat * vec4<f32>(v.normal, 0.0)).xyz;
    o.uv = v.uv;
    o.color = v.color;
    o.corner = vec2<f32>(0.0);
    return o;
}

fn use_tex(bit: u32) -> bool {
    return (frame.flags & FLAG_TEXTURES) != 0u && (mat.tex.x & bit) != 0u && meshu.flags.x != 0u;
}

fn use_vcolor() -> bool {
    return meshu.flags.y != 0u && (frame.flags & FLAG_VCOLOR) != 0u;
}

fn clipping() -> bool {
    return (frame.flags & FLAG_CLIP) != 0u;
}

fn clipped(world: vec3<f32>) -> bool {
    return clipping() && dot(world, frame.clip.xyz) > frame.clip.w;
}

fn env_on() -> bool {
    return frame.env.w > 0.5;
}

// World direction -> canonical Y-up frame of the environment map, rotated by the user.
fn env_canonical(d: vec3<f32>) -> vec3<f32> {
    var c = d;
    if frame.up.z > 0.5 {
        c = vec3<f32>(d.x, d.z, -d.y);
    }
    let s = sin(frame.env.y);
    let co = cos(frame.env.y);
    return vec3<f32>(co * c.x + s * c.z, c.y, -s * c.x + co * c.z);
}

fn env_sample(d: vec3<f32>, lod: f32) -> vec3<f32> {
    let c = normalize(env_canonical(d));
    let u = atan2(c.x, -c.z) / (2.0 * PI) + 0.5;
    let v = acos(clamp(c.y, -1.0, 1.0)) / PI;
    return textureSampleLevel(env_tex, env_samp, vec2<f32>(u, v), lod).rgb;
}

fn env_irradiance(n: vec3<f32>) -> vec3<f32> {
    let c = normalize(env_canonical(n));
    let x = c.x;
    let y = c.y;
    let z = c.z;
    let e = frame.sh[0].xyz * 0.282095
        + frame.sh[1].xyz * (0.488603 * y)
        + frame.sh[2].xyz * (0.488603 * z)
        + frame.sh[3].xyz * (0.488603 * x)
        + frame.sh[4].xyz * (1.092548 * x * y)
        + frame.sh[5].xyz * (1.092548 * y * z)
        + frame.sh[6].xyz * (0.315392 * (3.0 * z * z - 1.0))
        + frame.sh[7].xyz * (1.092548 * x * z)
        + frame.sh[8].xyz * (0.546274 * (x * x - y * y));
    return max(e, vec3<f32>(0.0));
}

fn view_dir(world: vec3<f32>) -> vec3<f32> {
    // Orthographic: cam_pos.w == 1 and cam_pos.xyz holds the view direction towards the eye.
    if frame.cam_pos.w > 0.5 {
        return normalize(frame.cam_pos.xyz);
    }
    return normalize(frame.cam_pos.xyz - world);
}

fn geometric_normal(p_dx: vec3<f32>, p_dy: vec3<f32>, v: vec3<f32>) -> vec3<f32> {
    var n = normalize(cross(p_dx, p_dy));
    if dot(n, v) < 0.0 {
        n = -n;
    }
    return n;
}

struct Surface {
    base: vec4<f32>,
    n: vec3<f32>,
    metallic: f32,
    roughness: f32,
    ao: f32,
    emissive: vec3<f32>,
};

fn pbr(s: Surface, v: vec3<f32>) -> vec3<f32> {
    let l = normalize(frame.light_dir.xyz);
    let n = s.n;
    let h = normalize(l + v);
    let ndl = max(dot(n, l), 0.0);
    let ndv = max(dot(n, v), 1e-4);
    let ndh = max(dot(n, h), 0.0);
    let vdh = max(dot(v, h), 0.0);
    let rough = clamp(s.roughness, 0.04, 1.0);
    let a = rough * rough;
    let a2 = a * a;
    let dd = ndh * ndh * (a2 - 1.0) + 1.0;
    let d = a2 / (PI * dd * dd);
    let k = (rough + 1.0) * (rough + 1.0) / 8.0;
    let g = (ndv / (ndv * (1.0 - k) + k)) * (ndl / (ndl * (1.0 - k) + k));
    let base = s.base.rgb;
    let f0 = mix(vec3<f32>(0.04), base, s.metallic);
    let f = f0 + (vec3<f32>(1.0) - f0) * pow(1.0 - vdh, 5.0);
    let spec = d * g * f / (4.0 * ndv * max(ndl, 1e-4) + 1e-4);
    let kd = (vec3<f32>(1.0) - f) * (1.0 - s.metallic);
    let intensity = frame.light_dir.w;
    let direct = (kd * base / PI + spec) * ndl * intensity * PI * 0.85;
    if env_on() {
        // Image-based lighting; the key light stays as a softer accent.
        let ei = frame.env.x;
        let amb_d = env_irradiance(n) / PI * base * (1.0 - s.metallic);
        let r = reflect(-v, n);
        let fr = f0 + (max(vec3<f32>(1.0 - rough), f0) - f0) * pow(1.0 - ndv, 5.0);
        let amb_s = env_sample(r, rough * max(frame.env.z - 1.0, 0.0)) * fr;
        return direct * 0.5 + (amb_d + amb_s) * ei * s.ao + s.emissive;
    }

    let up = frame.up.xyz;
    let sky = vec3<f32>(0.50, 0.53, 0.58);
    let ground = vec3<f32>(0.22, 0.21, 0.20);
    let hemi = dot(n, up) * 0.5 + 0.5;
    let amb_d = mix(ground, sky, hemi) * base * (1.0 - s.metallic);
    let r = reflect(-v, n);
    let env = mix(ground, sky * 1.4, dot(r, up) * 0.5 + 0.5);
    let fr = f0 + (max(vec3<f32>(1.0 - rough), f0) - f0) * pow(1.0 - ndv, 5.0);
    let amb_s = env * fr * (1.0 - 0.6 * rough);
    return direct + (amb_d + amb_s) * s.ao * (0.55 + 0.45 * intensity) + s.emissive;
}

// Khronos PBR Neutral tone mapping: keeps base colours faithful, compresses highlights.
fn pbr_neutral(color_in: vec3<f32>) -> vec3<f32> {
    let start = 0.8 - 0.04;
    let desat = 0.15;
    let x = min(color_in.r, min(color_in.g, color_in.b));
    let offset = select(0.04, x - 6.25 * x * x, x < 0.08);
    var c = color_in - offset;
    let peak = max(c.r, max(c.g, c.b));
    if peak < start {
        return c;
    }
    let d = 1.0 - start;
    let new_peak = 1.0 - d * d / (peak + d - start);
    c = c * (new_peak / peak);
    let g = 1.0 - 1.0 / (desat * (peak - new_peak) + 1.0);
    return mix(c, vec3<f32>(new_peak), g);
}

fn lambert(base: vec3<f32>, n: vec3<f32>) -> vec3<f32> {
    let l = normalize(frame.light_dir.xyz);
    let ndl = max(dot(n, l), 0.0);
    let hemi = dot(n, frame.up.xyz) * 0.5 + 0.5;
    let amb = mix(vec3<f32>(0.25), vec3<f32>(0.5), hemi);
    return base * (amb + ndl * 0.7 * frame.light_dir.w);
}

// Derivatives are passed in so they are always computed in uniform control flow.
fn perturb_normal(n: vec3<f32>, p_dx: vec3<f32>, p_dy: vec3<f32>, uv_dx: vec2<f32>, uv_dy: vec2<f32>, tn: vec3<f32>, front: bool) -> vec3<f32> {
    let det = uv_dx.x * uv_dy.y - uv_dy.x * uv_dx.y;
    if abs(det) < 1e-12 {
        return n;
    }
    let t_ = (uv_dy.y * p_dx - uv_dx.y * p_dy) / det;
    var t = t_ - n * dot(n, t_);
    if dot(t, t) < 1e-20 {
        return n;
    }
    t = normalize(t);
    var b = cross(n, t);
    if !front {
        t = -t;
        b = -b;
    }
    return normalize(mat3x3<f32>(t, b, n) * tn);
}

fn shade(in: VOut, front: bool) -> vec4<f32> {
    let v = view_dir(in.world);
    let p_dx = dpdx(in.world);
    let p_dy = dpdy(in.world);
    let uv_dx = dpdx(in.uv);
    let uv_dy = dpdy(in.uv);
    let gn = geometric_normal(p_dx, p_dy, v);
    let has_n = meshu.flags.w != 0u || length(in.normal) > 0.5;
    var n: vec3<f32>;
    if (frame.flags & FLAG_FLAT) != 0u || !has_n {
        n = gn;
    } else {
        n = normalize(in.normal);
        if !front {
            n = -n;
        }
    }

    // Always sample (uniform control flow); select by flags.
    let tb = textureSample(t_base, samp, in.uv);
    let tnrm = textureSample(t_normal, samp, in.uv).xyz * 2.0 - 1.0;
    let tmr = textureSample(t_mr, samp, in.uv);
    let tocc = textureSample(t_occ, samp, in.uv).r;
    let tem = textureSample(t_emis, samp, in.uv).rgb;
    let pn = perturb_normal(n, p_dx, p_dy, uv_dx, uv_dy, tnrm, front);

    var base = mat.base_color;
    if use_tex(TEX_BASE) {
        base = base * tb;
    }
    if use_vcolor() {
        base = base * in.color;
    }
    if mat.params.w == 1.0 && base.a < mat.params.z {
        discard;
    }
    if clipped(in.world) {
        discard;
    }

    var color: vec3<f32>;
    var alpha = select(1.0, base.a, mat.params.w == 2.0);
    let mode = frame.mode;
    if mode == MODE_SHADED {
        var s: Surface;
        s.base = base;
        s.n = select(n, pn, use_tex(TEX_NORMAL) && (frame.flags & FLAG_FLAT) == 0u);
        s.metallic = mat.params.x * select(1.0, tmr.b, use_tex(TEX_MR));
        s.roughness = mat.params.y * select(1.0, tmr.g, use_tex(TEX_MR));
        s.ao = select(1.0, tocc, use_tex(TEX_OCC));
        s.emissive = mat.emissive.rgb * select(vec3<f32>(1.0), tem, use_tex(TEX_EMIS));
        color = pbr_neutral(pbr(s, v));
    } else if mode == MODE_CLAY {
        var s: Surface;
        s.base = vec4<f32>(0.72, 0.71, 0.69, 1.0);
        s.n = n;
        s.metallic = 0.0;
        s.roughness = 0.55;
        s.ao = 1.0;
        s.emissive = vec3<f32>(0.0);
        color = pbr_neutral(pbr(s, v));
        alpha = 1.0;
    } else if mode == MODE_UNLIT {
        color = base.rgb + mat.emissive.rgb * select(vec3<f32>(1.0), tem, use_tex(TEX_EMIS));
    } else if mode == MODE_NORMALS {
        color = n * 0.5 + 0.5;
        // Output is sRGB-encoded by the target; pre-linearize so colours read as raw normals.
        color = pow(color, vec3<f32>(2.2));
        alpha = 1.0;
    } else if mode == MODE_UV {
        if meshu.flags.x != 0u {
            let c = floor(in.uv * 8.0);
            let checker = abs(c.x + c.y) % 2.0;
            let f = fract(in.uv);
            let tint = vec3<f32>(f.x, f.y, 1.0 - f.x * 0.5) * 0.6 + 0.4;
            color = lambert(mix(vec3<f32>(0.08), vec3<f32>(0.9), checker) * tint, n);
        } else {
            color = lambert(vec3<f32>(0.85, 0.15, 0.55), n);
        }
        alpha = 1.0;
    } else {
        let c = select(vec3<f32>(0.8), in.color.rgb, meshu.flags.y != 0u);
        color = lambert(c, n);
        alpha = 1.0;
    }

    // Section view: faces seen from behind are the inside of the cut.
    if clipping() && !front {
        color = vec3<f32>(0.55, 0.07, 0.05) * (0.55 + 0.45 * abs(dot(gn, v)));
        alpha = 1.0;
    }
    if meshu.flags.z != 0u {
        color = mix(color, vec3<f32>(1.0, 0.55, 0.1), 0.45);
    }
    return vec4<f32>(color, alpha);
}

@fragment
fn fs_main(in: VOut, @builtin(front_facing) front: bool) -> @location(0) vec4<f32> {
    return shade(in, front);
}

@fragment
fn fs_wire(in: VOut) -> @location(0) vec4<f32> {
    if clipped(in.world) {
        discard;
    }
    if meshu.flags.z != 0u {
        return vec4<f32>(1.0, 0.6, 0.15, 1.0);
    }
    return frame.wire_color;
}

// Lines topology meshes: unlit base colour.
@fragment
fn fs_line_mesh(in: VOut) -> @location(0) vec4<f32> {
    if clipped(in.world) {
        discard;
    }
    var c = mat.base_color.rgb;
    if use_vcolor() {
        c = c * in.color.rgb;
    }
    return vec4<f32>(c, 1.0);
}

// ---- Points: one screen-aligned quad per vertex (instance) ----

@vertex
fn vs_points(@builtin(vertex_index) vi: u32, v: VIn) -> VOut {
    var corners = array<vec2<f32>, 6>(
        vec2<f32>(-1.0, -1.0), vec2<f32>(1.0, -1.0), vec2<f32>(1.0, 1.0),
        vec2<f32>(-1.0, -1.0), vec2<f32>(1.0, 1.0), vec2<f32>(-1.0, 1.0),
    );
    let c = corners[vi];
    var o: VOut;
    let wp = meshu.model * vec4<f32>(v.pos, 1.0);
    var clip = frame.view_proj * wp;
    clip = vec4<f32>(clip.xy + c * frame.viewport.z / frame.viewport.xy * clip.w, clip.zw);
    o.clip = clip;
    o.world = wp.xyz;
    o.normal = (meshu.normal_mat * vec4<f32>(v.normal, 0.0)).xyz;
    o.uv = v.uv;
    o.color = v.color;
    o.corner = c;
    return o;
}

@fragment
fn fs_points(in: VOut) -> @location(0) vec4<f32> {
    if dot(in.corner, in.corner) > 1.0 || clipped(in.world) {
        discard;
    }
    var c = mat.base_color.rgb;
    if use_vcolor() {
        c = c * in.color.rgb;
    }
    if frame.mode == MODE_NORMALS && length(in.normal) > 0.5 {
        c = pow(normalize(in.normal) * 0.5 + 0.5, vec3<f32>(2.2));
    } else if length(in.normal) > 0.5 && frame.mode != MODE_UNLIT && meshu.flags.y == 0u {
        var n = normalize(in.normal);
        if dot(n, view_dir(in.world)) < 0.0 {
            n = -n;
        }
        c = lambert(c, n);
    }
    if meshu.flags.z != 0u {
        c = mix(c, vec3<f32>(1.0, 0.55, 0.1), 0.45);
    }
    return vec4<f32>(c, 1.0);
}

// ---- Picking: world position of the surface under the cursor (1x1 target) ----

@fragment
fn fs_pick(in: VOut) -> @location(0) vec4<f32> {
    if clipped(in.world) {
        discard;
    }
    return vec4<f32>(in.world, 1.0);
}

@fragment
fn fs_pick_points(in: VOut) -> @location(0) vec4<f32> {
    if dot(in.corner, in.corner) > 1.0 || clipped(in.world) {
        discard;
    }
    return vec4<f32>(in.world, 1.0);
}

// ---- Overlay lines (grid, axes, bounding box) ----

struct LIn {
    @location(0) pos: vec3<f32>,
    @location(1) color: vec4<f32>,
};

struct LOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) color: vec4<f32>,
};

@vertex
fn vs_overlay(v: LIn) -> LOut {
    var o: LOut;
    o.clip = frame.view_proj * vec4<f32>(v.pos, 1.0);
    o.color = v.color;
    return o;
}

@fragment
fn fs_overlay(in: LOut) -> @location(0) vec4<f32> {
    return in.color;
}

// ---- Background gradient ----

struct BgOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) t: f32,
    @location(1) ndc: vec2<f32>,
};

@vertex
fn vs_bg(@builtin(vertex_index) vi: u32) -> BgOut {
    let x = f32((vi << 1u) & 2u);
    let y = f32(vi & 2u);
    var o: BgOut;
    o.clip = vec4<f32>(x * 2.0 - 1.0, y * 2.0 - 1.0, 1.0, 1.0);
    o.t = y;
    o.ndc = vec2<f32>(x * 2.0 - 1.0, y * 2.0 - 1.0);
    return o;
}

@fragment
fn fs_bg(in: BgOut) -> @location(0) vec4<f32> {
    if frame.env_bg.x > 0.5 {
        let p0 = frame.inv_view_proj * vec4<f32>(in.ndc, 0.0, 1.0);
        let p1 = frame.inv_view_proj * vec4<f32>(in.ndc, 1.0, 1.0);
        let d = normalize(p1.xyz / p1.w - p0.xyz / p0.w);
        return vec4<f32>(pbr_neutral(env_sample(d, frame.env_bg.y) * frame.env.x), 1.0);
    }
    return vec4<f32>(mix(frame.bg_bottom.rgb, frame.bg_top.rgb, clamp(in.t, 0.0, 1.0)), 1.0);
}
