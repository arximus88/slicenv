//! Кнопки пластини на площині столу, як у Bambu Studio / OrcaSlicer: колонка квадратів
//! праворуч від заднього правого кута пластини, з перспективою та глибиною сцени.
//! Іконки — SVG Orca (`assets/plate`), растеризовані один раз в атлас із mip-рівнями.
use bytemuck::{Pod, Zeroable};
use glam::Vec2;
use wgpu::util::DeviceExt as _;

use super::{COLOR_FORMAT, DEPTH_FORMAT, SAMPLES};

/// Кнопка пластини. Порядок колонки згори вниз — як у Orca (`PartPlate::calc_vertex_for_icons`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PlateIcon {
    Delete,
    Orient,
    Arrange,
    Locked,
    Unlocked,
    Settings,
}

/// Позиція в колонці (0 — найближча до заднього краю).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PlateButton {
    Delete,
    Orient,
    Arrange,
    Lock,
    Settings,
    /// Назва пластини з олівцем — перейменування (не в колонці кнопок).
    Rename,
}

impl PlateButton {
    pub const ALL: [PlateButton; 5] = [
        PlateButton::Delete,
        PlateButton::Orient,
        PlateButton::Arrange,
        PlateButton::Lock,
        PlateButton::Settings,
    ];
}

/// Що намалювати: кнопка, прямокутник на столі (світові мм) і стан.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PlateIconQuad {
    pub icon: PlateIcon,
    pub min: Vec2,
    pub max: Vec2,
    pub hover: bool,
    pub dimmed: bool,
}

// Розміри Orca (мм при множнику 1): PARTPLATE_ICON_SIZE, _GAP_LEFT, _GAP_Y, _GAP_TOP.
const ICON: f32 = 16.;
const GAP_LEFT: f32 = 3.;
const GAP_Y: f32 = 5.;
const GAP_TOP: f32 = 3.;

/// Прямокутники кнопок для пластини зі світовими межами `lo..hi`; `gap` — частка ширини
/// між пластинами (0.2 у сцені). Множник як `PartPlate::icon_scale_factor`.
pub fn plate_button_rects(lo: Vec2, hi: Vec2, gap: f32) -> [(PlateButton, Vec2, Vec2); 5] {
    let size = hi - lo;
    let factor = (size.y / 200.).min(size.x * gap / (ICON + 2. * GAP_LEFT)).max(0.1);
    let (icon, left, gap_y, top) = (ICON * factor, GAP_LEFT * factor, GAP_Y * factor, GAP_TOP * factor);
    PlateButton::ALL.map(|button| {
        let index = button as usize as f32;
        let x = hi.x + left;
        let y_top = hi.y - top - index * (icon + gap_y);
        (button, Vec2::new(x, y_top - icon), Vec2::new(x + icon, y_top))
    })
}

const CELL: u32 = 128;
const LEVELS: u32 = 8; // 128 → 1
/// Варіанти кожної іконки в атласі: світла, світла-hover, темна, темна-hover.
const VARIANTS: [&str; 4] = ["", "_hover", "_dark", "_hover_dark"];
const ICONS: [(PlateIcon, &str); 6] = [
    (PlateIcon::Delete, "close"),
    (PlateIcon::Orient, "orient"),
    (PlateIcon::Arrange, "arrange"),
    (PlateIcon::Locked, "locked"),
    (PlateIcon::Unlocked, "unlocked"),
    (PlateIcon::Settings, "settings"),
];

fn svg(name: &str) -> &'static [u8] {
    macro_rules! files {
        ($($n:literal),* $(,)?) => {
            match name {
                $($n => include_bytes!(concat!("../assets/plate/plate_", $n, ".svg")).as_slice(),)*
                _ => &[],
            }
        };
    }
    files!(
        "close", "close_hover", "close_dark", "close_hover_dark",
        "orient", "orient_hover", "orient_dark", "orient_hover_dark",
        "arrange", "arrange_hover", "arrange_dark", "arrange_hover_dark",
        "locked", "locked_hover", "locked_dark", "locked_hover_dark",
        "unlocked", "unlocked_hover", "unlocked_dark", "unlocked_hover_dark",
        "settings", "settings_hover", "settings_dark", "settings_hover_dark",
    )
}

fn slot(icon: PlateIcon, hover: bool, dark: bool) -> u32 {
    let i = ICONS.iter().position(|(k, _)| *k == icon).unwrap_or(0) as u32;
    i * VARIANTS.len() as u32 + u32::from(hover) + 2 * u32::from(dark)
}

/// RGBA-атлас: усі іконки в один рядок клітинок CELL×CELL (неперемножена альфа).
fn rasterize() -> (u32, Vec<u8>) {
    let count = (ICONS.len() * VARIANTS.len()) as u32;
    let width = CELL * count;
    let mut atlas = vec![0u8; (width * CELL * 4) as usize];
    let options = resvg::usvg::Options::default();
    for (i, (_, name)) in ICONS.iter().enumerate() {
        for (v, suffix) in VARIANTS.iter().enumerate() {
            let index = (i * VARIANTS.len() + v) as u32;
            let Ok(tree) = resvg::usvg::Tree::from_data(svg(&format!("{name}{suffix}")), &options)
            else {
                continue;
            };
            let Some(mut pixmap) = resvg::tiny_skia::Pixmap::new(CELL, CELL) else {
                continue;
            };
            let size = tree.size();
            let scale = CELL as f32 / size.width().max(size.height());
            resvg::render(
                &tree,
                resvg::tiny_skia::Transform::from_scale(scale, scale),
                &mut pixmap.as_mut(),
            );
            for (p, px) in pixmap.pixels().iter().enumerate() {
                let c = px.demultiply();
                let (x, y) = (p as u32 % CELL, p as u32 / CELL);
                let at = ((y * width + index * CELL + x) * 4) as usize;
                atlas[at..at + 4].copy_from_slice(&[c.red(), c.green(), c.blue(), c.alpha()]);
            }
        }
    }
    (width, atlas)
}

/// Наступний mip-рівень: середнє 2×2 з вагою альфи, щоб прозорі краї не темніли.
pub(super) fn downsample(width: u32, height: u32, src: &[u8]) -> Vec<u8> {
    let (w, h) = ((width / 2).max(1), (height / 2).max(1));
    let mut out = vec![0u8; (w * h * 4) as usize];
    for y in 0..h {
        for x in 0..w {
            let mut acc = [0f32; 4];
            for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                let sx = (x * 2 + dx).min(width - 1);
                let sy = (y * 2 + dy).min(height - 1);
                let s = &src[((sy * width + sx) * 4) as usize..][..4];
                let a = f32::from(s[3]) / 255.;
                for c in 0..3 {
                    acc[c] += f32::from(s[c]) * a;
                }
                acc[3] += a;
            }
            let o = &mut out[((y * w + x) * 4) as usize..][..4];
            for c in 0..3 {
                o[c] = if acc[3] > 0. { (acc[c] / acc[3]).round() as u8 } else { 0 };
            }
            o[3] = (acc[3] / 4. * 255.).round() as u8;
        }
    }
    out
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Vertex {
    pos: [f32; 3],
    uv: [f32; 2],
    alpha: f32,
}

const SHADER: &str = r#"
struct Globals { view_proj: mat4x4<f32> }
@group(0) @binding(0) var<uniform> globals: Globals;
@group(1) @binding(0) var atlas: texture_2d<f32>;
@group(1) @binding(1) var atlas_sampler: sampler;
struct Out {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) alpha: f32,
}
@vertex
fn vs(@location(0) pos: vec3<f32>, @location(1) uv: vec2<f32>, @location(2) alpha: f32) -> Out {
    var out: Out;
    out.position = globals.view_proj * vec4<f32>(pos, 1.0);
    out.uv = uv;
    out.alpha = alpha;
    return out;
}
@fragment
fn fs(in: Out) -> @location(0) vec4<f32> {
    let c = textureSample(atlas, atlas_sampler, in.uv);
    return vec4<f32>(c.rgb, c.a * in.alpha);
}
"#;

/// Висота над столом, мм: вище сітки, щоб не мерехтіло.
const LIFT: f32 = 0.05;

pub(super) struct PlateIcons {
    pipeline: wgpu::RenderPipeline,
    bind_group: wgpu::BindGroup,
    slots: u32,
    quads: Option<(wgpu::Buffer, u32)>,
    last: Vec<PlateIconQuad>,
    dark: bool,
}

impl PlateIcons {
    pub(super) fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        globals_layout: &wgpu::BindGroupLayout,
    ) -> Self {
        let (width, mut level) = rasterize();
        let slots = width / CELL;
        let size = wgpu::Extent3d {
            width,
            height: CELL,
            depth_or_array_layers: 1,
        };
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("plate_icons"),
            size,
            mip_level_count: LEVELS,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let (mut w, mut h) = (width, CELL);
        for mip in 0..LEVELS {
            queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &texture,
                    mip_level: mip,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                &level,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(w * 4),
                    rows_per_image: Some(h),
                },
                wgpu::Extent3d {
                    width: w,
                    height: h,
                    depth_or_array_layers: 1,
                },
            );
            if mip + 1 < LEVELS {
                level = downsample(w, h, &level);
                (w, h) = ((w / 2).max(1), (h / 2).max(1));
            }
        }
        let view = texture.create_view(&Default::default());
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("plate_icons"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Linear,
            anisotropy_clamp: 8,
            ..Default::default()
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("plate_icons"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("plate_icons"),
            layout: &layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("plate_icons"),
            bind_group_layouts: &[Some(globals_layout), Some(&layout)],
            ..Default::default()
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("plate_icons"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("plate_icons"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs"),
                compilation_options: Default::default(),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: size_of::<Vertex>() as u64,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x2, 2 => Float32],
                })],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: COLOR_FORMAT,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState::default(),
            // Зворотна глибина (як решта сцени): об’єкти перед кнопками їх закривають.
            depth_stencil: Some(wgpu::DepthStencilState {
                format: DEPTH_FORMAT,
                depth_write_enabled: Some(false),
                depth_compare: Some(wgpu::CompareFunction::GreaterEqual),
                stencil: Default::default(),
                bias: Default::default(),
            }),
            multisample: wgpu::MultisampleState {
                count: SAMPLES,
                ..Default::default()
            },
            multiview_mask: None,
            cache: None,
        });
        Self {
            pipeline,
            bind_group,
            slots,
            quads: None,
            last: Vec::new(),
            dark: true,
        }
    }

    pub(super) fn set(&mut self, device: &wgpu::Device, quads: &[PlateIconQuad], dark: bool) {
        if self.last == quads && self.dark == dark {
            return;
        }
        self.last = quads.to_vec();
        self.dark = dark;
        if quads.is_empty() {
            self.quads = None;
            return;
        }
        let cell = 1. / self.slots as f32;
        let mut vertices = Vec::with_capacity(quads.len() * 6);
        for q in quads {
            let s = slot(q.icon, q.hover, dark) as f32;
            // Половина текселя відступу: сусідні клітинки атласу не просочуються на краях.
            let inset = cell * 0.5 / CELL as f32;
            let (u0, u1) = (s * cell + inset, (s + 1.) * cell - inset);
            let alpha = if q.dimmed { 0.45 } else { 1. };
            let v = |x: f32, y: f32, u: f32, w: f32| Vertex {
                pos: [x, y, LIFT],
                uv: [u, w],
                alpha,
            };
            // Верх картинки (v = 0) — задній край, +Y.
            let (a, b) = (q.min, q.max);
            vertices.extend_from_slice(&[
                v(a.x, a.y, u0, 1.),
                v(b.x, a.y, u1, 1.),
                v(b.x, b.y, u1, 0.),
                v(a.x, a.y, u0, 1.),
                v(b.x, b.y, u1, 0.),
                v(a.x, b.y, u0, 0.),
            ]);
        }
        let buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("plate_icon_quads"),
            contents: bytemuck::cast_slice(&vertices),
            usage: wgpu::BufferUsages::VERTEX,
        });
        self.quads = Some((buffer, vertices.len() as u32));
    }

    pub(super) fn draw(&self, pass: &mut wgpu::RenderPass<'_>) {
        if let Some((buffer, count)) = &self.quads {
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(1, &self.bind_group, &[]);
            pass.set_vertex_buffer(0, buffer.slice(..));
            pass.draw(0..*count, 0..1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_icon_variant_rasterizes() {
        let (width, atlas) = rasterize();
        let cells = width / CELL;
        assert_eq!(cells, 24);
        for cell in 0..cells {
            let opaque = (0..CELL * CELL)
                .filter(|p| {
                    let (x, y) = (p % CELL, p / CELL);
                    atlas[((y * width + cell * CELL + x) * 4 + 3) as usize] > 0
                })
                .count();
            assert!(opaque > 1000, "cell {cell} is empty");
        }
    }

    #[test]
    fn buttons_sit_in_the_gap_right_of_the_plate_top_down() {
        let rects = plate_button_rects(Vec2::ZERO, Vec2::splat(256.), 0.2);
        let gap_end = 256. * 1.2;
        let mut previous_top = f32::INFINITY;
        for (_, min, max) in rects {
            assert!(min.x > 256. && max.x < gap_end, "{min} {max}");
            assert!(max.y <= 256. && min.y >= 0.);
            assert!(max.y < previous_top);
            previous_top = min.y;
        }
        assert_eq!(rects[0].0, PlateButton::Delete);
    }

    #[test]
    fn mip_chain_keeps_transparent_edges_clean() {
        // Білий непрозорий піксель поруч із прозорим чорним не має сіріти.
        let src = [255, 255, 255, 255, 0, 0, 0, 0, 255, 255, 255, 255, 0, 0, 0, 0];
        let out = downsample(2, 2, &src);
        assert_eq!(&out[..3], &[255, 255, 255]);
        assert_eq!(out[3], 128);
    }
}
