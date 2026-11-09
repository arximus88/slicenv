// Режим «Фото»: валики з матеріалом (спрощений principled BSDF), IBL зі студії, тінь від
// ключового світла, контактна тінь на підлозі, ACES. Спереду дописуються env.wgsl і
// bead.wgsl (див. photo.rs).
//
// Тонмапінг — у кожному фрагменті, до розв’язання MSAA: ціль та сама sRGB, що й у
// звичайного Preview, без окремого HDR-буфера (менше VRAM, без зайвого проходу), і краї
// яскравих відблисків згладжуються вже після стиснення діапазону.

struct Globals {
    view_proj: mat4x4<f32>,
    // w = 0: перспективна камера (див. `bead_vertex`).
    eye: vec4<f32>,
    inv_view_proj: mat4x4<f32>,
    // Світ → тіньова карта ключового світла: xy — UV, z — глибина.
    shadow: mat4x4<f32>,
    // Світ → карта висот згори (контактна тінь): xy — UV, z — глибина.
    occupancy: mat4x4<f32>,
    // xyz — напрямок до світла, w — інтенсивність.
    light: vec4<f32>,
    // rgb — колір світла, w — інтенсивність оточення.
    light_color: vec4<f32>,
    // x — підтримки (0 як модель, 1 приглушено, 2 сховано), y/z — межі видимих інстансів,
    // w — сцена (0 — нейтральна підлога, 1 — стіл).
    view: vec4<u32>,
    // xy — центр підлоги, z — висота, w — радіус, де підлога зливається з фоном.
    floor: vec4<f32>,
    // x — експозиція, y — розмір текселя тіні (UV), z — тексель тіні у мм, w — висота верху
    // карти висот (мм).
    params: vec4<f32>,
    // x — глибина карти висот (мм), yzw — запас.
    params2: vec4<f32>,
    // rgb — колір столу (лінійний), w — шорсткість.
    bed_color: vec4<f32>,
}

struct Material {
    // rgb — колір (лінійний), a — напівпрозорість.
    base: vec4<f32>,
    // roughness, metallic, specular, clearcoat
    p0: vec4<f32>,
    // clearcoat roughness, sheen, anisotropy, emission
    p1: vec4<f32>,
    // pattern, scale, strength, —
    p2: vec4<f32>,
    // rgb — другий колір візерунка (лінійний)
    p3: vec4<f32>,
}

const MAX_MATERIALS: u32 = 32u;
const SPEC_MIPS: f32 = 5.0;

const PATTERN_MARBLE: u32 = 1u;
const PATTERN_SPARKLE: u32 = 2u;
const PATTERN_CARBON: u32 = 3u;
const PATTERN_WOOD: u32 = 4u;

@group(0) @binding(0) var<uniform> globals: Globals;
@group(0) @binding(1) var<uniform> materials: array<Material, MAX_MATERIALS>;
@group(0) @binding(2) var spec_cube: texture_cube<f32>;
@group(0) @binding(3) var irr_cube: texture_cube<f32>;
@group(0) @binding(4) var cube_sampler: sampler;
@group(0) @binding(5) var shadow_map: texture_depth_2d;
@group(0) @binding(6) var shadow_sampler: sampler_comparison;
@group(0) @binding(7) var occupancy_map: texture_depth_2d;

// ---- шум ----

fn hash31(p: vec3<f32>) -> f32 {
    var q = fract(p * 0.1031);
    q += dot(q, q.zyx + 31.32);
    return fract((q.x + q.y) * q.z);
}

fn hash33(p: vec3<f32>) -> vec3<f32> {
    var q = fract(p * vec3<f32>(0.1031, 0.1030, 0.0973));
    q += dot(q, q.yxz + 33.33);
    return fract((q.xxy + q.yxx) * q.zyx);
}

fn vnoise(p: vec3<f32>) -> f32 {
    let i = floor(p);
    let f = fract(p);
    let u = f * f * (3.0 - 2.0 * f);
    let a = mix(mix(hash31(i), hash31(i + vec3<f32>(1.0, 0.0, 0.0)), u.x),
        mix(hash31(i + vec3<f32>(0.0, 1.0, 0.0)), hash31(i + vec3<f32>(1.0, 1.0, 0.0)), u.x), u.y);
    let b = mix(mix(hash31(i + vec3<f32>(0.0, 0.0, 1.0)), hash31(i + vec3<f32>(1.0, 0.0, 1.0)), u.x),
        mix(hash31(i + vec3<f32>(0.0, 1.0, 1.0)), hash31(i + vec3<f32>(1.0, 1.0, 1.0)), u.x), u.y);
    return mix(a, b, u.z);
}

fn fbm(p: vec3<f32>) -> f32 {
    var sum = 0.0;
    var amp = 0.5;
    var q = p;
    for (var i = 0; i < 4; i++) {
        sum += amp * vnoise(q);
        q = q * 2.03 + vec3<f32>(17.1, 3.7, 9.2);
        amp *= 0.5;
    }
    return sum;
}

// Вкраплення: у кожній комірці з імовірністю `density` — куля радіуса r_lo…r_hi (частки
// комірки), зміщена так, щоб не виходити за комірку (сусідів перевіряти не треба). Край
// розмитий; `fw` — розмір пікселя в комірках: дрібні плями плавно стають середнім покриттям
// (частка площі зрізу кулі, ∫π(r²−z²)dz = 4πr³/3 на комірку).
fn speckle(q: vec3<f32>, density: f32, r_lo: f32, r_hi: f32, fw: f32) -> f32 {
    let cell = floor(q);
    let h = hash33(cell);
    let center = vec3<f32>(0.5) + (h - 0.5) * (1.0 - 2.0 * r_hi);
    let r = mix(r_lo, r_hi, hash31(cell + vec3<f32>(11.0, 3.0, 7.0)));
    let on = select(0.0, 1.0, hash31(cell + vec3<f32>(5.0, 19.0, 2.0)) < density);
    let d = length(fract(q) - center);
    let edge = max(r * 0.6, fw);
    let cover = on * (1.0 - smoothstep(r - edge * 0.5, r + edge * 0.5, d));
    let r_mid = 0.5 * (r_lo + r_hi);
    let mean = density * 4.18879 * r_mid * r_mid * r_mid;
    return mix(cover, mean, smoothstep(0.2, 0.55, fw));
}

// ---- BRDF ----

fn d_ggx(nh: f32, a: f32) -> f32 {
    let a2 = a * a;
    let f = nh * nh * (a2 - 1.0) + 1.0;
    return a2 / (PI * f * f);
}

fn d_ggx_aniso(nh: f32, th: f32, bh: f32, at: f32, ab: f32) -> f32 {
    let d = th * th / (at * at) + bh * bh / (ab * ab) + nh * nh;
    return 1.0 / (PI * at * ab * d * d);
}

fn v_smith(nv: f32, nl: f32, a: f32) -> f32 {
    let a2 = a * a;
    let gv = nl * sqrt(nv * nv * (1.0 - a2) + a2);
    let gl = nv * sqrt(nl * nl * (1.0 - a2) + a2);
    return 0.5 / max(gv + gl, 1e-5);
}

fn v_smith_aniso(at: f32, ab: f32, tv: f32, bv: f32, nv: f32, tl: f32, bl: f32, nl: f32) -> f32 {
    let lv = nl * length(vec3<f32>(at * tv, ab * bv, nv));
    let ll = nv * length(vec3<f32>(at * tl, ab * bl, nl));
    return 0.5 / max(lv + ll, 1e-5);
}

fn f_schlick(f0: vec3<f32>, c: f32) -> vec3<f32> {
    let k = pow(1.0 - c, 5.0);
    return f0 + (vec3<f32>(1.0) - f0) * k;
}

fn f_schlick1(f0: f32, c: f32) -> f32 {
    return f0 + (1.0 - f0) * pow(1.0 - c, 5.0);
}

// Тканинний відблиск Charlie (Estevez, Kulla) з видимістю Neubelt.
fn d_charlie(roughness: f32, nh: f32) -> f32 {
    let inv = 1.0 / max(roughness, 0.07);
    let sin2 = max(1.0 - nh * nh, 0.0078125);
    return (2.0 + inv) * pow(sin2, inv * 0.5) / (2.0 * PI);
}

fn v_neubelt(nv: f32, nl: f32) -> f32 {
    return 1.0 / max(4.0 * (nl + nv - nl * nv), 1e-4);
}

// Наближення інтегралу split-sum (Karis, «Mobile» EnvBRDF) — без LUT-текстури.
fn env_brdf(f0: vec3<f32>, nv: f32, roughness: f32) -> vec3<f32> {
    let c0 = vec4<f32>(-1.0, -0.0275, -0.572, 0.022);
    let c1 = vec4<f32>(1.0, 0.0425, 1.04, -0.04);
    let r = roughness * c0 + c1;
    let a004 = min(r.x * r.x, exp2(-9.28 * nv)) * r.x + r.y;
    let ab = vec2<f32>(-1.04, 1.04) * a004 + r.zw;
    return f0 * ab.x + vec3<f32>(ab.y);
}

fn aces(x: vec3<f32>) -> vec3<f32> {
    // ACES fitted (Stephen Hill): sRGB → AP1 → RRT+ODT → sRGB.
    let m1 = mat3x3<f32>(
        vec3<f32>(0.59719, 0.07600, 0.02840),
        vec3<f32>(0.35458, 0.90834, 0.13383),
        vec3<f32>(0.04823, 0.01566, 0.83777),
    );
    let m2 = mat3x3<f32>(
        vec3<f32>(1.60475, -0.10208, -0.00327),
        vec3<f32>(-0.53108, 1.10813, -0.07276),
        vec3<f32>(-0.07367, -0.00605, 1.07602),
    );
    let v = m1 * x;
    let a = v * (v + 0.0245786) - 0.000090537;
    let b = v * (0.983729 * v + 0.4329510) + 0.238081;
    return clamp(m2 * (a / b), vec3<f32>(0.0), vec3<f32>(1.0));
}

fn tonemap(c: vec3<f32>) -> vec3<f32> {
    return aces(c * globals.params.x);
}

// ---- тіні ----

const POISSON: array<vec2<f32>, 12> = array<vec2<f32>, 12>(
    vec2<f32>(-0.326, -0.406), vec2<f32>(-0.840, -0.074), vec2<f32>(-0.696, 0.457),
    vec2<f32>(-0.203, 0.621), vec2<f32>(0.962, -0.195), vec2<f32>(0.473, -0.480),
    vec2<f32>(0.519, 0.767), vec2<f32>(0.185, -0.893), vec2<f32>(0.507, 0.064),
    vec2<f32>(0.896, 0.412), vec2<f32>(-0.322, -0.933), vec2<f32>(-0.792, -0.598),
);

fn shadow_at(world: vec3<f32>, n: vec3<f32>) -> f32 {
    // Зсув уздовж нормалі й до світла на тексель-півтора: імпостор валика з боку світла
    // лежить трохи інакше, ніж з боку камери.
    let texel_mm = globals.params.z;
    let p = globals.shadow * vec4<f32>(world + n * texel_mm * 1.5 + globals.light.xyz * texel_mm * 2.0, 1.0);
    if any(p.xy < vec2<f32>(0.0)) || any(p.xy > vec2<f32>(1.0)) || p.z > 1.0 {
        return 1.0;
    }
    // Диск без випадкового повороту: поворот на піксель давав зерно, яке MSAA не розмиває
    // (затінення — раз на піксель); 12 точок поверх апаратного 2×2 дають рівний півтінь.
    let rot = mat2x2<f32>(1.0, 0.0, 0.0, 1.0);
    let radius = globals.params.y * 2.5;
    var lit = 0.0;
    for (var i = 0; i < 12; i++) {
        lit += textureSampleCompareLevel(shadow_map, shadow_sampler, p.xy + rot * POISSON[i] * radius, p.z);
    }
    return lit / 12.0;
}

// Контактна тінь: частка неба, яку закриває модель, з карти висот згори.
fn contact_ao(world: vec3<f32>) -> f32 {
    let p = globals.occupancy * vec4<f32>(world, 1.0);
    if any(p.xy < vec2<f32>(0.0)) || any(p.xy > vec2<f32>(1.0)) {
        return 1.0;
    }
    let size = vec2<f32>(textureDimensions(occupancy_map));
    let mm_per_uv = globals.params2.yz;
    let top = globals.params.w;
    let depth = globals.params2.x;
    var occluded = 0.0;
    var total = 0.0;
    for (var ring = 0; ring < 4; ring++) {
        let r = 1.2 * pow(2.2, f32(ring));
        for (var k = 0; k < 6; k++) {
            let a = (f32(k) + f32(ring) * 0.5) * 1.0471976;
            let offset = vec2<f32>(cos(a), sin(a)) * r;
            let uv = p.xy + offset / mm_per_uv;
            let texel = vec2<i32>(clamp(uv, vec2<f32>(0.0), vec2<f32>(0.9999)) * size);
            let d = textureLoad(occupancy_map, texel, 0);
            let h = max(top - d * depth - world.z, 0.0);
            // Синус кута підйому закриття: високе й близьке закриває більше неба.
            occluded += h / sqrt(h * h + r * r);
            total += 1.0;
        }
    }
    return clamp(1.0 - 0.9 * occluded / total, 0.0, 1.0);
}

// ---- освітлення поверхні ----

struct Surface {
    base: vec3<f32>,
    roughness: f32,
    metallic: f32,
    specular: f32,
    clearcoat: f32,
    clearcoat_roughness: f32,
    sheen: f32,
    anisotropy: f32,
    translucency: f32,
    emission: f32,
    tangent: vec3<f32>,
    ao: f32,
    shadow: f32,
}

fn default_surface(base: vec3<f32>, roughness: f32) -> Surface {
    var s: Surface;
    s.base = base;
    s.roughness = roughness;
    s.metallic = 0.0;
    s.specular = 0.5;
    s.clearcoat = 0.0;
    s.clearcoat_roughness = 0.1;
    s.sheen = 0.0;
    s.anisotropy = 0.0;
    s.translucency = 0.0;
    s.emission = 0.0;
    s.tangent = vec3<f32>(1.0, 0.0, 0.0);
    s.ao = 1.0;
    s.shadow = 1.0;
    return s;
}

fn shade(n_in: vec3<f32>, v: vec3<f32>, s: Surface) -> vec3<f32> {
    var n = n_in;
    if dot(n, v) < 0.0 {
        // Імпостор на силуеті може дати нормаль від ока — дзеркалимо в бік камери.
        n = normalize(n - v * dot(n, v) * 1.01);
    }
    let l = globals.light.xyz;
    let h = normalize(l + v);
    let nv = clamp(dot(n, v), 1e-4, 1.0);
    let nl_raw = dot(n, l);
    let nl = clamp(nl_raw, 0.0, 1.0);
    let nh = clamp(dot(n, h), 0.0, 1.0);
    let vh = clamp(dot(v, h), 0.0, 1.0);
    let rough = clamp(s.roughness, 0.03, 1.0);
    let a = rough * rough;
    let f0 = mix(vec3<f32>(0.08 * s.specular), s.base, s.metallic);
    let diffuse_color = s.base * (1.0 - s.metallic);
    let light = globals.light_color.rgb * globals.light.w;

    // Анізотропія: дотична — уздовж валика (волокна Silk), бідотична — поперек. Відблиск
    // волокнистої поверхні тягнеться поперек волокон, тож ширша частка — уздовж бідотичної
    // (той самий напрямок, що й зігнута нормаль оточення нижче).
    let t = normalize(s.tangent - n * dot(n, s.tangent));
    let b = cross(n, t);

    // --- пряме ключове світло ---
    var spec_d: f32;
    if s.anisotropy > 0.01 {
        let at = max(a * (1.0 - s.anisotropy), 0.002);
        let ab = max(a * (1.0 + s.anisotropy), 0.002);
        spec_d = d_ggx_aniso(nh, dot(t, h), dot(b, h), at, ab)
            * v_smith_aniso(at, ab, dot(t, v), dot(b, v), nv, dot(t, l), dot(b, l), nl);
    } else {
        spec_d = d_ggx(nh, a) * v_smith(nv, nl, a);
    }
    let f = f_schlick(f0, vh);
    // Напівпрозорість: обгорнутий дифуз (світло заходить за термінатор) і просвіт ззаду.
    let wrap = s.translucency * 0.5;
    let nl_wrap = clamp((nl_raw + wrap) / ((1.0 + wrap) * (1.0 + wrap)), 0.0, 1.0);
    var direct = (diffuse_color * (vec3<f32>(1.0) - f) / PI) * nl_wrap + f * spec_d * nl;
    let back = pow(clamp(dot(v, -l), 0.0, 1.0), 4.0) * s.translucency * 0.35;
    direct += diffuse_color * back;
    // Тканинний відблиск (матові, TPU).
    let sheen_color = mix(vec3<f32>(1.0), s.base, 0.5) * s.sheen;
    direct += sheen_color * d_charlie(rough, nh) * v_neubelt(nv, nl) * nl;
    // Лак.
    let ca = max(s.clearcoat_roughness * s.clearcoat_roughness, 0.002);
    let fc = f_schlick1(0.04, vh) * s.clearcoat;
    let coat = d_ggx(nh, ca) * (0.25 / max(vh * vh, 1e-3)) * fc;
    direct = direct * (1.0 - fc) + vec3<f32>(coat * nl);
    direct *= light * s.shadow;

    // --- оточення ---
    var bent = n;
    if s.anisotropy > 0.01 {
        // Зігнута нормаль (Filament): відблиск оточення тягнеться поперек дотичної.
        let aniso_t = cross(t, v);
        let aniso_n = cross(aniso_t, t);
        bent = normalize(mix(n, aniso_n, s.anisotropy * clamp(1.0 - rough * 0.5, 0.0, 1.0)));
    }
    let r = reflect(-v, bent);
    let env = globals.light_color.w;
    var irr = textureSampleLevel(irr_cube, cube_sampler, n, 0.0).rgb;
    if s.translucency > 0.0 {
        irr = mix(irr, textureSampleLevel(irr_cube, cube_sampler, -n, 0.0).rgb, s.translucency * 0.3);
    }
    let prefiltered = textureSampleLevel(spec_cube, cube_sampler, r, rough * SPEC_MIPS).rgb;
    let fr = env_brdf(f0, nv, rough);
    // Закривання відблиску (Lagarde): у западинах між шарами відблиск теж гасне.
    let spec_ao = clamp(pow(nv + s.ao, exp2(-16.0 * a - 1.0)) - 1.0 + s.ao, 0.0, 1.0);
    // Тінь ключового світла трохи гасить і оточення: софтбокс — частина того ж оточення.
    let env_shadow = mix(0.55, 1.0, s.shadow);
    var ambient = diffuse_color * irr * s.ao + prefiltered * fr * spec_ao * env_shadow;
    ambient += sheen_color * irr * 0.25 * s.ao;
    let fc_env = f_schlick1(0.04, nv) * s.clearcoat;
    let coat_env = textureSampleLevel(spec_cube, cube_sampler, reflect(-v, n), s.clearcoat_roughness * SPEC_MIPS).rgb;
    ambient = ambient * (1.0 - fc_env) + coat_env * fc_env * spec_ao * env_shadow;
    ambient *= env;

    return direct + ambient + s.base * s.emission * 2.0;
}

// ---- валики ----

struct BeadOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) world: vec3<f32>,
    @location(1) @interpolate(perspective, centroid) coord: vec3<f32>,
    @location(2) n_side: vec3<f32>,
    @location(3) n_front: vec3<f32>,
    @location(4) n_axis: vec3<f32>,
    @location(5) n_far: vec3<f32>,
    @location(6) @interpolate(flat) material: vec2<u32>,
    @location(7) @interpolate(flat) half: vec2<f32>,
}

@vertex
fn vs_bead(@builtin(vertex_index) vertex: u32) -> BeadOut {
    let b = bead_vertex(vertex, globals.view_proj, vec4<f32>(globals.eye.xyz, 0.0), globals.view);
    var out: BeadOut;
    out.clip = b.clip;
    out.world = b.world;
    out.coord = b.coord;
    out.n_side = b.n_side;
    out.n_front = b.n_front;
    out.n_axis = b.n_axis;
    out.n_far = b.n_far;
    out.material = b.material;
    out.half = b.half;
    return out;
}

// ---- профіль валика ----
//
// Переріз валика — сплющений stadium (Slic3r: прямокутник із півколами по краях), а сусідні
// шари й лінії зливаються: від «трубки» на поверхні лишається пологий горб, і між валиками —
// мілка канавка із заокругленим дном. Профіль задано кутом нормалі в площині перерізу
// як функцією положення `u` поперек валика (−1…1, ±1 — стик із сусіднім):
//   дуга кола з глибиною канавки `depth` (частка півшару): кут = asin(u / R),
//   R = (1 + depth²) / (2·depth); біля стику (|u| > PROFILE_FILLET) кут плавно повертається
//   до 0 — дно канавки заокруглене, нормаль на стику однакова з обох боків.
// CPU-дзеркало й тести — `photo/bead.rs::profile_angle`.

// Глибина канавки між шарами стінки (частка піввисоти шару): ≈ 0.022 мм на шарі 0.2 мм,
// порядку Rz бічної стінки FDM; нахил нормалі на схилі до ~19°.
const WALL_GROOVE: f32 = 0.22;
// Глибина канавки між лініями верхньої поверхні (частка півширини): лінії розплющені соплом.
const TOP_GROOVE: f32 = 0.12;
const PROFILE_FILLET: f32 = 0.78;

fn profile_angle(u: f32, depth: f32) -> f32 {
    let r = (1.0 + depth * depth) / (2.0 * depth);
    let a = abs(clamp(u, -1.0, 1.0));
    let arc = asin(min(a, PROFILE_FILLET) / r);
    let fillet = 1.0 - smoothstep(PROFILE_FILLET, 1.0, a);
    return sign(u) * arc * fillet;
}

// Скільки рельєфу лишити за розміром шару на екрані (пікселів на висоту/ширину валика):
// дрібніше за ~2 px рельєф не відтворити без муару — поверхня стає суцільною.
fn relief_lod(bead_px: f32) -> f32 {
    return smoothstep(1.6, 7.0, bead_px);
}

@fragment
fn fs_bead(in: BeadOut) -> @location(0) vec4<f32> {
    // Нормаль і похідні — до discard (похідні лише в рівномірному потоці керування).
    let footprint = fwidth(in.coord.x);
    let world_fw = length(fwidth(in.world));
    let v = normalize(globals.eye.xyz - in.world);
    let s_coord = clamp(in.coord.x, -1.0, 1.0);
    let c = clamp(in.coord.y - clamp(in.coord.y, 0.0, in.coord.z), -1.0, 1.0);
    let k = sqrt(max(1.0 - s_coord * s_coord - c * c, 0.0));
    let n_raw = in.n_side * s_coord + in.n_front * k + in.n_axis * c;
    // Еліптичний переріз (як у «Шарах»): справжня форма вільного валика.
    let n_ellipse = normalize(n_raw);

    // Базис перерізу: o — нормаль поверхні, яку валики утворюють разом (стінка — убік,
    // заповнення — угору), across — поперек валика в площині перерізу, fwd — уздовж.
    var o = normalize(in.n_far);
    let wall = abs(o.z) < 0.5;
    if wall {
        // Зблизька — бік за еліптичною нормаллю пікселя (зовнішня чи внутрішня половина
        // валика; на сходинках похилої поверхні камера бачить і дальню половину). Здалеку
        // вона мерехтить від пікселя до пікселя — тоді бік, який бачить камера, а коли
        // камера майже над валиком, усе одно головне — верх (top_cap).
        let side_view = dot(v, o);
        let near_px = relief_lod(2.0 / max(footprint, 1e-4)) > 0.5 || abs(side_view) < 0.25;
        let side_px = select(side_view, dot(n_ellipse, o), near_px);
        o = o * select(-1.0, 1.0, side_px >= 0.0);
    }
    let fwd = normalize(in.n_axis);
    let across = normalize(cross(o, fwd));
    // Положення поперек валика: −1…1 від стику до стику.
    let extent = select(in.half.x, in.half.y, wall);
    let u = clamp(dot(n_raw, across) * extent, -1.0, 1.0);
    let depth = select(TOP_GROOVE, WALL_GROOVE, wall);
    // Шар чи лінія на екрані: стрічка-імпостор має ширину 2 у coord.x.
    let lod = relief_lod(2.0 / max(footprint, 1e-4));
    let phi = profile_angle(u, depth) * lod;
    // Торець валика (кінець лінії) — теж рельєф: здалеку його нормаль зливається з поверхнею.
    let a_f = clamp(dot(n_ellipse, fwd), -1.0, 1.0) * lod;
    var n = normalize(fwd * a_f + (o * cos(phi) + across * sin(phi)) * sqrt(1.0 - a_f * a_f));
    // Верх стінки без шару над нею (останній шар, сходинки похилої поверхні, межа повзунка)
    // видно лише згори — там справжній еліптичний верх; збоку цю зону закриває валик вище,
    // тож профіль не страждає. Здалеку (lod → 0) зона верху менша за піксель і мерехтіла б,
    // тож замість неї — стала для валика нормаль, нахилена вгору на ~65°, з вагою 0.8:
    // приблизно середнє того, що від валика видно згори.
    let up_sign = select(-1.0, 1.0, across.z >= 0.0);
    let from_above = smoothstep(0.45, 0.85, abs(dot(v, across)));
    let cap_near = smoothstep(0.72, 0.95, u * up_sign);
    let top_cap = mix(0.8, cap_near, lod) * from_above * select(0.0, 1.0, wall);
    let cap_n = normalize(mix(normalize(o * 0.45 + across * up_sign), n_ellipse, lod));
    n = normalize(mix(n, cap_n, top_cap));
    // Спекулярне згладжування (Tokuyoshi–Kaplanyan): дисперсія нормалі в пікселі → шорсткість.
    let dndx = dpdx(n);
    let dndy = dpdy(n);
    let kernel = min(0.25 * (dot(dndx, dndx) + dot(dndy, dndy)), 0.18);
    // Рельєф, який LOD прибрав, теж розсіює відблиск (як карта нормалей із mip-рівнями).
    let removed = profile_angle(0.6, depth) * (1.0 - lod);
    let relief_var = 0.5 * removed * removed;
    if bead_outside(in.coord) {
        discard;
    }

    let m = materials[min(in.material.x, MAX_MATERIALS - 1u)];
    var s = default_surface(m.base.rgb, m.p0.x);
    s.metallic = m.p0.y;
    s.specular = m.p0.z;
    s.clearcoat = m.p0.w;
    s.clearcoat_roughness = m.p1.x;
    s.sheen = m.p1.y;
    s.anisotropy = m.p1.z;
    s.emission = m.p1.w;
    s.translucency = m.base.a;
    s.tangent = normalize(in.n_axis);

    s.roughness = sqrt(min(s.roughness * s.roughness + kernel + relief_var, 1.0));
    // Аналітичне затінення канавки: лише біля стику й пропорційно її глибині.
    let groove = 1.0 - 0.9 * depth * lod * smoothstep(0.55, 1.0, abs(u)) * (1.0 - top_cap);
    let near_floor = mix(0.7, 1.0, clamp((in.world.z - globals.floor.z) / 4.0, 0.0, 1.0));
    s.ao = groove * near_floor;

    let shadow = shadow_at(in.world, n);
    s.shadow = shadow;
    var sparkle = vec3<f32>(0.0);
    let pattern = u32(m.p2.x + 0.5);
    let scale = m.p2.y;
    let strength = m.p2.z;
    let p = in.world;
    if pattern == PATTERN_MARBLE {
        // Bambu PLA Marble — густі дрібні темні вкраплення (0.1–0.6 мм), круглі й розмиті, і
        // м’які прожилки. Вкраплення — кулі в комірках 3D-сітки: зріз площиною дає круглу
        // пляму; дрібніші за кілька пікселів зливаються в середній тон (без мерехтіння).
        let warp = fbm(p * 0.09 * scale) * 7.0;
        let vein = 1.0 - abs(sin(p.x * 0.21 * scale + p.y * 0.13 * scale + p.z * 0.17 * scale + warp));
        let veins = pow(vein, 6.0) * 0.35;
        let fine = speckle(p * 3.2 * scale, 0.7, 0.14, 0.3, world_fw * 3.2 * scale);
        let coarse = speckle(p * 1.1 * scale + 7.3, 0.12, 0.12, 0.24, world_fw * 1.1 * scale);
        let w = clamp(veins + fine * 0.55 + coarse * 0.75, 0.0, 1.0) * strength;
        s.base = mix(s.base, m.p3.rgb, w);
        s.roughness = mix(s.roughness, s.roughness * 0.85, w);
    } else if pattern == PATTERN_SPARKLE {
        // Пласкі лусочки ~0.15 мм з випадковим нахилом: спалахують, коли відбивають
        // світло або софтбокс. Дрібніші за піксель — гаснуть, інакше мерехтіли б.
        let q = p * 6.5 * scale;
        let cell = floor(q);
        let h = hash33(cell);
        let f = fract(q) - 0.5 - (h - 0.5) * 0.4;
        let fade = 1.0 - smoothstep(0.08, 0.25, world_fw * 6.5 * scale);
        if h.x < 0.4 && dot(f, f) < 0.1 {
            let tilt = hash33(cell + vec3<f32>(17.0, 5.0, 3.0)) - 0.5;
            let fn_ = normalize(n + tilt * 1.4);
            let r = reflect(-v, fn_);
            let direct = pow(clamp(dot(r, globals.light.xyz), 0.0, 1.0), 220.0) * 30.0 * shadow;
            let env = textureSampleLevel(spec_cube, cube_sampler, r, 0.0).rgb * 0.35;
            sparkle = m.p3.rgb * (globals.light_color.rgb * direct + env) * strength * fade;
        }
        // Далі блискітки зливаються в легкий «іній» на поверхні.
        s.base = mix(s.base, m.p3.rgb, 0.12 * strength * (1.0 - fade));
    } else if pattern == PATTERN_CARBON {
        // Короткі волокна: шум, витягнутий уздовж валика, змінює тон і шорсткість, плюс дрібне
        // темне «зерно» кінчиків волокон. Дрібніше за піксель — середнє (без мерехтіння).
        let t = s.tangent;
        let q = vec3<f32>(dot(p, t) * 1.2, dot(p, cross(t, vec3<f32>(0.0, 0.0, 1.0))) * 14.0, p.z * 14.0) * scale;
        let aa = 1.0 - smoothstep(0.3, 0.9, world_fw * 14.0 * scale);
        let fib = mix(0.5, vnoise(q) * 0.65 + vnoise(q * 2.7) * 0.35, aa);
        let grain = speckle(p * 6.0 * scale, 0.6, 0.12, 0.28, world_fw * 6.0 * scale);
        s.base *= mix(1.0, (0.75 + 0.5 * fib) * (1.0 - 0.6 * grain), strength);
        s.roughness = clamp(s.roughness + (0.5 - fib) * 0.25 * strength, 0.05, 1.0);
        s.sheen = s.sheen * (0.6 + fib * 0.8);
    } else if pattern == PATTERN_WOOD {
        // Смуги від шару до шару (температура сопла змінює відтінок) і повільні волокна.
        let band = fbm(vec3<f32>(p.x * 0.03, p.y * 0.03, p.z * 3.0) * scale);
        let grain = vnoise(vec3<f32>(p.x * 0.6, p.y * 0.6, p.z * 0.05) * scale);
        s.base *= 1.0 - strength * (band * 0.9 + grain * 0.25);
    }

    var alpha = 1.0;
    if (in.material.y & FLAG_SUPPORT) != 0u && globals.view.x == 1u {
        // Приглушені підтримки: світліші, матові й напівпрозорі (alpha-to-coverage).
        s.base = mix(s.base, vec3<f32>(0.75), 0.55);
        s.roughness = 0.9;
        s.clearcoat = 0.0;
        s.anisotropy = 0.0;
        alpha = 0.35;
    }

    let color = shade(n, v, s) + sparkle;
    return vec4<f32>(tonemap(color), alpha);
}

// ---- стіл і підлога ----

struct MeshOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) world: vec3<f32>,
    @location(1) normal: vec3<f32>,
}

@vertex
fn vs_mesh(@location(0) pos: vec3<f32>, @location(1) normal: vec3<f32>) -> MeshOut {
    var out: MeshOut;
    out.clip = globals.view_proj * vec4<f32>(pos, 1.0);
    out.world = pos;
    out.normal = normal;
    return out;
}

// Стіл: текстурована пластина PEI — дрібне зерно в нормалі й шорсткості.
@fragment
fn fs_bed(in: MeshOut, @builtin(front_facing) front: bool) -> @location(0) vec4<f32> {
    var n = normalize(in.normal);
    if !front {
        n = -n;
    }
    let v = normalize(globals.eye.xyz - in.world);
    let grain = vnoise(in.world * 4.0);
    let grain2 = vnoise(in.world * 11.0 + 5.0);
    if n.z > 0.5 {
        n = normalize(n + vec3<f32>(grain - 0.5, grain2 - 0.5, 0.0) * 0.12);
    }
    var s = default_surface(globals.bed_color.rgb * (0.9 + 0.2 * grain2), globals.bed_color.w + (grain - 0.5) * 0.1);
    s.specular = 0.45;
    s.shadow = shadow_at(in.world, n);
    s.ao = select(1.0, contact_ao(in.world), n.z > 0.5);
    return vec4<f32>(tonemap(shade(n, v, s)), 1.0);
}

// Нейтральна підлога: світло-сіра матова, з тінню й контактною тінню, далі зливається з фоном.
@fragment
fn fs_floor(in: MeshOut) -> @location(0) vec4<f32> {
    let n = vec3<f32>(0.0, 0.0, 1.0);
    let v = normalize(globals.eye.xyz - in.world);
    var s = default_surface(vec3<f32>(0.42), 0.62);
    s.specular = 0.35;
    s.shadow = shadow_at(in.world, n);
    s.ao = contact_ao(in.world);
    let lit = shade(n, v, s);
    let dist = length(in.world.xy - globals.floor.xy);
    let fade = smoothstep(globals.floor.w * 0.35, globals.floor.w, dist);
    let back = backdrop(-v) * globals.light_color.w;
    return vec4<f32>(tonemap(mix(lit, back, fade)), 1.0);
}

// ---- фон ----

struct ScreenOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) ndc: vec2<f32>,
}

@vertex
fn vs_screen(@builtin(vertex_index) vertex: u32) -> ScreenOut {
    let uv = vec2<f32>(f32((vertex << 1u) & 2u), f32(vertex & 2u));
    var out: ScreenOut;
    out.ndc = uv * 2.0 - 1.0;
    out.clip = vec4<f32>(out.ndc, 0.0, 1.0);
    return out;
}

@fragment
fn fs_background(in: ScreenOut) -> @location(0) vec4<f32> {
    // Обернена глибина: z = 1 — ближня площина, 0.5 — далі.
    let near = globals.inv_view_proj * vec4<f32>(in.ndc, 1.0, 1.0);
    let far = globals.inv_view_proj * vec4<f32>(in.ndc, 0.5, 1.0);
    let d = normalize(far.xyz / far.w - near.xyz / near.w);
    return vec4<f32>(tonemap(backdrop(d) * globals.light_color.w), 1.0);
}

// ---- переміщення ----

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
    return vec4<f32>(in.color.rgb * 0.85, 1.0);
}
