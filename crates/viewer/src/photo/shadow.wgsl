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

struct ShadowOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) @interpolate(perspective, centroid) coord: vec3<f32>,
}

@vertex
fn vs_shadow(@builtin(vertex_index) vertex: u32) -> ShadowOut {
    let b = bead_vertex(vertex, pass_.view_proj, pass_.dir, pass_.view, pass_.pick.xy);
    var out: ShadowOut;
    out.clip = b.clip;
    // Ортогональна проєкція: вироджений трикутник прихованої підтримки (z = −2) відкидається
    // так само, як у перспективі.
    out.coord = b.coord;
    return out;
}

@fragment
fn fs_shadow(in: ShadowOut) {
    if bead_outside(in.coord) {
        discard;
    }
}

// ---- пропускання прозорих (кольорова тінь) ----

struct TransmitOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) @interpolate(perspective, centroid) coord: vec3<f32>,
    @location(1) chord: f32,
    @location(2) @interpolate(flat) slot: u32,
    @location(3) depth: f32,
}

@vertex
fn vs_transmit(@builtin(vertex_index) vertex: u32) -> TransmitOut {
    let b = bead_vertex(vertex, pass_.view_proj, pass_.dir, pass_.view, pass_.pick.xy);
    var out: TransmitOut;
    out.clip = b.clip;
    out.coord = b.coord;
    out.chord = b.chord;
    out.slot = b.material.x;
    // Ортогональна проєкція: w = 1, глибина — та сама, що в тіньовій карті.
    out.depth = b.clip.z;
    return out;
}

struct TransmitTargets {
    // rgb — Σ σ·шлях (додається), a — найдальша глибина (максимум).
    @location(0) density: vec4<f32>,
    // r — найближча глибина (мінімум).
    @location(1) near: vec4<f32>,
}

@fragment
fn fs_transmit(in: TransmitOut) -> TransmitTargets {
    if bead_outside(in.coord) {
        discard;
    }
    let m = materials[min(in.slot, MAX_MATERIALS - 1u)];
    // Розсіяне вперед світло здебільшого йде далі (до тіні — половина σs), а заломлення в
    // стінках збирає світло в плями: тінь прозорого світліша за чисте поглинання (× 0.6).
    let sigma = (m.p4.rgb + vec3<f32>(m.p3.w * 0.5)) * 0.6;
    var out: TransmitTargets;
    out.density = vec4<f32>(sigma * in.chord, in.depth);
    out.near = vec4<f32>(in.depth, 0.0, 0.0, 0.0);
    return out;
}
