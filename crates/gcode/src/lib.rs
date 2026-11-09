//! Швидкий потоковий парсер G-code Bambu Studio / OrcaSlicer.

mod arc;
mod model;
mod parser;
mod report;
mod time;
mod view;

pub use arc::tessellate_arc;
pub use model::{FilamentUse, GcodeStats, Layer, Marker, Role, Segment, Toolpath};
pub use parser::{parse, parse_file};
pub use report::{EngineReport, LineRow, Report, Source, ToolRow};
pub use time::{MoveKind, TimeTotals};
pub use view::{
    ColorScheme, FilamentSlot, RANGE_COLORS, ViewOptions, filament_colors, filament_slots, gradient,
};
