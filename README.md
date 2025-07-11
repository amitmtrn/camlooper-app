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

#### Windows
1. Download the `.msi` installer from [releases](https://github.com/your-org/camlooper/releases)
2. Run the installer as administrator
3. Virtual camera drivers will be installed automatically

#### macOS
1. Download the `.dmg` file from [releases](https://github.com/your-org/camlooper/releases)
2. Drag CamLooper to Applications folder
3. Grant necessary permissions when prompted

#### Linux
```bash
# Download AppImage
wget https://github.com/your-org/camlooper/releases/latest/download/CamLooper.AppImage
chmod +x CamLooper.AppImage

# Install v4l2loopback for virtual camera support
sudo apt install v4l2loopback-dkms
sudo modprobe v4l2loopback
```

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
