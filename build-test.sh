#!/bin/bash

echo "🔧 CamLooper Build Test Script"
echo "==============================="

# Check if we're in the right directory
if [ ! -f "package.json" ]; then
    echo "❌ Error: Please run this script from the project root directory"
    exit 1
fi

echo "📁 Current directory: $(pwd)"

# Check Node.js and npm
echo "📦 Checking Node.js and npm..."
if command -v node &> /dev/null; then
    echo "✅ Node.js found: $(node --version)"
else
    echo "❌ Node.js not found. Please install Node.js from https://nodejs.org/"
    exit 1
fi

if command -v npm &> /dev/null; then
    echo "✅ npm found: $(npm --version)"
else
    echo "❌ npm not found"
    exit 1
fi

# Check Rust and Cargo
echo "🦀 Checking Rust and Cargo..."
if command -v rustc &> /dev/null; then
    echo "✅ Rust found: $(rustc --version)"
else
    echo "❌ Rust not found. Please install Rust from https://rustup.rs/"
    exit 1
fi

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
        echo "✅ Windows-specific dependencies will be handled by Tauri"
        
        # Check for Windows build tools
        if command -v cl &> /dev/null; then
            echo "✅ MSVC compiler found"
        else
            echo "⚠️  MSVC compiler not found"
            echo "   Install Visual Studio Build Tools or Visual Studio Community"
        fi
        ;;
    *)
        echo "❓ Unknown platform: $(uname -s)"
        ;;
esac

echo ""
echo "� Installing npm dependencies..."
if npm install; then
    echo "✅ npm dependencies installed"
else
    echo "❌ Failed to install npm dependencies"
    exit 1
fi

echo ""
echo "🔨 Testing Tauri development setup..."
echo "📋 Running cargo check in src-tauri..."
cd src-tauri

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

cd ..

echo ""
echo "🎯 Testing Tauri build process..."

case "$(uname -s)" in
    MINGW*|MSYS*|CYGWIN*)
        echo "🪟 Testing Windows build with npm run build:windows..."
        if npm run build:windows; then
            echo "✅ Windows build successful!"
        else
            echo "❌ Windows build failed"
            echo "   Make sure Visual Studio Build Tools are installed"
            exit 1
        fi
        ;;
    *)
        echo "🔨 Testing general Tauri build..."
        if npm run tauri:build; then
            echo "✅ Tauri build successful!"
        else
            echo "❌ Tauri build failed"
            echo "   Check the error messages above for specific issues"
            exit 1
        fi
        ;;
esac

echo ""
echo "🎉 Build test completed successfully!"
echo ""
echo "📋 Available build commands:"
echo "   Development:"
echo "   • npm run tauri:dev        - Start development server"
echo "   • npm run tauri:build:debug - Debug build"
echo ""
echo "   Production:"
echo "   • npm run tauri:build      - Standard production build"
echo "   • npm run build:windows    - Windows-specific build (MSVC)"
echo ""
echo "🚀 Your CamLooper app is ready for development!"