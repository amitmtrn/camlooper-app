# CamLooper Custom DirectShow Video Source Filter

This directory contains the source code for a custom DirectShow video source filter that replaces the softcam library dependency.

## Overview

The custom filter provides:
- DirectShow-compatible virtual camera source
- RGB24 video output format
- Configurable resolution and framerate
- Direct frame data injection from Rust
- No external dependencies beyond DirectShow SDK

## Prerequisites

### Windows Development Environment
1. **Visual Studio 2019 or later** with C++ development tools
2. **DirectShow SDK** (included with Windows SDK)
3. **CMake** 3.16 or later

### DirectShow SDK Setup
The DirectShow SDK is included with the Windows SDK. If you need to install it separately:

1. Download the DirectX SDK (June 2010) from Microsoft
2. Install to default location: `C:\Program Files (x86)\Microsoft DirectX SDK (June 2010)`
3. Update the paths in `CMakeLists.txt` if needed

## Building the Filter

### Option 1: Using the Build Script (Recommended)
```batch
# Open Visual Studio Developer Command Prompt
# Navigate to this directory
cd src-tauri/custom_filter

# Run the build script
build.bat
```

### Option 2: Manual CMake Build
```batch
# Open Visual Studio Developer Command Prompt
mkdir build
cd build

# Configure
cmake .. -G "Visual Studio 16 2019" -A Win32

# Build
cmake --build . --config Release
```

## Registration

After building, register the filter with the system:

```batch
# Run as Administrator
regsvr32 build\bin\Release\CustomVideoSource.ax
```

To unregister:
```batch
regsvr32 /u build\bin\Release\CustomVideoSource.ax
```

## Integration with CamLooper

The custom filter is integrated into the Rust codebase through:

1. **FFI Bindings**: `src-tauri/src/custom_virtual_camera.rs`
2. **DirectShow Integration**: Uses Windows COM APIs
3. **Frame Processing**: Converts JPEG frames to RGB24 format

### Usage in Rust
```rust
use crate::custom_virtual_camera::CustomVirtualCamera;

// Create and start the camera
let mut camera = CustomVirtualCamera::new("My Camera", 1920, 1080, 30)?;
camera.start().await?;

// Send frames
camera.send_frame(&rgb_data)?;

// Stop
camera.stop()?;
```

## Architecture

### Components

1. **CCustomVideoSourceFilter**: Main DirectShow filter class
2. **CCustomVideoOutputPin**: Output pin for video data
3. **ICustomVideoSource**: Custom interface for frame injection
4. **Rust Integration**: FFI bindings and COM interop

### Data Flow
```
Rust App → JPEG Frame → RGB24 Conversion → Custom Filter → DirectShow Graph → Applications
```

## Troubleshooting

### Build Issues
- **Compiler not found**: Run from Visual Studio Developer Command Prompt
- **DirectShow headers not found**: Check DirectX SDK installation
- **Linker errors**: Ensure all DirectShow libraries are linked

### Runtime Issues
- **Filter not found**: Ensure filter is registered with `regsvr32`
- **Permission denied**: Run registration as Administrator
- **Applications don't see camera**: Restart applications after registration

### Debugging
1. Use GraphStudio to test the filter
2. Check Windows Event Viewer for DirectShow errors
3. Enable DirectShow debug logging

## Testing

### Using GraphStudio
1. Install GraphStudio (free DirectShow testing tool)
2. Add the custom filter to a graph
3. Connect to a video renderer
4. Test frame delivery

### Using OBS Studio
1. Add a "Video Capture Device" source
2. Select "CamLooper Custom Video Source"
3. Verify video appears in preview

## Performance

The custom filter is optimized for:
- **Low latency**: Direct frame injection
- **High throughput**: Efficient memory management
- **Minimal overhead**: No unnecessary conversions

## Security Considerations

- The filter runs in the same process as the application
- No network communication or external dependencies
- Follows DirectShow security best practices

## Future Enhancements

Potential improvements:
- Support for additional video formats (YUV, NV12)
- Hardware acceleration integration
- Audio support
- Multiple output pins
- Dynamic resolution switching

## License

This custom filter is part of the CamLooper project and follows the same license terms. 