// The film path tracer on the GPU: the same light paths as `radiance` in
// `film/mod.rs` (keep the two in step), one pixel per invocation. Each
// dispatch adds `size.w` samples to every pixel's running sums.

const PI: f32 = 3.14159265;
const TAU: f32 = 6.28318531;
const FRAC_1_PI: f32 = 0.31830989;
const MAX_GLASS_HOPS: u32 = 12u;
const HAIR_BIT: u32 = 0x80000000u;
const NONE: u32 = 0xffffffffu;
const COAT_ROUGH: f32 = 0.06;
const SCALP_CELL: f32 = 0.009;
const MAX_HAIR_HOPS: u32 = 8u;
// shade.rs hair constants; floats[0..=WHITE_STEPS] is white_fiber's table.
const HAIR_LOBES: f32 = 0.6;
const HAIR_PIGMENT: f32 = 1.0;
const WHITE_STEPS: u32 = 64u;

struct Node { lo: vec3f, a: u32, hi: vec3f, count: u32 }
struct GTri { a: vec4f, e1: vec4f, e2: vec4f }
struct Vtx { p0: vec4f, p1: vec4f, p2: vec4f, tangent: vec4f, surface: vec4f }
struct Item {
    m0: vec4f, m1: vec4f, m2: vec4f,
    n0: vec4f, n1: vec4f, n2: vec4f,
    tint: vec4f,
    surface: vec4f,
    // object material, mesh materials base, mesh materials count, flags
    info: vec4u,
    // texture size, color, normal, orm offsets (u32 words)
    tex: vec4u,
    // emissive texture offset, imported size, vertex alpha offset, packed wraps
    imp: vec4u,
    // emissive, base alpha
    imp_emissive: vec4f,
    // alpha cutoff (< 0: none)
    imp_cut: vec4f,
    // smart per-vertex data offset in floats (relative, 8 per vertex; NONE:
    // none), first decal, decal count
    smart: vec4u,
    // face (face.rs): first float or NONE, line count, mole count
    face: vec4u,
}
// (emission, transmission, ior, subsurface), (scatter, skin specular),
// (pores, mottling, hair, flame), (detail slot or -1, tiles per unit,
// strength, color), (rotation cos, sin, first smart layer in floats, layer
// count), (metallic, clearcoat, sheen, roughness).
struct Mat { a: vec4f, b: vec4f, c: vec4f, d: vec4f, e: vec4f, f: vec4f }
// A library detail map: (first word, size, levels, 0), (mean sRGB color,
// mean roughness), (mean occlusion, slope gain, height mean, height spread).
struct DMap { info: vec4u, mean: vec4f, extra: vec4f }
// kind 0 sun (a = direction, cone angle), 1 point (a = position), 2 area
// (a = position, radius; b = normal), 3 sphere (a = position, radius);
// power.w = how bright it is for picking (luminance of its intensity).
struct Light { kind: vec4u, a: vec4f, b: vec4f, power: vec4f }
struct Scalp { key: vec4i, root: vec4f, color: vec4f }
struct Params {
    eye: vec4f,
    forward: vec4f,
    right: vec4f,
    up: vec4f,
    // width, height, first sample, samples this pass
    size: vec4u,
    // bounces, floor, background, light count
    opts: vec4u,
    // floor y, env rotate, env total (> 0 when the map has light), sun chance
    env: vec4f,
    // env width, env height, sun present, scalp mask (0: none)
    env_size: vec4u,
    sun_dir: vec4f,
    sun_power: vec4f,
    scalp_lo: vec4f,
    scalp_hi: vec4f,
    // mesh node count, hair node count, tile x and y (pixels)
    counts: vec4u,
    // fog density (0: none), height, falloff, anisotropy
    fog: vec4f,
    // fog scattering albedo, fog distance
    fog_color: vec4f,
    // environment irradiance (fog light choice), local light count
    light_info: vec4f,
    lens_shape: vec4f,
    fog_noise: vec4f,
}

@group(0) @binding(0) var<uniform> P: Params;
@group(0) @binding(1) var<storage, read> mesh_nodes: array<Node>;
@group(0) @binding(2) var<storage, read> mesh_tris: array<GTri>;
@group(0) @binding(3) var<storage, read> tri_info: array<vec4u>;
@group(0) @binding(4) var<storage, read> verts: array<Vtx>;
@group(0) @binding(5) var<storage, read> items: array<Item>;
@group(0) @binding(6) var<storage, read> mats: array<Mat>;
@group(0) @binding(7) var<storage, read> texels: array<u32>;
@group(0) @binding(8) var<storage, read> floats: array<f32>;
@group(0) @binding(9) var<storage, read> hair_nodes: array<Node>;
@group(0) @binding(10) var<storage, read> hair_tris: array<GTri>;
@group(0) @binding(11) var<storage, read> hair_segs: array<vec4f>;
@group(0) @binding(12) var<storage, read> env_pixels: array<vec4f>;
@group(0) @binding(13) var<storage, read> env_cdf: array<f32>;
@group(0) @binding(14) var<storage, read> lights: array<Light>;
@group(0) @binding(15) var<storage, read> scalp: array<Scalp>;
@group(0) @binding(16) var<storage, read_write> accum: array<vec4f>;
@group(0) @binding(17) var<storage, read> dmaps: array<DMap>;
@group(0) @binding(18) var<storage, read> fire_data: array<vec4f>;

// ---------------------------------------------------------------- random

var<private> rng_state: u32;

fn rng_seed(a: u32, b: u32) {
    var h = a * 0x9E3779B9u ^ (b + 0x7F4A7C15u) * 0x85EBCA6Bu;
    h ^= h >> 16u; h *= 0x7feb352du; h ^= h >> 15u; h *= 0x846ca68bu; h ^= h >> 16u;
    rng_state = h;
}

fn rnd() -> f32 {
    rng_state = rng_state * 747796405u + 2891336453u;
    var w = ((rng_state >> ((rng_state >> 28u) + 4u)) ^ rng_state) * 277803737u;
    w = (w >> 22u) ^ w;
    return f32(w >> 8u) / 16777216.0;
}

fn rnd2() -> vec2f { let a = rnd(); return vec2f(a, rnd()); }

// ---------------------------------------------------------------- small math

fn smoothstep1(a: f32, b: f32, x: f32) -> f32 {
    let t = clamp((x - a) / (b - a), 0.0, 1.0);
    return t * t * (3.0 - 2.0 * t);
}

fn srgb(c: vec3f) -> vec3f { return pow(clamp(c, vec3f(0.0), vec3f(1.0)), vec3f(2.2)); }

fn max3(v: vec3f) -> f32 { return max(v.x, max(v.y, v.z)); }

fn normalize_or(v: vec3f, f: vec3f) -> vec3f {
    let l = length(v);
    if (l > 1e-20 && l == l) { return v / l; }
    return f;
}

// glam's any_orthonormal_vector.
fn ortho(n: vec3f) -> vec3f {
    let s = select(-1.0, 1.0, n.z >= 0.0);
    let a = -1.0 / (s + n.z);
    let b = n.x * n.y * a;
    return vec3f(1.0 + s * n.x * n.x * a, s * b, -s * n.x);
}

struct Frame { t: vec3f, b: vec3f, n: vec3f }
fn frame(n: vec3f) -> Frame { let t = ortho(n); return Frame(t, cross(n, t), n); }
fn to_local(f: Frame, v: vec3f) -> vec3f { return vec3f(dot(v, f.t), dot(v, f.b), dot(v, f.n)); }
fn to_world(f: Frame, v: vec3f) -> vec3f { return f.t * v.x + f.b * v.y + f.n * v.z; }

fn sphere_dir(u: vec2f) -> vec3f {
    let z = 1.0 - 2.0 * u.x;
    let r = sqrt(max(1.0 - z * z, 0.0));
    let phi = TAU * u.y;
    return vec3f(r * cos(phi), r * sin(phi), z);
}

fn cosine_dir(u: vec2f) -> vec3f {
    let r = sqrt(u.x);
    let phi = TAU * u.y;
    return vec3f(r * cos(phi), r * sin(phi), sqrt(max(1.0 - u.x, 0.0)));
}

fn cone_dir(axis: vec3f, angle: f32, u: vec2f) -> vec3f {
    let cos_max = cos(angle);
    let z = 1.0 - u.x * (1.0 - cos_max);
    let r = sqrt(max(1.0 - z * z, 0.0));
    let phi = TAU * u.y;
    return to_world(frame(axis), vec3f(r * cos(phi), r * sin(phi), z));
}

fn mis(a0: f32, b0: f32) -> f32 {
    let s = max(max(a0, b0), 1e-20);
    let a = a0 / s;
    let b = b0 / s;
    return a * a / max(a * a + b * b, 1e-20);
}

// ---------------------------------------------------------------- textures

fn texel(offset: u32, size: u32, x: u32, y: u32) -> vec4f {
    return unpack4x8unorm(texels[offset + y * size + x]);
}

fn sample_tex(offset: u32, size: u32, uv: vec2f) -> vec4f {
    let s = f32(size);
    let x = clamp(uv.x * s - 0.5, 0.0, s - 1.0);
    let y = clamp(uv.y * s - 0.5, 0.0, s - 1.0);
    let x0 = u32(floor(x));
    let y0 = u32(floor(y));
    let x1 = min(x0 + 1u, size - 1u);
    let y1 = min(y0 + 1u, size - 1u);
    let fx = x - f32(x0);
    let fy = y - f32(y0);
    let top = mix(texel(offset, size, x0, y0), texel(offset, size, x1, y0), fx);
    let bottom = mix(texel(offset, size, x0, y1), texel(offset, size, x1, y1), fx);
    return mix(top, bottom, fy);
}

fn wrap1(v: f32, mode: u32) -> f32 {
    if (mode == 1u) { return v - floor(v); }
    if (mode == 2u) { let m = v - 2.0 * floor(v * 0.5); return 1.0 - abs(m - 1.0); }
    return clamp(v, 0.0, 1.0);
}

fn imported_uv(it: Item, uv: vec2f, slot: u32) -> vec2f {
    if ((it.info.w & 4u) == 0u) { return uv; }
    let w = it.imp.w >> (slot * 4u);
    return vec2f(wrap1(uv.x, w & 3u), wrap1(uv.y, (w >> 2u) & 3u));
}

// ---------------------------------------------------------------- geometry

fn vpos(v: Vtx) -> vec3f { return v.p0.xyz; }
fn vnormal(v: Vtx) -> vec3f { return v.p1.xyz; }
fn vcolor(v: Vtx) -> vec3f { return vec3f(v.p0.w, v.p1.w, v.p2.z); }
fn vuv(v: Vtx) -> vec2f { return v.p2.xy; }
fn vmaterial(v: Vtx) -> u32 { return bitcast<u32>(v.p2.w); }

fn model(it: Item, v: vec3f) -> vec3f { return it.m0.xyz * v.x + it.m1.xyz * v.y + it.m2.xyz * v.z; }
fn normal_of(it: Item, v: vec3f) -> vec3f { return it.n0.xyz * v.x + it.n1.xyz * v.y + it.n2.xyz * v.z; }
// The inverse of the normal matrix is the model matrix transposed.
fn to_object_normal(it: Item, v: vec3f) -> vec3f { return vec3f(dot(it.m0.xyz, v), dot(it.m1.xyz, v), dot(it.m2.xyz, v)); }

fn material_index(it: Item, v0: Vtx) -> u32 {
    if (v0.surface.x < 0.0) { return it.info.x; }
    let m = vmaterial(v0);
    if (m < it.info.z) { return it.info.y + m; }
    return it.info.x;
}

fn lerp_uv(tri: vec4u, u: f32, v: f32) -> vec2f {
    return vuv(verts[tri.y]) * (1.0 - u - v) + vuv(verts[tri.z]) * u + vuv(verts[tri.w]) * v;
}

// Whether a mesh hit lands on a clear (cut-out) texel.
fn clear(tri_index: u32, u: f32, v: f32) -> bool {
    let tri = tri_info[tri_index];
    let it = items[tri.x];
    if ((it.info.w & 3u) != 3u) { return false; }
    let uv = lerp_uv(tri, u, v);
    let alpha = sample_tex(it.tex.y, it.tex.x, imported_uv(it, uv, 0u)).w;
    if ((it.info.w & 4u) != 0u) {
        var vertex = 1.0;
        if (it.imp.z != NONE) {
            vertex = floats[it.imp.z + tri.y] * (1.0 - u - v) + floats[it.imp.z + tri.z] * u + floats[it.imp.z + tri.w] * v;
        }
        let cut = select(0.5, it.imp_cut.x, it.imp_cut.x >= 0.0);
        return alpha * it.imp_emissive.w * vertex < cut;
    }
    return alpha < 0.5;
}

// Glass and flames let shadow rays through.
fn is_glass(tri_index: u32) -> bool {
    let tri = tri_info[tri_index];
    let it = items[tri.x];
    let m = mats[material_index(it, verts[tri.y])];
    return m.a.y >= 0.5 || m.c.w > 0.5;
}

fn is_flame(tri_index: u32) -> bool {
    let tri = tri_info[tri_index];
    let it = items[tri.x];
    return mats[material_index(it, verts[tri.y])].c.w > 0.5;
}

struct Hit { t: f32, tri: u32, u: f32, v: f32, found: bool }

// Skip modes for candidate hits.
const SKIP_NONE: u32 = 0u;
const SKIP_CLEAR: u32 = 1u;
const SKIP_CLEAR_GLASS: u32 = 2u;
const SKIP_OTHER_ITEMS: u32 = 3u;
// Clear texels and flames (rays that are not the camera's).
const SKIP_CLEAR_FLAME: u32 = 4u;

fn get_node(hair: bool, i: u32) -> Node {
    if (hair) { return hair_nodes[i]; }
    return mesh_nodes[i];
}

fn get_tri(hair: bool, i: u32) -> GTri {
    if (hair) { return hair_tris[i]; }
    return mesh_tris[i];
}

fn slab(o: vec3f, inv: vec3f, lo: vec3f, hi: vec3f, t_max: f32) -> f32 {
    let t0 = (lo - o) * inv;
    let t1 = (hi - o) * inv;
    let a = min(t0, t1);
    let b = max(t0, t1);
    let near = max(a.x, max(a.y, a.z));
    let far = min(b.x, min(b.y, b.z));
    if (near <= far && far >= 0.0 && near <= t_max) { return near; }
    return -1e30;
}

fn skip(mode: u32, tri: u32, u: f32, v: f32, item: u32) -> bool {
    if (mode == SKIP_NONE) { return false; }
    if (mode == SKIP_OTHER_ITEMS && tri_info[tri].x != item) { return true; }
    if (clear(tri, u, v)) { return true; }
    if (mode == SKIP_CLEAR_FLAME) { return is_flame(tri); }
    return mode == SKIP_CLEAR_GLASS && is_glass(tri);
}

// Nearest (or any) hit with t in (t_min, t_max) on the mesh or hair tree.
fn walk(hair: bool, o: vec3f, d: vec3f, t_min: f32, t_max0: f32, mode: u32, item: u32, any: bool) -> Hit {
    var best = Hit(0.0, 0u, 0.0, 0.0, false);
    let count = select(P.counts.x, P.counts.y, hair);
    if (count == 0u) { return best; }
    var t_max = t_max0;
    let inv = 1.0 / d;
    var stack: array<u32, 32>;
    var sp = 0u;
    var at = 0u;
    loop {
        let node = get_node(hair, at);
        var go = false;
        if (node.count > 0u) {
            for (var k = 0u; k < node.count; k++) {
                let i = node.a + k;
                let g = get_tri(hair, i);
                let e1 = g.e1.xyz;
                let e2 = g.e2.xyz;
                let p = cross(d, e2);
                let det = dot(e1, p);
                if (abs(det) < 1e-12) { continue; }
                let id = 1.0 / det;
                let s = o - g.a.xyz;
                let u = dot(s, p) * id;
                if (u < 0.0 || u > 1.0) { continue; }
                let q = cross(s, e1);
                let v = dot(d, q) * id;
                if (v < 0.0 || u + v > 1.0) { continue; }
                let t = dot(e2, q) * id;
                if (t <= t_min || t >= t_max) { continue; }
                let tri = bitcast<u32>(g.a.w);
                if (!hair && skip(mode, tri, u, v, item)) { continue; }
                best = Hit(t, tri, u, v, true);
                if (any) { return best; }
                t_max = t;
            }
        } else {
            let l = at + 1u;
            let r = node.a;
            let nl = get_node(hair, l);
            let nr = get_node(hair, r);
            let tl = slab(o, inv, nl.lo, nl.hi, t_max);
            let tr = slab(o, inv, nr.lo, nr.hi, t_max);
            let hl = tl > -1e29;
            let hr = tr > -1e29;
            if (hl && hr) {
                var near = l;
                var far = r;
                if (tr < tl) { near = r; far = l; }
                if (sp < 32u) { stack[sp] = far; sp++; }
                at = near;
                go = true;
            } else if (hl) {
                at = l; go = true;
            } else if (hr) {
                at = r; go = true;
            }
        }
        if (go) { continue; }
        var found = false;
        loop {
            if (sp == 0u) { break; }
            sp--;
            let n = get_node(hair, stack[sp]);
            if (slab(o, inv, n.lo, n.hi, t_max) > -1e29) {
                at = stack[sp];
                found = true;
                break;
            }
        }
        if (!found) { return best; }
    }
    return best;
}

// Nearest hit (mesh, hair or floor). found = false: nothing; tri = NONE: floor.
// Only camera rays (and their glass hops) see flames.
fn trace(o: vec3f, d: vec3f, camera: bool) -> Hit {
    var h = walk(false, o, d, 1e-4, 3.4e38, select(SKIP_CLEAR_FLAME, SKIP_CLEAR, camera), 0u, false);
    let far = select(3.4e38, h.t, h.found);
    let hh = walk(true, o, d, 1e-4, far, SKIP_NONE, 0u, false);
    if (hh.found) { h = hh; h.tri = h.tri | HAIR_BIT; }
    if (P.opts.y != 0u && d.y < 0.0) {
        let t = (P.env.x - o.y) / d.y;
        if (t > 1e-4 && (!h.found || t < h.t)) { return Hit(t, NONE, 0.0, 0.0, true); }
    }
    return h;
}

fn blocked(o: vec3f, d: vec3f, far: f32) -> bool {
    if (P.opts.y != 0u) {
        let t = (P.env.x - o.y) / d.y;
        if (t > 1e-4 && t < far) { return true; }
    }
    if (walk(true, o, d, 1e-4, far, SKIP_NONE, 0u, true).found) { return true; }
    return walk(false, o, d, 1e-4, far, SKIP_CLEAR_GLASS, 0u, true).found;
}

// ---------------------------------------------------------------- skin

fn mat_noise(p: vec3f) -> f32 {
    let cell = vec3i(floor(p));
    var f = p - floor(p);
    f = f * f * (vec3f(3.0) - 2.0 * f);
    var value = 0.0;
    for (var z = 0; z <= 1; z++) {
        for (var y = 0; y <= 1; y++) {
            for (var x = 0; x <= 1; x++) {
                let c = cell + vec3i(x, y, z);
                var h = (bitcast<u32>(c.x) * 73856093u) ^ (bitcast<u32>(c.y) * 19349663u) ^ (bitcast<u32>(c.z) * 83492791u);
                h = (h ^ (h >> 16u)) * 0x7feb352du;
                h = (h ^ (h >> 15u)) * 0x846ca68bu;
                h ^= h >> 16u;
                let wx = select(f.x, 1.0 - f.x, x == 0);
                let wy = select(f.y, 1.0 - f.y, y == 0);
                let wz = select(f.z, 1.0 - f.z, z == 0);
                value += f32(h) / 4294967295.0 * wx * wy * wz;
            }
        }
    }
    return value;
}

fn pore_height(q: vec3f) -> f32 { let n = mat_noise(q); return -n * n * n; }

struct Skin { n: vec3f, color: vec3f, rough: f32 }

// Material::skin_detail.
fn skin_detail(m: Mat, p: vec3f, n: vec3f, color: vec3f, rough: f32) -> Skin {
    let pores = m.c.x;
    let mottling = m.c.y;
    if (m.a.w <= 0.0 || (pores == 0.0 && mottling == 0.0)) { return Skin(n, color, rough); }
    let q = p / 0.00025;
    let e = 0.08;
    let g = vec3f(
        pore_height(q + vec3f(e, 0.0, 0.0)) - pore_height(q - vec3f(e, 0.0, 0.0)),
        pore_height(q + vec3f(0.0, e, 0.0)) - pore_height(q - vec3f(0.0, e, 0.0)),
        pore_height(q + vec3f(0.0, 0.0, e)) - pore_height(q - vec3f(0.0, 0.0, e))) * (0.14 * pores / (2.0 * e));
    let bumped = normalize_or(n - (g - n * dot(g, n)), n);
    let pigment = mat_noise(p / 0.003) * 0.6 + mat_noise(p / 0.011) * 0.4 - 0.5;
    let tint = vec3f(1.0) + vec3f(0.08, -0.16, -0.20) * pigment * mottling;
    let r = clamp(rough + (mat_noise(p / 0.0008) - 0.5) * 0.18 * pores, 0.03, 1.0);
    return Skin(bumped, color * tint, r);
}

// ---------------------------------------------------------------- face

// face.rs: a human face's regional skin. The face's floats start at `o`
// (Face::pack): a header of 13 vec4, then the moles, then the lines.
struct FaceH {
    origin: vec3f, right: vec3f, up: vec3f, fwd: vec3f,
    eye: vec2f, eye_r: f32, seed: u32,
    tip: vec2f, nasion_y: f32, sub_y: f32,
    ala: vec2f, mouth: vec2f,
    stomion_y: f32, lip_top_y: f32, lip_bottom_y: f32, chin_y: f32,
    brow_y: f32, lip_z: f32, age_spots: f32, micro: f32,
    ear: vec3f, lips: f32,
    pores: f32, freckles: f32, redness: f32, under_eye: f32,
    oil: f32, stubble: f32, fuzz: f32,
    lip_color: vec3f, low_y: f32,
    sex: f32,
}

fn face_header(o: u32) -> FaceH {
    var f: FaceH;
    let a = fv4(o); let b = fv4(o + 4u); let c = fv4(o + 8u); let d = fv4(o + 12u);
    f.origin = a.xyz; f.right = b.xyz; f.up = c.xyz; f.fwd = d.xyz;
    f.eye = vec2f(a.w, b.w); f.eye_r = c.w; f.seed = bitcast<u32>(d.w);
    let e = fv4(o + 16u); f.tip = e.xy; f.nasion_y = e.z; f.sub_y = e.w;
    let g = fv4(o + 20u); f.ala = g.xy; f.mouth = g.zw;
    let h = fv4(o + 24u); f.stomion_y = h.x; f.lip_top_y = h.y; f.lip_bottom_y = h.z; f.chin_y = h.w;
    let i = fv4(o + 28u); f.brow_y = i.x; f.lip_z = i.y; f.age_spots = i.z; f.micro = i.w;
    let j = fv4(o + 32u); f.ear = j.xyz; f.lips = j.w;
    let k = fv4(o + 36u); f.pores = k.x; f.freckles = k.y; f.redness = k.z; f.under_eye = k.w;
    let l = fv4(o + 40u); f.oil = l.x; f.stubble = l.y; f.fuzz = l.z;
    let m = fv4(o + 44u); f.lip_color = m.xyz; f.low_y = m.w;
    f.sex = fv4(o + 48u).x;
    return f;
}

fn face_hash(c: vec3i, seed: u32) -> u32 {
    var h = (bitcast<u32>(c.x) * 73856093u) ^ (bitcast<u32>(c.y) * 19349663u) ^ (bitcast<u32>(c.z) * 83492791u) ^ (seed * 0x9e3779b9u);
    h = (h ^ (h >> 16u)) * 0x7feb352du;
    h = (h ^ (h >> 15u)) * 0x846ca68bu;
    return h ^ (h >> 16u);
}

fn face_unit(h: u32) -> f32 { return f32(h >> 8u) / 16777216.0; }

// (feature point, random of its cell)
fn face_worley(p: vec3f, cell: f32, seed: u32) -> vec4f {
    let q = p / cell;
    let base = vec3i(floor(q));
    var best = 3.4e38;
    var bf = vec3f(0.0);
    var br = 0.0;
    for (var z = -1; z <= 1; z++) {
        for (var y = -1; y <= 1; y++) {
            for (var x = -1; x <= 1; x++) {
                let c = base + vec3i(x, y, z);
                let j = vec3f(face_unit(face_hash(c, seed)), face_unit(face_hash(c, seed ^ 0x51edu)), face_unit(face_hash(c, seed ^ 0xa3c5u)));
                let f = vec3f(c) + vec3f(0.1) + j * 0.8;
                let dd = f - q;
                let d = dot(dd, dd);
                if (d < best) {
                    best = d;
                    bf = f;
                    br = face_unit(face_hash(c, seed ^ 0x7f4au));
                }
            }
        }
    }
    return vec4f(bf * cell, br);
}

// A pit of radius r: (gradient, height).
fn face_along(p: vec3f, f: vec3f, n: vec3f) -> vec3f { let d = p - f; return d - n * dot(d, n); }

fn face_pit(p: vec3f, f: vec3f, n: vec3f, r: f32) -> vec4f {
    let d = face_along(p, f, n);
    let t = dot(d, d) / (r * r);
    if (t >= 1.0) { return vec4f(0.0); }
    let s = 1.0 - t;
    return vec4f(d * (4.0 * s / (r * r)), -s * s);
}

fn face_blob(q: vec2f, c: vec2f, r: vec2f) -> f32 { let d = (q - c) / r; return exp(-dot(d, d)); }

// (mask, depth into the lip, position across it)
fn face_lip_mask(f: FaceH, q: vec2f) -> vec3f {
    let u = q.x / max(f.mouth.x, 1e-4);
    let slit = f.stomion_y + (f.mouth.y - f.stomion_y) * u * u;
    let v = q.y - slit;
    let w = max(1.0 - u * u, 0.0);
    let bu = (u - 0.3) / 0.16;
    let bc = u / 0.09;
    let bow = 1.0 + 0.12 * exp(-bu * bu) - 0.12 * exp(-bc * bc);
    let hu = max(f.lip_top_y - f.stomion_y, 1e-4) * 0.72 * pow(max(w, 1e-8), 0.45) * bow;
    let hl = max(f.stomion_y - f.lip_bottom_y, 1e-4) * pow(max(w, 1e-8), 0.6);
    var r: f32;
    if (v > 0.0) { r = v / max(hu, 1e-5); } else { r = -v / max(hl, 1e-5); }
    let mask = (1.0 - smoothstep1(0.9, 1.02, r)) * (1.0 - smoothstep1(0.94, 1.04, u));
    return vec3f(mask, 1.0 - abs(r * 2.0 - 1.0), r);
}

struct FaceRegions {
    forehead: f32, nose: f32, cheek: f32, under_eye: f32, chin: f32, ear: f32,
    lips: f32, lip_depth: f32, lip_r: f32, tzone: f32, beard: f32, sun: f32, face: f32,
}

fn face_regions(f: FaceH, q: vec2f, z: f32) -> FaceRegions {
    let er = f.eye_r;
    var g: FaceRegions;
    g.face = smoothstep1(f.ear.z + er * 0.8, f.ear.z + er * 3.0, z);
    let front = g.face;
    g.forehead = smoothstep1(f.brow_y, f.brow_y + er * 0.9, q.y) * (1.0 - smoothstep1(f.eye.x * 1.35, f.eye.x * 1.9, q.x)) * (1.0 - smoothstep1(f.brow_y + er * 5.5, f.brow_y + er * 7.5, q.y)) * front;
    let half = f.ala.x * (0.35 + 0.65 * smoothstep1(f.nasion_y, f.tip.y, q.y));
    g.nose = (1.0 - smoothstep1(half * 0.85, half * 1.2, q.x)) * smoothstep1(f.sub_y - er * 0.25, f.sub_y + er * 0.1, q.y) * (1.0 - smoothstep1(f.nasion_y, f.nasion_y + er * 0.6, q.y)) * smoothstep1(f.lip_z - er * 0.5, f.lip_z + er * 0.4, z);
    g.cheek = face_blob(q, vec2f(f.eye.x + er * 0.45, (f.tip.y + f.eye.y) * 0.5 - er * 0.4), vec2f(er * 1.7, er * 1.5)) * front * (1.0 - g.nose);
    g.under_eye = face_blob(q, vec2f(f.eye.x - er * 0.1, f.eye.y - er * 1.15), vec2f(er * 1.1, er * 0.5)) * front;
    g.chin = face_blob(q, vec2f(0.0, f.chin_y + er * 0.35), vec2f(er * 1.2, er * 0.9)) * front;
    let eq = vec3f(q.x, q.y, z) - f.ear;
    g.ear = exp(-dot(eq, eq) / (er * er * 3.6)) * smoothstep1(f.ear.x - er * 1.4, f.ear.x - er * 0.8, q.x);
    let lm = face_lip_mask(f, q);
    g.lips = lm.x * smoothstep1(f.lip_z - er * 0.8, f.lip_z - er * 0.35, z) * f.lips;
    g.lip_depth = lm.y;
    g.lip_r = lm.z;
    g.tzone = min(g.forehead * (1.0 - smoothstep1(f.eye.x * 0.5, f.eye.x * 1.2, q.x)) + g.nose + g.chin * 0.7, 1.0);
    let tb = min(q.x / max(f.mouth.x, 1e-4), 1.0);
    let line = f.sub_y - er * 0.1 + (f.mouth.y + er * 0.9 - f.sub_y + er * 0.1) * tb * tb + max(q.x - f.mouth.x, 0.0) * 0.75;
    g.beard = (1.0 - smoothstep1(line - er * 0.7, line + er * 0.3, q.y)) * smoothstep1(f.chin_y - er * 3.0, f.chin_y - er * 1.5, q.y) * (1.0 - smoothstep1(f.ear.x - er * 1.6, f.ear.x - er * 0.9, q.x)) * (1.0 - lm.x) * smoothstep1(f.ear.z + er * 0.2, f.ear.z + er * 1.5, z) * (1.0 - g.nose);
    g.sun = min((g.nose + g.cheek + g.forehead * 0.7) * front, 1.0);
    return g;
}

// Expression lines (Face::lines_height, shading grooves): (gradient, height).
fn face_lines(lines: u32, count: u32, q: vec2f, p: vec3f) -> vec3f {
    var h = 0.0;
    var grad = vec2f(0.0);
    for (var k = 0u; k < count; k++) {
        let o = lines + k * 24u;
        let head = fv4(o);
        let bounds = fv4(o + 4u);
        let w = head.y;
        let depth = head.z;
        let reach = w * 3.0;
        if (q.x < bounds.x - reach || q.y < bounds.y - reach || q.x > bounds.z + reach || q.y > bounds.w + reach) { continue; }
        let n = u32(head.x);
        var best_d2 = 3.4e38;
        var best_c = vec2f(0.0);
        var best_t = 0.0;
        var best_side = 1.0;
        let segments = f32(n - 1u);
        for (var i = 0u; i + 1u < n; i++) {
            let a = vec2f(floats[o + 8u + i * 2u], floats[o + 9u + i * 2u]);
            let b = vec2f(floats[o + 10u + i * 2u], floats[o + 11u + i * 2u]);
            let ab = b - a;
            let t = clamp(dot(q - a, ab) / max(dot(ab, ab), 1e-12), 0.0, 1.0);
            let c = a + ab * t;
            let dd = q - c;
            let d2 = dot(dd, dd);
            if (d2 < best_d2) {
                let aq = q - a;
                best_d2 = d2;
                best_c = c;
                best_t = (f32(i) + t) / segments;
                best_side = select(-1.0, 1.0, ab.x * aq.y - ab.y * aq.x < 0.0);
            }
        }
        let d = sqrt(best_d2);
        if (d > reach) { continue; }
        let taper = smoothstep1(0.0, 0.18, best_t) * (1.0 - smoothstep1(0.75, 1.0, best_t));
        let breaks = clamp(0.45 + 1.2 * (mat_noise(p / 0.006 + vec3f(f32(k) * 7.3)) - 0.5) + 0.4, 0.0, 1.0);
        let a = depth * taper * breaks;
        var dir = vec2f(0.0);
        if (d > 1e-9) { dir = (q - best_c) / d; }
        let x = d / w;
        let e = exp(-x * x);
        h -= a * e;
        grad += dir * (a * e * 2.0 * d / (w * w));
        if (head.w > 0.5) {
            let ds = d * best_side - w * 1.6;
            let wb = w * 1.4;
            let xb = ds / wb;
            let eb = exp(-xb * xb);
            h += 0.45 * a * eb;
            grad += dir * best_side * (0.45 * a * eb * -2.0 * ds / (wb * wb));
        }
    }
    return vec3f(grad, h);
}

struct FaceLook { n: vec3f, color: vec3f, rough: f32, coat: f32, sheen: f32 }

fn face_skin_cells(p: vec3f) -> f32 { return mat_noise(p / 0.00022) * 0.7 + mat_noise(p / 0.00009) * 0.3; }

// Face::shade: `info` = (first float, line count, mole count, -).
fn face_shade(info: vec4u, p: vec3f, n: vec3f, color: vec3f, rough: f32) -> FaceLook {
    let f = face_header(info.x);
    let d0 = p - f.origin;
    let xr = dot(d0, f.right);
    let q = vec2f(abs(xr), dot(d0, f.up));
    let sx = select(1.0, -1.0, xr < 0.0);
    let z = dot(d0, f.fwd);
    let g = face_regions(f, q, z);
    var hg = vec3f(0.0);
    var cavity = 0.0;
    let bare = (1.0 - g.lips) * (1.0 - 0.75 * g.under_eye);
    for (var k = 0u; k < 3u; k++) {
        var cell = 0.0007; var depth = 0.000035; var w = 0.5 + 0.6 * g.forehead + 0.3 * g.chin;
        if (k == 1u) { cell = 0.00095; depth = 0.00006; w = 0.35 + 0.8 * g.cheek + 0.3 * g.chin; }
        if (k == 2u) { cell = 0.0012; depth = 0.00008; w = 1.2 * g.nose + 0.4 * g.cheek; }
        let amount = f.pores * w * bare;
        if (amount <= 1e-3) { continue; }
        let wf = face_worley(p, cell, f.seed ^ (0x1000u + k));
        if (wf.w > 0.85) { continue; }
        let pit = face_pit(p, wf.xyz, n, cell * (0.17 + 0.16 * wf.w));
        hg += pit.xyz * (depth * amount);
        cavity += -pit.w * min(amount, 1.0) * (0.06 + 0.08 * f32(k) / 2.0);
    }
    let lines_at = info.x + 52u + info.z * 4u;
    let lh = face_lines(lines_at, info.y, q, p);
    hg += f.right * (lh.x * sx) + f.up * lh.y;
    cavity += clamp(-lh.z / 0.0002, 0.0, 1.0) * 0.6;
    for (var k = 0u; k < 3u; k++) {
        var dir = normalize(f.right + f.up);
        var spacing = 0.00045;
        if (k == 1u) { dir = normalize(f.right - f.up * 0.8); spacing = 0.0006; }
        if (k == 2u) { dir = normalize(f.up + f.fwd * 0.6); spacing = 0.0005; }
        let phase = dot(p, dir) / spacing + 2.5 * mat_noise(p / (spacing * 3.0) + vec3f(f32(k) * 3.1));
        let s = sin(PI * phase);
        let fl = pow(max(1.0 - abs(s), 1e-6), 4.0);
        let breaks = smoothstep1(0.45, 0.75, mat_noise(p / (spacing * 2.0) + vec3f(11.0 + f32(k))));
        let a = 0.000014 * f.micro * breaks * (1.0 - g.lips);
        let df = 4.0 * pow(max(1.0 - abs(s), 1e-6), 3.0) * sign(s) * cos(PI * phase) * PI;
        hg += dir * (a * df / spacing);
        cavity += fl * breaks * 0.08 * f.micro;
    }
    {
        let e = 0.00004;
        let grad = vec3f(
            face_skin_cells(p + vec3f(e, 0.0, 0.0)) - face_skin_cells(p - vec3f(e, 0.0, 0.0)),
            face_skin_cells(p + vec3f(0.0, e, 0.0)) - face_skin_cells(p - vec3f(0.0, e, 0.0)),
            face_skin_cells(p + vec3f(0.0, 0.0, e)) - face_skin_cells(p - vec3f(0.0, 0.0, e))) / (2.0 * e);
        hg += grad * (0.000012 * (1.0 - 0.6 * g.lips) * (0.6 + 0.4 * f.pores));
    }
    var lip_gloss = 0.0;
    if (g.lips > 0.01) {
        let spacing = 0.0009 * (0.8 + 0.4 * mat_noise(p / 0.004 + vec3f(5.0)));
        let phase = q.x / spacing + 0.8 * mat_noise(p / 0.0025) + (q.y - f.stomion_y) * q.x / (spacing * 0.04);
        let s = sin(PI * phase);
        let fl = pow(max(1.0 - abs(s), 1e-6), 10.0);
        let mid = smoothstep1(0.1, 0.35, g.lip_r) * (1.0 - smoothstep1(0.6, 0.95, g.lip_r));
        let broken = smoothstep1(0.15, 0.45, mat_noise(p / 0.0013 + vec3f(17.0)));
        let a = 0.00008 * g.lips * mid * broken;
        let df = 10.0 * pow(max(1.0 - abs(s), 1e-6), 9.0) * sign(s) * cos(PI * phase) * PI;
        hg += f.right * (sx * a * df / spacing);
        cavity += fl * mid * broken * g.lips * 0.6;
        lip_gloss = g.lips * (0.45 + 0.55 * smoothstep1(0.35, 0.75, mat_noise(p / 0.0012)));
    }
    var mole = 0.0;
    for (var k = 0u; k < info.z; k++) {
        let m = fv4(info.x + 52u + k * 4u);
        let d = distance(vec2f(q.x * sx, q.y), m.xy);
        let t = 1.0 - smoothstep1(m.z * 0.7, m.z, d);
        mole = max(mole, t * max(g.face, 0.3));
    }
    let bumped = normalize_or(n - (hg - n * dot(hg, n)), n);

    var c = color;
    let blotch = 0.7 + 0.6 * mat_noise(p / 0.006 + vec3f((f32(f.seed) * 0.37) % 100.0));
    let red = min(f.redness * (0.9 * g.cheek + 1.0 * g.nose + 0.5 * g.chin + 0.9 * g.ear) * blotch, 1.0);
    c *= mix(vec3f(1.0), vec3f(1.03, 0.82, 0.83), red);
    c *= mix(vec3f(1.0), vec3f(0.86, 0.80, 0.90), f.under_eye * g.under_eye);
    let mottle = mat_noise(p / 0.003) * 0.6 + mat_noise(p / 0.011) * 0.4 - 0.5;
    c *= vec3f(1.0) + vec3f(0.06, -0.10, -0.12) * mottle * 0.6;
    let spots = f.freckles + f.age_spots * 0.5;
    if (spots > 0.0 && g.sun > 0.01) {
        for (var k = 0u; k < 2u; k++) {
            var cell = 0.0028; var size = 0.3;
            var chance = g.sun * f.freckles * 0.8;
            if (k == 1u) { cell = 0.0045; size = 0.38; chance = g.sun * (f.age_spots * 0.12 + f.freckles * 0.2); }
            let wf = face_worley(p, cell, f.seed ^ (0x2000u + k));
            if (wf.w < chance) {
                let rr = cell * size * (0.35 + 0.4 * fract(wf.w * 7.31));
                let edge = rr * (0.75 + 0.5 * mat_noise(p / (rr * 0.8)));
                let t = 1.0 - smoothstep1(edge * 0.6, edge, length(face_along(p, wf.xyz, n)));
                let strength = 0.2 + 0.35 * (wf.w / max(chance, 1e-3));
                c *= mix(vec3f(1.0), vec3f(0.82, 0.70, 0.62), t * strength);
            }
        }
    }
    c *= mix(vec3f(1.0), vec3f(0.45, 0.33, 0.28), mole * 0.9);
    if (f.stubble > 0.0 && g.beard > 0.01) {
        let wf = face_worley(p, 0.0009, f.seed ^ 0x3000u);
        let dt = 1.0 - smoothstep1(0.00006, 0.00012, length(face_along(p, wf.xyz, n)));
        let a = f.stubble * g.beard;
        c *= mix(vec3f(1.0), vec3f(0.76, 0.71, 0.76), a * 0.6);
        if (wf.w < 0.8) { c *= mix(vec3f(1.0), vec3f(0.18, 0.15, 0.14), dt * a); }
    }
    var lip_base = f.lip_color;
    if (all(f.lip_color == vec3f(0.0))) { lip_base = color * mix(vec3f(0.95, 0.74, 0.8), vec3f(0.93, 0.78, 0.8), f.sex); }
    let rim = (1.0 - smoothstep1(0.0, 0.15, abs(g.lip_r - 1.04))) * smoothstep1(0.2, 0.6, f.lips) * (1.0 - smoothstep1(0.9, 1.0, q.x / max(f.mouth.x, 1e-4)));
    c *= 1.0 + 0.05 * rim * g.face;
    let lip = lip_base * (0.95 + 0.06 * g.lip_depth) * (0.72 + 0.28 * smoothstep1(0.0, 0.3, g.lip_r)) * (1.0 - 0.06 * smoothstep1(0.7, 1.0, g.lip_r));
    let streak = 0.9 + 0.2 * mat_noise(vec3f(q.x / 0.0007, q.y / 0.004, 3.0));
    c = mix(c, lip * streak, g.lips);
    c *= 1.0 - 0.22 * min(cavity, 1.0);

    var r = rough - 0.06 * g.face + 0.03 * g.cheek - 0.12 * f.oil * g.tzone + 0.18 * min(cavity, 1.0);
    r = r + (0.34 - r) * lip_gloss;
    let coat = clamp(f.oil * g.tzone * 0.3 * (0.6 + 0.8 * mat_noise(p / 0.004)) + lip_gloss * 0.25, 0.0, 1.0);
    let sheen = f.fuzz * 0.18 * g.face * (1.0 - g.lips);
    let keep = smoothstep1(f.low_y + 0.012, f.low_y + 0.03, p.y);
    return FaceLook(normalize_or(mix(n, bumped, keep), n), mix(color, clamp(c, vec3f(0.0), vec3f(1.0)), keep), rough + (clamp(r, 0.08, 1.0) - rough) * keep, coat * keep, sheen * keep);
}

fn cell_hash(c: vec3i) -> u32 {
    var h = (bitcast<u32>(c.x) * 0x8da6b343u) ^ (bitcast<u32>(c.y) * 0xd8163841u) ^ (bitcast<u32>(c.z) * 0xcb1ab31fu);
    h ^= h >> 16u; h *= 0x7feb352du; h ^= h >> 15u; h *= 0x846ca68bu;
    return h ^ (h >> 16u);
}

// Root pigment under dense hair (ScalpTint::at): (color, weight).
fn scalp_at(p: vec3f) -> vec4f {
    if (P.env_size.w == 0u) { return vec4f(0.0); }
    if (any(p < P.scalp_lo.xyz) || any(p > P.scalp_hi.xyz)) { return vec4f(0.0); }
    let k = vec3i(floor(p / SCALP_CELL));
    var best = vec4f(0.0);
    var nearest = SCALP_CELL;
    for (var dz = -1; dz <= 1; dz++) {
        for (var dy = -1; dy <= 1; dy++) {
            for (var dx = -1; dx <= 1; dx++) {
                let c = k + vec3i(dx, dy, dz);
                var slot = cell_hash(c) & P.env_size.w;
                loop {
                    let e = scalp[slot];
                    if (e.key.w == 0) { break; }
                    if (all(e.key.xyz == c)) {
                        let d = distance(e.root.xyz, p);
                        if (d < nearest) {
                            nearest = d;
                            let t = 1.0 - d / SCALP_CELL;
                            best = vec4f(e.color.xyz * 0.7, 0.85 * t * t * (3.0 - 2.0 * t) * e.root.w / (e.root.w + 8.0));
                        }
                        break;
                    }
                    slot = (slot + 1u) & P.env_size.w;
                }
            }
        }
    }
    return best;
}

// ---------------------------------------------------------------- detail maps

// detail.rs: Map::raw, sample, Params::triplanar and apply.
struct Raw { a: vec4f, b: vec4f }

fn detail_raw(m: DMap, level: u32, uv: vec2f) -> Raw {
    var off = m.info.x;
    for (var j = 0u; j < level; j++) { let s = m.info.y >> j; off += 2u * s * s; }
    let side = m.info.y >> level;
    let s = f32(side);
    let x = uv.x * s - 0.5;
    let y = -uv.y * s - 0.5;
    let x0 = floor(x);
    let y0 = floor(y);
    let fx = x - x0;
    let fy = y - y0;
    let si = i32(side);
    let ix0 = u32(((i32(x0) % si) + si) % si);
    let iy0 = u32(((i32(y0) % si) + si) % si);
    let ix1 = (ix0 + 1u) % side;
    let iy1 = (iy0 + 1u) % side;
    let t00 = off + 2u * (iy0 * side + ix0);
    let t10 = off + 2u * (iy0 * side + ix1);
    let t01 = off + 2u * (iy1 * side + ix0);
    let t11 = off + 2u * (iy1 * side + ix1);
    let a = mix(mix(unpack4x8unorm(texels[t00]), unpack4x8unorm(texels[t10]), fx), mix(unpack4x8unorm(texels[t01]), unpack4x8unorm(texels[t11]), fx), fy);
    let b = mix(mix(unpack4x8unorm(texels[t00 + 1u]), unpack4x8unorm(texels[t10 + 1u]), fx), mix(unpack4x8unorm(texels[t01 + 1u]), unpack4x8unorm(texels[t11 + 1u]), fx), fy);
    return Raw(a, b);
}

struct DTexel { tint: vec3f, slope: vec2f, rough: f32, ao: f32 }

fn detail_sample(m: DMap, uv: vec2f, lod0: f32) -> DTexel {
    let levels = m.info.z;
    let lod = clamp(lod0, 0.0, f32(levels - 1u));
    let lo = u32(floor(lod));
    var r = detail_raw(m, lo, uv);
    if (lod > f32(lo) + 1e-3 && lo + 1u < levels) {
        let f = lod - f32(lo);
        let r2 = detail_raw(m, lo + 1u, uv);
        r = Raw(mix(r.a, r2.a, f), mix(r.b, r2.b, f));
    }
    return DTexel(r.a.xyz / m.mean.xyz, vec2f(r.a.w * 2.0 - 1.0, r.b.x * 2.0 - 1.0) * m.extra.y, r.b.y - m.mean.w, r.b.z / m.extra.x);
}

struct Detailed { n: vec3f, color: vec3f, rough: f32 }

fn detail_apply(mat: Mat, p: vec3f, n: vec3f, color: vec3f, rough: f32, footprint: f32) -> Detailed {
    return detail_apply_with(u32(mat.d.x), mat.d.y, mat.d.z, mat.d.w, mat.e.x, mat.e.y, p, n, color, rough, footprint);
}

// Params::apply with map slot, tiles per unit, strength, color and turn.
fn detail_apply_with(slot: u32, freq: f32, strength: f32, dcolor: f32, c: f32, s: f32, p: vec3f, n: vec3f, color: vec3f, rough: f32, footprint: f32) -> Detailed {
    let m = dmaps[slot];
    let density = f32(m.info.y) * freq;
    let lod = log2(max(footprint * density, 1.0));
    var w = pow(abs(n), vec3f(4.0));
    w = w / max(w.x + w.y + w.z, 1e-12);
    var tint = vec3f(0.0);
    var rr = 0.0;
    var ao = 0.0;
    var bend = vec3f(0.0);
    for (var axis = 0u; axis < 3u; axis++) {
        if (w[axis] < 0.003) { continue; }
        let sign = select(1.0, -1.0, n[axis] < 0.0);
        var tu: vec3f;
        var tv: vec3f;
        if (axis == 0u) { tu = vec3f(0.0, 0.0, -sign); tv = vec3f(0.0, 1.0, 0.0); }
        else if (axis == 1u) { tu = vec3f(1.0, 0.0, 0.0); tv = vec3f(0.0, 0.0, -sign); }
        else { tu = vec3f(sign, 0.0, 0.0); tv = vec3f(0.0, 1.0, 0.0); }
        let uv0 = vec2f(dot(p, tu), dot(p, tv)) * freq;
        let uv = vec2f(c * uv0.x - s * uv0.y, s * uv0.x + c * uv0.y);
        let tx = detail_sample(m, uv, lod);
        let ru = tu * c - tv * s;
        let rv = tu * s + tv * c;
        bend += (ru * tx.slope.x + rv * tx.slope.y) * w[axis];
        tint += tx.tint * w[axis];
        rr += tx.rough * w[axis];
        ao += tx.ao * w[axis];
    }
    bend = bend - n * dot(n, bend);
    let normal = normalize_or(n + bend * strength, n);
    let t = mix(vec3f(1.0), tint, dcolor);
    let a = 1.0 + (min(ao, 1.5) - 1.0) * min(0.5 * strength, 1.0);
    let r = clamp(rough + rr * 0.6 * min(strength, 1.5), 0.03, 1.0);
    return Detailed(normal, clamp(color * t * a, vec3f(0.0), vec3f(1.0)), r);
}

// ---------------------------------------------------------------- smart materials

// smart.rs: layers, masks and decals, kept in step with the CPU.
struct SmartAt { p: vec3f, n: vec3f, edge: f32, cavity: f32, occl: f32, painted: vec4f, footprint: f32 }
struct SmartLook { color: vec3f, rough: f32, metal: f32, coat: f32, sheen: f32, emission: f32, n: vec3f }

fn fv4(o: u32) -> vec4f { return vec4f(floats[o], floats[o + 1u], floats[o + 2u], floats[o + 3u]); }

fn smart_fbm(p: vec3f, octaves: u32) -> f32 {
    let oct = clamp(octaves, 1u, 6u);
    var sum = 0.0;
    var amp = 1.0;
    var norm = 0.0;
    var q = p;
    for (var i = 0u; i < oct; i++) {
        sum += amp * mat_noise(q);
        norm += amp;
        amp *= 0.5;
        q = q * 2.03 + vec3f(17.17);
    }
    return clamp((sum / norm - 0.5) * (1.6 + 0.35 * f32(oct - 1u)) + 0.5, 0.0, 1.0);
}

fn smart_levels(v: f32, t: f32, c: f32) -> f32 {
    if (c <= 0.0) { return v; }
    let h = 0.5 / c;
    return smoothstep1(t - h, t + h, v);
}

// A library map's height, triplanar and unturned (smart::grunge_height).
fn smart_grunge(slot: u32, freq: f32, p: vec3f, n: vec3f, footprint: f32) -> f32 {
    let m = dmaps[slot];
    let density = f32(m.info.y) * freq;
    let levels = m.info.z;
    let lod = clamp(log2(max(footprint * density, 1.0)), 0.0, f32(levels - 1u));
    let lo = u32(floor(lod));
    var w = pow(abs(n), vec3f(4.0));
    w = w / max(w.x + w.y + w.z, 1e-12);
    var h = 0.0;
    for (var axis = 0u; axis < 3u; axis++) {
        if (w[axis] < 0.003) { continue; }
        let sign = select(1.0, -1.0, n[axis] < 0.0);
        var tu: vec3f;
        var tv: vec3f;
        if (axis == 0u) { tu = vec3f(0.0, 0.0, -sign); tv = vec3f(0.0, 1.0, 0.0); }
        else if (axis == 1u) { tu = vec3f(1.0, 0.0, 0.0); tv = vec3f(0.0, 0.0, -sign); }
        else { tu = vec3f(sign, 0.0, 0.0); tv = vec3f(0.0, 1.0, 0.0); }
        let uv = vec2f(dot(p, tu), dot(p, tv)) * freq;
        var r = detail_raw(m, lo, uv).b.w;
        if (lod > f32(lo) + 1e-3 && lo + 1u < levels) {
            r = mix(r, detail_raw(m, lo + 1u, uv).b.w, lod - f32(lo));
        }
        h += r * w[axis];
    }
    return clamp(0.5 + (h - m.extra.z) / (4.0 * m.extra.w), 0.0, 1.0);
}

// One mask term at float offset o: (source, op, invert, amount),
// (threshold, contrast, frequency, octaves), (parameters, seed),
// and two vec4 for the compiled world-normal metric.
fn smart_term(o: u32, at: SmartAt) -> f32 {
    let a = fv4(o);
    let b = fv4(o + 4u);
    let c = fv4(o + 8u);
    let seed = vec3f(1.7, 3.1, 5.3) * c.w;
    var v = 0.0;
    switch u32(a.x) {
        case 0u: { v = c.x; }
        case 1u: { v = at.edge; }
        case 2u: { v = at.cavity; }
        case 3u: { v = at.occl; }
        case 4u: { v = at.n.y; }
        case 5u: { v = -at.n.y; }
        case 6u: { v = dot(at.n, c.xyz); }
        case 7u: {
            if (c.y == c.x) { v = select(0.0, 1.0, at.p.y >= c.x); } else { v = (at.p.y - c.x) / (c.y - c.x); }
        }
        case 13u: {
            let axis = normalize(c.xyz);
            let p = at.p - axis * dot(at.p, axis) * 0.96;
            v = smart_fbm(p * b.z + seed, u32(b.w));
        }
        case 8u: { v = smart_fbm(at.p * b.z + seed, u32(b.w)); }
        case 9u: {
            if (c.x < 0.0) { v = 0.5; } else { v = smart_grunge(u32(c.x), b.z, at.p + seed * 0.01, at.n, at.footprint); }
        }
        case 11u: { v = dot(at.p,c.xyz) + c.w; }
        case 12u: {
            let m=fv4(o+12u);let e=fv4(o+16u);let n=at.n;
            let len=sqrt(max(dot(m.xyz,n*n)+2.0*(m.w*n.x*n.y+e.x*n.x*n.z+e.y*n.y*n.z),1e-12));
            v=dot(n,c.xyz)/len;
        }
        case 10u: {
            let s = u32(c.x);
            if (s < 4u) { v = at.painted[s]; }
        }
        default: {}
    }
    v = clamp(v, 0.0, 1.0);
    if (a.z > 0.5) { v = 1.0 - v; }
    return smart_levels(v, b.x, b.y) * a.w;
}

fn smart_fold(acc: f32, v: f32, op: u32) -> f32 {
    switch op {
        case 1u: { return max(acc, v); }
        case 2u: { return min(acc, v); }
        case 3u: { return acc + v; }
        case 4u: { return acc - v; }
        case 5u: { return 1.0 - (1.0 - acc) * (1.0 - v); }
        default: { return acc * v; }
    }
}

// smart::layers: `count` layers from float offset `off`.
fn smart_layers(off: u32, count: u32, at: SmartAt, look0: SmartLook) -> SmartLook {
    var look = look0;
    var o = off;
    for (var li = 0u; li < count; li++) {
        let h0 = fv4(o);
        let h1 = fv4(o + 4u);
        let h2 = fv4(o + 8u);
        let h3 = fv4(o + 12u);
        let h4 = fv4(o + 16u);
        let h5 = fv4(o + 20u);
        let h6 = fv4(o + 24u);
        let terms = u32(h5.z);
        var v = 1.0;
        for (var ti = 0u; ti < terms; ti++) {
            let to = o + 28u + ti * 20u;
            let x = smart_term(to, at);
            if (ti == 0u) { v = x; } else { v = smart_fold(v, x, u32(floats[to + 1u])); }
        }
        o = o + 28u + terms * 20u;
        v = clamp(v, 0.0, 1.0);
        if (h6.y > 0.5) { v = 1.0 - v; }
        if (h3.w > 0.0) { v = smart_levels(v, h3.z, h3.w); } else { v = pow(v, h5.w); }
        let w = v * h1.w;
        if (w <= 0.0) { continue; }
        let flags = u32(h0.w);
        var c = select(look.color, h0.xyz, (flags & 1u) != 0u);
        if ((flags & 2u) != 0u) { c *= h1.xyz; }
        if (h3.y > 0.0) {
            let q = at.p * h6.x;
            let vv = mat_noise(q + vec3f(31.7)) * 0.65 + mat_noise(q * 2.7 + vec3f(7.3)) * 0.35 - 0.5;
            c *= 1.0 + vv * 0.8 * h3.y;
        }
        var rough = select(look.rough, h2.x, (flags & 4u) != 0u);
        var nn = look.n;
        if (h4.x >= 0.0) {
            let dt = detail_apply_with(u32(h4.x), h4.y, h4.z, h4.w, h5.x, h5.y, at.p, look.n, c, rough, at.footprint);
            nn = dt.n;
            c = dt.color;
            rough = dt.rough;
        }
        look.color = mix(look.color, clamp(c, vec3f(0.0), vec3f(1.0)), w);
        look.rough = look.rough + (rough - look.rough) * w;
        look.metal += (select(look.metal, h2.y, (flags & 8u) != 0u) - look.metal) * w;
        look.coat += (select(look.coat, h2.z, (flags & 16u) != 0u) - look.coat) * w;
        look.sheen += (select(look.sheen, h2.w, (flags & 32u) != 0u) - look.sheen) * w;
        look.emission += (select(look.emission, h3.x, (flags & 64u) != 0u) - look.emission) * w;
        look.n = normalize_or(mix(look.n, nn, w), look.n);
    }
    look.color = clamp(look.color, vec3f(0.0), vec3f(1.0));
    look.rough = clamp(look.rough, 0.03, 1.0);
    return look;
}

fn smart_hash(h0: u32) -> u32 {
    var h = h0;
    h ^= h >> 16u; h *= 0x7feb352du; h ^= h >> 15u; h *= 0x846ca68bu;
    return h ^ (h >> 16u);
}

fn smart_rand(seed: u32, i: u32) -> f32 {
    return f32(smart_hash((seed * 0x9E3779B9u) ^ (i * 0x85EBCA6Bu + 0x632BE5ABu)) >> 8u) / 16777216.0;
}

fn smart_seg(p: vec2f, a: vec2f, b: vec2f) -> f32 {
    let ab = b - a;
    let t = clamp(dot(p - a, ab) / max(dot(ab, ab), 1e-12), 0.0, 1.0);
    return length(p - a - ab * t);
}

fn rune_point(k: u32) -> vec2f { return vec2f((f32(k % 3u) - 1.0) * 0.22, (f32(k / 3u) - 1.5) * 0.24); }

// smart::shape.
fn smart_shape(code: u32, seed: u32, uv: vec2f) -> f32 {
    if (code == 0u) {
        var d = smart_seg(uv, vec2f(0.0, -0.36), vec2f(0.0, 0.36));
        let branches = 2u + u32(smart_rand(seed, 0u) * 3.0);
        for (var i = 0u; i < 4u; i++) {
            if (i < branches) {
                let row = min(u32(smart_rand(seed, 1u + i) * 4.0), 3u);
                let side = select(2u, 0u, smart_rand(seed, 10u + i) < 0.5);
                let to_row = u32(clamp(i32(row) + i32(smart_rand(seed, 20u + i) * 3.0) - 1, 0, 3));
                d = min(d, smart_seg(uv, rune_point(1u + 3u * row), rune_point(side + 3u * to_row)));
            }
        }
        if (smart_rand(seed, 30u) > 0.45) { d = min(d, abs(length(uv) - 0.46)); }
        return 1.0 - smoothstep1(0.018, 0.032, d);
    }
    if (code == 1u) {
        var a = 0.0;
        for (var i = 0u; i < 14u; i++) {
            let c = vec2f(smart_rand(seed, i) - 0.5, smart_rand(seed, 20u + i) - 0.5) * 0.75;
            let ang = (smart_rand(seed, 40u + i) - 0.5) * 0.6;
            let half = 0.06 + smart_rand(seed, 60u + i) * 0.3;
            let w = 0.004 + smart_rand(seed, 80u + i) * 0.008;
            let dir = vec2f(cos(ang), sin(ang));
            let p0 = c - dir * half;
            let p1 = c + dir * half;
            let t = clamp(dot(uv - p0, dir) / (2.0 * half), 0.0, 1.0);
            let k = 2.0 * t - 1.0;
            let ww = w * sqrt(max(1.0 - k * k, 0.0));
            let cov = 1.0 - smoothstep1(ww * 0.4, max(ww, 1e-4), smart_seg(uv, p0, p1));
            a = max(a, cov * (0.6 + 0.4 * smart_rand(seed, 100u + i)));
        }
        return a;
    }
    if (code == 2u) {
        let r = length(uv);
        let th = atan2(uv.y, uv.x);
        let rim = 0.25 + 0.05 * sin(3.0 * th + smart_rand(seed, 0u) * TAU) + 0.035 * sin(5.0 * th + smart_rand(seed, 1u) * TAU)
            + 0.02 * sin(9.0 * th + smart_rand(seed, 2u) * TAU) + 0.012 * sin(15.0 * th + smart_rand(seed, 3u) * TAU);
        var a = 1.0 - smoothstep1(rim - 0.012, rim + 0.012, r);
        for (var i = 0u; i < 12u; i++) {
            let ang = smart_rand(seed, 10u + i) * TAU;
            let dist = 0.29 + smart_rand(seed, 30u + i) * 0.19;
            let rad = (0.008 + smart_rand(seed, 50u + i) * 0.028) * (1.25 - (dist - 0.29) / 0.19 * 0.75);
            let d = length(uv - vec2f(cos(ang), sin(ang)) * dist);
            a = max(a, 1.0 - smoothstep1(rad - 0.005, rad + 0.005, d));
        }
        return a;
    }
    if (code == 3u) {
        let r = length(uv);
        let th = atan2(uv.y, uv.x);
        let points = 4u + u32(smart_rand(seed, 0u) * 4.0);
        let f = (th + smart_rand(seed, 1u) * TAU) * f32(points) / TAU;
        let tri = abs((f - floor(f)) - 0.5) * 2.0;
        let star_r = 0.14 + 0.2 * tri * tri;
        let star = 1.0 - smoothstep1(star_r - 0.01, star_r + 0.01, r);
        let ring = 1.0 - smoothstep1(0.018, 0.03, abs(r - 0.43));
        return max(ring, star);
    }
    return 0.0;
}

struct DecalHit { a: f32, h: f32, rgb: vec3f }

struct DamageHit { coverage: f32, height: f32, color: vec3f, rough: f32, metal: f32 }
fn damage_hit(code: u32, seed: u32, uv: vec2f) -> DamageHit {
    let r=length(uv); let theta=atan2(uv.y,uv.x);
    let phase=smart_rand(seed,0u)*6.28318530718;
    let rag=(mat_noise(vec3f(cos(theta)*5.0,sin(theta)*5.0,phase))-0.5)*0.065+sin(theta*19.0+phase)*0.004;
    var h=DamageHit(1.0-smoothstep1(0.38,0.5,r),0.0,vec3f(0.52,0.54,0.53),0.38,1.0);
    switch code {
        case 5u,8u: {
            let edge = r - rag * (1.0 + 0.5 * sin((theta * 3.0 + phase)));
            let core = 1.0 - smoothstep1(0.065, 0.145, edge);
            let lip = (1.0 - smoothstep1(0.012, 0.052, abs((edge - 0.17))))
                * (0.35 + 0.65 * abs(cos((theta * 5.0 + phase))));
            h.height = -core * 1.7 + lip * 0.32;
            let steel = 1.0 - smoothstep1(0.22, 0.29, edge);
            let primer = 1.0 - smoothstep1(0.29, 0.345, edge);
            let abrasion = sin((theta * 31.0 + phase + r * 98.0)) * 0.5 + 0.5;
            h.color = mix(mix(mix(vec3f(0.12, 0.105, 0.075), vec3f(0.34, 0.29, 0.20), primer), vec3f(0.48, 0.50, 0.49) * (0.75 + 0.25 * abrasion), steel), vec3f(0.016, 0.018, 0.017), core);
            h.metal = steel * (1.0 - core * 0.94);
            h.rough = 0.72 - steel * 0.27 + core * 0.38;
            h.coverage = 1.0 - smoothstep1(0.32, 0.43, edge);
            let fan = (1.0 - smoothstep1(0.15, 0.48, r))
                * smoothstep1(-0.4, 0.8, cos((theta - phase)));
            h.color = mix(h.color, vec3f(0.055, 0.05, 0.04), fan * (1.0 - primer) * 0.65);
            for (var i=0u; i<9u; i++) {
                let a = smart_rand(seed, i + 1u) * TAU;
                let c = vec2f(cos(a), sin(a)) * (0.25 + smart_rand(seed, i + 20u) * 0.18);
                let pit = 1.0 - smoothstep1(0.004, 0.012 + smart_rand(seed, i + 40u) * 0.013, length((uv - c)));
                h.height -= pit * 0.32;
                h.color = mix(h.color, vec3f(0.12, 0.13, 0.12), pit);
                h.coverage = max(h.coverage, pit * 0.9);
            }
            if code == 8u {
                let char = 1.0 - smoothstep1(0.14, 0.40, edge);
                h.color = mix(h.color, vec3f(0.035, 0.03, 0.025), char * 0.75);
                h.metal *= 1.0 - char * 0.75;
                h.rough = max(h.rough, 0.8);
            }
        }
        case 6u: {
            let bowl = 1.0 - smoothstep1(0.0, 0.45, r);
            h.height = -bowl * bowl;
            let crack = 1.0 - smoothstep1(0.009, 0.023, abs((r - 0.29 - rag * 2.0)));
            h.coverage = crack * smoothstep1(0.38,0.68,mat_noise(vec3f(uv.x*33.0,uv.y*33.0,phase)));
            h.color = vec3f(0.38, 0.40, 0.38);
            h.rough = 0.46;
        }
        case 7u: {
            // Fine blister cells affect only the burnt paint, never the soot
            // opacity. High-frequency opacity used to expose a square noise
            // lattice (size / 49: 14 mm on the saved torso burn).
            let cells = mat_noise(vec3f(uv.x * 49.0, uv.y * 49.0, phase));
            let drift = mat_noise(vec3f(uv.x * 4.1 + 7.2, uv.y * 3.7, phase));
            let edge = r + (drift - 0.5) * 0.025;
            let bare = 1.0 - smoothstep1(0.020, 0.045, edge);
            let burnt = 1.0 - smoothstep1(0.035, 0.085, edge);
            let heat = smoothstep1(0.015, 0.08, r);
            h.color = mix(vec3f(0.055, 0.041, 0.028), mix(vec3f(0.085, 0.070, 0.045), vec3f(0.055, 0.065, 0.080), heat), bare);
            let ash = smoothstep1(0.64, 0.82, cells) * burnt * (1.0 - bare);
            h.color = mix(h.color, vec3f(0.28, 0.265, 0.235), ash * 0.35);
            // A continuous fan widens and diffuses uphill. Analytic falloff
            // removes texel-sized blotches without blurring the peeled center.
            let rise = smoothstep1(-0.12, 0.48, uv.y);
            let width = 0.075 + rise * 0.19;
            let wind = 0.035 * rise * rise;
            let cross = (uv.x - wind) / width;
            let plume = exp(-cross * cross * 2.0)
                * smoothstep1(-0.18, -0.02, uv.y) * (1.0 - smoothstep1(0.08, 0.49, uv.y));
            let halo = exp(-r * r * 65.0) * 0.25;
            h.coverage = max(max(burnt * 0.88, plume * 0.78), halo);
            h.rough = 0.91 - bare * (0.15 + cells * 0.10);
            h.metal = bare * (0.18 + cells * 0.12);
            h.height = smoothstep1(0.48, 0.80, cells) * burnt * (1.0 - bare) * 0.08 - bare * 0.025;
        }
        case 9u,10u: {
            let end = 1.0 - smoothstep1(0.42, 0.5, abs(uv.y));
            let width = 0.065 + 0.025 * sin(uv.y * 9.0 + phase)
                + 0.008 * sin(uv.y * 27.0 + phase);
            let x = abs(uv.x);
            let groove = 1.0 - smoothstep1(width * 0.65, width, x);
            let burr = (1.0 - smoothstep1(0.006, 0.020, abs(x - width * 1.15))) * (1.0 - groove);
            let chip = 1.0 - smoothstep1(width + 0.028, width + 0.065, x + rag * 0.15);
            let fine = (1.0 - smoothstep1(0.12, 0.22, x))
                * smoothstep1(0.94, 0.995, sin(uv.x * 330.0 + phase + uv.y * 2.0));
            h.coverage = max(chip, fine * 0.35) * end;
            h.height = (-groove * 0.18 + burr * 0.045 - fine * 0.008) * end * select(1.0, 0.2, code == 10u);
            h.color = mix(mix(vec3f(0.30, 0.255, 0.18), vec3f(0.65, 0.67, 0.65), max(groove, fine)), vec3f(0.10, 0.085, 0.065), burr * 0.85);
            h.metal = max(max(groove, burr), fine);
            h.rough = 0.63 - groove * 0.31;
        }
        case 11u,12u,13u: {

            let t = clamp((0.5 - uv.y), 0.0, 1.0);
            let center = sin((t * 14.0 + phase)) * 0.018;
            let width = 0.095 + 0.23 * t;
            let x = abs((uv.x - center));
            let breakup = mat_noise(vec3f(uv.x * 28.0, t * 19.0, phase));
            let stain = (1.0 - smoothstep1(width * 0.15, width, x)) * (1.0 - smoothstep1(0.68, 1.0, t));
            let source = 1.0 - smoothstep1(0.015, 0.055, length(vec2f(uv.x, (t-0.07) * 0.8)));
            h.coverage = max((stain * (0.45 + breakup * 0.45) * smoothstep1(0.0,0.06,t)), source);
            h.color = mix(mix(vec3f(0.29, 0.19, 0.105), vec3f(0.49, 0.34, 0.19), smoothstep1(0.0, 0.65, t)), vec3f(0.16, 0.105, 0.055), source * 0.8);
            h.rough = 0.86;
            h.metal = 0.0;
            if code == 12u {
                let wet_width = 0.012 + 0.060 * t;
                let wet = 1.0 - smoothstep1(wet_width * 0.35, wet_width, x);
                let tail = 1.0 - smoothstep1(0.80, 0.98, t);
                let drip = 1.0 - smoothstep1(0.012, 0.027, length(vec2f(uv.x - center, (t - 0.86) * 0.7)));
                let film = max(wet * smoothstep1(0.0, 0.035, t) * tail, drip * 0.8);
                let dust = (1.0 - wet) * stain * (0.35 + breakup * 0.3);
                h.coverage = max(film * (0.72 + breakup * 0.08), dust * 0.42);
                h.color = mix(vec3f(0.27, 0.20, 0.12), vec3f(0.065, 0.035, 0.014), film);
                h.rough = 0.76 - film * 0.65;
            }
            if code == 13u {
                h.color = vec3f(0.34, 0.32, 0.26);
                h.rough = 0.80;
                h.coverage *= 0.55;
            }
        }
        case 14u,15u: {
            let edge=1.0-smoothstep1(0.35,0.5,abs(uv.y));h.height=max(uv.x,0.0)*edge*4.0;
            let jitter=select(0.014*sin(uv.y*91.0+phase)+0.009*sin(uv.y*157.0-phase),0.004*(sin(uv.y*123.0+phase)+0.6*sin(uv.y*71.0)),code==14u);
            let seam=1.0-smoothstep1(select(0.010,0.003,code==14u),select(0.030,0.010,code==14u),abs(uv.x-jitter));
            h.coverage=seam*edge;h.color=vec3f(0.55,0.55,0.50);
            if code==15u {h.height-=seam*edge*2.0;}
        }
        default: {h.coverage=0.0;}
    }
    return h;
}

// Full damage result, including global runoff progress, shared by color and PBR.
fn damage_decal_hit(o: u32, uv: vec2f) -> DamageHit {
    let code=u32(floats[o+11u]);
    var coord=uv;
    if code>=11u && code<=13u {
        let range=fv4(o+24u).xy;
        coord.y=1.0-range.y+(uv.y+0.5)*(range.y-range.x)-0.5;
    }
    var hit=damage_hit(code,bitcast<u32>(floats[o+15u]),coord);
    if code>=11u && code<=13u {hit.coverage=pow(hit.coverage,1.0/max(floats[o+27u],0.1));}
    if code==7u && floats[o+26u]>0.5 {hit.height+=(1.0-smoothstep1(0.01,0.04,abs(length(uv)-0.30)))*0.8;}
    return hit;
}

fn smart_decal_at(o: u32, uv: vec2f) -> DecalHit {
    let code = u32(floats[o + 11u]);
    let d6 = fv4(o + 24u);
    let size = bitcast<u32>(d6.y);
    if (code == 4u && size > 0u) {
        let c = sample_tex(bitcast<u32>(d6.x), size, vec2f(uv.x + 0.5, 0.5 - uv.y));
        return DecalHit(c.w, c.w * dot(c.xyz, vec3f(0.3, 0.59, 0.11)), c.xyz);
    }
    if (code == 4u) { return DecalHit(0.0, 0.0, vec3f(0.0)); }
    if code>=5u {
        let hit=damage_decal_hit(o,uv);
        return DecalHit(hit.coverage,hit.height,hit.color);
    }
    let a = smart_shape(code, bitcast<u32>(floats[o + 15u]), uv);
    return DecalHit(a, a, vec3f(0.0));
}

// smart::decals: `count` decals of 7 vec4 from float offset `first`.
fn smart_decals(first: u32, count: u32, at: SmartAt, look0: SmartLook) -> SmartLook {
    var look = look0;
    for (var i = 0u; i < count; i++) {
        let o = first + i * 28u;
        let d0 = fv4(o);
        let d1 = fv4(o + 4u);
        let d2 = fv4(o + 8u);
        let d3 = fv4(o + 12u);
        let d4 = fv4(o + 16u);
        let d5 = fv4(o + 20u);
        let rel = at.p - d0.xyz;
        if (abs(dot(rel, d1.xyz)) > d0.w * 0.5) { continue; }
        let facing = dot(at.n, d1.xyz);
        if (facing < 0.15) { continue; }
        let uv = vec2f(dot(rel, d2.xyz), dot(rel, d3.xyz));
        if (abs(uv.x) > 0.5 || abs(uv.y) > 0.5) { continue; }
        let fade = smoothstep1(0.15, 0.4, facing) * d5.w;
        let hit = smart_decal_at(o, uv);
        let a = hit.a * fade;
        let relief = d1.w;
        if (relief != 0.0) {
            let e = 0.003;
            let hx = (smart_decal_at(o, uv + vec2f(e, 0.0)).h - smart_decal_at(o, uv - vec2f(e, 0.0)).h) / (2.0 * e);
            let hy = (smart_decal_at(o, uv + vec2f(0.0, e)).h - smart_decal_at(o, uv - vec2f(0.0, e)).h) / (2.0 * e);
            var bend = -(d2.xyz * d0.w * hx + d3.xyz * d0.w * hy) * relief * 0.01 * fade;
            bend = bend - look.n * dot(look.n, bend);
            look.n = normalize_or(look.n + bend, look.n);
        }
        if (a <= 0.0) { continue; }
        let flags = u32(d4.w);
        if u32(d2.w)>=5u {
            let hit=damage_decal_hit(o,uv);
            if ((flags & 1u)==0u) {look.color=mix(look.color,hit.color,a);}
            if ((flags & 2u)==0u) {look.rough=mix(look.rough,hit.rough,a);}
            if ((flags & 4u)==0u) {look.metal=mix(look.metal,hit.metal,a);}
        }
        if ((flags & 1u) != 0u) {
            look.color = mix(look.color, d4.xyz, a);
        } else if ((flags & 16u) != 0u) {
            look.color = mix(look.color, hit.rgb, a);
        } else if ((flags & 8u) != 0u) {
            look.color = mix(look.color, min(look.color * 1.6 + vec3f(0.08), vec3f(1.0)), a);
        } else if ((flags & 32u) != 0u) {
            look.color = mix(look.color, look.color * 0.3, a);
        }
        if ((flags & 2u) != 0u) { look.rough += (d5.x - look.rough) * a; }
        if ((flags & 4u) != 0u) { look.metal += (d5.y - look.metal) * a; }
        if (d5.z > 0.0) { look.emission += (d5.z - look.emission) * a; }
    }
    look.color = clamp(look.color, vec3f(0.0), vec3f(1.0));
    look.rough = clamp(look.rough, 0.03, 1.0);
    return look;
}

// ---------------------------------------------------------------- surfaces

struct Surface { albedo: vec3f, rough: f32, metal: f32, coat: f32, sheen: f32, skin: f32 }

struct SurfHit {
    p: vec3f, ng: vec3f, n: vec3f,
    s: Surface,
    has_sss: bool, sss_amount: f32, sss_radius: vec3f, sss_item: u32,
    emission: f32, emitted: vec3f, transmission: f32, ior: f32, front: bool,
    has_hair: bool, hair_dir: vec3f,
    flame: bool,
}

fn surface(o: vec3f, d: vec3f, t: f32, h: Hit) -> SurfHit {
    let tri = tri_info[h.tri];
    let it = items[tri.x];
    let v0 = verts[tri.y];
    let v1 = verts[tri.z];
    let v2 = verts[tri.w];
    let w0 = 1.0 - h.u - h.v;
    let p = o + d * t;
    let pw0 = model(it, vpos(v0));
    let pw1 = model(it, vpos(v1));
    let pw2 = model(it, vpos(v2));
    var ng = normalize_or(cross(pw1 - pw0, pw2 - pw0), vec3f(0.0, 1.0, 0.0));
    var n = normalize_or(normal_of(it, vnormal(v0) * w0 + vnormal(v1) * h.u + vnormal(v2) * h.v), ng);
    let front = dot(ng, d) < 0.0;
    if (dot(ng, d) > 0.0) { ng = -ng; }
    if (dot(n, ng) < 0.0) { n = -n; }
    let color = srgb(vcolor(v0)) * w0 + srgb(vcolor(v1)) * h.u + srgb(vcolor(v2)) * h.v;
    var s4 = v0.surface;
    let mi = material_index(it, v0);
    let m = mats[mi];
    if (s4.x < 0.0) { s4 = it.surface; }
    var r: SurfHit;
    r.p = p;
    r.front = front;
    r.emission = m.a.x;
    r.emitted = vec3f(0.0);
    r.transmission = m.a.y;
    r.ior = max(m.a.z, 1.0);
    r.has_sss = m.a.w > 0.0 && m.a.y <= 0.0;
    r.sss_amount = m.a.w;
    r.sss_radius = m.b.xyz;
    r.sss_item = tri.x;
    r.has_hair = false;
    r.flame = m.c.w > 0.5;
    r.s = Surface(color * it.tint.xyz, s4.x, s4.y, s4.z, s4.w, m.b.w * (1.0 - s4.y));
    let has_tex = (it.info.w & 1u) != 0u;
    // Smart layers and decals start from the color before them; the tint
    // goes on last.
    let smart = !has_tex && it.smart.x != NONE && (m.e.w > 0.0 || it.smart.z > 0u);
    var sv0: u32;
    var sv1: u32;
    var sv2: u32;
    if (smart) {
        sv0 = it.smart.x + tri.y * 8u;
        sv1 = it.smart.x + tri.z * 8u;
        sv2 = it.smart.x + tri.w * 8u;
        let base = fv4(sv0) * w0 + fv4(sv1) * h.u + fv4(sv2) * h.v;
        r.s.albedo = srgb(base.xyz);
        if (floats[sv0 + 3u] >= 0.0 && v0.surface.x >= 0.0) { r.s.rough = base.w; }
        if (v0.surface.x >= 0.0) {
            r.s.metal = m.f.x;
            r.s.coat = m.f.y;
            r.s.sheen = m.f.z;
        }
        r.s.skin = m.b.w * (1.0 - r.s.metal);
    }
    if (has_tex && v0.tangent.w != 0.0) {
        let uv = lerp_uv(tri, h.u, h.v);
        let c = sample_tex(it.tex.y, it.tex.x, imported_uv(it, uv, 0u));
        r.s.albedo *= srgb(c.xyz);
        let orm = sample_tex(it.tex.w, it.tex.x, imported_uv(it, uv, 2u));
        r.s.rough = orm.y;
        r.s.metal = orm.z;
        let nm = sample_tex(it.tex.z, it.tex.x, imported_uv(it, uv, 1u));
        let tn = nm.xyz * 2.0 - vec3f(1.0);
        var tan = model(it, v0.tangent.xyz * w0 + v1.tangent.xyz * h.u + v2.tangent.xyz * h.v);
        tan = normalize_or(tan - n * dot(n, tan), ortho(n));
        let bit = cross(n, tan) * sign(v0.tangent.w);
        if ((it.info.w & 2u) != 0u && m.c.z > 0.5) {
            r.has_hair = true;
            r.hair_dir = select(-bit, bit, front);
        }
        n = normalize_or(tan * tn.x + bit * tn.y + n * tn.z, n);
        let ao = sample_tex(it.tex.w, it.tex.x, imported_uv(it, uv, 3u)).x;
        r.s.albedo *= 0.5 + 0.5 * ao;
    }
    if ((it.info.w & 4u) != 0u) {
        r.emitted = it.imp_emissive.xyz;
        if (it.imp.x != NONE) {
            let c = sample_tex(it.imp.x, it.imp.y, imported_uv(it, lerp_uv(tri, h.u, h.v), 4u));
            r.emitted *= srgb(c.xyz);
        }
    }
    // How much library detail shows: none where a face shades the skin.
    var detail_share = 1.0;
    if (!has_tex && it.face.x != NONE && m.a.w > 0.0 && m.a.y <= 0.0) {
        let local = vpos(v0) * w0 + vpos(v1) * h.u + vpos(v2) * h.v;
        let local_n = normalize_or(to_object_normal(it, n), vec3f(0.0, 0.0, 1.0));
        // Below the face (down the neck) the plain skin detail shows.
        let keep = smoothstep1(floats[it.face.x + 47u] + 0.012, floats[it.face.x + 47u] + 0.03, local.y);
        detail_share = 1.0 - keep;
        var nin = local_n;
        var cin = pow(r.s.albedo, vec3f(1.0 / 2.2));
        var rin = r.s.rough;
        if (keep < 1.0 && (m.c.x > 0.0 || m.c.y > 0.0)) {
            let sk = skin_detail(m, local, local_n, cin, rin);
            nin = normalize_or(mix(nin, sk.n, 1.0 - keep), nin);
            cin = mix(cin, sk.color, 1.0 - keep);
            rin = mix(rin, sk.rough, 1.0 - keep);
        }
        let fl = face_shade(it.face, local, nin, cin, rin);
        n = normalize_or(normal_of(it, fl.n), n);
        r.s.albedo = srgb(fl.color);
        r.s.rough = fl.rough;
        r.s.coat = max(r.s.coat, fl.coat);
        r.s.sheen += fl.sheen;
    } else if (!has_tex && m.a.w > 0.0 && (m.c.x > 0.0 || m.c.y > 0.0)) {
        let local = vpos(v0) * w0 + vpos(v1) * h.u + vpos(v2) * h.v;
        let local_n = normalize_or(to_object_normal(it, n), vec3f(0.0, 0.0, 1.0));
        let sk = skin_detail(m, local, local_n, pow(r.s.albedo, vec3f(1.0 / 2.2)), r.s.rough);
        n = normalize_or(normal_of(it, sk.n), n);
        r.s.albedo = srgb(sk.color);
        r.s.rough = sk.rough;
    }
    // Library detail (weave, grain, pores), triplanar in object space;
    // bakes already carry it in their textures.
    if (!has_tex && m.d.x >= 0.0) {
        let local = vpos(v0) * w0 + vpos(v1) * h.u + vpos(v2) * h.v;
        let local_n = normalize_or(to_object_normal(it, n), vec3f(0.0, 0.0, 1.0));
        let dt = detail_apply_with(u32(m.d.x), m.d.y, m.d.z * detail_share, m.d.w * detail_share, m.e.x, m.e.y, local, local_n, pow(r.s.albedo, vec3f(1.0 / 2.2)), r.s.rough, t * 0.001);
        n = normalize_or(normal_of(it, dt.n), n);
        r.s.albedo = srgb(dt.color);
        r.s.rough = dt.rough;
    }
    if (smart) {
        let local = vpos(v0) * w0 + vpos(v1) * h.u + vpos(v2) * h.v;
        let sm = normalize_or(vnormal(v0) * w0 + vnormal(v1) * h.u + vnormal(v2) * h.v, vec3f(0.0, 1.0, 0.0));
        let x0 = fv4(sv0 + 4u);
        let x1 = fv4(sv1 + 4u);
        let x2 = fv4(sv2 + 4u);
        let wear = x0.xyz * w0 + x1.xyz * h.u + x2.xyz * h.v;
        let painted = unpack4x8unorm(bitcast<u32>(x0.w)) * w0 + unpack4x8unorm(bitcast<u32>(x1.w)) * h.u + unpack4x8unorm(bitcast<u32>(x2.w)) * h.v;
        let at = SmartAt(local, sm, wear.x, wear.y, wear.z, painted, t * 0.001);
        let local_n = normalize_or(to_object_normal(it, n), vec3f(0.0, 0.0, 1.0));
        var look = SmartLook(pow(r.s.albedo, vec3f(1.0 / 2.2)), r.s.rough, r.s.metal, r.s.coat, r.s.sheen, m.a.x, local_n);
        look = smart_layers(bitcast<u32>(m.e.z), u32(m.e.w), at, look);
        if (it.smart.z > 0u) { look = smart_decals(it.smart.y, it.smart.z, at, look); }
        n = normalize_or(normal_of(it, look.n), n);
        r.s = Surface(srgb(look.color) * it.tint.xyz, look.rough, look.metal, look.coat, look.sheen, m.b.w * (1.0 - look.metal));
        r.emission = look.emission;
    }
    if (m.a.w > 0.0 && m.a.y <= 0.0) {
        let tint = scalp_at(p);
        if (tint.w > 0.0) {
            r.s.albedo = mix(r.s.albedo, tint.xyz, tint.w);
            r.s.rough = max(r.s.rough, 0.65 * tint.w);
            r.s.skin *= 1.0 - tint.w;
        }
    }
    r.ng = ng;
    r.n = n;
    return r;
}

// ---------------------------------------------------------------- BSDF

fn schlick(f0: vec3f, c: f32) -> vec3f { return f0 + (vec3f(1.0) - f0) * pow(1.0 - clamp(c, 0.0, 1.0), 5.0); }
fn alpha_of(rough: f32) -> f32 { let r = clamp(rough, 0.03, 1.0); return r * r; }

fn d_ggx(h: vec3f, a: f32) -> f32 {
    let a2 = a * a;
    let c = h.z * h.z * (a2 - 1.0) + 1.0;
    return a2 / (PI * c * c);
}

fn g1(v: vec3f, a: f32) -> f32 {
    let z2 = v.z * v.z;
    if (z2 <= 0.0) { return 0.0; }
    let tan2 = (1.0 - z2) / z2;
    return 2.0 / (1.0 + sqrt(1.0 + a * a * tan2));
}

fn sample_vndf(v: vec3f, a: f32, u: vec2f) -> vec3f {
    let vh = normalize(vec3f(a * v.x, a * v.y, v.z));
    let len2 = vh.x * vh.x + vh.y * vh.y;
    var t1 = vec3f(1.0, 0.0, 0.0);
    if (len2 > 0.0) { t1 = vec3f(-vh.y, vh.x, 0.0) / sqrt(len2); }
    let t2 = cross(vh, t1);
    let r = sqrt(u.x);
    let phi = TAU * u.y;
    let p1 = r * cos(phi);
    let s = 0.5 * (1.0 + vh.z);
    let p2 = (1.0 - s) * sqrt(max(1.0 - p1 * p1, 0.0)) + s * r * sin(phi);
    let nh = t1 * p1 + t2 * p2 + vh * sqrt(max(1.0 - p1 * p1 - p2 * p2, 0.0));
    return normalize(vec3f(a * nh.x, a * nh.y, max(nh.z, 1e-6)));
}

fn half_or_zero(v: vec3f, l: vec3f) -> vec3f {
    let h = v + l;
    let len = length(h);
    if (len > 1e-20) { return h / len; }
    return vec3f(0.0);
}

fn pdf_spec(v: vec3f, l: vec3f, a: f32) -> f32 {
    let h = half_or_zero(v, l);
    if (h.z <= 0.0 || v.z <= 0.0) { return 0.0; }
    return g1(v, a) * d_ggx(h, a) / (4.0 * v.z);
}

fn spec(v: vec3f, l: vec3f, a: f32, f0: vec3f) -> vec3f {
    let h = half_or_zero(v, l);
    if (l.z <= 0.0 || v.z <= 0.0 || h.z <= 0.0) { return vec3f(0.0); }
    return schlick(f0, dot(v, h)) * d_ggx(h, a) * g1(v, a) * g1(l, a) / (4.0 * v.z * l.z);
}

fn f0_of(s: Surface) -> vec3f { return mix(vec3f(0.04), s.albedo, s.metal); }

fn weights(s: Surface, v: vec3f) -> vec3f {
    let fs = max3(schlick(f0_of(s), v.z));
    let coat = s.coat * schlick(vec3f(0.04), v.z).x;
    let diffuse = (1.0 - s.metal) * max(max3(s.albedo), 0.05) * (1.0 - fs);
    let specular = max(fs, 0.04);
    let total = diffuse + specular + coat;
    return vec3f(diffuse, specular, coat) / total;
}

fn bsdf_eval(s: Surface, v: vec3f, l: vec3f) -> vec3f {
    if (l.z <= 0.0 || v.z <= 0.0) { return vec3f(0.0); }
    let a = alpha_of(s.rough);
    let coat_f = s.coat * schlick(vec3f(0.04), v.z).x;
    let fs = schlick(f0_of(s), v.z);
    let diffuse = s.albedo * (1.0 - s.metal) * FRAC_1_PI * (vec3f(1.0) - fs);
    let sheen = s.albedo * s.sheen * max(pow(1.0 - l.z, 4.0), pow(1.0 - v.z, 4.0)) * 0.5 * FRAC_1_PI;
    let broad = s.skin * 0.5;
    let reflection = spec(v, l, a, f0_of(s)) * (1.0 - broad) + spec(v, l, alpha_of(min(s.rough + 0.25, 1.0)), f0_of(s)) * broad;
    let base = (diffuse + sheen + reflection) * (1.0 - coat_f);
    let coat = spec(v, l, alpha_of(COAT_ROUGH), vec3f(0.04)) * s.coat;
    return (base + coat) * l.z;
}

fn bsdf_pdf(s: Surface, v: vec3f, l: vec3f) -> f32 {
    if (l.z <= 0.0 || v.z <= 0.0) { return 0.0; }
    let w = weights(s, v);
    return w.x * l.z * FRAC_1_PI
        + w.y * ((1.0 - s.skin * 0.5) * pdf_spec(v, l, alpha_of(s.rough)) + s.skin * 0.5 * pdf_spec(v, l, alpha_of(min(s.rough + 0.25, 1.0))))
        + w.z * pdf_spec(v, l, alpha_of(COAT_ROUGH));
}

// z <= 0 means no direction.
fn bsdf_sample(s: Surface, v: vec3f, u: vec2f, pick: f32) -> vec3f {
    if (v.z <= 0.0) { return vec3f(0.0); }
    let w = weights(s, v);
    var l: vec3f;
    if (pick < w.x) {
        l = cosine_dir(u);
    } else {
        var a: f32;
        if (pick < w.x + w.y) {
            let broad = pick < w.x + w.y * s.skin * 0.5;
            a = alpha_of(select(s.rough, min(s.rough + 0.25, 1.0), broad));
        } else {
            a = alpha_of(COAT_ROUGH);
        }
        let h = sample_vndf(v, a, u);
        l = normalize(2.0 * dot(v, h) * h - v);
    }
    return l;
}

// shade.rs: hair, hair_pdf, hair_sample (R, TT, TRT plus the light the
// lobes miss, given back in the fiber's color).

fn erf1(x0: f32) -> f32 {
    let s = sign(x0);
    let x = abs(x0);
    let t = 1.0 / (1.0 + 0.3275911 * x);
    let y = 1.0 - (((((1.0614054 * t - 1.453152) * t) + 1.4214138) * t - 0.28449674) * t + 0.2548296) * t * exp(-x * x);
    return s * y;
}

fn white_fiber(tv: f32) -> f32 {
    let x = (clamp(tv, -1.0, 1.0) + 1.0) * 0.5 * f32(WHITE_STEPS);
    let i = min(u32(floor(x)), WHITE_STEPS - 1u);
    return floats[i] + (floats[i + 1u] - floats[i]) * (x - f32(i));
}

fn fiber_albedo(color: vec3f) -> vec3f {
    let c = clamp(color, vec3f(0.0), vec3f(1.0));
    return clamp(vec3f(1.0) - pow((vec3f(1.0) - c) / (vec3f(1.0) + c), vec3f(1.7)), vec3f(0.001), vec3f(0.999));
}

fn part_shift(k: u32) -> f32 { return select(select(0.14, 0.035, k == 1u), -0.07, k == 0u); }
fn part_sigma(k: u32) -> f32 { return select(select(0.30, 0.22, k == 1u), 0.16, k == 0u); }
fn part_side(k: u32) -> f32 { return select(1.0, -1.0, k == 1u); }

struct FiberView { tv: f32, c0: vec3f, c1: vec3f, c2: vec3f }

fn fiber_view(t: vec3f, pigment: vec3f, v: vec3f) -> FiberView {
    let tv = clamp(dot(t, v), -1.0, 1.0);
    let sv = sqrt(max(1.0 - tv * tv, 0.0));
    let f = 0.0465 + (1.0 - 0.0465) * pow(1.0 - sv, 5.0);
    let absorption = -log(pigment) * 0.4;
    let x = tv / 1.55;
    let path = 1.0 / sqrt(max(1.0 - x * x, 0.1));
    let tr = exp(-absorption * path);
    return FiberView(tv, vec3f(f), tr * (1.0 - f) * (1.0 - f), tr * tr * (1.0 - f) * (1.0 - f) * f);
}

fn fv_color(fv: FiberView, k: u32) -> vec3f { return select(select(fv.c2, fv.c1, k == 1u), fv.c0, k == 0u); }
fn fv_mean(fv: FiberView, k: u32) -> f32 { return clamp(-fv.tv + part_shift(k), -0.95, 0.95); }
fn fv_mass(fv: FiberView, k: u32) -> f32 {
    let m = fv_mean(fv, k);
    let s = part_sigma(k) * 1.41421356;
    return 0.5 * (erf1((1.0 - m) / s) - erf1((-1.0 - m) / s));
}

fn hair_phi(t: vec3f, tv: f32, v: vec3f, l: vec3f) -> vec3f {
    // (cos phi, tl, sl)
    let tl = clamp(dot(t, l), -1.0, 1.0);
    let sl = sqrt(max(1.0 - tl * tl, 0.0));
    let sv = sqrt(max(1.0 - tv * tv, 0.0));
    let phi = clamp(dot(l - t * tl, v - t * tv) / max(sl * sv, 1e-5), -1.0, 1.0);
    return vec3f(phi, tl, sl);
}

fn hair_bsdf(t: vec3f, color: vec3f, v: vec3f, l: vec3f) -> vec3f {
    let pigment = fiber_albedo(color);
    let fv = fiber_view(t, pigment, v);
    let a = hair_phi(t, fv.tv, v, l);
    var lobes = vec3f(0.0);
    for (var k = 0u; k < 3u; k++) {
        let sigma = part_sigma(k);
        let x = (a.y - fv_mean(fv, k)) / sigma;
        let lon = exp(-0.5 * x * x) / (sigma * sqrt(TAU));
        lobes += fv_color(fv, k) * lon * (1.0 + part_side(k) * a.x) / TAU;
    }
    let missing = max(1.0 - HAIR_LOBES * white_fiber(fv.tv), 0.0);
    return lobes * HAIR_LOBES + pow(pigment, vec3f(HAIR_PIGMENT)) * missing * a.z / (PI * PI);
}

fn hair_weights(fv: FiberView, pigment: vec3f) -> vec4f {
    let missing = max(1.0 - HAIR_LOBES * white_fiber(fv.tv), 0.0);
    var w = vec4f(0.0);
    for (var k = 0u; k < 3u; k++) { w[k] = HAIR_LOBES * max3(fv_color(fv, k)) * fv_mass(fv, k); }
    w.w = max3(pow(pigment, vec3f(HAIR_PIGMENT))) * missing;
    return w / max(w.x + w.y + w.z + w.w, 1e-8);
}

fn hair_pdf(t: vec3f, color: vec3f, v: vec3f, l: vec3f) -> f32 {
    let pigment = fiber_albedo(color);
    let fv = fiber_view(t, pigment, v);
    let w = hair_weights(fv, pigment);
    let a = hair_phi(t, fv.tv, v, l);
    var pdf = w.w * a.z / (PI * PI);
    for (var k = 0u; k < 3u; k++) {
        let sigma = part_sigma(k);
        let x = (a.y - fv_mean(fv, k)) / sigma;
        let g = exp(-0.5 * x * x) / (sigma * sqrt(TAU));
        pdf += w[k] * g / max(fv_mass(fv, k), 1e-6) * (1.0 + part_side(k) * a.x) / TAU;
    }
    return pdf;
}

// (direction, pdf); pdf 0 = none.
fn hair_sample(t: vec3f, color: vec3f, v: vec3f) -> vec4f {
    let pigment = fiber_albedo(color);
    let fv = fiber_view(t, pigment, v);
    let w = hair_weights(fv, pigment);
    let a = normalize_or(v - t * fv.tv, ortho(t));
    let b = cross(t, a);
    let pick = rnd();
    var k = 3u;
    if (pick < w.x) { k = 0u; } else if (pick < w.x + w.y) { k = 1u; } else if (pick < w.x + w.y + w.z) { k = 2u; }
    var tl = 0.0;
    var angle = 0.0;
    if (k == 3u) {
        for (var i = 0; i < 32; i++) {
            tl = 1.0 - 2.0 * rnd();
            if (rnd() <= sqrt(1.0 - tl * tl)) { break; }
        }
        angle = TAU * rnd();
    } else {
        let sigma = part_sigma(k);
        let side = part_side(k);
        tl = fv_mean(fv, k);
        for (var i = 0; i < 16; i++) {
            let u1 = max(rnd(), 1e-7);
            let u2 = rnd();
            let g = sqrt(-2.0 * log(u1)) * cos(TAU * u2);
            let x = fv_mean(fv, k) + g * sigma;
            if (x >= -1.0 && x <= 1.0) { tl = x; break; }
        }
        for (var i = 0; i < 32; i++) {
            angle = TAU * rnd() - PI;
            if (rnd() * 2.0 <= 1.0 + side * cos(angle)) { break; }
        }
    }
    let sl = sqrt(max(1.0 - tl * tl, 0.0));
    let l = normalize_or(t * tl + (a * cos(angle) + b * sin(angle)) * sl, t);
    let pdf = hair_pdf(t, color, v, l);
    if (pdf > 1e-8) { return vec4f(l, pdf); }
    return vec4f(0.0);
}

// ---------------------------------------------------------------- lights

struct LightSample { l: vec3f, power: vec3f, far: f32 }

fn light_sample(i: u32, p: vec3f, u: vec2f) -> LightSample {
    let li = lights[i];
    if (li.kind.x == 0u) {
        return LightSample(cone_dir(li.a.xyz, li.a.w, u), li.power.xyz, 3.4e38);
    }
    if (li.kind.x == 1u) {
        let delta = li.a.xyz - p;
        let d2 = max(dot(delta, delta), 1e-8);
        return LightSample(normalize_or(delta, vec3f(0.0, 1.0, 0.0)), li.power.xyz / d2, length(delta));
    }
    if (li.kind.x == 3u) {
        let delta = li.a.xyz - p;
        let d2 = dot(delta, delta);
        let dist = sqrt(d2);
        let radius = li.a.w;
        if (dist <= radius * 1.0001) {
            return LightSample(normalize_or(delta, vec3f(0.0, 1.0, 0.0)), li.power.xyz / (radius * radius), dist);
        }
        let axis = delta / dist;
        let sin2 = min(radius * radius / d2, 1.0);
        let cos_max = sqrt(1.0 - sin2);
        let l = cone_dir(axis, acos(cos_max), u);
        let along = dot(l, delta);
        let far = along - sqrt(max(radius * radius - (d2 - along * along), 0.0));
        let solid = TAU * sin2 / (1.0 + cos_max);
        return LightSample(l, li.power.xyz * (solid / (PI * radius * radius)), max(far, 0.0));
    }
    let f = frame(li.b.xyz);
    let r = li.a.w * sqrt(u.x);
    let phi = TAU * u.y;
    let q = li.a.xyz + f.t * (r * cos(phi)) + f.b * (r * sin(phi));
    let delta = q - p;
    let l = normalize_or(delta, vec3f(0.0, 1.0, 0.0));
    let power = li.power.xyz * (PI * li.a.w * li.a.w * max(dot(-l, li.b.xyz), 0.0) / max(dot(delta, delta), 1e-8));
    return LightSample(l, power, length(delta));
}

// Local lights (point, area, sphere) beyond four are picked by how much
// they could give a point (`LightSet::pick` in lighting.rs): two picks,
// each weighted by one over its chance. Suns are always sampled.
const ALL_LOCAL: f32 = 4.0;

fn light_importance(i: u32, p: vec3f, n: vec3f, use_n: bool) -> f32 {
    let li = lights[i];
    if (li.kind.x == 0u) { return 0.0; }
    var r = 1e-3;
    if (li.kind.x >= 2u) { r = li.a.w; }
    let delta = li.a.xyz - p;
    if (use_n && dot(delta, n) < -r) { return 0.0; }
    return li.power.w / max(max(dot(delta, delta), r * r), 1e-6);
}

// Light slots to visit: 0..count are the lights themselves (suns always,
// local ones only when few), count and count + 1 are picks.
struct Picks { i0: u32, w0: f32, i1: u32, w1: f32 }

fn few_lights() -> bool { return P.light_info.y <= ALL_LOCAL; }

fn pick_lights(p: vec3f, n: vec3f, use_n: bool) -> Picks {
    var out = Picks(0u, 0.0, 0u, 0.0);
    if (few_lights()) { return out; }
    var total = 0.0;
    for (var i = 0u; i < P.opts.w; i++) { total += light_importance(i, p, n, use_n); }
    if (total <= 0.0) { return out; }
    let x0 = rnd() * total;
    let x1 = rnd() * total;
    var acc = 0.0;
    var c0 = NONE;
    var c1 = NONE;
    var last = NONE;
    for (var i = 0u; i < P.opts.w; i++) {
        let w = light_importance(i, p, n, use_n);
        if (w > 0.0) {
            if (c0 == NONE && x0 < acc + w) { c0 = i; }
            if (c1 == NONE && x1 < acc + w) { c1 = i; }
            last = i;
        }
        acc += w;
    }
    if (c0 == NONE) { c0 = last; }
    if (c1 == NONE) { c1 = last; }
    if (c0 != NONE) { out.i0 = c0; out.w0 = total / (light_importance(c0, p, n, use_n) * 2.0); }
    if (c1 != NONE) { out.i1 = c1; out.w1 = total / (light_importance(c1, p, n, use_n) * 2.0); }
    return out;
}

// Slot j: which light and its weight (weight 0: skip).
fn light_slot(j: u32, pk: Picks) -> vec2f {
    if (j < P.opts.w) {
        if (lights[j].kind.x == 0u || few_lights()) { return vec2f(bitcast<f32>(j), 1.0); }
        return vec2f(0.0, 0.0);
    }
    if (j == P.opts.w) { return vec2f(bitcast<f32>(pk.i0), pk.w0); }
    return vec2f(bitcast<f32>(pk.i1), pk.w1);
}

// ---------------------------------------------------------------- fog

fn fog_on() -> bool { return P.fog.x > 0.0; }

fn fog_density_at(y: f32) -> f32 {
    if (P.fog.z <= 0.0 || y <= P.fog.y) { return P.fog.x; }
    return P.fog.x * exp(-(y - P.fog.y) / P.fog.z);
}

// Optical depth from o along d for s meters (`Fog::depth`).
fn fog_hash(v: u32) -> f32 {
    var x = v; x ^= x >> 16u; x *= 0x7feb352du; x ^= x >> 15u; x *= 0x846ca68bu; x ^= x >> 16u;
    return f32(x & 0xffffffu)/16777216.0;
}
fn fog_density_point(p: vec3f) -> f32 {
    if (P.fog_noise.x == 0.0) { return fog_density_at(p.y); }
    let q = p*P.fog_noise.y; let ip = vec3i(floor(q)); let f0 = fract(q); let f = f0*f0*(vec3f(3.0)-2.0*f0);
    var n = 0.0;
    for (var z=0i; z<2i; z++) { for (var y=0i; y<2i; y++) { for (var x=0i; x<2i; x++) {
        let k = bitcast<u32>(ip.x+x)*73856093u ^ bitcast<u32>(ip.y+y)*19349663u ^ bitcast<u32>(ip.z+z)*83492791u ^ u32(P.fog_noise.z);
        n += fog_hash(k)*select(1.0-f.x,f.x,x==1i)*select(1.0-f.y,f.y,y==1i)*select(1.0-f.z,f.z,z==1i);
    } } }
    return fog_density_at(p.y)*(1.0+P.fog_noise.x*(2.0*n-1.0));
}
fn fog_depth(o: vec3f, d: vec3f, s0: f32) -> f32 {
    let s = max(min(s0, P.fog_color.w), 0.0);
    if (P.fog_noise.x > 0.0) {
        let steps = u32(clamp(ceil(s*P.fog_noise.y*4.0),24.0,96.0));
        var tau=0.0;
        for (var i=0u; i<steps; i++) { tau += fog_density_point(o+d*(s*(f32(i)+0.5)/f32(steps))); }
        return tau*s/f32(steps);
    }
    let density = P.fog.x;
    let h = P.fog.y;
    let fall = P.fog.z;
    if (fall <= 0.0) { return density * s; }
    let y0 = o.y;
    if (abs(d.y) < 1e-5) { return fog_density_at(y0) * s; }
    let cross = clamp((h - y0) / d.y, 0.0, s);
    var below = vec2f(0.0, 0.0);
    var above = vec2f(0.0, 0.0);
    if ((y0 <= h) == (d.y > 0.0)) {
        if (y0 <= h) { below = vec2f(0.0, cross); above = vec2f(cross, s); } else { below = vec2f(cross, s); above = vec2f(0.0, cross); }
    } else if (y0 <= h) {
        below = vec2f(0.0, s);
    } else {
        above = vec2f(0.0, s);
    }
    var tau = density * (below.y - below.x);
    if (above.y > above.x) {
        let ea = exp(-max(y0 + d.y * above.x - h, 0.0) / fall);
        let eb = exp(-max(y0 + d.y * above.y - h, 0.0) / fall);
        tau += density * fall / d.y * (ea - eb);
    }
    return max(tau, 0.0);
}

fn fog_trans(o: vec3f, d: vec3f, s: f32) -> f32 {
    if (!fog_on()) { return 1.0; }
    return exp(-fog_depth(o, d, s));
}

fn fog_phase(c: f32) -> f32 {
    let g = P.fog.w;
    return (1.0 - g * g) / (4.0 * PI * pow(max(1.0 + g * g - 2.0 * g * c, 1e-6), 1.5));
}

fn luminance(c: vec3f) -> f32 { return dot(c, vec3f(0.2126, 0.7152, 0.0722)); }

// How much light i could give the segment o + d [0, tmax] (`inscatter`).
fn fog_weight(i: u32, o: vec3f, d: vec3f, tmax: f32) -> f32 {
    let li = lights[i];
    if (li.kind.x == 0u) { return luminance(li.power.xyz); }
    var r = 1e-3;
    if (li.kind.x >= 2u) { r = li.a.w; }
    let along = clamp(dot(li.a.xyz - o, d), 0.0, tmax);
    let q = o + d * along - li.a.xyz;
    return li.power.w / max(max(dot(q, q), r * r), 1e-4);
}

// Light scattered toward the camera by fog along o + d [0, t]: single
// scattering from one light or the sky (`inscatter` in mod.rs).
fn inscatter(o: vec3f, d: vec3f, t: f32) -> vec3f {
    let tmax = min(t, P.fog_color.w);
    if (tmax <= 1e-5) { return vec3f(0.0); }
    let env = P.light_info.x;
    var total = env;
    for (var i = 0u; i < P.opts.w; i++) { total += fog_weight(i, o, d, tmax); }
    if (total <= 0.0) { return vec3f(0.0); }
    var x = rnd() * total;
    var pick = NONE;
    var pw = env;
    for (var i = 0u; i < P.opts.w; i++) {
        let w = fog_weight(i, o, d, tmax);
        if (x < w) { pick = i; pw = w; break; }
        x -= w;
    }
    let chance = pw / total;
    if (chance <= 0.0) { return vec3f(0.0); }
    var s = rnd() * tmax;
    var pdf = 1.0 / tmax;
    if (pick != NONE && lights[pick].kind.x != 0u) {
        let c = lights[pick].a.xyz;
        let delta = dot(c - o, d);
        let h = sqrt(max(dot(c - o, c - o) - delta * delta, 1e-8));
        let a0 = atan(-delta / h);
        let a1 = atan((tmax - delta) / h);
        let a = a0 + (a1 - a0) * rnd();
        s = clamp(delta + h * tan(a), 0.0, tmax);
        pdf = h / (max(a1 - a0, 1e-9) * (h * h + (s - delta) * (s - delta)));
    }
    let xp = o + d * s;
    let scatter = P.fog_color.xyz * (fog_density_point(xp) * fog_trans(o, d, s) * fire_trans(o, d, s));
    var l: vec3f;
    var light: vec3f;
    var far = 3.4e38;
    var light_pdf = 1.0;
    if (pick != NONE) {
        let ls = light_sample(pick, xp, rnd2());
        l = ls.l;
        light = ls.power;
        far = ls.far;
    } else {
        let es = env_sample();
        l = es.l;
        light = es.c;
        light_pdf = es.pdf;
    }
    if (light_pdf <= 0.0 || all(light == vec3f(0.0)) || blocked(xp, l, far)) { return vec3f(0.0); }
    return scatter * light * (fog_phase(dot(l, d)) * light_trans(xp, l, far) / (light_pdf * pdf * chance));
}

// ---------------------------------------------------------------- environment

fn env_texel(x: i32, y: u32) -> vec3f {
    let w = i32(P.env_size.x);
    let xx = ((x % w) + w) % w;
    return env_pixels[y * P.env_size.x + u32(xx)].xyz;
}

fn env_at(d: vec3f) -> vec3f {
    let w = f32(P.env_size.x);
    let hh = f32(P.env_size.y);
    // atan2(0,0) is undefined on Metal; choose the CPU pole azimuth.
    var azimuth = 0.0;
    if (d.x != 0.0 || d.z != 0.0) { azimuth = atan2(d.z, d.x); }
    let a = (azimuth - P.env.y) / TAU;
    let u = a - floor(a);
    let v = atan2(length(vec2f(d.x, d.z)), d.y) / PI;
    let x = u * w - 0.5;
    let y = clamp(v * hh - 0.5, 0.0, hh - 1.0);
    let x0 = i32(floor(x));
    let y0 = u32(floor(y));
    let y1 = min(y0 + 1u, P.env_size.y - 1u);
    let fx = x - floor(x);
    let top = mix(env_texel(x0, y0), env_texel(x0 + 1, y0), fx);
    let bottom = mix(env_texel(x0, y1), env_texel(x0 + 1, y1), fx);
    var c = mix(top, bottom, y - floor(y));
    if (P.env_size.z != 0u && dot(d, P.sun_dir.xyz) >= cos(P.sun_dir.w)) { c += P.sun_power.xyz; }
    return c;
}

fn row_solid(y: u32) -> f32 {
    let hh = f32(P.env_size.y);
    return TAU / f32(P.env_size.x) * (cos(PI * f32(y) / hh) - cos(PI * f32(y + 1u) / hh));
}

fn env_pdf(d: vec3f) -> f32 {
    let w = P.env_size.x;
    let hh = P.env_size.y;
    let a = (atan2(d.z, d.x) - P.env.y) / TAU;
    let u = a - floor(a);
    let v = atan2(length(vec2f(d.x, d.z)), d.y) / PI;
    let x = u32(u * f32(w)) % w;
    let y = min(u32(v * f32(hh)), hh - 1u);
    let i = y * w + x;
    var map = 1.0 / (4.0 * PI);
    if (P.env.z > 0.0) { map = (env_cdf[i + 1u] - env_cdf[i]) / row_solid(y); }
    var sun = 0.0;
    if (P.env_size.z != 0u && dot(d, P.sun_dir.xyz) >= cos(P.sun_dir.w)) { sun = 1.0 / (TAU * (1.0 - cos(P.sun_dir.w))); }
    return (1.0 - P.env.w) * map + P.env.w * sun;
}

fn env_direction(u: f32, c: f32) -> vec3f {
    let phi = TAU * u;
    let r = sqrt(max(1.0 - c * c, 0.0));
    return vec3f(r * cos(phi), c, r * sin(phi));
}

struct EnvSample { l: vec3f, c: vec3f, pdf: f32 }

fn env_sample() -> EnvSample {
    var d: vec3f;
    if (rnd() < P.env.w) {
        d = cone_dir(P.sun_dir.xyz, P.sun_dir.w, rnd2());
    } else if (P.env.z <= 0.0) {
        d = sphere_dir(rnd2());
    } else {
        let n = P.env_size.x * P.env_size.y;
        let pick = rnd();
        // Last i with cdf[i] <= pick.
        var lo = 0u;
        var hi = n;
        while (lo < hi) {
            let mid = (lo + hi) / 2u;
            if (env_cdf[mid + 1u] <= pick) { lo = mid + 1u; } else { hi = mid; }
        }
        let i = min(lo, n - 1u);
        let x = i % P.env_size.x;
        let y = i / P.env_size.x;
        let u = (f32(x) + rnd()) / f32(P.env_size.x) + P.env.y / TAU;
        let hh = f32(P.env_size.y);
        let a = cos(PI * f32(y) / hh);
        let b = cos(PI * f32(y + 1u) / hh);
        d = env_direction(u, a + (b - a) * rnd());
    }
    return EnvSample(d, env_at(d), env_pdf(d));
}

// The plain studio backdrop behind the subject.
fn studio(d: vec3f) -> vec3f {
    let t = smoothstep1(-0.3, 0.6, d.y);
    return mix(vec3f(0.06, 0.055, 0.05), vec3f(0.32, 0.34, 0.38), t);
}

// ---------------------------------------------------------------- subsurface

struct Probe { weight: vec3f, found: bool, p: vec3f, ng: vec3f, n: vec3f }

// THIN and THROUGH in mod.rs.
const THIN: f32 = 6.0;
const THROUGH: f32 = 0.3;

fn probe(p: vec3f, n: vec3f, radius: vec3f, item: u32) -> Probe {
    let d = max(radius, vec3f(1e-5));
    let inward = walk(false, p, -n, 1e-4, THIN * max3(d), SKIP_OTHER_ITEMS, item, false);
    var through = vec3f(0.0);
    if (inward.found) { through = exp(-vec3f(inward.t) / d); }
    var chance = 0.0;
    if (max3(through) > 1e-3) { chance = THROUGH; }
    if (inward.found && rnd() < chance) {
        let s = surface(p, -n, inward.t, inward);
        return Probe(through / chance, true, s.p, -s.ng, -s.n);
    }
    let near = (vec3f(1.0) - through) / (1.0 - chance);
    let c = min(u32(rnd() * 3.0), 2u);
    let u = max(rnd(), 1e-7);
    var r: f32;
    if (rnd() < 0.25) { r = -d[c] * log(u); } else { r = -3.0 * d[c] * log(u); }
    let rmax = 12.0 * max3(d);
    r = min(r, rmax);
    let rad = 0.25 * exp(-r / d) / d + 0.75 * exp(-r / (3.0 * d)) / (3.0 * d);
    let pdf = (rad.x + rad.y + rad.z) / 3.0;
    var weight = vec3f(1.0);
    if (pdf > 0.0) { weight = rad / pdf; }
    weight *= near;
    let f = frame(n);
    let phi = TAU * rnd();
    let o = p + (f.t * cos(phi) + f.b * sin(phi)) * r + n * rmax;
    let h = walk(false, o, -n, 0.0, 2.0 * rmax, SKIP_OTHER_ITEMS, item, false);
    if (!h.found) { return Probe(weight, false, p, n, n); }
    let s = surface(o, -n, h.t, h);
    return Probe(weight, true, s.p, s.ng, s.n);
}

// ---------------------------------------------------------------- paths

// `INDIRECT_CLAMP` in mod.rs: light picked up after the first bounce is
// clamped per sample (brightest channel).
const INDIRECT_CLAMP: f32 = 10.0;

fn clampi(c: vec3f, bounce: u32) -> vec3f {
    if (bounce == 0u) { return c; }
    return c * (INDIRECT_CLAMP / max(max3(c), INDIRECT_CLAMP));
}

struct Result { color: vec3f, albedo: vec3f, normal: vec3f, depth: f32 }


// fire.rs packs blackbody emission and smoke coefficients once, identically
// for both tracers. Positions and directions stay in world distance units.
struct FireCell { e: vec4f, scatter: vec3f }
struct FireSample { e: vec4f, scatter: vec4f }
fn fire_interval(i: u32, o0: vec3f, d0: vec3f, far: f32) -> vec2f {
    let k = 1u + i * 8u;
    let inv = mat4x4f(fire_data[k],fire_data[k+1u],fire_data[k+2u],fire_data[k+3u]);
    let o = (inv * vec4f(o0,1.0)).xyz;
    let d = (inv * vec4f(d0,0.0)).xyz;
    let lo = fire_data[k+4u].xyz;
    let hi = lo + fire_data[k+5u].xyz;
    var a = 0.0; var b = far;
    for (var axis=0u;axis<3u;axis++) {
        if (abs(d[axis])<1e-9) { if(o[axis]<lo[axis] || o[axis]>hi[axis]) {return vec2f(0.0);} }
        else { let t0=(lo[axis]-o[axis])/d[axis];let t1=(hi[axis]-o[axis])/d[axis];a=max(a,min(t0,t1));b=min(b,max(t0,t1)); }
    }
    return vec2f(a,b);
}
fn fire_lookup(i: u32, p: vec3f) -> FireSample {
    let k=1u+i*8u;let dims=vec3u(fire_data[k+6u].xyz);
    let inv=mat4x4f(fire_data[k],fire_data[k+1u],fire_data[k+2u],fire_data[k+3u]);
    let q=((inv*vec4f(p,1.0)).xyz-fire_data[k+4u].xyz)/fire_data[k+5u].xyz*vec3f(dims)-vec3f(0.5);
    var out=FireSample(vec4f(0.0),vec4f(0.0));
    if(any(q<vec3f(0.0)) || any(q>vec3f(dims)-vec3f(1.0))) {return out;}
    let base=vec3u(floor(q));let f=fract(q);let offset=bitcast<u32>(fire_data[k+6u].w);
    for(var z=0u;z<2u;z++){for(var y=0u;y<2u;y++){for(var x=0u;x<2u;x++){
        let at=min(base+vec3u(x,y,z),dims-vec3u(1u));
        let w=select(1.0-f.x,f.x,x==1u)*select(1.0-f.y,f.y,y==1u)*select(1.0-f.z,f.z,z==1u);
        let idx=offset+((at.z*dims.y+at.y)*dims.x+at.x)*2u;
        out.e+=fire_data[idx]*w;out.scatter+=fire_data[idx+1u]*w;
    }}}
    return out;
}
fn fire_curl(p:vec3f,t:f32)->vec3f {
    let q=p+vec3f(t,-t*1.3,t*0.7);
    return vec3f(cos(q.y)*sin(q.x)-cos(q.z)*sin(q.x),
                 cos(q.z)*sin(q.y)-cos(q.x)*sin(q.y),
                 cos(q.x)*sin(q.z)-cos(q.y)*sin(q.z));
}
fn fire_at(i:u32,p:vec3f)->FireCell {
    let k=1u+i*8u;
    let inv=mat4x4f(fire_data[k],fire_data[k+1u],fire_data[k+2u],fire_data[k+3u]);
    let detail=fire_data[k+7u];
    let local=(inv*vec4f(p,1.0)).xyz;
    let coarse=fire_lookup(i,p);
    let energy=min((coarse.e.x+coarse.e.w)*detail.x,1.0);
    let r=local/detail.x;
    let phase=detail.y*3.0+detail.w;
    let strength=select(0.03+0.32*clamp(max(r.y,0.0)/2.0,0.0,1.0),0.015,detail.z>0.5);
    let displacement=(fire_curl(r*3.0,phase)+fire_curl(r*7.0,phase*1.7)*0.35)*(detail.x*strength*energy);
    // Convert local displacement back to world before the common lookup.
    let world_delta=inverse_fire_delta(inv,displacement);
    let sample=fire_lookup(i,p+world_delta);
    let band=max(1.0-abs((sample.scatter.w-0.35)/0.5),0.0);
    let base=max(1.0-max(r.y,0.0)*1.8,0.0);
    let core=clamp(1.0-max(r.y,0.0)/2.5,0.0,1.0);
    let sheet=max(band*band*2.5,core*2.5);
    var e=sample.e.xyz*sheet;
    if(detail.z>0.5){let blue=base*(1.0-base)*4.0;let energy=e.x;e.x*=1.0-blue*0.85;e.y*=1.0-blue*0.6;e.z+=energy*blue*0.85;}
    return FireCell(vec4f(e,sample.e.w),sample.scatter.xyz);
}
fn inverse_fire_delta(m:mat4x4f,d:vec3f)->vec3f {
    let a=m[0].xyz;let b=m[1].xyz;let c=m[2].xyz;
    let det=dot(a,cross(b,c));
    return vec3f(dot(d,cross(b,c)),dot(d,cross(c,a)),dot(d,cross(a,b)))/det;
}
fn fire_trans(o:vec3f,d:vec3f,far:f32)->f32 {
    var tau=0.0;
    for(var i=0u;i<u32(fire_data[0].x);i++) {
        let seg=fire_interval(i,o,d,far);if(seg.y<=seg.x){continue;}
        let ds=(seg.y-seg.x)/16.0;
        for(var j=0u;j<16u;j++){tau+=fire_at(i,o+d*(seg.x+(f32(j)+0.5)*ds)).e.w*ds;}
    }
    return exp(-tau);
}
fn fire_phase(c:f32)->f32 {let g=0.25;return (1.0-g*g)/(4.0*PI*pow(max(1.0+g*g-2.0*g*c,1e-6),1.5));}
fn light_trans(o:vec3f,d:vec3f,far:f32)->f32 {return fog_trans(o,d,far)*fire_trans(o,d,far);}
fn fire_march(o:vec3f,d:vec3f,far:f32)->vec4f {
    var cursor=0.0;var e=vec3f(0.0);var tr=1.0;let volumes=u32(fire_data[0].x);
    for(var segment=0u;segment<volumes;segment++) {
        var start=3.4e38;var end=0.0;var h=0.0;
        for(var i=0u;i<volumes;i++) {let seg=fire_interval(i,o,d,far);let a=max(seg.x,cursor);if(seg.y>a && a<start){start=a;end=seg.y;h=fire_data[1u+i*8u+4u].w;}}
        if(end<=start){break;}
        let count=u32(clamp(ceil((end-start)/max(h,1e-6)),1.0,512.0));let ds=(end-start)/f32(count);
        var incident=vec3f(0.0);var shaded=false;
        for(var j=0u;j<count;j++) {
            let p=o+d*(start+(f32(j)+0.5)*ds);var c=FireCell(vec4f(0.0),vec3f(0.0));
            for(var i=0u;i<volumes;i++){let a=fire_at(i,p);c.e+=a.e;c.scatter+=a.scatter;}
            let t=exp(-c.e.w*ds);var integ=ds;if(c.e.w>1e-6){integ=(1.0-t)/c.e.w;}
            if(any(c.scatter>vec3f(0.0)) && (!shaded || j%4u==0u)) {
                incident=env_at(vec3f(0.0,1.0,0.0))/(4.0*PI);shaded=true;
                let pk=pick_lights(p,vec3f(0.0),false);
                for(var li=0u;li<P.opts.w+2u;li++) {
                    let slot=light_slot(li,pk);if(slot.y<=0.0){continue;}
                    let ls=light_sample(bitcast<u32>(slot.x),p,vec2f(0.5));
                    if(!blocked(p,ls.l,ls.far)){incident+=ls.power*(slot.y*fire_phase(dot(ls.l,d))*light_trans(p,ls.l,ls.far));}
                }
            }
            e+=(c.e.xyz+c.scatter*incident)*(tr*integ*fog_trans(o,d,start+(f32(j)+0.5)*ds));tr*=t;
        }
        cursor=end;
    }
    return vec4f(e,tr);
}

fn radiance(o0: vec3f, d0: vec3f) -> Result {
    var o = o0;
    var d = d0;
    let bounces = P.opts.x;
    var color = vec3f(0.0);
    var through = vec3f(1.0);
    var res = Result(vec3f(0.0), vec3f(1.0), vec3f(0.0), 1e9);
    var hops = 0u;
    var hair_hops = 0u;
    var previous_pdf = -1.0;
    for (var step = 0u; step <= bounces + MAX_GLASS_HOPS + MAX_HAIR_HOPS; step++) {
        let bounce = step - hops;
        if (bounce > bounces) { break; }
        let hit = trace(o, d, previous_pdf < 0.0);
        let before_medium = through;
        let medium = fire_march(o, d, select(3.4e38, hit.t, hit.found));
        color += clampi(through * medium.xyz, bounce);
        through *= medium.w;
        if (fog_on()) {
            let tt = select(3.4e38, hit.t, hit.found);
            if (bounce == 0u) { color += before_medium * inscatter(o, d, tt); }
            through *= fog_trans(o, d, tt);
        }
        if (!hit.found) {
            let backdrop = step == 0u && P.opts.z == 0u;
            var c = env_at(d);
            if (backdrop) { c = studio(d); }
            var weight = 1.0;
            if (previous_pdf >= 0.0) { weight = mis(previous_pdf, env_pdf(d)); }
            color += through * c * weight;
            if (bounce == 0u) {
                res.albedo = c;
                if (fog_on()) { res.albedo=vec3f(1.0); res.normal=-d; res.depth=P.fog_color.w; }
            }
            break;
        }
        let t = hit.t;
        let is_floor = hit.tri == NONE;
        // Strands and textured hair cards share the fiber scattering.
        var hair = false;
        var tangent = vec3f(0.0);
        var pigment = vec3f(0.0);
        if (!is_floor && (hit.tri & HAIR_BIT) != 0u) {
            let seg = (hit.tri & ~HAIR_BIT) / 2u;
            hair = true;
            tangent = hair_segs[seg * 2u].xyz;
            pigment = hair_segs[seg * 2u + 1u].xyz;
        }
        var sh: SurfHit;
        if (!is_floor && !hair) {
            sh = surface(o, d, t, hit);
            if (sh.has_hair) { hair = true; tangent = sh.hair_dir; pigment = sh.s.albedo; }
        }
        if (hair) {
            let p = o + d * t;
            if (bounce == 0u) {
                res.albedo = max(pigment, vec3f(0.02));
                res.normal = select(tangent, -tangent, tangent.y < 0.0);
                res.depth = t;
            }
            let v = -d;
            let lift = 2e-4 * (1.0 + max3(abs(p)));
            let pk = pick_lights(p, vec3f(0.0, 1.0, 0.0), false);
            for (var j = 0u; j < P.opts.w + 2u; j++) {
                let slot = light_slot(j, pk);
                if (slot.y <= 0.0) { continue; }
                let ls = light_sample(bitcast<u32>(slot.x), p, rnd2());
                // Leave along the light, clear of this fiber's ribbon.
                let start = p + ls.l * lift;
                if (blocked(start, ls.l, ls.far - lift)) { continue; }
                color += clampi(through * hair_bsdf(tangent, pigment, v, ls.l) * ls.power * (slot.y * light_trans(p, ls.l, ls.far)), bounce);
            }
            let es = env_sample();
            let start = p + es.l * lift;
            if (es.pdf > 0.0 && !blocked(start, es.l, 3.4e38)) {
                var weight = 1.0;
                if (bounce != bounces) { weight = mis(es.pdf, hair_pdf(tangent, pigment, v, es.l)); }
                color += clampi(through * hair_bsdf(tangent, pigment, v, es.l) * es.c * (weight / es.pdf * light_trans(p, es.l, 3.4e38)), bounce);
            }
            if (bounce == bounces) { break; }
            // Follow the fiber's lobes so paths deep in a groom keep even weights.
            let hs = hair_sample(tangent, pigment, v);
            if (hs.w <= 0.0) { break; }
            let l = hs.xyz;
            through *= hair_bsdf(tangent, pigment, v, l) / hs.w;
            if (bounce >= 2u) {
                let keep = clamp(max3(through), 0.05, 0.95);
                if (rnd() > keep) { break; }
                through /= keep;
            }
            o = p + l * lift;
            d = l;
            previous_pdf = hs.w;
            if (bounce > 0u && hair_hops < MAX_HAIR_HOPS) {
                hair_hops++;
                hops++;
            }
            continue;
        }
        if (is_floor) {
            sh.p = o + d * t;
            sh.ng = vec3f(0.0, 1.0, 0.0);
            sh.n = vec3f(0.0, 1.0, 0.0);
            sh.s = Surface(vec3f(0.42), 0.7, 0.0, 0.0, 0.0, 0.0);
            sh.has_sss = false;
            sh.emission = 0.0;
            sh.emitted = vec3f(0.0);
            sh.transmission = 0.0;
            sh.ior = 1.0;
            sh.front = true;
            sh.flame = false;
        }
        let p = sh.p;
        let ng = sh.ng;
        let n = sh.n;
        var surf = sh.s;
        if (bounce == 0u) {
            res.albedo = max(surf.albedo, vec3f(0.02));
            res.normal = n;
            res.depth = t;
        }
        color += clampi(through * sh.emitted, bounce);
        if (sh.emission > 0.0) { color += clampi(through * surf.albedo * sh.emission, bounce); }
        // A flame is all glow: its light reaches the scene as its object's light.
        if (sh.flame) { break; }
        if (sh.transmission > 0.0 && rnd() < sh.transmission) {
            hops++;
            previous_pdf = -1.0;
            let eta = select(sh.ior, 1.0 / sh.ior, sh.front);
            let m = normalize_or(n + sphere_dir(rnd2()) * surf.rough * 0.5, n);
            let cosi = clamp(dot(-d, m), 1e-4, 1.0);
            let sin2t = eta * eta * (1.0 - cosi * cosi);
            let lift = ng * (1e-4 + 1e-4 * max3(abs(p)));
            var fresnel = 1.0;
            if (sin2t < 1.0) {
                let cost = sqrt(1.0 - sin2t);
                let rs = (eta * cosi - cost) / (eta * cosi + cost);
                let rp = (cosi - eta * cost) / (cosi + eta * cost);
                fresnel = 0.5 * (rs * rs + rp * rp);
            }
            if (rnd() < fresnel) {
                d = normalize_or(d + m * 2.0 * cosi, -d);
                o = p + lift;
            } else {
                let cost = sqrt(1.0 - sin2t);
                d = normalize_or(d * eta + m * (eta * cosi - cost), d);
                o = p - lift;
                through *= sqrt(surf.albedo / max(max3(surf.albedo), 1e-4));
            }
            continue;
        }
        let f = frame(n);
        let v = to_local(f, -d);
        if (v.z <= 0.0) { break; }
        let lift = ng * (1e-4 + 1e-4 * max3(abs(p)));
        if (sh.has_sss) {
            if (rnd() < 0.5) {
                let pr = probe(p, n, sh.sss_radius, sh.sss_item);
                var p2 = p;
                var ng2 = ng;
                var n2 = n;
                if (pr.found) { p2 = pr.p; ng2 = pr.ng; n2 = pr.n; }
                let enter = 1.0 - 0.04 - 0.96 * pow(1.0 - v.z, 5.0);
                through *= surf.albedo * pr.weight * (sh.sss_amount * enter * 2.0);
                let lift2 = ng2 * (1e-4 + 1e-4 * max3(abs(p2)));
                let pk = pick_lights(p2, ng2, true);
                for (var j = 0u; j < P.opts.w + 2u; j++) {
                    let slot = light_slot(j, pk);
                    if (slot.y <= 0.0) { continue; }
                    let ls = light_sample(bitcast<u32>(slot.x), p2, rnd2());
                    let c = dot(ls.l, n2);
                    if (c <= 0.0 || dot(ls.l, ng2) <= 0.0 || blocked(p2 + lift2, ls.l, ls.far - length(lift2))) { continue; }
                    color += clampi(through * ls.power * (c * FRAC_1_PI * slot.y * light_trans(p2, ls.l, ls.far)), bounce);
                }
                let es = env_sample();
                let c = max(dot(es.l, n2), 0.0);
                if (es.pdf > 0.0 && dot(es.l, ng2) > 0.0 && !blocked(p2 + lift2, es.l, 3.4e38)) {
                    let bp = c * FRAC_1_PI;
                    var weight = 1.0;
                    if (bounce != bounces) { weight = mis(es.pdf, bp); }
                    color += clampi(through * es.c * (bp * weight / es.pdf * light_trans(p2, es.l, 3.4e38)), bounce);
                }
                if (bounce == bounces) { break; }
                d = to_world(frame(n2), cosine_dir(rnd2()));
                o = p2 + lift2;
                previous_pdf = max(dot(d, n2), 0.0) * FRAC_1_PI;
                continue;
            }
            through *= 2.0;
            surf.albedo *= 1.0 - sh.sss_amount;
        }
        let pk = pick_lights(p, ng, true);
        for (var j = 0u; j < P.opts.w + 2u; j++) {
            let slot = light_slot(j, pk);
            if (slot.y <= 0.0) { continue; }
            let ls = light_sample(bitcast<u32>(slot.x), p, rnd2());
            if (dot(ls.l, ng) <= 0.0) { continue; }
            let fv = bsdf_eval(surf, v, to_local(f, ls.l));
            if (all(fv == vec3f(0.0)) || blocked(p + lift, ls.l, ls.far - length(lift))) { continue; }
            color += clampi(through * fv * ls.power * (slot.y * light_trans(p, ls.l, ls.far)), bounce);
        }
        let es = env_sample();
        if (es.pdf > 0.0 && dot(es.l, ng) > 0.0 && !blocked(p + lift, es.l, 3.4e38)) {
            let ll = to_local(f, es.l);
            var weight = 1.0;
            if (bounce != bounces) { weight = mis(es.pdf, bsdf_pdf(surf, v, ll)); }
            color += clampi(through * bsdf_eval(surf, v, ll) * es.c * (weight / es.pdf * light_trans(p, es.l, 3.4e38)), bounce);
        }
        if (bounce == bounces) { break; }
        let u = rnd2();
        let l = bsdf_sample(surf, v, u, rnd());
        if (l.z <= 0.0) { break; }
        let pdf = bsdf_pdf(surf, v, l);
        if (pdf <= 1e-6) { break; }
        through *= bsdf_eval(surf, v, l) / pdf;
        if (bounce >= 2u) {
            let keep = clamp(max3(through), 0.05, 0.95);
            if (rnd() > keep) { break; }
            through /= keep;
        }
        d = to_world(f, l);
        if (dot(d, ng) <= 0.0) { break; }
        previous_pdf = pdf;
        o = p + lift;
    }
    res.color = color;
    return res;
}

// ---------------------------------------------------------------- camera

fn camera_ray(x: f32, y: f32, lens: vec2f) -> array<vec3f, 2> {
    let w = f32(P.size.x);
    let h = f32(P.size.y);
    let sx = (x / w * 2.0 - 1.0) * (w / h);
    let sy = 1.0 - y / h * 2.0;
    if (P.forward.w > 0.0) {
        let hh = P.forward.w;
        return array<vec3f, 2>(P.eye.xyz + P.right.xyz * sx * hh + P.up.xyz * sy * hh, P.forward.xyz);
    }
    let fov_half = P.eye.w;
    let d = normalize(P.forward.xyz + P.right.xyz * sx * fov_half + P.up.xyz * sy * fov_half);
    let phi = TAU * lens.y;
    var aperture = vec2f(cos(phi),sin(phi))*sqrt(lens.x);
    if (P.lens_shape.x >= 3.0) {
        let sector = lens.y*P.lens_shape.x;
        let angle = floor(sector)*TAU/P.lens_shape.x+P.lens_shape.y;
        let next = angle+TAU/P.lens_shape.x;
        aperture = mix(vec2f(cos(angle),sin(angle)),vec2f(cos(next),sin(next)),fract(sector))*sqrt(lens.x);
    }
    let o = P.eye.xyz + P.right.w*(P.right.xyz*aperture.x+P.up.xyz*aperture.y);
    let point = P.eye.xyz + d * (P.up.w / max(dot(d, P.forward.xyz), 1e-6));
    return array<vec3f, 2>(o, normalize(point - o));
}

// NaN and infinity checks on the bits: fast math may drop x != x.
fn finite1(x: f32) -> bool { return (bitcast<u32>(x) & 0x7f800000u) != 0x7f800000u; }
fn finite3(v: vec3f) -> bool { return finite1(v.x) && finite1(v.y) && finite1(v.z); }

@compute @workgroup_size(8, 8)
fn main(@builtin(global_invocation_id) id: vec3u) {
    let gid = id + vec3u(P.counts.z, P.counts.w, 0u);
    if (gid.x >= P.size.x || gid.y >= P.size.y) { return; }
    let pixel = gid.y * P.size.x + gid.x;
    var sum = vec3f(0.0);
    var albedo = vec3f(0.0);
    var normal = vec3f(0.0);
    var depth = 0.0;
    for (var k = 0u; k < P.size.w; k++) {
        rng_seed(pixel + 1u, P.size.z + k + 7u);
        let j = rnd2();
        let ray = camera_ray(f32(gid.x) + j.x, f32(gid.y) + j.y, rnd2());
        let r = radiance(ray[0], ray[1]);
        var c = r.color;
        if (!finite3(c)) { c = vec3f(0.0); }
        sum += c;
        // A rare bad (NaN) guide would blacken the pixel in the denoiser.
        if (finite3(r.albedo) && finite3(r.normal) && finite1(r.depth)) {
            albedo += r.albedo;
            normal += r.normal;
            depth += min(r.depth, 1e4);
        } else {
            albedo += vec3f(0.5);
            depth += 1e4;
        }
    }
    accum[pixel * 3u] += vec4f(sum, 0.0);
    accum[pixel * 3u + 1u] += vec4f(albedo, 0.0);
    accum[pixel * 3u + 2u] += vec4f(normal, depth);
}
