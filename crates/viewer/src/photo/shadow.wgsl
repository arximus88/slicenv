// Тіньова карта ключового світла й карта висот згори (контактна тінь): лише глибина
// валиків з ортогональної камери. Спереду дописується bead.wgsl.

struct Pass {
    view_proj: mat4x4<f32>,
    // xyz — напрямок до камери (ортогональної), w = 1.
    dir: vec4<f32>,
    // Як `Globals.view` у photo.wgsl: режим підтримок і межі видимих інстансів.
    view: vec4<u32>,
}

@group(0) @binding(0) var<uniform> pass_: Pass;

struct ShadowOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) @interpolate(perspective, centroid) coord: vec3<f32>,
}

@vertex
fn vs_shadow(@builtin(vertex_index) vertex: u32) -> ShadowOut {
    let b = bead_vertex(vertex, pass_.view_proj, pass_.dir, pass_.view);
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
