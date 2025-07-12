# Windows Troubleshooting Guide

## Quick Diagnostics

If Windows upload is not working, follow these steps to diagnose the issue:

### Step 1: Check Console Output

1. **Open Developer Tools** in the CamLooper application:
   - Press `F12` or `Ctrl+Shift+I`
   - Go to the Console tab

2. **Look for specific error messages**:
   ```
   // Success messages you should see:
   "Starting stream upload: filename.mp4 (12345 bytes, 1024 byte chunks)"
   "Upload session created: temp file at C:\Users\...\AppData\Local\Temp\..."
   "Chunk 1 written: 1024 bytes (total: 1024/12345)"
   "Upload complete: 12345 bytes written"
   "Video loaded on Windows: filename.mp4 (12345 bytes, estimated 15s)"
   ```

### Step 2: Test Basic Upload

1. **Try a small test file first** (under 1MB)
2. **Use a common video format** (MP4, MOV, AVI)
3. **Watch the upload progress bar** - it should increase

### Step 3: Check File Permissions

1. **Verify temp directory access**:
   - Check that `%TEMP%` is writable
   - Ensure antivirus isn't blocking file creation

2. **Run as Administrator** if needed:
   - Right-click CamLooper → "Run as Administrator"

## Common Issues and Solutions

### Issue 1: "Failed to create temporary file"

**Symptoms:**
- Upload fails immediately
- Error mentions temp file creation

**Causes:**
- Antivirus blocking file creation
- Insufficient disk space
- Corrupted temp directory
- Permissions issues

**Solutions:**
1. **Check disk space**: Ensure at least 1GB free space
2. **Clear temp directory**:
   ```bash
   # In Command Prompt
   del /q /s "%TEMP%\*"
   ```
3. **Add antivirus exclusion**:
   - Add CamLooper installation directory to antivirus exceptions
   - Add `%TEMP%` to antivirus exceptions
4. **Reset temp directory permissions**:
   ```bash
   # In Command Prompt (as Administrator)
   icacls "%TEMP%" /grant %USERNAME%:F /t
   ```

### Issue 2: "Upload session not found"

**Symptoms:**
- Upload starts but fails during chunked upload
- Error about missing session

**Causes:**
- Session cleanup happening too early
- Memory issues causing session loss
- Multiple upload attempts interfering

**Solutions:**
1. **Restart CamLooper** to clear sessions
2. **Upload smaller files** (under 10MB)
3. **Avoid multiple simultaneous uploads**
4. **Check available RAM** (ensure 2GB+ free)

### Issue 3: "Unsupported video format"

**Symptoms:**
- Upload fails with format error
- File appears valid but not accepted

**Causes:**
- Unusual video codec
- Corrupted file header
- Non-standard file extension

**Solutions:**
1. **Convert to MP4**:
   ```bash
   # Using FFmpeg (if installed)
   ffmpeg -i input.mov -c:v libx264 -c:a aac output.mp4
   ```
2. **Try different formats**: MP4, MOV, AVI are most reliable
3. **Check file integrity**:
   - Try playing the file in Windows Media Player
   - Verify file isn't corrupted

### Issue 4: "Video loads but streaming shows weird colors"

**Symptoms:**
- Upload succeeds
- Video info shows correctly
- Streaming shows animated colors instead of video

**Explanation:**
This is **expected behavior** on Windows builds. The current Windows implementation:
- ✅ Uploads videos successfully
- ✅ Stores files properly
- ✅ Provides estimated metadata
- ⚠️ Shows generated frames instead of actual video content

**This is a limitation**, not a bug. See [Windows Limitations](WINDOWS_LIMITATIONS.md) for details.

### Issue 5: Application won't start

**Symptoms:**
- CamLooper doesn't launch
- Crashes on startup
- Error about missing DLLs

**Solutions:**
1. **Install Visual C++ Redistributables**:
   - Download from Microsoft website
   - Install both x86 and x64 versions

2. **Check Windows version**:
   - Windows 10 version 1903 or later required
   - Windows 11 fully supported

3. **Update Windows**:
   - Install latest Windows updates
   - Restart computer

4. **Run compatibility troubleshooter**:
   - Right-click CamLooper → Properties → Compatibility
   - Run compatibility troubleshooter

## Advanced Debugging

### Enable Debug Logging

1. **Set environment variable**:
   ```bash
   # In Command Prompt
   set RUST_LOG=debug
   camlooper.exe
   ```

2. **Check for additional log messages** in console

### Manual File Upload Test

1. **Create a simple test file**:
   ```bash
   # Create 1MB test file
   fsutil file createnew test_video.mp4 1048576
   ```

2. **Try uploading the test file**
3. **Check console for detailed error messages**

### Check Windows Event Viewer

1. **Open Event Viewer**:
   - Press `Win+R`, type `eventvwr.msc`
   - Navigate to Windows Logs → Application

2. **Look for CamLooper errors**:
   - Filter by Application name
   - Check for .NET or application crash events

### Memory and Resource Monitoring

1. **Open Task Manager** (`Ctrl+Shift+Esc`)
2. **Monitor CamLooper process**:
   - Check memory usage (should be under 500MB)
   - Watch for memory leaks during upload
   - Verify CPU usage is reasonable

## Registry and System Issues

### Clean Installation

If issues persist, try a clean installation:

1. **Uninstall CamLooper**:
   - Use Windows Add/Remove Programs
   - Delete installation directory manually

2. **Clear registry entries**:
   ```bash
   # In Command Prompt (as Administrator)
   reg delete "HKEY_CURRENT_USER\Software\CamLooper" /f
   ```

3. **Clear temp files**:
   ```bash
   del /q /s "%TEMP%\camlooper*"
   del /q /s "%APPDATA%\com.camlooper.app*"
   ```

4. **Reinstall CamLooper**

### System File Check

If Windows itself has issues:

```bash
# In Command Prompt (as Administrator)
sfc /scannow
DISM /Online /Cleanup-Image /RestoreHealth
```

## Getting Help

### Information to Collect

When reporting Windows issues, include:

1. **Windows version**: `winver` command output
2. **CamLooper version**: Check About dialog
3. **Console output**: Copy full error messages
4. **System specs**: RAM, CPU, disk space
5. **Antivirus software**: Name and version
6. **File details**: Size, format, source of video file

### Where to Report

1. **GitHub Issues**: For bugs and feature requests
2. **Discussion Forums**: For general help and questions
3. **Email Support**: For private/sensitive issues

### Temporary Workarounds

While waiting for fixes, you can:

1. **Use for testing**: Windows build works for interface testing
2. **Build on Windows**: Full functionality when built natively
3. **Use Linux/macOS**: Full feature support on other platforms
4. **File conversion**: Convert videos to supported formats

## Performance Optimization

### For Better Performance

1. **Close unnecessary applications**
2. **Ensure adequate disk space** (1GB+ free)
3. **Use SSD if available** for temp files
4. **Increase virtual memory** if needed
5. **Update graphics drivers**

### Resource Limits

Current Windows implementation limits:
- Maximum file size: 500MB recommended
- Maximum chunks: 1000 per upload
- Memory usage: ~100MB per upload session
- Concurrent uploads: 1 recommended

## Future Improvements

The Windows implementation will be enhanced with:
- [ ] Real video processing capabilities
- [ ] Actual frame extraction and streaming
- [ ] Better metadata extraction
- [ ] Full virtual camera functionality
- [ ] Performance optimizations

Check the [Windows Limitations](WINDOWS_LIMITATIONS.md) document for the latest status and roadmap. 