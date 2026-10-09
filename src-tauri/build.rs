fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        cc::Build::new()
            .file("native/microphone_permission.m")
            .flag("-fobjc-arc")
            .compile("hearfolio_microphone_permission");
        println!("cargo:rustc-link-lib=framework=AVFoundation");
        println!("cargo:rustc-link-lib=framework=Foundation");
        println!("cargo:rerun-if-changed=native/microphone_permission.m");
    }
    tauri_build::build()
}
