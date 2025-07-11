# Contributing to CamLooper

Thank you for your interest in contributing to CamLooper! This document provides guidelines and information for contributors.

## Table of Contents
- [Code of Conduct](#code-of-conduct)
- [Getting Started](#getting-started)
- [Development Setup](#development-setup)
- [Contribution Workflow](#contribution-workflow)
- [Coding Standards](#coding-standards)
- [Testing Guidelines](#testing-guidelines)
- [Documentation](#documentation)
- [Issue Reporting](#issue-reporting)
- [Feature Requests](#feature-requests)
- [Community](#community)

## Code of Conduct

### Our Pledge
We pledge to make participation in CamLooper a harassment-free experience for everyone, regardless of age, body size, disability, ethnicity, gender identity, level of experience, nationality, personal appearance, race, religion, or sexual identity and orientation.

### Expected Behavior
- Use welcoming and inclusive language
- Be respectful of differing viewpoints and experiences
- Gracefully accept constructive criticism
- Focus on what is best for the community
- Show empathy towards other community members

### Unacceptable Behavior
- The use of sexualized language or imagery
- Trolling, insulting/derogatory comments, and personal or political attacks
- Public or private harassment
- Publishing others' private information without explicit permission
- Other conduct which could reasonably be considered inappropriate

## Getting Started

### Ways to Contribute
1. **Bug Reports**: Help us identify and fix issues
2. **Feature Suggestions**: Propose new functionality
3. **Code Contributions**: Submit pull requests for fixes and features
4. **Documentation**: Improve or expand documentation
5. **Testing**: Test new features and report issues
6. **Community Support**: Help other users in discussions

### First Contribution
Looking for a first contribution? Look for issues labeled:
- `good first issue`: Easy issues for newcomers
- `help wanted`: Issues where we need community help
- `documentation`: Documentation improvements
- `bug`: Bug fixes

## Development Setup

### Prerequisites
Before contributing, ensure you have:
- **Node.js** 18+ with npm
- **Rust** 1.70+ with Cargo
- **Git** for version control
- **FFmpeg** development libraries

### Setup Instructions
1. **Fork the repository** on GitHub
2. **Clone your fork**:
   ```bash
   git clone https://github.com/your-username/camlooper.git
   cd camlooper
   ```

3. **Install dependencies**:
   ```bash
   npm install
   ```

4. **Set up development environment**:
   ```bash
   # Copy environment template
   cp .env.example .env
   
   # Install platform-specific dependencies
   # See DEVELOPMENT.md for detailed instructions
   ```

5. **Verify setup**:
   ```bash
   npm run tauri:dev
   ```

## Contribution Workflow

### 1. Create a Branch
```bash
# Create and switch to a new branch
git checkout -b feature/your-feature-name

# Or for bug fixes
git checkout -b bugfix/issue-description
```

### 2. Make Changes
- Follow our [coding standards](#coding-standards)
- Write or update tests as needed
- Update documentation if necessary
- Ensure your changes work across platforms

### 3. Commit Changes
Use conventional commit messages:
```bash
# Format: type(scope): description
git commit -m "feat(video): add H.265 codec support"
git commit -m "fix(upload): resolve memory leak in large files"
git commit -m "docs(api): update virtual camera documentation"
```

#### Commit Types
- `feat`: New feature
- `fix`: Bug fix
- `docs`: Documentation changes
- `style`: Code style changes (no logic changes)
- `refactor`: Code refactoring
- `test`: Adding or updating tests
- `chore`: Maintenance tasks

### 4. Test Your Changes
```bash
# Run all tests
npm run test
cargo test

# Run linting
npm run lint
cargo clippy

# Test on multiple platforms if possible
```

### 5. Push and Create PR
```bash
# Push your branch
git push origin feature/your-feature-name

# Create pull request on GitHub
# Fill out the PR template completely
```

## Coding Standards

### TypeScript/React Standards

#### Code Style
```typescript
// Use TypeScript for all new code
interface VideoPlayerProps {
  videoInfo: VideoInfo;
  onPlay: () => void;
}

// Use functional components with hooks
const VideoPlayer: React.FC<VideoPlayerProps> = ({ videoInfo, onPlay }) => {
  const [isPlaying, setIsPlaying] = useState(false);
  
  // Use descriptive variable names
  const handlePlayButtonClick = useCallback(() => {
    setIsPlaying(!isPlaying);
    onPlay();
  }, [isPlaying, onPlay]);

  return (
    <div className="video-player">
      {/* JSX content */}
    </div>
  );
};

export default VideoPlayer;
```

#### React Best Practices
- Use functional components and hooks
- Implement proper error boundaries
- Optimize with React.memo when appropriate
- Use proper dependency arrays in useEffect
- Handle loading and error states

#### State Management
```typescript
// Use local state for component-specific data
const [localState, setLocalState] = useState<StateType>(initialState);

// Use context for shared state
const { globalState, dispatch } = useContext(AppContext);

// Use custom hooks for complex logic
const { uploadVideo, isUploading, error } = useVideoUpload();
```

### Rust Standards

#### Code Style
```rust
// Use snake_case for functions and variables
fn process_video_frame(frame_data: &[u8]) -> Result<VideoFrame> {
    let processed_frame = decode_frame(frame_data)?;
    Ok(processed_frame)
}

// Use PascalCase for types
struct VideoProcessor {
    frame_buffer: Vec<VideoFrame>,
    is_processing: Arc<AtomicBool>,
}

// Use SCREAMING_SNAKE_CASE for constants
const MAX_BUFFER_SIZE: usize = 1000;
const DEFAULT_QUALITY: u8 = 80;
```

#### Error Handling
```rust
use anyhow::{anyhow, Result};

// Use Result<T, E> for fallible operations
fn load_video(path: &Path) -> Result<VideoInfo> {
    let file = std::fs::File::open(path)
        .map_err(|e| anyhow!("Failed to open video file: {}", e))?;
    
    // Process file...
    Ok(video_info)
}

// Use proper error propagation
#[tauri::command]
async fn upload_video(filename: String, data: Vec<u8>) -> Result<VideoInfo, String> {
    let video_info = video_processor::load_video_data(filename, data)
        .await
        .map_err(|e| e.to_string())?;
    
    Ok(video_info)
}
```

#### Async/Threading
```rust
// Use tokio for async operations
#[tokio::main]
async fn main() -> Result<()> {
    let result = async_operation().await?;
    Ok(())
}

// Use Arc<Mutex<T>> for shared state
struct SharedState {
    data: Arc<Mutex<HashMap<String, String>>>,
}

// Use channels for communication
let (sender, receiver) = mpsc::unbounded_channel();
```

### Documentation Standards

#### Code Comments
```typescript
/**
 * Uploads a video file and processes it for virtual camera use
 * @param file - The video file to upload
 * @param options - Upload configuration options
 * @returns Promise resolving to video information
 * @throws Error if upload fails or file is invalid
 */
async function uploadVideo(file: File, options: UploadOptions): Promise<VideoInfo> {
  // Implementation
}
```

```rust
/// Processes a video frame for streaming
/// 
/// # Arguments
/// 
/// * `frame_data` - Raw frame data bytes
/// * `quality` - JPEG quality setting (1-100)
/// 
/// # Returns
/// 
/// Processed frame ready for streaming
/// 
/// # Errors
/// 
/// Returns error if frame processing fails
fn process_frame(frame_data: &[u8], quality: u8) -> Result<VideoFrame> {
    // Implementation
}
```

## Testing Guidelines

### Frontend Testing
```typescript
// Component tests
import { render, screen, fireEvent } from '@testing-library/react';
import { VideoPlayer } from '../VideoPlayer';

describe('VideoPlayer', () => {
  it('should play video when play button is clicked', async () => {
    const mockOnPlay = jest.fn();
    render(<VideoPlayer videoInfo={mockVideoInfo} onPlay={mockOnPlay} />);
    
    const playButton = screen.getByRole('button', { name: /play/i });
    fireEvent.click(playButton);
    
    expect(mockOnPlay).toHaveBeenCalled();
  });
});

// Integration tests
import { invoke } from '@tauri-apps/api/core';

describe('Video Upload Integration', () => {
  it('should upload and process video successfully', async () => {
    const mockVideoData = new Uint8Array([/* mock data */]);
    
    const result = await invoke('upload_and_load_video', {
      filename: 'test.mp4',
      fileData: Array.from(mockVideoData)
    });
    
    expect(result).toHaveProperty('duration');
    expect(result).toHaveProperty('width');
  });
});
```

### Backend Testing
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[tokio::test]
    async fn test_video_processing() {
        let mut processor = VideoProcessor::new();
        let test_video = PathBuf::from("tests/fixtures/sample.mp4");
        
        let result = processor.load_video(test_video).await;
        assert!(result.is_ok());
        
        let video_info = result.unwrap();
        assert_eq!(video_info.width, 1920);
        assert_eq!(video_info.height, 1080);
        assert!(video_info.duration > 0.0);
    }

    #[test]
    fn test_error_handling() {
        let result = load_invalid_video();
        assert!(result.is_err());
        
        let error = result.unwrap_err();
        assert!(error.to_string().contains("Failed to open"));
    }
}
```

### Test Coverage
- Aim for >80% test coverage on new code
- Test both success and error cases
- Include integration tests for critical paths
- Test platform-specific functionality

## Documentation

### Types of Documentation

#### Code Documentation
- Inline comments for complex logic
- Function/method documentation
- Type definitions and interfaces
- Architecture decisions

#### User Documentation
- User guide updates for new features
- API documentation for new commands
- Troubleshooting guides for known issues
- Installation instructions

#### Developer Documentation
- Setup and build instructions
- Architecture documentation
- Contribution guidelines
- Release procedures

### Documentation Standards
1. **Clear and Concise**: Write for your audience's skill level
2. **Examples**: Include code examples and use cases
3. **Up-to-Date**: Update docs with code changes
4. **Searchable**: Use consistent terminology
5. **Accessible**: Consider different learning styles

## Issue Reporting

### Before Reporting
1. **Search existing issues** to avoid duplicates
2. **Check documentation** for known solutions
3. **Try latest version** to see if issue persists
4. **Gather debug information** (logs, system info)

### Bug Report Template
```markdown
**Describe the bug**
A clear description of what the bug is.

**To Reproduce**
Steps to reproduce the behavior:
1. Go to '...'
2. Click on '....'
3. See error

**Expected behavior**
What you expected to happen.

**Screenshots**
If applicable, add screenshots.

**Environment:**
- OS: [e.g. Windows 11, macOS 13.1]
- CamLooper Version: [e.g. 0.1.0]
- Hardware: [CPU, RAM, Graphics]

**Additional context**
Any other context about the problem.
```

### Quality Standards
- **Reproducible**: Provide clear steps to reproduce
- **Specific**: Include exact error messages
- **Complete**: Include all relevant information
- **Focused**: One issue per report

## Feature Requests

### Before Requesting
1. **Check existing requests** to avoid duplicates
2. **Consider alternatives** using current features
3. **Think about implementation** complexity
4. **Consider user impact** and use cases

### Feature Request Template
```markdown
**Is your feature request related to a problem?**
A clear description of what the problem is.

**Describe the solution you'd like**
A clear description of what you want to happen.

**Describe alternatives you've considered**
Other solutions or features you've considered.

**Use cases**
How would this feature be used?

**Additional context**
Mockups, examples, or other context.
```

### Evaluation Criteria
Features are evaluated on:
- **User Value**: How many users would benefit?
- **Complexity**: Implementation difficulty and maintenance burden
- **Compatibility**: Impact on existing functionality
- **Resources**: Development time and expertise required

## Community

### Communication Channels
- **GitHub Issues**: Bug reports and feature requests
- **GitHub Discussions**: Questions, ideas, and general discussion
- **Pull Requests**: Code review and collaboration

### Getting Help
1. **Documentation**: Check existing docs first
2. **Search**: Look for similar issues/discussions
3. **Ask Questions**: Use GitHub Discussions for questions
4. **Be Patient**: Maintainers are volunteers

### Helping Others
- Answer questions in discussions
- Review pull requests
- Test beta features
- Improve documentation
- Share your use cases and feedback

### Recognition
Contributors are recognized through:
- Contributor list in README
- Release notes mentions
- GitHub contributor statistics
- Special contributor badges

## Release Process

### Version Numbering
We follow [Semantic Versioning](https://semver.org/):
- **MAJOR**: Breaking changes
- **MINOR**: New features (backward compatible)
- **PATCH**: Bug fixes (backward compatible)

### Release Cycle
- **Patch releases**: As needed for critical bugs
- **Minor releases**: Monthly for new features
- **Major releases**: When significant breaking changes accumulate

### Contributing to Releases
- Test pre-release versions
- Report issues with release candidates
- Help with release documentation
- Verify builds on different platforms

---

Thank you for contributing to CamLooper! Your contributions help make virtual camera technology accessible to everyone. If you have questions about contributing, don't hesitate to ask in our [GitHub Discussions](../../discussions). 