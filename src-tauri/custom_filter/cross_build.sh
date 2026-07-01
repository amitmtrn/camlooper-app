#!/bin/bash

# Cross-compilation build script for CamLooper Custom DirectShow Filter
# This script handles building the custom filter for different target platforms

set -e

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

# Function to print colored output
print_status() {
    echo -e "${BLUE}[INFO]${NC} $1"
}

print_success() {
    echo -e "${GREEN}[SUCCESS]${NC} $1"
}

print_warning() {
    echo -e "${YELLOW}[WARNING]${NC} $1"
}

print_error() {
    echo -e "${RED}[ERROR]${NC} $1"
}

# Detect current platform
detect_platform() {
    case "$(uname -s)" in
        Linux*)     echo "linux";;
        Darwin*)    echo "macos";;
        CYGWIN*)    echo "windows";;
        MINGW*)     echo "windows";;
        MSYS*)      echo "windows";;
        *)          echo "unknown";;
    esac
}

# Check if we're cross-compiling
is_cross_compile() {
    if [ "$TARGET_PLATFORM" != "$(detect_platform)" ]; then
        return 0
    else
        return 1
    fi
}

# Setup Windows cross-compilation environment
setup_windows_cross_compile() {
    print_status "Setting up Windows cross-compilation environment..."
    
    # Check if we have the necessary tools
    if ! command -v x86_64-w64-mingw32-gcc &> /dev/null; then
        print_error "x86_64-w64-mingw32-gcc not found. Please install MinGW-w64 for cross-compilation."
        print_status "On Ubuntu/Debian: sudo apt install mingw-w64"
        print_status "On macOS: brew install mingw-w64"
        exit 1
    fi
    
    # Set up environment variables for cross-compilation
    export CC=x86_64-w64-mingw32-gcc
    export CXX=x86_64-w64-mingw32-g++
    export AR=x86_64-w64-mingw32-ar
    export STRIP=x86_64-w64-mingw32-strip
    export WINDRES=x86_64-w64-mingw32-windres
    
    print_success "Windows cross-compilation environment setup complete"
}

# Build the custom filter
build_filter() {
    local platform=$1
    local build_dir="build_${platform}"
    
    print_status "Building custom filter for $platform..."
    
    # Create build directory
    mkdir -p "$build_dir"
    cd "$build_dir"
    
    case "$platform" in
        "windows")
            if is_cross_compile; then
                # Cross-compile from Linux/macOS to Windows
                build_windows_cross_compile
            else
                # Native Windows build
                build_windows_native
            fi
            ;;
        "linux")
            print_warning "DirectShow filters are Windows-specific. Building placeholder for Linux."
            build_linux_placeholder
            ;;
        "macos")
            print_warning "DirectShow filters are Windows-specific. Building placeholder for macOS."
            build_macos_placeholder
            ;;
        *)
            print_error "Unsupported platform: $platform"
            exit 1
            ;;
    esac
    
    cd ..
    print_success "Build completed for $platform"
}

# Build Windows filter with cross-compilation
build_windows_cross_compile() {
    print_status "Cross-compiling Windows DirectShow filter..."
    
    # Create a simplified cross-compilation build
    cat > CMakeLists.txt << 'EOF'
cmake_minimum_required(VERSION 3.16)
project(CustomVideoSource)

# Set cross-compilation
set(CMAKE_SYSTEM_NAME Windows)
set(CMAKE_C_COMPILER x86_64-w64-mingw32-gcc)
set(CMAKE_CXX_COMPILER x86_64-w64-mingw32-g++)
set(CMAKE_RC_COMPILER x86_64-w64-mingw32-windres)

# Set output directories
set(CMAKE_RUNTIME_OUTPUT_DIRECTORY ${CMAKE_BINARY_DIR}/bin)
set(CMAKE_LIBRARY_OUTPUT_DIRECTORY ${CMAKE_BINARY_DIR}/lib)

# Create a minimal stub DLL for cross-compilation
add_library(CustomVideoSource SHARED
    ../CustomVideoSource_cross.cpp
)

# Set target properties
set_target_properties(CustomVideoSource PROPERTIES
    OUTPUT_NAME "CustomVideoSource"
    PREFIX ""
    SUFFIX ".ax"
)

# Link minimal Windows libraries
target_link_libraries(CustomVideoSource
    ole32
    oleaut32
    uuid
)
EOF

    # Configure and build
    cmake .. -DCMAKE_BUILD_TYPE=Release
    make -j$(nproc)
    
    print_success "Cross-compiled Windows filter created"
}

# Build Windows filter natively
build_windows_native() {
    print_status "Building Windows DirectShow filter natively..."
    
    # Use the original CMakeLists.txt
    cmake .. -G "Visual Studio 16 2019" -A Win32
    cmake --build . --config Release
    
    print_success "Native Windows filter built"
}

# Build Linux placeholder
build_linux_placeholder() {
    print_status "Creating Linux placeholder..."
    
    # Create a simple placeholder library
    cat > placeholder.cpp << 'EOF'
#include <iostream>

extern "C" {
    __attribute__((visibility("default")))
    void camlooper_placeholder() {
        std::cout << "CamLooper Custom Filter - Linux Placeholder" << std::endl;
    }
}
EOF

    g++ -shared -fPIC -o libCustomVideoSource.so placeholder.cpp
    
    print_success "Linux placeholder created"
}

# Build macOS placeholder
build_macos_placeholder() {
    print_status "Creating macOS placeholder..."
    
    # Create a simple placeholder library
    cat > placeholder.cpp << 'EOF'
#include <iostream>

extern "C" {
    __attribute__((visibility("default")))
    void camlooper_placeholder() {
        std::cout << "CamLooper Custom Filter - macOS Placeholder" << std::endl;
    }
}
EOF

    clang++ -shared -fPIC -o libCustomVideoSource.dylib placeholder.cpp
    
    print_success "macOS placeholder created"
}

# Main build function
main() {
    print_status "CamLooper Custom Filter Cross-Compilation Build Script"
    print_status "====================================================="
    
    # Parse command line arguments
    TARGET_PLATFORM=${1:-$(detect_platform)}
    BUILD_TYPE=${2:-"release"}
    
    print_status "Target platform: $TARGET_PLATFORM"
    print_status "Build type: $BUILD_TYPE"
    print_status "Current platform: $(detect_platform)"
    
    # Setup cross-compilation if needed
    if [ "$TARGET_PLATFORM" = "windows" ] && [ "$(detect_platform)" != "windows" ]; then
        setup_windows_cross_compile
    fi
    
    # Build the filter
    build_filter "$TARGET_PLATFORM"
    
    print_success "Build process completed successfully!"
    
    # Show output files
    print_status "Build outputs:"
    find . -name "*.ax" -o -name "*.so" -o -name "*.dylib" | while read file; do
        echo "  - $file"
    done
}

# Run main function with all arguments
main "$@" 