# CamLooper API Documentation

## Table of Contents
- [Overview](#overview)
- [Data Types](#data-types)
- [Virtual Camera API](#virtual-camera-api)
- [Video Upload API](#video-upload-api)
- [Video Processing API](#video-processing-api)
- [Event System](#event-system)
- [Error Handling](#error-handling)
- [Usage Examples](#usage-examples)

## Overview

CamLooper provides a comprehensive API through Tauri commands that bridge the React frontend with the Rust backend. All commands are asynchronous and return `Promise<T>` objects that can be awaited or used with `.then()/.catch()`.

### Invoking Commands

```typescript
import { invoke } from '@tauri-apps/api/core';

// Basic command invocation
const result = await invoke<ReturnType>('command_name', {
  parameter1: value1,
  parameter2: value2
});
```

## Data Types

### Core Types

```typescript
interface VideoInfo {
  id: string;
  filename: string;
  duration: number;
  width: number;
  height: number;
  fps: number;
  format: string;
}

interface VideoFrame {
  data: number[]; // Raw JPEG bytes
  timestamp: number;
  width: number;
  height: number;
}

interface StreamStatus {
  is_playing: boolean;
  current_time: number;
  duration: number;
  loop_count: number;
  current_loop: number;
  buffer_health: number; // 0.0 to 1.0
  actual_fps: number;
  target_fps: number;
  frames_dropped: number;
  average_processing_time: number;
  is_preparing: boolean;   // natural motion is buffering the clip; no frames out yet
  walk_truncated: boolean; // the clip outran the buffer, so the walk covers only its start
  walk_seconds: number;    // seconds of footage the walk covers; 0 outside natural motion
}

interface PerformanceMetrics {
  frames_processed: number;
  frames_dropped: number;
  average_encode_time: number;
  average_decode_time: number;
  current_quality: number; // 1-100
  buffer_size: number;
  memory_usage: number;
}
```

### Virtual Camera Types

```typescript
interface VirtualCameraConfig {
  width: number;
  height: number;
  fps: number;
  camera_name: string;
}

interface VirtualCameraStatus {
  is_active: boolean;
  camera_name: string;
  resolution: string;
  fps: number;
  frame_count: number;
}
```

### Upload Types

```typescript
interface StreamUploadResponse {
  upload_id: string;
  temp_file_path: string;
  chunk_size: number;
  expected_chunks: number;
}

interface ChunkUploadRequest {
  upload_id: string;
  chunk_index: number;
  chunk_data: number[]; // Raw binary data
  is_final_chunk: boolean;
}

interface ChunkUploadResponse {
  upload_id: string;
  chunk_index: number;
  bytes_written: number;
  total_bytes_written: number;
  is_complete: boolean;
  file_path?: string;
}
```

## Virtual Camera API

### start_virtual_camera

Starts the virtual camera with the specified configuration. All frames sent to the virtual camera will be automatically resized to match the configured resolution.

```typescript
async function start_virtual_camera(
  config?: VirtualCameraConfig
): Promise<VirtualCameraStatus>
```

**Parameters:**
- `config` (optional): Virtual camera configuration. Defaults to 1920x1080@30fps.

**Configuration Options:**
- `width`: Virtual camera output width in pixels
- `height`: Virtual camera output height in pixels  
- `fps`: Target frame rate for the virtual camera
- `camera_name`: Display name for the virtual camera device

**Frame Processing:**
- All input frames are automatically resized to match the configured resolution
- High-quality bilinear interpolation ensures smooth scaling
- Consistent output format across all supported platforms

**Example:**
```typescript
const config = {
  width: 1920,
  height: 1080,
  fps: 30,
  camera_name: "CamLooper Virtual Camera"
};

const status = await invoke<VirtualCameraStatus>('start_virtual_camera', { config });
console.log(`Virtual camera started: ${status.camera_name}`);
```

### stop_virtual_camera

Stops the currently active virtual camera.

```typescript
async function stop_virtual_camera(): Promise<VirtualCameraStatus>
```

**Example:**
```typescript
const status = await invoke<VirtualCameraStatus>('stop_virtual_camera');
console.log(`Virtual camera stopped: ${status.camera_name}`);
```

### get_virtual_camera_status

Retrieves the current status of the virtual camera.

```typescript
async function get_virtual_camera_status(): Promise<VirtualCameraStatus>
```

**Example:**
```typescript
const status = await invoke<VirtualCameraStatus>('get_virtual_camera_status');
if (status.is_active) {
  console.log(`Camera active: ${status.resolution} @ ${status.fps}fps`);
}
```

### send_frame_to_virtual_camera

Sends a frame to the virtual camera for display. Frames are automatically resized to match the configured virtual camera resolution using high-quality bilinear interpolation.

```typescript
async function send_frame_to_virtual_camera(
  frame_data: string
): Promise<void>
```

**Parameters:**
- `frame_data`: Base64-encoded JPEG frame data (will be automatically resized to match virtual camera configuration)

**Frame Processing:**
- **Automatic Resizing**: Frames are resized to match the configured virtual camera resolution
- **High-Quality Scaling**: Uses bilinear interpolation for smooth scaling
- **Format Handling**: Accepts JPEG input and converts to RGB for virtual camera output
- **Cross-Platform**: Consistent behavior across Windows, Linux, and macOS

**Example:**
```typescript
// Forward current frame to virtual camera
if (currentFrame && isVirtualCamActive) {
  await invoke('send_frame_to_virtual_camera', { 
    frameData: btoa(String.fromCharCode(...currentFrame.data))
  });
}
```

### list_video_devices

Lists available video devices on the system (Linux only).

```typescript
async function list_video_devices(): Promise<string[]>
```

**Example:**
```typescript
const devices = await invoke<string[]>('list_video_devices');
devices.forEach(device => console.log(device));
```

## Video Upload API

### Modern Streaming Upload (Recommended)

#### stream_upload_and_load_video

Uploads and loads a video file in one efficient operation using raw binary data.

```typescript
async function stream_upload_and_load_video(
  filename: string, 
  file_data: number[]
): Promise<VideoInfo>
```

**Parameters:**
- `filename`: Name of the video file
- `file_data`: Raw binary data as array of bytes

**Example:**
```typescript
const fileInput = document.querySelector('input[type="file"]');
const file = fileInput.files[0];

const arrayBuffer = await file.arrayBuffer();
const uint8Array = new Uint8Array(arrayBuffer);
const fileData = Array.from(uint8Array);

const videoInfo = await invoke<VideoInfo>('stream_upload_and_load_video', {
  filename: file.name,
  fileData
});

console.log(`Video loaded: ${videoInfo.duration}s, ${videoInfo.width}x${videoInfo.height}`);
```

#### start_stream_upload

Initializes a chunked upload session for large files.

```typescript
async function start_stream_upload(
  filename: string,
  file_size: number,
  chunk_size: number
): Promise<StreamUploadResponse>
```

#### upload_chunk_stream

Uploads a single chunk of data to an active upload session.

```typescript
async function upload_chunk_stream(
  request: ChunkUploadRequest
): Promise<ChunkUploadResponse>
```

### Legacy Upload (Base64)

#### upload_and_load_video

Legacy method using base64 encoding (less efficient for large files).

```typescript
async function upload_and_load_video(
  filename: string,
  file_data: string
): Promise<VideoInfo>
```

**Example:**
```typescript
const file = fileInput.files[0];
const reader = new FileReader();

reader.onload = async () => {
  const base64Data = btoa(reader.result as string);
  const videoInfo = await invoke<VideoInfo>('upload_and_load_video', {
    filename: file.name,
    fileData: base64Data
  });
};

reader.readAsBinaryString(file);
```

## Video Processing API

### load_video_file

Loads a video file from a local file path.

```typescript
async function load_video_file(file_path: string): Promise<VideoInfo>
```

**Example:**
```typescript
const videoInfo = await invoke<VideoInfo>('load_video_file', {
  filePath: '/path/to/video.mp4'
});
```

### start_video_stream

Starts streaming video frames from the loaded video.

```typescript
async function start_video_stream(): Promise<void>
```

**Example:**
```typescript
await invoke('start_video_stream');
setIsPlaying(true);

// Listen for frames
await listen<VideoFrame>('video-frame', (event) => {
  setCurrentFrame(event.payload);
});
```

### pause_video_stream

Pauses the video stream while maintaining the current position.

```typescript
async function pause_video_stream(): Promise<void>
```

**Example:**
```typescript
await invoke('pause_video_stream');
setIsPlaying(false);
```

### stop_video_stream

Stops the video stream and resets to the beginning.

```typescript
async function stop_video_stream(): Promise<void>
```

**Example:**
```typescript
await invoke('stop_video_stream');
setIsPlaying(false);
setCurrentFrame(null);
```

### get_video_stream_status

Retrieves the current playback status and statistics.

```typescript
async function get_video_stream_status(): Promise<StreamStatus>
```

**Example:**
```typescript
const status = await invoke<StreamStatus>('get_video_stream_status');
console.log(`Playing: ${status.is_playing}, Loop: ${status.current_loop}/${status.loop_count}`);
```

### set_video_loop_settings

Configures video looping behavior.

```typescript
async function set_video_loop_settings(
  loop_count: number,
  auto_start: boolean,
  natural_motion: boolean
): Promise<void>
```

**Parameters:**
- `loop_count`: Number of loops (1-10, or 10 for infinite)
- `auto_start`: Whether to start automatically after loading
- `natural_motion`: Walk the frame index at random instead of replaying the clip in
  order (default `true`). See below.

**Example:**
```typescript
await invoke('set_video_loop_settings', {
  loopCount: 5,
  autoStart: true,
  naturalMotion: true
});
```

**Natural motion**

A straight loop plays `1 2 3 … N, 1 2 3 …`, which cuts at the seam every pass and repeats
on a period equal to the clip length. Natural motion buffers the clip and walks its frame
index instead — `1 2 3 2 1 2 1 2 3 4 3 …` — stepping one frame at a time so motion stays
continuous, and reversing direction at random so there is no seam and no repeat.
Implemented in `src-tauri/src/frame_walk.rs`.

Two consequences for callers:

- Playback cannot start until the clip is buffered. `StreamStatus.is_preparing` is true
  for that gap; the buffer is then cached, so pause/resume on the same clip is instant.
- The buffer is capped at 60 seconds or 96 MB, whichever comes first. Past that,
  `walk_truncated` is set and the walk covers only `walk_seconds` of the clip.

Settings are read when streaming starts, so toggling this mid-stream applies on the next
start. With natural motion on there is no pass to count, so `current_loop` reports elapsed
playback in clip-lengths — `loop_count` still means "N clip durations", and `0` still means
forever.

### get_video_info

Returns information about the currently loaded video.

```typescript
async function get_video_info(): Promise<VideoInfo | null>
```

**Example:**
```typescript
const videoInfo = await invoke<VideoInfo | null>('get_video_info');
if (videoInfo) {
  console.log(`Video: ${videoInfo.filename}, Duration: ${videoInfo.duration}s`);
}
```

### get_performance_metrics

Retrieves detailed performance metrics.

```typescript
async function get_performance_metrics(): Promise<PerformanceMetrics>
```

**Example:**
```typescript
const metrics = await invoke<PerformanceMetrics>('get_performance_metrics');
console.log(`FPS: ${metrics.actual_fps}, Dropped: ${metrics.frames_dropped}`);
```

## Event System

CamLooper uses a push-based event system for real-time frame delivery.

### video-frame Event

Emitted for each processed video frame during playback.

```typescript
import { listen } from '@tauri-apps/api/event';

await listen<VideoFrame>('video-frame', (event) => {
  const frame = event.payload;
  setCurrentFrame(frame);
  
  // Forward to virtual camera if active
  if (isVirtualCamActive) {
    invoke('send_frame_to_virtual_camera', { 
      frameData: btoa(String.fromCharCode(...frame.data))
    });
  }
});
```

## Error Handling

All API commands return `Promise<T>` and may throw errors that should be handled appropriately.

### Common Error Types

- **File System Errors**: Invalid paths, permissions, disk space
- **Video Processing Errors**: Unsupported formats, codec issues
- **Virtual Camera Errors**: Device conflicts, driver issues
- **Upload Errors**: Network issues, invalid data

### Error Handling Examples

```typescript
try {
  const videoInfo = await invoke<VideoInfo>('upload_and_load_video', {
    filename: file.name,
    fileData: base64Data
  });
  console.log('Video loaded successfully:', videoInfo);
} catch (error) {
  console.error('Failed to load video:', error);
  // Handle specific error types
  if (error.includes('Unsupported video format')) {
    showFormatError();
  } else if (error.includes('Failed to decode')) {
    showCorruptionError();
  }
}
```

## Usage Examples

### Complete Video Upload and Playback

```typescript
import { invoke, listen } from '@tauri-apps/api';

class VideoManager {
  private currentFrame: VideoFrame | null = null;
  private isPlaying = false;
  private isVirtualCamActive = false;

  async uploadVideo(file: File) {
    try {
      // Use modern streaming upload
      const arrayBuffer = await file.arrayBuffer();
      const fileData = Array.from(new Uint8Array(arrayBuffer));
      
      const videoInfo = await invoke<VideoInfo>('stream_upload_and_load_video', {
        filename: file.name,
        fileData
      });
      
      console.log('Video uploaded:', videoInfo);
      return videoInfo;
    } catch (error) {
      console.error('Upload failed:', error);
      throw error;
    }
  }

  async startPlayback() {
    try {
      // Set up frame listener
      await listen<VideoFrame>('video-frame', (event) => {
        this.currentFrame = event.payload;
        this.displayFrame(event.payload);
      });

      // Start streaming
      await invoke('start_video_stream');
      this.isPlaying = true;
    } catch (error) {
      console.error('Playback failed:', error);
    }
  }

  async startVirtualCamera() {
    try {
      const status = await invoke<VirtualCameraStatus>('start_virtual_camera');
      this.isVirtualCamActive = status.is_active;
      console.log('Virtual camera started:', status);
    } catch (error) {
      console.error('Virtual camera failed:', error);
    }
  }

  private displayFrame(frame: VideoFrame) {
    // Display frame in UI
    const imageData = `data:image/jpeg;base64,${btoa(String.fromCharCode(...frame.data))}`;
    const imgElement = document.querySelector('#video-display') as HTMLImageElement;
    imgElement.src = imageData;

    // Forward to virtual camera
    if (this.isVirtualCamActive) {
      invoke('send_frame_to_virtual_camera', {
        frameData: btoa(String.fromCharCode(...frame.data))
      }).catch(console.error);
    }
  }
}
```

### Performance Monitoring

```typescript
class PerformanceMonitor {
  private updateInterval: number;

  start() {
    this.updateInterval = setInterval(async () => {
      try {
        const metrics = await invoke<PerformanceMetrics>('get_performance_metrics');
        const status = await invoke<StreamStatus>('get_video_stream_status');
        
        this.updateUI(metrics, status);
      } catch (error) {
        console.error('Failed to get metrics:', error);
      }
    }, 1000); // Update every second
  }

  stop() {
    if (this.updateInterval) {
      clearInterval(this.updateInterval);
    }
  }

  private updateUI(metrics: PerformanceMetrics, status: StreamStatus) {
    document.querySelector('#fps-display').textContent = 
      `${status.actual_fps.toFixed(1)} FPS`;
    document.querySelector('#buffer-health').textContent = 
      `${(status.buffer_health * 100).toFixed(0)}%`;
    document.querySelector('#frames-dropped').textContent = 
      `${metrics.frames_dropped} dropped`;
  }
}
```

---

This API documentation provides comprehensive coverage of all CamLooper functionality. For additional examples and integration patterns, see the [User Guide](USER_GUIDE.md) and [Development Guide](DEVELOPMENT.md). 