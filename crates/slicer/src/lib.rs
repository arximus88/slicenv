//! Нарізка через консольний Bambu Studio з установленими Bambu Studio / OrcaSlicer профілями.

mod cli;
mod engine;
mod orca_compat;
mod presets;
mod safe_mode;

pub use cli::{
    Cancelled, SliceCancellation, cancel_active_slice, slice_with_engine,
    slice_with_engine_cancellable,
};
pub use cli::{FailureKind, SAFE_MODE_FAILED_HINT, SliceFailure, explain_failure};
pub use safe_mode::{
    COMPATIBLE_NOTE, EngineCompat, NO_PROJECT_SETTINGS_NOTE, PlateAssets, Stage, attach_plate_assets,
    engine_compat, engine_crashes_in_safe_mode,
    is_slicenv_package,
};
pub use cli::{Overrides, SliceOutput, SliceRequest, cache_dir, find_bambu_studio, slice};
pub use engine::{Engine, EngineInfo, EngineKind, version_number};
pub use orca_compat::{ORCA_LEGACY_FILAMENT_KEYS, normalize_orca_config};
pub use presets::{
    BED_TYPES, BedResources, Kind, PresetInfo, PresetStore, ProfileCatalogMissing, Selection,
    project_selection,
};

pub use paths as product;
