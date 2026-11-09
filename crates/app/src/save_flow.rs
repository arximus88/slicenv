//! Що робить «Зберегти проєкт»: питає шлях чи мовчки пише туди, куди людина вже погодилась.
//!
//! Перше збереження відкритого проєкту завжди відкриває діалог: для безіменного — у теці
//! бібліотеки, для 3MF — з підставленим самим файлом (свідомий перезапис чи інше місце), для
//! STL/OBJ/STEP/G-code — з `<назва>.3mf` поруч. Після вдалого збереження через діалог шлях
//! стає підтвердженим, і Ctrl+S далі пише туди без питань. «Зберегти як…» питає завжди.
//! Логіка без GPUI, щоб її можна було перевірити тестами.
use std::path::{Path, PathBuf};

/// Як виконати збереження.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SaveAction {
    /// Шлях уже підтверджено: писати без діалогу.
    Silent(PathBuf),
    /// Відкрити діалог у `dir` із запропонованим ім’ям `name`.
    Dialog { dir: PathBuf, name: String },
}

/// Вибір між діалогом і тихим збереженням. `path` — відкритий файл (для безіменного — лише
/// запропоноване ім’я), `confirmed` — шлях, який людина вже підтвердила для цього проєкту.
/// `None` — немає що зберігати.
pub fn save_action(
    path: Option<&Path>,
    confirmed: Option<&Path>,
    library_dir: &Path,
    force_dialog: bool,
) -> Option<SaveAction> {
    let path = path?;
    if !force_dialog
        && let Some(confirmed) = confirmed.filter(|p| is_3mf(p))
    {
        return Some(SaveAction::Silent(confirmed.to_path_buf()));
    }
    // «Зберегти як…» після підтвердження пропонує останнє місце збереження, а не вихідну модель.
    let base = confirmed.filter(|p| is_3mf(p)).unwrap_or(path);
    let dir = base
        .parent()
        .filter(|d| !d.as_os_str().is_empty())
        .map(Path::to_path_buf)
        .unwrap_or_else(|| library_dir.to_path_buf());
    let stem = base.file_stem().unwrap_or_default().to_string_lossy();
    Some(SaveAction::Dialog { dir, name: format!("{stem}.3mf") })
}

/// Шлях із діалогу → файл, у який справді пишемо: завжди `.3mf`. `модель.stl` стає
/// `модель.3mf`, тож вихідна модель ніколи не перезаписується 3MF-ом.
pub fn normalize_target(mut target: PathBuf) -> PathBuf {
    if !is_3mf(&target) {
        target.set_extension("3mf");
    }
    target
}

fn is_3mf(path: &Path) -> bool {
    path.extension().is_some_and(|e| e.eq_ignore_ascii_case("3mf"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lib() -> PathBuf {
        PathBuf::from("/lib")
    }

    #[test]
    fn nothing_to_save_without_path() {
        assert_eq!(save_action(None, None, &lib(), false), None);
    }

    #[test]
    fn untitled_project_asks_in_library() {
        let path = PathBuf::from("/lib/Без назви.3mf");
        assert_eq!(
            save_action(Some(&path), None, &lib(), false),
            Some(SaveAction::Dialog { dir: lib(), name: "Без назви.3mf".into() })
        );
    }

    #[test]
    fn opened_3mf_asks_with_same_file_first_time() {
        let path = PathBuf::from("/m/box.3mf");
        assert_eq!(
            save_action(Some(&path), None, &lib(), false),
            Some(SaveAction::Dialog { dir: "/m".into(), name: "box.3mf".into() })
        );
    }

    #[test]
    fn opened_stl_suggests_3mf_next_to_it() {
        for model in ["/m/part.stl", "/m/part.STL", "/m/part.obj", "/m/part.step", "/m/part.gcode"] {
            let path = PathBuf::from(model);
            assert_eq!(
                save_action(Some(&path), None, &lib(), false),
                Some(SaveAction::Dialog { dir: "/m".into(), name: "part.3mf".into() }),
                "{model}"
            );
        }
    }

    #[test]
    fn confirmed_path_saves_silently() {
        let path = PathBuf::from("/m/part.stl");
        let confirmed = PathBuf::from("/out/part.3mf");
        assert_eq!(
            save_action(Some(&path), Some(&confirmed), &lib(), false),
            Some(SaveAction::Silent(confirmed))
        );
    }

    #[test]
    fn save_as_always_asks_starting_from_confirmed() {
        let path = PathBuf::from("/m/part.stl");
        let confirmed = PathBuf::from("/out/renamed.3mf");
        assert_eq!(
            save_action(Some(&path), Some(&confirmed), &lib(), true),
            Some(SaveAction::Dialog { dir: "/out".into(), name: "renamed.3mf".into() })
        );
    }

    #[test]
    fn never_silently_writes_non_3mf() {
        // Підтвердженим може бути лише 3MF; інакше — знову діалог.
        let path = PathBuf::from("/m/part.stl");
        assert_eq!(
            save_action(Some(&path), Some(&path), &lib(), false),
            Some(SaveAction::Dialog { dir: "/m".into(), name: "part.3mf".into() })
        );
    }

    #[test]
    fn stl_target_is_not_overwritten() {
        assert_eq!(normalize_target("/m/part.stl".into()), PathBuf::from("/m/part.3mf"));
        assert_eq!(normalize_target("/m/part.STL".into()), PathBuf::from("/m/part.3mf"));
        assert_eq!(normalize_target("/m/part".into()), PathBuf::from("/m/part.3mf"));
        assert_eq!(normalize_target("/m/part.3MF".into()), PathBuf::from("/m/part.3MF"));
    }
}
