// Спільне для «Фото»: читання інстансів траєкторії й стрічка-імпостор валика.
// Та сама геометрія, що `vs_toolpath` у shader.wgsl (див. коментарі там), але «око» може бути
// напрямком (ортогональна камера тіньової карти), а колір інстансу — слот і прапорці.
// CPU-дзеркало профілю — `photo/bead.rs::surface`.

@group(1) @binding(0) var<storage, read> segments: array<u32>;
@group(1) @binding(1) var<uniform> chunk: vec4<u32>;

const FLAG_SUPPORT: u32 = 1u;
const FLAG_SEAM: u32 = 2u;

// Які валики малює прохід (`pick.x` у `bead_vertex`): усі; лише непрозорі слоти; лише
// прозорі. `pick.y` — маска прозорих слотів (біт = слот, як `Photo::transparent_mask`).
const BEADS_ALL: u32 = 0u;
const BEADS_OPAQUE: u32 = 1u;
const BEADS_CLEAR: u32 = 2u;

struct Material {
    // rgb — колір (лінійний; для прозорих — пропускання шару absorption_distance),
    // a — напівпрозорість.
    base: vec4<f32>,
    // roughness, metallic, specular, clearcoat
    p0: vec4<f32>,
    // clearcoat roughness, sheen, anisotropy, emission
    p1: vec4<f32>,
    // pattern, scale, strength, transmission
    p2: vec4<f32>,
    // rgb — другий колір візерунка (лінійний), a — розсіяння σs (1/мм)
    p3: vec4<f32>,
    // rgb — поглинання σa (1/мм, лінійне), a — absorption_distance (мм)
    p4: vec4<f32>,
}

const MAX_MATERIALS: u32 = 32u;

// Переріз валика вищий за шар: сусідні шари злипаються. Еліпс висотою рівно в шар торкався
// сусіднього лише в точці — виріз між шарами на всю півширину (~0.2 мм) давав темні щілини.
// З перекриттям k виріз — hw·(1 − √(1 − 1/k²)) ≈ 0.33·hw при k = 1.35: мілка борозенка, як
// у справжньому друці (більше k — ще мілкіше, але більше перемальовок). Верх лишається на
// висоті сопла, низ заходить у шар нижче. CPU-дзеркало — `photo/bead.rs::BEAD_OVERLAP`.
const BEAD_OVERLAP: f32 = 1.35;

fn seg_f(base: u32, i: u32) -> f32 {
    return bitcast<f32>(segments[base + i]);
}

fn seg_v3(base: u32, i: u32) -> vec3<f32> {
    return vec3<f32>(seg_f(base, i), seg_f(base, i + 1u), seg_f(base, i + 2u));
}

struct Bead {
    clip: vec4<f32>,
    world: vec3<f32>,
    coord: vec3<f32>,
    n_side: vec3<f32>,
    n_front: vec3<f32>,
    n_axis: vec3<f32>,
    n_far: vec3<f32>,
    // Півширина й «масштаб висоти» перерізу, мм (профіль валика в photo.wgsl): y = hh·k,
    // тож dot(нормаль, вгору)·y = 1 на межі шару (півшару від середини).
    half: vec2<f32>,
    // Зсув стрічки-імпостора до ока від осі: точка поверхні = world − front·(1 − k).
    front: vec3<f32>,
    // Середній шлях променя ока крізь злитий переріз валика, мм (товщина прозорого).
    chord: f32,
    // x — слот філаменту, y — прапорці.
    material: vec2<u32>,
}

// `eye.w` = 0: перспективне око в точці eye.xyz; 1 — ортогональна камера, eye.xyz — напрямок
// до неї. `view.x` — режим підтримок (2 — сховати), view.y/z — перший і останній видимі
// інстанси (межі повзунків отримують заокруглені торці). `pick` — див. `BEADS_*`.
fn bead_vertex(vertex: u32, view_proj: mat4x4<f32>, eye: vec4<f32>, view: vec4<u32>, pick: vec2<u32>) -> Bead {
    var out: Bead;
    let local = vertex / 6u;
    let instance = chunk.x + local;
    let corner = array<u32, 6>(0u, 1u, 2u, 2u, 1u, 3u)[vertex % 6u];
    let base = local * 11u;
    let pt_from = seg_v3(base, 0u);
    let pt_to = seg_v3(base, 3u);
    let packed = segments[base + 6u];
    let size = unpack2x16float(segments[base + 7u]);
    let side_from = unpack2x16snorm(segments[base + 8u]) * 2.0;
    let side_to = unpack2x16snorm(segments[base + 9u]) * 2.0;
    let caps = segments[base + 10u];
    out.material = vec2<u32>(packed & 0xffu, (packed >> 8u) & 0xffu);

    let clear = ((pick.y >> min(out.material.x, MAX_MATERIALS - 1u)) & 1u) == 1u;
    let skipped = (pick.x == BEADS_OPAQUE && clear) || (pick.x == BEADS_CLEAR && !clear);
    if ((out.material.y & FLAG_SUPPORT) != 0u && view.x == 2u) || skipped {
        // Сховані підтримки й валики іншого проходу: вироджений трикутник за межами кліпу.
        out.clip = vec4<f32>(0.0, 0.0, -2.0, 1.0);
        return out;
    }

    let at_end = corner >= 2u;
    let lateral = select(-1.0, 1.0, (corner & 1u) == 1u);
    let dir = pt_to - pt_from;
    let len = length(dir);
    let fwd = select(vec3<f32>(1.0, 0.0, 0.0), dir / max(len, 1e-6), len > 1e-5);
    let side_xy = select(side_from, side_to, at_end);
    var side = vec3<f32>(side_xy, 0.0);
    if dot(side_xy, side_xy) < 1e-8 {
        side = vec3<f32>(0.0, -1.0, 0.0);
    }
    let side_n = normalize(side);
    let up = normalize(cross(side_n, fwd));

    let hw = max(size.x * 0.5, 0.01);
    let hh_layer = max(size.y * 0.5, 0.01);
    let hh = hh_layer * BEAD_OVERLAP;
    let axis = select(pt_from, pt_to, at_end) - vec3<f32>(0.0, 0.0, hh);
    let boundary = chunk.y == 0u && select(instance == view.y, instance == view.z, at_end);
    let round = boundary || select(caps & 1u, (caps >> 1u) & 1u, at_end) == 1u;
    let reach = select(0.0, 1.0, round);

    let to_eye = select(eye.xyz - axis, eye.xyz * 1000.0, eye.w > 0.5);
    var v = vec2<f32>(dot(to_eye, side_n) / hw, dot(to_eye, up) / hh);
    if dot(v, v) < 1e-10 {
        v = vec2<f32>(0.0, 1.0);
    }
    v = normalize(v);
    let p = vec2<f32>(v.y, -v.x);
    let silhouette = (side * p.x) * hw + up * (p.y * hh);
    let front = side_n * (v.x * hw) + up * (v.y * hh);
    let cap_sign = select(-1.0, 1.0, at_end);
    let pos = axis + front + silhouette * lateral + fwd * (cap_sign * hw * reach);

    out.n_side = side_n * (p.x / hw) + up * (p.y / hh);
    out.n_front = side_n * (v.x / hw) + up * (v.y / hh);
    out.n_axis = fwd / hw;
    let wall = (caps & 4u) != 0u;
    // Стінка — бічний напрямок без знака: з якого боку валика піксель, вирішує фрагментний
    // шейдер. Знак за оком у вершинах перекидається посеред валика, коли око над ним
    // (інтерпольований вектор вироджується в нуль).
    out.n_far = select(up * sign(v.y + 1e-6), side_n, wall);

    out.half = vec2<f32>(hw, hh * BEAD_OVERLAP);
    out.front = front;
    // Паралельні промені крізь прямокутник 2hw × 2hh (злиті валики): середній шлях =
    // площа / ширина проєкції, у 3D — поділений ще на частку напрямку в площині перерізу,
    // тож компоненти беруться ненормованими. Уздовж осі — не довше за сам валик.
    let ray = normalize(to_eye);
    let across_side = abs(dot(ray, side_n));
    let across_up = abs(dot(ray, up));
    // Товщина — від шару, не від перекритого перерізу (злиті валики не товщають).
    out.chord = min(2.0 * hw * hh_layer / max(hw * across_up + hh_layer * across_side, 1e-4), len + 2.0 * hw);
    let length_hw = len / hw;
    out.clip = view_proj * vec4<f32>(pos, 1.0);
    out.world = pos;
    out.coord = vec3<f32>(lateral, select(-reach, length_hw + reach, at_end), length_hw);
    return out;
}

// Торець — прямокутне продовження стрічки; поза півколом його кути відкидаються.
fn bead_outside(coord: vec3<f32>) -> bool {
    let cap = coord.y - clamp(coord.y, 0.0, coord.z);
    return cap != 0.0 && coord.x * coord.x + cap * cap > 1.0;
}
