# CamLooper Troubleshooting Guide

## Table of Contents
- [Quick Diagnostics](#quick-diagnostics)
- [Installation Issues](#installation-issues)
- [Video Upload Problems](#video-upload-problems)
- [Playback Issues](#playback-issues)
- [Virtual Camera Problems](#virtual-camera-problems)
- [Performance Issues](#performance-issues)
- [Platform-Specific Issues](#platform-specific-issues)
- [Error Messages](#error-messages)
- [Log Collection](#log-collection)
- [Getting Support](#getting-support)

## Quick Diagnostics

Before diving into specific issues, try these quick steps:

### Basic Checks
1. **Restart CamLooper** - Many issues resolve with a simple restart
2. **Check System Requirements** - Ensure your system meets minimum requirements
3. **Update Drivers** - Particularly graphics and camera drivers
4. **Close Other Applications** - Free up system resources
5. **Check Available Disk Space** - Ensure at least 1GB free space

### System Information
Collect this information for troubleshooting:
```bash
# Operating System and Version
# Available RAM and CPU
# Graphics Card Model and Driver Version
# CamLooper Version
# Other running applications
```

### Performance Check
Monitor these metrics in CamLooper:
- **FPS**: Should be close to target (usually 30)
- **Buffer Health**: Should stay green (>70%)
- **Dropped Frames**: Should remain low (<5% of total)
- **Memory Usage**: Monitor for gradual increases

## Installation Issues

### Windows Installation Problems

#### Issue: "Windows protected your PC" warning
**Cause**: Windows SmartScreen blocking unsigned application

**Solution**:
1. Click "More info" on the warning dialog
2. Click "Run anyway"
3. Alternative: Right-click installer → Properties → Unblock → Apply

#### Issue: Virtual camera drivers not installing
**Cause**: Insufficient permissions or conflicting drivers

**Solution**:
1. Run installer as Administrator:
   - Right-click installer → "Run as administrator"
2. Disable antivirus temporarily during installation
3. Check for conflicting virtual camera software:
   ```cmd
   # List installed virtual cameras
   driverquery | findstr /i camera
   ```
4. Manually install drivers:
   - Navigate to installation directory
   - Run `install_drivers.bat` as administrator

#### Issue: Installation fails with error 2502/2503
**Cause**: Windows Installer permissions issue

**Solution**:
1. Open Command Prompt as Administrator
2. Navigate to installer location
3. Run: `msiexec /i camlooper.msi`

### macOS Installation Problems

#### Issue: "Cannot open because it is from an unidentified developer"
**Cause**: macOS Gatekeeper blocking unsigned application

**Solution**:
1. **Method 1**: System Preferences approach
   - System Preferences → Security & Privacy → General
   - Click "Open Anyway" next to CamLooper message

2. **Method 2**: Terminal approach
   ```bash
   sudo xattr -cr /Applications/CamLooper.app
   sudo codesign --force --deep --sign - /Applications/CamLooper.app
   ```

#### Issue: Virtual camera not working in applications
**Cause**: Camera permissions not granted

**Solution**:
1. System Preferences → Security & Privacy → Camera
2. Check the box next to CamLooper
3. Restart CamLooper and target applications

### Linux Installation Problems

#### Issue: AppImage won't run
**Cause**: Missing execution permissions or FUSE

**Solution**:
1. Make executable:
   ```bash
   chmod +x CamLooper.AppImage
   ```
2. Install FUSE if needed:
   ```bash
   # Ubuntu/Debian
   sudo apt install fuse libfuse2
   
   # Fedora
   sudo dnf install fuse fuse-libs
   ```

#### Issue: v4l2loopback not working
**Cause**: Kernel module not loaded or configured

**Solution**:
1. Install v4l2loopback:
   ```bash
   sudo apt install v4l2loopback-dkms
   ```
2. Load the module:
   ```bash
   sudo modprobe v4l2loopback devices=1 video_nr=10 card_label="CamLooper"
   ```
3. Make permanent by adding to `/etc/modules`:
   ```bash
   echo 'v4l2loopback' | sudo tee -a /etc/modules
   ```

#### Issue: Permission denied errors
**Cause**: User not in video group

**Solution**:
```bash
# Add user to video group
sudo usermod -a -G video $USER

# Log out and back in, or use:
newgrp video
```

## Video Upload Problems

### Large File Upload Issues

#### Issue: Upload fails or times out
**Cause**: File too large or insufficient memory

**Solution**:
1. **Compress the video** using tools like HandBrake:
   - Target bitrate: 5-10 Mbps for 1080p
   - Use H.264 codec
   - Keep under 500MB if possible

2. **Free up system memory**:
   - Close unnecessary applications
   - Ensure at least 2GB free RAM

3. **Use chunked upload** for very large files:
   - CamLooper automatically uses chunked upload for files >100MB
   - Monitor upload progress in the interface

#### Issue: "Unsupported video format" error
**Cause**: Video format not supported by FFmpeg

**Solution**:
1. **Convert to supported format**:
   ```bash
   # Using FFmpeg
   ffmpeg -i input.mov -c:v libx264 -c:a aac output.mp4
   
   # Using HandBrake (GUI)
   # Select MP4 container with H.264 video
   ```

2. **Supported formats**:
   - **Recommended**: MP4 (H.264)
   - **Supported**: MOV, AVI, MKV, WMV, FLV, WEBM

### Upload Corruption Issues

#### Issue: Video appears corrupted after upload
**Cause**: Network interruption or encoding issues

**Solution**:
1. **Re-upload the file** completely
2. **Verify source file integrity**:
   ```bash
   # Test with FFmpeg
   ffmpeg -v error -i your_video.mp4 -f null -
   ```
3. **Try different upload method**:
   - Use file selector instead of drag-and-drop
   - Upload smaller files first to test

## Playback Issues

### Video Won't Play

#### Issue: Video loads but doesn't start playing
**Cause**: Codec or format compatibility issues

**Solution**:
1. **Check video information**:
   - Look at the video details panel
   - Verify duration, resolution, and format are detected

2. **Try different video file**:
   - Test with a known-good MP4 file
   - Use simple H.264 encoded video

3. **Restart video processor**:
   - Stop current video
   - Upload different file
   - Return to original file

#### Issue: Playback is choppy or stuttering
**Cause**: Performance limitations or resource constraints

**Solution**:
1. **Reduce video quality**:
   - Lower resolution to 720p or lower
   - Reduce frame rate to 24 FPS

2. **Close resource-intensive applications**:
   - Web browsers with many tabs
   - Video editing software
   - Games or streaming applications

3. **Check system performance**:
   - Monitor CPU usage (should be <80%)
   - Check available RAM (keep >2GB free)
   - Monitor disk I/O

### Loop Issues

#### Issue: Loops not seamless (visible gaps)
**Cause**: Video encoding or processing timing issues

**Solution**:
1. **Prepare video for seamless looping**:
   ```bash
   # Create seamless loop with FFmpeg
   ffmpeg -i input.mp4 -filter_complex "loop=loop=5:size=1:start=0" output.mp4
   ```

2. **Adjust loop settings**:
   - Try different loop counts
   - Enable/disable auto-start
   - Check if video naturally loops well

3. **Video preparation tips**:
   - Ensure first and last frames are similar
   - Use constant frame rate
   - Avoid abrupt scene changes at start/end

## Virtual Camera Problems

### Camera Not Detected

#### Issue: "CamLooper Virtual Camera" not appearing in applications
**Cause**: Virtual camera not started or driver issues

**Solution**:
1. **Verify virtual camera is active**:
   - Check status in CamLooper shows "Active"
   - Green indicator should be visible

2. **Restart applications**:
   - Close video conferencing app completely
   - Restart the application
   - Refresh camera list in app settings

3. **Restart virtual camera**:
   - Stop virtual camera in CamLooper
   - Wait 5 seconds
   - Start virtual camera again

4. **Check system camera list**:
   
   **Windows**:
   ```cmd
   # List available cameras
   dxdiag /t dxdiag_output.txt
   # Check "Sound" tab for video capture devices
   ```
   
   **macOS**:
   ```bash
   # List video devices
   system_profiler SPCameraDataType
   ```
   
   **Linux**:
   ```bash
   # List video devices
   v4l2-ctl --list-devices
   ```

### Poor Virtual Camera Quality

#### Issue: Virtual camera output is pixelated or low quality
**Cause**: Quality settings or resolution mismatch

**Solution**:
1. **Match resolutions**:
   - Set virtual camera resolution to match source video
   - Use 1920x1080 for best quality in most apps

2. **Adjust quality settings**:
   - Increase JPEG quality in advanced settings
   - Ensure "High Quality" mode is enabled

3. **Check target application settings**:
   - Some apps limit camera resolution
   - Check app-specific video quality settings

#### Issue: Virtual camera showing black screen
**Cause**: Video not playing or virtual camera synchronization issue

**Solution**:
1. **Ensure video is playing**:
   - Video should be actively playing in CamLooper
   - Check that frames are being displayed

2. **Restart virtual camera**:
   - Stop virtual camera
   - Stop video playback
   - Start video playback
   - Start virtual camera

3. **Check permissions**:
   - Verify camera permissions for target application
   - Some apps require explicit permission grants

## Performance Issues

### High CPU Usage

#### Issue: CamLooper using excessive CPU resources
**Cause**: Video processing demands or inefficient settings

**Solution**:
1. **Optimize video settings**:
   - Reduce video resolution
   - Lower frame rate to 24 FPS
   - Use lower quality settings

2. **System optimization**:
   - Close unnecessary applications
   - Disable background processes
   - Check for malware/viruses

3. **Hardware acceleration**:
   - Ensure graphics drivers are updated
   - Enable hardware acceleration if available
   - Consider GPU with hardware video decoding

### Memory Issues

#### Issue: Memory usage continuously increasing
**Cause**: Memory leak or excessive buffering

**Solution**:
1. **Restart CamLooper periodically**:
   - For long sessions, restart every few hours
   - Monitor memory usage over time

2. **Reduce buffer sizes**:
   - Lower frame buffer size in settings
   - Reduce video quality to decrease memory per frame

3. **Check system memory**:
   ```bash
   # Windows
   tasklist /fi "imagename eq camlooper.exe"
   
   # macOS
   ps aux | grep -i camlooper
   
   # Linux
   ps aux | grep -i camlooper
   ```

### Disk Space Issues

#### Issue: Temporary files filling up disk
**Cause**: Temporary video files not being cleaned up

**Solution**:
1. **Manual cleanup**:
   
   **Windows**: `%TEMP%\camlooper\*`
   
   **macOS**: `~/Library/Caches/com.camlooper.app/`
   
   **Linux**: `/tmp/camlooper/`

2. **Automatic cleanup**:
   - Restart CamLooper (triggers cleanup)
   - Upload smaller video files
   - Ensure adequate disk space (>1GB free)

## Platform-Specific Issues

> **Windows Users**: For comprehensive Windows troubleshooting, see the dedicated [Windows Troubleshooting Guide](WINDOWS_TROUBLESHOOTING.md).

### Windows-Specific

#### Issue: DirectShow errors
**Cause**: Windows media framework issues

**Solution**:
1. **Register DirectShow filters**:
   ```cmd
   regsvr32 quartz.dll
   regsvr32 qcap.dll
   ```

2. **Update Windows Media Feature Pack** (Windows N editions)

3. **Install Visual C++ Redistributables**

#### Issue: Windows Defender blocking application
**Cause**: False positive detection

**Solution**:
1. Add CamLooper to Windows Defender exclusions:
   - Windows Security → Virus & threat protection
   - Manage settings → Add exclusion
   - Add folder: CamLooper installation directory

### macOS-Specific

#### Issue: AVFoundation errors
**Cause**: macOS media framework permissions

**Solution**:
1. **Reset camera permissions**:
   ```bash
   sudo tccutil reset Camera
   ```

2. **Grant full disk access** (for some macOS versions):
   - System Preferences → Security & Privacy → Privacy
   - Full Disk Access → Add CamLooper

### Linux-Specific

#### Issue: PulseAudio conflicts
**Cause**: Audio system interfering with video processing

**Solution**:
1. **Restart PulseAudio**:
   ```bash
   pulseaudio -k
   pulseaudio --start
   ```

2. **Use ALSA directly** (advanced):
   ```bash
   # Add to environment
   export PULSE_SERVER=none
   ```

## Error Messages

### Common Error Messages and Solutions

#### "Failed to open video file"
**Causes**: File corruption, unsupported format, or access permissions

**Solutions**:
1. Verify file isn't corrupted
2. Convert to MP4/H.264 format
3. Check file permissions
4. Try uploading from different location

#### "FFmpeg initialization failed"
**Causes**: Missing FFmpeg libraries or incompatible version

**Solutions**:
1. Reinstall CamLooper (includes FFmpeg)
2. Update graphics drivers
3. Install Visual C++ Redistributables (Windows)

#### "Virtual camera creation failed"
**Causes**: Driver conflicts or permission issues

**Solutions**:
1. Restart as administrator/root
2. Uninstall conflicting virtual camera software
3. Reinstall virtual camera drivers

#### "Out of memory"
**Causes**: Insufficient RAM or memory leak

**Solutions**:
1. Close other applications
2. Reduce video resolution/quality
3. Restart CamLooper
4. Add more RAM to system

## Log Collection

### Enabling Debug Logs

#### Windows
1. Create shortcut to CamLooper
2. Add to target: `--debug --log-level=debug`
3. Run from Command Prompt:
   ```cmd
   set RUST_LOG=debug
   camlooper.exe
   ```

#### macOS
1. Open Terminal
2. Run:
   ```bash
   export RUST_LOG=debug
   /Applications/CamLooper.app/Contents/MacOS/camlooper
   ```

#### Linux
```bash
export RUST_LOG=debug
./CamLooper.AppImage
```

### Log Locations

**Windows**: `%APPDATA%\com.camlooper.app\logs\`

**macOS**: `~/Library/Logs/com.camlooper.app/`

**Linux**: `~/.local/share/com.camlooper.app/logs/`

### Useful Log Information
When reporting issues, include:
- Application logs (last 100 lines)
- System information
- Steps to reproduce
- Video file details (if relevant)
- Screenshots of error messages

## Getting Support

### Before Asking for Help
1. **Search existing issues** on GitHub
2. **Try basic troubleshooting** steps above
3. **Collect relevant information**:
   - Operating system and version
   - CamLooper version
   - Video file details
   - Error messages/logs
   - Steps to reproduce

### Where to Get Help
1. **GitHub Issues**: For bugs and feature requests
2. **GitHub Discussions**: For questions and community support
3. **Documentation**: Check other docs files for detailed information

### Reporting Bugs
Include this information:
- **OS**: Windows 11, macOS 13.1, Ubuntu 22.04, etc.
- **CamLooper Version**: Found in Help → About
- **Hardware**: CPU, RAM, Graphics card
- **Video Details**: Format, resolution, duration, file size
- **Steps to Reproduce**: Detailed steps that lead to the issue
- **Expected Behavior**: What should happen
- **Actual Behavior**: What actually happens
- **Logs**: Debug logs if available
- **Screenshots**: If applicable

### Performance Issues
For performance problems, also include:
- **System Resources**: CPU and memory usage during issue
- **Other Running Apps**: List of other applications running
- **Performance Metrics**: FPS, dropped frames, buffer health from CamLooper
- **Video Specifications**: Codec, bitrate, complexity

---

If you can't find a solution here, don't hesitate to ask for help in our [GitHub Discussions](../../discussions) or report a bug in [GitHub Issues](../../issues). The community and maintainers are here to help! 