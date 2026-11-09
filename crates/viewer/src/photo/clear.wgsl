// Прозорі філаменти (пресет з `transmission` > 0). Спереду дописуються env.wgsl, bead.wgsl і
// photo.wgsl (див. photo.rs). Порядок у кадрі «Фото» з прозорими:
//   1. `vs_thick`/`fs_thick` — товщина прозорого пластику вздовж кожного променя ока:
//      сума шляхів крізь усі прозорі валики на промені (зовнішні й внутрішні стінки,
//      заповнення), адитивно в R16F на півроздільності;
//   2. попередні проходи (photo.wgsl, `fs_bead_prepass`): трасована глибина найближчих
//      непрозорих і найближчих прозорих валиків на півроздільності;
//   3. непрозорі валики, стіл і фон — як завжди (`vs_bead_opaque`, справжня глибина);
//   4. `fs_clear` — лише найближча прозора поверхня (за глибиною попереднього проходу; що
//      за непрозорим — відкидається так само): відбиття + світло, розсіяне в
//      об’ємі, а те, що за деталлю, множиться на пропускання e^(−σ·товщина) змішуванням
//      (dual-source: колір + множник на канал; без нього — сірий множник і поправка відтінку).
//      Товщина — середнє 2×2 текселів (ціль на півроздільності), не менше власного валика.
// Внутрішні периметри й заповнення додають товщини, тож просвічують темнішими й густішими
// смугами — без сортування й без окремої геометрії.

@group(2) @binding(0) var thickness_map: texture_2d<f32>;

@vertex
fn vs_bead_clear(@builtin(vertex_index) vertex: u32) -> BeadOut {
    return bead_out(vertex, BEADS_CLEAR);
}

@vertex
fn vs_thick(@builtin(vertex_index) vertex: u32) -> BeadOut {
    return bead_out(vertex, BEADS_CLEAR);
}

// Прозорі валики сусідніх шарів раніше стояли стрічками на одній глибині й змішувались
// кілька разів; трасовані — один раз. Товщина × 1.12 повертає той самий тон (звірено на
// кубику PETG Translucent).
const THICK_GAIN: f32 = 1.12;

@fragment
fn fs_thick(in: BeadOut) -> @location(0) vec4<f32> {
    // Та сама трасована капсула, що й у кольорі: лише пікселі, які вона закриває.
    let hit = bead_hit(in);
    if hit.coverage <= 0.0 {
        discard;
    }
    // Гладкий профіль поперек валика (середнє 1, як у прямокутника): сусідні валики на
    // екрані перекриваються, і «коробки» давали б на півроздільності смугасту решітку.
    // `facing` — частка хорди кругового перерізу на цій відстані від осі.
    return vec4<f32>(in.chord * THICK_GAIN * mix(1.0, hit.facing * 1.2732395, 0.6) * hit.coverage, 0.0, 0.0, 0.0);
}

struct ClearShaded {
    // Власне світло поверхні (відбиття, розсіяння), до тонмапінгу.
    color: vec3<f32>,
    // Частка того, що за деталлю, яка проходить крізь неї (на канал).
    transmit: vec3<f32>,
    v: vec3<f32>,
    // Трасована глибина (`frag_depth`): між прозорими лишається найближчий.
    depth: f32,
}

fn clear_shade(in: BeadOut) -> ClearShaded {
    let b = bead_shading(in, true);
    let m = materials[min(in.material.x, MAX_MATERIALS - 1u)];
    let sigma_a = m.p4.rgb;
    let sigma_s = m.p3.w;
    let transmission = m.p2.w;
    let reference = m.p4.w;
    // Товщина вздовж променя: усі прозорі валики позаду пікселя разом із цим.
    // Ціль товщини — на півроздільності: середнє 2×2 сусідніх текселів гасить решітку від
    // дрібних валиків, а на силуеті (де середнє змішане з порожнечею) товщина не менша за
    // шлях крізь сам цей валик.
    let uv = in.clip.xy * globals.screen.xy;
    let quad = textureGather(0, thickness_map, cube_sampler, uv);
    let d = max(max(dot(quad, vec4<f32>(0.25)), in.chord), 0.05);

    var s = b.s;
    s.tint = transmit_at(b.pos);
    // Відбиття (пряме й оточення, лак) — той самий BSDF без дифузу.
    var mirror = s;
    mirror.base = vec3<f32>(0.0);
    mirror.sheen = 0.0;
    mirror.translucency = 0.0;
    mirror.emission = 0.0;
    let reflection = shade(b.n, b.v, mirror);
    // Що пройшло крізь межу (решта — відбилось).
    let nv = clamp(dot(b.n, b.v), 1e-4, 1.0);
    let enter = (1.0 - f_schlick1(0.08 * s.specular, nv)) * (1.0 - f_schlick1(0.04, nv) * s.clearcoat);

    // Освітлення всередині: оточення з обох боків (світло заходить і крізь дальню стінку) і
    // ключове світло без різкого термінатора.
    let l = globals.light.xyz;
    let irr = 0.5 * (textureSampleLevel(irr_cube, cube_sampler, b.n, 0.0).rgb
        + textureSampleLevel(irr_cube, cube_sampler, -b.n, 0.0).rgb);
    let key = globals.light_color.rgb * globals.light.w * s.shadow * s.tint;
    let inside = irr * globals.light_color.w * s.ao + key * (0.3 + 0.7 * clamp(dot(b.n, l) * 0.5 + 0.5, 0.0, 1.0)) / PI;
    // Розсіяне в об’ємі світло пройшло в середньому пів товщини туди й назад: колір густішає
    // з товщиною. Кількість розсіяного — 1 − e^(−σs·d) (каламутність).
    // Розсіяне здебільшого біля поверхні, звідки світло й виходить: шлях — чверть товщини.
    let path = 0.35 * d + 0.25 * reference;
    let scattered = inside * exp(-sigma_a * path) * (1.0 - exp(-sigma_s * d)) * 1.2;
    // Контрове світло крізь деталь до ока (ключове світло позаду).
    let through = key * exp(-sigma_a * d) * pow(clamp(dot(b.v, -l), 0.0, 1.0), 6.0) * 0.6;
    // Непрозора частка — звичайний дифуз кольором пластику.
    let diffuse = inside * s.base;

    var out: ClearShaded;
    out.color = reflection + enter * (transmission * (scattered + through) + (1.0 - transmission) * diffuse);
    out.transmit = enter * transmission * exp(-(sigma_a + sigma_s) * d);
    if (in.material.y & FLAG_SUPPORT) != 0u && globals.view.x == 1u {
        // Приглушені підтримки: світліші й прозоріші.
        out.color = mix(out.color, vec3<f32>(0.6) * globals.light_color.w, 0.4) * b.alpha;
        out.transmit = mix(vec3<f32>(1.0), out.transmit, b.alpha);
    }
    out.v = b.v;
    out.depth = b.depth;
    return out;
}

struct ClearAlphaOut {
    @location(0) color: vec4<f32>,
    @builtin(frag_depth) depth: f32,
}

// Без dual-source blending: сірий множник (альфа) і поправка відтінку за фоном студії.
//@early_depth_test(less_equal)
@fragment
fn fs_clear_alpha(in: BeadOut) -> ClearAlphaOut {
    let c = clear_shade(in);
    let gray = (c.transmit.r + c.transmit.g + c.transmit.b) / 3.0;
    let behind = tonemap(backdrop(-c.v) * globals.light_color.w);
    var out: ClearAlphaOut;
    out.color = vec4<f32>(tonemap(c.color) + (c.transmit - gray) * behind, 1.0 - gray);
    out.depth = c.depth;
    return out;
}
