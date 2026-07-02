use std::env;
use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-changed=custom_filter/CustomVideoSource_cross.cpp");
    println!("cargo:rerun-if-changed=custom_filter/CustomVideoSource.h");

    // Build the custom DirectShow filter only for Windows *targets*. Note we check
    // CARGO_CFG_TARGET_OS (the target) rather than cfg!(target_os = ...), which in a
    // build script reflects the host and would be wrong when cross-compiling.
    if env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        build_custom_filter();
    }
}

fn build_custom_filter() {
    let manifest_dir = env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR not set");
    let source = PathBuf::from(&manifest_dir)
        .join("custom_filter")
        .join("CustomVideoSource_cross.cpp");

    // The `cc` crate selects the correct compiler for the active target
    // (cl.exe for *-pc-windows-msvc, mingw g++ for *-pc-windows-gnu), compiles the
    // C++ source, and archives it into a static library in the matching format,
    // emitting the right rustc-link-search / rustc-link-lib=static directives.
    cc::Build::new()
        .cpp(true)
        .file(&source)
        .define("_WIN32_WINNT", "0x0601")
        .define("WINVER", "0x0601")
        .compile("CustomVideoSource");

    // uuid.lib provides the standard COM GUID constants (e.g. IID_IUnknown) that the
    // filter's QueryInterface references. (The custom GUIDs are defined in the source
    // via <initguid.h>.)
    println!("cargo:rustc-link-lib=dylib=uuid");
}
