use std::{
    sync::{Arc, Mutex, mpsc},
    time::{Duration, Instant},
};

use anyhow::{Context as _, Result};
use bytemuck::{Pod, Zeroable};
use wgpu::util::DeviceExt as _;

use std::ops::Range;

use crate::{BED_SIZE, Camera, Mesh};

#[path = "section.rs"]
mod section;
#[path = "plate_icons.rs"]
pub(crate) mod plate_icons;
#[path = "bed.rs"]
pub(crate) mod bed;
#[path = "photo.rs"]
pub mod photo;

const COLOR_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Bgra8UnormSrgb;
const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;
const SAMPLES: u32 = 4;
// Кольори тут лінійні: ціль sRGB, GPU сам застосує гаму при записі.
/// Фон робочої області Zed One Dark (#282c33) у лінійному просторі.
const CLEAR: wgpu::Color = wgpu::Color {
    r: 0.0212,
    g: 0.0252,
    b: 0.0331,
    a: 1.0,
};
const LIGHT_CLEAR: wgpu::Color = wgpu::Color {
    r: 0.85499,
    g: 0.87962,
    b: 0.91310,
    a: 1.0,
};
/// Модель без вибраного філаменту — фірмовий зелений Bambu (#00AE42), sRGB.
const DEFAULT_MODEL_COLOR: [f32; 4] = [0.0, 0.682, 0.259, 1.0];

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Globals {
    view_proj: [[f32; 4]; 4],
    eye: [f32; 4],
    light_top: [f32; 4],
    light_front: [f32; 4],
    model_color: [f32; 4],
    toolpath_view: [u32; 4],
    clip_plane: [f32; 4],
    cut_plane: [f32; 4],
    cut_lo: [f32; 4],
    cut_hi: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct MeshVertex {
    pos: [f32; 3],
    normal: [f32; 3],
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct FlatVertex {
    pub pos: [f32; 3],
    pub color: [f32; 4],
}

/// Цілі рендеру під конкретний розмір кадру; перебудовуються при зміні розміру.
struct Targets {
    width: u32,
    height: u32,
    msaa: wgpu::TextureView,
    depth: wgpu::TextureView,
    resolve: wgpu::Texture,
    readback: wgpu::Buffer,
    padded_row: u32,
}

/// Completed tightly packed BGRA8 sRGB frame. Dimensions belong to this submission,
/// even when the viewport was resized while it was in flight.
#[derive(Debug)]
pub struct RenderedFrame {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

struct PendingFrame {
    targets: Targets,
    completion: mpsc::Receiver<Result<(), wgpu::BufferAsyncError>>,
    started: Instant,
}

/// Матриця сітки (4 стовпці) як instance-атрибути `vs_mesh`.
const TRANSFORM_ATTRS: [wgpu::VertexAttribute; 4] =
    wgpu::vertex_attr_array![2 => Float32x4, 3 => Float32x4, 4 => Float32x4, 5 => Float32x4];

/// Ключ сітки об’єкта, що зараз перетягується гізмо.
pub const DRAG_MESH: u32 = u32::MAX;

/// Сітка однієї частини в її локальних координатах; спільна для всіх копій.
struct VolumeMesh {
    vertices: wgpu::Buffer,
    count: u32,
}

/// Елементи сцени, згруповані за сіткою: одна інстанс-матриця на копію частини.
struct SceneItems {
    transforms: wgpu::Buffer,
    capacity: usize,
    /// (сітка, діапазон інстансів у `transforms`)
    groups: Vec<(u64, std::ops::Range<u32>)>,
}

struct SceneMesh {
    signature: u64,
    vertices: (wgpu::Buffer, u32),
    offset: wgpu::Buffer,
}

pub struct Renderer {
    device: wgpu::Device,
    queue: wgpu::Queue,
    adapter_info: wgpu::AdapterInfo,
    globals: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
    mesh_pipeline: wgpu::RenderPipeline,
    modifier_pipeline: wgpu::RenderPipeline,
    ghost_depth_pipeline: wgpu::RenderPipeline,
    ghost_pipeline: wgpu::RenderPipeline,
    section_pipeline: wgpu::RenderPipeline,
    bed_pipeline: wgpu::RenderPipeline,
    bed_grid_pipeline: wgpu::RenderPipeline,
    line_pipeline: wgpu::RenderPipeline,
    overlay_pipeline: wgpu::RenderPipeline,
    surface_overlay_pipeline: wgpu::RenderPipeline,
    toolpath_pipeline: wgpu::RenderPipeline,
    toolpath_layout: wgpu::BindGroupLayout,
    /// Прив’язки сегментів траєкторії шматками в межах ліміту storage-буфера:
    /// (bind group, діапазон сегментів).
    toolpath_chunks: Vec<(wgpu::BindGroup, Range<u32>)>,
    /// Те саме для спрощеної копії (`ToolpathGpuData::lod`).
    lod_chunks: Vec<(wgpu::BindGroup, Range<u32>)>,
    toolpath_lod: bool,
    plate_icons: plate_icons::PlateIcons,
    decals: bed::Decals,
    bed_assets: Option<bed::BedAssets>,
    /// Які зображення `bed_assets` (логотип, написи) прочитались.
    bed_images_ok: Vec<bool>,
    plate_labels: Vec<bed::PlateLabel>,
    label_colors: bed::LabelColors,
    label_rects: std::collections::HashMap<u32, bed::LabelRects>,
    plate: (wgpu::Buffer, u32),
    grid: (wgpu::Buffer, u32),
    bed_bounds: ([f32; 2], [f32; 2]),
    light_theme: bool,
    clear_override: Option<wgpu::Color>,
    scene_meshes: std::collections::HashMap<u32, SceneMesh>,
    volume_meshes: std::collections::HashMap<u64, VolumeMesh>,
    scene_items: Option<SceneItems>,
    scene_beds: Option<Vec<(u32, glam::Vec3)>>,
    active_bed: u32,
    zero_offset: wgpu::Buffer,
    mesh: Option<(wgpu::Buffer, u32)>,
    model_color: [f32; 4],
    clip_plane: Option<[f32; 4]>,
    cut_tint: Option<([f32; 4], glam::Vec3, glam::Vec3)>,
    section_plane: Option<([f32; 4], (glam::Vec3, glam::Vec3))>,
    section_mesh: Option<(wgpu::Buffer, u32)>,
    modifiers: Option<(wgpu::Buffer, u32)>,
    highlight: Vec<(wgpu::Buffer, u32)>,
    hover: Option<(wgpu::Buffer, u32)>,
    overlay: Option<(wgpu::Buffer, u32)>,
    surface_overlay: Option<(wgpu::Buffer, u32)>,
    toolpath: Option<crate::ToolpathGpuData>,
    /// Видимі інстанси екструзії й вершини переміщень; `None` — усе.
    visible: Option<(Range<u32>, Range<u32>)>,
    show_travel: bool,
    show_bed: bool,
    /// Моделі сцени — напівпрозорим силуетом (`set_ghost_models`).
    ghost_models: bool,
    flat_toolpath: bool,
    targets: Option<Targets>,
    pending: Option<PendingFrame>,
    ready: Option<Arc<RenderedFrame>>,
    device_lost: Arc<Mutex<Option<String>>>,
    /// Режим «Фото»: `None`, доки його не ввімкнули (ресурси не створюються).
    photo: Option<photo::Photo>,
    /// Модель столу активної пластини в координатах траєкторії (для «Фото») і зсув
    /// активної пластини в сцені (там лежать написи столу).
    bed_body: Option<(Mesh, glam::Vec3)>,
    bed_shift: glam::Vec3,
    pub last_frame_time: Duration,
}

impl Renderer {
    pub fn new() -> Result<Self> {
        pollster::block_on(Self::new_async())
    }

    async fn new_async() -> Result<Self> {
        let instance =
            wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                ..Default::default()
            })
            .await
            .context("немає wgpu-адаптера")?;
        // Траєкторія читається зі storage-буфера у вершинному шейдері; без цього пайплайн
        // впав би validation panic замість зрозумілої помилки.
        if !adapter
            .get_downlevel_capabilities()
            .flags
            .contains(wgpu::DownlevelFlags::VERTEX_STORAGE)
        {
            anyhow::bail!(
                "wgpu-адаптер {} не підтримує storage-буфери у вершинному шейдері",
                adapter.get_info().name
            );
        }
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("viewer"),
                // Великі траєкторії: буфер і прив’язки — скільки дає адаптер, а не 256/128 MiB.
                required_limits: wgpu::Limits {
                    max_buffer_size: adapter.limits().max_buffer_size,
                    max_storage_buffer_binding_size: adapter
                        .limits()
                        .max_storage_buffer_binding_size,
                    ..Default::default()
                },
                // Типовий `Performance` бере у gpu-allocator блоки по 128–256 МБ на тип пам’яті:
                // 276 МБ VRAM ще на сторінці Home без жодної сцени. Великі буфери (траєкторія,
                // сітки) однаково отримують окремі виділення, тож дрібніші блоки — це лише
                // менший «запас» про всяк випадок. GPUI для свого пристрою робить так само.
                memory_hints: wgpu::MemoryHints::MemoryUsage,
                ..Default::default()
            })
            .await
            .context("не створився wgpu-пристрій")?;

        let device_lost = Arc::new(Mutex::new(None));
        let lost = Arc::clone(&device_lost);
        device.set_device_lost_callback(move |reason, message| {
            *lost.lock().unwrap_or_else(|e| e.into_inner()) =
                Some(format!("{reason:?}: {message}"));
        });

        let globals = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("globals"),
            size: size_of::<Globals>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bind_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("globals"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("globals"),
            layout: &bind_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: globals.as_entire_binding(),
            }],
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("viewer"),
            bind_group_layouts: &[Some(&bind_layout)],
            ..Default::default()
        });
        let shader = device.create_shader_module(wgpu::include_wgsl!("shader.wgsl"));
        let section_pipeline = section::pipeline(&device, &layout);
        let plate_icons = plate_icons::PlateIcons::new(&device, &queue, &bind_layout);
        let decals = bed::Decals::new(&device, &bind_layout);

        let mesh_attrs = wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x3];
        let flat_attrs = wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x4];
        let pipeline = |label,
                        vs,
                        fs,
                        stride,
                        attrs: &[wgpu::VertexAttribute],
                        topology,
                        depth_write: bool| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(&layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some(vs),
                    compilation_options: Default::default(),
                    buffers: &{
                        let mut buffers = vec![Some(wgpu::VertexBufferLayout {
                            array_stride: stride,
                            step_mode: wgpu::VertexStepMode::Vertex,
                            attributes: attrs,
                        })];
                        if vs == "vs_mesh" {
                            buffers.push(Some(wgpu::VertexBufferLayout {
                                array_stride: 64,
                                step_mode: wgpu::VertexStepMode::Instance,
                                attributes: &TRANSFORM_ATTRS,
                            }));
                        }
                        buffers
                    },
                },
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some(fs),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: COLOR_FORMAT,
                        blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                primitive: wgpu::PrimitiveState {
                    topology,
                    ..Default::default()
                },
                depth_stencil: Some(wgpu::DepthStencilState {
                    format: DEPTH_FORMAT,
                    depth_write_enabled: Some(depth_write),
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
        };
        let mesh_pipeline = pipeline(
            "mesh",
            "vs_mesh",
            "fs_mesh",
            size_of::<MeshVertex>() as u64,
            &mesh_attrs,
            wgpu::PrimitiveTopology::TriangleList,
            true,
        );
        // Підсвітка поверхні (вимірювання): трикутники з перевіркою глибини, без запису в неї.
        let surface_overlay_pipeline = pipeline(
            "surface_overlay",
            "vs_line",
            "fs_line",
            size_of::<FlatVertex>() as u64,
            &flat_attrs,
            wgpu::PrimitiveTopology::TriangleList,
            false,
        );
        let modifier_pipeline = pipeline(
            "modifiers",
            "vs_mesh",
            "fs_modifier",
            size_of::<MeshVertex>() as u64,
            &mesh_attrs,
            wgpu::PrimitiveTopology::TriangleList,
            false,
        );
        let ghost_depth_pipeline = pipeline(
            "ghost_depth",
            "vs_mesh",
            "fs_ghost_depth",
            size_of::<MeshVertex>() as u64,
            &mesh_attrs,
            wgpu::PrimitiveTopology::TriangleList,
            true,
        );
        let ghost_pipeline = pipeline(
            "ghost",
            "vs_mesh",
            "fs_ghost",
            size_of::<MeshVertex>() as u64,
            &mesh_attrs,
            wgpu::PrimitiveTopology::TriangleList,
            false,
        );
        // Стіл і оверлей не пишуть глибину: стіл знизу напівпрозорий, гізмо завжди поверх.
        let bed_pipeline = pipeline(
            "bed",
            "vs_line",
            "fs_bed",
            size_of::<FlatVertex>() as u64,
            &flat_attrs,
            wgpu::PrimitiveTopology::TriangleList,
            false,
        );
        let bed_grid_pipeline = pipeline(
            "bed_grid",
            "vs_line",
            "fs_bed",
            size_of::<FlatVertex>() as u64,
            &flat_attrs,
            wgpu::PrimitiveTopology::LineList,
            false,
        );
        let line_pipeline = pipeline(
            "lines",
            "vs_line",
            "fs_line",
            size_of::<FlatVertex>() as u64,
            &flat_attrs,
            wgpu::PrimitiveTopology::LineList,
            false,
        );
        let overlay_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("overlay"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_line"),
                compilation_options: Default::default(),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: size_of::<FlatVertex>() as u64,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &flat_attrs,
                })],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_line"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: COLOR_FORMAT,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                ..Default::default()
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: DEPTH_FORMAT,
                depth_write_enabled: Some(false),
                depth_compare: Some(wgpu::CompareFunction::Always),
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

        // Сегменти читаються зі storage-буфера за vertex_index (vertex pulling): інстансинг
        // 4-вершинних стрічок на RDNA втричі повільніший за один великий draw.
        let toolpath_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("toolpath"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });
        let toolpath_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("toolpath"),
                bind_group_layouts: &[Some(&bind_layout), Some(&toolpath_layout)],
                ..Default::default()
            });
        let toolpath_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("toolpath"),
            layout: Some(&toolpath_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_toolpath"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_toolpath"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: COLOR_FORMAT,
                    // Лінії непрозорі: змішування лише здорожчувало б MSAA-запис.
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                // Стрічка завжди повернута до камери, але обхід залежить від напрямку руху.
                cull_mode: None,
                ..Default::default()
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: DEPTH_FORMAT,
                depth_write_enabled: Some(true),
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

        let bed_bounds = ([0.0; 2], [BED_SIZE; 2]);
        let plate = upload(
            &device,
            "plate",
            &plate_vertices(bed_bounds.0, bed_bounds.1, [0.0; 4]),
        );
        let grid = upload(
            &device,
            "grid",
            &grid_vertices(bed_bounds.0, bed_bounds.1, [0.0; 4], [0.0; 4]),
        );

        let zero_offset = upload(
            &device,
            "identity transform",
            &[glam::Mat4::IDENTITY.to_cols_array()],
        )
        .0;
        Ok(Self {
            adapter_info: adapter.get_info(),
            device,
            queue,
            globals,
            bind_group,
            mesh_pipeline,
            modifier_pipeline,
            ghost_depth_pipeline,
            ghost_pipeline,
            section_pipeline,
            bed_pipeline,
            bed_grid_pipeline,
            plate_icons,
            decals,
            bed_assets: None,
            bed_images_ok: Vec::new(),
            plate_labels: Vec::new(),
            label_colors: Default::default(),
            label_rects: Default::default(),
            volume_meshes: Default::default(),
            scene_items: None,
            line_pipeline,
            overlay_pipeline,
            surface_overlay_pipeline,
            toolpath_pipeline,
            toolpath_layout,
            toolpath_chunks: Vec::new(),
            lod_chunks: Vec::new(),
            toolpath_lod: true,
            plate,
            grid,
            bed_bounds,
            light_theme: false,
            clear_override: None,
            scene_meshes: Default::default(),
            scene_beds: None,
            active_bed: 1,
            zero_offset,
            mesh: None,
            model_color: DEFAULT_MODEL_COLOR,
            clip_plane: None,
            cut_tint: None,
            section_plane: None,
            section_mesh: None,
            modifiers: None,
            highlight: Vec::new(),
            hover: None,
            overlay: None,
            surface_overlay: None,
            toolpath: None,
            visible: None,
            show_travel: false,
            show_bed: true,
            ghost_models: false,
            flat_toolpath: false,
            targets: None,
            pending: None,
            ready: None,
            device_lost,
            photo: None,
            bed_body: None,
            bed_shift: glam::Vec3::ZERO,
            last_frame_time: Duration::ZERO,
        })
    }

    /// Напр. «AMD Radeon 780M (RADV PHOENIX) · Vulkan».
    pub fn adapter_summary(&self) -> String {
        format!(
            "{} · {:?}",
            self.adapter_info.name, self.adapter_info.backend
        )
    }

    pub fn device(&self) -> &wgpu::Device {
        &self.device
    }

    pub fn queue(&self) -> &wgpu::Queue {
        &self.queue
    }

    pub fn set_mesh(&mut self, mesh: &Mesh) {
        self.scene_meshes.clear();
        // Одиночна сітка (Preview, мініатюри) — інший режим: елементи сцени «Підготовки»
        // не малюються, доки застосунок знову не викличе `set_scene_items`. Сітки частин
        // лишаються завантаженими для повернення на «Підготовку».
        self.scene_items = None;
        self.set_scene_beds(None, 1);
        let vertices: Vec<MeshVertex> = mesh
            .positions
            .iter()
            .zip(&mesh.normals)
            .map(|(&pos, &normal)| MeshVertex { pos, normal })
            .collect();
        self.mesh = (!vertices.is_empty()).then(|| upload(&self.device, "mesh", &vertices));
    }

    /// Чи завантажена сітка частини з ключем `key` (застосунок ключує за ідентичністю меша).
    pub fn has_volume_mesh(&self, key: u64) -> bool {
        self.volume_meshes.contains_key(&key)
    }

    /// Завантажує сітку частини в локальних координатах. Перетворення — у `set_scene_items`.
    pub fn set_volume_mesh(&mut self, key: u64, mesh: &Mesh) {
        let vertices: Vec<MeshVertex> = mesh
            .positions
            .iter()
            .zip(&mesh.normals)
            .map(|(&pos, &normal)| MeshVertex { pos, normal })
            .collect();
        if vertices.is_empty() {
            self.volume_meshes.remove(&key);
            return;
        }
        let (vertices, count) = upload(&self.device, "volume mesh", &vertices);
        self.volume_meshes.insert(key, VolumeMesh { vertices, count });
    }

    /// Прибирає сітки частин, яких немає в `keep`.
    pub fn retain_volume_meshes(&mut self, keep: &std::collections::HashSet<u64>) {
        self.volume_meshes.retain(|key, _| keep.contains(key));
    }

    /// Що малювати: (сітка частини, повна світова матриця). Зміна лише матриць —
    /// перетягування, поворот, розкладання — переписує кілька кілобайт, без вершин.
    pub fn set_scene_items(&mut self, items: &[(u64, glam::Mat4)]) {
        self.mesh = None;
        if items.is_empty() {
            self.scene_items = None;
            return;
        }
        let mut sorted: Vec<&(u64, glam::Mat4)> = items.iter().collect();
        sorted.sort_by_key(|(key, _)| *key);
        let mut groups: Vec<(u64, std::ops::Range<u32>)> = Vec::new();
        let mut matrices = Vec::with_capacity(sorted.len());
        for (i, (key, transform)) in sorted.iter().enumerate() {
            matrices.push(transform.to_cols_array());
            let i = i as u32;
            match groups.last_mut() {
                Some((last, range)) if last == key => range.end = i + 1,
                _ => groups.push((*key, i..i + 1)),
            }
        }
        let bytes: &[u8] = bytemuck::cast_slice(&matrices);
        let reuse = self
            .scene_items
            .take()
            .filter(|items| items.capacity >= matrices.len());
        let (transforms, capacity) = match reuse {
            Some(items) => (items.transforms, items.capacity),
            None => {
                let capacity = matrices.len().next_power_of_two().max(16);
                let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("scene item transforms"),
                    size: (capacity * 64) as u64,
                    usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                });
                (buffer, capacity)
            }
        };
        self.queue.write_buffer(&transforms, 0, bytes);
        self.scene_items = Some(SceneItems {
            transforms,
            capacity,
            groups,
        });
    }

    /// Unchanged plates retain their GPU allocation; offsets are independent instances.
    pub fn set_scene_mesh(&mut self, number: u32, mesh: &Mesh, offset: glam::Vec3) {
        use std::hash::{Hash, Hasher};
        self.mesh = None;
        if mesh.positions.is_empty() {
            self.scene_meshes.remove(&number);
            return;
        }
        let mut hash = std::collections::hash_map::DefaultHasher::new();
        bytemuck::cast_slice::<_, u8>(&mesh.positions).hash(&mut hash);
        bytemuck::cast_slice::<_, u8>(&mesh.normals).hash(&mut hash);
        let signature = hash.finish();
        if let Some(cached) = self.scene_meshes.get_mut(&number)
            && cached.signature == signature
        {
            let m = glam::Mat4::from_translation(offset).to_cols_array();
            self.queue
                .write_buffer(&cached.offset, 0, bytemuck::cast_slice(&m));
            return;
        }
        let vertices: Vec<MeshVertex> = mesh
            .positions
            .iter()
            .zip(&mesh.normals)
            .map(|(&pos, &normal)| MeshVertex { pos, normal })
            .collect();
        let offset_buffer = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("scene mesh transform"),
                contents: bytemuck::cast_slice(&glam::Mat4::from_translation(offset).to_cols_array()),
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            });
        self.scene_meshes.insert(
            number,
            SceneMesh {
                signature,
                vertices: upload(&self.device, "scene plate mesh", &vertices),
                offset: offset_buffer,
            },
        );
    }

    pub fn retain_scene_meshes(&mut self, numbers: &[u32]) {
        self.scene_meshes
            .retain(|number, _| *number == DRAG_MESH || numbers.contains(number));
    }

    /// Повна матриця сітки сцени без перезавантаження вершин (живе перетягування).
    pub fn set_scene_transform(&mut self, number: u32, transform: glam::Mat4) {
        if let Some(mesh) = self.scene_meshes.get(&number) {
            self.queue.write_buffer(
                &mesh.offset,
                0,
                bytemuck::cast_slice(&transform.to_cols_array()),
            );
        }
    }

    pub fn remove_scene_mesh(&mut self, number: u32) {
        self.scene_meshes.remove(&number);
    }

    /// Кнопки пластини на площині столу; порожній список ховає їх.
    pub fn set_plate_icons(&mut self, quads: &[plate_icons::PlateIconQuad]) {
        self.plate_icons.set(&self.device, quads, !self.light_theme);
    }

    pub fn set_scene_beds(&mut self, beds: Option<Vec<(u32, glam::Vec3)>>, active: u32) {
        if self.scene_beds == beds && self.active_bed == active {
            return;
        }
        self.scene_beds = beds;
        self.active_bed = active;
        if let Some(beds) = &self.scene_beds {
            self.scene_meshes
                .retain(|number, _| {
                    *number == 0 || *number == DRAG_MESH || beds.iter().any(|(n, _)| n == number)
                });
        }
        self.rebuild_bed();
    }

    /// Discard mesh fragments where `dot(plane.xyz, world) + plane.w > 0`.
    /// For a horizontal cut at Z=h use `[0., 0., 1., -h]`. None disables it.
    /// This is a visual cutaway only: no geometry edits or caps; toolpaths are unaffected.
    /// Non-finite coefficients disable clipping.
    pub fn set_clip_plane(&mut self, plane: Option<[f32; 4]>) {
        self.clip_plane = plane.filter(|p| p.iter().all(|v| v.is_finite()));
    }

    /// Прев’ю інструмента «Різання»: фрагменти моделі всередині рамки `bounds` над площиною
    /// `dot(plane.xyz, world) + plane.w = 0` фарбуються блакитним, під нею — пурпурним.
    /// Це лише колір фрагментів: геометрія й траєкторії не змінюються. `None` вимикає.
    pub fn set_cut_tint(&mut self, tint: Option<([f32; 4], (glam::Vec3, glam::Vec3))>) {
        self.cut_tint = tint
            .filter(|(p, (lo, hi))| p.iter().all(|v| v.is_finite()) && lo.is_finite() && hi.is_finite())
            .map(|(p, (lo, hi))| (p, lo, hi));
    }

    /// Opt-in cyan cut guide on `dot(plane.xyz, world) + plane.w = 0`.
    /// The rectangle spans the projected model bounds plus a margin. It is depth
    /// tested, double sided, and never writes depth or changes mesh/toolpath clipping.
    /// `None`, invalid bounds, or an invalid/zero normal clears it. Call with `None`
    /// when leaving Prepare; clipping alone never enables this guide. Identical
    /// plane/bounds inputs retain the existing GPU buffer.
    pub fn set_section_plane(&mut self, section: Option<([f32; 4], (glam::Vec3, glam::Vec3))>) {
        if self.section_plane == section {
            return;
        }
        self.section_mesh = section.and_then(|(plane, bounds)| {
            section::vertices(plane, bounds)
                .map(|vertices| upload(&self.device, "section_plane", &vertices))
        });
        self.section_plane = section.filter(|_| self.section_mesh.is_some());
    }

    /// Preview-only cyan translucent helper triangles, depth tested against the model
    /// and toolpaths, without depth writes. An empty mesh clears them. Transparency
    /// uses draw order (no sorting/OIT); helpers do not modify printable geometry.
    pub fn set_modifiers(&mut self, mesh: &Mesh) {
        let vertices: Vec<MeshVertex> = mesh
            .positions
            .iter()
            .zip(&mesh.normals)
            .map(|(&pos, &normal)| MeshVertex { pos, normal })
            .collect();
        self.modifiers =
            (!vertices.is_empty()).then(|| upload(&self.device, "modifiers", &vertices));
    }

    /// Рамка навколо вибраного обʼєкта (як виділення в Bambu Studio).
    pub fn set_highlight(&mut self, bounds: Option<(glam::Vec3, glam::Vec3)>) {
        self.set_highlights(bounds);
    }

    /// Рамки всіх вибраних обʼєктів (мультивибір).
    pub fn set_highlights(&mut self, bounds: impl IntoIterator<Item = (glam::Vec3, glam::Vec3)>) {
        // Акцент Zed (#74ade8) — той самий колір, що й вибір у списку обʼєктів.
        self.highlight = bounds
            .into_iter()
            .map(|(lo, hi)| {
                box_lines(
                    &self.device,
                    "highlight",
                    lo,
                    hi,
                    [0.175, 0.418, 0.807, 1.0],
                )
            })
            .collect();
    }

    /// Блідіша рамка обʼєкта під курсором — щоб було видно, що саме схопиш.
    pub fn set_hover(&mut self, bounds: Option<(glam::Vec3, glam::Vec3)>) {
        self.hover = bounds
            .map(|(lo, hi)| box_lines(&self.device, "hover", lo, hi, [0.30, 0.33, 0.40, 1.0]));
    }

    /// Колір моделі на «Підготовці» (sRGB, 0..1) — зазвичай колір філаменту.
    pub fn set_model_color(&mut self, rgb: [f32; 3]) {
        self.model_color = [rgb[0], rgb[1], rgb[2], 1.0];
    }

    /// Трикутники поверх сцени без перевірки глибини: гізмо, маркер сопла. Кольори лінійні.
    pub fn set_overlay(&mut self, triangles: &[FlatVertex]) {
        self.overlay = (!triangles.is_empty()).then(|| upload(&self.device, "overlay", triangles));
    }

    /// Трикутники, притиснуті до поверхні моделі: перекриваються її геометрією (на відміну від
    /// [`Self::set_overlay`]). Підсвітка ознак вимірювання. Кольори лінійні.
    pub fn set_surface_overlay(&mut self, triangles: &[FlatVertex]) {
        self.surface_overlay =
            (!triangles.is_empty()).then(|| upload(&self.device, "surface_overlay", triangles));
    }

    pub fn set_toolpath(&mut self, toolpath: &crate::ToolpathGpuData) {
        if !self
            .toolpath
            .as_ref()
            .is_some_and(|tp| tp.instance_buffer == toolpath.instance_buffer)
        {
            self.toolpath_chunks = self.toolpath_chunks_for(
                &toolpath.instance_buffer,
                toolpath.total_instances,
                false,
            );
            self.lod_chunks = toolpath.lod.as_ref().map_or_else(Vec::new, |lod| {
                self.toolpath_chunks_for(&lod.buffer, lod.count, true)
            });
        }
        self.toolpath = Some(toolpath.clone());
    }

    /// Буфер сегментів ділиться на прив’язки ≤ `max_storage_buffer_binding_size`; зсув
    /// кожної кратний 256 (64 сегменти × 44 байти = 11 × 256).
    /// `lod` — копія для загального плану: у ній немає меж повзунків (їх малює повний буфер).
    fn toolpath_chunks_for(
        &self,
        buffer: &wgpu::Buffer,
        total: u32,
        lod: bool,
    ) -> Vec<(wgpu::BindGroup, Range<u32>)> {
        const STRIDE: u64 = crate::toolpath::GpuInstance::WORDS as u64 * 4;
        let limit = self.device.limits().max_storage_buffer_binding_size;
        let per_chunk = ((limit / STRIDE) / 64 * 64).max(64) as u32;
        (0..total.div_ceil(per_chunk))
            .map(|i| {
                let range = i * per_chunk..((i + 1) * per_chunk).min(total);
                let base = upload_uniform(&self.device, [range.start, u32::from(lod), 0, 0]);
                let bind = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("toolpath"),
                    layout: &self.toolpath_layout,
                    entries: &[
                        wgpu::BindGroupEntry {
                            binding: 0,
                            resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                                buffer,
                                offset: u64::from(range.start) * STRIDE,
                                size: wgpu::BufferSize::new(u64::from(range.len() as u32) * STRIDE),
                            }),
                        },
                        wgpu::BindGroupEntry {
                            binding: 1,
                            resource: base.as_entire_binding(),
                        },
                    ],
                });
                (bind, range)
            })
            .collect()
    }

    /// Спрощена копія на загальному плані (типово увімкнена); вимкнення — для порівнянь.
    pub fn set_toolpath_lod(&mut self, enabled: bool) {
        self.toolpath_lod = enabled;
    }

    pub fn clear_toolpath(&mut self) {
        self.toolpath = None;
        self.toolpath_chunks.clear();
        self.lod_chunks.clear();
        self.visible = None;
    }

    /// Шари [from, to) поточної траєкторії.
    pub fn set_visible_layers(&mut self, range: Range<usize>) {
        let Some(tp) = &self.toolpath else { return };
        let span = |ranges: &[Range<u32>]| {
            let (a, b) = (range.start.min(ranges.len()), range.end.min(ranges.len()));
            if a < b {
                ranges[a].start..ranges[b - 1].end
            } else {
                0..0
            }
        };
        self.visible = Some((span(&tp.layer_ranges), span(&tp.travel_layer_ranges)));
    }

    /// Точні межі: інстанси екструзії й вершини переміщень (для повзунка рухів у шарі).
    pub fn set_visible_ranges(&mut self, instances: Range<u32>, travel: Range<u32>) {
        self.visible = Some((instances, travel));
    }

    pub fn set_all_layers_visible(&mut self) {
        self.visible = None;
    }

    pub fn set_show_travel(&mut self, show: bool) {
        self.show_travel = show;
    }

    pub fn set_bed_bounds(&mut self, lo: [f32; 2], hi: [f32; 2]) {
        if !lo.iter().chain(&hi).all(|v| v.is_finite()) || hi[0] <= lo[0] || hi[1] <= lo[1] {
            return;
        }
        self.bed_bounds = (lo, hi);
        self.rebuild_bed();
    }

    /// Select the live viewport palette. Repeated calls with the current value do no GPU work.
    pub fn set_light_theme(&mut self, light: bool) {
        if self.light_theme == light {
            return;
        }
        self.light_theme = light;
        self.rebuild_bed();
    }

    /// Стіл вибраного принтера: модель, логотип і написи типу пластини. `None` —
    /// процедурний стіл без написів. Той самий `key` нічого не перебудовує.
    pub fn set_bed_assets(&mut self, assets: Option<bed::BedAssets>) {
        let same = match (&self.bed_assets, &assets) {
            (Some(a), Some(b)) => a.key == b.key,
            (None, None) => true,
            _ => false,
        };
        if same {
            return;
        }
        self.bed_images_ok = match &assets {
            Some(a) => {
                let mut images: Vec<&bed::BedImage> = Vec::new();
                // Індекс 0 — логотип (або порожнє місце), далі написи.
                let empty = bed::BedImage::Png(Vec::new());
                images.push(a.logo.as_ref().unwrap_or(&empty));
                images.extend(a.decals.iter().map(|d| &d.image));
                self.decals.set_bed_images(&self.device, &self.queue, &images)
            }
            None => Vec::new(),
        };
        self.bed_assets = assets;
        self.rebuild_bed();
    }

    /// Назви й номери пластин сцени та їхні кольори (лінійний RGBA).
    pub fn set_plate_labels(&mut self, labels: Vec<bed::PlateLabel>, colors: bed::LabelColors) {
        if self.plate_labels == labels && self.label_colors == colors {
            return;
        }
        self.plate_labels = labels;
        self.label_colors = colors;
        self.rebuild_bed();
    }

    /// Де лежать назва, олівець і номер пластини (світові мм) — для кліків.
    pub fn plate_label_rects(&self, number: u32) -> Option<bed::LabelRects> {
        self.label_rects.get(&number).copied()
    }

    fn rebuild_bed(&mut self) {
        use glam::Vec2;
        let (lo, hi) = self.bed_bounds;
        let scene = self.scene_beds.is_some();
        let beds = self
            .scene_beds
            .clone()
            .unwrap_or_else(|| vec![(1, glam::Vec3::ZERO)]);
        let palette = bed::palette(self.light_theme);
        let (lo2, hi2) = (Vec2::from(lo), Vec2::from(hi));
        let assets = self.bed_assets.clone().unwrap_or_default();
        // Модель столу: верх трохи нижче площини, щоб сітка й написи не мерехтіли.
        let body = assets
            .model
            .clone()
            .filter(|m| m.triangle_count() > 0)
            .unwrap_or_else(|| bed::procedural_bed(lo2, hi2));
        let (body_lo, body_hi) = body.bounds().unwrap_or((lo2.extend(0.), hi2.extend(0.)));
        let sink = glam::Vec3::new(0., 0., -0.05 - body_hi.z);
        let images_ok = |i: usize| self.bed_images_ok.get(i).copied().unwrap_or(false);
        let type_label = assets
            .type_label
            .clone()
            .filter(|_| !(1..=assets.decals.len()).any(images_ok));
        if let Some(photo) = &mut self.photo {
            photo.set_bed(&self.device, &body, sink);
        }
        let mut plates = Vec::new();
        let mut grids = Vec::new();
        let mut under = Vec::new();
        let mut over = Vec::new();
        let mut rects = std::collections::HashMap::new();
        let mut keep = std::collections::HashSet::new();
        let white = [1.0f32; 4];
        for (number, offset) in beds {
            let shift = offset.truncate();
            let (a, b) = (lo2 + shift, hi2 + shift);
            let active = !scene || number == self.active_bed;
            if active {
                self.bed_shift = offset;
                plates.extend(bed::shaded_bed(&body, offset + sink, palette.bed));
                if images_ok(0) {
                    under.push(bed::Quad {
                        key: bed::Key::Bed(0),
                        min: body_lo.truncate() + shift,
                        max: body_hi.truncate() + shift,
                        z: 0.01,
                        color: white,
                        rotate: false,
                    });
                }
                for (i, decal) in assets.decals.iter().enumerate() {
                    if images_ok(i + 1) {
                        under.push(bed::Quad {
                            key: bed::Key::Bed(i + 1),
                            min: decal.min + shift,
                            max: decal.max + shift,
                            z: 0.02,
                            color: white,
                            rotate: false,
                        });
                    }
                }
                if let Some(label) = &type_label
                    && let Some(aspect) = self.decals.text_aspect(&self.device, &self.queue, label)
                {
                    keep.insert(label.clone());
                    // Як напис Bambu: вертикально вздовж лівого краю, згори вниз.
                    let [(min, max), _] = bed::bambu_decal_rects(hi2 - lo2);
                    // На всю довжину місця лівого SVG-напису Bambu, як «Bambu Textured PEI
                    // Plate» у Bambu Studio й OrcaSlicer; короткий підпис не вищий за 1.8 ширини
                    // того місця.
                    let full = max.y - min.y;
                    let mut width = (full / aspect.max(0.1)).min((max.x - min.x) * 1.8);
                    let mut length = (width * aspect).min(full);
                    let mut top = lo2.y + max.y;
                    let x0 = lo2.x + min.x;
                    // Логотип профілю (`bbl-3dp-logo.svg`) має свій вертикальний «Bambu Lab»
                    // у тому самому куті: підпис іде нижче нього, а забракне місця — менший.
                    if images_ok(0)
                        && let Some(mask) = self.decals.logo_mask()
                    {
                        match mask.free_span(
                            (body_lo.truncate(), body_hi.truncate()),
                            (x0, x0 + width),
                            (lo2.y + min.y, top),
                            length,
                            2.,
                        ) {
                            Some((bottom, free_top)) => {
                                let room = free_top - bottom;
                                if room < length {
                                    width *= room / length;
                                    length = room;
                                }
                                top = free_top;
                            }
                            None => length = 0.,
                        }
                    }
                    if length > 1. {
                        under.push(bed::Quad {
                            key: bed::Key::Text(label.clone()),
                            min: Vec2::new(x0, top - length) + shift,
                            max: Vec2::new(x0 + width, top) + shift,
                            z: 0.02,
                            color: bed::srgb([0.81, 0.82, 0.83]),
                            rotate: true,
                        });
                    }
                }
            } else {
                plates.extend(plate_vertices(a.to_array(), b.to_array(), bed::srgb(palette.plate)));
            }
            let line = if active { palette.grid_active } else { palette.grid };
            let mut grid = grid_vertices(lo, hi, bed::srgb(line), bed::srgb(palette.edge));
            for vertex in &mut grid {
                vertex.pos = (glam::Vec3::from(vertex.pos) + offset).to_array();
            }
            grids.extend(grid);
            if scene {
                let label = self.plate_labels.iter().find(|l| l.number == number);
                let name = label.map(|l| l.name.trim()).filter(|n| !n.is_empty());
                let digits = format!("{number:02}");
                let number_aspect = self
                    .decals
                    .text_aspect(&self.device, &self.queue, &digits)
                    .unwrap_or(1.2);
                let name_aspect = name.and_then(|n| self.decals.text_aspect(&self.device, &self.queue, n));
                let _ = self.decals.text_aspect(&self.device, &self.queue, bed::PENCIL_KEY);
                keep.insert(digits.clone());
                let r = bed::label_rects(a, b, name_aspect, number_aspect);
                let color = if active {
                    self.label_colors.active
                } else {
                    self.label_colors.inactive
                };
                if let (Some(name), Some((min, max))) = (name, r.name) {
                    keep.insert(name.to_owned());
                    over.push(bed::Quad { key: bed::Key::Text(name.to_owned()), min, max, z: 0.05, color, rotate: false });
                }
                let mut pencil = color;
                pencil[3] *= 0.7;
                over.push(bed::Quad { key: bed::Key::Text(bed::PENCIL_KEY.into()), min: r.pencil.0, max: r.pencil.1, z: 0.05, color: pencil, rotate: false });
                over.push(bed::Quad { key: bed::Key::Text(digits), min: r.number.0, max: r.number.1, z: 0.05, color, rotate: false });
                rects.insert(number, r);
            }
        }
        self.decals.retain_text(&keep);
        self.decals.set_quads(&self.device, under, over);
        self.label_rects = rects;
        self.plate = upload(&self.device, "plates", &plates);
        self.grid = upload(&self.device, "grids", &grids);
        self.bed_body = Some((body, sink));
    }

    /// Фон сцени з теми ОС (лінійний RGB); `None` — вбудований фон світлої/темної теми.
    /// Кольори ролей ліній G-code від теми не залежать.
    pub fn set_scene_background(&mut self, linear: Option<[f32; 3]>) {
        self.clear_override = linear.map(|[r, g, b]| wgpu::Color {
            r: r as f64,
            g: g as f64,
            b: b as f64,
            a: 1.0,
        });
    }

    fn live_clear(&self) -> wgpu::Color {
        if let Some(c) = self.clear_override {
            return c;
        }
        if self.light_theme { LIGHT_CLEAR } else { CLEAR }
    }

    pub fn set_flat_toolpath(&mut self, flat: bool) {
        self.flat_toolpath = flat;
    }
    /// Моделі (`set_mesh`, сітки й елементи сцени) — напівпрозорим сірим силуетом замість
    /// кольору філаменту: «Перегляд», доки пластину не нарізано.
    pub fn set_ghost_models(&mut self, ghost: bool) {
        self.ghost_models = ghost;
    }

    pub fn has_toolpath(&self) -> bool {
        self.toolpath.is_some()
    }

    pub fn set_show_bed(&mut self, show: bool) {
        self.show_bed = show;
    }

    /// Режим Preview «Фото». `Some` — увімкнути (перший раз запікає IBL і створює
    /// пайплайни) або оновити налаштування; `None` — вимкнути й звільнити ресурси.
    /// Траєкторію для режиму будує [`crate::ToolpathGpuExt::build_photo`] — застосунок
    /// ставить її через [`Self::set_toolpath`] замість звичайної.
    pub fn set_photo(&mut self, settings: Option<photo::PhotoSettings>) {
        match (settings, &mut self.photo) {
            (None, _) => self.photo = None,
            (Some(settings), Some(photo)) => photo.set_settings(&self.queue, settings),
            (Some(settings), None) => {
                let _s = crate::profile::Span::new("viewer::photo_enter");
                let mut photo = photo::Photo::new(
                    &self.device,
                    &self.queue,
                    &self.toolpath_layout,
                    &self.decals,
                    settings,
                );
                let ready = self.bed_body.as_ref().map(|(body, offset)| {
                    photo.set_bed(&self.device, body, *offset);
                });
                self.photo = Some(photo);
                if ready.is_none() {
                    // Стіл ще не будувався (лише стартовий): rebuild_bed передасть його.
                    self.rebuild_bed();
                }
            }
        }
    }

    pub fn photo_active(&self) -> bool {
        self.photo.is_some()
    }

    pub fn photo_settings(&self) -> Option<&photo::PhotoSettings> {
        self.photo.as_ref().map(photo::Photo::settings)
    }

    /// Скільки тривало запікання IBL при вході в режим (з очікуванням GPU).
    pub fn photo_bake_time(&self) -> Option<Duration> {
        self.photo.as_ref().map(photo::Photo::bake_time)
    }

    /// Оцінка VRAM ресурсів «Фото» в байтах (текстури IBL, тіньові карти, буфери).
    pub fn photo_bytes(&self) -> Option<u64> {
        self.photo.as_ref().map(photo::Photo::bytes)
    }

    /// Знімок у режимі «Фото» поза екраном у PNG (RGBA, непрозорий). Розмір довільний у
    /// межах ліміту текстур пристрою; цілі кадру після цього перестворюються під в’юпорт.
    pub fn render_photo_png(&mut self, width: u32, height: u32, camera: &Camera) -> Result<Vec<u8>> {
        anyhow::ensure!(self.photo.is_some(), "режим «Фото» не ввімкнено");
        let limit = self.device.limits().max_texture_dimension_2d;
        anyhow::ensure!(width <= limit && height <= limit, "знімок більший за {limit} px");
        let mut pixels = self.render_target(width, height, camera, self.live_clear(), true);
        for px in pixels.chunks_exact_mut(4) {
            px.swap(0, 2);
            px[3] = 255;
        }
        let img = image::RgbaImage::from_raw(width, height, pixels).context("розмір знімка")?;
        let mut png = Vec::new();
        img.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)?;
        // Великі цілі знімка не тримаємо: наступний кадр в’юпорта створить свої.
        self.targets = None;
        Ok(png)
    }

    /// Видимі інстанси екструзії поточної траєкторії.
    fn visible_instances(&self) -> Range<u32> {
        let total = self.toolpath.as_ref().map_or(0, |tp| tp.total_instances);
        let range = self.visible.as_ref().map_or(0..total, |v| v.0.clone());
        range.start.min(total)..range.end.min(total)
    }

    /// Уніформи «Фото» й ключ тіньових карт; `true` — карти треба перемалювати.
    fn prepare_photo(&mut self, width: u32, height: u32, camera: &Camera) -> bool {
        let visible = self.visible_instances();
        let Some(photo) = &mut self.photo else {
            return false;
        };
        let view = photo::PhotoView {
            camera,
            width,
            height,
            instance_buffer: self.toolpath.as_ref().map(|tp| &tp.instance_buffer),
            visible,
            bounds: self.toolpath.as_ref().and_then(|tp| tp.bounds),
            bed_shift: self.bed_shift,
        };
        photo.prepare(&self.device, &self.queue, &view)
    }

    /// Прив’язки шматків і діапазони вершин валиків для видимих інстансів `range`.
    /// `ordered` — від шарів біля ока (рання перевірка глибини); `shadow` — для тіньових
    /// карт: спрощена копія завжди, якщо є (тексель тіні ≥ допуску LOD).
    fn bead_draws(
        &self,
        tp: &crate::ToolpathGpuData,
        range: Range<u32>,
        camera: &Camera,
        height: u32,
        shadow: bool,
    ) -> Vec<(&wgpu::BindGroup, Range<u32>)> {
        let parts = if shadow {
            match (&tp.lod, self.lod_chunks.is_empty()) {
                (Some(lod), false) => lod.split(range.clone()).map_or_else(
                    || vec![(false, range.clone())],
                    |[head, middle, tail]| vec![(false, head), (true, middle), (false, tail)],
                ),
                _ => vec![(false, range)],
            }
        } else {
            self.toolpath_draws(tp, range, camera, height)
        };
        let mut out = Vec::new();
        for (lod, range) in parts {
            let chunks = if lod {
                &self.lod_chunks
            } else {
                &self.toolpath_chunks
            };
            // Повний буфер — від шарів біля ока до дальніх, щоб рання перевірка глибини
            // відкидала закрите (на близькому плані згори в порядку друку кадр у 10+ разів
            // дорожчий). LOD — лише на загальному плані, там порядок майже не важить.
            let ordered = if lod || shadow {
                vec![range]
            } else {
                toolpath_order(&tp.layer_ranges, &tp.layer_z, range, camera.eye().z)
            };
            // 6 вершин (два трикутники) на сегмент; індекс у шматку — vertex_index / 6.
            for range in ordered {
                for (bind, chunk) in chunks {
                    let (a, b) = (range.start.max(chunk.start), range.end.min(chunk.end));
                    if a < b {
                        out.push((bind, (a - chunk.start) * 6..(b - chunk.start) * 6));
                    }
                }
            }
        }
        out
    }

    /// Рендерить кадр і повертає пікселі BGRA (sRGB), рядок за рядком без вирівнювання.
    pub fn render(&mut self, width: u32, height: u32, camera: &Camera) -> Vec<u8> {
        self.render_target(width, height, camera, self.live_clear(), self.show_bed)
    }

    pub fn render_target(
        &mut self,
        width: u32,
        height: u32,
        camera: &Camera,
        clear: wgpu::Color,
        show_bed: bool,
    ) -> Vec<u8> {
        let started = Instant::now();
        self.check_device().expect("viewer device lost");
        let t = self
            .take_targets(width, height)
            .expect("invalid snapshot dimensions");
        let photo_shadows = self.prepare_photo(width, height, camera);
        self.encode_frame(&t, camera, clear, show_bed, photo_shadows);
        let (sender, receiver) = mpsc::sync_channel(1);
        t.readback
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |result| {
                let _ = sender.send(result);
            });
        self.device
            .poll(wgpu::PollType::wait_indefinitely())
            .expect("wgpu poll failed");
        receiver
            .recv()
            .expect("readback callback dropped")
            .expect("readback map failed");
        let pixels = read_pixels(&t).expect("readback range");
        t.readback.unmap();
        self.targets = Some(t);
        self.last_frame_time = started.elapsed();
        pixels
    }

    /// Submit at most one live frame. Returns false while a frame is pending; the
    /// requested dimensions/camera are not queued in that case. Never waits for GPU
    /// completion. Zero dimensions clamp to one; oversized targets return an error.
    /// Resources are bounded: one in-flight target set and one retained ready frame;
    /// interleaved synchronous snapshots may retain one additional idle target set.
    /// Callers retaining returned Arcs are responsible for releasing older frames.
    pub fn submit_frame(&mut self, width: u32, height: u32, camera: &Camera) -> Result<bool> {
        self.check_device()?;
        if self.pending.is_some() {
            return Ok(false);
        }
        let started = Instant::now();
        let targets = {
            let _s = crate::profile::Span::new("viewer::take_targets");
            self.take_targets(width, height)?
        };
        {
            let _s = crate::profile::Span::new("viewer::encode_submit");
            let photo_shadows = self.prepare_photo(targets.width, targets.height, camera);
            self.encode_frame(&targets, camera, self.live_clear(), self.show_bed, photo_shadows);
        }
        let (sender, completion) = mpsc::channel();
        targets
            .readback
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |result| {
                let _ = sender.send(result);
            });
        self.pending = Some(PendingFrame {
            targets,
            completion,
            started,
        });
        Ok(true)
    }

    pub fn has_pending_frame(&self) -> bool {
        self.pending.is_some()
    }

    /// Віддає утримуваний кадр, щоб застосунок міг забрати пікселі без копії
    /// (`Arc::try_unwrap`). Після цього `ready_frame()` — `None` до наступного кадру.
    pub fn release_ready(&mut self) {
        self.ready = None;
    }

    /// Last successful async frame, retained across submissions, resizes and errors.
    pub fn ready_frame(&self) -> Option<&Arc<RenderedFrame>> {
        self.ready.as_ref()
    }

    /// Drive callbacks with Poll (never Wait), returning only newly completed frames.
    /// The app must schedule another poll/repaint while has_pending_frame() is true,
    /// including when no input arrives. Keep displaying ready_frame() in between.
    /// On a resize, compare returned dimensions and submit the newest size after
    /// completion. A rejected submission is not queued: the app must retain dirtiness.
    /// Errors cancel the pending readback; device loss requires a new Renderer.
    pub fn poll_frame(&mut self) -> Result<Option<Arc<RenderedFrame>>> {
        if let Err(error) = self.device.poll(wgpu::PollType::Poll) {
            self.cancel_pending();
            return Err(error.into());
        }
        self.check_device()?;
        let Some(pending) = &self.pending else {
            return Ok(None);
        };
        match pending.completion.try_recv() {
            Err(mpsc::TryRecvError::Empty) => return Ok(None),
            Err(mpsc::TryRecvError::Disconnected) => {
                self.cancel_pending();
                anyhow::bail!("readback callback dropped");
            }
            Ok(Err(error)) => {
                self.cancel_pending();
                return Err(error.into());
            }
            Ok(Ok(())) => {}
        }
        let pending = self.pending.take().unwrap();
        let _s = crate::profile::Span::new("viewer::read_pixels");
        let pixels = read_pixels(&pending.targets);
        pending.targets.readback.unmap();
        let frame = Arc::new(RenderedFrame {
            width: pending.targets.width,
            height: pending.targets.height,
            pixels: pixels?,
        });
        self.targets = Some(pending.targets);
        self.last_frame_time = pending.started.elapsed();
        // Затримка кадру від подання до готових пікселів — те, що відчуває рука на орбіті.
        crate::profile::record("viewer::gpu_frame", self.last_frame_time);
        self.ready = Some(Arc::clone(&frame));
        Ok(Some(frame))
    }

    /// Як `poll_frame`, але чекає GPU до `timeout`: кадр показується в той самий
    /// цикл малювання, без пропущеного кадру вікна. Тайм-аут не є помилкою.
    pub fn wait_frame(&mut self, timeout: std::time::Duration) -> Result<Option<Arc<RenderedFrame>>> {
        if self.pending.is_none() {
            return Ok(None);
        }
        match self.device.poll(wgpu::PollType::Wait {
            submission_index: None,
            timeout: Some(timeout),
        }) {
            Ok(_) | Err(wgpu::PollError::Timeout) => {}
            Err(error) => {
                self.cancel_pending();
                return Err(error.into());
            }
        }
        self.poll_frame()
    }

    /// Очікування GPU для фонового потоку: блокує, доки подана робота (і readback) не
    /// завершиться, або до `timeout`. UI-потік потім забирає кадр через `poll_frame`.
    pub fn gpu_waiter(&self) -> impl FnOnce(std::time::Duration) + Send + 'static {
        let device = self.device.clone();
        move |timeout| {
            let _ = device.poll(wgpu::PollType::Wait {
                submission_index: None,
                timeout: Some(timeout),
            });
        }
    }

    fn cancel_pending(&mut self) {
        // Drop rather than recycle on failure. A lost device may already have
        // destroyed its buffers, in which case unmap itself is a validation error.
        self.pending = None;
    }

    fn check_device(&mut self) -> Result<()> {
        let lost = self
            .device_lost
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        if let Some(message) = lost {
            self.cancel_pending();
            anyhow::bail!("viewer device lost: {message}");
        }
        Ok(())
    }

    fn take_targets(&mut self, width: u32, height: u32) -> Result<Targets> {
        let (width, height) = (width.max(1), height.max(1));
        let limits = self.device.limits();
        anyhow::ensure!(
            width <= limits.max_texture_dimension_2d && height <= limits.max_texture_dimension_2d,
            "viewport dimensions exceed device texture limit"
        );
        let bytes = (u64::from(width) * 4)
            .next_multiple_of(u64::from(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT))
            * u64::from(height);
        anyhow::ensure!(
            bytes <= limits.max_buffer_size,
            "viewport readback exceeds device buffer limit"
        );
        if self
            .targets
            .as_ref()
            .is_some_and(|t| t.width != width || t.height != height)
        {
            self.targets = None;
        }
        Ok(self
            .targets
            .take()
            .unwrap_or_else(|| create_targets(&self.device, width, height)))
    }

    fn encode_frame(
        &self,
        t: &Targets,
        camera: &Camera,
        clear: wgpu::Color,
        show_bed: bool,
        photo_shadows: bool,
    ) {
        let (width, height) = (t.width, t.height);
        let (top, front) = light_dirs(camera);
        let globals = Globals {
            view_proj: camera
                .view_proj(width as f32 / height as f32)
                .to_cols_array_2d(),
            eye: camera.eye().extend(1.0).to_array(),
            light_top: top.extend(0.0).to_array(),
            light_front: front.extend(0.0).to_array(),
            model_color: self.model_color,
            clip_plane: self.clip_plane.unwrap_or([0.0; 4]),
            cut_plane: self.cut_tint.map_or([0.0; 4], |t| t.0),
            cut_lo: self
                .cut_tint
                .map_or([0.0; 4], |t| [t.1.x, t.1.y, t.1.z, 1.0]),
            cut_hi: self
                .cut_tint
                .map_or([0.0; 4], |t| [t.2.x, t.2.y, t.2.z, 0.0]),
            toolpath_view: {
                let total = self.toolpath.as_ref().map_or(0, |tp| tp.total_instances);
                let range = self.visible.as_ref().map_or(0..total, |v| v.0.clone());
                [
                    u32::from(self.flat_toolpath),
                    range.start,
                    range.end.min(total).saturating_sub(1),
                    0,
                ]
            },
        };
        self.queue
            .write_buffer(&self.globals, 0, bytemuck::bytes_of(&globals));
        // Згори стіл — тло, малюється першим; знизу — напівпрозора плита поверх моделі.
        let bed_last = camera.eye().z < 0.0;

        let mut encoder = self.device.create_command_encoder(&Default::default());
        if let Some(photo) = &self.photo {
            self.encode_photo(photo, &mut encoder, t, camera, photo_shadows);
        } else {
            let resolve_view = t.resolve.create_view(&Default::default());
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("viewer"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &t.msaa,
                    depth_slice: None,
                    resolve_target: Some(&resolve_view),
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(clear),
                        store: wgpu::StoreOp::Discard,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &t.depth,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(0.0),
                        store: wgpu::StoreOp::Discard,
                    }),
                    stencil_ops: None,
                }),
                ..Default::default()
            });
            pass.set_bind_group(0, &self.bind_group, &[]);

            let draw_bed = |pass: &mut wgpu::RenderPass<'_>| {
                if !show_bed {
                    return;
                }
                pass.set_pipeline(&self.bed_pipeline);
                pass.set_vertex_buffer(0, self.plate.0.slice(..));
                pass.draw(0..self.plate.1, 0..1);
                self.decals.draw(pass, false);
                pass.set_pipeline(&self.bed_grid_pipeline);
                pass.set_vertex_buffer(0, self.grid.0.slice(..));
                pass.draw(0..self.grid.1, 0..1);
                self.decals.draw(pass, true);
            };
            if !bed_last {
                draw_bed(&mut pass);
            }

            let passes: &[&wgpu::RenderPipeline] = if self.ghost_models {
                &[&self.ghost_depth_pipeline, &self.ghost_pipeline]
            } else {
                &[&self.mesh_pipeline]
            };
            for &mesh_pipeline in passes {
                if let Some((buffer, count)) = &self.mesh {
                    pass.set_pipeline(mesh_pipeline);
                    pass.set_vertex_buffer(1, self.zero_offset.slice(..));
                    pass.set_vertex_buffer(0, buffer.slice(..));
                    pass.draw(0..*count, 0..1);
                }

                for mesh in self.scene_meshes.values() {
                    pass.set_pipeline(mesh_pipeline);
                    pass.set_vertex_buffer(0, mesh.vertices.0.slice(..));
                    pass.set_vertex_buffer(1, mesh.offset.slice(..));
                    pass.draw(0..mesh.vertices.1, 0..1);
                }

                if let Some(items) = &self.scene_items {
                    pass.set_pipeline(mesh_pipeline);
                    pass.set_vertex_buffer(1, items.transforms.slice(..));
                    for (key, instances) in &items.groups {
                        if let Some(mesh) = self.volume_meshes.get(key) {
                            pass.set_vertex_buffer(0, mesh.vertices.slice(..));
                            pass.draw(0..mesh.count, instances.clone());
                        }
                    }
                }
            }

            // Кнопки пластини лежать на столі: знизу їх не видно, як і в Bambu Studio.
            if show_bed && !bed_last {
                self.plate_icons.draw(&mut pass);
                pass.set_bind_group(0, &self.bind_group, &[]);
            }

            if let Some(tp) = &self.toolpath {
                let (instances, travel) = self
                    .visible
                    .clone()
                    .unwrap_or((0..tp.total_instances, 0..tp.travel_count));
                let instances =
                    instances.start.min(tp.total_instances)..instances.end.min(tp.total_instances);
                if !instances.is_empty() {
                    pass.set_pipeline(&self.toolpath_pipeline);
                    for (bind, vertices) in self.bead_draws(tp, instances, camera, height, false) {
                        pass.set_bind_group(1, bind, &[]);
                        pass.draw(vertices, 0..1);
                    }
                }
                let travel = travel.start.min(tp.travel_count)..travel.end.min(tp.travel_count);
                if self.show_travel
                    && !travel.is_empty()
                    && let Some(buffer) = &tp.travel_buffer
                {
                    pass.set_pipeline(&self.line_pipeline);
                    pass.set_vertex_buffer(0, buffer.slice(..));
                    pass.draw(travel, 0..1);
                }
            }

            if let Some((buffer, count)) = &self.modifiers {
                pass.set_pipeline(&self.modifier_pipeline);
                pass.set_vertex_buffer(1, self.zero_offset.slice(..));
                pass.set_vertex_buffer(0, buffer.slice(..));
                pass.draw(0..*count, 0..1);
            }

            if let Some((buffer, count)) = &self.section_mesh {
                pass.set_pipeline(&self.section_pipeline);
                pass.set_vertex_buffer(0, buffer.slice(..));
                pass.draw(0..*count, 0..1);
            }

            for (buffer, count) in self.hover.iter().chain(&self.highlight) {
                pass.set_pipeline(&self.line_pipeline);
                pass.set_vertex_buffer(0, buffer.slice(..));
                pass.draw(0..*count, 0..1);
            }

            if bed_last {
                draw_bed(&mut pass);
            }

            if let Some((buffer, count)) = &self.surface_overlay {
                pass.set_pipeline(&self.surface_overlay_pipeline);
                pass.set_vertex_buffer(0, buffer.slice(..));
                pass.draw(0..*count, 0..1);
            }

            if let Some((buffer, count)) = &self.overlay {
                pass.set_pipeline(&self.overlay_pipeline);
                pass.set_vertex_buffer(0, buffer.slice(..));
                pass.draw(0..*count, 0..1);
            }
        }
        encoder.copy_texture_to_buffer(
            t.resolve.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &t.readback,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(t.padded_row),
                    rows_per_image: Some(height),
                },
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );
        let _s = crate::profile::Span::new("viewer::queue_submit");
        self.queue.submit([encoder.finish()]);
    }

    /// Кадр «Фото»: за потреби тіньові карти, потім фон, стіл чи підлога, валики, переміщення.
    fn encode_photo(
        &self,
        photo: &photo::Photo,
        encoder: &mut wgpu::CommandEncoder,
        t: &Targets,
        camera: &Camera,
        shadows: bool,
    ) {
        let visible = self.visible_instances();
        let (draws, shadow_draws, travel) = match &self.toolpath {
            Some(tp) if !visible.is_empty() => {
                let travel = self.visible.as_ref().map_or(0..tp.travel_count, |v| v.1.clone());
                let travel = travel.start.min(tp.travel_count)..travel.end.min(tp.travel_count);
                (
                    self.bead_draws(tp, visible.clone(), camera, t.height, false),
                    if shadows {
                        self.bead_draws(tp, visible, camera, t.height, true)
                    } else {
                        Vec::new()
                    },
                    tp.travel_buffer.as_ref().map(|b| (b, travel)),
                )
            }
            _ => (Vec::new(), Vec::new(), None),
        };
        // Порожня траєкторія теж очищає тіньові карти (інакше лишилась би стара тінь).
        if shadows {
            photo.encode_shadows(encoder, &shadow_draws);
        }
        let frame = photo::PhotoFrame {
            draws,
            travel,
            decals: &self.decals,
        };
        let resolve_view = t.resolve.create_view(&Default::default());
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("photo"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &t.msaa,
                depth_slice: None,
                resolve_target: Some(&resolve_view),
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                    store: wgpu::StoreOp::Discard,
                },
            })],
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: &t.depth,
                depth_ops: Some(wgpu::Operations {
                    load: wgpu::LoadOp::Clear(0.0),
                    store: wgpu::StoreOp::Discard,
                }),
                stencil_ops: None,
            }),
            ..Default::default()
        });
        photo.draw(&mut pass, &frame);
    }

    /// Що малювати для видимих інстансів `range`: спрощена копія, якщо навіть найближча
    /// точка траєкторії далі, ніж треба, щоб похибка LOD була менша за пів пікселя. Краї
    /// діапазону (повзунки шарів і рухів) завжди з повного буфера, тож межа точна.
    fn toolpath_draws(
        &self,
        tp: &crate::ToolpathGpuData,
        range: Range<u32>,
        camera: &Camera,
        height: u32,
    ) -> Vec<(bool, Range<u32>)> {
        let full = vec![(false, range.clone())];
        let (Some(lod), Some((lo, hi))) = (&tp.lod, tp.bounds) else {
            return full;
        };
        if self.lod_chunks.is_empty() || !self.toolpath_lod {
            return full;
        }
        let eye = camera.eye();
        let forward = (camera.target - eye).normalize_or_zero();
        let nearest = ((lo + hi) * 0.5 - eye).dot(forward)
            - ((hi - lo) * 0.5).dot(forward.abs());
        if camera.pixel_size(nearest, height) < 2.0 * lod.tolerance {
            return full;
        }
        lod.split(range).map_or(full, |[head, middle, tail]| {
            vec![(false, head), (true, middle), (false, tail)]
        })
    }

    /// Рендерить превʼю меша в PNG із прозорим тлом.
    pub fn render_mesh_thumbnail(&mut self, mesh: &Mesh, size: u32) -> Option<Vec<u8>> {
        self.render_mesh_view(mesh, size, None)
    }

    /// Мініатюра з кута над столом `pitch` (радіани); `None` — звичайний ізометричний ракурс.
    /// «Згори» для `top_N.png`/`pick_N.png` у `.gcode.3mf`.
    pub fn render_mesh_view(&mut self, mesh: &Mesh, size: u32, pitch: Option<f32>) -> Option<Vec<u8>> {
        let (lo, hi) = mesh.bounds()?;
        self.set_mesh(mesh);
        self.clear_toolpath();
        self.set_overlay(&[]);
        self.set_surface_overlay(&[]);
        self.set_highlight(None);
        self.set_hover(None);

        let mut camera = Camera::default();
        if let Some(pitch) = pitch {
            camera.pitch = pitch;
            camera.yaw = -90f32.to_radians();
        }
        camera.frame_tight(lo, hi);

        let mut pixels = self.render_target(
            size,
            size,
            &camera,
            wgpu::Color {
                r: 0.0,
                g: 0.0,
                b: 0.0,
                a: 0.0,
            },
            false,
        );
        for px in pixels.chunks_exact_mut(4) {
            px.swap(0, 2); // BGRA -> RGBA
        }
        let img = image::RgbaImage::from_raw(size, size, pixels)?;
        let mut png = Vec::new();
        let mut cursor = std::io::Cursor::new(&mut png);
        img.write_to(&mut cursor, image::ImageFormat::Png).ok()?;
        Some(png)
    }
}

fn read_pixels(t: &Targets) -> Result<Vec<u8>> {
    let data = t.readback.slice(..).get_mapped_range()?;
    let row = t.width as usize * 4;
    let mut pixels = Vec::with_capacity(row * t.height as usize);
    for chunk in data.chunks_exact(t.padded_row as usize) {
        pixels.extend_from_slice(&chunk[..row]);
    }
    Ok(pixels)
}

fn upload_uniform(device: &wgpu::Device, value: [u32; 4]) -> wgpu::Buffer {
    device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("toolpath_chunk"),
        contents: bytemuck::cast_slice(&value),
        usage: wgpu::BufferUsages::UNIFORM,
    })
}

fn upload<T: Pod>(device: &wgpu::Device, label: &str, vertices: &[T]) -> (wgpu::Buffer, u32) {
    let buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some(label),
        contents: bytemuck::cast_slice(vertices),
        usage: wgpu::BufferUsages::VERTEX,
    });
    (buffer, vertices.len() as u32)
}

fn create_targets(device: &wgpu::Device, width: u32, height: u32) -> Targets {
    let size = wgpu::Extent3d {
        width,
        height,
        depth_or_array_layers: 1,
    };
    let texture = |label, format, samples, usage| {
        device.create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size,
            mip_level_count: 1,
            sample_count: samples,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage,
            view_formats: &[],
        })
    };
    let attachment = wgpu::TextureUsages::RENDER_ATTACHMENT;
    let msaa = texture("msaa", COLOR_FORMAT, SAMPLES, attachment);
    let depth = texture("depth", DEPTH_FORMAT, SAMPLES, attachment);
    let resolve = texture(
        "resolve",
        COLOR_FORMAT,
        1,
        attachment | wgpu::TextureUsages::COPY_SRC,
    );

    let padded_row = (width * 4).next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("readback"),
        size: padded_row as u64 * height as u64,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });

    Targets {
        width,
        height,
        msaa: msaa.create_view(&Default::default()),
        depth: depth.create_view(&Default::default()),
        resolve,
        readback,
        padded_row,
    }
}

/// Напрямки світла Orca (задані в просторі камери) у світових координатах.
fn light_dirs(camera: &Camera) -> (glam::Vec3, glam::Vec3) {
    let forward = (camera.target - camera.eye()).normalize_or_zero();
    let mut right = forward.cross(glam::Vec3::Z);
    if right.length_squared() < 1e-8 {
        right = glam::Vec3::X;
    }
    let right = right.normalize();
    let up = right.cross(forward);
    let to_world = |v: glam::Vec3| (right * v.x + up * v.y - forward * v.z).normalize();
    (
        to_world(glam::Vec3::new(-0.457_495_7, 0.457_495_7, 0.762_492_9)),
        to_world(glam::Vec3::new(0.698_507_4, 0.139_701_5, 0.698_507_4)),
    )
}

fn plate_vertices(lo: [f32; 2], hi: [f32; 2], color: [f32; 4]) -> Vec<FlatVertex> {
    let [x, y] = lo;
    let [w, h] = hi;
    [[x, y], [w, y], [w, h], [x, y], [w, h], [x, h]]
        .map(|[x, y]| FlatVertex {
            pos: [x, y, -0.3],
            color,
        })
        .to_vec()
}

/// 10 mm grid and bounding rectangle of the selected printer.
fn grid_vertices(lo: [f32; 2], hi: [f32; 2], line_color: [f32; 4], edge: [f32; 4]) -> Vec<FlatVertex> {
    let mut out = Vec::new();
    let mut line = |a: [f32; 3], b: [f32; 3], color: [f32; 4]| {
        out.push(FlatVertex { pos: a, color });
        out.push(FlatVertex { pos: b, color });
    };
    for axis in 0..2 {
        let mut p = (lo[axis] / 10.0).ceil() * 10.0;
        while p <= hi[axis] {
            if axis == 0 {
                line([p, lo[1], 0.0], [p, hi[1], 0.0], line_color);
            } else {
                line([lo[0], p, 0.0], [hi[0], p, 0.0], line_color);
            }
            p += 10.0;
        }
    }
    let [x, y] = lo;
    let [w, h] = hi;
    line([x, y, 0.0], [w, y, 0.0], edge);
    line([w, y, 0.0], [w, h, 0.0], edge);
    line([w, h, 0.0], [x, h, 0.0], edge);
    line([x, h, 0.0], [x, y, 0.0], edge);
    line(
        [x, y, 0.05],
        [(x + 30.0).min(w), y, 0.05],
        [0.632, 0.168, 0.184, 1.0],
    );
    line(
        [x, y, 0.05],
        [x, (y + 30.0).min(h), 0.05],
        [0.356, 0.533, 0.220, 1.0],
    );
    out
}

/// 12 ребер прямокутної рамки як лінії.
fn box_lines(
    device: &wgpu::Device,
    label: &str,
    lo: glam::Vec3,
    hi: glam::Vec3,
    color: [f32; 4],
) -> (wgpu::Buffer, u32) {
    let corner = |i: u32| {
        [
            if i & 1 == 0 { lo.x } else { hi.x },
            if i & 2 == 0 { lo.y } else { hi.y },
            if i & 4 == 0 { lo.z } else { hi.z },
        ]
    };
    let mut lines = Vec::new();
    for a in 0..8u32 {
        for bit in [1, 2, 4] {
            if a & bit == 0 {
                lines.push(FlatVertex {
                    pos: corner(a),
                    color,
                });
                lines.push(FlatVertex {
                    pos: corner(a | bit),
                    color,
                });
            }
        }
    }
    upload(device, label, &lines)
}

#[cfg(test)]
mod async_tests {
    use super::*;

    #[test]
    fn scene_meshes_reuse_gpu_buffers_and_keep_independent_offsets() {
        let Some(mut renderer) = crate::test_renderer("scene_meshes_reuse_gpu_buffers") else {
            return;
        };
        let mesh = Mesh::calibration_cube();
        let offset = glam::Vec3::new(BED_SIZE * 1.2, 0., 0.);
        renderer.set_scene_beds(Some(vec![(1, glam::Vec3::ZERO), (2, offset)]), 2);
        renderer.set_scene_mesh(1, &mesh, glam::Vec3::ZERO);
        renderer.set_scene_mesh(2, &mesh, offset);
        renderer.set_scene_mesh(0, &mesh, -offset);
        let first = renderer.scene_meshes[&1].vertices.0.clone();
        let second = renderer.scene_meshes[&2].vertices.0.clone();
        renderer.set_scene_mesh(1, &mesh, offset);
        assert_eq!(renderer.scene_meshes[&1].vertices.0, first);
        assert_eq!(renderer.scene_meshes[&2].vertices.0, second);
        renderer.set_scene_beds(Some(vec![(1, glam::Vec3::ZERO), (2, offset)]), 1);
        assert_eq!(renderer.scene_meshes[&2].vertices.0, second);
        let mut changed = mesh.clone();
        changed.positions[0][0] += 1.;
        renderer.set_scene_mesh(1, &changed, glam::Vec3::ZERO);
        assert_ne!(renderer.scene_meshes[&1].vertices.0, first);
        assert_eq!(renderer.scene_meshes[&2].vertices.0, second);
        let mut camera = Camera::default();
        camera.frame(Some((-offset, offset + glam::Vec3::splat(BED_SIZE))));
        assert_eq!(renderer.render(128, 96, &camera).len(), 128 * 96 * 4);
        renderer.retain_scene_meshes(&[1, 2]);
        assert!(!renderer.scene_meshes.contains_key(&0));
        renderer.set_mesh(&mesh);
        assert!(renderer.scene_meshes.is_empty());
        assert!(renderer.scene_beds.is_none());
    }

    #[test]
    fn section_plane_reuses_buffer_and_clears_invalid_input() {
        let Some(mut renderer) = crate::test_renderer("section_plane_reuses_buffer") else {
            return;
        };
        let bounds = (glam::Vec3::ZERO, glam::Vec3::splat(20.0));
        let plane = [0.0, 0.0, 1.0, -10.0];
        renderer.set_section_plane(Some((plane, bounds)));
        let first = renderer.section_mesh.as_ref().unwrap().0.clone();
        renderer.set_section_plane(Some((plane, bounds)));
        assert_eq!(renderer.section_mesh.as_ref().unwrap().0, first);
        renderer.set_section_plane(Some(([1.0, 0.0, 0.0, -10.0], bounds)));
        let rotated = renderer.section_mesh.as_ref().unwrap().0.clone();
        assert_ne!(rotated, first);
        renderer.set_section_plane(Some((plane, (bounds.0, bounds.1 * 2.0))));
        assert_ne!(renderer.section_mesh.as_ref().unwrap().0, rotated);
        renderer.set_section_plane(Some(([0.0; 4], bounds)));
        assert!(renderer.section_mesh.is_none());
        assert!(renderer.section_plane.is_none());
        renderer.set_section_plane(Some((plane, bounds)));
        renderer.set_section_plane(None);
        assert!(renderer.section_mesh.is_none());
    }

    #[test]
    fn light_theme_changes_live_background_with_bed_hidden() {
        let Some(mut renderer) = crate::test_renderer("light_theme_changes_live_background") else {
            return;
        };
        renderer.set_mesh(&Mesh::calibration_cube());
        renderer.set_show_bed(false);
        let camera = Camera::default();
        let dark = renderer.render(32, 32, &camera);
        renderer.set_light_theme(true);
        let light = renderer.render(32, 32, &camera);
        // The corner is clear background; output pixels are BGRA.
        assert_ne!(&dark[..4], &light[..4]);
        assert!(light[0] > dark[0] && light[1] > dark[1] && light[2] > dark[2]);
        renderer.set_light_theme(true); // idempotent
        let still_light = renderer.render(32, 32, &camera);
        assert_eq!(&light[..4], &still_light[..4]);
        // Фон теми ОС (Omarchy) перекриває вбудований; None повертає його.
        // Tokyo Night #1a1b26 у лінійному просторі; вихід BGRA у sRGB.
        renderer.set_scene_background(Some([0.0103, 0.0103, 0.0194]));
        let themed = renderer.render(32, 32, &camera);
        assert!((themed[2] as i32 - 0x1a).abs() <= 2, "{:?}", &themed[..4]);
        assert!((themed[0] as i32 - 0x26).abs() <= 2, "{:?}", &themed[..4]);
        renderer.set_scene_background(None);
        assert_eq!(&renderer.render(32, 32, &camera)[..4], &light[..4]);
    }

    #[test]
    fn cancelled_mapping_reports_error_and_can_submit_again() {
        let Some(mut renderer) = crate::test_renderer("cancelled_mapping_reports_error") else {
            return;
        };
        let camera = Camera::default();
        assert!(renderer.submit_frame(13, 7, &camera).unwrap());
        // Force a real map cancellation, rather than injecting a callback result.
        renderer.pending.as_ref().unwrap().targets.readback.unmap();
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            match renderer.poll_frame() {
                Err(_) => break,
                Ok(None) => {}
                Ok(Some(_)) => panic!("cancelled mapping produced a frame"),
            }
            assert!(
                Instant::now() < deadline,
                "map cancellation was not reported"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(!renderer.has_pending_frame());
        assert!(renderer.submit_frame(17, 9, &camera).unwrap());
        loop {
            if let Some(frame) = renderer.poll_frame().unwrap() {
                assert_eq!((frame.width, frame.height), (17, 9));
                break;
            }
            assert!(Instant::now() < deadline, "recovery frame did not complete");
            std::thread::sleep(Duration::from_millis(1));
        }
    }
}

/// Порядок малювання повного буфера: шари вище ока — за зростанням, нижче — блоками
/// за спаданням; обидві половини йдуть від ближчого до ока шару до дальшого, тож рання
/// перевірка глибини відкидає закрите.
fn toolpath_order(
    layer_ranges: &[Range<u32>],
    layer_z: &[f32],
    range: Range<u32>,
    eye_z: f32,
) -> Vec<Range<u32>> {
    /// Інстансів на виклик малювання: дрібніше — більше викликів, крупніше — грубіший
    /// порядок усередині блоку.
    const BLOCK: u32 = 4096;
    let layer = layer_z.partition_point(|z| *z < eye_z);
    let split = layer_ranges
        .get(layer)
        .map_or(range.end, |r| r.start)
        .clamp(range.start, range.end);
    let mut out = Vec::with_capacity(((split - range.start) / BLOCK + 2) as usize);
    if split < range.end {
        out.push(split..range.end);
    }
    let mut end = split;
    while end > range.start {
        let start = end.saturating_sub(BLOCK).max(range.start);
        out.push(start..end);
        end = start;
    }
    out
}

#[cfg(test)]
mod toolpath_order_tests {
    use super::toolpath_order;

    #[test]
    fn layers_near_eye_first_and_every_instance_once() {
        // 3 шари по 5000 інстансів на Z 1, 2, 3.
        let ranges = [0..5000, 5000..10000, 10000..15000];
        let z = [1.0, 2.0, 3.0];
        // Око над моделлю: від верхнього шару вниз.
        let above = toolpath_order(&ranges, &z, 0..15000, 10.0);
        assert_eq!(above.first().unwrap().end, 15000);
        assert!(above.windows(2).all(|w| w[1].end == w[0].start));
        // Око під моделлю: порядок друку одним викликом.
        assert_eq!(toolpath_order(&ranges, &z, 0..15000, 0.0), vec![0..15000]);
        // Око між шарами: спершу шари вище, потім нижче — від найближчого.
        let mid = toolpath_order(&ranges, &z, 0..15000, 1.5);
        assert_eq!(mid[0], 5000..15000);
        assert_eq!(mid[1].end, 5000);
        // Повзунок шарів: лише видимий діапазон, кожен інстанс рівно раз.
        let part = toolpath_order(&ranges, &z, 2000..12000, 2.5);
        let mut covered: Vec<u32> = part.iter().flat_map(|r| r.clone()).collect();
        covered.sort_unstable();
        assert_eq!(covered, (2000..12000).collect::<Vec<_>>());
    }
}
