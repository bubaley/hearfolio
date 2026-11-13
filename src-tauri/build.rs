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
    let mut attributes = tauri_build::Attributes::new();
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows")
        && std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc")
    {
        // tauri-build embeds the Common Controls v6 manifest only into the app binary, so `cargo test`
        // binaries that pull in dialog code fail to start (STATUS_ENTRYPOINT_NOT_FOUND). Embed it through the
        // linker instead, which covers the app and every test binary alike.
        let manifest = std::env::current_dir()
            .unwrap()
            .join("windows-app-manifest.xml");
        println!("cargo:rerun-if-changed={}", manifest.display());
        println!("cargo:rustc-link-arg=/MANIFEST:EMBED");
        println!("cargo:rustc-link-arg=/MANIFESTINPUT:{}", manifest.display());
        attributes = attributes
            .windows_attributes(tauri_build::WindowsAttributes::new_without_app_manifest());
    }
    tauri_build::try_build(attributes).expect("failed to run tauri-build");
}
