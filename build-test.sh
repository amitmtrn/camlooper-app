#!/bin/bash

echo "🔧 CamLooper Build Test Script"
echo "==============================="

# Check if we're in the right directory
if [ ! -f "src-tauri/Cargo.toml" ]; then
    echo "❌ Error: Please run this script from the project root directory"
    exit 1
fi

echo "📁 Current directory: $(pwd)"
echo "🦀 Rust version check..."

# Check Rust installation
if command -v rustc &> /dev/null; then
    echo "✅ Rust found: $(rustc --version)"
else
    echo "❌ Rust not found. Please install Rust from https://rustup.rs/"
    exit 1
fi

# Check Cargo
if command -v cargo &> /dev/null; then
    echo "✅ Cargo found: $(cargo --version)"
else
    echo "❌ Cargo not found"
    exit 1
fi

echo ""
echo "🔍 Checking platform-specific dependencies..."

case "$(uname -s)" in
    Linux*)
        echo "🐧 Linux detected"
        echo "📦 Checking for required packages..."
        
        # Check for FFmpeg development libraries
        if pkg-config --exists libavcodec libavformat libavutil; then
            echo "✅ FFmpeg development libraries found"
        else
            echo "⚠️  FFmpeg development libraries not found"
            echo "   Install with: sudo apt-get install ffmpeg libavcodec-dev libavformat-dev libavutil-dev"
        fi
        
        # Check for v4l development libraries
        if pkg-config --exists libv4l2; then
            echo "✅ v4l2 development libraries found"
        else
            echo "⚠️  v4l2 development libraries not found"
            echo "   Install with: sudo apt-get install libv4l-dev"
        fi
        ;;
    Darwin*)
        echo "🍎 macOS detected"
        echo "📦 Checking for required packages..."
        
        # Check for FFmpeg (usually installed via Homebrew)
        if pkg-config --exists libavcodec libavformat libavutil; then
            echo "✅ FFmpeg development libraries found"
        else
            echo "⚠️  FFmpeg development libraries not found"
            echo "   Install with: brew install ffmpeg"
        fi
        ;;
    MINGW*|MSYS*|CYGWIN*)
        echo "🪟 Windows detected"
        echo "📦 Windows build will use native APIs (no FFmpeg required)"
        echo "✅ Windows-specific dependencies will be handled by Cargo"
        ;;
    *)
        echo "❓ Unknown platform: $(uname -s)"
        ;;
esac

echo ""
echo "🔨 Starting Cargo check..."
cd src-tauri

# First try a simple check
echo "📋 Running cargo check..."
if cargo check; then
    echo "✅ Cargo check passed!"
else
    echo "❌ Cargo check failed"
    echo ""
    echo "🔧 Troubleshooting tips:"
    echo "  1. Make sure all system dependencies are installed"
    echo "  2. On Linux: sudo apt-get install build-essential pkg-config"
    echo "  3. On Windows: Install Visual Studio Build Tools"
    echo "  4. Try: cargo clean && cargo check"
    exit 1
fi

echo ""
echo "🎯 Attempting to build..."
if cargo build; then
    echo "✅ Build successful!"
    echo ""
    echo "🚀 Your CamLooper Tauri app should now be ready!"
    echo "   Run with: cargo tauri dev"
else
    echo "❌ Build failed"
    echo "   Check the error messages above for specific issues"
    exit 1
fi

echo ""
echo "🎉 Build test completed successfully!"
echo "   Next steps:"
echo "   1. Run 'cargo tauri dev' to start development"
echo "   2. Or run 'cargo tauri build' for production build"