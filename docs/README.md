# CamLooper Documentation

Welcome to CamLooper - the ultimate virtual camera solution for content creators, streamers, and professionals.

## 📖 Table of Contents

- [Overview](#overview)
- [Quick Start](#quick-start)
- [Documentation Structure](#documentation-structure)
- [Features](#features)
- [System Requirements](#system-requirements)
- [Installation](#installation)
- [Basic Usage](#basic-usage)
- [Support](#support)

## Overview

[CamLooper](https://camlooper.com) is a Tauri-based desktop application that transforms any video recording into a seamless virtual camera. It enables you to loop videos infinitely and use them as camera sources in video conferencing applications, streaming software, and any application that supports camera input.

### Key Benefits

- 🎥 **Universal Compatibility**: Works with Zoom, Teams, OBS, Discord, and virtually any application
- 🔄 **Seamless Looping**: Perfect smooth transitions with customizable loop settings
- ⚡ **High Performance**: Optimized FFmpeg processing with minimal system impact
- 🔒 **Privacy First**: All processing happens locally - your videos never leave your device
- 💰 **Completely Free**: No subscriptions, no hidden fees, supported by unobtrusive ads

## Quick Start

1. **Download** CamLooper from [camlooper.com](https://camlooper.com) or our [releases page](../../releases)
2. **Install** the application on your system
3. **Upload** your video file using drag-and-drop
4. **Configure** loop settings and quality options
5. **Activate** the virtual camera
6. **Select** "CamLooper Virtual Camera" in your video conferencing app

## Documentation Structure

- **[User Guide](USER_GUIDE.md)** - Complete user manual with step-by-step instructions
- **[Architecture](ARCHITECTURE.md)** - Technical architecture and system design
- **[API Documentation](API.md)** - Complete API reference for developers
- **[Development Guide](DEVELOPMENT.md)** - Setup and development workflow
- **[Cross-Compilation Guide](CROSS_COMPILATION_GUIDE.md)** - Building for other platforms
- **[Windows Virtual Camera](WINDOWS_VIRTUAL_CAMERA_IMPLEMENTATION.md)** - How the DirectShow filter works, plus [manual setup fallback](WINDOWS_VIRTUAL_CAMERA_SETUP.md)
- **[Troubleshooting](TROUBLESHOOTING.md)** - Common issues and solutions
- **[Contributing](CONTRIBUTING.md)** - How to contribute, including the license terms and sign-off

### Project policies

- **[LICENSE](../LICENSE)** - PolyForm Noncommercial 1.0.0 (source-available, not open source)
- **[THIRD-PARTY.md](../THIRD-PARTY.md)** - Bundled components and their licenses
- **[TRADEMARK.md](../TRADEMARK.md)** - Use of the CamLooper name and logo
- **[SECURITY.md](../SECURITY.md)** - Reporting vulnerabilities
- **[CODE_OF_CONDUCT.md](../CODE_OF_CONDUCT.md)** - Community expectations

## Features

### Core Features
- **Virtual Camera Creation**: Convert any video into a virtual camera source
- **Infinite Looping**: Seamless video loops with smooth transitions
- **Real-time Processing**: Live frame streaming with optimized performance
- **Multi-format Support**: Works with all major video formats (MP4, AVI, MOV, etc.)
- **Quality Controls**: Adjustable video quality and encoding settings

### Advanced Features
- **Loop Control**: Set specific loop counts or infinite loops
- **Performance Monitoring**: Real-time metrics and performance tracking
- **Auto-start Options**: Automatic playback configuration
- **Frame Buffer Management**: Intelligent buffering for smooth playback
- **Virtual Camera Management**: Full control over virtual camera devices

## System Requirements

### Minimum Requirements
- **OS**: Windows 10/11, macOS 10.15+, or Linux (Ubuntu 18.04+)
- **CPU**: Intel Core i3 or AMD equivalent
- **RAM**: 4GB RAM
- **Storage**: 100MB free space
- **Graphics**: DirectX 11 compatible (Windows) or equivalent

### Recommended Requirements
- **CPU**: Intel Core i5 or AMD Ryzen 5
- **RAM**: 8GB RAM
- **Graphics**: Dedicated GPU for better performance

### Dependencies
- **FFmpeg**: Automatically bundled with the application
- **Virtual Camera Drivers**: Installed automatically during setup

## Installation

### Windows
1. Download the `.msi` installer from releases
2. Run the installer as administrator
3. Follow the installation wizard
4. Virtual camera drivers will be installed automatically

**Virtual Camera Setup** (Automatic):
✨ **No manual setup required!** The application now uses a custom-built DirectShow virtual camera:
1. Start CamLooper - custom virtual camera filter is built and registered automatically
2. Virtual camera appears instantly in all applications (Discord, Zoom, Teams, etc.)
3. No external dependencies - everything is self-contained

**Note**: Windows virtual camera support now features a custom DirectShow implementation that replaces external dependencies. The application builds and registers its own virtual camera filter, creating a real DirectShow virtual camera that appears in all camera applications with better performance and reliability.

### macOS
1. Download the `.dmg` file from releases
2. Drag CamLooper to Applications folder
3. Grant necessary permissions when prompted
4. Install virtual camera component if requested

### Linux
1. Download the `.AppImage` or `.deb` package
2. Make executable: `chmod +x CamLooper.AppImage`
3. Run or install: `./CamLooper.AppImage` or `sudo dpkg -i camlooper.deb`
4. Install v4l2loopback for virtual camera support

## Basic Usage

### 1. Upload Video
```
Drag and drop your video file into the upload area
Supported formats: MP4, AVI, MOV, MKV, WMV, FLV
```

### 2. Configure Settings
```
- Set loop count (1-10 or infinite)
- Adjust video quality (1-100)
- Configure auto-start options
- Set frame rate preferences
```

### 3. Start Virtual Camera
```
1. Click "Start Virtual Camera"
2. Configure camera settings if needed
3. Virtual camera becomes available system-wide
```

### 4. Use in Applications
```
1. Open your video conferencing app
2. Go to camera settings
3. Select "CamLooper Virtual Camera"
4. Your looped video is now your camera source
```

## Support

### Getting Help
- **Website**: Visit [camlooper.com](https://camlooper.com) for downloads and details
- **Documentation**: Start with this documentation set
- **Troubleshooting**: Check [troubleshooting guide](TROUBLESHOOTING.md)
- **Issues**: Report bugs on [GitHub Issues](../../issues)
- **Discussions**: Join [GitHub Discussions](../../discussions)

### Performance Tips
- Use H.264 encoded videos for best performance
- Keep video resolution at or below 1920x1080 for optimal performance
- Close unnecessary applications while using CamLooper
- Monitor performance metrics in the app for optimization

### Security & Privacy
- CamLooper processes all videos locally on your device — no video or frame data is ever uploaded
- Virtual camera streams only within your local system
- Temporary files are automatically cleaned up
- The only outbound request is the ad banner iframe loaded from `camlooper.com/ads/banner`
- Report vulnerabilities privately — see [SECURITY.md](../SECURITY.md)

---

**Ready to get started?** Check out the [User Guide](USER_GUIDE.md) for detailed instructions! 