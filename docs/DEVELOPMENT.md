# CamLooper Development Guide

## Table of Contents
- [Development Environment Setup](#development-environment-setup)
- [Project Structure](#project-structure)
- [Building and Running](#building-and-running)
- [Development Workflow](#development-workflow)
- [Testing](#testing)
- [Debugging](#debugging)
- [Performance Profiling](#performance-profiling)
- [Platform-Specific Development](#platform-specific-development)
- [Contributing Guidelines](#contributing-guidelines)
- [Release Process](#release-process)

## Development Environment Setup

### Prerequisites

#### Required Tools
- **Node.js** 18+ with npm
- **Rust** 1.70+ with Cargo
- **Git** for version control
- **FFmpeg** development libraries

#### Platform-Specific Requirements

**Windows:**
- Visual Studio Build Tools 2019+
- Windows SDK 10.0.19041+
- DirectShow SDK (for virtual camera)

**macOS:**
- Xcode Command Line Tools
- macOS SDK 10.15+
- Core Foundation framework

**Linux:**
- Build essentials (`build-essential` on Ubuntu)
- V4L2 development headers
- ALSA development libraries

### Initial Setup

#### 1. Clone Repository
```bash
git clone https://github.com/your-org/camlooper.git
cd camlooper
```

#### 2. Install Dependencies
```bash
# Frontend dependencies
npm install

# Install Tauri CLI
npm install --save-dev @tauri-apps/cli

# Rust dependencies (handled automatically by Cargo)
```

#### 3. Install FFmpeg Development Libraries

**Windows:**
- Download FFmpeg development libraries
- Extract to `C:\ffmpeg`
- Add to system PATH

**macOS:**
```bash
brew install ffmpeg pkg-config
```

**Linux (Ubuntu/Debian):**
```bash
sudo apt update
sudo apt install ffmpeg libavcodec-dev libavformat-dev libavutil-dev \
                 libswscale-dev libswresample-dev pkg-config \
                 libv4l-dev v4l2loopback-dkms
```

#### 4. Environment Variables
Create a `.env` file in the project root:
```bash
# Development environment
TAURI_DEBUG=true
RUST_LOG=debug
RUST_BACKTRACE=1

# FFmpeg paths (if custom installation)
FFMPEG_PKG_CONFIG_PATH=/usr/local/lib/pkgconfig
```

## Project Structure

```
camlooper/
├── src/                          # Frontend source (React/TypeScript)
│   ├── components/              # React components
│   │   ├── ui/                 # shadcn/ui components
│   │   ├── Features.tsx        # Feature showcase
│   │   ├── Hero.tsx           # Landing page hero
│   │   └── ...                # Other components
│   ├── hooks/                  # Custom React hooks
│   ├── lib/                    # Utility libraries
│   │   ├── utils.ts           # General utilities
│   │   └── stream-upload.ts   # Upload helpers
│   ├── App.tsx                # Main application component
│   └── main.tsx               # Application entry point
├── src-tauri/                   # Backend source (Rust)
│   ├── src/
│   │   ├── lib.rs             # Main library and Tauri commands
│   │   ├── main.rs            # Application entry point
│   │   ├── video_processor.rs # Core video processing
│   │   ├── video_upload.rs    # File upload handling
│   │   └── virtual_camera.rs  # Virtual camera implementation
│   ├── Cargo.toml             # Rust dependencies
│   ├── tauri.conf.json        # Tauri configuration
│   └── build.rs               # Build script
├── docs/                        # Documentation
├── public/                      # Static assets
├── package.json                # Node.js dependencies
└── README.md                   # Project readme
```

### Key Files and Their Purposes

#### Frontend
- **`App.tsx`**: Main application logic and state management
- **`lib/stream-upload.ts`**: Efficient file upload utilities
- **`components/ui/`**: Reusable UI components from shadcn/ui

#### Backend
- **`lib.rs`**: Tauri command definitions and application setup
- **`video_processor.rs`**: FFmpeg integration and video processing
- **`video_upload.rs`**: File upload and session management
- **`virtual_camera.rs`**: Platform-specific virtual camera implementation

## Building and Running

### Development Mode
```bash
# Start development server with hot reload
npm run tauri:dev

# Alternative: separate frontend and backend
npm run dev        # Frontend only (port 1420)
cargo tauri dev    # Full application
```

### Production Build
```bash
# Build for current platform
npm run tauri:build

# Build for specific platforms
npm run build:windows      # Windows x64
npm run build:windows:32   # Windows x86
npm run build:windows:arm  # Windows ARM64
```

### Build Targets
The project supports multiple build targets configured in `package.json`:

```json
{
  "scripts": {
    "tauri:build": "tauri build",
    "build:windows": "tauri build --target x86_64-pc-windows-gnu",
    "build:windows:msvc": "tauri build --target x86_64-pc-windows-msvc",
    "build:macos": "tauri build --target x86_64-apple-darwin",
    "build:linux": "tauri build --target x86_64-unknown-linux-gnu"
  }
}
```

## Development Workflow

### Code Organization

#### Frontend Development
```typescript
// Use TypeScript for all new code
interface VideoState {
  isPlaying: boolean;
  currentFrame: VideoFrame | null;
  // ... other properties
}

// Follow React best practices
const VideoPlayer: React.FC<VideoPlayerProps> = ({ videoInfo }) => {
  const [state, setState] = useState<VideoState>({
    isPlaying: false,
    currentFrame: null
  });
  
  // Use custom hooks for complex logic
  const { uploadVideo, isUploading } = useVideoUpload();
  
  return (
    // JSX implementation
  );
};
```

#### Backend Development
```rust
// Use proper error handling
use anyhow::{anyhow, Result};

#[tauri::command]
async fn process_video(file_path: String) -> Result<VideoInfo, String> {
    let video_info = video_processor::load_video_file(file_path)
        .await
        .map_err(|e| e.to_string())?;
    
    Ok(video_info)
}

// Follow Rust conventions
pub struct VideoProcessor {
    // Use Arc<Mutex<T>> for shared state
    status: Arc<RwLock<StreamStatus>>,
    // Use channels for communication
    frame_sender: Option<mpsc::UnboundedSender<VideoFrame>>,
}
```

### Git Workflow

#### Branch Strategy
```bash
# Main branches
main          # Production-ready code
develop       # Integration branch

# Feature branches
feature/video-filters
feature/audio-support
bugfix/memory-leak
hotfix/critical-security
```

#### Commit Messages
Follow conventional commits:
```
feat(video): add H.265 codec support
fix(upload): resolve memory leak in large file uploads
docs(api): update virtual camera documentation
refactor(ui): improve component structure
test(e2e): add virtual camera integration tests
```

### Code Quality

#### Linting and Formatting
```bash
# Frontend
npm run lint          # ESLint
npm run lint:fix      # Auto-fix issues

# Backend
cargo fmt             # Format Rust code
cargo clippy          # Rust linter
cargo clippy -- -D warnings  # Strict mode
```

#### Type Safety
```typescript
// Use strict TypeScript configuration
// Enable all strict mode options in tsconfig.json
{
  "compilerOptions": {
    "strict": true,
    "noImplicitAny": true,
    "strictNullChecks": true
  }
}
```

## Testing

### Frontend Testing
```bash
# Install testing dependencies
npm install --save-dev @testing-library/react @testing-library/jest-dom vitest

# Run tests
npm run test
npm run test:watch    # Watch mode
npm run test:coverage # Coverage report
```

#### Example Test
```typescript
import { render, screen } from '@testing-library/react';
import { VideoPlayer } from '../components/VideoPlayer';

describe('VideoPlayer', () => {
  it('should display video information', () => {
    const mockVideoInfo = {
      filename: 'test.mp4',
      duration: 120,
      width: 1920,
      height: 1080
    };
    
    render(<VideoPlayer videoInfo={mockVideoInfo} />);
    
    expect(screen.getByText('test.mp4')).toBeInTheDocument();
    expect(screen.getByText('1920x1080')).toBeInTheDocument();
  });
});
```

### Backend Testing
```bash
# Run Rust tests
cargo test
cargo test -- --nocapture  # Show println! output
cargo test video_processor  # Test specific module
```

#### Example Test
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[tokio::test]
    async fn test_video_loading() {
        let mut processor = VideoProcessor::new();
        let test_video = PathBuf::from("tests/fixtures/sample.mp4");
        
        let result = processor.load_video(test_video).await;
        assert!(result.is_ok());
        
        let video_info = result.unwrap();
        assert_eq!(video_info.width, 1920);
        assert_eq!(video_info.height, 1080);
    }
}
```

### Integration Testing
```bash
# End-to-end testing with Tauri
npm install --save-dev @tauri-apps/api-test

# Run integration tests
npm run test:integration
```

## Debugging

### Frontend Debugging
```bash
# Enable debug mode
export TAURI_DEBUG=true
npm run tauri:dev

# Browser developer tools
# React Developer Tools
# Redux DevTools (if using Redux)
```

### Backend Debugging
```bash
# Enable Rust logging
export RUST_LOG=debug
export RUST_BACKTRACE=1

# Use rust-gdb for native debugging
rust-gdb target/debug/camlooper

# Logging in Rust code
use log::{debug, info, warn, error};

fn process_frame() {
    debug!("Processing frame: {}", frame_id);
    info!("Frame processed successfully");
    warn!("Buffer getting full");
    error!("Failed to process frame: {}", error);
}
```

### Performance Debugging
```rust
// Use timing measurements
use std::time::Instant;

let start = Instant::now();
process_video_frame().await?;
let duration = start.elapsed();
println!("Frame processing took: {:?}", duration);

// Memory profiling
use std::alloc::{GlobalAlloc, Layout, System};

struct TrackingAllocator;

unsafe impl GlobalAlloc for TrackingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let ptr = System.alloc(layout);
        println!("Allocated {} bytes", layout.size());
        ptr
    }
    
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        println!("Deallocated {} bytes", layout.size());
        System.dealloc(ptr, layout);
    }
}
```

## Performance Profiling

### CPU Profiling
```bash
# Install profiling tools
cargo install flamegraph
cargo install perf

# Generate flame graph
cargo flamegraph --bin camlooper

# Profile with perf
perf record --call-graph=dwarf target/release/camlooper
perf report
```

### Memory Profiling
```bash
# Install valgrind (Linux)
valgrind --tool=memcheck --leak-check=full target/debug/camlooper

# Use heaptrack (Linux)
heaptrack target/debug/camlooper
heaptrack_gui heaptrack.camlooper.*.gz
```

### GPU Profiling
```bash
# NVIDIA GPUs
nvidia-smi dmon -s pucvmet -d 1

# AMD GPUs (Linux)
radeontop

# Intel GPUs
intel_gpu_top
```

## Platform-Specific Development

### Windows Development
```toml
# Cargo.toml - Windows-specific dependencies
[target.'cfg(windows)'.dependencies]
windows = { version = "0.58", features = [
    "Win32_Media_DirectShow",
    "Win32_Media_MediaFoundation",
    "Win32_System_Com"
]}
```

```rust
// Windows-specific virtual camera implementation
#[cfg(windows)]
mod windows_camera {
    use windows::Win32::Media::DirectShow::*;
    
    pub fn create_virtual_camera() -> Result<()> {
        // DirectShow implementation
        Ok(())
    }
}
```

### macOS Development
```toml
# Cargo.toml - macOS-specific dependencies
[target.'cfg(target_os = "macos")'.dependencies]
core-foundation = "0.9"
core-graphics = "0.23"
```

```rust
// macOS-specific implementation
#[cfg(target_os = "macos")]
mod macos_camera {
    use core_foundation::*;
    
    pub fn create_virtual_camera() -> Result<()> {
        // AVFoundation implementation
        Ok(())
    }
}
```

### Linux Development
```toml
# Cargo.toml - Linux-specific dependencies
[target.'cfg(target_os = "linux")'].dependencies]
v4l = "0.14"
```

```rust
// Linux V4L2 implementation
#[cfg(target_os = "linux")]
mod linux_camera {
    use v4l::Device;
    
    pub fn create_virtual_camera() -> Result<()> {
        // V4L2 loopback implementation
        Ok(())
    }
}
```

## Contributing Guidelines

### Code Style

#### Rust Style
```rust
// Use snake_case for functions and variables
fn process_video_frame() -> Result<VideoFrame> {
    let frame_data = Vec::new();
    // ...
}

// Use PascalCase for types
struct VideoProcessor {
    frame_buffer: Vec<VideoFrame>,
}

// Use SCREAMING_SNAKE_CASE for constants
const MAX_BUFFER_SIZE: usize = 1000;
```

#### TypeScript Style
```typescript
// Use camelCase for functions and variables
const processVideoFrame = async (): Promise<VideoFrame> => {
  const frameData: Uint8Array = new Uint8Array();
  // ...
};

// Use PascalCase for components and types
interface VideoProcessorConfig {
  bufferSize: number;
  quality: number;
}

const VideoProcessor: React.FC<VideoProcessorProps> = ({ config }) => {
  // ...
};
```

### Pull Request Process

1. **Fork and Branch**
   ```bash
   git checkout -b feature/your-feature-name
   ```

2. **Make Changes**
   - Write code following style guidelines
   - Add tests for new functionality
   - Update documentation

3. **Test Thoroughly**
   ```bash
   npm run test
   cargo test
   npm run lint
   cargo clippy
   ```

4. **Submit PR**
   - Clear description of changes
   - Reference related issues
   - Include screenshots for UI changes

### Documentation Requirements

- Update API documentation for new commands
- Add inline code comments for complex logic
- Update user guide for new features
- Include examples in documentation

## Release Process

### Version Management
```bash
# Update version in multiple files
# package.json
# src-tauri/Cargo.toml
# src-tauri/tauri.conf.json

# Create version tag
git tag -a v0.2.0 -m "Release version 0.2.0"
git push origin v0.2.0
```

### Build Automation
```yaml
# .github/workflows/release.yml
name: Release
on:
  push:
    tags: ['v*']

jobs:
  build:
    strategy:
      matrix:
        platform: [windows-latest, macos-latest, ubuntu-latest]
    
    runs-on: ${{ matrix.platform }}
    
    steps:
      - uses: actions/checkout@v4
      - name: Setup Node.js
        uses: actions/setup-node@v4
        with:
          node-version: 18
      
      - name: Install Rust
        uses: dtolnay/rust-toolchain@stable
      
      - name: Build application
        run: npm run tauri:build
      
      - name: Upload artifacts
        uses: actions/upload-artifact@v4
        with:
          name: release-${{ matrix.platform }}
          path: src-tauri/target/release/bundle/
```

### Release Checklist

- [ ] Update version numbers
- [ ] Update CHANGELOG.md
- [ ] Run full test suite
- [ ] Build for all platforms
- [ ] Test builds on clean systems
- [ ] Update documentation
- [ ] Create GitHub release
- [ ] Announce on social media

---

This development guide provides everything needed to contribute to CamLooper. For additional help, join our [GitHub Discussions](../../discussions) or check the [Architecture Documentation](ARCHITECTURE.md) for technical details. 