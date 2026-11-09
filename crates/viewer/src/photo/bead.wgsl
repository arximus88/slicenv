// Спільне для «Фото»: читання інстансів траєкторії й трасований валик.
// Валик — капсула з еліптичним перерізом (сегмент осі + два заокруглені кінці). Вершинний
// шейдер ставить прямокутник, повернутий до камери, що покриває весь силует капсули з
// будь-якого ракурсу; фрагментний перетинає промінь камери з капсулою (`bead_trace`) і пише
// справжню глибину. Сусідні сегменти — окремі капсули з однаковим перерізом, тож стики
// зливаються без зубців (стрічка-імпостор, як у shader.wgsl, уздовж осі вироджувалась).
// «Око» може бути напрямком (ортогональна камера тіньової карти), колір інстансу — слот і
// прапорці. CPU-дзеркало перерізу — `photo/bead.rs::surface`.

@group(1) @binding(0) var<storage, read> segments: array<u32>;
@group(1) @binding(1) var<uniform> chunk: vec4<u32>;

const FLAG_SUPPORT: u32 = 1u;
const FLAG_SEAM: u32 = 2u;
// Лінія стінки (стос по висоті): у `Bead::material.y`, поруч із прапорцями інстансу.
const FLAG_WALL: u32 = 256u;

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
    // Точка прямокутника (площина перед капсулою, повернута до камери).
    world: vec3<f32>,
    // Кінці осі валика (центри перерізу).
    a: vec3<f32>,
    b: vec3<f32>,
    // Бічний і верхній напрямки перерізу (одиничні, перпендикулярні осі; верх — z ≥ 0).
    side: vec3<f32>,
    up: vec3<f32>,
    // Півширина, піввисота перерізу (з перекриттям), півшару, мм.
    dims: vec3<f32>,
    // Площини косого стику з сусіднім сегментом на початку й кінці: xyz — нормаль (уздовж
    // осі), w = 1 — стик є (циліндр обрізано площиною), 0 — вільний кінець (півкуля).
    cut_a: vec4<f32>,
    cut_b: vec4<f32>,
    // Середній шлях променя ока крізь злитий переріз валика, мм (товщина прозорого).
    chord: f32,
    // x — слот філаменту, y — прапорці (`FLAG_*`, зокрема `FLAG_WALL`).
    material: vec2<u32>,
}

// `eye.w` = 0: перспективне око в точці eye.xyz; 1 — ортогональна камера, eye.xyz — напрямок
// до неї. `view.x` — режим підтримок (2 — сховати). `pick` — див. `BEADS_*`.
fn bead_vertex(vertex: u32, view_proj: mat4x4<f32>, eye: vec4<f32>, view: vec4<u32>, pick: vec2<u32>) -> Bead {
    var out: Bead;
    let local = vertex / 6u;
    let corner = array<u32, 6>(0u, 1u, 2u, 2u, 1u, 3u)[vertex % 6u];
    let base = local * 11u;
    let pt_from = seg_v3(base, 0u);
    let pt_to = seg_v3(base, 3u);
    let packed = segments[base + 6u];
    let size = unpack2x16float(segments[base + 7u]);
    let caps = segments[base + 10u];
    let side_from = unpack2x16snorm(segments[base + 8u]) * 2.0;
    let side_to = unpack2x16snorm(segments[base + 9u]) * 2.0;
    let wall = select(0u, FLAG_WALL, (caps & 4u) != 0u);
    out.material = vec2<u32>(packed & 0xffu, ((packed >> 8u) & 0xffu) | wall);

    let clear = ((pick.y >> min(out.material.x, MAX_MATERIALS - 1u)) & 1u) == 1u;
    let skipped = (pick.x == BEADS_OPAQUE && clear) || (pick.x == BEADS_CLEAR && !clear);
    if ((out.material.y & FLAG_SUPPORT) != 0u && view.x == 2u) || skipped {
        // Сховані підтримки й валики іншого проходу: вироджений трикутник за межами кліпу.
        out.clip = vec4<f32>(0.0, 0.0, -2.0, 1.0);
        return out;
    }

    let hw = max(size.x * 0.5, 0.01);
    let hh_layer = max(size.y * 0.5, 0.01);
    let hh = hh_layer * BEAD_OVERLAP;
    let a = pt_from - vec3<f32>(0.0, 0.0, hh);
    let b = pt_to - vec3<f32>(0.0, 0.0, hh);
    let dir = b - a;
    let len = length(dir);
    let fwd = select(vec3<f32>(1.0, 0.0, 0.0), dir / max(len, 1e-6), len > 1e-5);
    // Бік — горизонтальний перпендикуляр до осі (вертикальний рух — бік −Y), верх — z ≥ 0.
    var side = cross(fwd, vec3<f32>(0.0, 0.0, 1.0));
    if dot(side, side) < 1e-8 {
        side = vec3<f32>(0.0, -1.0, 0.0);
    }
    side = normalize(side);
    var up = normalize(cross(side, fwd));
    if up.z < 0.0 {
        up = -up;
    }
    out.a = a;
    out.b = b;
    out.side = side;
    out.up = up;
    out.dims = vec3<f32>(hw, hh, hh_layer);
    // Косий стик (`ToolpathInstance::join`): площина з бічним вектором стику й вертикаллю.
    // |m| = 1/cos(половини повороту) — з нього tg половини повороту для розміру прямокутника.
    let joined_a = (caps & 1u) == 0u && len > 1e-5;
    let joined_b = (caps & 2u) == 0u && len > 1e-5;
    out.cut_a = vec4<f32>(miter_normal(side_from, fwd), select(0.0, 1.0, joined_a));
    out.cut_b = vec4<f32>(miter_normal(side_to, fwd), select(0.0, 1.0, joined_b));
    let tan_a = sqrt(max(dot(side_from, side_from) - 1.0, 0.0));
    let tan_b = sqrt(max(dot(side_to, side_to) - 1.0, 0.0));

    // Прямокутник у площині, перпендикулярній до напрямку погляду на середину валика, на
    // глибині найближчої точки капсули: проєкції кінців осі, розширені на опорну функцію
    // еліпсоїда перерізу (hw, hw, hh) — по висоті стос шарів не перекривається зайве.
    let ortho = eye.w > 0.5;
    let mid = 0.5 * (a + b);
    let d = select(normalize(mid - eye.xyz), -normalize(eye.xyz), ortho);
    let origin = select(eye.xyz, mid - d * 1000.0, ortho);
    let ta = dot(a - origin, d);
    let tb = dot(b - origin, d);
    let rd_ext = bead_extent(d, up, hw, hh);
    let t_near = max(min(ta, tb) - rd_ext, 1e-3);
    var pa: vec3<f32>;
    var pb: vec3<f32>;
    // Масштаб проєкції на площину t_near для кожного кінця (ортогонально — 1).
    var sa = 1.0;
    var sb = 1.0;
    if ortho {
        pa = a - d * (ta - t_near);
        pb = b - d * (tb - t_near);
    } else {
        // Перспектива: кінці проєктуються на площину крізь око; розмір на глибині t —
        // не більше ніж ×t_near/(t − r), видовжений до краю кадру на 1/cos.
        pa = eye.xyz + (a - eye.xyz) * (t_near / max(ta, t_near + 1e-3));
        pb = eye.xyz + (b - eye.xyz) * (t_near / max(tb, t_near + 1e-3));
        sa = t_near / max(ta - rd_ext, 1e-3) * length(a - eye.xyz) / max(ta, 1e-3);
        sb = t_near / max(tb - rd_ext, 1e-3) * length(b - eye.xyz) / max(tb, 1e-3);
    }
    var u = pb - pa;
    u = u - d * dot(u, d);
    if dot(u, u) < 1e-12 {
        u = cross(d, select(vec3<f32>(0.0, 0.0, 1.0), vec3<f32>(1.0, 0.0, 0.0), abs(d.z) > 0.9));
    }
    u = normalize(u);
    let w = normalize(cross(d, u));
    // Запас 6 % — на згладжений край (пів пікселя) і наближення перспективи.
    let ext_u = bead_extent(u, up, hw, hh) * 1.06;
    let ext_w = bead_extent(w, up, hw, hh) * 1.06;
    let half_w = ext_w * max(sa, sb);
    let at_end = corner >= 2u;
    let lateral = select(-1.0, 1.0, (corner & 1u) == 1u);
    // Вільний кінець — півкуля (повний виступ); стик — лише зріз циліндра: виступає за
    // кінець осі на частку від кута погляду вздовж осі й від повороту.
    let along = abs(dot(fwd, d));
    let reach_a = select(1.0, min(1.0, along + tan_a + 0.15), joined_a);
    let reach_b = select(1.0, min(1.0, along + tan_b + 0.15), joined_b);
    let pos = select(pa - u * (ext_u * sa * reach_a), pb + u * (ext_u * sb * reach_b), at_end) + w * (half_w * lateral);

    // Паралельні промені крізь прямокутник 2hw × 2hh (злиті валики): середній шлях =
    // площа / ширина проєкції, у 3D — поділений ще на частку напрямку в площині перерізу,
    // тож компоненти беруться ненормованими. Уздовж осі — не довше за сам валик. Товщина —
    // від шару, не від перекритого перерізу (злиті валики не товщають).
    let across_side = abs(dot(d, side));
    let across_up = abs(dot(d, up));
    out.chord = min(2.0 * hw * hh_layer / max(hw * across_up + hh_layer * across_side, 1e-4), len + 2.0 * hw);
    out.clip = view_proj * vec4<f32>(pos, 1.0);
    out.world = pos;
    return out;
}

// Нормаль площини косого стику (горизонтальний бічний вектор стику × вертикаль), повернута
// уздовж осі.
fn miter_normal(m: vec2<f32>, fwd: vec3<f32>) -> vec3<f32> {
    var n = vec3<f32>(m.y, -m.x, 0.0);
    if dot(n, n) < 1e-8 {
        return fwd;
    }
    n = normalize(n);
    return select(-n, n, dot(n, fwd) >= 0.0);
}

// Опорна функція еліпсоїда перерізу (hw уздовж і вбік, hh угору) в одиничному напрямку.
fn bead_extent(dir: vec3<f32>, up: vec3<f32>, hw: f32, hh: f32) -> f32 {
    let z = dot(dir, up);
    return sqrt(hw * hw * max(1.0 - z * z, 0.0) + hh * hh * z * z);
}

// Результат трасування пікселя.
struct BeadHit {
    // Точка поверхні й нормаль еліптичного перерізу (світові).
    pos: vec3<f32>,
    normal: vec3<f32>,
    // Точка на осі під пікселем і зміщення від неї в базисі (уздовж, бік, верх), мм.
    axis: vec3<f32>,
    local: vec3<f32>,
    // Частка пікселя, яку закриває капсула (згладжений силует для alpha-to-coverage);
    // 0 — промінь пройшов повз (discard).
    coverage: f32,
    // |cos| між променем і нормаллю в просторі кругового перерізу: 1 — промінь крізь
    // середину валика, 0 — по дотичній (профіль товщини прозорого).
    facing: f32,
}

// Промінь `ro + t·rd` (rd одиничний, світові мм) і капсула валика. У масштабованому базисі
// (верх × hw/hh) переріз — коло радіуса hw: перетин з нескінченним циліндром на відрізку осі
// або з кулею на кінці, найближчий. `px` — розмір пікселя біля валика, мм: край згладжується
// за відстанню променя до капсули.
fn bead_trace(ro: vec3<f32>, rd: vec3<f32>, a: vec3<f32>, b: vec3<f32>, side: vec3<f32>, up: vec3<f32>, dims: vec3<f32>, cut_a: vec4<f32>, cut_b: vec4<f32>, px: f32) -> BeadHit {
    var out: BeadHit;
    let hw = dims.x;
    let k = hw / dims.y;
    let ab = b - a;
    let len = length(ab);
    let fwd = select(vec3<f32>(1.0, 0.0, 0.0), ab / max(len, 1e-6), len > 1e-5);
    let to_s = mat3x3<f32>(
        vec3<f32>(fwd.x, side.x, up.x * k),
        vec3<f32>(fwd.y, side.y, up.y * k),
        vec3<f32>(fwd.z, side.z, up.z * k),
    );
    let o = to_s * (ro - a);
    let raw = to_s * rd;
    let scale = length(raw);
    let d = raw / max(scale, 1e-8);

    // Найближче зближення променя з віссю. На косому стику вісь триває далі (циліндр
    // обрізає площина), інакше край пікселя біля стику згасав би.
    let x_lo = select(0.0, -1e6, cut_a.w > 0.5);
    let x_hi = select(len, 1e6, cut_b.w > 0.5);
    let dx = d.x;
    let denom = 1.0 - dx * dx;
    var x = clamp(o.x, x_lo, x_hi);
    if denom > 1e-6 {
        x = clamp((o.x - dx * dot(d, o)) / denom, x_lo, x_hi);
    }
    let c = vec3<f32>(x, 0.0, 0.0);
    let t_close = max(dot(c - o, d), 0.0);
    let gap = o + d * t_close - c;
    let dist = length(gap);
    // Відстань у масштабованому базисі → мм: по боку 1:1, по висоті ×hh/hw.
    let g = gap / max(dist, 1e-8);
    let px_s = px * max(sqrt(g.y * g.y + g.z * g.z * k * k), 1e-3);
    out.coverage = clamp(0.5 + (hw - dist) / px_s, 0.0, 1.0);

    var t = -1.0;
    var n = g;
    if dist <= hw {
        // Циліндр.
        let a2 = d.y * d.y + d.z * d.z;
        if a2 > 1e-8 {
            let b2 = o.y * d.y + o.z * d.z;
            let c2 = o.y * o.y + o.z * o.z - hw * hw;
            let h = b2 * b2 - a2 * c2;
            if h >= 0.0 {
                let tc = (-b2 - sqrt(h)) / a2;
                let xc = o.x + d.x * tc;
                if xc >= x_lo && xc <= x_hi {
                    t = tc;
                    n = vec3<f32>(0.0, o.y + d.y * tc, o.z + d.z * tc) / hw;
                }
            }
        }
        // Кулі на кінцях.
        for (var i = 0; i < 2; i++) {
            // На косому стику півкулі немає.
            if select(cut_a.w, cut_b.w, i == 1) > 0.5 {
                continue;
            }
            let cc = vec3<f32>(select(0.0, len, i == 1), 0.0, 0.0);
            let oc = o - cc;
            let bs = dot(oc, d);
            let h = bs * bs - (dot(oc, oc) - hw * hw);
            if h >= 0.0 {
                let ts = -bs - sqrt(h);
                if ts >= 0.0 && (t < 0.0 || ts < t) {
                    t = ts;
                    n = (oc + d * ts) / hw;
                }
            }
        }
    }
    var ps: vec3<f32>;
    if t >= 0.0 {
        ps = o + d * t;
    } else {
        // Край пікселя поза капсулою: найближча точка силуету.
        ps = c + g * hw;
        n = g;
    }
    // Обрізання площинами стиків: те, що за площиною, малює сусідній сегмент.
    let wpos = a + fwd * ps.x + side * ps.y + up * (ps.z / k);
    // Допуск 2 % півширини: точка рівно на площині не відкидається обома сусідами (щілина).
    let tol = hw * 0.02;
    if (cut_a.w > 0.5 && dot(wpos - a, cut_a.xyz) < -tol) || (cut_b.w > 0.5 && dot(wpos - b, cut_b.xyz) > tol) {
        out.coverage = 0.0;
    }
    out.facing = abs(dot(n, d));
    let along = clamp(ps.x, 0.0, len);
    let lside = ps.y;
    let lup = ps.z / k;
    out.axis = a + fwd * along;
    out.local = vec3<f32>(ps.x - along, lside, lup);
    out.pos = a + fwd * ps.x + side * lside + up * lup;
    out.normal = normalize(fwd * n.x + side * n.y + up * (n.z * k));
    return out;
}

// Глибина точки для буфера глибини (clip.z / clip.w).
fn bead_depth(view_proj: mat4x4<f32>, pos: vec3<f32>) -> f32 {
    let c = view_proj * vec4<f32>(pos, 1.0);
    return c.z / c.w;
}
