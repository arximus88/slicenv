// Процедурна студія «Фото»: градієнт фону + софтбокси. Вісь Z — вгору, як у сцені.
// Яскравість лінійна HDR (1.0 — «біле» після тонмапінгу). Один і той самий опис
// запікається в кубічну карту (ibl.wgsl) і малює фон (photo.wgsl, лише `backdrop`).

const PI: f32 = 3.14159265;

// Напрямок до ключового світла: ліворуч, трохи спереду, згори (камера типово дивиться з −Y),
// тож світлотінь окреслює форму, а не б’є в лоб.
// Те саме число — `KEY_LIGHT` у photo.rs (тіньова карта).
const KEY_DIR: vec3<f32> = vec3<f32>(-0.6991266, -0.2996257, 0.6491890);

// Безшовний студійний фон: темний низ, світліший горизонт, м’який верх.
fn backdrop(d: vec3<f32>) -> vec3<f32> {
    let z = d.z;
    let top = vec3<f32>(0.30, 0.31, 0.33);
    let horizon = vec3<f32>(0.20, 0.205, 0.215);
    let ground = vec3<f32>(0.075, 0.074, 0.072);
    let up = mix(horizon, top, smoothstep(0.0, 0.9, z));
    let down = mix(horizon * 0.8, ground, smoothstep(0.0, -0.35, z));
    return select(down, up, z >= 0.0);
}

// Прямокутний софтбокс: центр `c` (одиничний), осі `t`, `b`, напіврозміри в тангенсах,
// м’який край `soft`. Гномонічна проєкція: прямокутник на площині, дотичній у центрі.
fn softbox(d: vec3<f32>, c: vec3<f32>, t: vec3<f32>, b: vec3<f32>, half_size: vec2<f32>, soft: f32) -> f32 {
    let k = dot(d, c);
    if k <= 0.05 {
        return 0.0;
    }
    let uv = vec2<f32>(dot(d, t), dot(d, b)) / k;
    let q = abs(uv) / half_size;
    let edge = max(q.x, q.y);
    // Трохи яскравіший центр, як у справжнього софтбокса з дифузором.
    let falloff = 1.0 - 0.25 * dot(q, q);
    return (1.0 - smoothstep(1.0 - soft, 1.0, edge)) * max(falloff, 0.0);
}

fn basis_t(c: vec3<f32>) -> vec3<f32> {
    return normalize(cross(vec3<f32>(0.0, 0.0, 1.0), c));
}

fn environment(d: vec3<f32>) -> vec3<f32> {
    var color = backdrop(d);
    // Ключовий: великий, теплуватий.
    let kt = basis_t(KEY_DIR);
    let kb = cross(KEY_DIR, kt);
    color += vec3<f32>(1.0, 0.97, 0.92) * 5.5 * softbox(d, KEY_DIR, kt, kb, vec2<f32>(0.42, 0.32), 0.25);
    // Заповнювальний праворуч: висока вузька смуга, холодніша й слабша.
    let fill = normalize(vec3<f32>(0.85, -0.35, 0.38));
    let ft = basis_t(fill);
    let fb = cross(fill, ft);
    color += vec3<f32>(0.85, 0.92, 1.0) * 2.2 * softbox(d, fill, ft, fb, vec2<f32>(0.14, 0.6), 0.3);
    // Контровий позаду: окреслює силует.
    let rim = normalize(vec3<f32>(0.25, 0.9, 0.42));
    let rt = basis_t(rim);
    let rb = cross(rim, rt);
    color += vec3<f32>(1.0) * 3.0 * softbox(d, rim, rt, rb, vec2<f32>(0.55, 0.1), 0.35);
    // Стельова панель.
    let top = vec3<f32>(0.0, 0.0, 1.0);
    color += vec3<f32>(1.0) * 1.1 * softbox(d, top, vec3<f32>(1.0, 0.0, 0.0), vec3<f32>(0.0, 1.0, 0.0), vec2<f32>(0.7, 0.45), 0.5);
    return color;
}

// Напрямок у світі для текселя грані кубічної карти (порядок граней WebGPU: +X −X +Y −Y +Z −Z).
fn cube_dir(face: u32, uv: vec2<f32>) -> vec3<f32> {
    let u = uv.x * 2.0 - 1.0;
    let v = uv.y * 2.0 - 1.0;
    switch face {
        case 0u: { return normalize(vec3<f32>(1.0, -v, -u)); }
        case 1u: { return normalize(vec3<f32>(-1.0, -v, u)); }
        case 2u: { return normalize(vec3<f32>(u, 1.0, v)); }
        case 3u: { return normalize(vec3<f32>(u, -1.0, -v)); }
        case 4u: { return normalize(vec3<f32>(u, -v, 1.0)); }
        default: { return normalize(vec3<f32>(-u, -v, -1.0)); }
    }
}
