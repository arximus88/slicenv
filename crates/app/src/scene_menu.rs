//! Контекстне меню 3D-сцени «Підготовки» (правий клік без перетягування), як у Bambu Studio /
//! OrcaSlicer: на порожньому місці — меню столу, на об’єкті — меню об’єкта. Тут же дії
//! мультивибору (вибрати все, видалити, розкласти) і додавання / заміна / перезавантаження моделей.

use std::path::PathBuf;

use gpui::{AnyElement, Context, PathPromptOptions, Pixels, Point, Window};
use project::{Handy, InstanceRef, Primitive, Project, VolumeKind};

use crate::{
    jobs::JobKind,
    menu::{Entry, Item},
    ui::IconName,
    widgets::ToastKind,
    workspace::{Page, Workspace},
};

/// Відстань між об’єктами при розкладанні, мм (як у кнопки «Розкласти»).
const SPACING: f64 = 6.0;

/// Що саме під курсором, коли відкрито меню.
#[derive(Clone, Debug)]
pub enum SceneMenuKind {
    /// Стіл: пластина (номер) і точка кліку в координатах пластини.
    Bed {
        plate: Option<u32>,
        at: Option<[f64; 2]>,
    },
    /// Об’єкт; дії діють на весь вибір, до якого він входить.
    Object(InstanceRef),
}

pub struct SceneMenu {
    pub position: Point<Pixels>,
    pub kind: SceneMenuKind,
    pub open_sub: Option<usize>,
}

/// Точка розширення меню об’єкта: кожна функція повертає свої пункти (порожній список — нічого).
/// Нова дія («Опорні ребра…» тощо) додає сюди свою функцію, не чіпаючи решту меню.
pub(crate) const OBJECT_MENU_EXTENSIONS: &[fn(&Workspace, InstanceRef) -> Vec<Entry>] =
    &[crate::support_fins::object_menu];

fn mm(value: f32) -> f64 {
    f64::from(value)
}

impl Workspace {
    // ---- відкриття й закриття ----

    pub fn open_scene_menu(
        &mut self,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.page != Page::Prepare
            || self.project.is_none()
            || self.modal.is_some()
            || self.jobs.is_running(JobKind::Open)
        {
            return;
        }
        self.file_menu = None;
        let kind = if let Some(key) = self.hit_test(position) {
            // Правий клік на об’єкті з поточного вибору діє на весь вибір, інакше вибирає його.
            if !self.selected_instances.contains(&key) || self.selected_volume.is_some() {
                self.select_instance(Some(key), window, cx);
            }
            SceneMenuKind::Object(key)
        } else {
            let index = self.plate_at(position);
            if let Some(index) = index.filter(|i| *i != self.active_plate) {
                self.show_plate(index);
            }
            let plate = self
                .plates
                .get(self.active_plate)
                .map(|p| p.number)
                .filter(|n| *n > 0);
            let at = index
                .and(plate)
                .zip(self.bed_point(position))
                .map(|(n, p)| {
                    let local = p - self.plate_offset(n);
                    [mm(local.x), mm(local.y)]
                });
            // Порожнє місце знімає вибір: «Розкласти» й «Автоповорот» діють на всю пластину.
            if !self.selected_instances.is_empty() {
                self.select_instance(None, window, cx);
            }
            SceneMenuKind::Bed { plate, at }
        };
        self.scene_menu = Some(SceneMenu {
            position,
            kind,
            open_sub: None,
        });
        cx.notify();
    }

    pub fn close_scene_menu(&mut self, cx: &mut Context<Self>) {
        if self.scene_menu.take().is_some() {
            cx.notify();
        }
    }

    fn set_scene_menu_sub(&mut self, index: Option<usize>, cx: &mut Context<Self>) {
        if let Some(menu) = &mut self.scene_menu
            && menu.open_sub != index
        {
            menu.open_sub = index;
            cx.notify();
        }
    }

    /// Шар меню для `Workspace::overlays`.
    pub fn scene_menu_overlay(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let Some(menu) = &self.scene_menu else {
            return Vec::new();
        };
        let (position, kind, open_sub) = (menu.position, menu.kind.clone(), menu.open_sub);
        let entries = match kind {
            SceneMenuKind::Bed { plate, at } => self.bed_entries(plate, at),
            SceneMenuKind::Object(key) => self.object_entries(key),
        };
        crate::menu::render(
            "scene-menu",
            position,
            &entries,
            open_sub,
            |ws, cx| ws.close_scene_menu(cx),
            |ws, index, cx| ws.set_scene_menu_sub(index, cx),
            window,
            cx,
        )
    }

    // ---- пункти ----

    fn active_plate_number(&self) -> Option<u32> {
        self.plates
            .get(self.active_plate)
            .map(|p| p.number)
            .filter(|n| *n > 0)
    }

    /// Стан меню столу / пластини для чистої побудови пунктів.
    fn bed_menu_state(&self, plate: Option<u32>, at: Option<[f64; 2]>) -> BedMenuState {
        let project = self.project.as_ref();
        // Додавати можна лише на справжню пластину.
        let target = plate.or_else(|| self.plates.iter().map(|p| p.number).find(|n| *n > 0));
        BedMenuState {
            plate,
            at,
            index: self.active_plate,
            locked: plate.is_some_and(|n| project.is_some_and(|p| p.plate_locked(n))),
            target,
            target_locked: target.is_some_and(|n| project.is_some_and(|p| p.plate_locked(n))),
            has_objects: project.is_some_and(|p| !p.objects.is_empty()),
            reloadable: self.has_reloadable(None),
        }
    }

    fn bed_entries(&self, plate: Option<u32>, at: Option<[f64; 2]>) -> Vec<Entry> {
        bed_menu(&self.bed_menu_state(plate, at))
    }

    fn object_menu_state(&self, key: InstanceRef) -> ObjectMenuState {
        let keys = self.selection_keys();
        let project = self.project.as_ref();
        let object = project.and_then(|p| p.object(key.object).ok());
        let current = project
            .and_then(|p| p.instance(key).ok())
            .and_then(|i| i.plate);
        let parts = object.map_or(0, |o| {
            o.volumes
                .iter()
                .filter(|v| v.kind == project::VolumeKind::Part)
                .count()
        });
        let all_parts = object.is_some_and(|o| {
            o.volumes
                .iter()
                .all(|v| v.kind == project::VolumeKind::Part)
        });
        ObjectMenuState {
            count: keys.len(),
            locked: self.selection_on_locked_plate(),
            on_plate: current.is_some(),
            plates: self
                .plates
                .iter()
                .filter(|p| p.number > 0 && Some(p.number) != current)
                .map(|p| {
                    (
                        p.number,
                        crate::i18n::plate_title(p),
                        project.is_some_and(|pr| pr.plate_locked(p.number)),
                    )
                })
                .collect(),
            printable: project.is_some_and(|p| {
                keys.iter()
                    .all(|k| p.instance(*k).is_ok_and(|i| i.printable))
            }),
            reloadable: self.has_reloadable(Some(&keys)),
            parts,
            all_parts,
            layer_range: project.is_some_and(|p| p.can_add_layer_range(key.object)),
        }
    }

    fn object_entries(&self, key: InstanceRef) -> Vec<Entry> {
        let mut entries = object_menu(&self.object_menu_state(key));
        let extras: Vec<Entry> = OBJECT_MENU_EXTENSIONS
            .iter()
            .flat_map(|extension| extension(self, key))
            .collect();
        if !extras.is_empty() {
            entries.push(Entry::Separator);
            entries.extend(extras);
        }
        entries
    }

    /// Правий клік на рядку об’єкта в дереві «Об’єкти»: те саме меню, що й у сцені.
    pub fn open_object_menu(
        &mut self,
        key: InstanceRef,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.project.is_none() || self.modal.is_some() || self.jobs.is_running(JobKind::Open) {
            return;
        }
        self.file_menu = None;
        if !self.selected_instances.contains(&key) || self.selected_volume.is_some() {
            self.select_instance(Some(key), window, cx);
        }
        self.scene_menu = Some(SceneMenu {
            position,
            kind: SceneMenuKind::Object(key),
            open_sub: None,
        });
        cx.notify();
    }

    /// Правий клік на рядку пластини: меню пластини (як на порожньому столі).
    pub fn open_plate_menu(
        &mut self,
        index: usize,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.project.is_none() || self.modal.is_some() || self.jobs.is_running(JobKind::Open) {
            return;
        }
        self.file_menu = None;
        if index != self.active_plate {
            self.show_plate(index);
        }
        if !self.selected_instances.is_empty() {
            self.select_instance(None, window, cx);
        }
        let plate = self.active_plate_number();
        self.scene_menu = Some(SceneMenu {
            position,
            kind: SceneMenuKind::Bed { plate, at: None },
            open_sub: None,
        });
        cx.notify();
    }

    /// Клавіші сцени без модифікаторів: «A» — розкласти.
    pub fn scene_action_key(
        &mut self,
        key: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        match key {
            "a" if self.project.is_some() => {
                self.arrange_selection_or_plate(window, cx);
                true
            }
            _ => false,
        }
    }

    // ---- вибір ----

    /// Вибрані інстанси (первинний — якщо список порожній).
    pub fn selection_keys(&self) -> Vec<InstanceRef> {
        if self.selected_instances.is_empty() {
            self.selected.into_iter().collect()
        } else {
            self.selected_instances.clone()
        }
    }

    fn selection_on_locked_plate(&self) -> bool {
        let Some(project) = &self.project else {
            return false;
        };
        self.selection_keys().iter().any(|k| {
            project
                .instance(*k)
                .ok()
                .and_then(|i| i.plate)
                .is_some_and(|n| project.plate_locked(n))
        })
    }

    fn set_selection_keys(&mut self, keys: Vec<InstanceRef>) {
        self.selected = keys.last().copied();
        self.selected_volume = None;
        self.selected_instances = keys;
    }

    /// Ctrl+A: усі інстанси показаної пластини; `all_plates` — усі пластини проєкту.
    pub fn select_all(&mut self, all_plates: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(project) = &self.project else { return };
        let number = self.plates.get(self.active_plate).map(|p| p.number);
        let keys: Vec<InstanceRef> = project
            .objects
            .iter()
            .flat_map(|o| {
                o.instances
                    .iter()
                    .enumerate()
                    .filter(move |(_, i)| all_plates || Some(i.plate.unwrap_or(0)) == number)
                    .map(move |(instance, _)| InstanceRef {
                        object: o.id,
                        instance,
                    })
            })
            .collect();
        self.set_selection_keys(keys);
        self.refresh_modifiers();
        self.sync_selection(window, cx);
        cx.notify();
    }

    // ---- спільна обгортка редагування ----

    /// Контрольна точка → зміна проєкту → перебудова змінених пластин. Помилка повертає
    /// проєкт у стан до зміни й показує сповіщення. `Some(keys)` — новий вибір.
    pub(crate) fn mutate(
        &mut self,
        label: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
        edit: impl FnOnce(&mut Project) -> anyhow::Result<Option<Vec<InstanceRef>>>,
    ) -> bool {
        if self.project.is_none() {
            return false;
        }
        self.checkpoint(label);
        let Some(project) = self.project.as_mut() else {
            return false;
        };
        let backup = project.clone();
        match edit(project) {
            Ok(selection) => {
                if let Some(keys) = selection {
                    self.set_selection_keys(keys);
                }
                self.project_changed(window, cx);
                true
            }
            Err(err) => {
                *project = backup;
                self.history_pending = None;
                self.toast(ToastKind::Error, format!("{err:#}"), cx);
                false
            }
        }
    }

    // ---- дії над вибором ----

    pub fn delete_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_volume.is_some() {
            return;
        }
        let keys = self.selection_keys();
        if keys.is_empty() {
            return;
        }
        self.mutate("Видалення", window, cx, move |p| {
            p.delete_instances(&keys)?;
            Ok(Some(Vec::new()))
        });
    }

    pub fn duplicate_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_volume.is_some() {
            return;
        }
        let keys = self.selection_keys();
        if keys.is_empty() {
            return;
        }
        self.mutate("Дублювання", window, cx, move |p| {
            let mut copies = Vec::new();
            for key in keys {
                let copy = p.duplicate_instance(key)?;
                p.translate(copy, project::DVec3::new(10.0, 10.0, 0.0))?;
                copies.push(copy);
            }
            Ok(Some(copies))
        });
    }

    /// Центр групи вибраних — у центр столу.
    pub fn center_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let keys = self.selection_keys();
        let (lo, hi) = self.bed_bounds();
        if keys.is_empty() {
            return;
        }
        self.mutate("Центрування", window, cx, move |p| {
            let (mut min, mut max) = (project::DVec3::MAX, project::DVec3::MIN);
            for key in &keys {
                anyhow::ensure!(p.instance(*key)?.plate.is_some(), "інстанс поза пластиною");
                if let Some(b) = p.instance_bounds(*key)? {
                    min = min.min(b.min);
                    max = max.max(b.max);
                }
            }
            anyhow::ensure!(min.is_finite(), "порожня сітка");
            let shift = project::DVec3::new(
                (lo[0] + hi[0] - min.x - max.x) / 2.0,
                (lo[1] + hi[1] - min.y - max.y) / 2.0,
                0.0,
            );
            for key in &keys {
                p.translate(*key, shift)?;
            }
            Ok(Some(keys))
        });
    }

    pub fn mirror_selected(&mut self, axis: usize, window: &mut Window, cx: &mut Context<Self>) {
        // Від більших індексів до менших: дзеркальний інстанс може піти в окремий об’єкт.
        let mut keys = self.selection_keys();
        keys.sort();
        keys.reverse();
        if keys.is_empty() {
            return;
        }
        self.mutate("Дзеркало", window, cx, move |p| {
            let mirrored = keys
                .into_iter()
                .map(|key| p.mirror_instance(key, axis))
                .collect::<anyhow::Result<Vec<_>>>()?;
            Ok(Some(mirrored))
        });
    }

    /// «A»: з вибором із двох і більше об’єктів — лише їх, інакше всю пластину.
    pub fn arrange_selection_or_plate(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let keys = self.selection_keys();
        if keys.len() < 2 {
            self.arrange_plate(window, cx);
            return;
        }
        let Some(number) = self.active_plate_number() else {
            return;
        };
        let (lo, hi) = self.bed_bounds();
        self.mutate("Розкладання", window, cx, move |p| {
            p.arrange_instances_on_bed(number, &keys, SPACING, lo, hi)?;
            Ok(Some(keys))
        });
    }

    pub fn confirm_delete_all(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.project.as_ref().is_none_or(|p| p.objects.is_empty()) {
            return;
        }
        self.confirm(
            "Видалити все",
            "Усі об’єкти проєкту буде видалено. Дію можна скасувати (Ctrl+Z).",
            "Видалити",
            |ws, window, cx| {
                ws.mutate("Видалення всього", window, cx, |p| {
                    p.delete_all_objects();
                    Ok(Some(Vec::new()))
                });
            },
            window,
            cx,
        );
    }

    // ---- додавання ----

    fn placement(&self, at: Option<[f64; 2]>) -> ([f64; 2], [f64; 2], [f64; 2]) {
        let (lo, hi) = self.bed_bounds();
        let center = at.unwrap_or([(lo[0] + hi[0]) / 2.0, (lo[1] + hi[1]) / 2.0]);
        (center, lo, hi)
    }

    fn show_plate_number(&mut self, number: u32) {
        if let Some(index) = self.plates.iter().position(|p| p.number == number) {
            self.show_plate(index);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn add_shape(
        &mut self,
        label: &'static str,
        name: &str,
        mesh: project::IndexedMesh,
        plate: u32,
        at: Option<[f64; 2]>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (center, lo, hi) = self.placement(at);
        let name = crate::i18n::tr(name);
        if self.mutate(label, window, cx, move |p| {
            Ok(Some(vec![
                p.add_mesh_object(&name, mesh, plate, center, lo, hi)?,
            ]))
        }) {
            self.show_plate_number(plate);
        }
    }

    pub fn add_primitive(
        &mut self,
        shape: Primitive,
        name: &'static str,
        plate: u32,
        at: Option<[f64; 2]>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.add_shape(
            "Додавання примітива",
            name,
            shape.mesh(),
            plate,
            at,
            window,
            cx,
        );
    }

    pub fn add_handy(
        &mut self,
        model: Handy,
        name: &'static str,
        plate: u32,
        at: Option<[f64; 2]>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.add_shape(
            "Додавання моделі",
            name,
            model.mesh(),
            plate,
            at,
            window,
            cx,
        );
    }

    /// «Додати моделі…»: вибір файлів STL / 3MF / OBJ, читання у фоні, одна зміна проєкту.
    pub fn prompt_add_models(
        &mut self,
        plate: u32,
        at: Option<[f64; 2]>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Знімок до діалогу: результат належить цьому проєкту, геометрії й пластині.
        let origin = self.job_target().geometry().plate_exists(plate);
        let paths = crate::file_menu::prompt_open(
            window,
            crate::file_menu::OpenKind::Models,
            "Додати моделі (STL, 3MF, OBJ)",
            true,
        );
        cx.spawn_in(window, async move |this, cx| {
            let paths = paths.await;
            if paths.is_empty() {
                return;
            }
            this.update(cx, |this, cx| {
                this.spawn_job(
                    JobKind::Import,
                    origin,
                    move || {
                        paths
                            .into_iter()
                            .map(|p| (p.clone(), project::read_model(&p)))
                            .collect::<Vec<_>>()
                    },
                    move |this, models, window, cx| {
                        this.add_read_models(models, plate, at, window, cx)
                    },
                    cx,
                );
            })
            .ok();
        })
        .detach();
    }

    pub(crate) fn add_read_models(
        &mut self,
        models: Vec<(PathBuf, anyhow::Result<project::ImportedModel>)>,
        plate: u32,
        at: Option<[f64; 2]>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mut ready = Vec::new();
        for (path, model) in models {
            match model {
                Ok(model) => ready.push(model),
                Err(err) => self.toast(
                    ToastKind::Error,
                    crate::tr_format!("{}: {err:#}", path.display()),
                    cx,
                ),
            }
        }
        if ready.is_empty() {
            return;
        }
        let (center, lo, hi) = self.placement(at);
        let many = ready.len() > 1;
        if self.mutate("Додавання моделей", window, cx, move |p| {
            let mut keys = Vec::new();
            for model in ready {
                keys.extend(p.add_model(model, plate, center, lo, hi)?);
            }
            if many {
                // Кілька файлів з одного місця не мають лягти один на одного.
                let _ = p.arrange_instances_on_bed(plate, &keys, SPACING, lo, hi);
            }
            Ok(Some(keys))
        }) {
            self.show_plate_number(plate);
        }
    }

    // ---- перезавантаження й заміна ----

    /// Чи є в `keys` (або в усьому проєкті) частини з відомим існуючим файлом-джерелом.
    fn has_reloadable(&self, keys: Option<&[InstanceRef]>) -> bool {
        let Some(project) = &self.project else {
            return false;
        };
        let ids = self.reload_targets(keys);
        ids.into_iter()
            .any(|id| !project.reloadable_sources(id).is_empty())
    }

    fn reload_targets(&self, keys: Option<&[InstanceRef]>) -> Vec<u32> {
        let mut ids: Vec<u32> = match (keys, &self.project) {
            (Some(keys), _) => keys.iter().map(|k| k.object).collect(),
            (None, Some(p)) => p.objects.iter().map(|o| o.id).collect(),
            (None, None) => Vec::new(),
        };
        ids.sort_unstable();
        ids.dedup();
        ids
    }

    /// Перечитує з диска файли-джерела вибраних об’єктів (`selection`) чи всіх.
    pub fn reload_from_disk(
        &mut self,
        selection: bool,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(project) = self.project.clone() else {
            return;
        };
        let keys = self.selection_keys();
        let ids = self.reload_targets(selection.then_some(keys.as_slice()));
        if !ids
            .iter()
            .any(|id| !project.reloadable_sources(*id).is_empty())
        {
            self.toast(
                ToastKind::Info,
                crate::i18n::tr("Немає відомих файлів для перезавантаження"),
                cx,
            );
            return;
        }
        self.spawn_job(
            JobKind::Replace,
            self.job_target().geometry(),
            move || project.read_reload_sources(&ids),
            |this, loaded, window, cx| {
                this.apply_replacements(
                    "Перезавантаження з диска",
                    window,
                    cx,
                    move |p| p.apply_reloaded(loaded),
                )
            },
            cx,
        );
    }

    /// «Замінити всі файлами 3D…»: тека, об’єкти зіставляються з файлами за назвою.
    pub fn prompt_replace_folder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(project) = self.project.clone() else {
            return;
        };
        // Знімок і ревізії — разом, до діалогу вибору теки (зіставлення йде за цим знімком).
        let target = self.job_target().geometry();
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some(crate::i18n::tr("Тека з файлами для заміни").into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = paths.await else {
                return;
            };
            let Some(dir) = paths.into_iter().next() else {
                return;
            };
            this.update(cx, |this, cx| {
                this.spawn_job(
                    JobKind::Replace,
                    target,
                    move || {
                        let files = Project::scan_replacement_folder(&dir)?;
                        let (matches, unmatched) = project.replacement_matches(&files);
                        let meshes = matches
                            .into_iter()
                            .map(|(object, volume, path)| {
                                let mesh = project::read_mesh_file(&path);
                                (object, volume, path, mesh)
                            })
                            .collect::<Vec<_>>();
                        anyhow::Ok((meshes, unmatched))
                    },
                    |this, prepared, window, cx| match prepared {
                        Ok((meshes, unmatched)) => this.apply_replacements(
                            "Заміна файлами",
                            window,
                            cx,
                            move |p| {
                                let mut report = project::ReplaceReport {
                                    replaced: 0,
                                    skipped: unmatched,
                                };
                                for (object, volume, path, mesh) in meshes {
                                    match mesh.and_then(|m| {
                                        p.replace_volume_mesh(object, volume, m, Some(&path))
                                    }) {
                                        Ok(()) => report.replaced += 1,
                                        Err(_) => report.skipped += 1,
                                    }
                                }
                                Ok(report)
                            },
                        ),
                        Err(err) => this.toast(ToastKind::Error, format!("{err:#}"), cx),
                    },
                    cx,
                );
            })
            .ok();
        })
        .detach();
    }

    /// Застосовує заміну сіток як одну зміну; якщо нічого не замінено — проєкт не чіпається.
    fn apply_replacements(
        &mut self,
        label: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
        apply: impl FnOnce(&mut Project) -> anyhow::Result<project::ReplaceReport>,
    ) {
        let Some(project) = self.project.as_mut() else {
            return;
        };
        let mut staged = project.clone();
        let report = match apply(&mut staged) {
            Ok(report) => report,
            Err(err) => {
                self.toast(ToastKind::Error, format!("{err:#}"), cx);
                return;
            }
        };
        if report.replaced == 0 {
            self.toast(
                ToastKind::Info,
                crate::tr_format!("Нічого не замінено (пропущено: {})", report.skipped),
                cx,
            );
            return;
        }
        self.checkpoint(label);
        if let Some(project) = self.project.as_mut() {
            *project = staged;
        }
        self.project_changed(window, cx);
        self.toast(
            ToastKind::Success,
            crate::tr_format!(
                "Замінено: {}, пропущено: {}",
                report.replaced,
                report.skipped
            ),
            cx,
        );
    }

    // ---- експорт ----

    pub fn prompt_export_stl(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let keys = self.selection_keys();
        let Some(project) = &self.project else { return };
        if keys.is_empty() {
            return;
        }
        let name = match keys.as_slice() {
            [key] => project
                .object(key.object)
                .map(|o| o.name.clone())
                .unwrap_or_default(),
            _ => self.model_name.trim_end_matches(".3mf").to_owned(),
        };
        let name = format!("{}.stl", if name.is_empty() { "model" } else { &name });
        let dir = self
            .path
            .as_deref()
            .and_then(|p| p.parent())
            .map_or_else(|| self.library_dir.clone(), PathBuf::from);
        let target = cx.prompt_for_new_path(&dir, Some(&name));
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(mut target))) = target.await else {
                return;
            };
            if target
                .extension()
                .is_none_or(|e| !e.eq_ignore_ascii_case("stl"))
            {
                target.set_extension("stl");
            }
            this.update_in(cx, |this, _, cx| {
                let Some(project) = &this.project else { return };
                match project.export_stl(&keys, &target) {
                    Ok(count) => this.toast(
                        ToastKind::Success,
                        crate::tr_format!(
                            "Експортовано {} трикутників: {}",
                            crate::group_digits(count),
                            crate::shorten_home(&target)
                        ),
                        cx,
                    ),
                    Err(err) => this.toast(
                        ToastKind::Error,
                        crate::tr_format!("Не експортовано: {err:#}"),
                        cx,
                    ),
                }
            })
            .ok();
        })
        .detach();
    }
}

// ---- побудова пунктів (чиста логіка, без `Workspace`) ----

/// Що потрібно знати меню столу / пластини.
#[derive(Clone, Debug, Default)]
pub(crate) struct BedMenuState {
    /// Пластина під курсором (`None` — «поза пластинами»).
    pub plate: Option<u32>,
    pub at: Option<[f64; 2]>,
    /// Індекс показаної пластини у `Workspace::plates`.
    pub index: usize,
    pub locked: bool,
    /// Куди додавати нові об’єкти.
    pub target: Option<u32>,
    pub target_locked: bool,
    pub has_objects: bool,
    pub reloadable: bool,
}

/// Що потрібно знати меню об’єкта.
#[derive(Clone, Debug, Default)]
pub(crate) struct ObjectMenuState {
    /// Скільки інстансів вибрано (дії діють на всі).
    pub count: usize,
    /// Хоч один вибраний — на заблокованій пластині.
    pub locked: bool,
    /// Об’єкт під курсором стоїть на пластині.
    pub on_plate: bool,
    /// Інші пластини: номер, назва, заблокована.
    pub plates: Vec<(u32, String, bool)>,
    /// Усі вибрані друкуються.
    pub printable: bool,
    pub reloadable: bool,
    /// Деталей (роль «Деталь») в об’єкті під курсором.
    pub parts: usize,
    /// Усі частини — деталі (без модифікаторів тощо).
    pub all_parts: bool,
    pub layer_range: bool,
}

fn primitive_label(shape: Primitive) -> (&'static str, IconName) {
    match shape {
        Primitive::Cube => ("Куб", IconName::Box),
        Primitive::Cylinder => ("Циліндр", IconName::Cylinder),
        Primitive::Sphere => ("Сфера", IconName::Circle),
        Primitive::Cone => ("Конус", IconName::Cone),
        Primitive::Disc => ("Диск", IconName::Disc),
        Primitive::Torus => ("Тор", IconName::Torus),
    }
}

/// Ролі частин, які можна додати з меню об’єкта.
const ADD_VOLUMES: [(VolumeKind, &str, IconName); 5] = [
    (VolumeKind::Part, "Додати елемент", IconName::Box),
    (
        VolumeKind::NegativeVolume,
        "Додати об’єм для віднімання",
        IconName::SquareDashed,
    ),
    (
        VolumeKind::Modifier,
        "Додати модифікатор",
        IconName::SlidersHorizontal,
    ),
    (
        VolumeKind::SupportBlocker,
        "Додати блокувальник підтримки",
        IconName::EyeOff,
    ),
    (
        VolumeKind::SupportEnforcer,
        "Додати примусові підтримки",
        IconName::Layers2,
    ),
];

pub(crate) fn bed_menu(s: &BedMenuState) -> Vec<Entry> {
    let (plate, at, index, locked, target) = (s.plate, s.at, s.index, s.locked, s.target);
    let target_locked = s.target_locked;
    let add = move |item: Item| -> Entry {
        match target {
            None => item.disabled("Немає пластини").entry(),
            Some(_) if target_locked => item.disabled("Пластина заблокована").entry(),
            Some(_) => item.entry(),
        }
    };
    let on_plate = |item: Item| -> Entry {
        match plate {
            None => item.disabled("Немає пластини").entry(),
            Some(_) if locked => item.disabled("Пластина заблокована").entry(),
            Some(_) => item.entry(),
        }
    };

    let primitives: Vec<Entry> = Primitive::ALL
        .into_iter()
        .map(|shape| {
            let (label, icon) = primitive_label(shape);
            add(Item::new(label).icon(icon).on(move |ws, window, cx| {
                if let Some(n) = target {
                    ws.add_primitive(shape, label, n, at, window, cx);
                }
            }))
        })
        .collect();
    let handy: Vec<Entry> = Handy::ALL
        .into_iter()
        .map(|model| {
            let (label, icon) = match model {
                Handy::CalibrationCube => ("Калібрувальний куб 20 мм", IconName::Box),
                Handy::CalibrationCylinder => ("Калібрувальний циліндр Ø20 мм", IconName::Cylinder),
                Handy::StepTower => ("Ступінчаста вежа", IconName::Layers),
            };
            add(Item::new(label).icon(icon).on(move |ws, window, cx| {
                if let Some(n) = target {
                    ws.add_handy(model, label, n, at, window, cx);
                }
            }))
        })
        .collect();

    vec![
        Item::new("Вибрати все")
            .icon(IconName::ListChecks)
            .shortcut("Ctrl+A")
            .enabled(s.has_objects)
            .on(|ws, window, cx| ws.select_all(false, window, cx))
            .entry(),
        Item::new("Вибрати всі пластини")
            .icon(IconName::ListChecks)
            .enabled(s.has_objects)
            .on(|ws, window, cx| ws.select_all(true, window, cx))
            .entry(),
        Item::new("Видалити все")
            .icon(IconName::Trash)
            .shortcut("Ctrl+Shift+D")
            .enabled(s.has_objects)
            .on(|ws, window, cx| ws.confirm_delete_all(window, cx))
            .entry(),
        on_plate(
            Item::new("Розкласти")
                .icon(IconName::LayoutGrid)
                .shortcut("A")
                .on(|ws, window, cx| ws.arrange_plate(window, cx)),
        ),
        {
            let item = Item::new("Перезавантажити все")
                .icon(IconName::RefreshCw)
                .on(|ws, window, cx| ws.reload_from_disk(false, window, cx));
            if s.reloadable {
                item.entry()
            } else {
                item.disabled("Файл-джерело невідомий або зник").entry()
            }
        },
        on_plate(
            Item::new("Автоповорот")
                .icon(IconName::RotateCw)
                .on(move |ws, _, cx| ws.orient_plate(index, cx)),
        ),
        Item::new("Видалити пластину")
            .icon(IconName::Minus)
            .enabled(plate.is_some())
            .on(|ws, window, cx| ws.delete_plate(window, cx))
            .entry(),
        Entry::Separator,
        add(Item::new("Додати примітив")
            .icon(IconName::Shapes)
            .submenu(primitives)),
        add(Item::new("Додати готову модель")
            .icon(IconName::Package)
            .submenu(handy)),
        add(Item::new("Додати моделі…")
            .icon(IconName::FilePlus)
            .on(move |ws, window, cx| {
                if let Some(n) = target {
                    ws.prompt_add_models(n, at, window, cx);
                }
            })),
        Item::new("Замінити всі файлами 3D…")
            .icon(IconName::ReplaceAll)
            .enabled(s.has_objects)
            .on(|ws, window, cx| ws.prompt_replace_folder(window, cx))
            .entry(),
        Entry::Separator,
        Item::new("Заблокувати")
            .checked(locked)
            .enabled(plate.is_some())
            .on(move |ws, window, cx| ws.toggle_plate_lock(index, window, cx))
            .entry(),
        Item::new("Змінити назву пластини")
            .icon(IconName::Pencil)
            .enabled(plate.is_some())
            .on(|ws, window, cx| ws.prompt_rename_plate(window, cx))
            .entry(),
        Item::new("Налаштування пластини…")
            .icon(IconName::SlidersHorizontal)
            .enabled(plate.is_some())
            .on(move |ws, window, cx| ws.plate_settings(index, window, cx))
            .entry(),
    ]
}

pub(crate) fn object_menu(s: &ObjectMenuState) -> Vec<Entry> {
    let locked = s.locked;
    let single = s.count == 1;
    // Редагування: недоступне на заблокованій пластині.
    let guard = move |item: Item| -> Entry {
        if locked {
            item.disabled("Пластина заблокована").entry()
        } else {
            item.entry()
        }
    };
    // Редагування одного об’єкта.
    let one = move |item: Item| -> Entry {
        if !single {
            item.disabled("Лише для одного об’єкта").entry()
        } else if locked {
            item.disabled("Пластина заблокована").entry()
        } else {
            item.entry()
        }
    };
    let mirror = |axis: usize, label: &'static str| -> Entry {
        Item::new(label)
            .icon(if axis == 2 {
                IconName::FlipVertical2
            } else {
                IconName::FlipHorizontal2
            })
            .on(move |ws, window, cx| ws.mirror_selected(axis, window, cx))
            .entry()
    };

    let mut move_to: Vec<Entry> = s
        .plates
        .iter()
        .map(|(n, title, plate_locked)| {
            let n = *n;
            let item = Item::raw(title.clone())
                .icon(IconName::Layers2)
                .on(move |ws, window, cx| ws.move_selection_to_plate(Some(n), window, cx));
            if *plate_locked {
                item.disabled("Пластина заблокована").entry()
            } else {
                item.entry()
            }
        })
        .collect();
    if !move_to.is_empty() {
        move_to.push(Entry::Separator);
    }
    move_to.push(
        Item::new("Нова пластина")
            .icon(IconName::Plus)
            .on(|ws, window, cx| ws.move_selection_to_plate(None, window, cx))
            .entry(),
    );

    let split = {
        let to_objects = Item::new("На об’єкти")
            .icon(IconName::Boxes)
            .on(|ws, window, cx| ws.split_to_objects(window, cx));
        let to_objects = if s.all_parts {
            to_objects.entry()
        } else {
            to_objects
                .disabled("Спершу приберіть модифікатори й інші не-деталі")
                .entry()
        };
        let to_parts = Item::new("На частини")
            .icon(IconName::Box)
            .on(|ws, window, cx| ws.split_to_parts(window, cx));
        let to_parts = if s.parts > 0 {
            to_parts.entry()
        } else {
            to_parts.disabled("Немає деталей").entry()
        };
        vec![to_objects, to_parts]
    };

    let add_volume = ADD_VOLUMES.into_iter().map(|(kind, label, icon)| {
        let shapes = Primitive::ALL
            .into_iter()
            .map(|shape| {
                let (name, icon) = primitive_label(shape);
                Item::new(name)
                    .icon(icon)
                    .on(move |ws, window, cx| ws.add_volume_primitive(kind, shape, name, window, cx))
                    .entry()
            })
            .collect();
        one(Item::new(label).icon(icon).submenu(shapes))
    });

    let mut entries = vec![
        {
            let item = Item::new("Заповнити пластину копіями")
                .icon(IconName::LayoutGrid)
                .on(|ws, window, cx| ws.fill_plate_with_copies(window, cx));
            if s.on_plate {
                one(item)
            } else {
                item.disabled("Об’єкт поза пластиною").entry()
            }
        },
        guard(
            Item::new("Зробити копію")
                .icon(IconName::Copy)
                .shortcut("Ctrl+D")
                .on(|ws, window, cx| ws.duplicate_selected(window, cx)),
        ),
    ];
    if s.count >= 2 {
        entries.push(
            Item::new("Об’єднати в збірку")
                .icon(IconName::Combine)
                .shortcut("Ctrl+G")
                .on(|ws, window, cx| ws.merge_selection(window, cx))
                .entry(),
        );
    }
    entries.extend([
        Entry::Separator,
        guard(
            Item::new("По центру пластини")
                .icon(IconName::Crosshair)
                .on(|ws, window, cx| ws.center_selected(window, cx)),
        ),
        guard(
            Item::new("Покласти на стіл")
                .icon(IconName::ArrowDownToLine)
                .on(|ws, window, cx| ws.drop_selected(window, cx)),
        ),
        guard(
            Item::new("Автоорієнтація")
                .icon(IconName::RotateCw)
                .shortcut("Q")
                .on(|ws, _, cx| ws.orient_selection(cx)),
        ),
        one(Item::new("Розділити").icon(IconName::Split).submenu(split)),
        guard(
            Item::new("Дзеркально")
                .icon(IconName::FlipHorizontal2)
                .submenu(vec![
                    mirror(0, "Вздовж X"),
                    mirror(1, "Вздовж Y"),
                    mirror(2, "Вздовж Z"),
                ]),
        ),
        guard(
            Item::new("Перенести на пластину")
                .icon(IconName::Layers2)
                .submenu(move_to),
        ),
        Item::new("Видалити")
            .icon(IconName::Trash)
            .shortcut("Delete")
            .on(|ws, window, cx| ws.delete_selected(window, cx))
            .entry(),
        Entry::Separator,
    ]);
    entries.extend(add_volume);
    entries.push({
        let item = Item::new("Модифікатор діапазону висоти шарів")
            .icon(IconName::Layers)
            .on(|ws, window, cx| ws.add_layer_range_selected(window, cx));
        if single && !s.layer_range {
            item.disabled("Недоступно для цього об’єкта").entry()
        } else {
            one(item)
        }
    });
    entries.extend([
        Entry::Separator,
        Item::new("Доступно для друку")
            .checked(s.printable)
            .on({
                let printable = s.printable;
                move |ws, window, cx| ws.set_selection_printable(!printable, window, cx)
            })
            .entry(),
        {
            let item = Item::new("Редагувати налаштування процесу друку")
                .icon(IconName::SlidersHorizontal)
                .on(|ws, window, cx| ws.open_scoped_settings(None, window, cx));
            if single {
                item.entry()
            } else {
                item.disabled("Лише для одного об’єкта").entry()
            }
        },
        Entry::Separator,
        {
            let item = Item::new("Перезавантажити з диска")
                .icon(IconName::RefreshCw)
                .on(|ws, window, cx| ws.reload_from_disk(true, window, cx));
            if s.reloadable {
                item.entry()
            } else {
                item.disabled("Файл-джерело невідомий або зник").entry()
            }
        },
        {
            let item = Item::new("Замінити на STL…")
                .icon(IconName::Replace)
                .on(|ws, window, cx| ws.prompt_replace_stl(window, cx));
            if single && s.parts != 1 {
                item.disabled("Лише для об’єкта з однією деталлю").entry()
            } else {
                one(item)
            }
        },
        Item::new("Експортувати як один STL…")
            .icon(IconName::Download)
            .on(|ws, window, cx| ws.prompt_export_stl(window, cx))
            .entry(),
        guard(
            Item::new("Перетворити з дюймів")
                .icon(IconName::Ruler)
                .shortcut("×25.4")
                .on(|ws, window, cx| {
                    ws.convert_selection_units(project::INCH, "Перетворення з дюймів", window, cx)
                }),
        ),
        guard(
            Item::new("Перетворити з метричної")
                .icon(IconName::Ruler)
                .shortcut("÷25.4")
                .on(|ws, window, cx| {
                    ws.convert_selection_units(
                        1.0 / project::INCH,
                        "Перетворення з метричної",
                        window,
                        cx,
                    )
                }),
        ),
    ]);
    entries
}

// ---- дії меню об’єкта ----

/// Найбільше копій за одне «Заповнити пластину».
const FILL_LIMIT: usize = 200;

impl Workspace {
    /// Один вибраний інстанс (без вибраної частини).
    fn single_selection(&self) -> Option<InstanceRef> {
        match self.selection_keys().as_slice() {
            [key] if self.selected_volume.is_none() => Some(*key),
            _ => None,
        }
    }

    pub fn fill_plate_with_copies(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(key) = self.single_selection() else {
            return;
        };
        let (lo, hi) = self.bed_bounds();
        let mut added = 0;
        if self.mutate("Заповнення пластини", window, cx, |p| {
            let keys = p.fill_plate_with_copies(key, SPACING, lo, hi, FILL_LIMIT)?;
            added = keys.len() - 1;
            Ok(Some(keys))
        }) {
            self.toast(
                ToastKind::Success,
                crate::tr_format!("Додано копій: {added}"),
                cx,
            );
        }
    }

    pub fn drop_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let keys = self.selection_keys();
        if keys.is_empty() {
            return;
        }
        self.mutate("Покласти на стіл", window, cx, move |p| {
            for key in &keys {
                p.drop_to_bed(*key)?;
            }
            Ok(Some(keys))
        });
    }

    pub fn set_selection_printable(
        &mut self,
        printable: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let keys = self.selection_keys();
        if keys.is_empty() {
            return;
        }
        self.mutate("Доступно для друку", window, cx, move |p| {
            for key in &keys {
                p.set_printable(*key, printable)?;
            }
            Ok(Some(keys))
        });
    }

    pub fn convert_selection_units(
        &mut self,
        factor: f64,
        label: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let keys = self.selection_keys();
        if keys.is_empty() {
            return;
        }
        self.mutate(label, window, cx, move |p| {
            for key in &keys {
                p.convert_units(*key, factor)?;
            }
            Ok(Some(keys))
        });
    }

    /// Переносить вибране на пластину `plate`; `None` — на нову пластину.
    pub fn move_selection_to_plate(
        &mut self,
        plate: Option<u32>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let keys = self.selection_keys();
        if keys.is_empty() {
            return;
        }
        let mut target = plate;
        if self.mutate("Перенесення на пластину", window, cx, |p| {
            let n = match plate {
                Some(n) => n,
                None => p.add_plate("")?,
            };
            anyhow::ensure!(!p.plate_locked(n), "пластина заблокована");
            for key in &keys {
                p.move_to_plate(*key, Some(n))?;
            }
            target = Some(n);
            Ok(Some(keys))
        }) && let Some(n) = target
        {
            self.show_plate_number(n);
        }
    }

    pub fn add_volume_primitive(
        &mut self,
        kind: VolumeKind,
        shape: Primitive,
        name: &'static str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(key) = self.single_selection() else {
            return;
        };
        let name = crate::i18n::tr(name);
        self.mutate("Додавання частини", window, cx, move |p| {
            p.add_primitive_volume(key, kind, &name, shape.mesh())?;
            Ok(Some(vec![key]))
        });
        self.collapsed_objects.remove(&key);
    }

    pub fn split_to_parts(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(key) = self.single_selection() else {
            return;
        };
        self.mutate("Розділення на частини", window, cx, move |p| {
            p.split_into_parts(key.object)?;
            Ok(Some(vec![key]))
        });
        self.collapsed_objects.remove(&key);
    }

    /// «Розділити › На об’єкти»: збірку — по частинах, суцільну сітку — по шматках.
    pub fn split_to_objects(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(key) = self.single_selection() else {
            return;
        };
        self.mutate("Розділення", window, cx, move |p| {
            if p.object(key.object)?.volumes.len() < 2 {
                p.split_into_parts(key.object)?;
            }
            Ok(Some(p.split_instance(key)?))
        });
    }

    pub fn add_layer_range_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(key) = self.single_selection() else {
            return;
        };
        let mut index = None;
        if self.mutate("Діапазон висот шарів", window, cx, |p| {
            index = Some(p.add_layer_range(key.object)?);
            Ok(Some(vec![key]))
        }) {
            self.collapsed_objects.remove(&key);
            self.open_scoped_settings(index, window, cx);
        }
    }

    /// «Замінити на STL…»: нова сітка єдиної деталі об’єкта, трансформації лишаються.
    pub fn prompt_replace_stl(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(key) = self.single_selection() else {
            return;
        };
        let Some(volume) = self.project.as_ref().and_then(|p| {
            let object = p.object(key.object).ok()?;
            let mut parts = object
                .volumes
                .iter()
                .enumerate()
                .filter(|(_, v)| v.kind == VolumeKind::Part);
            match (parts.next(), parts.next()) {
                (Some((i, _)), None) => Some(i),
                _ => None,
            }
        }) else {
            return;
        };
        let target = self.job_target().geometry();
        let paths = crate::file_menu::prompt_open(
            window,
            crate::file_menu::OpenKind::Stl,
            "Замінити на STL",
            false,
        );
        cx.spawn_in(window, async move |this, cx| {
            let Some(path) = paths.await.into_iter().next() else {
                return;
            };
            this.update(cx, |this, cx| {
                this.spawn_job(
                    JobKind::Replace,
                    target,
                    move || {
                        let mesh = project::read_mesh_file(&path);
                        (path, mesh)
                    },
                    move |this, (path, mesh), window, cx| match mesh {
                        Ok(mesh) => this.apply_replacements("Заміна на STL", window, cx, move |p| {
                            p.replace_volume_mesh(key.object, volume, mesh, Some(&path))?;
                            Ok(project::ReplaceReport {
                                replaced: 1,
                                skipped: 0,
                            })
                        }),
                        Err(err) => this.toast(
                            ToastKind::Error,
                            crate::tr_format!("{}: {err:#}", path.display()),
                            cx,
                        ),
                    },
                    cx,
                );
            })
            .ok();
        })
        .detach();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn find<'a>(entries: &'a [Entry], label: &str) -> Option<&'a Item> {
        let label = crate::i18n::tr(label);
        entries.iter().find_map(|e| match e {
            Entry::Item(item) if item.label.as_ref() == label => Some(item),
            Entry::Item(item) => find(&item.submenu, &label),
            Entry::Separator => None,
        })
    }

    fn item<'a>(entries: &'a [Entry], label: &str) -> &'a Item {
        find(entries, label).unwrap_or_else(|| panic!("немає пункту {label}"))
    }

    fn single() -> ObjectMenuState {
        ObjectMenuState {
            count: 1,
            on_plate: true,
            plates: vec![(2, "2 · Деталі".into(), false), (3, "Пластина 3".into(), true)],
            printable: true,
            parts: 1,
            all_parts: true,
            layer_range: true,
            ..Default::default()
        }
    }

    #[test]
    fn single_object_menu_enables_editing() {
        let menu = object_menu(&single());
        for label in [
            "Заповнити пластину копіями",
            "Зробити копію",
            "Розділити",
            "Дзеркально",
            "Додати модифікатор",
            "Модифікатор діапазону висоти шарів",
            "Замінити на STL…",
            "Перетворити з дюймів",
            "Редагувати налаштування процесу друку",
        ] {
            assert!(item(&menu, label).enabled, "{label}");
        }
        assert!(!item(&menu, "Перезавантажити з диска").enabled);
        assert_eq!(item(&menu, "Доступно для друку").checked, Some(true));
        assert!(find(&menu, "Об’єднати в збірку").is_none());
        // П’ять ролей частин, у кожній — усі примітиви.
        for (_, label, _) in ADD_VOLUMES {
            assert_eq!(item(&menu, label).submenu.len(), Primitive::ALL.len());
        }
    }

    #[test]
    fn move_submenu_lists_other_plates_and_new_plate() {
        let menu = object_menu(&single());
        let sub = &item(&menu, "Перенести на пластину").submenu;
        let labels: Vec<_> = sub
            .iter()
            .filter_map(|e| match e {
                Entry::Item(i) => Some((i.label.to_string(), i.enabled)),
                Entry::Separator => None,
            })
            .collect();
        assert_eq!(
            labels,
            vec![
                ("2 · Деталі".to_owned(), true),
                ("Пластина 3".to_owned(), false),
                (crate::i18n::tr("Нова пластина"), true),
            ]
        );
        assert!(matches!(sub.last(), Some(Entry::Item(_))));
    }

    #[test]
    fn multi_selection_disables_single_object_actions() {
        let state = ObjectMenuState {
            count: 3,
            printable: false,
            ..single()
        };
        let menu = object_menu(&state);
        for label in [
            "Заповнити пластину копіями",
            "Розділити",
            "Додати елемент",
            "Замінити на STL…",
            "Редагувати налаштування процесу друку",
        ] {
            let it = item(&menu, label);
            assert!(!it.enabled && it.hint.is_some(), "{label}");
        }
        assert!(item(&menu, "Дзеркально").enabled);
        assert!(item(&menu, "Об’єднати в збірку").enabled);
        assert_eq!(item(&menu, "Доступно для друку").checked, Some(false));
    }

    #[test]
    fn locked_plate_disables_edits_but_not_delete_or_export() {
        let state = ObjectMenuState {
            locked: true,
            ..single()
        };
        let menu = object_menu(&state);
        for label in ["Дзеркально", "По центру пластини", "Перетворити з дюймів"] {
            assert!(!item(&menu, label).enabled, "{label}");
        }
        assert!(item(&menu, "Видалити").enabled);
        assert!(item(&menu, "Експортувати як один STL…").enabled);
    }

    #[test]
    fn assembly_with_modifiers_cannot_split_into_objects() {
        let state = ObjectMenuState {
            all_parts: false,
            parts: 2,
            ..single()
        };
        let menu = object_menu(&state);
        assert!(!item(&menu, "На об’єкти").enabled);
        assert!(item(&menu, "На частини").enabled);
        assert!(!item(&menu, "Замінити на STL…").enabled);
    }

    #[test]
    fn bed_menu_reflects_lock_and_target() {
        let state = BedMenuState {
            plate: Some(1),
            target: Some(1),
            has_objects: true,
            ..Default::default()
        };
        let menu = bed_menu(&state);
        assert!(item(&menu, "Розкласти").enabled);
        assert_eq!(item(&menu, "Заблокувати").checked, Some(false));
        assert_eq!(
            item(&menu, "Додати примітив").submenu.len(),
            Primitive::ALL.len()
        );
        let locked = bed_menu(&BedMenuState {
            locked: true,
            target_locked: true,
            ..state
        });
        assert!(!item(&locked, "Розкласти").enabled);
        assert!(!item(&locked, "Додати моделі…").enabled);
        assert_eq!(item(&locked, "Заблокувати").checked, Some(true));
        let empty = bed_menu(&BedMenuState::default());
        assert!(!item(&empty, "Вибрати все").enabled);
        assert!(!item(&empty, "Додати примітив").enabled);
    }
}
