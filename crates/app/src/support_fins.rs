//! External tool: pinned, checksummed upstream CLI. No geometry engine is vendored.
use anyhow::{Context as _, Result, ensure};
use project::{DAffine3, DVec3, InstanceRef, Project, Volume, VolumeKind};
use sha2::{Digest, Sha256};
use std::{
    path::{Path, PathBuf},
    process::Command,
    sync::Arc,
};

pub const VERSION: &str = "a6c9fa2c667db600a721f15270f323666feccc08";
const SHA256: &str = "034a5aa3c9c44a185094ccc91321f5704beeadb09858dbe1a7fa2e40d7cc61ab";
const MARKER: &str = "slicenv_tool_plugin";
const CALIBRATION: &str = "https://github.com/gittrahan/support-fins/blob/a6c9fa2c667db600a721f15270f323666feccc08/docs/CALIBRATION.md";

pub fn directory() -> PathBuf {
    paths::data_dir().join("plugins/support-fins")
}
fn cli() -> PathBuf {
    directory().join("source/plugins/cli/support-fins.js")
}
pub fn installed() -> bool {
    cli().is_file()
        && std::fs::read_to_string(directory().join("VERSION")).is_ok_and(|s| s == VERSION)
}
pub fn runtime() -> Option<String> {
    ["bun", "node", "deno"]
        .into_iter()
        .find(|name| {
            paths::host::command(name).arg("--version").output().is_ok_and(|o| {
                let version = String::from_utf8_lossy(&o.stdout);
                o.status.success() && runtime_supported(name, &version)
            })
        })
        .map(str::to_owned)
}
fn runtime_supported(name: &str, version: &str) -> bool {
    let text = version
        .trim()
        .trim_start_matches("deno ")
        .trim_start_matches('v');
    let mut parts = text.split('.');
    let major = parts
        .next()
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(0);
    let minor = parts
        .next()
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(0);
    match name {
        "node" => major > 20 || (major == 20 && minor >= 10),
        "deno" => major >= 2,
        "bun" => major >= 1,
        _ => false,
    }
}
fn checked(mut command: Command) -> Result<()> {
    let output = command
        .output()
        .context("Не вдалося запустити зовнішню програму")?;
    ensure!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(())
}
pub fn install() -> Result<()> {
    let destination = directory();
    let parent = destination.parent().unwrap();
    std::fs::create_dir_all(parent)?;
    let stage = tempfile::tempdir_in(parent)?;
    let archive = stage.path().join("source.tar.gz");
    let mut download = paths::host::command("curl");
    download
        .args([
            "--fail",
            "--location",
            "--silent",
            "--show-error",
            "--max-time",
            "180",
            "--output",
        ])
        .arg(&archive)
        .arg(format!(
            "https://codeload.github.com/gittrahan/support-fins/tar.gz/{VERSION}"
        ));
    checked(download)?;
    ensure!(
        format!("{:x}", Sha256::digest(std::fs::read(&archive)?)) == SHA256,
        "Support Fins: SHA-256 не збігається"
    );
    let source = stage.path().join("source");
    std::fs::create_dir(&source)?;
    let mut extract = paths::host::command("tar");
    extract
        .arg("-xzf")
        .arg(&archive)
        .arg("--strip-components=1")
        .arg("-C")
        .arg(&source);
    checked(extract)?;
    // Source archives have no package manifest; make ESM explicit for Node 20.
    std::fs::write(
        source.join("package.json"),
        r#"{"private":true,"type":"module"}"#,
    )?;
    std::fs::remove_file(archive)?;
    std::fs::write(stage.path().join("VERSION"), VERSION)?;
    std::fs::copy(source.join("LICENSE"), stage.path().join("LICENSE"))?;
    std::fs::write(
        stage.path().join("PROVENANCE.md"),
        include_str!("../../../docs/support-fins.md"),
    )?;
    // Publish a complete directory; roll back if the final rename fails.
    let ready = stage.path().join("ready");
    std::fs::create_dir(&ready)?;
    for entry in ["source", "VERSION", "LICENSE", "PROVENANCE.md"] {
        std::fs::rename(stage.path().join(entry), ready.join(entry))?;
    }
    let backup = stage.path().join("previous");
    if destination.exists() {
        std::fs::rename(&destination, &backup)?;
    }
    if let Err(err) = std::fs::rename(&ready, &destination) {
        if backup.exists() {
            std::fs::rename(backup, destination)?;
        }
        return Err(err.into());
    }
    Ok(())
}
pub fn remove() -> Result<()> {
    if directory().exists() {
        std::fs::remove_dir_all(directory())?;
    }
    Ok(())
}
fn is_fins(v: &Volume) -> bool {
    v.config
        .get(MARKER)
        .or_else(|| v.config.get("bambu_lab_tool_plugin"))
        .is_some_and(|s| s == "support-fins")
}
pub fn remove_parts(project: &mut Project, key: InstanceRef) {
    if let Some(object) = project.objects.iter_mut().find(|o| o.id == key.object) {
        object.volumes.retain(|v| !is_fins(v));
    }
}
/// Export plate/world coordinates, keeping the input unchanged and excluding old fins.
fn export(project: &Project, key: InstanceRef, path: &Path) -> Result<()> {
    use std::io::Write;
    let object = project.object(key.object)?;
    let instance = project.instance(key)?;
    ensure!(
        instance.transform.is_finite() && instance.transform.matrix3.determinant().abs() > 1e-12,
        "Некоректна трансформація"
    );
    let triangles: usize = object
        .volumes
        .iter()
        .filter(|v| v.kind == VolumeKind::Part && !is_fins(v))
        .map(|v| v.mesh.triangles.len())
        .sum();
    ensure!(
        triangles > 0 && triangles <= u32::MAX as usize,
        "Порожня або завелика сітка"
    );
    let mut out = std::io::BufWriter::new(std::fs::File::create(path)?);
    out.write_all(&[0; 80])?;
    out.write_all(&(triangles as u32).to_le_bytes())?;
    for volume in &object.volumes {
        if volume.kind != VolumeKind::Part || is_fins(volume) {
            continue;
        }
        let transform = instance.transform * volume.transform;
        ensure!(
            transform.is_finite() && transform.matrix3.determinant().abs() > 1e-12,
            "Некоректна трансформація частини"
        );
        for t in &volume.mesh.triangles {
            let mut points = [DVec3::ZERO; 3];
            for (i, index) in t.iter().enumerate() {
                points[i] = transform.transform_point3(DVec3::from(
                    *volume
                        .mesh
                        .positions
                        .get(*index as usize)
                        .context("Індекс поза сіткою")?,
                ));
                ensure!(
                    points[i].is_finite() && points[i].as_vec3().is_finite(),
                    "Некоректна вершина"
                );
            }
            if transform.matrix3.determinant() < 0. {
                points.swap(1, 2);
            }
            let normal = (points[1] - points[0])
                .cross(points[2] - points[0])
                .normalize_or_zero();
            for point in [normal, points[0], points[1], points[2]] {
                for value in point.as_vec3().to_array() {
                    out.write_all(&value.to_le_bytes())?;
                }
            }
            out.write_all(&[0; 2])?;
        }
    }
    out.flush()?;
    Ok(())
}
fn import(path: &Path, transform: DAffine3) -> Result<Volume> {
    // viewer::load_stl centres and seats meshes for display; tool output must
    // retain its exact plate coordinates, including the original translation.
    let mesh = project::read_stl(path)?;
    ensure!(
        !mesh.triangles.is_empty(),
        "Support Fins: немає плавців для цієї орієнтації"
    );
    ensure!(
        mesh.positions.iter().flatten().all(|v| v.is_finite()),
        "Некоректна вершина"
    );
    let mut volume = Volume {
        name: "Support fins".into(),
        mesh: Arc::new(mesh),
        transform: transform.inverse(),
        ..Default::default()
    };
    volume.config.insert(MARKER.into(), "support-fins".into());
    Ok(volume)
}
fn generate(project: Project, key: InstanceRef, options: Vec<(String, String)>) -> Result<Volume> {
    ensure!(
        installed(),
        "Спочатку встановіть Support Fins у налаштуваннях плагінів"
    );
    ensure!(
        !project
            .object(key.object)?
            .volumes
            .iter()
            .any(|v| v.kind == VolumeKind::NegativeVolume),
        "Support Fins: спочатку застосуйте віднімання до сітки"
    );
    let runtime = runtime().context("Потрібен bun, Node.js 20.10+ або Deno 2")?;
    let temp = tempfile::tempdir()?;
    let input = temp.path().join("input.stl");
    let output = temp.path().join("fins.stl");
    export(&project, key, &input)?;
    let mut command = paths::host::command(&runtime);
    if runtime == "deno" {
        command.args(["run", "-RW"]);
    }
    command
        .arg(cli())
        .arg(&input)
        .args(["--fins-only", "-o"])
        .arg(&output);
    for (key, value) in options {
        if key == "tines" {
            if value == "off" {
                command.arg("--no-tines");
            }
        } else {
            command.arg(format!("--{key}")).arg(value);
        }
    }
    checked(command)?;
    ensure!(
        output.is_file(),
        "Support Fins: немає плавців для цієї орієнтації"
    );
    import(&output, project.instance(key)?.transform)
}

use crate::{
    jobs::{JobKind, JobTarget},
    ui,
    widgets::{self, ToastKind},
    workspace::Workspace,
};
use gpui::{Context, Window, div, prelude::*, px};
use gpui_component::input::{Input, InputState};
use std::rc::Rc;

impl Workspace {
    pub(crate) fn open_plugins(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let runtime = runtime().unwrap_or_else(|| crate::i18n::tr("Не знайдено"));
        self.open_modal(
            Rc::new(move |ws, _, cx| {
                let body = div()
                    .p(px(16.))
                    .flex()
                    .flex_col()
                    .gap(px(12.))
                    .child("Support Fins · MIT")
                    .child(format!(
                        "{}: {}",
                        crate::i18n::tr("Версія"),
                        if installed() {
                            "0.7.0 · a6c9fa2c667d"
                        } else {
                            "—"
                        }
                    ))
                    .child(format!("Runtime: {runtime}"))
                    .child(crate::i18n::tr("Потрібен bun, Node.js 20.10+ або Deno 2"))
                    .child(
                        div()
                            .flex()
                            .gap(px(8.))
                            .child(
                                ui::ghost_button(
                                    "fins-install",
                                    None,
                                    if installed() {
                                        "Оновити"
                                    } else {
                                        "Встановити"
                                    },
                                )
                                .on_click(cx.listener(
                                    |this, _, _, cx| this.fins_operation(None, 0, Vec::new(), cx),
                                )),
                            )
                            .child(
                                ui::ghost_button("fins-uninstall", None, "Видалити плагін")
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.fins_operation(None, 1, Vec::new(), cx)
                                    })),
                            ),
                    )
                    .child(
                        ui::ghost_button("fins-calibration", None, "Калібрувальні моделі")
                            .on_click(|_, _, cx| cx.open_url(CALIBRATION)),
                    )
                    .when(ws.jobs.is_running(JobKind::Fins), |d| {
                        d.child(ui::spinner("plugin-progress"))
                    });
                let footer = ui::ghost_button("plugins-close", None, "Готово")
                    .on_click(cx.listener(|this, _, _, cx| this.close_modal(cx)));
                widgets::modal("Плагіни", 540., body, Some(footer.into_any_element()))
            }),
            window,
            cx,
        );
    }
    pub(crate) fn open_support_fins(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(key) = self.selected else { return };
        let profile_value = |machine: bool, name: &str, default: &str| -> String {
            let (kind, selection, editor) = if machine {
                (
                    slicer::Kind::Machine,
                    &self.selection.machine,
                    &self.machine_editor,
                )
            } else {
                (
                    slicer::Kind::Process,
                    &self.selection.process,
                    &self.process_editor,
                )
            };
            let mut values = self
                .presets
                .as_ref()
                .and_then(|s| s.flatten(kind, selection).ok())
                .unwrap_or_default();
            if let Some(object) = self
                .project
                .as_ref()
                .and_then(|p| p.object(key.object).ok())
                && let Some(v) = object.config.get(name)
            {
                values.insert(name.into(), serde_json::Value::String(v.clone()));
            }
            if let Some(editor) = editor {
                values.extend(editor.overrides());
            }
            let value = values
                .get(name)
                .map(|v| v.as_array().and_then(|a| a.first()).unwrap_or(v));
            value
                .map(|v| {
                    v.as_str()
                        .map(str::to_owned)
                        .unwrap_or_else(|| v.to_string())
                })
                .unwrap_or_else(|| default.into())
        };
        let definitions = [
            (
                "nozzle",
                "Сопло, мм",
                profile_value(true, "nozzle_diameter", "0.4"),
            ),
            (
                "layer-height",
                "Висота шару, мм",
                profile_value(false, "layer_height", "0.2"),
            ),
            (
                "gap",
                "Зазор підтримки, мм",
                profile_value(false, "support_top_z_distance", "0.2"),
            ),
            (
                "threshold",
                "Кут нависання від столу, °",
                // Той самий кут від столу, що й поріг підтримок процесу; 0 у процесі —
                // «авто», тоді типове значення плагіна.
                Some(profile_value(false, "support_threshold_angle", "0"))
                    .filter(|v| v.parse::<f32>().is_ok_and(|a| a > 0.0))
                    .unwrap_or_else(|| "45".into()),
            ),
            ("tine-density", "Зчеплення зубців, %", "0".into()),
        ];
        let fields: Vec<_> = definitions
            .into_iter()
            .map(|(flag, label, value)| {
                (
                    flag,
                    label,
                    cx.new(|cx| InputState::new(window, cx).default_value(value)),
                )
            })
            .collect();
        let choices = Rc::new(std::cell::RefCell::new(vec![
            (
                "mode",
                "Розміщення",
                vec![("auto", "Авто"), ("full", "Повне")],
                0usize,
            ),
            (
                "tines",
                "Зубці",
                vec![("on", "Увімкнено"), ("off", "Вимкнено")],
                0,
            ),
            (
                "tine-dir",
                "Напрям зубців",
                vec![("horizontal", "Горизонтально"), ("vertical", "Вертикально")],
                0,
            ),
            (
                "cutout",
                "Вирізи",
                vec![
                    ("none", "Без вирізів"),
                    ("diamond", "Ромби"),
                    ("triangle", "Трикутники"),
                    ("arch", "Арки"),
                    ("lattice", "Решітка"),
                    ("honeycomb", "Стільники"),
                ],
                0,
            ),
            (
                "pad-style",
                "Основа",
                vec![
                    ("auto", "Авто"),
                    ("off", "Вимкнено"),
                    ("light", "Легка"),
                    ("sure", "Міцна"),
                ],
                0,
            ),
        ]));
        self.open_modal(
            Rc::new(move |ws, window, cx| {
                let mut body = div().p(px(16.)).flex().flex_col().gap(px(8.));
                for (_, label, input) in &fields {
                    body = body.child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(12.))
                            .child(div().w(px(250.)).child(crate::i18n::tr(label)))
                            .child(div().w(px(140.)).child(widgets::field(
                                input,
                                Input::new(input),
                                window,
                                cx,
                            ))),
                    );
                }
                for (i, (_, label, values, index)) in choices.borrow().iter().enumerate() {
                    let options = choices.clone();
                    body = body.child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(12.))
                            .child(div().w(px(250.)).child(crate::i18n::tr(label)))
                            .child(
                                ui::ghost_button(("fins-choice", i), None, values[*index].1)
                                    .on_click(cx.listener(move |_, _, _, cx| {
                                        let mut options = options.borrow_mut();
                                        let row = &mut options[i];
                                        row.3 = (row.3 + 1) % row.2.len();
                                        cx.notify();
                                    })),
                            ),
                    );
                }
                body = body.child(crate::i18n::tr(
                    "Плавці спільні для всіх копій об’єкта. Після повороту створіть їх знову.",
                ));
                if !installed() {
                    body = body.child(
                        ui::ghost_button("fins-settings", None, "Встановити плагін…").on_click(
                            cx.listener(|this, _, window, cx| this.open_plugins(window, cx)),
                        ),
                    );
                }
                if ws.jobs.is_running(JobKind::Fins) {
                    body = body.child(ui::spinner("fins-progress"));
                }
                let fields = fields.clone();
                let choices = choices.clone();
                let footer = div()
                    .flex()
                    .gap(px(8.))
                    .child(
                        ui::ghost_button("fins-add", None, "Додати плавці").on_click(cx.listener(
                            move |this, _, _, cx| {
                                let mut options: Vec<_> = fields
                                    .iter()
                                    .map(|(flag, _, input)| {
                                        (flag.to_string(), input.read(cx).value().trim().to_owned())
                                    })
                                    .collect();
                                options.extend(choices.borrow().iter().map(
                                    |(flag, _, values, index)| {
                                        (flag.to_string(), values[*index].0.to_string())
                                    },
                                ));
                                this.fins_operation(Some(key), 2, options, cx);
                            },
                        )),
                    )
                    .child(
                        ui::ghost_button("fins-remove", None, "Видалити плавці").on_click(
                            cx.listener(move |this, _, window, cx| {
                                if this.jobs.is_running(JobKind::Fins) {
                                    return;
                                }
                                this.checkpoint("Видалення плавців");
                                if let Some(project) = &mut this.project {
                                    remove_parts(project, key);
                                }
                                this.selected_volume = None;
                                this.project_changed(window, cx);
                            }),
                        ),
                    )
                    .child(
                        ui::ghost_button("fins-close", None, "Готово")
                            .on_click(cx.listener(|this, _, _, cx| this.close_modal(cx))),
                    );
                widgets::modal("Support Fins…", 540., body, Some(footer.into_any_element()))
            }),
            window,
            cx,
        );
    }
    fn fins_operation(
        &mut self,
        key: Option<InstanceRef>,
        operation: u8,
        options: Vec<(String, String)>,
        cx: &mut Context<Self>,
    ) {
        if self.jobs.is_running(JobKind::Fins) {
            return;
        }
        self.finish_scene_gesture();
        let before = self.project.clone();
        // Встановлення й видалення плагіна документа не стосуються.
        let target = if operation >= 2 {
            self.job_target().geometry().idle_scene()
        } else {
            JobTarget::any()
        };
        self.spawn_job(
            JobKind::Fins,
            target,
            move || match operation {
                0 => {
                    install()?;
                    Ok(None)
                }
                1 => {
                    remove()?;
                    Ok(None)
                }
                _ => generate(
                    before.context("Немає проєкту")?,
                    key.context("Немає об’єкта")?,
                    options,
                )
                .map(Some),
            },
            move |this, result: anyhow::Result<_>, window, cx| match result {
                Ok(Some(volume)) => {
                    if let Some(key) = key {
                        this.checkpoint("Додавання плавців");
                        if let Some(project) = this.project.as_mut() {
                            remove_parts(project, key);
                            if let Some(object) =
                                project.objects.iter_mut().find(|o| o.id == key.object)
                            {
                                object.volumes.push(volume);
                            }
                        }
                        this.project_changed(window, cx);
                    }
                }
                Ok(None) => this.toast(
                    ToastKind::Success,
                    crate::i18n::tr(if operation == 1 {
                        "Плагін видалено"
                    } else {
                        "Плагін встановлено"
                    }),
                    cx,
                ),
                Err(e) => this.toast(ToastKind::Error, format!("Support Fins: {e:#}"), cx),
            },
            cx,
        );
    }
}

/// Пункт меню об’єкта в сцені: відкриває панель плавців для вибраного об’єкта.
pub(crate) fn object_menu(
    ws: &Workspace,
    _key: project::InstanceRef,
) -> Vec<crate::menu::Entry> {
    let item = crate::menu::Item::new("Support Fins…").icon(crate::ui::IconName::Package);
    let item = if ws.selection_keys().len() > 1 {
        item.disabled("Лише для одного об’єкта")
    } else {
        item.on(|ws, window, cx| ws.open_support_fins(window, cx))
    };
    vec![item.entry()]
}

#[cfg(test)]
mod tests {
    #[test]
    fn legacy_and_current_fins_markers_are_recognized() {
        for marker in [super::MARKER, "bambu_lab_tool_plugin"] {
            let mut volume = project::Volume::default();
            volume.config.insert(marker.into(), "support-fins".into());
            assert!(super::is_fins(&volume));
        }
        assert!(!super::is_fins(&project::Volume::default()));
    }

    use super::*;
    fn project() -> (Project, InstanceRef) {
        let mesh = project::IndexedMesh {
            positions: vec![[0., 0., 0.], [2., 0., 0.], [0., 3., 0.]],
            triangles: vec![[0, 1, 2]],
            ..Default::default()
        };
        let volume = Volume {
            mesh: Arc::new(mesh),
            transform: DAffine3::from_translation(DVec3::new(1., 2., 3.)),
            ..Default::default()
        };
        let instance = project::Instance {
            transform: DAffine3::from_scale_rotation_translation(
                DVec3::new(-2., 1., 1.),
                project::DQuat::from_rotation_y(0.61),
                DVec3::new(100., 50., 10.),
            ),
            ..Default::default()
        };
        let object = project::Object {
            id: 7,
            volumes: vec![volume],
            instances: vec![instance],
            ..Default::default()
        };
        let mut project = Project::default();
        project.objects.push(object);
        (
            project,
            InstanceRef {
                object: 7,
                instance: 0,
            },
        )
    }
    #[test]
    fn world_stl_roundtrip_and_removable_part_preserve_original() {
        let (mut p, key) = project();
        let original = p.clone();
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("mesh.stl");
        export(&p, key, &path).unwrap();
        let volume = import(&path, p.instance(key).unwrap().transform).unwrap();
        // Export reverses mirrored winding; imported volume is world-space with inverse instance placement.
        let world = p.instance(key).unwrap().transform * volume.transform;
        let expected = p.instance(key).unwrap().transform * p.objects[0].volumes[0].transform;
        for (i, j) in [0, 2, 1].into_iter().enumerate() {
            let actual = world.transform_point3(DVec3::from(volume.mesh.positions[i]));
            let wanted = expected.transform_point3(DVec3::from(
                original.objects[0].volumes[0].mesh.positions[j],
            ));
            assert!((actual - wanted).length() < 1e-4);
        }
        p.objects[0].volumes.push(volume);
        export(&p, key, &path).unwrap(); // old fins must never be re-exported
        assert_eq!(viewer::load_stl(&path).unwrap().positions.len(), 3);
        remove_parts(&mut p, key);
        assert_eq!(p, original);
    }
    #[test]
    fn fins_survive_3mf_and_history_can_undo_addition() {
        let (mut p, key) = project();
        p.plates.push(project::Plate {
            index: 1,
            ..Default::default()
        });
        p.sync_plates();
        let before = p.clone();
        let temp = tempfile::tempdir().unwrap();
        let stl = temp.path().join("fins.stl");
        export(&p, key, &stl).unwrap();
        let fins = import(&stl, p.instance(key).unwrap().transform).unwrap();
        p.objects[0].volumes.push(fins);
        let mut history = project::history::History::default();
        let selection = project::history::Selection {
            primary: Some(key),
            ..Default::default()
        };
        assert!(history.record(
            project::history::Entry {
                label: "Додавання плавців".into(),
                project: before.clone(),
                selection: selection.clone()
            },
            &p
        ));
        let file = temp.path().join("parts.3mf");
        p.save_3mf(&file).unwrap();
        let mut loaded = Project::load(&file).unwrap();
        assert_eq!(loaded.objects[0].volumes.len(), 2);
        assert!(is_fins(&loaded.objects[0].volumes[1]));
        let loaded_key = InstanceRef {
            object: loaded.objects[0].id,
            instance: 0,
        };
        remove_parts(&mut loaded, loaded_key);
        assert_eq!(loaded.objects[0].volumes.len(), 1);
        history.undo(&mut p, selection.clone()).unwrap();
        assert_eq!(p, before);
        history.redo(&mut p, selection).unwrap();
        assert_eq!(p.objects[0].volumes.len(), 2);
    }
    #[test]
    fn removal_uses_plugin_identity_not_user_part_name() {
        let (mut p, key) = project();
        p.objects[0].volumes[0].name = "Support fins".into();
        let original = p.clone();
        remove_parts(&mut p, key);
        assert_eq!(p, original);
    }
    #[test]
    fn runtime_minimum_versions() {
        assert!(!runtime_supported("node", "v20.9.0"));
        assert!(runtime_supported("node", "v20.10.0"));
        assert!(runtime_supported("node", "v22.0.0"));
        assert!(runtime_supported("deno", "deno 2.5.1 (stable, release)"));
        assert!(!runtime_supported("deno", "deno 1.9.0"));
        assert!(runtime_supported("bun", "1.3.0"));
    }
    #[test]
    fn invalid_transform_and_indices_are_rejected() {
        let (mut p, key) = project();
        let temp = tempfile::NamedTempFile::new().unwrap();
        p.objects[0].instances[0].transform = DAffine3::from_scale(DVec3::ZERO);
        assert!(export(&p, key, temp.path()).is_err());
        p.objects[0].instances[0].transform = DAffine3::IDENTITY;
        Arc::make_mut(&mut p.objects[0].volumes[0].mesh).triangles[0][0] = 999;
        assert!(export(&p, key, temp.path()).is_err());
    }
}
