fn main() {
    println!("cargo:rerun-if-changed=assets/icon/kuulo.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        winresource::WindowsResource::new()
            .set_icon("assets/icon/kuulo.ico")
            .set("LegalCopyright", "Copyright © 2026 Steffan Poulsen. GPL-3.0-or-later.")
            .compile()
            .expect("the Windows resources compile");
    }
}
