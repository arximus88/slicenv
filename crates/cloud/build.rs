//! Шим до закритого `bambu_networking` збирається лише там, де ми перевірили ABI плагіна:
//! - Linux x86_64: libstdc++, `_GLIBCXX_USE_CXX11_ABI=1`;
//! - Windows x86_64 MSVC: MSVC STL, динамічний CRT (/MD, release-ітератори), як у плагіна
//!   (імпортує MSVCP140/VCRUNTIME140). Рядки, створені плагіном, звільняє шим і навпаки, тож
//!   купа мусить бути спільною (ucrtbase) — зі статичним CRT шим не збирається.
//!
//! Шим передає `std::string`/`std::function`/`std::map` через межу бібліотеки, тож на macOS
//! (libc++) потрібна окрема перевірена версія — див. docs/PORTING.md. Без шима крейт
//! компілюється, а хмара повідомляє «не підтримується на цій ОС» (`cfg(bambu_plugin)` вимкнено).
fn main() {
    println!("cargo:rustc-check-cfg=cfg(bambu_plugin)");
    println!("cargo:rerun-if-changed=native");
    let var = |name| std::env::var(name).unwrap_or_default();
    let os = var("CARGO_CFG_TARGET_OS");
    let arch = var("CARGO_CFG_TARGET_ARCH");
    let env = var("CARGO_CFG_TARGET_ENV");
    let crt_static = var("CARGO_CFG_TARGET_FEATURE")
        .split(',')
        .any(|f| f == "crt-static");
    let mut build = cc::Build::new();
    build.cpp(true).std("c++17");
    match (os.as_str(), arch.as_str(), env.as_str()) {
        ("linux", "x86_64", _) => {
            build
                .cpp_link_stdlib("stdc++")
                .define("_GLIBCXX_USE_CXX11_ABI", "1");
            println!("cargo:rustc-link-lib=dl");
        }
        ("windows", "x86_64", "msvc") if !crt_static => {
            // /MD навіть у debug-профілі: /MDd змінює макет контейнерів (_ITERATOR_DEBUG_LEVEL).
            build
                .static_crt(false)
                .debug(false)
                .flag("/EHsc")
                .flag("/utf-8")
                .define("_ITERATOR_DEBUG_LEVEL", "0");
        }
        _ => return,
    }
    println!("cargo:rustc-cfg=bambu_plugin");
    build
        .file("native/shim.cpp")
        .file("native/camera.cpp")
        .compile("bambu_cloud_shim");
}
