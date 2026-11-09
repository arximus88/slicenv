//! Bundled engine, Orca pattern, and section artwork.
use gpui::{AssetSource, SharedString};
use std::borrow::Cow;
pub struct Assets;
impl AssetSource for Assets {
    fn load(&self, path: &str) -> gpui::Result<Option<Cow<'static, [u8]>>> {
        match path {
            "help/bottom_color_penetration_layers.png" => Ok(Some(Cow::Borrowed(include_bytes!(
                "../assets/help/bottom_color_penetration_layers.png"
            )))),
            "help/bottom_color_penetration_layers_dark.png" => Ok(Some(Cow::Borrowed(
                include_bytes!("../assets/help/bottom_color_penetration_layers_dark.png"),
            ))),
            "help/top_color_penetration_layers.png" => Ok(Some(Cow::Borrowed(include_bytes!(
                "../assets/help/top_color_penetration_layers.png"
            )))),
            "help/top_color_penetration_layers_dark.png" => Ok(Some(Cow::Borrowed(
                include_bytes!("../assets/help/top_color_penetration_layers_dark.png"),
            ))),
            "help/seam_position.png" => Ok(Some(Cow::Borrowed(include_bytes!(
                "../assets/help/seam_position.png"
            )))),
            "help/seam_position_dark.png" => Ok(Some(Cow::Borrowed(include_bytes!(
                "../assets/help/seam_position_dark.png"
            )))),
            "help/top_shell_layers.png" => Ok(Some(Cow::Borrowed(include_bytes!(
                "../assets/help/top_shell_layers.png"
            )))),
            "help/top_shell_layers_dark.png" => Ok(Some(Cow::Borrowed(include_bytes!(
                "../assets/help/top_shell_layers_dark.png"
            )))),
            "app/slicenv.svg" => Ok(Some(Cow::Borrowed(include_bytes!(
                "../assets/app/slicenv.svg"
            )))),
            "engines/bambu.png" => Ok(Some(Cow::Borrowed(include_bytes!(
                "../assets/engines/bambu.png"
            )))),
            "engines/orca.png" => Ok(Some(Cow::Borrowed(include_bytes!(
                "../assets/engines/orca.png"
            )))),
            _ => match crate::pattern_icons::asset(path) {
                Some(bytes) => Ok(Some(Cow::Borrowed(bytes))),
                None => gpui_kit_assets::AllAssets::new("").load(path),
            },
        }
    }
    fn list(&self, path: &str) -> gpui::Result<Vec<SharedString>> {
        gpui_kit_assets::AllAssets::new("").list(path)
    }
}
