struct Globals {
    view_proj: mat4x4<f32>,
    eye: vec4<f32>,
    // Світло прив'язане до камери, як в Orca: напрямки вже у світових координатах.
    light_top: vec4<f32>,
    light_front: vec4<f32>,
    // Колір моделі на «Підготовці», sRGB.
    model_color: vec4<f32>,
    // flat style, first visible instance, last visible instance, reserved
    toolpath_view: vec4<u32>,
    clip_plane: vec4<f32>,
    // Прев’ю різання: площина (dot(xyz, p) + w), рамка вибраного об’єкта; lo.w = 1 — увімкнено.
    cut_plane: vec4<f32>,
    cut_lo: vec4<f32>,
    cut_hi: vec4<f32>,
}

@group(0) @binding(0) var<uniform> globals: Globals;

fn srgb_to_linear(c: vec3<f32>) -> vec3<f32> {
    let lo = c / 12.92;
    let hi = pow((c + vec3<f32>(0.055)) / 1.055, vec3<f32>(2.4));
    return select(hi, lo, c <= vec3<f32>(0.04045));
}

// Освітлення шейдера gouraud_light з Orca/PrusaSlicer: верхнє світло з бліком, фронтальне й
// навколишнє. Рахується в sRGB, як там, щоб кольори й контраст шарів збігались з Orca.
fn orca_light(n: vec3<f32>, to_eye: vec3<f32>, base: vec3<f32>) -> vec3<f32> {
    let top = globals.light_top.xyz;
    let front = globals.light_front.xyz;
    let n_top = max(dot(n, top), 0.0);
    var intensity = 0.3 + n_top * 0.8 * 0.6 + max(dot(n, front), 0.0) * 0.3 * 0.6;
    var spec = 0.0;
    if n_top > 0.0 {
        spec = 0.125 * 0.6 * pow(max(dot(reflect(-top, n), to_eye), 0.0), 20.0);
    }
    return srgb_to_linear(clamp(vec3<f32>(spec) + base * intensity, vec3<f32>(0.0), vec3<f32>(1.0)));
}

// ---- модель ----

struct MeshOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) @interpolate(perspective, centroid) world: vec3<f32>,
    @location(1) normal: vec3<f32>,
}

@vertex
fn vs_mesh(
    @location(0) pos: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) m0: vec4<f32>,
    @location(3) m1: vec4<f32>,
    @location(4) m2: vec4<f32>,
    @location(5) m3: vec4<f32>,
) -> MeshOut {
    // Матриця сітки: зсув пластини або живий поворот/масштаб під час перетягування.
    let m = mat4x4<f32>(m0, m1, m2, m3);
    let world = (m * vec4<f32>(pos, 1.0)).xyz;
    var out: MeshOut;
    out.clip = globals.view_proj * vec4<f32>(world, 1.0);
    out.world = world;
    // Кофакторна матриця = det·(M⁻¹)ᵀ: правильні нормалі при масштабі по осях, а знак
    // det для дзеркальних частин компенсує перевернутий обхід (front_facing у fs_mesh).
    let a = m0.xyz;
    let b = m1.xyz;
    let c = m2.xyz;
    out.normal = mat3x3<f32>(cross(b, c), cross(c, a), cross(a, b)) * normal;
    return out;
}

@fragment
fn fs_mesh(in: MeshOut, @builtin(front_facing) front: bool) -> @location(0) vec4<f32> {
    if dot(globals.clip_plane, vec4<f32>(in.world, 1.0)) > 0.0 { discard; }
    var n = normalize(in.normal);
    if !front {
        n = -n;
    }
    let to_eye = normalize(globals.eye.xyz - in.world);
    var base = globals.model_color.rgb;
    // Як в Orca: частина над площиною — блакитна, під нею — пурпурна (лише в рамці вибраного).
    if globals.cut_lo.w > 0.5
        && all(in.world >= globals.cut_lo.xyz - vec3<f32>(0.02))
        && all(in.world <= globals.cut_hi.xyz + vec3<f32>(0.02)) {
        let tint = select(vec3<f32>(1.0, 0.0, 1.0), vec3<f32>(0.0, 1.0, 1.0), dot(globals.cut_plane, vec4<f32>(in.world, 1.0)) > 0.0);
        base = mix(base, tint, 0.5);
    }
    return vec4<f32>(orca_light(n, to_eye, base), 1.0);
}

// Preview helpers are independent of model clipping and toolpath visibility.
@fragment
fn fs_modifier(in: MeshOut, @builtin(front_facing) front: bool) -> @location(0) vec4<f32> {
    var n = normalize(in.normal);
    if !front { n = -n; }
    return vec4<f32>(orca_light(n, normalize(globals.eye.xyz - in.world), vec3<f32>(0.18, 0.72, 1.0)), 0.28);
}

// ---- лінії й плоскі кольорові трикутники ----

struct LineOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) color: vec4<f32>,
}

@vertex
fn vs_line(@location(0) pos: vec3<f32>, @location(1) color: vec4<f32>) -> LineOut {
    var out: LineOut;
    out.clip = globals.view_proj * vec4<f32>(pos, 1.0);
    out.color = color;
    return out;
}

@fragment
fn fs_line(in: LineOut) -> @location(0) vec4<f32> {
    return in.color;
}

// Стіл: знизу напівпрозорий, щоб було видно низ моделі й перший шар.
@fragment
fn fs_bed(in: LineOut) -> @location(0) vec4<f32> {
    let below = globals.eye.z < 0.0;
    return vec4<f32>(in.color.rgb, in.color.a * select(1.0, 0.35, below));
}

// ---- траєкторія екструзії: стрічка-імпостор еліптичного валика ----
//
// Кожен сегмент — не сітка трубки, а 4 кути (два трикутники), повернуті до камери:
// силует еліпса ширина × висота ліворуч і праворуч (s = ∓1). Нормаль справжнього еліпса
// відновлюється попіксельно з `s`, тож валик гладкий за будь-якої відстані, а вершин
// у 4.5 раза й трикутників у 16 разів менше, ніж у 8-гранної трубки. Площина стрічки
// проходить через найближчу до ока точку перерізу, тож глибина — на поверхні валика.
// Заокруглений торець — продовження стрічки на півширини з нормаллю півеліпсоїда.

struct ToolpathOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) world: vec3<f32>,
    @location(1) color: vec4<f32>,
    // x — s поперек стрічки; y — позиція вздовж осі в півширинах від початку; z — довжина
    // в півширинах. centroid — щоб MSAA не екстраполював за край.
    @location(2) @interpolate(perspective, centroid) coord: vec3<f32>,
    // Нормалі еліпса: на силуеті (s = 1), у найближчій точці (s = 0) і вздовж осі.
    @location(3) n_side: vec3<f32>,
    @location(4) n_front: vec3<f32>,
    @location(5) n_axis: vec3<f32>,
    // Нормаль поверхні, яку лінії утворюють разом: здалеку валик зливається в неї.
    @location(6) n_far: vec3<f32>,
}

// `GpuInstance` (11 слів: from, to, колір unorm8×4, ширина/висота f16, бічні вектори
// snorm16 × 2, caps) поспіль; шматок буфера починається із сегмента chunk.x;
// chunk.y = 1 — спрощена копія (LOD).
@group(1) @binding(0) var<storage, read> segments: array<u32>;
@group(1) @binding(1) var<uniform> chunk: vec4<u32>;

fn seg_f(base: u32, i: u32) -> f32 {
    return bitcast<f32>(segments[base + i]);
}

fn seg_v3(base: u32, i: u32) -> vec3<f32> {
    return vec3<f32>(seg_f(base, i), seg_f(base, i + 1u), seg_f(base, i + 2u));
}

@vertex
fn vs_toolpath(@builtin(vertex_index) vertex: u32) -> ToolpathOut {
    var out: ToolpathOut;

    // Два трикутники на сегмент; кути: 0 — початок ліворуч, 1 — початок праворуч,
    // 2 — кінець ліворуч, 3 — кінець праворуч.
    let local = vertex / 6u;
    let instance = chunk.x + local;
    let corner = array<u32, 6>(0u, 1u, 2u, 2u, 1u, 3u)[vertex % 6u];
    let base = local * 11u;
    let pt_from = seg_v3(base, 0u);
    let pt_to = seg_v3(base, 3u);
    let color = unpack4x8unorm(segments[base + 6u]);
    let size = unpack2x16float(segments[base + 7u]);
    let width = size.x;
    let height = size.y;
    let side_from = unpack2x16snorm(segments[base + 8u]) * 2.0;
    let side_to = unpack2x16snorm(segments[base + 9u]) * 2.0;
    let caps = segments[base + 10u];

    if (caps & 8u) != 0u {
        // Маркер-куля: квадрат до камери, винесений на радіус до ока, щоб лягти поверх валика.
        let r = max(width * 0.5, 0.05);
        let f = normalize(globals.eye.xyz - pt_from);
        var right = cross(f, vec3<f32>(0.0, 0.0, 1.0));
        if dot(right, right) < 1e-6 {
            right = vec3<f32>(1.0, 0.0, 0.0);
        }
        right = normalize(right);
        let up_s = cross(right, f);
        let q = vec2<f32>(select(-1.0, 1.0, (corner & 1u) == 1u), select(-1.0, 1.0, corner >= 2u));
        let pos = pt_from + f * r + (right * q.x + up_s * q.y) * r;
        out.clip = globals.view_proj * vec4<f32>(pos, 1.0);
        out.world = pos;
        out.coord = vec3<f32>(q, -1.0);
        out.n_side = right;
        out.n_front = f;
        out.n_axis = up_s;
        out.n_far = f;
        out.color = color;
        return out;
    }

    let at_end = corner >= 2u;
    let lateral = select(-1.0, 1.0, (corner & 1u) == 1u);
    let dir = pt_to - pt_from;
    let len = length(dir);
    let fwd = select(vec3<f32>(1.0, 0.0, 0.0), dir / max(len, 1e-6), len > 1e-5);
    // Бічний вектор на цьому кінці: на стику — спільна бісектриса з сусідньою лінією
    // (подовжена на 1/cos), тож зігнутий валик не має щілини зовні повороту.
    let side_xy = select(side_from, side_to, at_end);
    var side = vec3<f32>(side_xy, 0.0);
    if dot(side_xy, side_xy) < 1e-8 {
        side = vec3<f32>(0.0, -1.0, 0.0); // вертикальний рух
    }
    let side_n = normalize(side);
    let up = normalize(cross(side_n, fwd));

    let hw = max(width * 0.5, 0.01);
    let hh = select(max(height * 0.5, 0.01), 0.01, globals.toolpath_view.x == 1u);
    // Сопло на висоті Z, тож вісь валика на півшару нижче.
    let axis = select(pt_from, pt_to, at_end) - vec3<f32>(0.0, 0.0, hh);
    // Межі повзунків — лише в повному буфері (chunk.y = 0), у спрощеному їх немає.
    let boundary = chunk.y == 0u
        && select(instance == globals.toolpath_view.y, instance == globals.toolpath_view.z, at_end);
    let round = boundary || select(caps & 1u, (caps >> 1u) & 1u, at_end) == 1u;
    let reach = select(0.0, 1.0, round);

    // Напрямок до ока в площині перерізу, у просторі одиничного кола (еліпс / (hw, hh)):
    // там силует — перпендикуляр до нього, а найближча точка — він сам.
    let to_eye = globals.eye.xyz - axis;
    var v = vec2<f32>(dot(to_eye, side_n) / hw, dot(to_eye, up) / hh);
    if dot(v, v) < 1e-10 {
        v = vec2<f32>(0.0, 1.0); // дивимось точно вздовж осі
    }
    v = normalize(v);
    let p = vec2<f32>(v.y, -v.x);
    // Точка кола q → точка еліпса side·q.x·hw + up·q.y·hh; нормаль еліпса — (q.x/hw, q.y/hh).
    // Зсув до найближчої точки не змінює положення на екрані, лише глибину.
    let silhouette = (side * p.x) * hw + up * (p.y * hh);
    let front = side_n * (v.x * hw) + up * (v.y * hh);
    let cap_sign = select(-1.0, 1.0, at_end);
    let pos = axis + front + silhouette * lateral + fwd * (cap_sign * hw * reach);

    out.n_side = side_n * (p.x / hw) + up * (p.y / hh);
    out.n_front = side_n * (v.x / hw) + up * (v.y / hh);
    out.n_axis = fwd / hw;
    // Стінки лежать стосом по висоті — поверхня дивиться вбік (до ока); решта ліній
    // лягає поруч у шарі — поверхня дивиться вгору (до ока, якщо знизу).
    let wall = (caps & 4u) != 0u;
    out.n_far = select(up * sign(v.y + 1e-6), side_n * sign(v.x + 1e-6), wall);

    let length_hw = len / hw;
    out.clip = globals.view_proj * vec4<f32>(pos, 1.0);
    out.world = pos;
    out.coord = vec3<f32>(lateral, select(-reach, length_hw + reach, at_end), length_hw);
    out.color = color;
    return out;
}

// Освітлення валика в точці профілю s (−1…1) і торця c; `groove` — темніші краї.
fn ribbon_light(in: ToolpathOut, s: f32, c: f32, to_eye: vec3<f32>) -> vec3<f32> {
    let k = sqrt(max(1.0 - s * s - c * c, 0.0));
    let n = normalize(in.n_side * s + in.n_front * k + in.n_axis * c);
    let groove = mix(0.55, 1.0, sqrt(k));
    return orca_light(n, to_eye, in.color.rgb * groove);
}

@fragment
fn fs_toolpath(in: ToolpathOut) -> @location(0) vec4<f32> {
    if in.coord.z < 0.0 {
        // Маркер-куля (coord.z = −1): коло в квадраті, нормаль сфери.
        let r2 = dot(in.coord.xy, in.coord.xy);
        if r2 > 1.0 {
            discard;
        }
        let n = normalize(in.n_side * in.coord.x + in.n_axis * in.coord.y
            + in.n_front * sqrt(1.0 - r2));
        return vec4<f32>(orca_light(n, in.n_front, in.color.rgb), in.color.a);
    }
    if globals.toolpath_view.x == 1u {
        return vec4<f32>(srgb_to_linear(in.color.rgb), in.color.a);
    }
    // Торець — прямокутне продовження стрічки; поза півколом його відкидаємо, інакше
    // кути виступають темними квадратами й пишуть глибину.
    let cap = in.coord.y - clamp(in.coord.y, 0.0, in.coord.z);
    if cap != 0.0 && in.coord.x * in.coord.x + cap * cap > 1.0 {
        discard;
    }
    let to_eye = normalize(globals.eye.xyz - in.world);
    let s = clamp(in.coord.x, -1.0, 1.0);
    // Заокруглений торець: c — винос за кінець осі (у півширинах), від’ємний — за початок.
    let c = clamp(in.coord.y - clamp(in.coord.y, 0.0, in.coord.z), -1.0, 1.0);
    let near = ribbon_light(in, s, c, to_eye);
    // Проти муару: коли валик тонший за кілька пікселів, піксель бачить увесь профіль —
    // беремо середнє освітлення профілю (3 точки) замість однієї, а 40 % — освітлення
    // спільної поверхні ліній, щоб здалеку читалась форма моделі.
    let flatten = smoothstep(0.4, 1.4, fwidth(in.coord.x));
    if flatten <= 0.0 {
        return vec4<f32>(near, in.color.a);
    }
    let profile = (ribbon_light(in, -0.7, 0.0, to_eye) + ribbon_light(in, 0.0, 0.0, to_eye)
        + ribbon_light(in, 0.7, 0.0, to_eye)) / 3.0;
    let surface = orca_light(normalize(in.n_far), to_eye, in.color.rgb);
    let far = mix(profile, surface, 0.4);
    return vec4<f32>(mix(near, far, flatten), in.color.a);
}
