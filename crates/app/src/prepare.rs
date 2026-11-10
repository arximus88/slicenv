//! «Підготовка» в розкладці Zed: ліворуч док «Об’єкти», по центру спільна 3D-сцена пластин,
//! праворуч док «Друк» (принтер · філамент · процес).

use gpui::{AnyElement, Context, Window, div, prelude::*, px};
use gpui_component::resizable::{h_resizable, resizable_panel};
use settings::PresetType;

use crate::{
    theme::zed,
    ui::{self, IconName},
    workspace::Workspace,
};

pub fn render(
    ws: &mut Workspace,
    _window: &mut Window,
    cx: &mut Context<Workspace>,
) -> impl IntoElement {
    // Доки — з кешу кадру, центр зі сценою — окремий вид: орбіта й зум сповіщають лише
    // його, тож дерево об’єктів і редактор процесу не будуються заново (`panel_cache`).
    let left = ws.cached_panel("prepare-objects", crate::objects::panel, cx);
    let right = ws.cached_panel(SETTINGS_PANEL, settings_panel, cx);
    let center = ws.viewport_view("prepare-center", center, cx);
    docked("prepare-docks", ws, Some(left), center, Some(right), cx)
}

/// Ключ кешу панелі «Друк»: «Перегляд» вбудовує той самий вид, що й «Підготовка».
pub const SETTINGS_PANEL: &str = "prepare-settings";

/// Три колонки Zed зі змінною шириною; доки ховаються з рядка стану або Ctrl+B / Ctrl+Alt+B.
/// Правий док — панель «Друк» (однакова ширина на обох сторінках), лівий у Preview —
/// статистика нарізки: таблиця на кілька колонок, їй треба ширше.
pub fn docked(
    id: &'static str,
    ws: &Workspace,
    left: Option<AnyElement>,
    center: AnyElement,
    right: Option<AnyElement>,
    cx: &mut Context<Workspace>,
) -> impl IntoElement {
    // Панель режимів і дій проєкту — над центром, між доками.
    let center = ws.with_project_bar(center, cx);
    let mut group = h_resizable(id);
    if let Some(left) = left {
        let (size, range) = if id == "preview-docks" {
            (460., 380.0..640.)
        } else {
            (272., 200.0..480.)
        };
        group = group.child(
            resizable_panel()
                .size(px(size))
                .size_range(px(range.start)..px(range.end))
                .fixed()
                .visible(ws.left_dock)
                .child(left),
        );
    }
    group = group.child(resizable_panel().child(center));
    if let Some(right) = right {
        group = group.child(
            resizable_panel()
                .size(px(380.))
                .size_range(px(300.)..px(640.))
                .fixed()
                .visible(ws.right_dock)
                .child(right),
        );
    }
    group
}

/// Ракурси Bambu Studio: клавіша, іконка, назва, азимут і кут над столом.
pub const VIEWS: [(&str, IconName, &str, f32, f32); 7] = [
    ("0", IconName::Box, "Ізометрія", -100., 35.),
    ("1", IconName::Square, "Згори", -90., 90.),
    ("2", IconName::SquareDashed, "Знизу", -90., -90.),
    ("3", IconName::RectangleHorizontal, "Спереду", -90., 0.),
    ("4", IconName::RectangleHorizontal, "Ззаду", 90., 0.),
    ("5", IconName::PanelLeft, "Ліворуч", 180., 0.),
    ("6", IconName::PanelRight, "Праворуч", 0., 0.),
];

impl Workspace {
    /// Scene placement is separate from project-local editing coordinates.
    pub fn plate_offset(&self, number: u32) -> glam::Vec3 {
        if number == 0 {
            return glam::Vec3::ZERO;
        }
        let count = self.plates.iter().filter(|p| p.number > 0).count();
        let columns = (count as f64).sqrt().ceil().max(1.) as usize;
        let slot = self
            .plates
            .iter()
            .filter(|p| p.number > 0)
            .position(|p| p.number == number)
            .unwrap_or(0);
        let (lo, hi) = self.bed_bounds();
        glam::Vec3::new(
            (slot % columns) as f32 * (hi[0] - lo[0]) as f32 * 1.2,
            -((slot / columns) as f32) * (hi[1] - lo[1]) as f32 * 1.2,
            0.,
        )
    }

    /// Пластина, яку сцена показує одну: «Перегляд» малює лише активну, у початку координат —
    /// там само, де стіл і траєкторія після нарізки. `None` — усі пластини («Підготовка»).
    pub fn solo_plate(&self) -> Option<u32> {
        (self.page == crate::workspace::Page::Preview)
            .then(|| self.plates.get(self.active_plate).map_or(0, |p| p.number))
    }

    /// Зсув пластини на сцені: на «Перегляді» єдина пластина стоїть у початку координат.
    pub fn scene_offset(&self, number: u32) -> glam::Vec3 {
        if self.solo_plate().is_some() {
            glam::Vec3::ZERO
        } else {
            self.plate_offset(number)
        }
    }

    pub fn instance_offset(&self, key: project::InstanceRef) -> glam::Vec3 {
        self.project
            .as_ref()
            .and_then(|p| p.instance(key).ok())
            .map_or(glam::Vec3::ZERO, |i| {
                self.plate_offset(i.plate.unwrap_or(0))
            })
    }

    pub fn scene_selected_bounds(&self) -> Option<project::Bounds> {
        let mut b = self.selected_bounds()?;
        let offset = self.instance_offset(self.selected?).as_dvec3();
        b.min += offset;
        b.max += offset;
        Some(b)
    }

    pub fn invalidate_plate_bounds(&self) {
        self.plate_bounds.replace(None);
    }

    /// Рамка вмісту пластини (у її власних координатах). З проєктом — об’єднання рамок
    /// інстансів із кешу; без проєкту — сітка пластини (калібрувальний кубик, STL).
    pub(crate) fn plate_mesh_bounds(&self, index: usize) -> Option<(glam::Vec3, glam::Vec3)> {
        let mut cache = self.plate_bounds.borrow_mut();
        let fresh = cache.as_ref().is_some_and(|c| c.len() == self.plates.len());
        if !fresh {
            *cache = Some(
                self.plates
                    .iter()
                    .map(|plate| self.compute_plate_bounds(plate))
                    .collect(),
            );
        }
        cache.as_ref().and_then(|c| c.get(index).copied().flatten())
    }

    fn compute_plate_bounds(&self, plate: &viewer::Plate) -> Option<(glam::Vec3, glam::Vec3)> {
        let Some(project) = &self.project else {
            return plate.mesh.bounds();
        };
        let key = (plate.number > 0).then_some(plate.number);
        let mut out: Option<(glam::Vec3, glam::Vec3)> = None;
        for object in &project.objects {
            for (instance, placement) in object.instances.iter().enumerate() {
                if placement.plate != key {
                    continue;
                }
                let key = project::InstanceRef {
                    object: object.id,
                    instance,
                };
                // Під час живого повороту/масштабу точної рамки в кеші ще немає: рамка з кутів
                // стартової (як у підсвітки), інакше — точна, на вимогу (рідкісні виклики).
                let live = self
                    .live_drag
                    .as_ref()
                    .is_some_and(|d| d.key == key && d.volume.is_none());
                let Some(b) = self
                    .cached_instance_bounds(key)
                    .or_else(|| live.then(|| self.selected_bounds()).flatten())
                    .or_else(|| project.instance_bounds(key).ok().flatten())
                else {
                    continue;
                };
                let (lo, hi) = (b.min.as_vec3(), b.max.as_vec3());
                out = Some(out.map_or((lo, hi), |(a, c)| (a.min(lo), c.max(hi))));
            }
        }
        out
    }

    /// Кількість трикутників пластини для рядка стану, без обходу вершин.
    pub(crate) fn plate_triangle_count(&self, index: usize) -> usize {
        let Some(plate) = self.plates.get(index) else {
            return 0;
        };
        let Some(project) = &self.project else {
            return plate.mesh.triangle_count();
        };
        let key = (plate.number > 0).then_some(plate.number);
        project
            .objects
            .iter()
            .map(|o| {
                let per_instance: usize = o
                    .volumes
                    .iter()
                    .filter(|v| v.kind == project::VolumeKind::Part)
                    .map(|v| v.mesh.triangles.len())
                    .sum();
                per_instance * o.instances.iter().filter(|i| i.plate == key).count()
            })
            .sum()
    }

    /// Пластина, яку зараз показує булеве прев’ю (одна сітка замість частин).
    fn boolean_plate(&self) -> Option<u32> {
        let active = self.plates.get(self.active_plate)?.number;
        self.boolean_mesh
            .as_ref()
            .filter(|(revision, _)| self.boolean_preview && *revision == self.geometry_revision)
            .map(|_| active)
    }

    /// Елементи сцени «Підготовки»: кожна друкована частина кожної копії — своя GPU-сітка
    /// (спільна для копій) зі світовою матрицею. Вершини вантажаться лише для нових мешів.
    pub(crate) fn sync_scene_items(&mut self) {
        let Some(project) = &self.project else {
            self.renderer.set_scene_items(&[]);
            self.renderer.retain_volume_meshes(&Default::default());
            self.gpu_meshes.clear();
            return;
        };
        let skip = self.boolean_plate();
        let solo = self.solo_plate();
        let mut items = Vec::new();
        let mut used = std::collections::HashSet::new();
        let mut uploads = Vec::new();
        let mut uploads_origin_pending: Vec<usize> = Vec::new();
        let mut item_parts: Vec<(u64, project::DAffine3, glam::DMat4)> = Vec::new();
        for object in &project.objects {
            for placement in &object.instances {
                let number = placement.plate.unwrap_or(0);
                if skip == Some(number) || solo.is_some_and(|s| s != number) {
                    continue;
                }
                let offset = glam::DMat4::from_translation(self.scene_offset(number).as_dvec3());
                for volume in &object.volumes {
                    if volume.kind != project::VolumeKind::Part {
                        continue;
                    }
                    let key = std::sync::Arc::as_ptr(&volume.mesh) as usize as u64;
                    let origin = match self.gpu_meshes.get(&key) {
                        Some((_, origin)) => *origin,
                        None => {
                            if used.insert(key) {
                                uploads.push((key, volume.mesh.clone()));
                            }
                            // Центр знадобиться для матриці: рахуємо разом із завантаженням.
                            uploads_origin_pending.push(items.len());
                            glam::DVec3::ZERO
                        }
                    };
                    used.insert(key);
                    // Матриця в f64 разом із центром сітки, у f32 — лише результат.
                    let local = placement.transform
                        * volume.transform
                        * project::DAffine3::from_translation(origin);
                    items.push((key, (offset * glam::DMat4::from(local)).as_mat4()));
                    item_parts.push((key, placement.transform * volume.transform, offset));
                }
            }
        }
        for (key, mesh) in uploads {
            let (gpu, origin) = crate::objects::local_mesh(&mesh);
            self.renderer.set_volume_mesh(key, &gpu);
            self.gpu_meshes.insert(key, (mesh, origin));
        }
        // Елементи нових мешів отримують матрицю з уже відомим центром.
        for index in uploads_origin_pending {
            let (key, local, offset) = item_parts[index];
            let origin = self.gpu_meshes.get(&key).map_or(glam::DVec3::ZERO, |(_, o)| *o);
            let local = local * project::DAffine3::from_translation(origin);
            items[index] = (key, (offset * glam::DMat4::from(local)).as_mat4());
        }
        self.gpu_meshes.retain(|key, _| used.contains(key));
        self.renderer.retain_volume_meshes(&used);
        self.renderer.set_scene_items(&items);
    }

    pub fn sync_plate_scene(&mut self) {
        self.invalidate_plate_bounds();
        // «Перегляд» — лише активна пластина: сусідні не малюються ні столом, ні силуетом.
        let solo = self.solo_plate();
        let shown: Vec<u32> = self
            .plates
            .iter()
            .map(|p| p.number)
            .filter(|n| solo.is_none_or(|s| s == *n))
            .collect();
        let beds = shown
            .iter()
            .filter(|n| **n > 0)
            .map(|n| (*n, self.scene_offset(*n)))
            .collect();
        let active = self.plates.get(self.active_plate).map_or(1, |p| p.number);
        self.renderer.retain_scene_meshes(&shown);
        self.renderer.set_scene_beds(Some(beds), active);
        // З проєктом сітки пластин порожні (частини — `sync_scene_items`); булеве прев’ю
        // й показ без проєкту лишаються однією сіткою на пластину.
        let boolean = self.boolean_plate();
        for plate in self.plates.iter().filter(|p| shown.contains(&p.number)) {
            let offset = self.scene_offset(plate.number);
            let mesh = self
                .boolean_mesh
                .as_ref()
                .filter(|_| boolean == Some(plate.number))
                .map_or(&plate.mesh, |(_, mesh)| mesh);
            self.renderer.set_scene_mesh(plate.number, mesh, offset);
        }
        self.sync_scene_items();
        self.dirty = true;
    }

    pub fn fit_all_plates(&mut self) {
        if let Some((lo, hi)) = self.scene_bounds() {
            let aspect = self.viewport_bounds.map_or(1., |b| {
                f32::from(b.size.width) / f32::from(b.size.height).max(1.)
            });
            self.camera.frame_scene(lo, hi, aspect);
        }
        self.dirty = true;
    }

    pub fn plate_at(&self, position: gpui::Point<gpui::Pixels>) -> Option<usize> {
        let point = self.bed_point(position)?;
        let (lo, hi) = self.bed_bounds();
        self.plates.iter().enumerate().find_map(|(i, p)| {
            if p.number == 0 {
                return None;
            }
            let q = point - self.plate_offset(p.number);
            (q.x >= lo[0] as f32
                && q.x <= hi[0] as f32
                && q.y >= lo[1] as f32
                && q.y <= hi[1] as f32)
                .then_some(i)
        })
    }

    /// На сцені моделі пластин: «Підготовка» або «Перегляд», доки нічого не нарізано.
    pub fn scene_shows_models(&self) -> bool {
        self.page == crate::workspace::Page::Prepare
            || (self.page == crate::workspace::Page::Preview && self.preview.is_none())
    }

    /// Рамка того, що зараз на сцені: модель пластини чи траєкторія.
    pub fn scene_bounds(&self) -> Option<(glam::Vec3, glam::Vec3)> {
        match &self.preview {
            Some(preview) if self.page == crate::workspace::Page::Preview => {
                preview.built.gpu.bounds
            }
            _ => {
                let mut bounds: Option<(glam::Vec3, glam::Vec3)> = None;
                let solo = self.solo_plate();
                for (index, plate) in self.plates.iter().enumerate() {
                    if solo.is_some_and(|s| s != plate.number) {
                        continue;
                    }
                    let offset = self.scene_offset(plate.number);
                    let bed = self.bed_bounds();
                    let mesh = self.plate_mesh_bounds(index);
                    let range = if plate.number > 0 {
                        let lo = glam::Vec3::new(bed.0[0] as f32, bed.0[1] as f32, 0.);
                        let hi = glam::Vec3::new(bed.1[0] as f32, bed.1[1] as f32, 0.);
                        Some(mesh.map_or((lo, hi), |(a, b)| (lo.min(a), hi.max(b))))
                    } else {
                        mesh
                    };
                    if let Some((lo, hi)) = range {
                        let (lo, hi) = (lo + offset, hi + offset);
                        bounds = Some(bounds.map_or((lo, hi), |(a, b)| (a.min(lo), b.max(hi))));
                    }
                }
                bounds
            }
        }
    }

    /// Готовий ракурс; кадр підганяється під стіл і модель.
    pub fn set_view(&mut self, yaw_deg: f32, pitch_deg: f32) {
        self.cancel_navigation();
        self.camera.set_angles(yaw_deg, pitch_deg);
        if self.scene_shows_models() {
            self.fit_all_plates();
        } else {
            self.camera.frame(self.scene_bounds());
        }
        self.dirty = true;
    }

    /// «Вписати»: активна пластина; готові ракурси охоплюють усі пластини.
    pub fn zoom_to_fit(&mut self) {
        if self.scene_shows_models() {
            self.frame_active_plate();
        } else {
            self.camera.frame(self.scene_bounds());
        }
        self.dirty = true;
    }

    /// Кадр на стіл активної пластини разом з її моделями.
    pub fn frame_active_plate(&mut self) {
        if let Some(plate) = self.plates.get(self.active_plate) {
            let offset = self.scene_offset(plate.number);
            let (a, b) = self.bed_bounds();
            let lo = glam::Vec3::new(a[0] as f32, a[1] as f32, 0.);
            let hi = glam::Vec3::new(b[0] as f32, b[1] as f32, 0.);
            let (lo, hi) = self
                .plate_mesh_bounds(self.active_plate)
                .map_or((lo, hi), |(a, b)| (lo.min(a), hi.max(b)));
            self.camera.frame_tight(lo + offset, hi + offset);
        }
        self.dirty = true;
    }

    /// Клавіші камери на «Підготовці» й «Перегляді»: 0–6 — ракурси, Z — вписати.
    pub fn camera_key(&mut self, key: &str) -> bool {
        if let Some(&(_, _, _, yaw, pitch)) = VIEWS.iter().find(|v| v.0 == key) {
            self.set_view(yaw, pitch);
            return true;
        }
        if key == "z" {
            self.zoom_to_fit();
            return true;
        }
        false
    }
}

/// Спільна сцена всіх пластин із навігацією та інструментами.
fn center(ws: &mut Workspace, window: &mut Window, cx: &mut Context<Workspace>) -> AnyElement {
    let z = zed();
    div()
        .flex()
        .flex_col()
        .size_full()
        .min_w_0()
        .bg(z.editor)
        .child(
            ws.viewport(cx)
                .child(crate::navigation::cube(ws, cx))
                .child(crate::tools::toolbar(ws, cx))
                .children(crate::tool_panels::panel(ws, window, cx))
                .children(crate::tools::readout(ws))
                .children(crate::plate_controls::tooltip(ws))
                .children(crate::measure_tool::labels(ws))
                .children(crate::assembly::cut_card(ws, cx)),
        )
        .into_any_element()
}

/// Правий док «Підготовки» й «Перегляду»: профілі принтера, філаменту й процесу та
/// редактор процесу.
pub(crate) fn settings_panel(
    ws: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> AnyElement {
    let z = zed();
    let title = |ws: &Workspace, kind: PresetType, title: &'static str| {
        let dirty = ws.editor(kind).is_some_and(|e| e.is_dirty());
        ui::group_title(title).when(dirty, |d| {
            d.child(div().size(px(6.)).rounded_full().bg(z.warning))
                .text_color(z.text)
        })
    };
    let gear =
        |id: &'static str, kind: PresetType, tip: &'static str, cx: &mut Context<Workspace>| {
            ui::icon_button(id, IconName::SlidersHorizontal, tip).on_click(
                cx.listener(move |this, _, window, cx| this.open_editor_dialog(kind, window, cx)),
            )
        };
    let editor = ws.preset_editor_view(PresetType::Process, window, cx);
    let machine_title = title(ws, PresetType::Machine, "Принтер")
        .child(div().flex_1())
        .child(gear(
            "gear-machine",
            PresetType::Machine,
            "Налаштування принтера",
            cx,
        ));
    let filament_title = title(ws, PresetType::Filament, "Філамент")
        .child(div().flex_1())
        .child(gear(
            "gear-filament",
            PresetType::Filament,
            "Налаштування філаменту",
            cx,
        ));
    let process_title = title(ws, PresetType::Process, "Процес");
    let control = |el: AnyElement| div().px(px(10.)).pb(px(6.)).child(el);

    ui::dock()
        .child(ui::panel_header("Друк", Vec::new()))
        .child(engine_button(ws, cx))
        .child(
            div()
                .id("prepare-settings")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .pb(px(12.))
                .children(crate::engines::profile_status(ws, cx))
                .child(machine_title.pr(px(4.)))
                .child(control(ws.machine_select.clone().into_any_element()))
                .child(ui::prop_row("Пластина", ws.bed_select.clone()).pb(px(4.)))
                .child(ui::divider())
                .child(filament_title.pr(px(4.)))
                .child(control(ws.filament_select.clone().into_any_element()))
                .child(ui::divider())
                .child(process_title)
                .child(control(ws.process_select.clone().into_any_element()))
                .child(editor),
        )
        .into_any_element()
}

/// Кнопка «Рушій нарізки» вгорі панелі «Друк»: підпис, назва рушія й підказка, щоб
/// посилання «Рушій нарізки → …» з карток помилок вело саме сюди.
fn engine_button(ws: &Workspace, cx: &mut Context<Workspace>) -> impl IntoElement {
    let z = zed();
    let tip: gpui::SharedString = crate::tr_format!(
        "Рушій нарізки: {} — натисни, щоб змінити або перевірити сумісність проєкту",
        ws.engine_label
    )
    .into();
    div()
        .id("engine-manager")
        .flex()
        .items_center()
        .gap(px(6.))
        .mx(px(6.))
        .h(px(26.))
        .px(px(6.))
        .rounded(px(4.))
        .cursor_pointer()
        .text_sm()
        .hover(|s| s.bg(z.element_hover))
        .active(|s| s.bg(z.element_selected))
        .child(ui::icon(IconName::Settings).text_color(z.text_muted))
        .child(
            div()
                .flex_none()
                .text_color(z.text_muted)
                .child(crate::i18n::tr("Рушій нарізки")),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .overflow_hidden()
                .text_ellipsis()
                .whitespace_nowrap()
                .child(ws.engine_label.clone()),
        )
        .child(ui::icon(IconName::ChevronsUpDown).size(px(12.)).text_color(z.text_muted))
        .tooltip(move |window, cx| gpui_component::tooltip::Tooltip::new(tip.clone()).build(window, cx))
        .on_click(cx.listener(|this, _, window, cx| this.open_engines(window, cx)))
}
