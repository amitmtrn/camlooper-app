# CamLooper User Guide

## Table of Contents
- [Getting Started](#getting-started)
- [Installation](#installation)
- [Interface Overview](#interface-overview)
- [Video Upload and Setup](#video-upload-and-setup)
- [Video Controls](#video-controls)
- [Virtual Camera Setup](#virtual-camera-setup)
- [Using with Applications](#using-with-applications)
- [Advanced Settings](#advanced-settings)
- [Performance Optimization](#performance-optimization)
- [Common Use Cases](#common-use-cases)
- [Tips and Best Practices](#tips-and-best-practices)

## Getting Started

Welcome to CamLooper! This guide will walk you through everything you need to know to use CamLooper effectively for creating virtual cameras from your video recordings.

### What is CamLooper?

CamLooper transforms any video recording into a seamless virtual camera that you can use in video conferencing applications, streaming software, and any application that supports camera input. It's perfect for:

- Creating professional backgrounds for video calls
- Looping demo videos for presentations
- Using pre-recorded content in live streams
- Testing applications with consistent video input
- Educational content delivery

## Installation

### System Requirements

**Minimum:**
- Windows 10/11, macOS 10.15+, or Linux (Ubuntu 18.04+)
- 4GB RAM
- 100MB free disk space
- Intel Core i3 or AMD equivalent

**Recommended:**
- 8GB RAM
- Dedicated graphics card
- Intel Core i5 or AMD Ryzen 5

### Download and Install

#### Windows
1. Download the `.msi` installer from the [releases page](../../releases)
2. Right-click the installer and select "Run as administrator"
3. Follow the installation wizard
4. Virtual camera drivers will be installed automatically
5. Restart your computer if prompted

#### macOS
1. Download the `.dmg` file from the [releases page](../../releases)
2. Open the `.dmg` file
3. Drag CamLooper to your Applications folder
4. Open Applications and launch CamLooper
5. Grant necessary permissions when prompted:
   - Camera access (for virtual camera creation)
   - File access (for video uploads)

#### Linux
1. Download the `.AppImage` or `.deb` package
2. For AppImage:
   ```bash
   chmod +x CamLooper.AppImage
   ./CamLooper.AppImage
   ```
3. For Debian/Ubuntu (.deb):
   ```bash
   sudo dpkg -i camlooper.deb
   sudo apt-get install -f  # Install dependencies if needed
   ```
4. Install v4l2loopback for virtual camera support:
   ```bash
   sudo apt install v4l2loopback-dkms
   sudo modprobe v4l2loopback
   ```

## Interface Overview

CamLooper's interface is divided into several main areas:

### Main Video Area
- **Video Preview**: Shows your uploaded video or current frame
- **Video Player Controls**: Play, pause, stop buttons
- **Progress Bar**: Shows current playback position
- **Loop Counter**: Displays current loop number

### Control Panel (Right Side)
- **Upload Section**: Drag-and-drop area for video files
- **Loop Settings**: Configure loop count and behavior
- **Virtual Camera Controls**: Start/stop virtual camera
- **Performance Metrics**: Real-time statistics
- **Compatible Apps**: Shows detected applications

### Status Bar (Bottom)
- **Video Information**: Duration, resolution, frame rate
- **Performance Indicators**: FPS, buffer health, dropped frames
- **Connection Status**: Virtual camera status

## Video Upload and Setup

### Supported Video Formats

CamLooper supports all major video formats:
- **MP4** (recommended for best performance)
- **MOV** (QuickTime)
- **AVI** (Audio Video Interleave)
- **MKV** (Matroska)
- **WMV** (Windows Media Video)
- **FLV** (Flash Video)
- **WEBM** (Web Media)

### Uploading Your Video

#### Method 1: Drag and Drop (Recommended)
1. Click and drag your video file into the upload area
2. Wait for the upload progress to complete
3. Video information will appear once loaded

#### Method 2: File Selector
1. Click the "Select Video" button in the upload area
2. Browse and select your video file
3. Click "Open" to begin upload

### Video Requirements and Tips

**Optimal Settings:**
- **Resolution**: 1920x1080 (1080p) or lower for best performance
- **Frame Rate**: 30 FPS maximum
- **Codec**: H.264 for optimal compatibility and performance
- **Duration**: Under 10 minutes for smooth looping

**File Size Guidelines:**
- **Small files** (under 100MB): Upload instantly
- **Medium files** (100MB - 500MB): May take 10-30 seconds
- **Large files** (over 500MB): Consider compressing for better performance

## Video Controls

### Basic Playback Controls

#### Play/Pause Button
- **Play**: Starts video streaming and virtual camera output
- **Pause**: Temporarily stops playback, maintains current position
- **Auto-resume**: Automatically resumes after buffering

#### Stop Button
- Stops playback completely
- Resets to beginning of video
- Stops virtual camera output

#### Loop Settings
1. **Loop Count Slider**: Set number of repetitions (1-10)
   - Drag slider to desired number
   - Set to 10 for infinite looping
2. **Auto-start Toggle**: Automatically play when video loads

### Advanced Playback Features

#### Buffer Management
- **Buffer Health**: Green = good, Yellow = moderate, Red = low
- **Automatic Adjustment**: Quality adjusts based on performance
- **Manual Quality**: Advanced users can override in settings

#### Performance Monitoring
- **Real-time FPS**: Shows actual frames per second
- **Dropped Frames**: Indicates performance issues
- **Processing Time**: Average time per frame

## Virtual Camera Setup

### Starting the Virtual Camera

1. **Load a Video**: Upload and configure your video first
2. **Click "Start Virtual Camera"**: Located in the control panel
3. **Wait for Activation**: Status will show "Active" when ready
4. **Verify in Apps**: "CamLooper Virtual Camera" appears in camera lists

### Virtual Camera Settings

#### Resolution Options
- **1920x1080** (recommended for most applications)
- **1280x720** (for lower-end systems)
- **Custom**: Set specific dimensions if needed

#### Frame Rate
- **30 FPS**: Default, works with all applications
- **60 FPS**: For high-refresh applications (uses more resources)
- **24 FPS**: For cinematic content

### Troubleshooting Virtual Camera

#### Camera Not Appearing
1. **Restart CamLooper**: Close and reopen the application
2. **Check Permissions**: Ensure camera permissions are granted
3. **Restart Target App**: Close and reopen your video conferencing app
4. **Reboot System**: Some systems require restart after installation

#### Poor Video Quality
1. **Check Resolution**: Match virtual camera to video resolution
2. **Adjust Quality**: Lower quality setting in advanced options
3. **Reduce Frame Rate**: Use 24 FPS instead of 30 FPS
4. **Close Other Apps**: Free up system resources

## Using with Applications

### Video Conferencing

#### Zoom
1. Open Zoom
2. Go to Settings → Video
3. Select "CamLooper Virtual Camera" from camera dropdown
4. Click "Test Video" to verify

#### Microsoft Teams
1. Open Teams
2. Click profile picture → Settings → Devices
3. Under "Camera", select "CamLooper Virtual Camera"
4. Test in a call or meeting

#### Google Meet
1. Join a meeting or start a test call
2. Click the three dots (More options)
3. Select Settings → Video
4. Choose "CamLooper Virtual Camera"

#### Discord
1. Open Discord
2. Go to User Settings (gear icon)
3. Select Voice & Video
4. Under "Camera", choose "CamLooper Virtual Camera"

### Streaming Software

#### OBS Studio
1. Add new Source → Video Capture Device
2. Create new source named "CamLooper"
3. Select "CamLooper Virtual Camera" as device
4. Configure as needed for your scene

#### Streamlabs
1. Add Source → Video Capture Device
2. Name your source "CamLooper"
3. Select "CamLooper Virtual Camera"
4. Position and resize as needed

### Other Applications

Most applications that support camera input will work with CamLooper:
- **Skype**: Settings → Audio & Video → Camera
- **WhatsApp Web**: Camera icon in video call
- **Slack**: Settings → Audio & Video
- **Browser Applications**: Usually auto-detect virtual cameras

## Advanced Settings

### Loop Configuration

#### Custom Loop Counts
- Set specific number of repetitions
- Infinite mode for continuous playback
- Auto-stop after predetermined loops

#### Loop Timing
- **Seamless**: No gap between loops (default)
- **Pause Between**: Brief pause for emphasis
- **Fade Transition**: Smooth fade between loops

### Quality Settings

#### Automatic Quality
- **Enabled**: Adjusts based on system performance (recommended)
- **Disabled**: Maintains constant quality regardless of performance

#### Manual Quality Override
- **High (80-100)**: Best quality, high system usage
- **Medium (50-80)**: Balanced quality and performance
- **Low (20-50)**: Performance priority, reduced quality

### Performance Tuning

#### Buffer Size
- **Large Buffer**: Smoother playback, higher memory usage
- **Small Buffer**: Lower memory, potential stuttering
- **Auto**: Adjusts based on video and system capabilities

#### Hardware Acceleration
- **Auto-detect**: Uses GPU when available
- **Force CPU**: Uses only processor (compatibility mode)
- **Force GPU**: Requires compatible graphics card

## Performance Optimization

### System Optimization

#### Close Unnecessary Applications
- Web browsers with many tabs
- Video editing software
- Resource-intensive games
- Background streaming services

#### Monitor System Resources
- **CPU Usage**: Should stay under 80% during playback
- **Memory Usage**: Keep at least 2GB free
- **Disk Space**: Ensure adequate temporary file space

### Video Optimization

#### Pre-processing Your Videos
1. **Compress Large Files**: Use HandBrake or similar tools
2. **Optimize Codec**: Convert to H.264 if needed
3. **Reduce Resolution**: Scale down to 1080p or 720p
4. **Limit Duration**: Keep loops under 5 minutes for best performance

#### Format Recommendations
- **Best**: MP4 with H.264 codec, 30 FPS, 1080p
- **Good**: MOV with H.264 codec, 24 FPS, 720p
- **Acceptable**: Any supported format under 500MB

## Common Use Cases

### Professional Video Calls

**Scenario**: You want a professional background for work meetings.

**Setup**:
1. Record or find a professional background video
2. Upload to CamLooper
3. Set infinite loop mode
4. Start virtual camera before your meeting
5. Select CamLooper as your camera in video conferencing app

**Tips**:
- Use subtle movement to avoid looking static
- Ensure good lighting in your recording
- Test before important meetings

### Educational Content

**Scenario**: Teaching online with consistent visual aids.

**Setup**:
1. Create educational video with slides or demonstrations
2. Upload to CamLooper with 3-5 loop count
3. Use as visual aid during lessons
4. Switch between live camera and CamLooper as needed

**Tips**:
- Include key points in the looped video
- Keep loops short (30-60 seconds)
- Prepare multiple videos for different topics

### Streaming and Content Creation

**Scenario**: Using pre-recorded content in live streams.

**Setup**:
1. Create branded intro/outro videos
2. Set up multiple CamLooper instances if needed
3. Use with OBS for professional streaming
4. Switch between live and recorded content

**Tips**:
- Create seamless loops for background content
- Use high-quality source videos
- Test stream setup before going live

### Software Development and Testing

**Scenario**: Testing applications that require camera input.

**Setup**:
1. Create test videos with known content
2. Use consistent video for reproducible testing
3. Test different resolutions and frame rates
4. Automate testing with scripted video content

**Tips**:
- Create videos with specific test patterns
- Use different lighting conditions
- Include edge cases in test videos

## Tips and Best Practices

### Video Preparation

1. **Plan Your Content**: Storyboard your video before recording
2. **Stable Recording**: Use tripods or stabilization for smooth footage
3. **Good Lighting**: Ensure even, bright lighting
4. **Audio Consideration**: CamLooper focuses on video; handle audio separately
5. **File Organization**: Keep your source videos organized

### Performance Best Practices

1. **Regular Cleanup**: Clear temporary files periodically
2. **Update Drivers**: Keep graphics drivers current
3. **Monitor Performance**: Watch the performance metrics panel
4. **Restart Regularly**: Restart CamLooper if running for extended periods
5. **System Maintenance**: Keep your system updated and optimized

### Creative Tips

1. **Seamless Loops**: Make ending frame match starting frame
2. **Natural Movement**: Include subtle motion to avoid static appearance
3. **Color Consistency**: Maintain consistent lighting and color temperature
4. **Test Different Angles**: Experiment with camera positions
5. **Brand Integration**: Include your branding subtly in background videos

### Troubleshooting Common Issues

#### Video Won't Upload
- Check file format is supported
- Ensure file isn't corrupted
- Try compressing the file
- Restart CamLooper and try again

#### Poor Playback Performance
- Close other applications
- Reduce video resolution
- Lower quality settings
- Check available system memory

#### Virtual Camera Not Working
- Verify camera permissions
- Restart target application
- Check for driver updates
- Try different virtual camera settings

---

Need more help? Check our [Troubleshooting Guide](TROUBLESHOOTING.md) for detailed solutions to common problems, or visit our [GitHub Discussions](../../discussions) for community support. 