//! Managed package cards and streamed backend progress.
use crate::{
    theme::zed,
    ui::{self, IconName, Toggle as _},
    widgets::{self, ToastKind},
    workspace::Workspace,
};
use futures::StreamExt;
use gpui::{Context, SharedString, Window, div, prelude::*, px};
use serde_json::Value;
use std::rc::Rc;

#[derive(Clone, Debug, Default)]
pub struct Progress {
    pub stage: String,
    pub downloaded: u64,
    pub total: Option<u64>,
    pub percent: Option<f32>,
}
impl Progress {
    fn from_event(v: &Value) -> Option<Self> {
        (v["event"] == "progress").then(|| Self {
            stage: v["stage"].as_str().unwrap_or("").into(),
            downloaded: v["downloaded"].as_u64().unwrap_or(0),
            total: v["total"].as_u64().filter(|v| *v > 0),
            percent: v["percent"]
                .as_f64()
                .filter(|v| v.is_finite())
                .map(|v| v.clamp(0., 100.) as f32),
        })
    }
    pub(crate) fn label(&self) -> String {
        match self.stage.as_str() {
            "download" => match self.total {
                Some(total) => crate::tr_format!(
                    "Завантаження · {:.1} / {:.1} МБ{}",
                    self.downloaded as f64 / 1048576.,
                    total as f64 / 1048576.,
                    self.percent
                        .map(|p| format!(" · {p:.0}%"))
                        .unwrap_or_default()
                ),
                None => {
                    crate::tr_format!("Завантаження · {:.1} МБ", self.downloaded as f64 / 1048576.)
                }
            },
            "verify" => "Перевірка SHA256…".into(),
            "extract" => "Розпакування…".into(),
            "register" => "Реєстрація версії…".into(),
            // Orca 2.5 пакує профілі в .opc, тож каталог тягнеться з GitHub — хвилини.
            "profiles" => match self.total {
                Some(total) => crate::tr_format!(
                    "Завантаження профілів · {} / {}",
                    self.downloaded,
                    total
                ),
                None => "Завантаження профілів…".into(),
            },
            "done" => "Встановлення завершено".into(),
            _ => "Підготовка завантаження…".into(),
        }
    }
}
pub struct Packages {
    pub busy: bool,
    pub releases: Vec<Value>,
    pub managed: Vec<Value>,
    pub error: Option<String>,
    pub status: String,
    pub channel: &'static str,
    pub active_asset: Option<String>,
    pub progress: Option<Progress>,
    pub operation: u64,
    /// Скасування поточної операції (вбиває процес менеджера пакетів).
    pub cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// Родина, яку майстер попросив встановити, коли список випусків ще не отримано.
    pub pending_family: Option<&'static str>,
    /// Шлях до виконуваного файлу щойно встановленого рушія (для майстра).
    pub installed_executable: Option<std::path::PathBuf>,
    /// Родини, для яких розгорнуто «Старші версії».
    pub older_open: std::collections::BTreeSet<String>,
}
impl Default for Packages {
    fn default() -> Self {
        Self {
            busy: false,
            releases: Vec::new(),
            managed: Vec::new(),
            error: None,
            status: String::new(),
            channel: "stable",
            active_asset: None,
            progress: None,
            operation: 0,
            cancel: Default::default(),
            pending_family: None,
            installed_executable: None,
            older_open: Default::default(),
        }
    }
}
impl Packages {
    fn trace(&self) {
        if paths::env("SLICENV_UI_TRACE").is_some() {
            eprintln!(
                "PACKAGE_TRACE {}",
                serde_json::json!({
                    "busy": self.busy, "channel": self.channel, "operation": self.operation,
                    "active_asset": self.active_asset, "stage": self.progress.as_ref().map(|p| &p.stage),
                    "percent": self.progress.as_ref().and_then(|p| p.percent), "error": self.error,
                    "releases": self.releases.iter().map(|r| &r["tag"]).collect::<Vec<_>>(),
                    "installed": self.managed.iter().map(|m| &m["version"]).collect::<Vec<_>>(),
                })
            );
        }
    }
}
fn asset_key(family: &str, asset: &Value) -> String {
    format!("{family}:{}", asset["id"])
}
fn version_key(release: &Value, asset: &Value) -> String {
    if release["channel"] == "nightly" {
        format!("nightly-{}-{}", release["release_id"], asset["id"])
    } else {
        release["tag"]
            .as_str()
            .unwrap_or("")
            .trim_start_matches(['v', 'V'])
            .to_owned()
    }
}
pub(crate) fn installed<'a>(managed: &'a [Value], release: &Value, asset: &Value) -> Option<&'a Value> {
    managed.iter().find(|m| {
        m["family"] == release["family"]
            && (m["asset_id"]
                .as_u64()
                .is_some_and(|id| Some(id) == asset["id"].as_u64())
                || m["version"].as_str() == Some(version_key(release, asset).as_str()))
    })
}
fn active(ws: &Workspace, manifest: &Value) -> bool {
    ws.engine.as_ref().is_some_and(|e| {
        let actual = std::fs::canonicalize(&e.path).unwrap_or_else(|_| e.path.clone());
        let executable = manifest["executable_path"]
            .as_str()
            .map(std::path::PathBuf::from);
        let directory = manifest["directory"]
            .as_str()
            .map(std::path::PathBuf::from)
            .or_else(|| {
                executable
                    .as_ref()
                    .and_then(|p| p.parent().map(|p| p.to_owned()))
            });
        directory.is_some_and(|d| actual.starts_with(std::fs::canonicalize(&d).unwrap_or(d)))
    })
}
/// Менеджер рушіїв (`crate::engine_manager`): Linux — AppImage, Windows — portable zip,
/// macOS — dmg (на Mac не перевірено).
pub(crate) const MANAGER_SUPPORTED: bool = true;

/// Операція менеджера; `cancel` перериває завантаження чи розпакування (часткова установка
/// лишається лише в службовій `.staging`-теці, яку менеджер прибирає).
pub(crate) fn run_cancellable(
    op: &crate::engine_manager::Op,
    events: Option<&futures::channel::mpsc::UnboundedSender<Value>>,
    cancel: &std::sync::atomic::AtomicBool,
) -> anyhow::Result<Value> {
    let mut forward = |event: Value| {
        if let Some(tx) = events {
            let _ = tx.unbounded_send(event);
        }
    };
    let result = crate::engine_manager::run(op, &mut forward, cancel);
    if cancel.load(std::sync::atomic::Ordering::Acquire) {
        anyhow::bail!("{}", crate::i18n::tr("Скасовано"));
    }
    result
}

/// Номер версії з тегу як числа: «v2.5.0» → [2, 5, 0], «v02.09.01.70» → [2, 9, 1, 70].
fn version_key_numbers(tag: &str) -> Vec<u64> {
    crate::engine_manager::clean_tag(tag)
        .split(|c: char| !c.is_ascii_digit())
        .take_while(|p| !p.is_empty())
        .filter_map(|p| p.parse().ok())
        .collect()
}

/// Найновіший стабільний випуск родини за номером версії (не за датою публікації: виправлення
/// для старої гілки буває опубліковане пізніше за новішу версію).
pub(crate) fn newest_stable<'a>(releases: &'a [Value], family: &str) -> Option<&'a Value> {
    releases
        .iter()
        .filter(|r| r["family"] == family && r["channel"] == "stable")
        .max_by_key(|r| version_key_numbers(r["tag"].as_str().unwrap_or("")))
}

/// Збірка, яку майстер ставить «в один клік»: на Linux — AppImage для новішої Ubuntu, деінде —
/// перший пакет для цієї системи з SHA256 (без контрольної суми менеджер установку відхиляє).
pub(crate) fn recommended_asset(release: &Value) -> Option<&Value> {
    let assets: Vec<&Value> = release["assets"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|a| a["sha256"].as_str().is_some_and(|v| v.len() == 64))
        .collect();
    let name = |a: &Value| {
        a["name"]
            .as_str()
            .unwrap_or("")
            .to_lowercase()
            .replace(['-', '_', '.'], "")
    };
    assets
        .iter()
        .find(|a| name(a).contains("ubuntu24") && name(a).ends_with("appimage"))
        .or_else(|| assets.iter().find(|a| name(a).ends_with("appimage")))
        .or_else(|| assets.first())
        .copied()
}

/// Порядок показу: OrcaSlicer, потім Bambu Studio; у межах родини — від найновішої версії.
fn ordered_releases(releases: &[Value]) -> Vec<usize> {
    let mut order: Vec<usize> = (0..releases.len()).collect();
    order.sort_by_key(|&i| {
        let r = &releases[i];
        (
            r["family"] != "orca",
            std::cmp::Reverse(version_key_numbers(r["tag"].as_str().unwrap_or(""))),
        )
    });
    order
}

fn older_toggle(ws: &Workspace, family: &str, count: usize, ri: usize, cx: &mut Context<Workspace>) -> gpui::AnyElement {
    let open = ws.packages.older_open.contains(family);
    let family = family.to_owned();
    ui::ghost_button(
        ("package-older", ri),
        Some(if open { IconName::ChevronDown } else { IconName::ChevronRight }),
        crate::tr_format!("Старші версії {} ({count})", product_name(&family)),
    )
    .on_click(cx.listener(move |this, _, _, cx| {
        if !this.packages.older_open.remove(&family) {
            this.packages.older_open.insert(family.clone());
        }
        cx.notify();
    }))
    .into_any_element()
}

pub(crate) fn product_name(family: &str) -> &str {
    match family {
        "orca" => "OrcaSlicer",
        "bambu" => "Bambu Studio",
        other => other,
    }
}

pub(crate) fn engine_logo(family: &str) -> gpui::AnyElement {
    gpui::img(if family == "orca" {
        "engines/orca.png"
    } else {
        "engines/bambu.png"
    })
    .size(px(28.))
    .flex_none()
    .into_any_element()
}
pub(crate) fn asset_label(asset: &Value) -> String {
    let name = asset["name"].as_str().unwrap_or("");
    let lower = name.to_lowercase();
    let platform_name = lower.replace(['-', '_', '.'], "");
    let platform = if platform_name.contains("ubuntu22") {
        "Ubuntu 22.04"
    } else if platform_name.contains("ubuntu24") {
        "Ubuntu 24.04"
    } else if platform_name.contains("ubuntu20") {
        "Ubuntu 20.04"
    } else if lower.ends_with(".zip") {
        if crate::engine_manager::asset_arch(name) == Some(crate::engine_manager::Arch::Arm64) {
            "Windows ARM64"
        } else {
            "Windows x64"
        }
    } else if lower.ends_with(".dmg") {
        "macOS"
    } else {
        "Linux x86_64"
    };
    let format = if lower.ends_with(".appimage") {
        "AppImage"
    } else if lower.ends_with(".zip") {
        "Portable ZIP"
    } else if lower.ends_with(".dmg") {
        "DMG"
    } else {
        "Архів"
    };
    crate::tr_format!(
        "{platform} · {} · {:.0} МБ",
        crate::i18n::tr(format),
        asset["size"].as_f64().unwrap_or(0.) / 1048576.
    )
}

/// Сторінка завантаження Bambu Studio (коли на GitHub немає portable-збірки).
const BAMBU_DOWNLOAD: &str = "https://bambulab.com/en/download/studio";

impl Workspace {
    pub(crate) fn package_action(
        &mut self,
        op: crate::engine_manager::Op,
        asset: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.packages.busy
            || self.engine_loading
            || self.slicing()
        {
            return;
        }
        self.packages.busy = true;
        self.packages.error = None;
        self.packages.progress = None;
        self.packages.active_asset = asset;
        self.packages.operation += 1;
        self.packages.cancel = Default::default();
        let cancel = self.packages.cancel.clone();
        self.packages.trace();
        let list_releases = matches!(op, crate::engine_manager::Op::ListReleases { .. });
        let operation = self.packages.operation;
        self.packages.status = if list_releases {
            "Отримання випусків…"
        } else {
            "Підготовка операції…"
        }
        .into();
        let (tx, mut events) = futures::channel::mpsc::unbounded();
        cx.spawn_in(window, async move |this, cx| {
            let work = cx.background_executor().spawn(async move {
                let result = run_cancellable(&op, Some(&tx), &cancel);
                drop(tx);
                let managed = crate::engine_manager::list_default();
                (result, managed)
            });
            while let Some(event) = events.next().await {
                this.update_in(cx, |this, _, cx| {
                    if this.packages.operation == operation {
                        this.packages.progress = Progress::from_event(&event);
                        this.packages.trace();
                        cx.notify();
                    }
                })
                .ok();
            }
            let (result, managed) = work.await;
            this.update_in(cx, |this, window, cx| {
                if this.packages.operation != operation {
                    return;
                }
                this.packages.busy = false;
                this.packages.active_asset = None;
                if let Ok(managed) = managed.as_ref() {
                    this.packages.managed = managed.as_array().cloned().unwrap_or_default();
                }
                match result {
                    Ok(value) => {
                        if let Some(error) = value["profile_error"].as_str() {
                            this.packages.error = Some(crate::tr_format!(
                                "Рушій встановлено; не вдалося отримати профілі: {error}"
                            ));
                        }
                        if list_releases {
                            this.packages.releases = value.as_array().cloned().unwrap_or_default();
                        } else if let Some(path) = value["executable_path"].as_str() {
                            this.packages.installed_executable = Some(path.into());
                        }
                        this.packages.status = if list_releases {
                            "Список оновлено"
                        } else {
                            "Готово"
                        }
                        .into();
                        if let Err(e) = managed {
                            this.packages.error = Some(crate::tr_format!(
                                "Операція завершена; список не оновився: {e:#}"
                            ));
                        }
                    }
                    Err(e) => {
                        this.packages.status = "Операцію не завершено".into();
                        this.packages.error = Some(format!("{e:#}"));
                        this.packages.pending_family = None;
                        if !this.packages.cancel.load(std::sync::atomic::Ordering::Acquire) {
                            this.toast(ToastKind::Error, crate::tr_format!("Пакети: {e:#}"), cx);
                        }
                    }
                }
                this.packages.trace();
                this.wizard_packages_done(window, cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }
    pub(crate) fn cancel_packages(&mut self, cx: &mut Context<Self>) {
        if self.packages.busy {
            self.packages.cancel.store(true, std::sync::atomic::Ordering::Release);
            self.packages.pending_family = None;
            cx.notify();
        }
    }
    pub(crate) fn refresh_packages(
        &mut self,
        channel: &'static str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.packages.busy {
            return;
        }
        self.packages.channel = channel;
        self.packages.releases.clear();
        self.package_action(
            crate::engine_manager::Op::ListReleases { channel: channel.into(), limit: 6 },
            None,
            window,
            cx,
        );
    }
    pub(crate) fn install_asset(
        &mut self,
        release: &Value,
        asset: &Value,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if installed(&self.packages.managed, release, asset).is_some() {
            return;
        }
        let Some(family) = release["family"].as_str() else {
            return;
        };
        let Some(tag) = release["tag"].as_str() else {
            return;
        };
        let Some(asset_id) = asset["id"].as_u64() else {
            return;
        };
        let vendors = self
            .presets
            .as_ref()
            .and_then(|store| store.vendor_for_machine(&self.selection.machine))
            .into_iter()
            .collect();
        self.package_action(
            crate::engine_manager::Op::Install {
                family: family.into(),
                tag: tag.into(),
                asset_id,
                vendors,
            },
            Some(asset_key(family, asset)),
            window,
            cx,
        );
    }
    fn remove_package(&mut self, manifest: Value, window: &mut Window, cx: &mut Context<Self>) {
        if self.packages.busy || active(self, &manifest) {
            return;
        }
        self.open_modal(
            Rc::new(move |_, _, cx| {
                let manifest = manifest.clone();
                let family = manifest["family"].as_str().unwrap_or("").to_owned();
                let version = manifest["version"].as_str().unwrap_or("").to_owned();
                widgets::modal(
                    "Видалити встановлену версію?",
                    480.,
                    div().p(px(12.)).text_sm().child(crate::tr_format!(
                        "Видалити {family} {version}? Інші версії залишаться.",
                        family = product_name(&family)
                    )),
                    Some(
                        ui::ghost_button(
                            "package-remove-confirm",
                            Some(IconName::Trash),
                            "Видалити",
                        )
                        .on_click(cx.listener(move |this, _, window, cx| {
                            if active(this, &manifest) {
                                return;
                            }
                            this.package_action(
                                crate::engine_manager::Op::Remove {
                                    family: family.clone(),
                                    version: version.clone(),
                                },
                                None,
                                window,
                                cx,
                            );
                            this.show_packages(window, cx);
                        }))
                        .into_any_element(),
                    ),
                )
                .into_any_element()
            }),
            window,
            cx,
        );
    }
    fn show_packages(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_modal(Rc::new(|ws,window,cx|{
            let z=zed();
            let mut rows=div().id("package-list").max_h(px((f32::from(window.viewport_size().height)-300.).clamp(140.,460.))).overflow_y_scroll().flex().flex_col().gap(px(10.));
            // Кожна родина: найновіша версія відкрито, старші — під «Старші версії (N)».
            let order=ordered_releases(&ws.packages.releases);
            for(position,&ri) in order.iter().enumerate() {
                let release=&ws.packages.releases[ri];
                let family=release["family"].as_str().unwrap_or("");let tag=release["tag"].as_str().unwrap_or("");
                let first=position==0 || ws.packages.releases[order[position-1]]["family"]!=release["family"];
                let in_family=order.iter().filter(|&&i|ws.packages.releases[i]["family"]==release["family"]).count();
                let open=ws.packages.older_open.contains(family);
                // Встановлені старші версії видно й у згорнутому стані.
                let has_installed=release["assets"].as_array().into_iter().flatten().any(|a|installed(&ws.packages.managed,release,a).is_some());
                if !first && !open && !has_installed {continue;}
                let channel=release["channel"].as_str().unwrap_or("stable");
                let url=release["html_url"].as_str().unwrap_or("").to_owned();
                let mut card=div().flex().flex_col().p(px(10.)).gap(px(8.)).rounded(px(6.)).border_1().border_color(z.border_variant).bg(z.element.opacity(0.45))
                    .child(div().flex().items_center().gap(px(8.)).child(engine_logo(family))
                        .child(div().flex_1().text_sm().child(format!("{} · {tag}",if family=="orca" {"OrcaSlicer"} else {"Bambu Studio"})))
                        .child(div().text_xs().px(px(6.)).py(px(2.)).rounded(px(4.)).bg(z.element_selected).child(channel.to_owned()))
                        .when(url.starts_with("https://"),|d|d.child(ui::icon_button(("package-changelog",ri),IconName::ExternalLink,"Джерело та changelog").on_click(move|_,_,cx|cx.open_url(&url)))));
                for(ai,asset) in release["assets"].as_array().into_iter().flatten().enumerate() {
                    let manifest=installed(&ws.packages.managed,release,asset);
                    let active=manifest.is_some_and(|m|active(ws,m));
                    let exact=manifest.is_some_and(|m|m["asset_id"].as_u64()==asset["id"].as_u64());
                    let loading=ws.packages.active_asset.as_deref()==Some(asset_key(family,asset).as_str());
                    let valid=asset["sha256"].as_str().is_some_and(|v|v.len()==64);
                    let release=release.clone();let asset=asset.clone();
                    let mut row=div().flex().items_center().gap(px(8.)).min_h(px(28.)).child(div().flex_1().text_sm().child(asset_label(&asset)));
                    if loading {
                        row=row.child(ui::spinner(SharedString::from(format!("package-row-loading-{ri}-{ai}")))).child(div().text_xs().child(ws.packages.progress.as_ref().map(|p|p.label()).unwrap_or_else(||"Підготовка…".into())));
                    } else if let Some(manifest)=manifest {
                        let manifest=manifest.clone();
                        row=row.child(ui::icon(IconName::CircleCheck).text_color(z.success))
                            .child(div().text_xs().text_color(z.success).child(crate::i18n::tr(if active {"Активна"} else if exact {"Встановлено"} else {"Інша збірка встановлена"})))
                            .child(ui::icon_button(SharedString::from(format!("package-remove-{ri}-{ai}")),IconName::Trash,if active {"Спочатку вибери інший рушій"} else {"Видалити цю версію"})
                                .inert(active || ws.packages.busy).when(!active && !ws.packages.busy,|d|d.on_click(cx.listener(move|this,_,window,cx|this.remove_package(manifest.clone(),window,cx)))));
                    } else {
                        row=row.child(ui::ghost_button(SharedString::from(format!("package-install-{ri}-{ai}")),Some(IconName::Download),if valid {"Встановити"} else {"Немає SHA256"})
                            .inert(!valid || ws.packages.busy).when(valid && !ws.packages.busy,|d|d.on_click(cx.listener(move|this,_,window,cx|this.install_asset(&release,&asset,window,cx)))));
                    }
                    card=card.child(row);
                }
                rows=rows.child(card);
                // «Старші версії (N)» — одразу після найновішої версії родини.
                if first && in_family>1 {
                    rows=rows.child(older_toggle(ws,family,in_family-1,ri,cx));
                }
            }
            // Keep older or rotated Nightly installs visible even if their release
            // assets no longer occur in the selected channel's newest list.
            for(i,m) in ws.packages.managed.iter().enumerate().filter(|(_,m)|!ws.packages.releases.iter().any(|r|r["assets"].as_array().into_iter().flatten().any(|a|installed(std::slice::from_ref(*m),r,a).is_some()))) {
                let is_active=active(ws,m);let manifest=m.clone();let url=m["source_url"].as_str().unwrap_or("").to_owned();
                rows=rows.child(div().flex().items_center().gap(px(8.)).p(px(10.)).border_1().border_color(z.border_variant).rounded(px(6.))
                    .child(engine_logo(m["family"].as_str().unwrap_or("")))
                    .child(div().flex_1().text_sm().child(format!("{} · {}",product_name(m["family"].as_str().unwrap_or("")),m["version"].as_str().unwrap_or(""))))
                    .child(div().text_xs().text_color(z.success).child(crate::i18n::tr(if is_active {"Активна"}else{"Встановлено"})))
                    .when(url.starts_with("https://"),|d|d.child(ui::icon_button(("package-other-source",i),IconName::ExternalLink,"Джерело та changelog").on_click(move|_,_,cx|cx.open_url(&url))))
                    .child(ui::icon_button(("package-other-remove",i),IconName::Trash,"Видалити").inert(is_active || ws.packages.busy)
                        .when(!is_active && !ws.packages.busy,|d|d.on_click(cx.listener(move|this,_,window,cx|this.remove_package(manifest.clone(),window,cx))))));
            }
            let channels=div().flex().items_center().gap(px(4.)).children([("stable","Stable"),("beta","Beta / RC"),("nightly","Nightly"),("all","Усі")].into_iter().enumerate().map(|(i,(channel,label))|{
                ui::ghost_button(("package-channel",i),None,label).toggled(ws.packages.channel==channel).inert(ws.packages.busy)
                    .when(!ws.packages.busy,|d|d.on_click(cx.listener(move|this,_,window,cx|this.refresh_packages(channel,window,cx))))
            })).child(div().flex_1()).child(ui::icon_button("package-refresh",IconName::RefreshCw,"Оновити список").inert(ws.packages.busy)
                .when(!ws.packages.busy,|d|d.on_click(cx.listener(|this,_,window,cx|this.refresh_packages(this.packages.channel,window,cx)))));
            let mut status=div().flex().flex_col().gap(px(6.));
            if ws.packages.busy {
                status=status.child(div().flex().items_center().gap(px(8.)).child(ui::spinner("package-operation"))
                    .child(div().text_sm().child(ws.packages.progress.as_ref().map(|p|p.label()).unwrap_or_else(||crate::i18n::tr(&ws.packages.status)))));
                if let Some(percent)=ws.packages.progress.as_ref().and_then(|p|p.percent).filter(|_|ws.packages.progress.as_ref().is_some_and(|p|matches!(p.stage.as_str(),"download"|"profiles"))) {status=status.child(widgets::progress(percent));}
            } else {status=status.child(div().text_xs().text_color(z.text_muted).child(crate::i18n::tr(&ws.packages.status)));}
            let mut body=div().flex().flex_col().p(px(12.)).gap(px(10.))
                .child(div().text_xs().text_color(z.text_muted).child(crate::i18n::tr(match crate::engine_manager::Os::CURRENT {
                    crate::engine_manager::Os::Windows => "Офіційні portable-збірки для Windows під ваш процесор: розпаковуються в теку Slicenv без інсталятора, прав адміністратора й ярликів. Нові версії встановлюються поруч; активний рушій обирається окремо.",
                    crate::engine_manager::Os::Mac => "Офіційні збірки для macOS (DMG). Нові версії встановлюються поруч; активний рушій обирається окремо.",
                    crate::engine_manager::Os::Linux => "Офіційні Linux x86_64 пакети. Нові версії встановлюються поруч; активний рушій обирається окремо.",
                })))
                .child(channels).child(status).child(rows);
            if ws.packages.releases.is_empty() && !ws.packages.busy {body=body.child(div().text_sm().child(crate::i18n::tr("У цьому каналі немає опублікованих збірок для цієї системи.")));}
            // Bambu публікує portable-zip для Windows; якщо його в каналі немає — лише інсталятор.
            if cfg!(windows) && !ws.packages.busy && !ws.packages.releases.is_empty() && !ws.packages.releases.iter().any(|r| r["family"]=="bambu") {
                body=body.child(div().flex().items_center().gap(px(8.)).p(px(10.)).rounded(px(6.)).border_1().border_color(z.border_variant)
                    .child(engine_logo("bambu"))
                    .child(div().flex_1().text_sm().child(crate::i18n::tr("Bambu Studio на Windows встановлюється офіційним інсталятором.")))
                    .child(ui::ghost_button("package-bambu-download",Some(IconName::ExternalLink),"Сторінка завантаження").on_click(|_,_,cx|cx.open_url(BAMBU_DOWNLOAD)))
                    .child(ui::ghost_button("package-bambu-rescan",Some(IconName::RefreshCw),"Шукати знову").on_click(cx.listener(|this,_,_,cx|{this.refresh_engine_lists();cx.notify();}))));
            }
            if let Some(error)=&ws.packages.error {body=body.child(div().text_xs().text_color(z.error).child(error.clone()));}
            widgets::modal("Пакети рушіїв",860.,body,None).into_any_element()
        }),window,cx);
    }
    pub fn open_packages(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.show_packages(window, cx);
        if self.packages.releases.is_empty() && !self.packages.busy {
            self.refresh_packages(self.packages.channel, window, cx);
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn identifies_installed_asset_and_reserves_shared_version_directory() {
        let r = serde_json::json!({"family":"orca","tag":"v2.4.1","channel":"stable"});
        let a = serde_json::json!({"id":10});
        let installed_versions =
            vec![serde_json::json!({"family":"orca","version":"2.4.1","asset_id":10})];
        assert!(installed(&installed_versions, &r, &a).is_some());
        assert!(installed(&installed_versions, &r, &serde_json::json!({"id":11})).is_some());
        assert!(
            installed(
                &installed_versions,
                &serde_json::json!({"family":"bambu","tag":"v2.4.1"}),
                &a
            )
            .is_none()
        );
        assert_ne!(
            version_key(
                &serde_json::json!({"channel":"nightly","release_id":1}),
                &serde_json::json!({"id":10})
            ),
            version_key(
                &serde_json::json!({"channel":"nightly","release_id":1}),
                &serde_json::json!({"id":11})
            )
        );
    }
    #[test]
    fn asset_labels_distinguish_upstream_platform_spellings() {
        for (name, expected) in [
            ("BambuStudio_ubuntu22.04.AppImage", "Ubuntu 22.04"),
            ("BambuStudio_ubuntu-24.04.AppImage", "Ubuntu 24.04"),
            ("Orca_Ubuntu2404.AppImage", "Ubuntu 24.04"),
        ] {
            assert!(
                asset_label(&serde_json::json!({"name":name,"size":1048576})).starts_with(expected)
            );
        }
    }
    #[test]
    fn newest_stable_is_chosen_by_version_not_by_order() {
        let releases = vec![
            serde_json::json!({"family":"orca","tag":"v2.4.2","channel":"stable"}),
            serde_json::json!({"family":"orca","tag":"v2.5.0-rc","channel":"beta"}),
            serde_json::json!({"family":"orca","tag":"v2.5.0","channel":"stable"}),
            serde_json::json!({"family":"orca","tag":"v2.3.1","channel":"stable"}),
            serde_json::json!({"family":"bambu","tag":"v02.09.01.70","channel":"stable"}),
        ];
        assert_eq!(newest_stable(&releases, "orca").unwrap()["tag"], "v2.5.0");
        assert_eq!(newest_stable(&releases, "bambu").unwrap()["tag"], "v02.09.01.70");
        assert!(asset_label(&serde_json::json!({"name":"OrcaSlicer_Windows_V2.5.0_arm64_portable.zip","size":1})).starts_with("Windows ARM64"));
    }
    #[test]
    fn progress_keeps_unknown_lengths_indeterminate() {
        let p = Progress::from_event(
            &serde_json::json!({"event":"progress","stage":"download","downloaded":123}),
        )
        .unwrap();
        assert!(p.percent.is_none() && p.total.is_none());
        let p = Progress::from_event(&serde_json::json!({"event":"progress","stage":"extract"}))
            .unwrap();
        assert_eq!(p.label(), "Розпакування…");
    }
}
