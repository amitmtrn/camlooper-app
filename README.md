# CamLooper

<div align="center">
  <img src="public/favicon.ico" alt="CamLooper Logo" width="80" height="80">
  
  **Transform any video recording into a seamless virtual camera**
  
  [![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)
  [![Platform Support](https://img.shields.io/badge/platform-Windows%20%7C%20macOS%20%7C%20Linux-blue.svg)](https://github.com/your-org/camlooper/releases)
  [![Version](https://img.shields.io/github/v/release/your-org/camlooper)](https://github.com/your-org/camlooper/releases)
  
  [Download](https://github.com/your-org/camlooper/releases) • [Documentation](docs/) • [Report Bug](https://github.com/your-org/camlooper/issues) • [Feature Request](https://github.com/your-org/camlooper/issues)
</div>

## 🎥 What is CamLooper?

CamLooper is a powerful desktop application that transforms any video recording into a virtual camera that works seamlessly with video conferencing applications, streaming software, and any application that supports camera input. Perfect for content creators, streamers, educators, and professionals who want to use pre-recorded content in their video calls or streams.

### ✨ Key Features

- 🎬 **Universal Virtual Camera** - Works with Zoom, Teams, OBS, Discord, and virtually any application
- 🔄 **Seamless Video Looping** - Perfect smooth transitions with customizable loop settings
- ⚡ **High Performance** - Optimized FFmpeg processing with minimal system impact
- 🔒 **Privacy First** - All processing happens locally - your videos never leave your device
- 💰 **Completely Free** - No subscriptions, no hidden fees, supported by unobtrusive ads
- 🌐 **Cross-Platform** - Native support for Windows, macOS, and Linux
- 📱 **Modern UI** - Beautiful, intuitive interface built with modern web technologies

## 🚀 Quick Start

1. **Download** CamLooper from our [releases page](https://github.com/your-org/camlooper/releases)
2. **Install** the application on your system
3. **Upload** your video file using drag-and-drop
4. **Configure** loop settings and quality options
5. **Activate** the virtual camera
6. **Select** "CamLooper Virtual Camera" in your video conferencing app

## 📥 Installation

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

### Download & Install

Grab the installer for your platform from the
[releases page](https://github.com/amitmtrn/camlooper-app/releases).

> ⚠️ **Unsigned builds:** the published installers are not yet code-signed. Windows
> SmartScreen and macOS Gatekeeper will warn on first launch — this is expected. On
> **macOS the virtual-camera System Extension will not load until the app is signed and
> notarized** with an Apple Developer certificate (see [Building & Releasing](#-building--releasing));
> until then the app runs but the virtual camera won't register on macOS.

#### Windows
1. Download the `.exe` (NSIS) or `.msi` installer.
2. Run it as administrator (SmartScreen → *More info* → *Run anyway*).
3. Virtual camera drivers are installed automatically.

#### macOS
1. Download the `.dmg` matching your chip — **Apple Silicon** (`aarch64`) or **Intel** (`x86_64`).
2. Open the DMG and drag CamLooper to Applications.
3. First launch: right-click → *Open* to bypass Gatekeeper, then grant camera permission.

#### Linux
**For the working virtual camera, use the `.deb`/`.rpm`** (not the AppImage). They pull in
`v4l2loopback-dkms` + `ffmpeg`, install the module config, and **auto-load `v4l2loopback`**
(on install and every boot) so the "CamLooper Virtual Camera" device is ready with no manual
setup:
```bash
sudo apt install ./camlooper_*.deb     # Debian/Ubuntu
sudo dnf install ./camlooper-*.rpm      # Fedora/RHEL
```

The **AppImage** is portable but cannot install a kernel module, so the virtual camera only
works if the host already has `v4l2loopback` loaded:
```bash
wget https://github.com/amitmtrn/camlooper-app/releases/latest/download/camlooper_x86_64.AppImage
chmod +x camlooper_*.AppImage
./camlooper_*.AppImage
# one-time host setup for the AppImage:
sudo apt install v4l2loopback-dkms && sudo modprobe v4l2loopback
```

### 🏗 Building & Releasing

Because of native dependencies (FFmpeg, the Windows DirectShow filter, the macOS System
Extension), the app **cannot be cross-compiled** — each OS builds on its own machine.
CI does this for you: pushing a `v*` tag runs
[`.github/workflows/build-cross-platform.yml`](.github/workflows/build-cross-platform.yml),
which builds Windows, macOS (Intel + Apple Silicon), and Linux on native runners via
[`tauri-action`](https://github.com/tauri-apps/tauri-action) and attaches all installers
to a **draft GitHub Release**.

```bash
git tag v0.1.1
git push origin v0.1.1     # -> draft Release with .exe/.msi, two .dmg, .AppImage/.deb/.rpm
```

**Code signing** (optional, unblocks warning-free installs + the macOS virtual camera) is
wired in — add these repository *Secrets* and the workflow uses them automatically:
`APPLE_CERTIFICATE`, `APPLE_CERTIFICATE_PASSWORD`, `APPLE_SIGNING_IDENTITY`, `APPLE_ID`,
`APPLE_PASSWORD`, `APPLE_TEAM_ID` (macOS); `WINDOWS_CERTIFICATE`,
`WINDOWS_CERTIFICATE_PASSWORD` (Windows).

To build locally for **your current OS only**: `npm ci` then `npm run build:linux` /
`npm run build:macos` (or `build:macos:intel`) / `npm run build:windows:msvc`.

## 🎯 Use Cases

### Professional Video Calls
- Create professional backgrounds for work meetings
- Use pre-recorded presentations in calls
- Maintain consistent branding across meetings

### Content Creation & Streaming
- Loop intro/outro videos in live streams
- Use pre-recorded content with OBS Studio
- Create consistent visual elements for content

### Education & Training
- Use educational videos as visual aids in online classes
- Loop demonstration videos during presentations
- Provide consistent video content for training sessions

### Software Development
- Test applications with consistent video input
- Create reproducible test scenarios
- Demonstrate software with controlled video content

## 🖥️ Platform Support

### Full Support
- ✅ **Linux**: Complete functionality with FFmpeg and v4l2loopback
- ✅ **macOS**: Complete functionality with FFmpeg and AVFoundation

### Limited Support
- ⚠️ **Windows**: Basic functionality with limitations
  - Video upload works but with limited metadata extraction
  - Streaming shows generated frames instead of actual video
  - See [Windows Limitations](docs/WINDOWS_LIMITATIONS.md) for details and solutions

## 🛠 Technology Stack

CamLooper is built with modern, high-performance technologies:

- **Frontend**: React 18, TypeScript, Tailwind CSS, shadcn/ui
- **Backend**: Rust, Tauri, FFmpeg
- **Build System**: Vite, Cargo
- **Cross-Platform**: Native Windows, macOS, and Linux support

## 📚 Documentation

Comprehensive documentation is available in the [`docs/`](docs/) directory:

- **[User Guide](docs/USER_GUIDE.md)** - Complete user manual with step-by-step instructions
- **[API Documentation](docs/API.md)** - Complete API reference for developers
- **[Architecture](docs/ARCHITECTURE.md)** - Technical architecture and system design
- **[Development Guide](docs/DEVELOPMENT.md)** - Setup and development workflow
- **[Troubleshooting](docs/TROUBLESHOOTING.md)** - Common issues and solutions
- **[Contributing](docs/CONTRIBUTING.md)** - How to contribute to the project

## 🔧 Development

### Prerequisites
- Node.js 18+ with npm
- Rust 1.70+ with Cargo
- FFmpeg development libraries

### Setup
```bash
# Clone the repository
git clone https://github.com/your-org/camlooper.git
cd camlooper

# Install dependencies
npm install

# Start development server
npm run tauri:dev
```

### Building
```bash
# Build for production
npm run tauri:build

# Build for specific platforms
npm run build:windows
npm run build:macos
npm run build:linux
```

For detailed development instructions, see our [Development Guide](docs/DEVELOPMENT.md).

## 🤝 Contributing

We welcome contributions from the community! Whether you're fixing bugs, adding features, improving documentation, or helping other users, your contributions make CamLooper better for everyone.

### Ways to Contribute
- 🐛 Report bugs and issues
- 💡 Suggest new features
- 🔧 Submit pull requests
- 📖 Improve documentation
- 💬 Help other users in discussions

Please read our [Contributing Guide](docs/CONTRIBUTING.md) for detailed information on how to get started.

## 📄 License

CamLooper is released under the [MIT License](LICENSE). This means you can use, modify, and distribute the software freely, including for commercial purposes.

## 🙏 Acknowledgments

CamLooper is built on top of amazing open-source technologies:

- [Tauri](https://tauri.app/) - For the cross-platform application framework
- [FFmpeg](https://ffmpeg.org/) - For video processing capabilities
- [React](https://reactjs.org/) - For the user interface
- [Rust](https://www.rust-lang.org/) - For high-performance backend processing

## 📞 Support

### Getting Help
- 📖 **Documentation**: Start with our comprehensive [documentation](docs/)
- 🐛 **Issues**: Report bugs on [GitHub Issues](https://github.com/your-org/camlooper/issues)
- 💬 **Discussions**: Ask questions in [GitHub Discussions](https://github.com/your-org/camlooper/discussions)
- 🔧 **Troubleshooting**: Check our [troubleshooting guide](docs/TROUBLESHOOTING.md)

### Community
Join our growing community of content creators, developers, and enthusiasts:
- Share your use cases and creative applications
- Help other users with questions and issues
- Contribute to the project's development
- Suggest new features and improvements

---

<div align="center">
  <p>Made with ❤️ by the CamLooper community</p>
  <p>
    <a href="https://github.com/your-org/camlooper/stargazers">⭐ Star us on GitHub</a> •
    <a href="https://github.com/your-org/camlooper/releases">📥 Download Latest</a> •
    <a href="docs/">📚 Read Docs</a>
  </p>
</div>
