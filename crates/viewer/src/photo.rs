//! Режим Preview «Фото»: реалістичний вигляд надрукованого виробу (етап 1 —
//! інтерактивний растровий PBR). Архітектура й таблиця матеріалів — docs/PHOTO-MODE.md.
//!
//! Ресурси (запечене IBL, тіньові карти, пайплайни) створюються лише при вході в режим
//! ([`Renderer::set_photo`](crate::Renderer::set_photo)) і звільняються при виході, тож
//! звичайний Preview нічого не платить. Кадр малюється в ті самі цілі рендерера
//! (sRGB, MSAA ×4, обернена глибина), тонмапінг — у фрагментному шейдері.

#[path = "photo/bead.rs"]
pub mod bead;
#[path = "photo/material.rs"]
pub mod material;

use std::{
    ops::Range,
    sync::Mutex,
    time::{Duration, Instant},
};

use bytemuck::{Pod, Zeroable};
use glam::{Mat4, Vec3};
use wgpu::util::DeviceExt as _;

use super::{COLOR_FORMAT, DEPTH_FORMAT, FlatVertex, MeshVertex, SAMPLES, bed};
use crate::{Camera, Mesh};
pub use material::{MaterialTable, Pattern, PhotoMaterial, Preset};

/// Слотів філаменту в уніформі матеріалів (`MAX_MATERIALS` у photo.wgsl).
pub const MAX_MATERIALS: usize = 32;
/// Напрямок до ключового світла (`KEY_DIR` у env.wgsl).
pub const KEY_LIGHT: Vec3 = Vec3::new(-0.699_126_6, -0.299_625_7, 0.649_189);

const ENV_SIZE: u32 = 256;
const ENV_MIPS: u32 = 9;
const SPEC_SIZE: u32 = 128;
/// Рівні шорсткості 0, 0.2 … 1 (`SPEC_MIPS` у photo.wgsl = 5 — останній індекс).
const SPEC_MIPS: u32 = 6;
const IRR_SIZE: u32 = 32;
const SHADOW_SIZE: u32 = 2048;
const OCCUPANCY_SIZE: u32 = 512;
/// Карта пропускання прозорих від ключового світла (кольорова тінь).
const TRANSMIT_SIZE: u32 = 1024;
const TRANSMIT_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;
const TRANSMIT_NEAR_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::R16Float;
/// Товщина прозорого вздовж променя ока (мм), на півроздільності кадру.
const THICKNESS_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::R16Float;
/// Розсіяння σs = `haze` · це / `absorption_distance` (1/мм).
const HAZE_SCALE: f32 = 2.0;
const HDR_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;
const SHADOW_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;

/// Як показувати підтримки.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SupportView {
    /// Тим самим матеріалом, що й модель.
    Shown,
    /// Світліші, матові й напівпрозорі.
    #[default]
    Dimmed,
    Hidden,
}

impl SupportView {
    fn code(self) -> u32 {
        match self {
            Self::Shown => 0,
            Self::Dimmed => 1,
            Self::Hidden => 2,
        }
    }
}

/// На чому стоїть виріб.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Stage {
    /// Стіл активної пластини з тим самим виглядом, що в Preview (модель і написи профілю).
    #[default]
    Bed,
    /// Нейтральна світло-сіра підлога, що зливається з фоном.
    Floor,
}

/// Налаштування режиму «Фото» з застосунку.
#[derive(Clone, Debug, PartialEq)]
pub struct PhotoSettings {
    /// Матеріал кожного слота філаменту (`Segment::tool`); бракує — останній або типовий.
    pub materials: Vec<PhotoMaterial>,
    pub supports: SupportView,
    pub stage: Stage,
    pub travel: bool,
    pub exposure: f32,
}

impl Default for PhotoSettings {
    fn default() -> Self {
        Self {
            materials: vec![PhotoMaterial::default()],
            supports: SupportView::default(),
            stage: Stage::default(),
            travel: false,
            exposure: 1.0,
        }
    }
}

fn srgb_to_linear(c: f32) -> f32 {
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct GpuMaterial {
    base: [f32; 4],
    p0: [f32; 4],
    p1: [f32; 4],
    p2: [f32; 4],
    p3: [f32; 4],
    p4: [f32; 4],
}

impl From<&PhotoMaterial> for GpuMaterial {
    fn from(m: &PhotoMaterial) -> Self {
        let lin = |c: [f32; 3]| c.map(srgb_to_linear);
        // Для прозорих — пропускання шару `absorption_distance` (з компенсацією пастельних).
        let [r, g, b] = lin(m.shading_color());
        let [pr, pg, pb] = lin(m.pattern_color);
        let [ar, ag, ab] = m.absorption();
        let scatter = if m.is_transparent() { m.haze * HAZE_SCALE / m.absorption_distance } else { 0.0 };
        Self {
            base: [r, g, b, m.translucency],
            p0: [m.roughness, m.metallic, m.specular, m.clearcoat],
            p1: [m.clearcoat_roughness, m.sheen, m.anisotropy, m.emission],
            p2: [m.pattern.id() as f32, m.pattern_scale, m.pattern_strength, m.transmission],
            p3: [pr, pg, pb, scatter],
            p4: [ar, ag, ab, m.absorption_distance],
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Globals {
    view_proj: [[f32; 4]; 4],
    eye: [f32; 4],
    inv_view_proj: [[f32; 4]; 4],
    shadow: [[f32; 4]; 4],
    occupancy: [[f32; 4]; 4],
    light: [f32; 4],
    light_color: [f32; 4],
    view: [u32; 4],
    floor: [f32; 4],
    params: [f32; 4],
    params2: [f32; 4],
    bed_color: [f32; 4],
    clear: [u32; 4],
    screen: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct ShadowPass {
    view_proj: [[f32; 4]; 4],
    dir: [f32; 4],
    view: [u32; 4],
    /// Фільтр валиків (`BEADS_*` у bead.wgsl) і маска прозорих слотів.
    pick: [u32; 4],
}

/// Фільтр валиків проходу (`BEADS_*` у bead.wgsl).
const BEADS_ALL: u32 = 0;
const BEADS_OPAQUE: u32 = 1;
const BEADS_CLEAR: u32 = 2;

/// Ціль товщини прозорого під поточний розмір кадру.
struct Thickness {
    size: (u32, u32),
    view: wgpu::TextureView,
    bind: wgpu::BindGroup,
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct DecalGlobals {
    view_proj: [[f32; 4]; 4],
    eye: [f32; 4],
}

/// Проєкції тіньової карти й карти висот для рамки траєкторії.
#[derive(Clone, Copy, Debug, PartialEq)]
struct LightRig {
    shadow: Mat4,
    occupancy: Mat4,
    /// Тексель тіні в мм.
    texel_mm: f32,
    /// Висота, що відповідає глибині 0 карти висот, і діапазон глибини (мм).
    occupancy_top: f32,
    occupancy_depth: f32,
    /// Розмір карти висот у мм (x, y).
    occupancy_mm: [f32; 2],
}

/// NDC → UV текстури: x вправо, y вниз (рядок 0 — верх кадру), глибина як є.
fn uv_bias() -> Mat4 {
    Mat4::from_cols_array(&[
        0.5, 0.0, 0.0, 0.0, //
        0.0, -0.5, 0.0, 0.0, //
        0.0, 0.0, 1.0, 0.0, //
        0.5, 0.5, 0.0, 1.0,
    ])
}

impl LightRig {
    fn new(lo: Vec3, hi: Vec3) -> (Self, Mat4, Mat4) {
        let center = (lo + hi) * 0.5;
        let radius = ((hi - lo) * 0.5).length() + 2.0;
        let back = radius * 2.0 + 10.0;
        let eye = center + KEY_LIGHT * back;
        let view = Mat4::look_at_rh(eye, center, Vec3::Z);
        // Тінь найвищої точки падає далі за сферу — дальня площина з запасом на неї.
        let far = back + radius + (hi.z - lo.z).max(1.0) / KEY_LIGHT.z + 20.0;
        let proj = Mat4::orthographic_rh(-radius, radius, -radius, radius, 0.1, far);
        let shadow_vp = proj * view;

        // Згори: XY рамки з запасом під радіус контактної тіні.
        let margin = 25.0;
        let half = ((hi - lo).truncate() * 0.5 + margin).max(glam::Vec2::splat(5.0));
        let top_eye = Vec3::new(center.x, center.y, hi.z + 10.0);
        let near = 1.0;
        let top_far = top_eye.z - (lo.z.min(0.0) - 5.0);
        let top_view = Mat4::look_at_rh(top_eye, center.truncate().extend(lo.z), Vec3::Y);
        let top_proj = Mat4::orthographic_rh(-half.x, half.x, -half.y, half.y, near, top_far);
        let occupancy_vp = top_proj * top_view;
        (
            Self {
                shadow: uv_bias() * shadow_vp,
                occupancy: uv_bias() * occupancy_vp,
                texel_mm: 2.0 * radius / SHADOW_SIZE as f32,
                occupancy_top: top_eye.z - near,
                occupancy_depth: top_far - near,
                occupancy_mm: [2.0 * half.x, 2.0 * half.y],
            },
            shadow_vp,
            occupancy_vp,
        )
    }
}

/// Що зараз у тіньових картах: перемальовуються лише при зміні.
#[derive(Clone, PartialEq)]
struct ShadowKey {
    buffer: wgpu::Buffer,
    visible: Range<u32>,
    supports: SupportView,
    bounds: [f32; 6],
    /// Матеріали (від прозорості й кольору залежить кольорова тінь).
    materials: Vec<[u32; 4]>,
}

/// Валики для одного проходу: прив’язка шматка й діапазон вершин у ньому.
pub(super) type BeadDraws<'a> = Vec<(&'a wgpu::BindGroup, Range<u32>)>;

/// Дані кадру від рендерера для основного проходу.
pub(super) struct PhotoFrame<'a> {
    /// Від ближніх шарів до дальніх.
    pub draws: BeadDraws<'a>,
    pub travel: Option<(&'a wgpu::Buffer, Range<u32>)>,
    pub decals: &'a bed::Decals,
}

/// Що потрібно для уніформ кадру й ключа тіньових карт.
pub(super) struct PhotoView<'a> {
    pub camera: &'a Camera,
    pub width: u32,
    pub height: u32,
    pub instance_buffer: Option<&'a wgpu::Buffer>,
    /// Видимі інстанси (для заокруглених торців на межах повзунків і ключа тіні).
    pub visible: Range<u32>,
    pub bounds: Option<(Vec3, Vec3)>,
    /// Зсув активної пластини, на якій лежать написи столу.
    pub bed_shift: Vec3,
}

pub(crate) struct Photo {
    settings: PhotoSettings,
    globals: wgpu::Buffer,
    materials: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
    bead_pipeline: wgpu::RenderPipeline,
    bed_pipeline: wgpu::RenderPipeline,
    floor_pipeline: wgpu::RenderPipeline,
    background_pipeline: wgpu::RenderPipeline,
    line_pipeline: wgpu::RenderPipeline,
    shadow_pipeline: wgpu::RenderPipeline,
    /// Непрозорі валики, коли є й прозорі.
    bead_opaque_pipeline: wgpu::RenderPipeline,
    /// Прозорі: глибина найближчих, потім колір зі змішуванням.
    clear_depth_pipeline: wgpu::RenderPipeline,
    clear_pipeline: wgpu::RenderPipeline,
    thickness_pipeline: wgpu::RenderPipeline,
    thickness_layout: wgpu::BindGroupLayout,
    thickness: Option<Thickness>,
    transmit_pipeline: wgpu::RenderPipeline,
    transmit_pass: wgpu::Buffer,
    transmit_bind: wgpu::BindGroup,
    transmit_view: wgpu::TextureView,
    transmit_near_view: wgpu::TextureView,
    decal_pipeline: wgpu::RenderPipeline,
    decal_globals: wgpu::Buffer,
    decal_bind: wgpu::BindGroup,
    shadow_pass: wgpu::Buffer,
    shadow_bind: wgpu::BindGroup,
    occupancy_pass: wgpu::Buffer,
    occupancy_bind: wgpu::BindGroup,
    shadow_view: wgpu::TextureView,
    occupancy_view: wgpu::TextureView,
    bed_mesh: Option<(wgpu::Buffer, u32)>,
    floor_mesh: Option<(wgpu::Buffer, u32, [f32; 4])>,
    shadow_key: Mutex<Option<ShadowKey>>,
    /// Байти текстур і буферів режиму (оцінка VRAM).
    bytes: u64,
    bake_time: Duration,
}

/// Вихід прозорого з dual-source blending: колір і множник того, що за деталлю, на канал.
const CLEAR_DUAL_SOURCE: &str = "
struct ClearOut {
    @location(0) @blend_src(0) color: vec4<f32>,
    @location(0) @blend_src(1) transmit: vec4<f32>,
}

@fragment
fn fs_clear(in: BeadOut) -> ClearOut {
    let c = clear_shade(in);
    var out: ClearOut;
    out.color = vec4<f32>(tonemap(c.color), 1.0);
    out.transmit = vec4<f32>(c.transmit, 0.0);
    return out;
}
";

/// Біт на кожен прозорий слот; слоти без власного матеріалу беруть останній.
fn transparent_mask(materials: &[PhotoMaterial]) -> u32 {
    let last = materials.last().is_some_and(PhotoMaterial::is_transparent);
    (0..MAX_MATERIALS).fold(0, |mask, i| {
        let clear = materials.get(i).map_or(last, PhotoMaterial::is_transparent);
        mask | (u32::from(clear) << i)
    })
}

/// Ключ матеріалу для тіньових карт (бітово).
fn gpu_key(m: &PhotoMaterial) -> [u32; 4] {
    let g = GpuMaterial::from(m);
    [g.p2[3], g.p4[0], g.p4[1], g.p4[2]].map(f32::to_bits)
}

/// Збирає шейдер із частин: спільний код спереду.
fn module(device: &wgpu::Device, label: &str, parts: &[&str]) -> wgpu::ShaderModule {
    device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some(label),
        source: wgpu::ShaderSource::Wgsl(parts.concat().into()),
    })
}

fn texture_entry(binding: u32, dimension: wgpu::TextureViewDimension, sample: wgpu::TextureSampleType) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture {
            sample_type: sample,
            view_dimension: dimension,
            multisampled: false,
        },
        count: None,
    }
}

fn uniform_entry(binding: u32, visibility: wgpu::ShaderStages) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Uniform,
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    }
}

fn uniform_buffer(device: &wgpu::Device, label: &str, size: usize) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some(label),
        size: size as u64,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

/// Кубічна HDR-карта з `mips` рівнями, у яку пишуть обчислювальні шейдери.
fn cube_texture(device: &wgpu::Device, label: &str, size: u32, mips: u32) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d {
            width: size,
            height: size,
            depth_or_array_layers: 6,
        },
        mip_level_count: mips,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: HDR_FORMAT,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::STORAGE_BINDING,
        view_formats: &[],
    })
}

fn cube_bytes(size: u32, mips: u32) -> u64 {
    (0..mips)
        .map(|m| u64::from((size >> m).max(1)).pow(2) * 6 * 8)
        .sum()
}

fn mip_view(texture: &wgpu::Texture, mip: u32, dimension: wgpu::TextureViewDimension) -> wgpu::TextureView {
    texture.create_view(&wgpu::TextureViewDescriptor {
        dimension: Some(dimension),
        base_mip_level: mip,
        mip_level_count: Some(1),
        ..Default::default()
    })
}

fn cube_view(texture: &wgpu::Texture) -> wgpu::TextureView {
    texture.create_view(&wgpu::TextureViewDescriptor {
        dimension: Some(wgpu::TextureViewDimension::Cube),
        ..Default::default()
    })
}

/// IBL з процедурної студії: оточення → mip-рівні → розмиті відблиски й опромінення.
fn bake_ibl(device: &wgpu::Device, queue: &wgpu::Queue) -> (wgpu::Texture, wgpu::Texture) {
    let shader = module(
        device,
        "photo_ibl",
        &[include_str!("photo/env.wgsl"), include_str!("photo/ibl.wgsl")],
    );
    let pipeline = |entry: &str| {
        device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some(entry),
            layout: None,
            module: &shader,
            entry_point: Some(entry),
            compilation_options: Default::default(),
            cache: None,
        })
    };
    let (env_p, down_p, pre_p, irr_p) = (
        pipeline("cs_env"),
        pipeline("cs_downsample"),
        pipeline("cs_prefilter"),
        pipeline("cs_irradiance"),
    );
    let env = cube_texture(device, "photo_env", ENV_SIZE, ENV_MIPS);
    let spec = cube_texture(device, "photo_specular", SPEC_SIZE, SPEC_MIPS);
    let irr = cube_texture(device, "photo_irradiance", IRR_SIZE, 1);
    let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some("photo_ibl"),
        mag_filter: wgpu::FilterMode::Linear,
        min_filter: wgpu::FilterMode::Linear,
        mipmap_filter: wgpu::MipmapFilterMode::Linear,
        ..Default::default()
    });
    let env_cube = cube_view(&env);
    let mut encoder = device.create_command_encoder(&Default::default());
    // Кожна диспетчеризація має власний буфер параметрів: усі вони в одному поданні.
    let params = |v: [f32; 4]| {
        device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("photo_ibl_params"),
            contents: bytemuck::cast_slice(&v),
            usage: wgpu::BufferUsages::UNIFORM,
        })
    };
    let dispatch = |encoder: &mut wgpu::CommandEncoder,
                    pipeline: &wgpu::ComputePipeline,
                    entries: &[wgpu::BindGroupEntry<'_>],
                    size: u32| {
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("photo_ibl"),
            layout: &pipeline.get_bind_group_layout(0),
            entries,
        });
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("photo_ibl"),
            timestamp_writes: None,
        });
        pass.set_pipeline(pipeline);
        pass.set_bind_group(0, &bind, &[]);
        pass.dispatch_workgroups(size.div_ceil(8), size.div_ceil(8), 6);
    };

    let p = params([ENV_SIZE as f32, 0.0, 0.0, 0.0]);
    let target = mip_view(&env, 0, wgpu::TextureViewDimension::D2Array);
    dispatch(
        &mut encoder,
        &env_p,
        &[
            wgpu::BindGroupEntry { binding: 0, resource: p.as_entire_binding() },
            wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&target) },
        ],
        ENV_SIZE,
    );
    for mip in 1..ENV_MIPS {
        let size = (ENV_SIZE >> mip).max(1);
        let p = params([size as f32, 0.0, 0.0, 0.0]);
        let source = mip_view(&env, mip - 1, wgpu::TextureViewDimension::D2Array);
        let target = mip_view(&env, mip, wgpu::TextureViewDimension::D2Array);
        dispatch(
            &mut encoder,
            &down_p,
            &[
                wgpu::BindGroupEntry { binding: 0, resource: p.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&target) },
                wgpu::BindGroupEntry { binding: 4, resource: wgpu::BindingResource::TextureView(&source) },
            ],
            size,
        );
    }
    for mip in 0..SPEC_MIPS {
        let size = (SPEC_SIZE >> mip).max(1);
        let roughness = mip as f32 / (SPEC_MIPS - 1) as f32;
        let samples = if mip == 0 { 1.0 } else { 96.0 };
        let p = params([size as f32, roughness, samples, ENV_SIZE as f32]);
        let target = mip_view(&spec, mip, wgpu::TextureViewDimension::D2Array);
        dispatch(
            &mut encoder,
            &pre_p,
            &[
                wgpu::BindGroupEntry { binding: 0, resource: p.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&target) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(&env_cube) },
                wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::Sampler(&sampler) },
            ],
            size,
        );
    }
    let p = params([IRR_SIZE as f32, 0.0, 512.0, ENV_SIZE as f32]);
    let target = mip_view(&irr, 0, wgpu::TextureViewDimension::D2Array);
    dispatch(
        &mut encoder,
        &irr_p,
        &[
            wgpu::BindGroupEntry { binding: 0, resource: p.as_entire_binding() },
            wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&target) },
            wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(&env_cube) },
            wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::Sampler(&sampler) },
        ],
        IRR_SIZE,
    );
    queue.submit([encoder.finish()]);
    // Карта оточення потрібна лише для запікання: після подання її можна відпустити —
    // wgpu тримає ресурс, доки GPU не закінчить.
    (spec, irr)
}

/// Колірна ціль, яку потім читає шейдер.
fn color_texture(device: &wgpu::Device, label: &str, (width, height): (u32, u32), format: wgpu::TextureFormat) -> wgpu::TextureView {
    device
        .create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        })
        .create_view(&Default::default())
}

fn depth_texture(device: &wgpu::Device, label: &str, size: u32) -> wgpu::TextureView {
    device
        .create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d {
                width: size,
                height: size,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: SHADOW_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        })
        .create_view(&Default::default())
}

impl Photo {
    pub(super) fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        toolpath_layout: &wgpu::BindGroupLayout,
        decals: &bed::Decals,
        settings: PhotoSettings,
    ) -> Self {
        let started = Instant::now();
        let (spec, irr) = bake_ibl(device, queue);
        let shadow_view = depth_texture(device, "photo_shadow", SHADOW_SIZE);
        let occupancy_view = depth_texture(device, "photo_occupancy", OCCUPANCY_SIZE);
        let transmit_size = (TRANSMIT_SIZE, TRANSMIT_SIZE);
        let transmit_view = color_texture(device, "photo_transmit", transmit_size, TRANSMIT_FORMAT);
        let transmit_near_view = color_texture(device, "photo_transmit_near", transmit_size, TRANSMIT_NEAR_FORMAT);
        let dual_source = device.features().contains(wgpu::Features::DUAL_SOURCE_BLENDING);

        let globals = uniform_buffer(device, "photo_globals", size_of::<Globals>());
        let materials = uniform_buffer(device, "photo_materials", size_of::<GpuMaterial>() * MAX_MATERIALS);
        let float = wgpu::TextureSampleType::Float { filterable: true };
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("photo"),
            entries: &[
                uniform_entry(0, wgpu::ShaderStages::VERTEX_FRAGMENT),
                uniform_entry(1, wgpu::ShaderStages::FRAGMENT),
                texture_entry(2, wgpu::TextureViewDimension::Cube, float),
                texture_entry(3, wgpu::TextureViewDimension::Cube, float),
                wgpu::BindGroupLayoutEntry {
                    binding: 4,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                texture_entry(5, wgpu::TextureViewDimension::D2, wgpu::TextureSampleType::Depth),
                wgpu::BindGroupLayoutEntry {
                    binding: 6,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Comparison),
                    count: None,
                },
                texture_entry(7, wgpu::TextureViewDimension::D2, wgpu::TextureSampleType::Depth),
                texture_entry(8, wgpu::TextureViewDimension::D2, float),
                texture_entry(9, wgpu::TextureViewDimension::D2, float),
            ],
        });
        let cube_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("photo_cube"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Linear,
            ..Default::default()
        });
        let shadow_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("photo_shadow"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            compare: Some(wgpu::CompareFunction::LessEqual),
            ..Default::default()
        });
        let (spec_view, irr_view) = (cube_view(&spec), cube_view(&irr));
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("photo"),
            layout: &layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: globals.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: materials.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(&spec_view) },
                wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::TextureView(&irr_view) },
                wgpu::BindGroupEntry { binding: 4, resource: wgpu::BindingResource::Sampler(&cube_sampler) },
                wgpu::BindGroupEntry { binding: 5, resource: wgpu::BindingResource::TextureView(&shadow_view) },
                wgpu::BindGroupEntry { binding: 6, resource: wgpu::BindingResource::Sampler(&shadow_sampler) },
                wgpu::BindGroupEntry { binding: 7, resource: wgpu::BindingResource::TextureView(&occupancy_view) },
                wgpu::BindGroupEntry { binding: 8, resource: wgpu::BindingResource::TextureView(&transmit_view) },
                wgpu::BindGroupEntry { binding: 9, resource: wgpu::BindingResource::TextureView(&transmit_near_view) },
            ],
        });

        let shader = module(
            device,
            "photo",
            &[
                include_str!("photo/env.wgsl"),
                include_str!("photo/bead.wgsl"),
                include_str!("photo/photo.wgsl"),
            ],
        );
        let bead_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("photo_beads"),
            bind_group_layouts: &[Some(&layout), Some(toolpath_layout)],
            ..Default::default()
        });
        // Стіл, підлога, фон і лінії не читають траєкторію: група 1 їм не потрібна.
        let scene_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("photo_scene"),
            bind_group_layouts: &[Some(&layout)],
            ..Default::default()
        });
        let mesh_attrs = wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x3];
        let flat_attrs = wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x4];
        struct Spec<'a> {
            label: &'a str,
            vs: &'a str,
            fs: &'a str,
            buffer: Option<wgpu::VertexBufferLayout<'a>>,
            topology: wgpu::PrimitiveTopology,
            depth: bool,
            coverage: bool,
        }
        let pipeline = |s: Spec<'_>| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(s.label),
                layout: Some(if s.vs.starts_with("vs_bead") { &bead_layout } else { &scene_layout }),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some(s.vs),
                    compilation_options: Default::default(),
                    buffers: &[s.buffer],
                },
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some(s.fs),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: COLOR_FORMAT,
                        blend: None,
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                primitive: wgpu::PrimitiveState {
                    topology: s.topology,
                    ..Default::default()
                },
                depth_stencil: Some(wgpu::DepthStencilState {
                    format: DEPTH_FORMAT,
                    depth_write_enabled: Some(s.depth),
                    depth_compare: Some(if s.depth {
                        wgpu::CompareFunction::GreaterEqual
                    } else {
                        wgpu::CompareFunction::Always
                    }),
                    stencil: Default::default(),
                    bias: Default::default(),
                }),
                multisample: wgpu::MultisampleState {
                    count: SAMPLES,
                    alpha_to_coverage_enabled: s.coverage,
                    ..Default::default()
                },
                multiview_mask: None,
                cache: None,
            })
        };
        let mesh_layout = || wgpu::VertexBufferLayout {
            array_stride: size_of::<MeshVertex>() as u64,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &mesh_attrs,
        };
        let tri = wgpu::PrimitiveTopology::TriangleList;
        let bead_pipeline = pipeline(Spec { label: "photo_bead", vs: "vs_bead", fs: "fs_bead", buffer: None, topology: tri, depth: true, coverage: true });
        let bead_opaque_pipeline = pipeline(Spec { label: "photo_bead_opaque", vs: "vs_bead_opaque", fs: "fs_bead", buffer: None, topology: tri, depth: true, coverage: true });
        let bed_pipeline = pipeline(Spec { label: "photo_bed", vs: "vs_mesh", fs: "fs_bed", buffer: Some(mesh_layout()), topology: tri, depth: true, coverage: false });
        let floor_pipeline = pipeline(Spec { label: "photo_floor", vs: "vs_mesh", fs: "fs_floor", buffer: Some(mesh_layout()), topology: tri, depth: true, coverage: false });
        let background_pipeline = pipeline(Spec { label: "photo_background", vs: "vs_screen", fs: "fs_background", buffer: None, topology: tri, depth: false, coverage: false });
        let line_pipeline = pipeline(Spec {
            label: "photo_lines",
            vs: "vs_line",
            fs: "fs_line",
            buffer: Some(wgpu::VertexBufferLayout {
                array_stride: size_of::<FlatVertex>() as u64,
                step_mode: wgpu::VertexStepMode::Vertex,
                attributes: &flat_attrs,
            }),
            topology: wgpu::PrimitiveTopology::LineList,
            depth: true,
            coverage: false,
        });

        // Тіньові проходи: лише глибина, стандартна (не обернена), з нахиловим зсувом.
        let pass_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("photo_shadow_pass"),
            entries: &[uniform_entry(0, wgpu::ShaderStages::VERTEX), uniform_entry(1, wgpu::ShaderStages::FRAGMENT)],
        });
        let shadow_shader = module(
            device,
            "photo_shadow",
            &[include_str!("photo/bead.wgsl"), include_str!("photo/shadow.wgsl")],
        );
        let shadow_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("photo_shadow"),
            bind_group_layouts: &[Some(&pass_layout), Some(toolpath_layout)],
            ..Default::default()
        });
        let shadow_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("photo_shadow"),
            layout: Some(&shadow_layout),
            vertex: wgpu::VertexState {
                module: &shadow_shader,
                entry_point: Some("vs_shadow"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shadow_shader,
                entry_point: Some("fs_shadow"),
                compilation_options: Default::default(),
                targets: &[],
            }),
            primitive: Default::default(),
            depth_stencil: Some(wgpu::DepthStencilState {
                format: SHADOW_FORMAT,
                depth_write_enabled: Some(true),
                depth_compare: Some(wgpu::CompareFunction::Less),
                stencil: Default::default(),
                bias: wgpu::DepthBiasState {
                    constant: 2,
                    slope_scale: 1.5,
                    clamp: 0.0,
                },
            }),
            multisample: Default::default(),
            multiview_mask: None,
            cache: None,
        });
        let pass = |label| {
            let buffer = uniform_buffer(device, label, size_of::<ShadowPass>());
            let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some(label),
                layout: &pass_layout,
                entries: &[
                    wgpu::BindGroupEntry { binding: 0, resource: buffer.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 1, resource: materials.as_entire_binding() },
                ],
            });
            (buffer, bind)
        };
        let (shadow_pass, shadow_bind) = pass("photo_shadow_pass");
        let (occupancy_pass, occupancy_bind) = pass("photo_occupancy_pass");
        let (transmit_pass, transmit_bind) = pass("photo_transmit_pass");
        // Пропускання: оптична густина додається, найдальша (альфа) і найближча глибини —
        // максимум і мінімум.
        let add = wgpu::BlendComponent {
            src_factor: wgpu::BlendFactor::One,
            dst_factor: wgpu::BlendFactor::One,
            operation: wgpu::BlendOperation::Add,
        };
        let extreme = |operation| wgpu::BlendComponent { operation, ..add };
        let transmit_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("photo_transmit"),
            layout: Some(&shadow_layout),
            vertex: wgpu::VertexState {
                module: &shadow_shader,
                entry_point: Some("vs_transmit"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shadow_shader,
                entry_point: Some("fs_transmit"),
                compilation_options: Default::default(),
                targets: &[
                    Some(wgpu::ColorTargetState {
                        format: TRANSMIT_FORMAT,
                        blend: Some(wgpu::BlendState { color: add, alpha: extreme(wgpu::BlendOperation::Max) }),
                        write_mask: wgpu::ColorWrites::ALL,
                    }),
                    Some(wgpu::ColorTargetState {
                        format: TRANSMIT_NEAR_FORMAT,
                        blend: Some(wgpu::BlendState {
                            color: extreme(wgpu::BlendOperation::Min),
                            alpha: extreme(wgpu::BlendOperation::Min),
                        }),
                        write_mask: wgpu::ColorWrites::RED,
                    }),
                ],
            }),
            primitive: Default::default(),
            depth_stencil: None,
            multisample: Default::default(),
            multiview_mask: None,
            cache: None,
        });

        // Прозорі. Модуль із dual-source — лише коли пристрій його вміє (директива `enable`
        // інакше не скомпілюється).
        let clear_parts = [
            include_str!("photo/env.wgsl"),
            include_str!("photo/bead.wgsl"),
            include_str!("photo/photo.wgsl"),
            include_str!("photo/clear.wgsl"),
        ];
        let clear_shader = if dual_source {
            let mut parts = vec!["enable dual_source_blending;\n"];
            parts.extend(clear_parts);
            parts.push(CLEAR_DUAL_SOURCE);
            module(device, "photo_clear", &parts)
        } else {
            module(device, "photo_clear", &clear_parts)
        };
        let thickness_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("photo_thickness"),
            entries: &[texture_entry(0, wgpu::TextureViewDimension::D2, float)],
        });
        let clear_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("photo_clear"),
            bind_group_layouts: &[Some(&layout), Some(toolpath_layout), Some(&thickness_layout)],
            ..Default::default()
        });
        struct Clear<'a> {
            label: &'a str,
            layout: &'a wgpu::PipelineLayout,
            vs: &'a str,
            fs: &'a str,
            target: wgpu::ColorTargetState,
            depth: Option<(bool, wgpu::CompareFunction)>,
            samples: u32,
        }
        let clear_pipeline_of = |c: Clear<'_>| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(c.label),
                layout: Some(c.layout),
                vertex: wgpu::VertexState {
                    module: &clear_shader,
                    entry_point: Some(c.vs),
                    compilation_options: Default::default(),
                    buffers: &[],
                },
                fragment: Some(wgpu::FragmentState {
                    module: &clear_shader,
                    entry_point: Some(c.fs),
                    compilation_options: Default::default(),
                    targets: &[Some(c.target)],
                }),
                primitive: Default::default(),
                depth_stencil: c.depth.map(|(write, compare)| wgpu::DepthStencilState {
                    format: DEPTH_FORMAT,
                    depth_write_enabled: Some(write),
                    depth_compare: Some(compare),
                    stencil: Default::default(),
                    bias: Default::default(),
                }),
                multisample: wgpu::MultisampleState { count: c.samples, ..Default::default() },
                multiview_mask: None,
                cache: None,
            })
        };
        let frame_target = |blend, write_mask| wgpu::ColorTargetState { format: COLOR_FORMAT, blend, write_mask };
        let thickness_pipeline = clear_pipeline_of(Clear {
            label: "photo_thickness",
            layout: &bead_layout,
            vs: "vs_thick",
            fs: "fs_thick",
            target: wgpu::ColorTargetState {
                format: THICKNESS_FORMAT,
                blend: Some(wgpu::BlendState { color: add, alpha: add }),
                write_mask: wgpu::ColorWrites::RED,
            },
            depth: None,
            samples: 1,
        });
        let clear_depth_pipeline = clear_pipeline_of(Clear {
            label: "photo_clear_depth",
            layout: &clear_layout,
            vs: "vs_bead_clear",
            fs: "fs_bead_depth",
            target: frame_target(None, wgpu::ColorWrites::empty()),
            depth: Some((true, wgpu::CompareFunction::GreaterEqual)),
            samples: SAMPLES,
        });
        // Лише найближча прозора поверхня: глибина рівна записаній попереднім проходом.
        let clear_blend = if dual_source {
            let over = wgpu::BlendComponent {
                src_factor: wgpu::BlendFactor::One,
                dst_factor: wgpu::BlendFactor::Src1,
                operation: wgpu::BlendOperation::Add,
            };
            wgpu::BlendState { color: over, alpha: over }
        } else {
            wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING
        };
        let clear_pipeline = clear_pipeline_of(Clear {
            label: "photo_clear",
            layout: &clear_layout,
            vs: "vs_bead_clear",
            fs: if dual_source { "fs_clear" } else { "fs_clear_alpha" },
            target: frame_target(Some(clear_blend), wgpu::ColorWrites::ALL),
            depth: Some((false, wgpu::CompareFunction::GreaterEqual)),
            samples: SAMPLES,
        });

        // Написи столу — тим самим шейдером, що в Preview, але у свою прив’язку globals
        // (матриця зсунута на активну пластину, яскравість приглушена).
        let decal_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("photo_decals"),
            entries: &[uniform_entry(0, wgpu::ShaderStages::VERTEX_FRAGMENT)],
        });
        let decal_pipeline = decals.pipeline(device, &decal_layout);
        let decal_globals = uniform_buffer(device, "photo_decals", size_of::<DecalGlobals>());
        let decal_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("photo_decals"),
            layout: &decal_layout,
            entries: &[wgpu::BindGroupEntry { binding: 0, resource: decal_globals.as_entire_binding() }],
        });

        let bytes = cube_bytes(SPEC_SIZE, SPEC_MIPS)
            + cube_bytes(IRR_SIZE, 1)
            + u64::from(SHADOW_SIZE).pow(2) * 4
            + u64::from(OCCUPANCY_SIZE).pow(2) * 4
            + u64::from(TRANSMIT_SIZE).pow(2) * (8 + 2)
            + (size_of::<Globals>() + size_of::<GpuMaterial>() * MAX_MATERIALS) as u64;
        // Чекаємо запікання: вхід у режим — одноразова пауза в кілька мс, а час потрібен для
        // звіту продуктивності.
        let _ = device.poll(wgpu::PollType::wait_indefinitely());
        let photo = Self {
            settings: settings.clone(),
            globals,
            materials,
            bind_group,
            bead_pipeline,
            bed_pipeline,
            floor_pipeline,
            background_pipeline,
            line_pipeline,
            shadow_pipeline,
            bead_opaque_pipeline,
            clear_depth_pipeline,
            clear_pipeline,
            thickness_pipeline,
            thickness_layout,
            thickness: None,
            transmit_pipeline,
            transmit_pass,
            transmit_bind,
            transmit_view,
            transmit_near_view,
            decal_pipeline,
            decal_globals,
            decal_bind,
            shadow_pass,
            shadow_bind,
            occupancy_pass,
            occupancy_bind,
            shadow_view,
            occupancy_view,
            bed_mesh: None,
            floor_mesh: None,
            shadow_key: Mutex::new(None),
            bytes,
            bake_time: started.elapsed(),
        };
        photo.write_materials(queue);
        photo
    }

    pub(super) fn bake_time(&self) -> Duration {
        self.bake_time
    }

    /// Оцінка VRAM ресурсів режиму, байти (без цілей кадру — вони спільні з Preview).
    pub(super) fn bytes(&self) -> u64 {
        self.bytes
            + self.thickness.as_ref().map_or(0, |t| u64::from(t.size.0) * u64::from(t.size.1) * 2)
            + self.bed_mesh.as_ref().map_or(0, |m| m.0.size())
            + self.floor_mesh.as_ref().map_or(0, |m| m.0.size())
    }

    pub(super) fn settings(&self) -> &PhotoSettings {
        &self.settings
    }

    pub(super) fn set_settings(&mut self, queue: &wgpu::Queue, settings: PhotoSettings) {
        if settings == self.settings {
            return;
        }
        self.settings = settings;
        self.write_materials(queue);
    }

    /// Біт на кожен прозорий слот (`Segment::tool`); слоти без власного матеріалу беруть
    /// останній (як `write_materials`).
    fn transparent_mask(&self) -> u32 {
        transparent_mask(&self.settings.materials)
    }

    /// Чи є в кадрі прозорі (окремі проходи товщини й пропускання).
    pub(super) fn has_transparent(&self) -> bool {
        self.transparent_mask() != 0
    }

    /// Ціль товщини під розмір кадру (півроздільність); без прозорих — звільняється.
    fn ensure_thickness(&mut self, device: &wgpu::Device, width: u32, height: u32) {
        if !self.has_transparent() {
            self.thickness = None;
            return;
        }
        let size = (width.div_ceil(2).max(1), height.div_ceil(2).max(1));
        if self.thickness.as_ref().is_some_and(|t| t.size == size) {
            return;
        }
        let view = color_texture(device, "photo_thickness", size, THICKNESS_FORMAT);
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("photo_thickness"),
            layout: &self.thickness_layout,
            entries: &[wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&view) }],
        });
        self.thickness = Some(Thickness { size, view, bind });
    }

    fn write_materials(&self, queue: &wgpu::Queue) {
        let fallback = self.settings.materials.last().copied().unwrap_or_default();
        let data: Vec<GpuMaterial> = (0..MAX_MATERIALS)
            .map(|i| GpuMaterial::from(self.settings.materials.get(i).unwrap_or(&fallback)))
            .collect();
        queue.write_buffer(&self.materials, 0, bytemuck::cast_slice(&data));
    }

    /// Модель столу активної пластини (як у Preview) зі зсувом `offset`.
    pub(super) fn set_bed(&mut self, device: &wgpu::Device, body: &Mesh, offset: Vec3) {
        let vertices: Vec<MeshVertex> = body
            .positions
            .iter()
            .zip(&body.normals)
            .map(|(p, n)| MeshVertex {
                pos: (Vec3::from(*p) + offset).to_array(),
                normal: *n,
            })
            .collect();
        self.bed_mesh = (!vertices.is_empty()).then(|| {
            (
                device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("photo_bed"),
                    contents: bytemuck::cast_slice(&vertices),
                    usage: wgpu::BufferUsages::VERTEX,
                }),
                vertices.len() as u32,
            )
        });
    }

    /// Підлога під рамкою траєкторії: квадрат, що далі зливається з фоном.
    fn ensure_floor(&mut self, device: &wgpu::Device, bounds: Option<(Vec3, Vec3)>) -> [f32; 4] {
        let (lo, hi) = bounds.unwrap_or((Vec3::ZERO, Vec3::new(crate::BED_SIZE, crate::BED_SIZE, 0.0)));
        let center = (lo + hi) * 0.5;
        let size = (hi - lo).truncate().max_element().max(20.0);
        let fade = size * 3.0 + 150.0;
        let key = [center.x, center.y, -0.01, fade];
        if self.floor_mesh.as_ref().is_some_and(|f| f.2 == key) {
            return key;
        }
        let r = fade * 1.2;
        let corner = |x: f32, y: f32| MeshVertex {
            pos: [center.x + x * r, center.y + y * r, key[2]],
            normal: [0.0, 0.0, 1.0],
        };
        let quad = [
            corner(-1., -1.),
            corner(1., -1.),
            corner(1., 1.),
            corner(-1., -1.),
            corner(1., 1.),
            corner(-1., 1.),
        ];
        self.floor_mesh = Some((
            device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("photo_floor"),
                contents: bytemuck::cast_slice(&quad),
                usage: wgpu::BufferUsages::VERTEX,
            }),
            6,
            key,
        ));
        key
    }

    /// Готує уніформи кадру (і за потреби підлогу) — до кодування. `true` — тіньові карти
    /// застаріли й їх треба перемалювати в цьому кадрі.
    pub(super) fn prepare(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, frame: &PhotoView<'_>) -> bool {
        let floor = self.ensure_floor(device, frame.bounds);
        self.ensure_thickness(device, frame.width, frame.height);
        let mask = self.transparent_mask();
        let aspect = frame.width as f32 / frame.height.max(1) as f32;
        let view_proj = frame.camera.view_proj(aspect);
        let eye = frame.camera.eye();
        let (lo, hi) = frame
            .bounds
            .map(|(lo, hi)| (lo - Vec3::new(1.0, 1.0, 0.5), hi + Vec3::splat(1.0)))
            .unwrap_or((Vec3::ZERO, Vec3::new(crate::BED_SIZE, crate::BED_SIZE, 1.0)));
        let (rig, shadow_vp, occupancy_vp) = LightRig::new(lo, hi);
        let visible_last = frame.visible.end.saturating_sub(1);
        let view = [
            self.settings.supports.code(),
            frame.visible.start,
            visible_last,
            u32::from(self.settings.stage == Stage::Bed),
        ];
        let globals = Globals {
            view_proj: view_proj.to_cols_array_2d(),
            eye: [eye.x, eye.y, eye.z, 0.0],
            inv_view_proj: view_proj.inverse().to_cols_array_2d(),
            shadow: rig.shadow.to_cols_array_2d(),
            occupancy: rig.occupancy.to_cols_array_2d(),
            light: [KEY_LIGHT.x, KEY_LIGHT.y, KEY_LIGHT.z, 3.2],
            light_color: [1.0, 0.97, 0.92, 1.0],
            view,
            floor,
            params: [self.settings.exposure, 1.0 / SHADOW_SIZE as f32, rig.texel_mm, rig.occupancy_top],
            params2: [rig.occupancy_depth, rig.occupancy_mm[0], rig.occupancy_mm[1], 0.0],
            bed_color: [0.05, 0.05, 0.056, 0.55],
            clear: [mask, 0, 0, 0],
            screen: [1.0 / frame.width.max(1) as f32, 1.0 / frame.height.max(1) as f32, 0.0, 0.0],
        };
        queue.write_buffer(&self.globals, 0, bytemuck::bytes_of(&globals));
        let decal = DecalGlobals {
            view_proj: (view_proj * Mat4::from_translation(-frame.bed_shift)).to_cols_array_2d(),
            // w — яскравість написів: вони не освітлюються, тож приглушені під тонмапінг.
            eye: [eye.x, eye.y, eye.z, 0.45],
        };
        queue.write_buffer(&self.decal_globals, 0, bytemuck::bytes_of(&decal));

        let key = frame.instance_buffer.map(|buffer| ShadowKey {
            buffer: buffer.clone(),
            visible: frame.visible.clone(),
            supports: self.settings.supports,
            bounds: [lo.x, lo.y, lo.z, hi.x, hi.y, hi.z],
            materials: self.settings.materials.iter().map(gpu_key).collect(),
        });
        let mut current = self.shadow_key.lock().unwrap_or_else(|e| e.into_inner());
        if *current == key && key.is_some() {
            return false;
        }
        *current = key;
        let pass = |vp: Mat4, dir: Vec3, beads: u32| ShadowPass {
            view_proj: vp.to_cols_array_2d(),
            dir: [dir.x, dir.y, dir.z, 1.0],
            view,
            pick: [beads, mask, 0, 0],
        };
        // З прозорими тіньова карта — лише від непрозорих; прозорі дають карту пропускання.
        let shadow_beads = if mask == 0 { BEADS_ALL } else { BEADS_OPAQUE };
        queue.write_buffer(&self.shadow_pass, 0, bytemuck::bytes_of(&pass(shadow_vp, KEY_LIGHT, shadow_beads)));
        queue.write_buffer(&self.occupancy_pass, 0, bytemuck::bytes_of(&pass(occupancy_vp, Vec3::Z, BEADS_ALL)));
        queue.write_buffer(&self.transmit_pass, 0, bytemuck::bytes_of(&pass(shadow_vp, KEY_LIGHT, BEADS_CLEAR)));
        true
    }

    /// Тіньова карта й карта висот (лише коли змінились траєкторія, видимі шари чи підтримки).
    pub(super) fn encode_shadows(&self, encoder: &mut wgpu::CommandEncoder, draws: &BeadDraws<'_>) {
        for (view, bind) in [
            (&self.shadow_view, &self.shadow_bind),
            (&self.occupancy_view, &self.occupancy_bind),
        ] {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("photo_shadow"),
                color_attachments: &[],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                ..Default::default()
            });
            pass.set_pipeline(&self.shadow_pipeline);
            pass.set_bind_group(0, bind, &[]);
            for (chunk, range) in draws {
                pass.set_bind_group(1, *chunk, &[]);
                pass.draw(range.clone(), 0..1);
            }
        }
        if !self.has_transparent() {
            return;
        }
        let target = |view, clear| {
            Some(wgpu::RenderPassColorAttachment {
                view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations { load: wgpu::LoadOp::Clear(clear), store: wgpu::StoreOp::Store },
            })
        };
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("photo_transmit"),
            color_attachments: &[
                target(&self.transmit_view, wgpu::Color::TRANSPARENT),
                target(&self.transmit_near_view, wgpu::Color::WHITE),
            ],
            ..Default::default()
        });
        pass.set_pipeline(&self.transmit_pipeline);
        pass.set_bind_group(0, &self.transmit_bind, &[]);
        for (chunk, range) in draws {
            pass.set_bind_group(1, *chunk, &[]);
            pass.draw(range.clone(), 0..1);
        }
    }

    /// Товщина прозорого вздовж променів ока (перед основним проходом, щокадру).
    pub(super) fn encode_thickness(&self, encoder: &mut wgpu::CommandEncoder, draws: &BeadDraws<'_>) {
        let Some(thickness) = &self.thickness else {
            return;
        };
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("photo_thickness"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &thickness.view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT), store: wgpu::StoreOp::Store },
            })],
            ..Default::default()
        });
        pass.set_pipeline(&self.thickness_pipeline);
        pass.set_bind_group(0, &self.bind_group, &[]);
        for (chunk, range) in draws {
            pass.set_bind_group(1, *chunk, &[]);
            pass.draw(range.clone(), 0..1);
        }
    }

    /// Основний прохід у вже відкритий render pass рендерера.
    pub(super) fn draw(&self, pass: &mut wgpu::RenderPass<'_>, frame: &PhotoFrame<'_>) {
        pass.set_bind_group(0, &self.bind_group, &[]);
        pass.set_pipeline(&self.background_pipeline);
        pass.draw(0..3, 0..1);

        match self.settings.stage {
            Stage::Bed => {
                if let Some((buffer, count)) = &self.bed_mesh {
                    pass.set_pipeline(&self.bed_pipeline);
                    pass.set_vertex_buffer(0, buffer.slice(..));
                    pass.draw(0..*count, 0..1);
                }
                pass.set_bind_group(0, &self.decal_bind, &[]);
                frame.decals.draw_with(pass, &self.decal_pipeline, false);
                pass.set_bind_group(0, &self.bind_group, &[]);
            }
            Stage::Floor => {
                if let Some((buffer, count, _)) = &self.floor_mesh {
                    pass.set_pipeline(&self.floor_pipeline);
                    pass.set_vertex_buffer(0, buffer.slice(..));
                    pass.draw(0..*count, 0..1);
                }
            }
        }

        let clear = self.thickness.as_ref().filter(|_| self.has_transparent());
        if !frame.draws.is_empty() {
            pass.set_pipeline(if clear.is_some() { &self.bead_opaque_pipeline } else { &self.bead_pipeline });
            for (chunk, range) in &frame.draws {
                pass.set_bind_group(1, *chunk, &[]);
                pass.draw(range.clone(), 0..1);
            }
        }
        if let Some(thickness) = clear
            && !frame.draws.is_empty()
        {
            // Спершу глибина найближчих прозорих валиків, потім колір лише для них: інакше
            // дальній валик, намальований раніше за ближчий, змішався б двічі.
            pass.set_bind_group(2, &thickness.bind, &[]);
            for pipeline in [&self.clear_depth_pipeline, &self.clear_pipeline] {
                pass.set_pipeline(pipeline);
                for (chunk, range) in &frame.draws {
                    pass.set_bind_group(1, *chunk, &[]);
                    pass.draw(range.clone(), 0..1);
                }
            }
        }
        if self.settings.travel
            && let Some((buffer, range)) = &frame.travel
            && !range.is_empty()
        {
            pass.set_pipeline(&self.line_pipeline);
            pass.set_vertex_buffer(0, buffer.slice(..));
            pass.draw(range.clone(), 0..1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn light_rig_maps_bounds_inside_both_maps() {
        let (lo, hi) = (Vec3::new(100.0, 90.0, 0.0), Vec3::new(160.0, 150.0, 80.0));
        let (rig, _, _) = LightRig::new(lo, hi);
        for i in 0..8 {
            let p = Vec3::new(
                if i & 1 == 0 { lo.x } else { hi.x },
                if i & 2 == 0 { lo.y } else { hi.y },
                if i & 4 == 0 { lo.z } else { hi.z },
            );
            let s = rig.shadow.project_point3(p);
            assert!(s.x > 0.0 && s.x < 1.0 && s.y > 0.0 && s.y < 1.0 && s.z > 0.0 && s.z < 1.0, "{s}");
            let o = rig.occupancy.project_point3(p);
            assert!(o.x > 0.0 && o.x < 1.0 && o.y > 0.0 && o.y < 1.0 && o.z > 0.0 && o.z < 1.0, "{o}");
            // Глибина карти висот відновлює висоту точки.
            let z = rig.occupancy_top - o.z * rig.occupancy_depth;
            assert!((z - p.z).abs() < 1e-2, "{z} ≠ {}", p.z);
        }
        // Тінь верхньої точки на підлозі теж у межах карти (і не за дальньою площиною).
        let top = Vec3::new(130.0, 120.0, 80.0);
        let on_floor = top - KEY_LIGHT * (top.z / KEY_LIGHT.z);
        let s = rig.shadow.project_point3(on_floor);
        assert!(s.z < 1.0 && s.z > rig.shadow.project_point3(top).z);
        // Ближче до світла — менша глибина.
        let near = rig.shadow.project_point3(top + KEY_LIGHT * 5.0);
        assert!(near.z < rig.shadow.project_point3(top).z);
        // Згори y UV зростає до −Y світу (рядок 0 — верх кадру = +Y).
        let a = rig.occupancy.project_point3(Vec3::new(130.0, 140.0, 0.0));
        let b = rig.occupancy.project_point3(Vec3::new(130.0, 100.0, 0.0));
        assert!(a.y < b.y);
    }

    #[test]
    fn gpu_material_is_linear_and_packed_for_wgsl() {
        assert_eq!(size_of::<GpuMaterial>(), 96);
        assert_eq!(size_of::<Globals>() % 16, 0);
        assert_eq!(size_of::<ShadowPass>(), 112);
        let table = MaterialTable::builtin();
        let silk = table.resolve(table.index_of("silk").unwrap(), [1.0, 0.5, 0.0]);
        let gpu = GpuMaterial::from(&silk);
        assert_eq!(gpu.base[0], 1.0);
        assert!((gpu.base[1] - 0.214).abs() < 1e-3, "sRGB 0.5 → лінійне 0.214");
        assert_eq!(gpu.p1[2], silk.anisotropy);
        let marble = table.resolve(table.index_of("marble").unwrap(), [1.0; 3]);
        assert_eq!(GpuMaterial::from(&marble).p2[0], Pattern::Marble.id() as f32);
    }

    #[test]
    fn transparent_mask_marks_clear_slots_and_inherits_last() {
        let table = MaterialTable::builtin();
        let basic = table.resolve(table.default, [0.5; 3]);
        let clear = table.resolve(table.index_of("translucent").unwrap(), [0.4, 0.7, 1.0]);
        assert_eq!(transparent_mask(&[basic]), 0);
        assert_eq!(transparent_mask(&[]), 0);
        assert_eq!(transparent_mask(&[basic, clear, basic]), 0b10);
        // Слоти за межами списку беруть останній матеріал — тут прозорий.
        assert_eq!(transparent_mask(&[basic, clear]), !1);
        let gpu = GpuMaterial::from(&clear);
        assert_eq!(gpu.p2[3], clear.transmission);
        assert!(gpu.p4[0] > gpu.p4[1] && gpu.p4[2] == 0.0 && gpu.p3[3] > 0.0);
        assert_eq!(GpuMaterial::from(&basic).p4, [0.0, 0.0, 0.0, basic.absorption_distance]);
    }

    /// Суцільний блок 10×10 мм із вкладених квадратів, `layers` шарів; `feature` — роль.
    fn block_gcode(layers: usize, feature: &str) -> String {
        use std::fmt::Write as _;
        let mut out = String::from("G90\nM83\n");
        for layer in 0..layers {
            let z = 0.2 * (layer + 1) as f32;
            let _ = writeln!(out, "; CHANGE_LAYER\n; Z_HEIGHT: {z:.2}\n; LAYER_HEIGHT: 0.2\nG1 Z{z:.2}\n; FEATURE: {feature}\n; LINE_WIDTH: 0.42");
            for ring in 0..13 {
                let (a, b) = (120.0 + ring as f32 * 0.4, 130.0 - ring as f32 * 0.4);
                let _ = writeln!(out, "G0 X{a:.2} Y{a:.2}\nG1 X{b:.2} E0.1\nG1 Y{b:.2} E0.1\nG1 X{a:.2} E0.1\nG1 Y{a:.2} E0.1");
            }
        }
        out
    }

    #[test]
    fn photo_frame_shades_material_hides_supports_and_frees_resources() {
        use crate::ToolpathGpuExt as _;
        let Some(mut r) = crate::test_renderer("photo_frame") else {
            return;
        };
        let t = gcode::parse(&block_gcode(40, "Outer wall")).unwrap();
        let built = t.build_photo(r.device());
        r.set_toolpath(&built.gpu);
        let table = MaterialTable::builtin();
        let red = table.resolve(table.default, [0.8, 0.1, 0.1]);
        let settings = PhotoSettings {
            materials: vec![red],
            stage: Stage::Floor,
            ..Default::default()
        };
        r.set_photo(Some(settings.clone()));
        assert!(r.photo_active());
        let bytes = r.photo_bytes().unwrap();
        assert!(bytes > 16 << 20 && bytes < 64 << 20, "{bytes} байт");
        let (lo, hi) = t.bounds().unwrap();
        let mut camera = Camera::default();
        camera.frame_tight(lo, hi);
        let (w, h) = (160u32, 160u32);
        let pixel = |px: &[u8], x: u32, y: u32| {
            let i = ((y * w + x) * 4) as usize;
            [px[i + 2], px[i + 1], px[i]]
        };
        let px = r.render(w, h, &camera);
        let center = pixel(&px, w / 2, h / 2);
        assert!(center[0] > 60 && center[0] > center[1] * 2 && center[0] > center[2] * 2, "{center:?}");
        // Кут кадру — фон студії: нейтрально-сірий.
        let corner = pixel(&px, 2, 2);
        assert!(corner[0].abs_diff(corner[2]) < 16 && corner[0] > 20, "{corner:?}");

        // Той самий блок як підтримка: сховані — у центрі лише підлога.
        let t = gcode::parse(&block_gcode(40, "Support")).unwrap();
        let built = t.build_photo(r.device());
        r.set_toolpath(&built.gpu);
        r.set_photo(Some(PhotoSettings { supports: SupportView::Hidden, ..settings.clone() }));
        let px = r.render(w, h, &camera);
        let floor = pixel(&px, w / 2, h / 2);
        assert!(floor[0].abs_diff(floor[2]) < 16, "підтримка не схована: {floor:?}");

        // Прозорий синій блок: колір синій, а тінь на підлозі кольорова (синя), а не сіра, і
        // світліша за тінь непрозорого.
        let t = gcode::parse(&block_gcode(40, "Outer wall")).unwrap();
        let built = t.build_photo(r.device());
        r.set_toolpath(&built.gpu);
        let mut camera = camera.clone();
        camera.distance *= 3.0;
        let blue = [0.38, 0.69, 1.0];
        r.set_photo(Some(PhotoSettings { materials: vec![table.resolve(table.default, blue)], ..settings.clone() }));
        let opaque = r.render(w, h, &camera);
        let clear = table.resolve(table.index_of("translucent").unwrap(), blue);
        r.set_photo(Some(PhotoSettings { materials: vec![clear], ..settings.clone() }));
        let px = r.render(w, h, &camera);
        let c = pixel(&px, w / 2, h / 2);
        assert!(c[2] > c[0] + 30 && c[2] > 60, "прозорий синій: {c:?}");
        // Тінь — нейтральні (сірі) пікселі підлоги непрозорого кадру, темніші за її медіану.
        let sum = |p: [u8; 3]| p.iter().map(|&c| u32::from(c)).sum::<u32>();
        let floor: Vec<(u32, u32)> = (0..h)
            .flat_map(|y| (0..w).map(move |x| (x, y)))
            .filter(|&(x, y)| {
                let p = pixel(&opaque, x, y);
                p[0].abs_diff(p[2]) < 6
            })
            .collect();
        let mut sums: Vec<u32> = floor.iter().map(|&(x, y)| sum(pixel(&opaque, x, y))).collect();
        sums.sort_unstable();
        let median = sums[sums.len() / 2];
        let shade: Vec<_> = floor.into_iter().filter(|&(x, y)| sum(pixel(&opaque, x, y)) * 20 < median * 17).collect();
        assert!(shade.len() > 50, "тінь блока не знайдена");
        let mean = |img: &[u8], ch: usize| shade.iter().map(|&(x, y)| f64::from(pixel(img, x, y)[ch])).sum::<f64>() / shade.len() as f64;
        let (clear_rb, opaque_rb) = (mean(&px, 2) - mean(&px, 0), mean(&opaque, 2) - mean(&opaque, 0));
        assert!(clear_rb > opaque_rb + 3.0, "тінь прозорого кольорова: B−R {clear_rb:.1} vs {opaque_rb:.1}");
        let light = |img: &[u8]| (0..3).map(|ch| mean(img, ch)).sum::<f64>();
        assert!(light(&px) > light(&opaque), "тінь прозорого світліша");
        assert!(r.photo_bytes().unwrap() > bytes, "ціль товщини врахована");
        r.set_photo(Some(settings.clone()));

        // PNG у довільному розмірі; вихід із режиму звільняє ресурси, звичайний кадр працює.
        let png = r.render_photo_png(320, 200, &camera).unwrap();
        let img = image::load_from_memory(&png).unwrap();
        assert_eq!((img.width(), img.height()), (320, 200));
        r.set_photo(None);
        assert!(!r.photo_active() && r.photo_bytes().is_none());
        assert_eq!(r.render(32, 32, &camera).len(), 32 * 32 * 4);
    }
}
