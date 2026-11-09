//! Readable setting descriptions and a persistent, clickable documentation view.
use crate::{
    theme::{self, zed},
    ui::{self, IconName},
    widgets,
    workspace::Workspace,
};
use gpui::{App, Context, SharedString, Window, div, img, prelude::*, px};
use gpui_component::scroll::ScrollableElement;
use gpui_component::tooltip::Tooltip;
use std::rc::Rc;

/// Return the embedded illustration asset path for a setting, using the active theme.
pub fn illustration(key: &str) -> Option<&'static str> {
    let suffix = if theme::is_light() {
        ".png"
    } else {
        "_dark.png"
    };
    let base = match key {
        "bottom_color_penetration_layers" => "help/bottom_color_penetration_layers",
        "top_color_penetration_layers" => "help/top_color_penetration_layers",
        "seam_position" => "help/seam_position",
        "top_shell_layers" => "help/top_shell_layers",
        _ => return None,
    };
    Some(match (base, suffix) {
        ("help/bottom_color_penetration_layers", ".png") => {
            "help/bottom_color_penetration_layers.png"
        }
        ("help/bottom_color_penetration_layers", _) => {
            "help/bottom_color_penetration_layers_dark.png"
        }
        ("help/top_color_penetration_layers", ".png") => "help/top_color_penetration_layers.png",
        ("help/top_color_penetration_layers", _) => "help/top_color_penetration_layers_dark.png",
        ("help/seam_position", ".png") => "help/seam_position.png",
        ("help/seam_position", _) => "help/seam_position_dark.png",
        ("help/top_shell_layers", ".png") => "help/top_shell_layers.png",
        _ => "help/top_shell_layers_dark.png",
    })
}

pub fn tooltip(
    title: SharedString,
    description: SharedString,
    has_wiki: bool,
    window: &mut Window,
    cx: &mut App,
) -> gpui::AnyView {
    tooltip_inner(title, description, has_wiki, None, window, cx)
}

/// Keyed variant for callers that know which setting owns the tooltip.
pub fn tooltip_with_key(
    title: SharedString,
    description: SharedString,
    has_wiki: bool,
    key: &str,
    window: &mut Window,
    cx: &mut App,
) -> gpui::AnyView {
    if illustration(key).is_none() {
        return tooltip(title, description, has_wiki, window, cx);
    }
    tooltip_inner(title, description, has_wiki, illustration(key), window, cx)
}

fn tooltip_inner(
    title: SharedString,
    description: SharedString,
    has_wiki: bool,
    image_path: Option<&'static str>,
    window: &mut Window,
    cx: &mut App,
) -> gpui::AnyView {
    Tooltip::element(move |_, _| {
        let z = zed();
        let mut card = div()
            .w(px(350.))
            .max_w(px(350.))
            .flex()
            .flex_col()
            .gap(px(8.))
            .py(px(6.));
        card = card.child(
            div()
                .text_sm()
                .font_weight(gpui::FontWeight::MEDIUM)
                .whitespace_normal()
                .child(title.clone()),
        );
        if let Some(path) = image_path {
            card = card.child(
                div()
                    .flex()
                    .justify_center()
                    .child(img(path).w(px(320.)).object_fit(gpui::ObjectFit::Contain)),
            );
        }
        card.child(
            div()
                .max_h(px(210.))
                .overflow_hidden()
                .text_sm()
                .whitespace_normal()
                .child(description.clone()),
        )
        .child(
            div()
                .text_xs()
                .text_color(z.text_muted)
                .child(crate::i18n::tr(if has_wiki {
                    "? — повний опис і посилання на Wiki"
                } else {
                    "? — повний опис"
                })),
        )
    })
    .max_w(px(376.))
    .build(window, cx)
}

impl Workspace {
    pub fn open_setting_help(
        &mut self,
        title: String,
        key: String,
        description: String,
        url: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_modal(
            Rc::new(move |_, window, cx| {
                let viewport_height = f32::from(window.viewport_size().height);
                let mut content = div().flex().flex_col().gap(px(12.)).whitespace_normal();
                if let Some(path) = illustration(&key) {
                    content = content.child(
                        div().flex().justify_center().child(
                            img(path)
                                .w(px(360.))
                                .max_w(px(360.))
                                .object_fit(gpui::ObjectFit::Contain),
                        ),
                    );
                }
                content = content
                    .child(
                        div()
                            .text_xs()
                            .text_color(zed().text_muted)
                            .child(key.clone()),
                    )
                    .child(
                        div()
                            .id("setting-description")
                            .text_sm()
                            .whitespace_normal()
                            .child(description.clone()),
                    );
                let mut body = div().flex().flex_col().p(px(16.)).gap(px(12.)).child(
                    div()
                        .max_h(px((viewport_height - 260.).clamp(140., 420.)))
                        .overflow_y_scrollbar()
                        .child(content),
                );
                if let Some(url) = url.as_ref().filter(|u| u.starts_with("https://")) {
                    let target = url.clone();
                    body = body.child(
                        ui::ghost_button(
                            "setting-wiki",
                            Some(IconName::ExternalLink),
                            "Відкрити Wiki ↗",
                        )
                        .on_click(move |_, _, cx| cx.open_url(&target)),
                    );
                }
                let footer = ui::ghost_button("setting-help-close", None, "Готово")
                    .on_click(cx.listener(|this, _, _, cx| this.close_modal(cx)));
                widgets::modal(title.clone(), 580., body, Some(footer.into_any_element()))
                    .into_any_element()
            }),
            window,
            cx,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::illustration;

    #[test]
    fn maps_only_proven_setting_illustrations() {
        for key in [
            "bottom_color_penetration_layers",
            "top_color_penetration_layers",
            "seam_position",
            "top_shell_layers",
        ] {
            assert!(illustration(key).is_some(), "missing mapping for {key}");
        }
        for key in ["wall_loops", "top_shell_thickness", "unknown"] {
            assert_eq!(illustration(key), None, "unexpected mapping for {key}");
        }
    }
}
