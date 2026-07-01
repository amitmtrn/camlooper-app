use std::env;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=custom_filter/CustomVideoSource_cross.cpp");
    println!("cargo:rerun-if-changed=custom_filter/CustomVideoSource.h");
    
    // Only build the custom filter on Windows targets
    if cfg!(target_os = "windows") {
        build_custom_filter();
    }
}

fn build_custom_filter() {
    let out_dir = env::var("OUT_DIR").expect("OUT_DIR not set");
    let manifest_dir = env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR not set");
    
    // Determine if we're cross-compiling
    let target = env::var("TARGET").expect("TARGET not set");
    let host = env::var("HOST").expect("HOST not set");
    let is_cross_compile = target != host;
    
    println!("cargo:warning=Building custom filter for target: {}", target);
    println!("cargo:warning=Host: {}", host);
    println!("cargo:warning=Cross-compiling: {}", is_cross_compile);
    
    let filter_dir = PathBuf::from(manifest_dir).join("custom_filter");
    let build_dir = PathBuf::from(&out_dir).join("custom_filter_build");
    
    // Create build directory
    std::fs::create_dir_all(&build_dir).expect("Failed to create build directory");
    
    if is_cross_compile {
        build_cross_compiled_filter(&filter_dir, &build_dir, &target);
    } else {
        build_native_filter(&filter_dir, &build_dir);
    }
    
    // Link the library
    let lib_path = build_dir.join("libCustomVideoSource.a");
    if lib_path.exists() {
        println!("cargo:rustc-link-search=native={}", build_dir.display());
        println!("cargo:rustc-link-lib=static=CustomVideoSource");
    } else {
        println!("cargo:warning=Custom filter library not found at {}", lib_path.display());
    }
}

fn build_cross_compiled_filter(filter_dir: &PathBuf, build_dir: &PathBuf, target: &str) {
    println!("cargo:warning=Cross-compiling custom filter for {}", target);
    
    // Check if we have the cross-compilation tools
    let cc = if target.contains("x86_64") {
        "x86_64-w64-mingw32-gcc"
    } else if target.contains("i686") {
        "i686-w64-mingw32-gcc"
    } else {
        println!("cargo:warning=Unsupported target for cross-compilation: {}", target);
        return;
    };
    
    let cxx = cc.replace("gcc", "g++");
    
    // Check if cross-compiler is available
    if Command::new(cc).arg("--version").output().is_err() {
        println!("cargo:warning=Cross-compiler {} not found. Skipping custom filter build.", cc);
        println!("cargo:warning=Install with: sudo apt install mingw-w64 (Ubuntu/Debian) or brew install mingw-w64 (macOS)");
        return;
    }
    
    // Build the filter
    let source_file = filter_dir.join("CustomVideoSource_cross.cpp");
    let output_file = build_dir.join("libCustomVideoSource.a");
    
    let status = Command::new(cxx)
        .args(&[
            "-c",
            "-fPIC",
            "-O2",
            "-DWIN32",
            "-D_WIN32",
            "-D_WIN32_WINNT=0x0601",
            "-DWINVER=0x0601",
            "-shared",
            "-o", &output_file.to_string_lossy(),
            &source_file.to_string_lossy(),
        ])
        .current_dir(build_dir)
        .status();
    
    match status {
        Ok(exit_status) => {
            if exit_status.success() {
                println!("cargo:warning=Cross-compiled custom filter successfully");
            } else {
                println!("cargo:warning=Cross-compilation failed with exit code: {}", exit_status);
            }
        }
        Err(e) => {
            println!("cargo:warning=Failed to run cross-compiler: {}", e);
        }
    }
}

fn build_native_filter(filter_dir: &PathBuf, build_dir: &PathBuf) {
    println!("cargo:warning=Building native custom filter");
    
    // For native Windows builds, we'll create a simple stub
    // In a real implementation, this would use the full DirectShow SDK
    let source_file = filter_dir.join("CustomVideoSource_cross.cpp");
    let output_file = build_dir.join("libCustomVideoSource.a");
    
    // Try to use cl.exe (Visual Studio) if available
    let status = Command::new("cl.exe")
        .args(&[
            "/c",
            "/O2",
            "/DWIN32",
            "/D_WIN32",
            "/D_WIN32_WINNT=0x0601",
            "/DWINVER=0x0601",
            "/LD",
            &format!("/Fe{}", output_file.to_string_lossy()),
            &source_file.to_string_lossy(),
        ])
        .current_dir(build_dir)
        .status();
    
    match status {
        Ok(exit_status) => {
            if exit_status.success() {
                println!("cargo:warning=Native custom filter built successfully with cl.exe");
                return;
            }
        }
        Err(_) => {
            // cl.exe not found, try gcc/g++
        }
    }
    
    // Fallback to gcc/g++ if cl.exe is not available
    let status = Command::new("g++")
        .args(&[
            "-c",
            "-fPIC",
            "-O2",
            "-DWIN32",
            "-D_WIN32",
            "-D_WIN32_WINNT=0x0601",
            "-DWINVER=0x0601",
            "-shared",
            "-o", &output_file.to_string_lossy(),
            &source_file.to_string_lossy(),
        ])
        .current_dir(build_dir)
        .status();
    
    match status {
        Ok(exit_status) => {
            if exit_status.success() {
                println!("cargo:warning=Native custom filter built successfully with g++");
            } else {
                println!("cargo:warning=Native build failed with exit code: {}", exit_status);
            }
        }
        Err(e) => {
            println!("cargo:warning=Failed to run native compiler: {}", e);
        }
    }
}
