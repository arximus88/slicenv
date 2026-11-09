//! Offscreen 3D-вʼюпорт: стіл A1 + модель, рендер у wgpu-текстуру й зчитування в BGRA.
//!
//! GPUI на Linux не дає вбудувати чужу wgpu-поверхню (`paint_surface` є лише на macOS),
//! тож кадр рендериться окремо й показується як зображення. На iGPU з UMA-памʼяттю
//! зчитування дешеве.
//!
//! Live viewports use [`Renderer::submit_frame`] and [`Renderer::poll_frame`]. Schedule
//! polling/repaint while [`Renderer::has_pending_frame`] is true, retaining the last
//! image between completions and retrying dirty state after a busy submission.
//! [`Renderer::render`] / [`Renderer::render_target`] block and are for snapshots.

mod camera;
pub mod gizmo;
pub mod measure;
mod mesh;
use paths::profile;
mod gcode_gpu;
pub use gcode_gpu::{BuiltToolpath, ToolpathGpuExt};
mod renderer;
mod threemf;
mod toolpath;

pub use camera::Camera;
pub use gizmo::{AXIS_COLORS, Gizmo, GizmoKind, Handle, Screen};
pub use mesh::{
    Mesh, Plate, PlateImages, SUPPORTED_EXTENSIONS, load_model, load_stl, load_stl_raw, local_mesh,
    plate_images, plates_of, thumbnail, thumbnail_cached,
};
pub use renderer::bed::{
    BedAssets, BedDecal, BedImage, LabelColors, LabelRects, PlateLabel, bambu_decal_rects,
    label_rects,
};
pub use renderer::plate_icons::{PlateButton, PlateIcon, PlateIconQuad, plate_button_rects};
pub use renderer::photo::{
    self, MAX_MATERIALS, MaterialTable, Pattern, PhotoMaterial, PhotoSettings, Preset, Stage,
    SupportView,
};
pub use renderer::{DRAG_MESH, FlatVertex, RenderedFrame, Renderer};
pub use toolpath::{LOD_TOLERANCE, ToolpathGpuData, ToolpathInstance, ToolpathLod};

/// Робоча зона Bambu Lab A1, мм.
pub const BED_SIZE: f32 = 256.0;

/// Рендерер для GPU-тестів і бенчмарків. Без wgpu-адаптера (CI-машина без GPU) —
/// `None` і рядок `SKIP` у stderr, щоб тест завершився успішно, а не панікував.
/// `SLICENV_REQUIRE_GPU=1` перетворює пропуск на провал: так GPU-раннер не
/// пропустить регресію мовчки.
#[doc(hidden)]
pub fn test_renderer(test: &str) -> Option<Renderer> {
    match Renderer::new() {
        Ok(renderer) => Some(renderer),
        Err(error) if std::env::var_os("SLICENV_REQUIRE_GPU").is_some_and(|v| v == "1") => {
            panic!("{test}: SLICENV_REQUIRE_GPU=1, але адаптера немає: {error:#}")
        }
        Err(error) => {
            eprintln!("SKIP {test}: немає wgpu-адаптера ({error:#})");
            None
        }
    }
}
