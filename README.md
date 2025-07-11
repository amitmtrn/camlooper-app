# CamLooper

A Tauri-based video looping application with virtual camera functionality.

## Features

- 🎥 Video file upload and processing
- 🔄 Looping video playback
- 📹 Virtual camera output to share loops with other applications
- 🖥️ Cross-platform support (Windows, macOS, Linux)
- ⚡ Hardware-accelerated processing where available

## Virtual Camera Support

### Windows
- **Native DirectShow Integration**: Uses Windows DirectShow and COM APIs for maximum compatibility
- **No External Dependencies**: Works without requiring FFmpeg installation
- **Registry Integration**: Proper virtual camera device registration
- **Universal Compatibility**: Works with Zoom, Teams, OBS, and other Windows applications

### Linux
- **v4l2loopback Support**: Integrates with v4l2loopback virtual camera devices
- **FFmpeg Integration**: High-quality video processing and format conversion
- **Multiple Device Support**: Can create multiple virtual camera instances

### macOS
- **AVFoundation Ready**: Prepared for native macOS virtual camera implementation
- **FFmpeg Fallback**: Current implementation uses FFmpeg for video processing

## Prerequisites

### All Platforms
- [Rust](https://rustup.rs/) 1.70 or later
- [Node.js](https://nodejs.org/) 18 or later
- [Tauri CLI](https://tauri.app/start/prerequisites/)

### Platform-Specific Requirements

#### Windows
- **Visual Studio Build Tools** or Visual Studio with C++ support
- **Windows 10/11** with DirectShow support
- **Administrator privileges** (for virtual camera registration)

#### Linux
- **FFmpeg development libraries**:
  ```bash
  sudo apt-get install ffmpeg libavcodec-dev libavformat-dev libavutil-dev
  ```
- **v4l2 development libraries**:
  ```bash
  sudo apt-get install libv4l-dev
  ```
- **v4l2loopback module** (for virtual camera):
  ```bash
  sudo apt-get install v4l2loopback-dkms
  sudo modprobe v4l2loopback
  ```

#### macOS
- **FFmpeg** (via Homebrew):
  ```bash
  brew install ffmpeg
  ```
- **Xcode Command Line Tools**:
  ```bash
  xcode-select --install
  ```

## Quick Start

1. **Clone the repository**:
   ```bash
   git clone <repository-url>
   cd camlooper
   ```

2. **Install dependencies**:
   ```bash
   npm install
   ```

3. **Check build requirements** (optional):
   ```bash
   chmod +x build-test.sh
   ./build-test.sh
   ```

4. **Start development server**:
   ```bash
   npm run tauri dev
   ```

## Building for Production

### Development Build
```bash
npm run tauri build
```

### Release Build
```bash
npm run tauri build --release
```

## Virtual Camera Architecture

### Windows Implementation

The Windows virtual camera implementation provides enterprise-grade virtual camera functionality:

```rust
// Example usage
use virtual_camera::{VirtualCameraConfig, start_virtual_camera};

let config = VirtualCameraConfig {
    width: 1920,
    height: 1080,
    fps: 30,
    camera_name: "CamLooper Virtual Camera".to_string(),
};

let status = start_virtual_camera(Some(config)).await?;
```

**Key Components:**
- `WindowsVirtualCamera`: Core virtual camera management
- `VirtualCameraFilter`: DirectShow filter implementation
- `VirtualCameraManager`: Multi-instance camera support
- Registry integration for system-wide device availability

**Features:**
- ✅ DirectShow and MediaFoundation API integration
- ✅ COM interface implementation for Windows compatibility
- ✅ Multi-instance virtual camera support
- ✅ Proper COM lifecycle management
- ✅ RGB frame processing and conversion
- ✅ Device enumeration and management

### Cross-Platform Compatibility

The virtual camera system automatically adapts to each platform:

| Platform | Technology | Features |
|----------|------------|----------|
| Windows | DirectShow/COM | Native integration, no external deps |
| Linux | v4l2loopback + FFmpeg | High compatibility, multiple formats |
| macOS | AVFoundation (planned) | Native macOS integration |

## Troubleshooting

### Common Build Issues

#### FFmpeg Not Found (Linux/macOS)
```bash
# Ubuntu/Debian
sudo apt-get install ffmpeg libavcodec-dev libavformat-dev libavutil-dev

# macOS
brew install ffmpeg
```

#### Windows Build Tools Missing
- Install [Visual Studio Build Tools](https://visualstudio.microsoft.com/downloads/#build-tools-for-visual-studio-2022)
- Or install Visual Studio Community with "Desktop development with C++" workload

#### Virtual Camera Not Appearing

**Windows:**
- Ensure application is run with administrator privileges
- Check DirectShow filter registration in registry
- Verify COM subsystem is properly initialized

**Linux:**
- Load v4l2loopback module: `sudo modprobe v4l2loopback`
- Check available devices: `ls /dev/video*`
- Ensure user has permission to access video devices

### Debug Mode

Enable detailed logging by setting environment variables:

```bash
# Enable Rust logging
RUST_LOG=debug npm run tauri dev

# Enable Tauri debugging
TAURI_DEBUG=true npm run tauri dev
```

## API Reference

### Virtual Camera Functions

- `start_virtual_camera(config?)` - Initialize and start virtual camera
- `stop_virtual_camera()` - Stop and cleanup virtual camera
- `send_frame_to_virtual_camera(frameData)` - Send video frame to virtual camera
- `get_virtual_camera_status()` - Get current camera status
- `list_video_devices()` - Enumerate available video devices

### Configuration Options

```typescript
interface VirtualCameraConfig {
  width: number;        // Frame width in pixels
  height: number;       // Frame height in pixels
  fps: number;          // Frames per second
  camera_name: string;  // Display name for the virtual camera
}
```

## Development

### Project Structure

```
camlooper/
├── src/                    # Frontend React/TypeScript code
├── src-tauri/             # Tauri Rust backend
│   ├── src/
│   │   ├── virtual_camera.rs    # Cross-platform virtual camera
│   │   ├── windows_vcam.rs      # Windows-specific implementation
│   │   ├── video_processor.rs   # Video processing engine
│   │   └── video_upload.rs      # File upload handling
│   └── Cargo.toml
├── build-test.sh         # Build verification script
└── README.md
```

### Adding New Features

1. **Backend (Rust)**: Add new functions in `src-tauri/src/`
2. **Frontend (TypeScript)**: Update React components in `src/`
3. **Tauri Commands**: Register new commands in `lib.rs`
4. **Testing**: Add tests and update `build-test.sh`

## Contributing

1. Fork the repository
2. Create a feature branch: `git checkout -b feature-name`
3. Make changes and test with `./build-test.sh`
4. Commit changes: `git commit -am 'Add feature'`
5. Push to branch: `git push origin feature-name`
6. Create a Pull Request

## License

[Add your license information here]

## Acknowledgments

- Built with [Tauri](https://tauri.app/) for cross-platform desktop applications
- Uses [FFmpeg](https://ffmpeg.org/) for video processing on Linux/macOS
- Windows implementation leverages DirectShow and COM APIs
- Virtual camera functionality inspired by OBS Studio's virtual camera
