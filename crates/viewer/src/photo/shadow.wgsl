// Тіньова карта ключового світла й карта висот згори (контактна тінь): лише глибина
// валиків з ортогональної камери. Карта пропускання (`fs_transmit`) — оптична густина
// прозорих валиків уздовж променя світла для кольорової тіні. Спереду дописується bead.wgsl.

struct Pass {
    view_proj: mat4x4<f32>,
    // xyz — напрямок до камери (ортогональної), w = 1.
    dir: vec4<f32>,
    // Як `Globals.view` у photo.wgsl: режим підтримок і межі видимих інстансів.
    view: vec4<u32>,
    // xy — фільтр валиків (`BEADS_*`, маска прозорих слотів).
    pick: vec4<u32>,
}

@group(0) @binding(0) var<uniform> pass_: Pass;
@group(0) @binding(1) var<uniform> materials: array<Material, MAX_MATERIALS>;

// Валик для тіньових проходів: прямокутник перед капсулою й дані для трасування (як
// `BeadOut` у photo.wgsl, але камера ортогональна).
struct ShadowOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) world: vec3<f32>,
    @location(1) @interpolate(flat) a: vec3<f32>,
    @location(2) @interpolate(flat) b: vec3<f32>,
    @location(3) @interpolate(flat) side: vec3<f32>,
    @location(4) @interpolate(flat) up: vec3<f32>,
    @location(5) @interpolate(flat) dims: vec3<f32>,
    @location(8) @interpolate(flat) cut_a: vec4<f32>,
    @location(9) @interpolate(flat) cut_b: vec4<f32>,
    @location(6) @interpolate(flat) chord: f32,
    @location(7) @interpolate(flat) slot: u32,
}

fn shadow_vertex(vertex: u32) -> ShadowOut {
    let b = bead_vertex(vertex, pass_.view_proj, pass_.dir, pass_.view, pass_.pick.xy);
    var out: ShadowOut;
    // Ортогональна проєкція: вироджений трикутник прихованої підтримки (z = −2) відкидається
    // так само, як у перспективі.
    out.clip = b.clip;
    out.world = b.world;
    out.a = b.a;
    out.b = b.b;
    out.side = b.side;
    out.up = b.up;
    out.dims = b.dims;
    out.cut_a = b.cut_a;
    out.cut_b = b.cut_b;
    out.chord = b.chord;
    out.slot = b.material.x;
    return out;
}

@vertex
fn vs_shadow(@builtin(vertex_index) vertex: u32) -> ShadowOut {
    return shadow_vertex(vertex);
}

// Паралельний промінь світла крізь піксель тіньової карти і капсула валика. Без
// згладжування краю (тіньова карта — лише глибина): піксель або закритий, або ні.
fn shadow_hit(in: ShadowOut) -> BeadHit {
    return bead_trace(in.world, -normalize(pass_.dir.xyz), in.a, in.b, in.side, in.up, in.dims, in.cut_a, in.cut_b, 1e-4);
}

// Пряма глибина: трасована поверхня не ближча за прямокутник — глибина не менша.
//@early_depth_test(greater_equal)
@fragment
fn fs_shadow(in: ShadowOut) -> @builtin(frag_depth) f32 {
    let hit = shadow_hit(in);
    if hit.coverage < 0.5 {
        discard;
    }
    return bead_depth(pass_.view_proj, hit.pos);
}

// ---- пропускання прозорих (кольорова тінь) ----

struct TransmitTargets {
    // rgb — Σ σ·шлях (додається), a — найдальша глибина (максимум).
    @location(0) density: vec4<f32>,
    // r — найближча глибина (мінімум).
    @location(1) near: vec4<f32>,
}

@vertex
fn vs_transmit(@builtin(vertex_index) vertex: u32) -> ShadowOut {
    return shadow_vertex(vertex);
}

@fragment
fn fs_transmit(in: ShadowOut) -> TransmitTargets {
    let hit = shadow_hit(in);
    if hit.coverage < 0.5 {
        discard;
    }
    // Ортогональна проєкція: та сама глибина, що в тіньовій карті.
    let depth = bead_depth(pass_.view_proj, hit.pos);
    let m = materials[min(in.slot, MAX_MATERIALS - 1u)];
    // Розсіяне вперед світло здебільшого йде далі (до тіні — половина σs), а заломлення в
    // стінках збирає світло в плями: тінь прозорого світліша за чисте поглинання (× 0.6).
    let sigma = (m.p4.rgb + vec3<f32>(m.p3.w * 0.5)) * 0.6;
    var out: TransmitTargets;
    out.density = vec4<f32>(sigma * in.chord, depth);
    out.near = vec4<f32>(depth, 0.0, 0.0, 0.0);
    return out;
}
