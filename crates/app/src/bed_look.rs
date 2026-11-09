//! Стіл, як у Bambu Studio: модель і написи беремо з профілів встановленого рушія під час
//! роботи (у репозиторії їх немає — ліцензія), назви й номери пластин — з проєкту.
use std::{cell::RefCell, path::Path};

use glam::Vec2;
use viewer::{BedAssets, BedDecal, BedImage, LabelColors, PlateLabel};

use crate::workspace::Workspace;

/// Префікс SVG-написів Bambu для типу пластини (`PartPlate::init_bed_type_info`).
fn bed_prefix(bed_type: &str) -> Option<&'static str> {
    let t = bed_type.to_ascii_lowercase();
    Some(if t.contains("supertack") {
        "st"
    } else if t.contains("textured pei") {
        "pte"
    } else if t.contains("high temp") || t.contains("smooth pei") || t == "pei plate" {
        "pei"
    } else if t.contains("engineering") {
        "ep"
    } else if t.contains("cool") {
        "pc"
    } else {
        return None;
    })
}

/// «Textured PEI Plate» → «Bambu Textured PEI Plate», як у написах Bambu.
fn bambu_label(bed_type: &str) -> String {
    if bed_type.to_ascii_lowercase().starts_with("bambu") {
        bed_type.to_owned()
    } else {
        format!("Bambu {bed_type}")
    }
}

fn image(path: &Path) -> Option<BedImage> {
    let bytes = std::fs::read(path).ok()?;
    let ext = path.extension()?.to_string_lossy().to_ascii_lowercase();
    match ext.as_str() {
        "svg" => Some(BedImage::Svg(bytes)),
        "png" => Some(BedImage::Png(bytes)),
        _ => None,
    }
}

/// Зібрати стіл принтера. Без рушія чи профілю — процедурний стіл з назвою типу пластини.
pub(crate) fn assets(
    resources: Option<&slicer::BedResources>,
    bed_type: &str,
    lo: Vec2,
    hi: Vec2,
) -> BedAssets {
    let mut out = BedAssets {
        type_label: (!bed_type.is_empty()).then(|| bed_type.to_owned()),
        ..Default::default()
    };
    let Some(res) = resources else {
        out.key = format!("procedural|{bed_type}|{lo}|{hi}");
        return out;
    };
    if let Some(path) = &res.model
        && let Ok(mut mesh) = viewer::load_stl_raw(path)
    {
        // Модель Bambu Studio (`bbl-3dp-*`) лежить у координатах столу, а та сама модель
        // в OrcaSlicer і моделі інших виробників — довкола нуля (`Bed3D::update_model_offset`).
        // Тож: накриває стіл — лишаємо, ні — центруємо на столі.
        if let Some((a, b)) = mesh.bounds() {
            let covers =
                a.x <= lo.x + 1. && a.y <= lo.y + 1. && b.x >= hi.x - 1. && b.y >= hi.y - 1.;
            if !covers {
                let shift = ((lo + hi) * 0.5 - (a.truncate() + b.truncate()) * 0.5).extend(0.);
                for p in &mut mesh.positions {
                    *p = (glam::Vec3::from(*p) + shift).to_array();
                }
            }
        }
        out.model = Some(mesh);
    }
    // Написи типу пластини — лише для принтерів Bambu, як у самому Bambu Studio.
    if res.vendor == "BBL"
        && let Some(images) = &res.images
        && let Some(prefix) = bed_prefix(bed_type)
    {
        let [left, bottom] = viewer::bambu_decal_rects(hi - lo);
        for (name, (min, max)) in [("left", left), ("bottom", bottom)] {
            if let Some(svg) = image(&images.join(format!("bbl_bed_{prefix}_{name}.svg"))) {
                out.decals.push(BedDecal {
                    min: lo + min,
                    max: lo + max,
                    image: svg,
                });
            }
        }
    }
    if out.decals.is_empty() {
        if res.vendor == "BBL" {
            // SVG-написів не знайшлось: замість логотипа з вертикальним «Bambu Lab» — один
            // рядок «Bambu <тип>» на місці лівого напису, як у Bambu Studio й OrcaSlicer.
            out.type_label = out.type_label.take().map(|t| bambu_label(&t));
        } else {
            // Логотип інших виробників (`bed_texture`) — це весь вигляд їхнього столу.
            out.logo = res.texture.as_deref().and_then(image);
        }
    }
    out.key = format!(
        "{:?}|{:?}|{}|{bed_type}|{lo}|{hi}",
        res.model,
        res.texture,
        out.decals.len()
    );
    out
}

thread_local! {
    /// Останній ключ (принтер, тип пластини, рушій): файли читаємо лише при зміні.
    static LAST: RefCell<String> = const { RefCell::new(String::new()) };
}

fn linear(c: gpui::Hsla) -> [f32; 4] {
    let c = c.to_rgb();
    let l = |v: f32| {
        if v <= 0.04045 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    };
    [l(c.r), l(c.g), l(c.b), c.a]
}

impl Workspace {
    /// Тип пластини активної пластини: власний з 3MF або з панелі «Друк».
    fn active_bed_type(&self) -> String {
        let number = self.plates.get(self.active_plate).map(|p| p.number);
        self.project
            .as_ref()
            .and_then(|p| p.plates.iter().find(|p| Some(p.index) == number))
            .and_then(|p| p.bed_type.clone())
            .unwrap_or_else(|| self.selection.bed_type.clone())
    }

    /// Викликається з кожного кадру: рендерер сам пропускає незмінене.
    pub(crate) fn sync_bed_look(&mut self) {
        let (lo, hi) = self.bed_bounds();
        let (lo, hi) = (
            Vec2::new(lo[0] as f32, lo[1] as f32),
            Vec2::new(hi[0] as f32, hi[1] as f32),
        );
        let bed_type = self.active_bed_type();
        let key = format!(
            "{}|{}|{bed_type}|{lo}|{hi}",
            self.selection.machine,
            self.presets
                .as_ref()
                .map_or_else(String::new, |p| p.root().display().to_string())
        );
        let changed = LAST.with(|last| {
            let mut last = last.borrow_mut();
            (*last != key).then(|| *last = key)
        });
        if changed.is_some() {
            let resources = self
                .presets
                .as_ref()
                .and_then(|store| store.bed_resources(&self.selection.machine));
            let assets = assets(resources.as_ref(), &bed_type, lo, hi);
            self.renderer.set_bed_assets(Some(assets));
            self.dirty = true;
        }
        let labels = self
            .project
            .as_ref()
            .map(|p| {
                p.plates
                    .iter()
                    .map(|plate| PlateLabel {
                        number: plate.index,
                        name: plate.name.clone(),
                    })
                    .collect()
            })
            .unwrap_or_default();
        let z = crate::theme::zed();
        self.renderer.set_plate_labels(
            labels,
            LabelColors {
                active: linear(z.accent),
                inactive: linear(z.text_muted),
            },
        );
    }

    /// Пластина, на чию назву чи олівець показує курсор.
    pub(crate) fn plate_label_at(&self, position: gpui::Point<gpui::Pixels>) -> Option<usize> {
        if self.page != crate::workspace::Page::Prepare || self.project.is_none() {
            return None;
        }
        let p = self.bed_point(position)?.truncate();
        self.plates.iter().position(|plate| {
            plate.number > 0
                && self
                    .renderer
                    .plate_label_rects(plate.number)
                    .is_some_and(|r| {
                        let (a, b) = r.rename_area();
                        p.cmpge(a).all() && p.cmple(b).all()
                    })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bed_types_map_to_bambu_svg_prefixes() {
        assert_eq!(bed_prefix("Textured PEI Plate"), Some("pte"));
        assert_eq!(bed_prefix("High Temp Plate"), Some("pei"));
        assert_eq!(bed_prefix("Cool Plate"), Some("pc"));
        assert_eq!(bed_prefix("Engineering Plate"), Some("ep"));
        assert_eq!(bed_prefix("Supertack Plate"), Some("st"));
        assert_eq!(bed_prefix("Printer Specific Plate"), None);
    }

    #[test]
    fn without_engine_bed_is_procedural_with_type_label() {
        let a = assets(None, "Textured PEI Plate", Vec2::ZERO, Vec2::splat(256.));
        assert!(a.model.is_none() && a.logo.is_none() && a.decals.is_empty());
        assert_eq!(a.type_label.as_deref(), Some("Textured PEI Plate"));
    }

    #[test]
    fn installed_bambu_engine_provides_model_logo_and_decals() {
        // Лише на машині з розпакованим рушієм; у CI тест нічого не перевіряє.
        let Some(root) = std::env::var_os("SLICENV_BED_RESOURCES") else {
            return;
        };
        let root = std::path::PathBuf::from(root);
        let res = slicer::BedResources {
            vendor: "BBL".into(),
            model: Some(root.join("profiles/BBL/bbl-3dp-X1.stl")),
            texture: Some(root.join("profiles/BBL/bbl-3dp-logo.svg")),
            images: Some(root.join("images")),
        };
        let a = assets(
            Some(&res),
            "Textured PEI Plate",
            Vec2::ZERO,
            Vec2::splat(256.),
        );
        assert!(a.model.is_some() && a.logo.is_none());
        assert_eq!(a.decals.len(), 2);
        // Невідомий тип: без логотипа з «Bambu Lab», один рядок «Bambu <тип>».
        let other = assets(Some(&res), "Unknown Plate", Vec2::ZERO, Vec2::splat(256.));
        assert!(other.logo.is_none() && other.decals.is_empty());
        assert_eq!(other.type_label.as_deref(), Some("Bambu Unknown Plate"));
    }

    #[test]
    fn bambu_bed_without_svg_gets_one_line_label_instead_of_logo() {
        let dir = tempfile::tempdir().unwrap();
        let logo = dir.path().join("bbl-3dp-logo.svg");
        std::fs::write(&logo, br#"<svg xmlns="http://www.w3.org/2000/svg" width="4" height="4"/>"#).unwrap();
        let res = |vendor: &str| slicer::BedResources {
            vendor: vendor.into(),
            model: None,
            texture: Some(logo.clone()),
            images: None,
        };
        let a = assets(Some(&res("BBL")), "Textured PEI Plate", Vec2::ZERO, Vec2::splat(256.));
        assert!(a.logo.is_none() && a.decals.is_empty());
        assert_eq!(a.type_label.as_deref(), Some("Bambu Textured PEI Plate"));
        // Інший виробник: його логотип лишається, підпис — без «Bambu».
        let v = assets(Some(&res("Voron")), "Textured PEI Plate", Vec2::ZERO, Vec2::splat(256.));
        assert!(v.logo.is_some());
        assert_eq!(v.type_label.as_deref(), Some("Textured PEI Plate"));
        assert_eq!(bambu_label("Bambu Cool Plate"), "Bambu Cool Plate");
    }
}
