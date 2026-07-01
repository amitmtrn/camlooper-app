# Cross-Compilation Guide for Custom Virtual Camera Driver

This guide explains how to cross-compile the CamLooper custom virtual camera driver for different target platforms.

## Overview

The custom virtual camera driver has been designed to support cross-compilation, allowing you to build Windows applications from Linux or macOS development environments.

## Prerequisites

### For Cross-Compilation from Linux to Windows

1. **Install MinGW-w64**:
   ```bash
   # Ubuntu/Debian
   sudo apt update
   sudo apt install mingw-w64
   
   # CentOS/RHEL/Fedora
   sudo dnf install mingw64-gcc-c++ mingw64-gcc
   
   # Arch Linux
   sudo pacman -S mingw-w64-gcc
   ```

2. **Verify Installation**:
   ```bash
   x86_64-w64-mingw32-gcc --version
   x86_64-w64-mingw32-g++ --version
   ```

### For Cross-Compilation from macOS to Windows

1. **Install MinGW-w64**:
   ```bash
   # Using Homebrew
   brew install mingw-w64
   
   # Using MacPorts
   sudo port install mingw-w64
   ```

2. **Verify Installation**:
   ```bash
   x86_64-w64-mingw32-gcc --version
   x86_64-w64-mingw32-g++ --version
   ```

## Build Process

### Automatic Cross-Compilation

The build system automatically detects cross-compilation scenarios and handles them appropriately:

```bash
# Build for Windows from Linux/macOS
cargo build --target x86_64-pc-windows-gnu

# Build for Windows from Linux/macOS (MSVC)
cargo build --target x86_64-pc-windows-msvc
```

### Manual Cross-Compilation

If you need more control over the build process:

1. **Build the Custom Filter**:
   ```bash
   cd src-tauri/custom_filter
   ./cross_build.sh windows
   ```

2. **Build the Application**:
   ```bash
   cargo build --target x86_64-pc-windows-gnu
   ```

## Build Script Details

### Cross-Compilation Build Script (`cross_build.sh`)

The build script automatically:

1. **Detects Platform**: Identifies current and target platforms
2. **Sets Up Environment**: Configures cross-compilation tools
3. **Builds Filter**: Compiles the custom DirectShow filter
4. **Handles Dependencies**: Manages Windows-specific libraries

#### Usage Examples

```bash
# Build for current platform
./cross_build.sh

# Build for Windows from Linux
./cross_build.sh windows

# Build for specific architecture
./cross_build.sh windows release
```

### Build Script Features

- **Automatic Tool Detection**: Finds appropriate cross-compilers
- **Error Handling**: Provides helpful error messages and installation instructions
- **Multiple Targets**: Supports x86_64 and i686 Windows targets
- **Fallback Options**: Gracefully handles missing tools

## Rust Build Integration

### Build Script (`build.rs`)

The Rust build script automatically:

1. **Detects Cross-Compilation**: Compares `TARGET` and `HOST` environment variables
2. **Builds Custom Filter**: Compiles the C++ filter code
3. **Links Libraries**: Properly links the custom filter with the Rust application
4. **Handles Errors**: Provides warnings when tools are missing

### Key Features

- **Automatic Detection**: No manual configuration required
- **Tool Fallbacks**: Tries multiple compilers (cl.exe, g++, cross-compilers)
- **Error Reporting**: Clear warnings about missing tools
- **Conditional Building**: Only builds on Windows targets

## Supported Targets

### Windows Targets

| Target | Architecture | Compiler | Status |
|--------|-------------|----------|---------|
| `x86_64-pc-windows-gnu` | x86_64 | MinGW-w64 | ✅ Supported |
| `x86_64-pc-windows-msvc` | x86_64 | MSVC | ✅ Supported |
| `i686-pc-windows-gnu` | i686 | MinGW-w64 | ✅ Supported |
| `i686-pc-windows-msvc` | i686 | MSVC | ✅ Supported |

### Non-Windows Targets

| Target | Status | Notes |
|--------|--------|-------|
| Linux | ✅ Supported | Uses v4l2loopback |
| macOS | ✅ Supported | Uses AVFoundation |
| Android | ❌ Not Supported | No virtual camera support |
| iOS | ❌ Not Supported | No virtual camera support |

## Troubleshooting

### Common Issues

#### 1. Cross-Compiler Not Found

**Error**: `Cross-compiler x86_64-w64-mingw32-gcc not found`

**Solution**:
```bash
# Ubuntu/Debian
sudo apt install mingw-w64

# macOS
brew install mingw-w64

# Verify installation
x86_64-w64-mingw32-gcc --version
```

#### 2. Missing Windows Headers

**Error**: `fatal error: windows.h: No such file or directory`

**Solution**: The cross-compiler includes Windows headers automatically. If this error occurs, reinstall MinGW-w64.

#### 3. Linking Errors

**Error**: `undefined reference to 'CreateCustomVideoSource'`

**Solution**: The build script should handle linking automatically. Check that the custom filter was built successfully.

#### 4. Build Script Permission Denied

**Error**: `Permission denied: ./cross_build.sh`

**Solution**:
```bash
chmod +x custom_filter/cross_build.sh
```

### Debugging Build Issues

1. **Enable Verbose Output**:
   ```bash
   cargo build --target x86_64-pc-windows-gnu -vv
   ```

2. **Check Build Script Output**:
   ```bash
   cd custom_filter
   ./cross_build.sh windows 2>&1 | tee build.log
   ```

3. **Verify Tool Availability**:
   ```bash
   which x86_64-w64-mingw32-gcc
   which x86_64-w64-mingw32-g++
   ```

## Performance Considerations

### Cross-Compilation Performance

- **Build Time**: Cross-compilation is typically slower than native builds
- **Optimization**: Use `-O2` or `-O3` for release builds
- **Parallel Builds**: The build script uses parallel compilation when available

### Runtime Performance

- **No Impact**: Cross-compiled binaries perform identically to native builds
- **Optimization**: All optimizations are preserved during cross-compilation
- **Size**: Binary size is comparable to native builds

## CI/CD Integration

### GitHub Actions Example

```yaml
name: Build Windows
on: [push, pull_request]

jobs:
  build-windows:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v3
      
      - name: Install MinGW-w64
        run: |
          sudo apt update
          sudo apt install mingw-w64
      
      - name: Build for Windows
        run: |
          cargo build --target x86_64-pc-windows-gnu --release
      
      - name: Upload Artifacts
        uses: actions/upload-artifact@v3
        with:
          name: windows-build
          path: target/x86_64-pc-windows-gnu/release/
```

### GitLab CI Example

```yaml
build-windows:
  image: ubuntu:20.04
  before_script:
    - apt update && apt install -y mingw-w64 cargo
  script:
    - cargo build --target x86_64-pc-windows-gnu --release
  artifacts:
    paths:
      - target/x86_64-pc-windows-gnu/release/
```

## Advanced Configuration

### Custom Compiler Flags

You can customize compiler flags by modifying the build script:

```bash
# In cross_build.sh, modify the compiler arguments
g++ -c -fPIC -O3 -DWIN32 -D_WIN32 -march=native ...
```

### Multiple Target Builds

Build for multiple targets simultaneously:

```bash
# Build for both 32-bit and 64-bit Windows
cargo build --target x86_64-pc-windows-gnu
cargo build --target i686-pc-windows-gnu
```

### Conditional Features

Enable/disable features based on target:

```toml
# In Cargo.toml
[target.'cfg(target_os = "windows")'.dependencies]
windows = { version = "0.58", features = ["Win32_Media_DirectShow"] }

[target.'cfg(not(target_os = "windows"))'.dependencies]
# Non-Windows dependencies
```

## Best Practices

### Development Workflow

1. **Use Native Builds for Development**: Faster iteration
2. **Use Cross-Compilation for Testing**: Verify compatibility
3. **Use CI/CD for Releases**: Automated builds for all targets

### Code Organization

1. **Platform-Specific Code**: Use `#[cfg(target_os = "windows")]`
2. **Conditional Compilation**: Handle platform differences gracefully
3. **Error Handling**: Provide clear error messages for missing tools

### Testing

1. **Test on Target Platform**: Always verify cross-compiled binaries
2. **Test Multiple Targets**: Ensure compatibility across architectures
3. **Automated Testing**: Use CI/CD for continuous verification

## Conclusion

The cross-compilation setup provides a robust solution for building Windows applications from Linux or macOS development environments. The automated build system handles most complexity, while providing clear error messages and fallback options when tools are missing.

Key benefits:
- **No Windows Development Environment Required**: Build Windows apps from Linux/macOS
- **Automated Tool Detection**: Minimal manual configuration
- **Comprehensive Error Handling**: Clear guidance for missing tools
- **CI/CD Ready**: Easy integration with automated build systems 