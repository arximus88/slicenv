//! Вигляд столу, як у Bambu Studio / OrcaSlicer: активна пластина — «справжній» стіл
//! (модель столу з профілю принтера, логотип і написи типу пластини), неактивні — світлі
//! й нейтральні. Над пластиною — її назва з 3MF, у куті — номер.
//!
//! Ресурси (STL столу, SVG логотипа й написів) застосунок читає з профілів встановленого
//! рушія під час роботи; у репозиторії їх немає. Без рушія стіл будується процедурно, а
//! назву типу пластини малюємо самі.
use std::collections::HashMap;

use bytemuck::{Pod, Zeroable};
use glam::{Vec2, Vec3};
use wgpu::util::DeviceExt as _;

use super::{COLOR_FORMAT, DEPTH_FORMAT, FlatVertex, SAMPLES, plate_icons::downsample};
use crate::Mesh;

/// Растрове чи векторне зображення з профілю принтера.
#[derive(Clone, Debug, PartialEq)]
pub enum BedImage {
    Svg(Vec<u8>),
    Png(Vec<u8>),
}

/// Напис на столі в координатах пластини (мм).
#[derive(Clone, Debug, PartialEq)]
pub struct BedDecal {
    pub min: Vec2,
    pub max: Vec2,
    pub image: BedImage,
}

/// Що відомо про стіл вибраного принтера.
#[derive(Clone, Debug, Default)]
pub struct BedAssets {
    /// Модель столу (`bed_model`) у координатах пластини, верх — на Z = 0.
    pub model: Option<Mesh>,
    /// Логотип (`bed_texture`), натягнутий на рамку моделі або столу.
    pub logo: Option<BedImage>,
    /// Написи типу пластини (`bbl_bed_*_left/bottom.svg`).
    pub decals: Vec<BedDecal>,
    /// Назва типу пластини для процедурного напису, коли SVG-написів немає.
    pub type_label: Option<String>,
    /// Ключ для кешу: той самий ключ — нічого не перебудовуємо.
    pub key: String,
}

/// Підпис пластини: номер і назва з 3MF (`plater_name`).
#[derive(Clone, Debug, PartialEq)]
pub struct PlateLabel {
    pub number: u32,
    pub name: String,
}

/// Кольори підписів (лінійний RGBA): активна пластина й решта.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LabelColors {
    pub active: [f32; 4],
    pub inactive: [f32; 4],
}

impl Default for LabelColors {
    fn default() -> Self {
        Self {
            active: [0.9, 0.25, 0.02, 1.0],
            inactive: [0.35, 0.37, 0.4, 1.0],
        }
    }
}

pub(super) fn srgb(c: [f32; 3]) -> [f32; 4] {
    let l = |v: f32| {
        if v <= 0.04045 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    };
    [l(c[0]), l(c[1]), l(c[2]), 1.0]
}

/// Кольори Bambu Studio (`PartPlate.cpp`, `3DBed.cpp`), sRGB.
pub(super) struct Palette {
    pub bed: [f32; 3],
    pub plate: [f32; 3],
    pub grid: [f32; 3],
    pub grid_active: [f32; 3],
    pub edge: [f32; 3],
}

pub(super) fn palette(light: bool) -> Palette {
    if light {
        Palette {
            bed: [0.3255, 0.337, 0.337],
            plate: [0.82, 0.82, 0.82],
            grid: [0.93, 0.93, 0.93],
            grid_active: [0.5294, 0.5451, 0.5333],
            edge: [0.70, 0.70, 0.70],
        }
    } else {
        // Темне тло (#282c33) темніше за стіл Bambu, тож активний стіл тут світліший,
        // а неактивні пластини — ледь світліші за тло.
        Palette {
            bed: [0.34, 0.34, 0.37],
            plate: [0.215, 0.22, 0.24],
            grid: [0.29, 0.295, 0.32],
            grid_active: [0.46, 0.46, 0.50],
            edge: [0.36, 0.36, 0.40],
        }
    }
}

/// Трикутники моделі столу з «запеченим» світлом: верх рівний, боки темніші. Порядок —
/// низ, боки, верх: стіл малюється без запису глибини, тож згори видно верх.
pub(super) fn shaded_bed(mesh: &Mesh, offset: Vec3, color: [f32; 3]) -> Vec<FlatVertex> {
    let mut tris: Vec<(f32, [FlatVertex; 3])> = Vec::with_capacity(mesh.triangle_count());
    for (p, n) in mesh
        .positions
        .as_chunks::<3>()
        .0
        .iter()
        .zip(mesh.normals.as_chunks::<3>().0)
    {
        let nz = n[0][2];
        let shade = if nz > 0.5 {
            1.0
        } else if nz < -0.5 {
            0.55
        } else {
            0.72
        };
        let c = srgb(color.map(|v| (v * shade).min(1.0)));
        let v = |i: usize| FlatVertex {
            pos: (Vec3::from(p[i]) + offset).to_array(),
            color: c,
        };
        tris.push((nz, [v(0), v(1), v(2)]));
    }
    tris.sort_by(|a, b| a.0.total_cmp(&b.0));
    tris.into_iter().flat_map(|(_, t)| t).collect()
}

/// Процедурний стіл без рушія: заокруглена плита з полями, як у справжнього столу Bambu.
pub(super) fn procedural_bed(lo: Vec2, hi: Vec2) -> Mesh {
    let (min, max) = (lo - Vec2::new(1., 10.), hi + Vec2::new(2., 10.));
    let r = 6.0f32;
    let mut outline = Vec::new();
    let corners = [
        (Vec2::new(max.x - r, min.y + r), -90.0f32),
        (Vec2::new(max.x - r, max.y - r), 0.),
        (Vec2::new(min.x + r, max.y - r), 90.),
        (Vec2::new(min.x + r, min.y + r), 180.),
    ];
    for (c, start) in corners {
        for i in 0..=6 {
            let a = (start + i as f32 * 15.).to_radians();
            outline.push(c + Vec2::new(a.cos(), a.sin()) * r);
        }
    }
    let center = (min + max) * 0.5;
    let (top, bottom) = (-0.05f32, -0.6f32);
    let mut mesh = Mesh::default();
    for i in 0..outline.len() {
        let (a, b) = (outline[i], outline[(i + 1) % outline.len()]);
        mesh.push_triangle(center.extend(top), a.extend(top), b.extend(top));
        mesh.push_triangle(center.extend(bottom), b.extend(bottom), a.extend(bottom));
        mesh.push_triangle(a.extend(bottom), b.extend(bottom), b.extend(top));
        mesh.push_triangle(a.extend(bottom), b.extend(top), a.extend(top));
    }
    mesh
}

/// Розташування написів типу пластини Bambu для одного екструдера (`init_bed_type_info`):
/// ліворуч і знизу, база 256×256, A1 mini (180×180) — свій зсув лівого напису.
pub fn bambu_decal_rects(bed: Vec2) -> [(Vec2, Vec2); 2] {
    let rate = bed / 256.;
    let left = if (bed.x - 180.).abs() < 0.5 && (bed.y - 180.).abs() < 0.5 {
        Vec2::new(10., 35.)
    } else {
        Vec2::new(10., 52.) * rate
    };
    let left_size = Vec2::new(8.393, 192.) * rate;
    let bottom = Vec2::new(74., -10.) * rate;
    let bottom_size = Vec2::new(148., 12.) * rate;
    [(left, left + left_size), (bottom, bottom + bottom_size)]
}

/// Груба маска непрозорих місць зображення: де на логотипі вже щось намальовано.
#[derive(Clone, Debug, PartialEq)]
pub struct Mask {
    width: usize,
    height: usize,
    cells: Vec<bool>,
}

impl Mask {
    /// Клітинка зайнята, коли в ній є хоч один помітний піксель (альфа > 24).
    fn from_rgba(width: u32, height: u32, pixels: &[u8], cells: usize) -> Self {
        let (w, h) = (width as usize, height as usize);
        let (cw, ch) = (cells.min(w).max(1), cells.min(h).max(1));
        let mut out = vec![false; cw * ch];
        for (i, px) in pixels.as_chunks::<4>().0.iter().enumerate() {
            if px[3] > 24 {
                let (x, y) = (i % w, i / w);
                out[(y * ch / h) * cw + x * cw / w] = true;
            }
        }
        Self {
            width: cw,
            height: ch,
            cells: out,
        }
    }

    /// Чи зайнята точка; `uv` — як у текстури (v = 0 — верхній край).
    fn at(&self, u: f32, v: f32) -> bool {
        if !(0.0..1.0).contains(&u) || !(0.0..1.0).contains(&v) {
            return false;
        }
        let (x, y) = (
            (u * self.width as f32) as usize,
            (v * self.height as f32) as usize,
        );
        self.cells[y * self.width + x]
    }

    /// Найвищий вільний від логотипа відрізок `y` колонки `x0..x1` у межах `y0..y1`
    /// довжиною щонайменше `want` мм; немає такого — найдовший. Логотип натягнуто на
    /// рамку `body` (верх зображення — max y). Повертає (низ, верх) з відступом `gap`.
    pub fn free_span(
        &self,
        body: (Vec2, Vec2),
        (x0, x1): (f32, f32),
        (y0, y1): (f32, f32),
        want: f32,
        gap: f32,
    ) -> Option<(f32, f32)> {
        const STEP: f32 = 0.5;
        let size = body.1 - body.0;
        if size.x <= 0. || size.y <= 0. || x1 <= x0 || y1 <= y0 {
            return None;
        }
        // Колонку беремо з запасом `gap` з боків, щоб текст не торкався логотипа.
        let busy = |y: f32| {
            let v = (body.1.y - y) / size.y;
            let mut x = x0 - gap;
            while x <= x1 + gap {
                if self.at((x - body.0.x) / size.x, v) {
                    return true;
                }
                x += STEP;
            }
            false
        };
        let mut spans: Vec<(f32, f32)> = Vec::new();
        let mut top: Option<f32> = None;
        let mut y = y1;
        loop {
            let end = y < y0;
            let y_c = y.max(y0);
            if end || busy(y_c) {
                if let Some(t) = top.take() {
                    // Відступ від логотипа зверху (якщо відрізок не починається з краю
                    // колонки) і знизу.
                    let t = if t < y1 { t - gap } else { t };
                    let b = if end { y0 } else { y_c + gap };
                    if t > b {
                        spans.push((b, t));
                    }
                }
                if end {
                    break;
                }
            } else if top.is_none() {
                top = Some(y_c);
            }
            y -= STEP;
        }
        spans
            .iter()
            .copied()
            .find(|(b, t)| t - b >= want)
            .or_else(|| spans.iter().copied().max_by(|a, b| (a.1 - a.0).total_cmp(&(b.1 - b.0))))
    }
}

// ---- текстури ----

const FONT: &[u8] = include_bytes!("../../app/assets/fonts/IBMPlexSans-SemiBold.ttf");

fn svg_options() -> resvg::usvg::Options<'static> {
    let mut options = resvg::usvg::Options::default();
    options.fontdb_mut().load_font_data(FONT.to_vec());
    options.font_family = "IBM Plex Sans".into();
    options
}

/// RGBA (неперемножена альфа) не більше `limit` по довшій стороні.
fn rasterize(image: &BedImage, limit: u32) -> Option<(u32, u32, Vec<u8>)> {
    match image {
        BedImage::Png(bytes) => {
            let img = image::load_from_memory(bytes).ok()?.into_rgba8();
            let img = if img.width().max(img.height()) > limit {
                image::imageops::thumbnail(&img, limit.min(img.width()), limit.min(img.height()))
            } else {
                img
            };
            Some((img.width(), img.height(), img.into_raw()))
        }
        BedImage::Svg(bytes) => {
            let tree = resvg::usvg::Tree::from_data(bytes, &svg_options()).ok()?;
            render_tree(&tree, None, limit)
        }
    }
}

fn render_tree(
    tree: &resvg::usvg::Tree,
    crop: Option<resvg::tiny_skia::Rect>,
    limit: u32,
) -> Option<(u32, u32, Vec<u8>)> {
    let area = crop.unwrap_or_else(|| {
        resvg::tiny_skia::Rect::from_xywh(0., 0., tree.size().width(), tree.size().height())
            .unwrap()
    });
    let scale = limit as f32 / area.width().max(area.height());
    let (w, h) = (
        (area.width() * scale).ceil().max(1.) as u32,
        (area.height() * scale).ceil().max(1.) as u32,
    );
    let mut pixmap = resvg::tiny_skia::Pixmap::new(w, h)?;
    resvg::render(
        tree,
        resvg::tiny_skia::Transform::from_translate(-area.x(), -area.y()).post_scale(scale, scale),
        &mut pixmap.as_mut(),
    );
    let mut out = Vec::with_capacity((w * h * 4) as usize);
    for px in pixmap.pixels() {
        let c = px.demultiply();
        out.extend_from_slice(&[c.red(), c.green(), c.blue(), c.alpha()]);
    }
    Some((w, h, out))
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// Білий текст, обрізаний по своїх межах; колір дає вершина. Повертає й пропорцію.
fn rasterize_text(text: &str, height: u32) -> Option<(u32, u32, Vec<u8>)> {
    let svg = format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="4000" height="140"><text x="10" y="100" font-family="IBM Plex Sans" font-weight="600" font-size="100" fill="white">{}</text></svg>"#,
        escape(text)
    );
    let tree = resvg::usvg::Tree::from_data(svg.as_bytes(), &svg_options()).ok()?;
    let bbox = tree.root().abs_bounding_box();
    // Висота рядка постійна (не за гліфами), щоб «01» і «Ab» стояли на одній лінії.
    let crop = resvg::tiny_skia::Rect::from_xywh(bbox.x() - 4., 12., bbox.width() + 8., 116.)?;
    let scale = height as f32 / crop.height();
    render_tree(
        &tree,
        Some(crop),
        (crop.width().max(crop.height()) * scale) as u32,
    )
}

/// Олівець Lucide (ISC), білий контур.
const PENCIL: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="white" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M21.174 6.812a1 1 0 0 0-3.986-3.987L3.842 16.174a2 2 0 0 0-.5.83l-1.321 4.352a.5.5 0 0 0 .623.622l4.353-1.32a2 2 0 0 0 .83-.497z"/><path d="m15 5 4 4"/></svg>"#;

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Vertex {
    pos: [f32; 3],
    uv: [f32; 2],
    color: [f32; 4],
}

const SHADER: &str = r#"
struct Globals { view_proj: mat4x4<f32>, eye: vec4<f32> }
@group(0) @binding(0) var<uniform> globals: Globals;
@group(1) @binding(0) var image: texture_2d<f32>;
@group(1) @binding(1) var image_sampler: sampler;
struct Out {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) color: vec4<f32>,
}
@vertex
fn vs(@location(0) pos: vec3<f32>, @location(1) uv: vec2<f32>, @location(2) color: vec4<f32>) -> Out {
    var out: Out;
    out.position = globals.view_proj * vec4<f32>(pos, 1.0);
    out.uv = uv;
    out.color = color;
    return out;
}
@fragment
fn fs(in: Out) -> @location(0) vec4<f32> {
    let c = textureSample(image, image_sampler, in.uv);
    let below = select(1.0, 0.35, globals.eye.z < 0.0);
    return vec4<f32>(c.rgb * in.color.rgb * globals.eye.w, c.a * in.color.a * below);
}
"#;

struct Texture {
    bind_group: wgpu::BindGroup,
    /// Ширина / висота.
    aspect: f32,
}

/// Текстуровані прямокутники на площині столу: логотип, написи, підписи пластин.
pub(super) struct Decals {
    pipeline: Option<wgpu::RenderPipeline>,
    layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    /// Текстури столу з профілю (логотип і написи) — за індексом у `BedAssets`.
    bed: Vec<Texture>,
    /// Де на логотипі щось намальовано — щоб власний підпис не ліг поверх.
    logo_mask: Option<Mask>,
    /// Текстури тексту за рядком; олівець — під ключем "\u{0}pencil".
    text: HashMap<String, Texture>,
    /// Що малювати: (текстура, вершини) — під сіткою (стіл) і над нею (підписи).
    under_grid: Vec<(Key, wgpu::Buffer, u32)>,
    over_grid: Vec<(Key, wgpu::Buffer, u32)>,
}

#[derive(Clone)]
pub(super) enum Key {
    Bed(usize),
    Text(String),
}

/// Квад у світі: кути `min..max` на висоті `z`; `rotate` — текст іде вздовж −Y, верх літер — до +X.
pub(super) struct Quad {
    pub key: Key,
    pub min: Vec2,
    pub max: Vec2,
    pub z: f32,
    pub color: [f32; 4],
    pub rotate: bool,
}

impl Decals {
    pub(super) fn new(device: &wgpu::Device, globals_layout: &wgpu::BindGroupLayout) -> Self {
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("bed_decals"),
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
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("bed_decals"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Linear,
            anisotropy_clamp: 8,
            ..Default::default()
        });
        let mut decals = Self {
            pipeline: None,
            layout,
            sampler,
            bed: Vec::new(),
            logo_mask: None,
            text: HashMap::new(),
            under_grid: Vec::new(),
            over_grid: Vec::new(),
        };
        decals.pipeline = Some(decals.pipeline(device, globals_layout));
        decals
    }

    /// Пайплайн написів для групи 0 з макетом `globals_layout` (uniform `Globals { view_proj,
    /// eye }`; `eye.w` — яскравість). «Фото» малює ними в ту саму ціль своїми globals.
    pub(super) fn pipeline(&self, device: &wgpu::Device, globals_layout: &wgpu::BindGroupLayout) -> wgpu::RenderPipeline {
        let layout = &self.layout;
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("bed_decals"),
            bind_group_layouts: &[Some(globals_layout), Some(layout)],
            ..Default::default()
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("bed_decals"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("bed_decals"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs"),
                compilation_options: Default::default(),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: size_of::<Vertex>() as u64,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x2, 2 => Float32x4],
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
            primitive: wgpu::PrimitiveState {
                cull_mode: None,
                ..Default::default()
            },
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
        })
    }

    fn upload(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        (width, height, pixels): (u32, u32, Vec<u8>),
    ) -> Texture {
        let levels = 32 - width.max(height).leading_zeros();
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("bed_decal"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: levels,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let (mut w, mut h, mut level) = (width, height, pixels);
        for mip in 0..levels {
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
            if mip + 1 < levels {
                level = downsample(w, h, &level);
                (w, h) = ((w / 2).max(1), (h / 2).max(1));
            }
        }
        let view = texture.create_view(&Default::default());
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("bed_decal"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
            ],
        });
        Texture {
            bind_group,
            aspect: width as f32 / height as f32,
        }
    }

    /// Текстури логотипа й написів; `None` на місці зображення, що не прочиталось.
    pub(super) fn set_bed_images(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        images: &[&BedImage],
    ) -> Vec<bool> {
        self.bed.clear();
        self.logo_mask = None;
        let mut ok = Vec::new();
        for (i, image) in images.iter().enumerate() {
            // Логотип (перший) накриває весь стіл — більша роздільність.
            let limit = if i == 0 { 1536 } else { 1024 };
            match rasterize(image, limit) {
                Some(pixels) => {
                    if i == 0 {
                        self.logo_mask = Some(Mask::from_rgba(pixels.0, pixels.1, &pixels.2, 256));
                    }
                    let texture = self.upload(device, queue, pixels);
                    self.bed.push(texture);
                    ok.push(true);
                }
                None => {
                    // Порожнє місце, щоб індекси збігались.
                    let texture = self.upload(device, queue, (1, 1, vec![0; 4]));
                    self.bed.push(texture);
                    ok.push(false);
                }
            }
        }
        ok
    }

    /// Маска логотипа (`None` — логотипа немає або він не прочитався).
    pub(super) fn logo_mask(&self) -> Option<&Mask> {
        self.logo_mask.as_ref()
    }

    /// Пропорція тексту (ширина / висота), растеризує за потреби.
    pub(super) fn text_aspect(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        text: &str,
    ) -> Option<f32> {
        if !self.text.contains_key(text) {
            let pixels = if text == PENCIL_KEY {
                resvg::usvg::Tree::from_data(PENCIL.as_bytes(), &svg_options())
                    .ok()
                    .and_then(|t| render_tree(&t, None, 64))
            } else {
                rasterize_text(text, 96)
            }?;
            let texture = self.upload(device, queue, pixels);
            self.text.insert(text.to_owned(), texture);
        }
        self.text.get(text).map(|t| t.aspect)
    }

    /// Забути текстури рядків, яких більше немає (перейменування).
    pub(super) fn retain_text(&mut self, keep: &std::collections::HashSet<String>) {
        self.text.retain(|k, _| k == PENCIL_KEY || keep.contains(k));
    }

    pub(super) fn set_quads(&mut self, device: &wgpu::Device, under: Vec<Quad>, over: Vec<Quad>) {
        let build = |quads: Vec<Quad>| {
            quads
                .into_iter()
                .map(|q| {
                    let (a, b) = (q.min, q.max);
                    let corners = [
                        (Vec2::new(a.x, a.y), [0., 1.]),
                        (Vec2::new(b.x, a.y), [1., 1.]),
                        (Vec2::new(b.x, b.y), [1., 0.]),
                        (Vec2::new(a.x, b.y), [0., 0.]),
                    ];
                    let v = |(p, uv): (Vec2, [f32; 2])| {
                        // Повернутий напис, як SVG Bambu: читається згори вниз (u уздовж −Y),
                        // верх літер дивиться на +X.
                        let uv = if q.rotate {
                            [(b.y - p.y) / (b.y - a.y), (b.x - p.x) / (b.x - a.x)]
                        } else {
                            uv
                        };
                        Vertex {
                            pos: [p.x, p.y, q.z],
                            uv,
                            color: q.color,
                        }
                    };
                    let vertices = [0, 1, 2, 0, 2, 3].map(|i| v(corners[i]));
                    let buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                        label: Some("bed_decal_quad"),
                        contents: bytemuck::cast_slice(&vertices),
                        usage: wgpu::BufferUsages::VERTEX,
                    });
                    (q.key, buffer, 6)
                })
                .collect()
        };
        self.under_grid = build(under);
        self.over_grid = build(over);
    }

    fn texture(&self, key: &Key) -> Option<&Texture> {
        match key {
            Key::Bed(i) => self.bed.get(*i),
            Key::Text(t) => self.text.get(t),
        }
    }

    pub(super) fn draw(&self, pass: &mut wgpu::RenderPass<'_>, over_grid: bool) {
        if let Some(pipeline) = &self.pipeline {
            self.draw_with(pass, pipeline, over_grid);
        }
    }

    /// Як [`Self::draw`], але власним пайплайном (група 0 уже прив’язана викликачем).
    pub(super) fn draw_with(&self, pass: &mut wgpu::RenderPass<'_>, pipeline: &wgpu::RenderPipeline, over_grid: bool) {
        let list = if over_grid {
            &self.over_grid
        } else {
            &self.under_grid
        };
        if list.is_empty() {
            return;
        }
        pass.set_pipeline(pipeline);
        for (key, buffer, count) in list {
            if let Some(texture) = self.texture(key) {
                pass.set_bind_group(1, &texture.bind_group, &[]);
                pass.set_vertex_buffer(0, buffer.slice(..));
                pass.draw(0..*count, 0..1);
            }
        }
    }
}

pub(super) const PENCIL_KEY: &str = "\u{0}pencil";

/// Рамки підпису пластини у світових координатах: назва, олівець і номер.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LabelRects {
    pub name: Option<(Vec2, Vec2)>,
    pub pencil: (Vec2, Vec2),
    pub number: (Vec2, Vec2),
}

impl LabelRects {
    /// Зона кліку для перейменування: назва разом з олівцем.
    pub fn rename_area(&self) -> (Vec2, Vec2) {
        match self.name {
            Some((a, b)) => (a.min(self.pencil.0), b.max(self.pencil.1)),
            None => self.pencil,
        }
    }
}

/// Розкладка підпису для пластини `lo..hi` (світові мм): назва над заднім краєм зліва,
/// номер — у проміжку праворуч від нижнього кута, як у Bambu Studio.
pub fn label_rects(lo: Vec2, hi: Vec2, name_aspect: Option<f32>, number_aspect: f32) -> LabelRects {
    let size = hi - lo;
    let factor = (size.y / 200.).max(0.1);
    let height = 9. * factor;
    // За заднім полем столу (модель Bambu виступає на 10 мм), як `PARTPLATE_PLATENAME_OFFSET_Y`.
    let y0 = hi.y + 8.5 * factor;
    let pencil_size = height * 0.75;
    let room = size.x - pencil_size - height;
    let name = name_aspect.map(|aspect| {
        let h = height.min(room / aspect.max(0.01));
        let x0 = lo.x + 1.;
        let yc = y0 + height * 0.5;
        (
            Vec2::new(x0, yc - h * 0.5),
            Vec2::new(x0 + h * aspect, yc + h * 0.5),
        )
    });
    let pencil_x = name.map_or(lo.x + 1., |(_, b)| b.x + height * 0.35);
    let pencil_y = y0 + (height - pencil_size) * 0.5;
    let pencil = (
        Vec2::new(pencil_x, pencil_y),
        Vec2::new(pencil_x + pencil_size, pencil_y + pencil_size),
    );
    // Колонка кнопок займає 16·factor мм праворуч; номер — під нею, біля нижнього краю.
    let column = 16. * (size.y / 200.).max(0.1);
    let mut nh = 9. * factor;
    if nh * number_aspect > column + 4. * factor {
        nh = (column + 4. * factor) / number_aspect;
    }
    let nx = hi.x + 3. * factor;
    let number = (
        Vec2::new(nx, lo.y),
        Vec2::new(nx + nh * number_aspect, lo.y + nh),
    );
    LabelRects {
        name,
        pencil,
        number,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_rasterizes_with_bundled_font() {
        let (w, h, px) = rasterize_text("Fidget Spider Web XL · Пластина", 64).unwrap();
        assert!(w > h * 5, "{w}×{h}");
        assert!(px.chunks(4).filter(|p| p[3] > 128).count() > 500);
        let (w2, h2, _) = rasterize_text("01", 64).unwrap();
        assert_eq!(h2, h);
        assert!(w2 < w / 5);
    }

    #[test]
    fn labels_sit_above_back_edge_and_number_in_right_gap() {
        let lo = Vec2::ZERO;
        let hi = Vec2::splat(256.);
        let r = label_rects(lo, hi, Some(8.), 1.3);
        let (a, b) = r.name.unwrap();
        assert!(a.y > hi.y && b.x < hi.x, "{a} {b}");
        assert!(r.pencil.0.x > b.x);
        assert!(r.number.0.x > hi.x && r.number.1.x < hi.x * 1.2);
        // Задовга назва стискається, щоб не вилізти за пластину.
        let long = label_rects(lo, hi, Some(80.), 1.3);
        assert!(long.pencil.1.x <= hi.x + 0.01, "{:?}", long.pencil);
    }

    #[test]
    fn decal_rects_follow_bambu_layout() {
        let [left, bottom] = bambu_decal_rects(Vec2::splat(256.));
        assert_eq!(left.0, Vec2::new(10., 52.));
        assert_eq!(bottom.0, Vec2::new(74., -10.));
        let [mini, _] = bambu_decal_rects(Vec2::splat(180.));
        assert_eq!(mini.0, Vec2::new(10., 35.));
    }

    #[test]
    fn free_span_skips_logo_in_label_column() {
        // Логотип 100×100 на рамці 0..100: зайнято верхню чверть лівої колонки (як «Bambu
        // Lab» у `bbl-3dp-logo.svg`) і нижню смугу.
        let mut px = vec![0u8; 100 * 100 * 4];
        for y in 0..100 {
            for x in 0..100 {
                if (x < 20 && (5..25).contains(&y)) || y > 95 {
                    px[(y * 100 + x) * 4 + 3] = 255;
                }
            }
        }
        let mask = Mask::from_rgba(100, 100, &px, 100);
        let body = (Vec2::ZERO, Vec2::splat(100.));
        // Колонка x 5..10, підпис хоче 40 мм униз від y = 98.
        let (b, t) = mask.free_span(body, (5., 10.), (10., 98.), 40., 2.).unwrap();
        assert!(t <= 75. - 2. + 0.01 && t > 70., "верх {t}");
        assert!(t - b >= 40., "{b}..{t}");
        // Колонка правіше логотипа вільна від самого верху.
        let (_, t) = mask.free_span(body, (40., 45.), (10., 90.), 40., 2.).unwrap();
        assert_eq!(t, 90.);
        // Порожня маска — уся колонка.
        let empty = Mask::from_rgba(10, 10, &[0; 400], 10);
        assert_eq!(empty.free_span(body, (5., 10.), (10., 90.), 40., 2.), Some((10., 90.)));
    }

    #[test]
    fn procedural_bed_covers_plate_with_margins() {
        let mesh = procedural_bed(Vec2::ZERO, Vec2::splat(256.));
        let (lo, hi) = mesh.bounds().unwrap();
        assert!(lo.x < 0. && lo.y <= -9.9 && hi.x > 256. && hi.y >= 265.9);
        assert!(hi.z <= 0.);
    }
}
