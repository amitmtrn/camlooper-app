# Windows Virtual Camera Implementation

## Overview

This implementation provides a comprehensive Windows virtual camera solution using DirectShow and COM interfaces instead of the limited `virtualcam_rs` crate. The solution creates a proper virtual camera that can be used by any Windows application that supports video capture devices.

## Architecture

### Core Components

1. **WindowsVirtualCamera** - Main struct that manages virtual camera lifecycle
2. **VirtualCameraManager** - Global manager for multiple virtual camera instances
3. **VirtualCameraFilter** - COM-based DirectShow filter implementation
4. **Frame Management** - RGB frame processing and DirectShow integration

### Key Features

- DirectShow and MediaFoundation API integration
- COM interface implementation for Windows compatibility
- Registry-based device registration (requires admin privileges)
- RGB frame processing and conversion
- Multi-instance virtual camera support
- Proper COM lifetime management

## Implementation Details

### Dependencies Added

```toml
[target.'cfg(windows)'.dependencies]
windows = { version = "0.58", features = [
    "Win32_Media_DirectShow",
    "Win32_Media_MediaFoundation", 
    "Win32_System_Com",
    "Win32_Foundation",
    "Win32_Media",
    "Win32_System_Registry",
    "Win32_System_LibraryLoader",
    "Win32_System_Memory",
    "Win32_System_SystemServices",
    "Win32_Graphics_Gdi",
    "Win32_Security",
    "implement"
] }
widestring = "1.0"
uuid = { version = "1.0", features = ["v4"] }
```

### Core Files

1. **src-tauri/src/windows_vcam.rs** - Windows-specific virtual camera implementation
2. **src-tauri/src/virtual_camera.rs** - Updated with Windows integration
3. **src-tauri/Cargo.toml** - Updated dependencies

## Usage

### Starting a Virtual Camera

```rust
use virtual_camera::{VirtualCameraConfig, start_virtual_camera};

let config = VirtualCameraConfig {
    width: 1920,
    height: 1080,
    fps: 30,
    camera_name: "CamLooper Virtual Camera".to_string(),
};

let status = start_virtual_camera(Some(config)).await?;
println!("Virtual camera started: {:?}", status);
```

### Sending Frames

```rust
use virtual_camera::send_frame_to_virtual_camera;

// RGB frame data (width * height * 3 bytes)
let rgb_frame: Vec<u8> = get_video_frame();
send_frame_to_virtual_camera(rgb_frame).await?;
```

### Stopping the Virtual Camera

```rust
use virtual_camera::stop_virtual_camera;

let status = stop_virtual_camera().await?;
println!("Virtual camera stopped: {:?}", status);
```

## Technical Implementation

### COM Interface Implementation

The implementation includes a complete DirectShow filter that implements:

- `IUnknown` - Basic COM object interface
- `IBaseFilter` - DirectShow filter interface
- Proper reference counting and lifecycle management

### Frame Processing

1. **Input**: JPEG frames from video processor
2. **Conversion**: JPEG to RGB24 format
3. **DirectShow**: RGB frames sent to virtual camera filter
4. **Output**: Available to Windows applications as video capture device

### Registry Integration

The virtual camera registers itself in the Windows registry as a video capture device:

- CLSID registration for DirectShow filter
- Device enumeration support
- Friendly name configuration

## Platform-Specific Features

### Windows-Only Features

- DirectShow filter implementation
- COM object registration
- Windows registry integration
- GDI bitmap creation utilities
- MediaFoundation API support

### Cross-Platform Compatibility

The implementation maintains compatibility with:
- Linux (v4l2loopback via FFmpeg)
- macOS (placeholder for AVFoundation)
- Graceful fallback for unsupported platforms

## API Functions

### Core Functions

- `WindowsVirtualCamera::new()` - Create new virtual camera instance
- `WindowsVirtualCamera::start()` - Start the virtual camera
- `WindowsVirtualCamera::stop()` - Stop the virtual camera
- `send_frame_to_virtual_camera()` - Send RGB frame data
- `list_video_devices()` - Enumerate all video devices

### Utility Functions

- `ensure_com_initialized()` - Initialize COM subsystem
- `create_bitmap_from_rgb()` - Convert RGB data to Windows bitmap

## Requirements

### System Requirements

- Windows 10 or later
- Administrator privileges (for registry operations)
- DirectShow support
- COM subsystem

### Development Requirements

- Rust 1.70+
- Windows SDK
- Visual Studio Build Tools

## Limitations and Notes

### Current Limitations

1. **Registry Operations**: Requires administrator privileges for full registration
2. **DirectShow Filter**: Simplified implementation, not a full production filter
3. **Frame Format**: Currently supports RGB24 format only
4. **Single Stream**: One output pin per virtual camera

### Production Considerations

For a production implementation, consider:

1. **Digital Signing**: Sign the COM component for Windows trust
2. **Installer**: MSI installer for proper registration
3. **Multiple Formats**: Support for various pixel formats (YUV, etc.)
4. **Error Handling**: More robust error handling and recovery
5. **Performance**: Optimize frame processing pipeline

## Integration with Existing Code

### Virtual Camera Module

The implementation integrates seamlessly with the existing virtual camera architecture:

```rust
#[cfg(target_os = "windows")]
async fn start_platform_camera(&mut self, frame_receiver: mpsc::UnboundedReceiver<Vec<u8>>) -> Result<()> {
    // Initialize Windows virtual camera
    let mut vcam = windows_vcam::WindowsVirtualCamera::new(
        &config.camera_name,
        config.width,
        config.height,
        config.fps,
    )?;
    
    vcam.start()?;
    self.windows_vcam = Some(vcam);
    
    // Frame processing loop...
}
```

### Frame Processing Pipeline

1. Video processor generates JPEG frames
2. Frames sent to virtual camera via channel
3. Windows implementation converts JPEG to RGB
4. RGB frames sent to DirectShow filter
5. Applications can capture from virtual camera

## Testing

### Manual Testing

1. Build the application: `npm run build:windows`
2. Start the virtual camera
3. Open Windows Camera app or OBS Studio
4. Look for "CamLooper Virtual Camera" in device list
5. Select the virtual camera as video source
6. Send test frames and verify display

### Automated Testing

```rust
#[cfg(test)]
mod tests {
    use super::*;
    
    #[tokio::test]
    async fn test_virtual_camera_lifecycle() {
        let config = VirtualCameraConfig::default();
        let status = start_virtual_camera(Some(config)).await.unwrap();
        assert!(status.is_active);
        
        let stop_status = stop_virtual_camera().await.unwrap();
        assert!(!stop_status.is_active);
    }
}
```

### Build Commands

```bash
# Development
npm run tauri:dev              # Start development server
npm run tauri:build:debug      # Debug build

# Production
npm run tauri:build            # Standard cross-platform build  
npm run build:windows          # Windows-specific build (MSVC)

# Testing
./build-test.sh                # Comprehensive build verification
```

## Troubleshooting

### Common Issues

1. **COM Initialization Failure**: Ensure proper Windows version and permissions
2. **Registry Access Denied**: Run with administrator privileges
3. **Device Not Appearing**: Check DirectShow filter registration
4. **Frame Not Displaying**: Verify RGB format and dimensions

### Debug Information

Enable debug logging to troubleshoot issues:

```rust
env_logger::init();
log::debug!("Starting Windows virtual camera: {}", name);
```

## Future Enhancements

### Planned Features

1. **Multiple Pin Types**: Support for different output formats
2. **Property Pages**: Configuration UI for the DirectShow filter
3. **Audio Support**: Virtual audio device integration
4. **Hardware Acceleration**: GPU-based frame processing
5. **Installer Package**: MSI installer with proper registration

### Performance Optimizations

1. **Zero-Copy**: Direct memory mapping for frame transfer
2. **Async Processing**: Fully asynchronous frame pipeline
3. **Format Negotiation**: Dynamic format selection based on consumer
4. **Buffer Management**: Optimized frame buffering strategy

This implementation provides a robust foundation for Windows virtual camera functionality that can be extended and customized for specific use cases.