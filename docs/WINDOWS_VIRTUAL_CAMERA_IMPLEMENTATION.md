# Windows Virtual Camera Implementation

## Overview

This implementation provides a Windows virtual camera solution using the **softcam** library that creates an actual DirectShow virtual camera device that Windows applications can detect and use.

**Status**: ✅ **FULLY IMPLEMENTED** - Complete implementation with automatic softcam setup

## 🚀 Quick Setup Guide

### ✨ Automatic Setup (Recommended)

**No manual installation required!** The application now handles everything automatically:

1. **Start CamLooper**: Launch the application
2. **Automatic download**: Softcam is downloaded automatically on first use
3. **Auto-registration**: The DLL is registered automatically
4. **Ready to use**: Your virtual camera works immediately!

The application includes a complete softcam manager that:
- Downloads the latest softcam release from GitHub
- Extracts both 32-bit and 64-bit DLLs
- Registers the DLLs using `regsvr32`
- Provides fallback manual installation instructions if needed

### 🔧 Manual Setup (Fallback)

If automatic setup fails, you can install manually:

1. **Download softcam**: Visit [softcam releases](https://github.com/tshino/softcam/releases)
2. **Extract files**: Extract the downloaded zip file
3. **Register DLL**: Run as Administrator:
   ```cmd
   regsvr32 "C:\path\to\softcam.dll"
   ```
4. **Verify**: Open Windows Camera app - you should see "DirectShow Softcam" in device list

### 🧪 Test Your Application

1. **Start virtual camera**: Enable virtual camera in CamLooper
2. **Test with apps**: Try Discord, Zoom, Teams, OBS Studio
3. **Check logs**: Look for "Softcam setup completed successfully" messages

## 🎯 What This Solves

### ❌ Previous Problem
- Windows applications couldn't see the virtual camera
- No DirectShow filter was registered
- Camera was "invisible" to the system

### ✅ Current Solution
- **Real DirectShow device**: Creates actual virtual camera that Windows can see
- **Automatic installation**: Downloads and sets up softcam automatically
- **Dynamic loading**: Works even if softcam isn't installed (graceful degradation)
- **Proper integration**: Uses industry-standard softcam library
- **Zero-configuration**: No manual setup required for most users

## 📋 Implementation Details

### Architecture

```
CamLooper Application
        ↓
Softcam Manager (Automatic Setup)
        ↓
Windows Virtual Camera (Rust)
        ↓
Softcam DLL (Dynamic Loading)
        ↓
DirectShow Filter
        ↓
Windows Camera System
        ↓
Applications (Discord, Zoom, etc.)
```

### Key Components

1. **`SoftcamManager`** - Handles automatic download and registration
2. **`WindowsVirtualCamera`** - Main Rust struct for camera operations
3. **Dynamic DLL Loading** - Loads softcam.dll at runtime
4. **FFI Bindings** - Function pointers to softcam functions
5. **Frame Processing** - RGB24 frame conversion and delivery

### Function Mappings

```rust
// Softcam API Functions (loaded dynamically)
scCreateCamera(width, height, fps) -> camera_handle
scSendFrame(camera_handle, rgb_data) -> result  
scDeleteCamera(camera_handle)
```

### Automatic Setup Process

1. **GitHub API Call**: Fetches latest softcam release info
2. **Download ZIP**: Downloads the release package
3. **Extract DLLs**: Extracts both x86 and x64 versions
4. **Register DLLs**: Uses `regsvr32` to register with Windows
5. **Verify Setup**: Checks if softcam is now available

## 🛠️ Build Configuration

### Cargo.toml Dependencies

```toml
[target.'cfg(windows)'.dependencies]
windows = { version = "0.58", features = [
    "Win32_Media_DirectShow",
    "Win32_System_LibraryLoader",
    "Win32_Foundation"
] }
reqwest = { version = "0.11", features = ["json"] }
serde_json = "1.0"
zip = "0.6"
cc = "1.0"
```

### Build Process

- ✅ **Dynamic linking**: No static dependencies on softcam
- ✅ **Graceful degradation**: App works without softcam (shows helpful error)
- ✅ **Cross-platform**: Only affects Windows builds
- ✅ **Automatic setup**: Downloads and installs softcam automatically

## 🔧 Usage Examples

### Basic Virtual Camera

```rust
use crate::windows_virtual_camera::WindowsVirtualCamera;

// Create virtual camera
let mut camera = WindowsVirtualCamera::new("My Camera", 1920, 1080, 30)?;

// Start the camera
camera.start()?;

// Send frames (RGB24 format)
let rgb_frame = vec![0u8; 1920 * 1080 * 3];
camera.send_frame(&rgb_frame)?;

// Stop when done
camera.stop()?;
```

### Integration with Existing Code

The main `virtual_camera.rs` automatically uses the Windows implementation:

```rust
#[cfg(target_os = "windows")]
async fn start_platform_camera(&self, mut frame_receiver: mpsc::UnboundedReceiver<Vec<u8>>) -> Result<()> {
    // Creates WindowsVirtualCamera
    // Converts JPEG → RGB24
    // Sends frames to softcam
}
```

### Tauri Commands

The application exposes these commands for softcam management:

```typescript
// Check if softcam is available
const isAvailable = await invoke('is_softcam_available');

// Setup softcam automatically
await invoke('setup_softcam');

// Get softcam status
const status = await invoke('get_softcam_status');
```

## 🧪 Testing

### Manual Testing

1. **Start CamLooper**
2. **Open Windows Camera app**
3. **Look for "DirectShow Softcam"** in device list
4. **Select it as video source**
5. **Verify video appears**

### Programmatic Testing

```rust
// Check if softcam is available
if softcam_manager::is_softcam_available().await {
    println!("✅ Softcam ready");
} else {
    println!("❌ Softcam not available");
}

// Setup softcam automatically
match softcam_manager::setup_softcam().await {
    Ok(_) => println!("✅ Softcam setup successful"),
    Err(e) => println!("❌ Setup failed: {}", e),
}
```

## 📦 Softcam Library Details

### What is Softcam?

- **Library**: C++ DirectShow virtual camera library by [tshino](https://github.com/tshino/softcam)
- **Purpose**: Creates actual DirectShow filters that Windows recognizes as cameras
- **License**: MIT (compatible with your project)
- **Stability**: Production-ready, actively maintained

### API Functions

```c
// C API (called via FFI)
scCamera scCreateCamera(int width, int height, int fps);
int scSendFrame(scCamera cam, const uint8_t* image);  
void scDeleteCamera(scCamera cam);
```

### Installation Methods

1. **Automatic setup** (recommended) - handled by CamLooper
2. **Pre-built releases** - manual download from GitHub
3. **Build from source** - requires Visual Studio

## 🚨 Troubleshooting

### Camera Not Visible

**Problem**: Windows apps don't see the camera

**Solutions**:
```cmd
# Check if DLL is registered
regsvr32 "C:\path\to\softcam.dll"

# Re-register if needed
regsvr32 /u "C:\path\to\softcam.dll"
regsvr32 "C:\path\to\softcam.dll"
```

### Automatic Setup Fails

**Problem**: Softcam doesn't install automatically

**Solutions**:
1. **Check internet connection** - required for download
2. **Run as Administrator** - required for DLL registration
3. **Check antivirus** - may block DLL registration
4. **Manual installation** - follow fallback instructions above

### Performance Issues

**Problem**: High CPU usage or frame drops

**Solutions**:
1. **Reduce video resolution** - try 720p instead of 1080p
2. **Lower frame rate** - 30 FPS instead of 60 FPS
3. **Close other applications** - free up system resources
4. **Check video encoding** - H.264 works best

## 📁 File Structure

```
src-tauri/
├── src/
│   ├── softcam_manager.rs      # Automatic softcam setup
│   ├── windows_virtual_camera.rs # Windows camera implementation
│   ├── virtual_camera.rs       # Platform-agnostic interface
│   └── lib.rs                  # Tauri command handlers
├── downloads/                  # Softcam DLL storage
│   ├── .gitignore             # Ignore downloaded files
│   └── README.md              # Purpose explanation
└── Cargo.toml                 # Dependencies
```

## 🔄 Future Enhancements

- **Multiple camera support** - Create multiple virtual cameras
- **Advanced frame processing** - Filters, effects, overlays
- **Performance optimization** - Hardware acceleration
- **Configuration persistence** - Save camera settings
- **Plugin system** - Third-party camera effects

---

**Status**: ✅ **Production Ready** - Full automatic setup with fallback manual installation