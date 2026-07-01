# Troubleshooting Guide

## Common Build and Runtime Issues

### 1. Build Errors

#### Missing Dependencies
If you encounter errors about missing dependencies, ensure you have all required tools installed:

```bash
# On Ubuntu/Debian
sudo apt update
sudo apt install -y libwebkit2gtk-4.0-dev build-essential curl wget libssl-dev libgtk-3-dev libayatana-appindicator3-dev librsvg2-dev

# On macOS
brew install cairo pango gdk-pixbuf librsvg

# On Windows
# Install Visual Studio Build Tools with C++ workload
```

#### Rust/Cargo Issues
- Ensure you have the latest stable Rust version:
  ```bash
  rustup update stable
  ```
- Clear cargo cache if you encounter dependency resolution issues:
  ```bash
  cargo clean
  rm -rf Cargo.lock
  ```

#### MP4 Crate API Compatibility Issues
If you encounter compilation errors related to the mp4 crate API (especially on Windows), this is likely due to API changes in mp4 crate v0.14.0.

**Common Error Messages:**
- `failed to resolve: could not find SampleDescription in mp4`
- `binary operation == cannot be applied to type Result<TrackType, mp4::Error>`
- `no method named sample_description found for reference &Mp4Track`
- `mismatched types: expected Vec<Vec<u8>>, found Vec<Bytes>`

**Solution:**
The codebase has been updated to use the correct mp4 crate v0.14.0 API. If you're still encountering these errors:

1. **Update your dependencies:**
   ```bash
   cd src-tauri
   cargo update
   ```

2. **Clean and rebuild:**
   ```bash
   cargo clean
   cargo build
   ```

3. **Check your mp4 crate version in `Cargo.toml`:**
   ```toml
   [target.'cfg(windows)'.dependencies]
   mp4 = "0.14.0"
   ```

**Key API Changes Addressed:**
- `track.track_type()` now returns `Result<TrackType, mp4::Error>` instead of `TrackType`
- `SampleDescription` enum and `sample_description()` method are no longer available
- `sample.bytes` is now `mp4::Bytes` type instead of `Vec<u8>`
- Duration handling now uses `as_secs_f64()` method

### 2. Runtime Errors

#### FFmpeg Issues (Linux/macOS)
- **Error**: `FFmpeg library not found`
  ```bash
  # Ubuntu/Debian
  sudo apt install ffmpeg libavcodec-dev libavformat-dev libavutil-dev
  
  # macOS
  brew install ffmpeg
  ```

#### Windows Video Processing Issues
- **Error**: `Unsupported video format` on Windows
  - This has been fixed in the latest version with proper filename handling
  - Ensure you're using the latest code with the `load_video_with_original_name()` method

- **Error**: Broken video display (black screen)
  - This has been fixed with proper JPEG encoding in the Windows implementation
  - The fix includes using the `image` crate for proper RGB to JPEG conversion

#### Virtual Camera Issues
- **Windows**: Virtual camera functionality is currently limited
- **Linux**: Requires v4l2loopback module
  ```bash
  sudo modprobe v4l2loopback devices=1 video_nr=10 card_label="CamLooper" exclusive_caps=1
  ```
- **macOS**: Requires system permissions for camera access

### 3. Development Issues

#### Hot Reload Not Working
- Stop the dev server and restart:
  ```bash
  npm run tauri dev
  ```
- Clear node_modules and reinstall:
  ```bash
  rm -rf node_modules package-lock.json
  npm install
  ```

#### File Upload Issues
- **Check file permissions**: Ensure the application has read access to the video file
- **File path issues**: Avoid paths with special characters or spaces
- **File size limits**: Very large files (>1GB) may cause memory issues

#### Windows-Specific File Selection Issues
If file selection isn't working on Windows:
- This has been fixed in the latest version
- The fix addresses issues with `webkitRelativePath` property not being supported
- Error handling and UI state management have been improved

### 4. Performance Issues

#### High Memory Usage
- **Large video files**: The application loads entire files into memory
- **Solution**: Use smaller files or implement streaming for large files
- **Monitor**: Check Task Manager/Activity Monitor for memory usage

#### Slow Processing
- **Hardware**: Ensure adequate CPU and RAM
- **File format**: Some formats may process slower than others
- **Windows optimization**: The latest version uses real video data processing instead of synthetic frames

### 5. Platform-Specific Issues

#### Windows
- **Build tools**: Ensure Visual Studio Build Tools are installed
- **WebView2**: May require WebView2 runtime installation
- **File associations**: Windows may block certain file types

#### Linux
- **Display server**: Some desktop environments may have issues with certain graphics operations
- **Permissions**: May need to run with elevated permissions for virtual camera access

#### macOS
- **Gatekeeper**: App may be blocked by macOS security settings
- **Notarization**: Development builds are not notarized

## Getting Help

### Debug Information
When reporting issues, include:
- Operating system and version
- Rust version (`rustc --version`)
- Node.js version (`node --version`)
- Error messages (full stack trace)
- Steps to reproduce

### Console Logs
Enable verbose logging by checking the browser console (F12) and the terminal output where you ran `npm run tauri dev`.

### Common Log Patterns
- **Upload issues**: Look for "Upload Error" in the console
- **Format issues**: Check for "Unsupported video format" messages
- **API issues**: Look for mp4 crate or FFmpeg-related errors
- **Windows processing**: Check for "Windows video processing" debug messages

### Community Support
- Check existing issues on the GitHub repository
- Create a new issue with detailed information
- Include relevant log files and error messages

## Quick Fixes

### Reset Application State
```bash
# Clear all temporary files
rm -rf node_modules/.cache
rm -rf src-tauri/target

# Reinstall dependencies
npm install
cd src-tauri && cargo build
```

### Update Dependencies
```bash
# Update npm packages
npm update

# Update Rust dependencies
cd src-tauri && cargo update
```

### Force Clean Build
```bash
# Complete clean build
rm -rf node_modules package-lock.json
rm -rf src-tauri/target src-tauri/Cargo.lock
npm install
npm run tauri dev
``` 