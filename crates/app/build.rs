//! Windows: іконка (ресурс ID 1 — його бере gpui для класу вікна й Провідник для exe)
//! і VERSIONINFO. `winresource` — build-залежність лише для Windows-хоста (MSVC `rc.exe`),
//! тож Linux/macOS-збірка її не компілює й не потребує. Крос-збірка з Linux під Windows
//! іде без ресурсів (exe без іконки, решта та сама).

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=assets/app/slicenv.ico");
    #[cfg(windows)]
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/app/slicenv.ico")
            .set("ProductName", "Slicenv")
            .set("FileDescription", "Slicenv")
            .set("CompanyName", "Borys Kharchenko")
            .set("LegalCopyright", "AGPL-3.0-or-later")
            .set("OriginalFilename", "slicenv.exe")
            .set("InternalName", "slicenv");
        res.compile()
            .expect("ресурси Windows (rc.exe з Windows SDK)");
    }
}
