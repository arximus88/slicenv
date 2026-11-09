// Запікання IBL «Фото» на GPU при вході в режим (до env.wgsl дописується спереду):
//   cs_env        — процедурне оточення в кубічну карту (рівень 0);
//   cs_downsample — наступний mip-рівень оточення (середнє 2×2);
//   cs_prefilter  — попередньо розмита карта відблиску GGX для шорсткості рівня;
//   cs_irradiance — косинусна згортка (дифузне освітлення).

struct Params {
    // x — розмір грані цілі, y — шорсткість (prefilter), z — кількість вибірок, w — розмір грані джерела.
    size: vec4<f32>,
}

@group(0) @binding(0) var<uniform> params: Params;
@group(0) @binding(1) var target_tex: texture_storage_2d_array<rgba16float, write>;
@group(0) @binding(2) var source_cube: texture_cube<f32>;
@group(0) @binding(3) var source_sampler: sampler;
@group(0) @binding(4) var source_array: texture_2d_array<f32>;

fn texel_dir(id: vec3<u32>) -> vec3<f32> {
    let size = params.size.x;
    let uv = (vec2<f32>(id.xy) + 0.5) / size;
    return cube_dir(id.z, uv);
}

@compute @workgroup_size(8, 8, 1)
fn cs_env(@builtin(global_invocation_id) id: vec3<u32>) {
    if f32(id.x) >= params.size.x || f32(id.y) >= params.size.x {
        return;
    }
    // 2×2 вибірки на тексель — без драбинки на краях софтбоксів.
    let size = params.size.x;
    var sum = vec3<f32>(0.0);
    for (var i = 0u; i < 4u; i++) {
        let o = vec2<f32>(f32(i & 1u), f32(i >> 1u)) * 0.5 + 0.25;
        sum += environment(cube_dir(id.z, (vec2<f32>(id.xy) + o) / size));
    }
    textureStore(target_tex, id.xy, id.z, vec4<f32>(sum * 0.25, 1.0));
}

@compute @workgroup_size(8, 8, 1)
fn cs_downsample(@builtin(global_invocation_id) id: vec3<u32>) {
    if f32(id.x) >= params.size.x || f32(id.y) >= params.size.x {
        return;
    }
    let p = vec2<i32>(id.xy) * 2;
    let layer = i32(id.z);
    let c = textureLoad(source_array, p, layer, 0) + textureLoad(source_array, p + vec2<i32>(1, 0), layer, 0)
        + textureLoad(source_array, p + vec2<i32>(0, 1), layer, 0) + textureLoad(source_array, p + vec2<i32>(1, 1), layer, 0);
    textureStore(target_tex, id.xy, id.z, c * 0.25);
}

fn radical_inverse(bits_in: u32) -> f32 {
    var bits = bits_in;
    bits = (bits << 16u) | (bits >> 16u);
    bits = ((bits & 0x55555555u) << 1u) | ((bits & 0xAAAAAAAAu) >> 1u);
    bits = ((bits & 0x33333333u) << 2u) | ((bits & 0xCCCCCCCCu) >> 2u);
    bits = ((bits & 0x0F0F0F0Fu) << 4u) | ((bits & 0xF0F0F0F0u) >> 4u);
    bits = ((bits & 0x00FF00FFu) << 8u) | ((bits & 0xFF00FF00u) >> 8u);
    return f32(bits) * 2.3283064365386963e-10;
}

fn tangent_frame(n: vec3<f32>) -> mat3x3<f32> {
    let up = select(vec3<f32>(1.0, 0.0, 0.0), vec3<f32>(0.0, 0.0, 1.0), abs(n.z) < 0.999);
    let t = normalize(cross(up, n));
    let b = cross(n, t);
    return mat3x3<f32>(t, b, n);
}

// Filtered importance sampling (Křivánek, Colbert): рівень mip за густиною вибірки,
// тож навіть 64 вибірки дають рівне розмиття без шуму.
@compute @workgroup_size(8, 8, 1)
fn cs_prefilter(@builtin(global_invocation_id) id: vec3<u32>) {
    if f32(id.x) >= params.size.x || f32(id.y) >= params.size.x {
        return;
    }
    let n = texel_dir(id);
    let roughness = params.size.y;
    let a = roughness * roughness;
    let count = u32(params.size.z);
    let frame = tangent_frame(n);
    let source_size = params.size.w;
    let texel_solid_angle = 4.0 * PI / (6.0 * source_size * source_size);
    var sum = vec3<f32>(0.0);
    var weight = 0.0;
    for (var i = 0u; i < count; i++) {
        let xi = vec2<f32>(f32(i) / f32(count), radical_inverse(i));
        let phi = 2.0 * PI * xi.x;
        let cos_t = sqrt((1.0 - xi.y) / (1.0 + (a * a - 1.0) * xi.y));
        let sin_t = sqrt(1.0 - cos_t * cos_t);
        let h = frame * vec3<f32>(sin_t * cos(phi), sin_t * sin(phi), cos_t);
        let l = 2.0 * dot(n, h) * h - n;
        let nl = dot(n, l);
        if nl > 0.0 {
            let nh = max(dot(n, h), 0.0);
            let d2 = nh * nh * (a * a - 1.0) + 1.0;
            let d = a * a / (PI * d2 * d2);
            let pdf = d * 0.25 + 1e-4;
            let sample_solid_angle = 1.0 / (f32(count) * pdf);
            let lod = select(0.5 * log2(sample_solid_angle / texel_solid_angle) + 1.0, 0.0, roughness == 0.0);
            sum += textureSampleLevel(source_cube, source_sampler, l, lod).rgb * nl;
            weight += nl;
        }
    }
    textureStore(target_tex, id.xy, id.z, vec4<f32>(sum / max(weight, 1e-4), 1.0));
}

@compute @workgroup_size(8, 8, 1)
fn cs_irradiance(@builtin(global_invocation_id) id: vec3<u32>) {
    if f32(id.x) >= params.size.x || f32(id.y) >= params.size.x {
        return;
    }
    let n = texel_dir(id);
    let frame = tangent_frame(n);
    let count = u32(params.size.z);
    var sum = vec3<f32>(0.0);
    for (var i = 0u; i < count; i++) {
        // Косинусний розподіл: pdf = cos/π, тож середнє вибірок і є опроміненням / π.
        let xi = vec2<f32>(f32(i) / f32(count), radical_inverse(i));
        let r = sqrt(xi.y);
        let phi = 2.0 * PI * xi.x;
        let l = frame * vec3<f32>(r * cos(phi), r * sin(phi), sqrt(max(1.0 - xi.y, 0.0)));
        sum += textureSampleLevel(source_cube, source_sampler, l, 4.0).rgb;
    }
    // Зберігаємо E/π: шейдер множить на альбедо без ділення.
    textureStore(target_tex, id.xy, id.z, vec4<f32>(sum / f32(count), 1.0));
}
