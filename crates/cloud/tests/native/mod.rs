//! Збірка фейкових бібліотек плагіна тим самим компілятором і C++-рантаймом, що й шим:
//! GCC/libstdc++ на Linux, MSVC (/MD) на Windows. Інакше тест перевіряв би не той ABI.
use std::{
    path::{Path, PathBuf},
    process::Command,
};

/// Фейкова бібліотека з `tests/<source>`; `defines` — у вигляді `-DNAME` / `-DNAME=value`.
pub fn shared_library(source: &str, out: &Path, defines: &[String]) -> PathBuf {
    let source = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join(source);
    let status = compiler(&source, out, defines).status().unwrap();
    assert!(status.success(), "не зібралася {}", source.display());
    out.to_owned()
}

/// Ім'я файлу бібліотеки на цій ОС: `known` → `known.so` / `known.dll`.
pub fn library_name(stem: &str) -> String {
    format!("{stem}{}", std::env::consts::DLL_SUFFIX)
}

#[cfg(target_os = "linux")]
fn compiler(source: &Path, out: &Path, defines: &[String]) -> Command {
    let mut command = Command::new("c++");
    command
        .args([
            "-std=c++17",
            "-shared",
            "-fPIC",
            "-pthread",
            "-D_GLIBCXX_USE_CXX11_ABI=1",
        ])
        .args(defines)
        .arg(source)
        .arg("-o")
        .arg(out);
    command
}

#[cfg(windows)]
fn compiler(source: &Path, out: &Path, defines: &[String]) -> Command {
    let cl = cc::windows_registry::find_tool("x86_64-pc-windows-msvc", "cl.exe")
        .expect("MSVC cl.exe (Visual Studio Build Tools)");
    let mut command = cl.to_command();
    // Проміжні .obj/.lib/.exp — поруч із бібліотекою в тимчасовій теці тесту.
    let mut objects = out.parent().unwrap().as_os_str().to_owned();
    objects.push("\\");
    command
        .args([
            "/nologo",
            "/std:c++17",
            "/EHsc",
            "/MD",
            "/O2",
            "/utf-8",
            "/LD",
        ])
        .args(defines)
        .arg(source)
        .arg({
            let mut fo = std::ffi::OsString::from("/Fo");
            fo.push(&objects);
            fo
        })
        .arg({
            let mut fe = std::ffi::OsString::from("/Fe");
            fe.push(out);
            fe
        });
    command
}
