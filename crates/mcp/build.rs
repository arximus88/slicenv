//! Windows: та сама іконка й VERSIONINFO, що в slicenv.exe (див. crates/app/build.rs).

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=../app/assets/app/slicenv.ico");
    #[cfg(windows)]
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("../app/assets/app/slicenv.ico")
            .set("ProductName", "Slicenv")
            .set("FileDescription", "Slicenv MCP server")
            .set("CompanyName", "Borys Kharchenko")
            .set("LegalCopyright", "AGPL-3.0-or-later")
            .set("OriginalFilename", "slicenv-mcp.exe")
            .set("InternalName", "slicenv-mcp");
        res.compile()
            .expect("ресурси Windows (rc.exe з Windows SDK)");
    }
}
