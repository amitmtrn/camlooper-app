# Windows Limitations and Solutions

> **Historical document.** This describes the pre-0.2.0 state, when Windows builds were
> cross-compiled from Linux and FFmpeg could not be linked. Windows now builds natively in
> CI and ships the [softcam](https://github.com/tshino/softcam) DirectShow filter with a
> bundled FFmpeg. For the current design see
> [WINDOWS_VIRTUAL_CAMERA_IMPLEMENTATION.md](WINDOWS_VIRTUAL_CAMERA_IMPLEMENTATION.md); for
> current problems see [WINDOWS_TROUBLESHOOTING.md](WINDOWS_TROUBLESHOOTING.md). Kept for
> context on why the Windows code path is structured the way it is.

## Current Windows Upload Issue

The Windows build of CamLooper has limitations due to cross-compilation issues with FFmpeg. This document explains the current state and provides solutions.

## Issue Description

When building CamLooper for Windows, the following error occurs:

```
error: failed to run custom build command for `ffmpeg-sys-next v7.1.3`
...
thread 'main' panicked at .../ffmpeg-sys-next-7.1.3/build.rs:1035:14:
called `Result::unwrap()` on an `Err` value: pkg-config has not been configured to support cross-compilation.
```

This happens because:
1. FFmpeg is a native library that requires platform-specific binaries
2. Cross-compiling FFmpeg from Linux to Windows requires complex setup
3. The `ffmpeg-sys-next` crate doesn't handle cross-compilation well

## Current Solution

We've implemented conditional compilation to handle Windows builds:

### Features Available on Windows:
- ✅ Video file upload (basic functionality)
- ✅ File validation and metadata extraction (limited)
- ✅ Basic streaming with generated frames
- ✅ Virtual camera stub implementation
- ✅ All UI components work normally

### Features Limited on Windows:
- ❌ Detailed video metadata extraction (duration, fps, resolution)
- ❌ Actual video frame processing and decoding
- ❌ Real video frame streaming
- ❌ Full virtual camera functionality

### What Windows Users See:
- Videos upload successfully but show default metadata
- "Streaming" shows generated colorful frames instead of actual video
- Duration shows as 10 seconds (default)
- Resolution shows as 640x480 (default)
- Format shows as "Windows (Limited)"

## Solutions for Full Windows Support

### Option 1: Native Windows Build (Recommended)

Build CamLooper directly on Windows instead of cross-compiling:

1. **Install Prerequisites on Windows:**
   ```powershell
   # Install Rust
   winget install Rustlang.Rust.MSVC
   
   # Install Node.js
   winget install OpenJS.NodeJS
   
   # Install FFmpeg (using chocolatey)
   choco install ffmpeg
   ```

2. **Build on Windows:**
   ```bash
   git clone <repository>
   cd camlooper
   npm install
   npm run tauri:build
   ```

### Option 2: Use Pre-built FFmpeg Binaries

Modify the build to use pre-compiled FFmpeg binaries for Windows:

1. **Download FFmpeg Windows binaries**
2. **Configure environment variables**
3. **Use static linking**

### Option 3: Alternative Video Library

Replace FFmpeg with a Windows-friendly video library:

```toml
[target.'cfg(windows)'.dependencies]
# Use Windows Media Foundation
wmf = "0.1"
# Or use OpenCV with pre-built binaries
opencv = { version = "0.92", features = ["opencv-4"] }
```

### Option 4: WebAssembly Video Processing

Use browser-based video processing for Windows:

```toml
[target.'cfg(windows)'.dependencies]
# Use WebAssembly for video processing
ffmpeg-wasm = "0.1"
```

## Temporary Workaround

For immediate Windows functionality, the current build provides:

1. **Upload Works**: Files can be uploaded and stored
2. **Basic Streaming**: Generated frames for testing
3. **UI Functionality**: All interface components work
4. **Virtual Camera Stub**: Basic API available

### Using the Windows Build:

```bash
# The upload will work but with limited functionality
# You'll see generated frames instead of actual video
npm run tauri:build -- --target x86_64-pc-windows-msvc
```

## Future Improvements

### Short Term:
- [ ] Add Windows Media Foundation support
- [ ] Implement basic video metadata extraction
- [ ] Add file format validation

### Medium Term:
- [ ] Full FFmpeg integration for Windows
- [ ] Real video frame processing
- [ ] DirectShow virtual camera implementation

### Long Term:
- [ ] WebAssembly video processing
- [ ] Browser-based video handling
- [ ] Cross-platform video abstraction layer

## Error Messages and Debugging

### Common Windows Errors:

1. **"Upload failed" in UI**
   - Likely due to file permissions or temp directory issues
   - Check Windows temp directory access

2. **"Video format not supported"**
   - File validation works, but processing is limited
   - Try different video formats

3. **"Streaming not working"**
   - Windows shows generated frames instead of video
   - This is expected behavior in current build

### Debug Steps:

1. **Check Console Output:**
   ```bash
   # Look for these messages in console
   "Video loaded on Windows (limited functionality)"
   "Windows virtual camera started (stub)"
   ```

2. **Verify File Upload:**
   ```bash
   # Check if temp files are created
   dir %TEMP%
   ```

3. **Test Basic Functionality:**
   - Upload a small video file
   - Check if upload progress shows
   - Verify if streaming starts (with generated frames)

## Contributing

To help improve Windows support:

1. **Test on Windows**: Try building and running on Windows
2. **Report Issues**: Create issues with specific error messages
3. **Submit PRs**: Contribute Windows-specific implementations
4. **Documentation**: Help improve Windows-specific docs

## Contact

If you encounter Windows-specific issues not covered here, please:
1. Check the console output for specific error messages
2. Create an issue with your Windows version and error details
3. Include the full error log from the build process 