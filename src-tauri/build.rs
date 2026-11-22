fn main() {
    // macOS: the virtual camera's app-side bridge (extension activation + CoreMediaIO sink)
    // is Objective-C. CARGO_CFG_TARGET_OS rather than cfg!(), because a build script is
    // compiled for the host and this must follow the *target*.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        println!("cargo:rerun-if-changed=macos/camera_bridge.m");
        cc::Build::new()
            .file("macos/camera_bridge.m")
            .flag("-fobjc-arc")
            .flag("-mmacosx-version-min=13.0")
            .compile("camlooper_camera_bridge");
        for framework in [
            "AVFoundation",
            "CoreMedia",
            "CoreMediaIO",
            "CoreVideo",
            "Foundation",
            "SystemExtensions",
        ] {
            println!("cargo:rustc-link-lib=framework={framework}");
        }

        // FFmpeg's libraries carry @rpath install names. In the bundle they sit in
        // Contents/Frameworks; in `tauri dev` and `cargo test` there is no bundle, so
        // those builds also look in the source tree the libraries were built into.
        println!("cargo:rustc-link-arg=-Wl,-rpath,@executable_path/../Frameworks");
        let bundled_ffmpeg = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("drivers/macos/ffmpeg/lib");
        println!("cargo:rerun-if-changed={}", bundled_ffmpeg.display());
        if bundled_ffmpeg.is_dir() && std::env::var("PROFILE").as_deref() != Ok("release") {
            println!("cargo:rustc-link-arg=-Wl,-rpath,{}", bundled_ffmpeg.display());
        }
    }

    // tauri_build::build() embeds the Windows exe manifest (Common-Controls v6 SxS
    // dependency, DPI awareness, icon). Without it, cross-compiled binaries fail on
    // Windows with "TaskDialogIndirect could not be located in comctl32" because lld
    // does not auto-insert the manifest that msvc link.exe would.
    tauri_build::build();
}
