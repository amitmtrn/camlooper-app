use anyhow::{anyhow, Result};
#[cfg(not(windows))]
use ffmpeg_next as ffmpeg;
use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tauri::AppHandle;
use tokio::sync::{Mutex, RwLock};
use uuid::Uuid;
use std::collections::VecDeque;
use std::process::Stdio;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::pipeline::{self, Geometry, PipelineSpec, PreviewSpec, Source};
use crate::preview::{JpegStreamReader, PreviewEncoder};

static FRAME_QUEUE: Lazy<Arc<Mutex<VecDeque<FrameBatch>>>> = Lazy::new(|| Arc::new(Mutex::new(VecDeque::new())));


#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VideoInfo {
    pub id: String,
    pub filename: String,
    pub duration: f64,
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    pub format: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VideoFrame {
    pub data: Vec<u8>, // Raw JPEG bytes
    pub timestamp: f64,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FrameBatch {
    pub frames: Vec<VideoFrame>,
    pub sequence_id: u64,
    pub total_frames: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamStatus {
    pub is_playing: bool,
    pub current_time: f64,
    pub duration: f64,
    pub loop_count: u32,
    pub current_loop: u32,
    pub buffer_health: f32, // 0.0 to 1.0
    pub actual_fps: f64,
    pub target_fps: f64,
    pub frames_dropped: u32,
    pub average_processing_time: f64,
    /// Natural motion decodes the clip into memory before it can start. True for that gap.
    pub is_preparing: bool,
    /// The clip outran the natural-motion window, so the walk only covers its start.
    pub walk_truncated: bool,
    /// Seconds of footage the walk is actually covering. 0 when not in natural motion.
    pub walk_seconds: f64,
}

/// Playback options pushed from the UI. A named struct rather than a tuple because the
/// fields are unrelated booleans and counts that are easy to transpose at a call site.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct LoopSettings {
    /// Number of passes to play; `0` means forever.
    pub loop_count: u32,
    /// Read by the frontend only -- auto-play is implemented in React, not here.
    pub auto_start: bool,
    /// Walk the frame index at random instead of playing a straight loop.
    pub natural_motion: bool,
}

impl Default for LoopSettings {
    fn default() -> Self {
        Self { loop_count: 10, auto_start: true, natural_motion: true }
    }
}

/// Seconds of video buffered for natural motion -- enough for the loop clips this app is
/// built around, and bounded so a long file cannot exhaust memory.
const WALK_WINDOW_SECS: f64 = 60.0;

/// Hard ceiling on the buffer, whichever limit is reached first.
///
/// The frame cap alone does not bound memory, because JPEG size depends on content and on
/// the store width, which follows the user's output resolution. Measured at 720p on a
/// deliberately detailed synthetic clip: the old `-q:v 5` yuvj420p store ran ~47 KB a frame,
/// and the `-q:v 2` yuvj422p store this now uses runs ~81 KB. Ordinary camera footage
/// compresses well below both.
///
/// 96 MiB was sized against the old, lower-quality store; at 81 KB a frame it would cut the
/// walk to ~40 s, and a short window makes the walk hover in one spot instead of covering
/// the clip (see `frame_walk::covers_the_clip_rather_than_hovering_in_one_spot`). 192 MiB
/// keeps the full 60 s window at 720p so [`WALK_WINDOW_SECS`] stays the binding limit, and
/// still bounds the one large allocation the app makes. What was actually kept is reported
/// to the UI as `walk_seconds`.
const MAX_WALK_BYTES: usize = 192 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PerformanceMetrics {
    pub frames_processed: u64,
    pub frames_dropped: u64,
    pub average_encode_time: f64,
    pub average_decode_time: f64,
    pub current_quality: u8,
    pub buffer_size: usize,
    pub memory_usage: u64,
}

// FFmpeg implementation for all platforms
pub struct VideoProcessor {
    video_info: Option<VideoInfo>,
    video_path: Option<PathBuf>,
    stream_status: Arc<RwLock<StreamStatus>>,
    app_handle: Option<AppHandle>,
    is_streaming: Arc<AtomicBool>,
    loop_settings: Arc<RwLock<LoopSettings>>,
    performance_metrics: Arc<RwLock<PerformanceMetrics>>,
    /// Frames decoded for natural motion, kept between streams so pause/resume on the same
    /// clip doesn't pay the decode wait again. Cleared when a different video is loaded.
    walk_cache: Arc<RwLock<Option<WalkBuffer>>>,
}

/// The natural-motion frame store: JPEGs, appended by the decoder and read at random by
/// the walk. Behind a `std::sync::Mutex` because it is touched once per tick for a length
/// and one `Arc` clone — both too short to be worth an async lock.
type SharedFrames = Arc<std::sync::Mutex<Vec<Arc<[u8]>>>>;

/// A clip decoded to JPEG frames, ready for random access.
struct WalkBuffer {
    source: PathBuf,
    /// Width the store was encoded at. Part of the cache key: the same clip decoded for a
    /// 720p camera is the wrong store for a 1080p one.
    width: u32,
    /// Shared so resuming a cached clip hands out a handle rather than copying ~70 MB.
    frames: SharedFrames,
    /// The source ran past `WALK_WINDOW_SECS`, so `frames` holds only its opening window.
    truncated: bool,
}

impl VideoProcessor {
    pub fn new() -> Self {
        #[cfg(not(windows))]
        {
            ffmpeg::init().ok();
        }
        
        Self {
            video_info: None,
            video_path: None,
            stream_status: Arc::new(RwLock::new(StreamStatus {
                is_playing: false,
                current_time: 0.0,
                duration: 0.0,
                loop_count: 10,
                current_loop: 0,
                buffer_health: 0.0,
                actual_fps: 0.0,
                target_fps: 30.0,
                frames_dropped: 0,
                average_processing_time: 0.0,
                is_preparing: false,
                walk_truncated: false,
                walk_seconds: 0.0,
            })),
            app_handle: None,
            is_streaming: Arc::new(AtomicBool::new(false)),
            loop_settings: Arc::new(RwLock::new(LoopSettings::default())),
            performance_metrics: Arc::new(RwLock::new(PerformanceMetrics {
                frames_processed: 0,
                frames_dropped: 0,
                average_encode_time: 0.0,
                average_decode_time: 0.0,
                current_quality: 80,
                buffer_size: 0,
                memory_usage: 0,
            })),
            walk_cache: Arc::new(RwLock::new(None)),
        }
    }

    pub fn set_app_handle(&mut self, app_handle: AppHandle) {
        self.app_handle = Some(app_handle);
    }

    pub async fn load_video(&mut self, file_path: PathBuf) -> Result<VideoInfo> {
        // A recording can be written back to a path we've already buffered, so drop the
        // cache on load rather than trusting the path alone to spot new content.
        *self.walk_cache.write().await = None;

        #[cfg(not(windows))]
        {
            // FFmpeg implementation for all platforms except Windows
            let video_info = {
                let input = ffmpeg::format::input(&file_path)?;
                let video_stream = input
                    .streams()
                    .best(ffmpeg::media::Type::Video)
                    .ok_or_else(|| anyhow!("No video stream found"))?;
                let codec_context = ffmpeg::codec::context::Context::from_parameters(video_stream.parameters())?;
                let decoder = codec_context.decoder().video()?;
                
                let duration = if video_stream.duration() != ffmpeg::ffi::AV_NOPTS_VALUE {
                    video_stream.duration() as f64 * f64::from(video_stream.time_base())
                } else {
                    input.duration() as f64 / f64::from(ffmpeg::ffi::AV_TIME_BASE)
                };

                let fps = f64::from(video_stream.avg_frame_rate());
                
                VideoInfo {
                    id: Uuid::new_v4().to_string(),
                    filename: file_path
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or("unknown")
                        .to_string(),
                    duration,
                    width: decoder.width(),
                    height: decoder.height(),
                    fps,
                    format: format!("{:?}", decoder.format()),
                }
            };

            self.video_info = Some(video_info.clone());
            self.video_path = Some(file_path);

            // Update status
            {
                let mut status = self.stream_status.write().await;
                status.duration = video_info.duration;
                status.target_fps = video_info.fps.max(30.0).min(60.0);
            }

            Ok(video_info)
        }
        
        #[cfg(windows)]
        {
            // Windows-specific implementation without FFmpeg
            // Enhanced version with better video handling
            use std::fs;
            use std::io::Read;
            
            // Check if file exists
            if !file_path.exists() {
                return Err(anyhow!("Video file not found: {:?}", file_path));
            }
            
            // Get file metadata
            let metadata = fs::metadata(&file_path)?;
            let file_size = metadata.len();
            
            // Basic file validation
            let file_extension = file_path
                .extension()
                .and_then(|ext| ext.to_str())
                .unwrap_or("")
                .to_lowercase();
            
            if !matches!(file_extension.as_str(), "mp4" | "mov" | "avi" | "mkv" | "webm" | "flv" | "wmv" | "m4v") {
                return Err(anyhow!("Unsupported video format: {}", file_extension));
            }
            
            // Try to read basic file header for validation
            let mut file = fs::File::open(&file_path)?;
            let mut header = [0u8; 12];
            file.read_exact(&mut header).map_err(|e| anyhow!("Failed to read file header: {}", e))?;
            
            // Basic video duration estimation based on file size
            // This is a rough approximation - larger files generally mean longer videos
            let estimated_duration = match file_size {
                0..=1_000_000 => 5.0,           // < 1MB: ~5 seconds
                1_000_001..=10_000_000 => 15.0, // 1-10MB: ~15 seconds
                10_000_001..=50_000_000 => 60.0, // 10-50MB: ~1 minute
                50_000_001..=200_000_000 => 300.0, // 50-200MB: ~5 minutes
                _ => 600.0,                      // > 200MB: ~10 minutes
            };
            
            // Basic resolution estimation based on file size and format
            let (estimated_width, estimated_height) = match file_size {
                0..=5_000_000 => (640, 480),     // Small files: 480p
                5_000_001..=20_000_000 => (1280, 720), // Medium files: 720p
                _ => (1920, 1080),               // Large files: 1080p
            };
            
            // Create a more accurate video info structure
            let video_info = VideoInfo {
                id: Uuid::new_v4().to_string(),
                filename: file_path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("unknown")
                    .to_string(),
                duration: estimated_duration,
                width: estimated_width,
                height: estimated_height,
                fps: 30.0, // Default fps
                format: format!("Windows Compatible ({})", file_extension.to_uppercase()),
            };

            self.video_info = Some(video_info.clone());
            self.video_path = Some(file_path.clone());

            // Update status
            {
                let mut status = self.stream_status.write().await;
                status.duration = video_info.duration;
                status.target_fps = video_info.fps;
            }

            println!("Video loaded on Windows: {} ({} bytes, estimated {}s)", 
                video_info.filename, file_size, estimated_duration);
            println!("Windows video info: {:?}", video_info);
            Ok(video_info)
        }
    }

    pub async fn load_video_with_original_name(&mut self, file_path: PathBuf, original_filename: &str) -> Result<VideoInfo> {
        *self.walk_cache.write().await = None;

        #[cfg(not(windows))]
        {
            // FFmpeg implementation for all platforms except Windows
            let video_info = {
                let input = ffmpeg::format::input(&file_path)?;
                let video_stream = input
                    .streams()
                    .best(ffmpeg::media::Type::Video)
                    .ok_or_else(|| anyhow!("No video stream found"))?;
                let codec_context = ffmpeg::codec::context::Context::from_parameters(video_stream.parameters())?;
                let decoder = codec_context.decoder().video()?;
                
                let duration = if video_stream.duration() != ffmpeg::ffi::AV_NOPTS_VALUE {
                    video_stream.duration() as f64 * f64::from(video_stream.time_base())
                } else {
                    input.duration() as f64 / f64::from(ffmpeg::ffi::AV_TIME_BASE)
                };

                let fps = f64::from(video_stream.avg_frame_rate());
                
                VideoInfo {
                    id: Uuid::new_v4().to_string(),
                    filename: original_filename.to_string(),
                    duration,
                    width: decoder.width(),
                    height: decoder.height(),
                    fps,
                    format: format!("{:?}", decoder.format()),
                }
            };

            self.video_info = Some(video_info.clone());
            self.video_path = Some(file_path);

            // Update status
            {
                let mut status = self.stream_status.write().await;
                status.duration = video_info.duration;
                status.target_fps = video_info.fps.max(30.0).min(60.0);
            }

            Ok(video_info)
        }
        
        #[cfg(windows)]
        {
            // Windows-specific implementation using mp4 crate for real video processing
            
            // Check if file exists
            if !file_path.exists() {
                return Err(anyhow!("Video file not found: {:?}", file_path));
            }
            
            // Get file metadata
            let metadata = std::fs::metadata(&file_path)?;
            let file_size = metadata.len();
            
            // Use original filename for format detection instead of temp file path
            let file_extension = std::path::Path::new(original_filename)
                .extension()
                .and_then(|ext| ext.to_str())
                .unwrap_or("")
                .to_lowercase();
            
            println!("Windows video loading: original_filename={}, file_extension={}", original_filename, file_extension);
            
            if !matches!(file_extension.as_str(), "mp4" | "mov" | "avi" | "mkv" | "webm" | "flv" | "wmv" | "m4v") {
                return Err(anyhow!("Unsupported video format: {}", file_extension));
            }
            
            // Try to parse MP4 file using mp4 crate for real video information
            let video_info = if file_extension == "mp4" || file_extension == "mov" || file_extension == "m4v" {
                match Self::parse_mp4_file(&file_path, original_filename) {
                    Ok(info) => {
                        println!("Successfully parsed MP4 file: {:?}", info);
                        info
                    }
                    Err(e) => {
                        println!("Failed to parse MP4 file: {}, falling back to estimation", e);
                        Self::create_estimated_video_info(original_filename, file_size, &file_extension)
                    }
                }
            } else {
                // For non-MP4 files, use estimation
                Self::create_estimated_video_info(original_filename, file_size, &file_extension)
            };

            self.video_info = Some(video_info.clone());
            self.video_path = Some(file_path.clone());

            // Update status
            {
                let mut status = self.stream_status.write().await;
                status.duration = video_info.duration;
                status.target_fps = video_info.fps;
            }

            println!("Video loaded on Windows: {} ({} bytes, {}s)", 
                video_info.filename, file_size, video_info.duration);
            println!("Windows video info: {:?}", video_info);
            Ok(video_info)
        }
    }

    pub async fn start_streaming(&mut self) -> Result<()> {
        if self.is_streaming.load(Ordering::Relaxed) {
            return Ok(());
        }

        let video_path = self.video_path.clone()
            .ok_or_else(|| anyhow!("No video file loaded"))?;

        // Resolve ffmpeg binary once — bundled resource on Windows, PATH on Linux.
        let ffmpeg_path = if let Some(app) = self.app_handle.as_ref() {
            crate::camera_capture::resolve_ffmpeg_path(app)
                .map_err(|e| anyhow!("Failed to locate ffmpeg: {e}"))?
        } else {
            std::path::PathBuf::from("ffmpeg")
        };

        self.is_streaming.store(true, Ordering::Relaxed);
        
        {
            let mut status = self.stream_status.write().await;
            status.is_playing = true;
            status.current_time = 0.0;
            status.current_loop = 0;
            status.buffer_health = 0.0;
            status.actual_fps = 0.0;
            status.walk_truncated = false;
            status.walk_seconds = 0.0;
        }

        {
            let is_streaming = self.is_streaming.clone();
            let stream_status = self.stream_status.clone();
            let loop_settings = self.loop_settings.clone();
            let performance_metrics = self.performance_metrics.clone();
            let walk_cache = self.walk_cache.clone();

            let fps = self.video_info.as_ref().map(|info| info.fps).unwrap_or(30.0);
            let width = self.video_info.as_ref().map(|info| info.width).unwrap_or(1920);
            // Height is no longer needed: the pipeline derives it from the camera geometry.
            let _ = &self.video_info;

            // Clear frame queue
            if let Ok(mut q) = FRAME_QUEUE.try_lock() {
                q.clear();
            }

            tokio::task::spawn(async move {
                let settings = *loop_settings.read().await;

                let result = if settings.natural_motion {
                    Self::walk_video_frames(
                        ffmpeg_path,
                        video_path,
                        is_streaming,
                        stream_status.clone(),
                        walk_cache,
                        performance_metrics,
                        settings,
                        fps,
                        width,
                    ).await
                } else {
                    Self::run_loop_pipeline(
                        ffmpeg_path,
                        video_path,
                        is_streaming,
                        stream_status.clone(),
                        performance_metrics,
                        settings,
                        fps,
                    ).await
                };

                if let Err(e) = result {
                    eprintln!("Video streaming error: {}", e);
                    // Clear the preparing flag on the way out, or the UI sits on
                    // "Preparing…" forever after a failed decode.
                    let mut status = stream_status.write().await;
                    status.is_preparing = false;
                    status.is_playing = false;
                }
            });
        }

        println!("Video streaming started");
        Ok(())
    }

    pub async fn pause_streaming(&mut self) -> Result<()> {
        self.is_streaming.store(false, Ordering::Relaxed);
        
        {
            let mut status = self.stream_status.write().await;
            status.is_playing = false;
        }

        Ok(())
    }

    pub async fn stop_streaming(&mut self) -> Result<()> {
        self.is_streaming.store(false, Ordering::Relaxed);

        {
            let mut status = self.stream_status.write().await;
            status.is_playing = false;
            status.current_time = 0.0;
            status.current_loop = 0;
            status.walk_seconds = 0.0;
            status.walk_truncated = false;
        }

        // Release the natural-motion store. Up to ~192 MB was staying resident for the rest
        // of the session on nothing but a stop — only loading a different video cleared it.
        // Pause deliberately keeps it, which is what the cache is for; re-decoding after a
        // stop is cheap now that playback no longer waits for the whole window.
        *self.walk_cache.write().await = None;

        Ok(())
    }

    pub async fn get_stream_status(&self) -> StreamStatus {
        self.stream_status.read().await.clone()
    }

    pub async fn set_loop_settings(&self, settings: LoopSettings) -> Result<()> {
        {
            let mut current = self.loop_settings.write().await;
            *current = settings;
        }

        let mut status = self.stream_status.write().await;
        status.loop_count = settings.loop_count;

        Ok(())
    }

    pub async fn get_performance_metrics(&self) -> PerformanceMetrics {
        self.performance_metrics.read().await.clone()
    }

    /// Straight-loop playback: one ffmpeg, from the source file to the camera.
    ///
    /// This used to be two processes. The producer decoded the clip, scaled it, and encoded
    /// MJPEG; the sink decoded that MJPEG straight back to raw and handed it to the platform
    /// camera. Nothing between those two steps needed a JPEG — it was only ever a way to
    /// move frames between processes. Measured at 720p the encode cost ~8.2 ms of CPU per
    /// frame and the decode ~4.7 ms, against ~2.0 ms to emit raw frames in one pass, and the
    /// MJPEG stage re-subsampled chroma on the way through. So: one process, no JPEG on the
    /// camera path, and a separate small preview leg for the UI.
    ///
    /// Looping happens inside ffmpeg via `-stream_loop` rather than by respawning the
    /// process once per pass.
    async fn run_loop_pipeline(
        ffmpeg_path: PathBuf,
        video_path: PathBuf,
        is_streaming: Arc<AtomicBool>,
        stream_status: Arc<RwLock<StreamStatus>>,
        performance_metrics: Arc<RwLock<PerformanceMetrics>>,
        settings: LoopSettings,
        target_fps: f64,
    ) -> Result<()> {
        use std::time::Instant;

        let fps = if target_fps.is_finite() && target_fps > 0.0 { target_fps } else { 30.0 };
        let (sink, camera_geometry) = crate::virtual_camera::active_sink().await;

        // With the camera off the user is only previewing, so the pipeline still runs — it
        // just has no camera leg. Fall back to the configured output size for the preview's
        // aspect ratio.
        let geometry = camera_geometry.unwrap_or_else(|| {
            let (w, h) = crate::virtual_camera::output_resolution();
            Geometry { width: w, height: h, fps: fps.round().max(1.0) as u32 }
        });

        // loop_count 0 means "forever"; otherwise -stream_loop counts *additional* passes.
        let stream_loop = if settings.loop_count == 0 {
            -1
        } else {
            settings.loop_count.saturating_sub(1) as i32
        };

        let spec = PipelineSpec {
            source: Source::File {
                path: video_path.to_string_lossy().into_owned(),
                stream_loop,
            },
            sink: sink.clone(),
            geometry,
            preview: Some(PreviewSpec::default()),
            realtime: true,
        };

        let mut cmd = tokio::process::Command::new(&ffmpeg_path);
        cmd.args(pipeline::build(&spec))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true);
        #[cfg(target_os = "windows")]
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW

        let mut child = cmd
            .spawn()
            .map_err(|e| anyhow!("Failed to start ffmpeg ({}): {}", ffmpeg_path.display(), e))?;
        let mut stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow!("Failed to open the pipeline's stdout"))?;

        let started = Instant::now();
        let frame_bytes = sink.frame_bytes(&geometry);

        // Progress is derived from elapsed time rather than counted frames: on a device sink
        // the pixels go straight from ffmpeg to /dev/videoN and Rust never sees them.
        let clip_seconds = {
            let status = stream_status.read().await;
            if status.duration > 0.0 { status.duration } else { 0.0 }
        };

        match frame_bytes {
            // Push sink (Windows softcam): raw frames come back on stdout and Rust forwards
            // them into shared memory. One reusable buffer, so there is no per-frame
            // allocation on the camera path at all.
            Some(size) => {
                let mut buf = vec![0u8; size];
                let mut frames = 0u64;
                let mut preview = PreviewEncoder::new(&geometry, PreviewSpec::default());

                loop {
                    if !is_streaming.load(Ordering::Relaxed) {
                        break;
                    }
                    if let Err(e) = stdout.read_exact(&mut buf).await {
                        // EOF is the normal end of a bounded loop count.
                        if is_streaming.load(Ordering::Relaxed) {
                            println!("Pipeline ended: {}", e);
                        }
                        break;
                    }

                    if let Err(e) = crate::virtual_camera::push_raw_frame(&buf).await {
                        eprintln!("Failed to deliver frame to the virtual camera: {}", e);
                        break;
                    }
                    frames += 1;

                    // Derived from the frame we already have in hand, at a fraction of the
                    // camera's rate. Never blocks the loop above.
                    preview.maybe_publish(&buf);

                    Self::publish_progress(
                        &stream_status,
                        &performance_metrics,
                        started,
                        fps,
                        clip_seconds,
                        settings.loop_count,
                        frames,
                    )
                    .await;
                }
            }
            // Device sink (Linux v4l2) or preview-only: ffmpeg owns the camera leg, and
            // stdout carries the preview MJPEG.
            None => {
                let mut reader = JpegStreamReader::new();
                let mut chunk = vec![0u8; 32 * 1024];
                let mut frames = 0u64;

                loop {
                    if !is_streaming.load(Ordering::Relaxed) {
                        break;
                    }
                    let n = match stdout.read(&mut chunk).await {
                        Ok(0) => break,
                        Ok(n) => n,
                        Err(e) => {
                            eprintln!("Failed to read the preview stream: {}", e);
                            break;
                        }
                    };
                    reader.extend(&chunk[..n]);
                    while let Some(jpeg) = reader.next_frame() {
                        publish_preview_frame(jpeg);
                        frames += 1;
                    }

                    Self::publish_progress(
                        &stream_status,
                        &performance_metrics,
                        started,
                        fps,
                        clip_seconds,
                        settings.loop_count,
                        (started.elapsed().as_secs_f64() * fps) as u64,
                    )
                    .await;
                    let _ = frames;
                }
            }
        }

        let _ = child.kill().await;
        let _ = child.wait().await;

        {
            let mut status = stream_status.write().await;
            status.is_playing = false;
            status.current_time = 0.0;
            status.buffer_health = 0.0;
            status.actual_fps = 0.0;
        }

        println!("Video streaming completed");
        Ok(())
    }

    /// Shared status bookkeeping for the loop pipeline.
    ///
    /// `frames` is the camera's frame count where we have one and an estimate from elapsed
    /// time where we do not; either way it only drives the UI.
    async fn publish_progress(
        stream_status: &Arc<RwLock<StreamStatus>>,
        performance_metrics: &Arc<RwLock<PerformanceMetrics>>,
        started: std::time::Instant,
        fps: f64,
        clip_seconds: f64,
        loop_count: u32,
        frames: u64,
    ) {
        let elapsed = started.elapsed().as_secs_f64();
        let (current_time, current_loop) = if clip_seconds > 0.0 {
            (elapsed % clip_seconds, (elapsed / clip_seconds) as u32)
        } else {
            (elapsed, 0)
        };

        {
            let mut status = stream_status.write().await;
            status.current_time = current_time;
            status.current_loop = current_loop;
            status.buffer_health = 1.0;
            if elapsed > 0.0 {
                status.actual_fps = frames as f64 / elapsed;
            }
            if loop_count > 0 && current_loop >= loop_count {
                status.is_playing = false;
            }
        }
        {
            let mut metrics = performance_metrics.write().await;
            metrics.frames_processed = frames;
            metrics.buffer_size = 0;
        }
        let _ = fps;
    }
    
    /// Decode a clip into the natural-motion frame store.
    ///
    /// The store is MJPEG because the walk needs random access and raw frames cannot be held
    /// (1800 frames at 720p is ~4.7 GB). See [`crate::pipeline::mjpeg_store_args`] for why
    /// the quality settings are what they are — briefly: this is the one place the app's own
    /// encoding quality is visible in the output, so it is near-transparent and, unlike
    /// before, pins a pixel format.
    ///
    /// `store_width` is clamped to the source width by the caller: encoding an upscaled
    /// frame pays JPEG rates for invented detail.
    /// Collecting wrapper around [`Self::decode_walk_frames_into`], for callers that want
    /// the whole store at once rather than watching it fill.
    #[cfg(test)]
    async fn decode_walk_frames(
        ffmpeg_path: &PathBuf,
        video_path: &PathBuf,
        store_width: u32,
        max_frames: usize,
        is_streaming: &Arc<AtomicBool>,
    ) -> Result<(Vec<Vec<u8>>, bool)> {
        let store: SharedFrames = Arc::new(std::sync::Mutex::new(Vec::new()));
        let truncated = Self::decode_walk_frames_into(
            ffmpeg_path,
            video_path,
            store_width,
            max_frames,
            is_streaming,
            &store,
        )
        .await?;
        let frames = store
            .lock()
            .unwrap()
            .iter()
            .map(|f| f.to_vec())
            .collect();
        Ok((frames, truncated))
    }

    /// Decode into `store`, appending each frame as it arrives.
    ///
    /// Appending rather than returning at the end is what lets playback start on a short
    /// prefix while the rest of the clip fills in behind the walk — the "preparing" spinner
    /// used to sit there for the whole decode.
    async fn decode_walk_frames_into(
        ffmpeg_path: &PathBuf,
        video_path: &PathBuf,
        store_width: u32,
        max_frames: usize,
        is_streaming: &Arc<AtomicBool>,
        store: &SharedFrames,
    ) -> Result<bool> {
        // One frame beyond the cap, so that "we hit the cap" and "the clip is exactly the
        // window long" stay distinguishable.
        let args = pipeline::mjpeg_store_args(
            video_path,
            store_width,
            max_frames.saturating_add(1),
        );

        let mut cmd = tokio::process::Command::new(ffmpeg_path);
        cmd.args(&args)
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        #[cfg(target_os = "windows")]
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW

        let mut child = cmd.spawn()
            .map_err(|e| anyhow!("Failed to start ffmpeg ({}): {}", ffmpeg_path.display(), e))?;
        let mut stdout = child.stdout.take().unwrap();

        let mut reader = JpegStreamReader::new();
        let mut chunk = [0u8; 65536];
        let mut frame_bytes = 0usize;
        let mut hit_byte_cap = false;
        let mut count = 0usize;

        loop {
            // Stop pressed mid-decode: bail out instead of making the user wait for a clip
            // they've already cancelled.
            if !is_streaming.load(Ordering::Relaxed) {
                break;
            }

            match stdout.read(&mut chunk).await {
                Ok(0) => break,
                Ok(n) => {
                    reader.extend(&chunk[..n]);

                    while let Some(frame) = reader.next_frame() {
                        frame_bytes += frame.len();
                        // One frame past the window is decoded on purpose, to tell "the clip
                        // is longer than the window" from "the clip is exactly the window
                        // long". Don't publish it.
                        if count < max_frames {
                            store.lock().unwrap().push(Arc::from(frame.into_boxed_slice()));
                        }
                        count += 1;

                        if frame_bytes >= MAX_WALK_BYTES {
                            hit_byte_cap = true;
                            break;
                        }
                    }

                    if hit_byte_cap || count > max_frames {
                        break;
                    }
                }
                Err(e) => return Err(anyhow!("Failed to read from ffmpeg stdout: {}", e)),
            }
        }

        // Dropping the handle kills ffmpeg (kill_on_drop) when we stopped early; without
        // closing stdout first it would otherwise block writing into a full pipe.
        drop(stdout);
        let _ = child.kill().await;
        let _ = child.wait().await;

        if count == 0 {
            return Err(anyhow!("ffmpeg decoded no frames from {}", video_path.display()));
        }

        // Getting past the cap means there was more clip than the window holds. A file
        // exactly the window long lands on the cap and is not truncated. Heavy footage can
        // exhaust the byte budget before the frame budget.
        Ok(count > max_frames || hit_byte_cap)
    }

    /// Natural motion: buffer the clip, then walk its frame index at random instead of
    /// replaying it front to back. See [`crate::frame_walk`] for why the walk looks live.
    #[allow(clippy::too_many_arguments)]
    async fn walk_video_frames(
        ffmpeg_path: PathBuf,
        video_path: PathBuf,
        is_streaming: Arc<AtomicBool>,
        stream_status: Arc<RwLock<StreamStatus>>,
        walk_cache: Arc<RwLock<Option<WalkBuffer>>>,
        performance_metrics: Arc<RwLock<PerformanceMetrics>>,
        settings: LoopSettings,
        target_fps: f64,
        source_width: u32,
    ) -> Result<()> {
        use std::time::{Duration, Instant};

        // Probed fps can be zero or NaN on a malformed file, and it divides the tick period.
        let fps = if target_fps.is_finite() && target_fps > 0.0 { target_fps } else { 30.0 };
        let max_frames = ((WALK_WINDOW_SECS * fps).ceil() as usize).max(1);

        // Never store more detail than the source has. Encoding at the camera's width
        // upscales a small clip and then pays JPEG rates for the invented pixels — at 1080p
        // output from a 640-wide source that is roughly 9x the bytes for no extra detail.
        // The decoder scales up once, later, with lanczos.
        let (out_width, _) = crate::virtual_camera::output_resolution();
        let store_width = source_width.min(out_width).max(2) & !1; // even: yuvj422p needs it

        // Reuse the buffer when the same clip is already decoded at the same size, so
        // pause/resume doesn't pay the decode wait again. The width has to be part of the
        // key: changing the Resolution setting mid-session used to silently reuse a store
        // decoded for the old size.
        let cached = {
            let cache = walk_cache.read().await;
            match cache.as_ref() {
                Some(buf) if buf.source == video_path && buf.width == store_width => {
                    Some((Arc::clone(&buf.frames), buf.truncated))
                }
                _ => None,
            }
        };

        // Playback used to wait for the whole window to decode — up to a minute of video
        // behind a spinner. Now it starts on a short prefix and the store fills in behind
        // the walk. That is safe because the walk moves at most one frame per tick while the
        // decoder runs without `-re`; see `FrameWalk::grow`.
        let (frames, decoding) = match cached {
            Some((frames, truncated)) => {
                {
                    let mut status = stream_status.write().await;
                    status.walk_truncated = truncated;
                }
                (frames, None)
            }
            None => {
                let store: SharedFrames = Arc::new(std::sync::Mutex::new(Vec::new()));
                {
                    let mut status = stream_status.write().await;
                    status.is_preparing = true;
                    status.walk_truncated = false;
                }

                let handle = {
                    let (ffmpeg_path, video_path) = (ffmpeg_path.clone(), video_path.clone());
                    let (is_streaming, store) = (is_streaming.clone(), Arc::clone(&store));
                    let (walk_cache, stream_status) = (walk_cache.clone(), stream_status.clone());
                    tokio::spawn(async move {
                        let result = Self::decode_walk_frames_into(
                            &ffmpeg_path,
                            &video_path,
                            store_width,
                            max_frames,
                            &is_streaming,
                            &store,
                        )
                        .await;
                        match result {
                            Ok(truncated) => {
                                // Only cache a decode that ran to completion; a cancelled one
                                // holds just the opening of the clip.
                                if is_streaming.load(Ordering::Relaxed) {
                                    *walk_cache.write().await = Some(WalkBuffer {
                                        source: video_path,
                                        width: store_width,
                                        frames: Arc::clone(&store),
                                        truncated,
                                    });
                                }
                                let mut status = stream_status.write().await;
                                status.walk_truncated = truncated;
                            }
                            Err(e) => eprintln!("Natural motion decode failed: {}", e),
                        }
                    })
                };

                // Enough to walk on while the rest arrives. One second, floored at a handful
                // of frames so a very short clip still starts.
                let prefix = ((fps.round() as usize).max(1)).min(max_frames).max(4);
                loop {
                    if !is_streaming.load(Ordering::Relaxed) {
                        let mut status = stream_status.write().await;
                        status.is_playing = false;
                        status.is_preparing = false;
                        return Ok(());
                    }
                    if store.lock().unwrap().len() >= prefix || handle.is_finished() {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }

                {
                    let mut status = stream_status.write().await;
                    status.is_preparing = false;
                }

                if store.lock().unwrap().is_empty() {
                    let mut status = stream_status.write().await;
                    status.is_playing = false;
                    return Err(anyhow!("ffmpeg decoded no frames from {}", video_path.display()));
                }
                (store, Some(handle))
            }
        };
        let _ = decoding;

        {
            let n = frames.lock().unwrap().len();
            println!("Natural motion: playing from {n} frames, store still filling");
        }

        // The walk hands out JPEGs from its in-memory store, but the camera takes raw
        // frames, so this path keeps a decoder. Unlike the old design that is not a round
        // trip: the MJPEG here is the random-access *store* the walk needs, and the decoder
        // is the only thing turning it back into pixels — there is no re-encode.
        let (sink, camera_geometry) = crate::virtual_camera::active_sink().await;
        let geometry = camera_geometry.unwrap_or_else(|| {
            let (w, h) = crate::virtual_camera::output_resolution();
            Geometry { width: w, height: h, fps: fps.round().max(1.0) as u32 }
        });
        let decoder_spec = PipelineSpec {
            source: Source::MjpegStdin { fps: geometry.fps },
            sink: sink.clone(),
            geometry,
            preview: Some(PreviewSpec::default()),
            // The ticker below is the clock; -re here would fight it.
            realtime: false,
        };

        let mut decoder = {
            let mut cmd = tokio::process::Command::new(&ffmpeg_path);
            cmd.args(pipeline::build(&decoder_spec))
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::inherit())
                .kill_on_drop(true);
            #[cfg(target_os = "windows")]
            cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
            cmd.spawn()
                .map_err(|e| anyhow!("Failed to start the walk decoder ({}): {}", ffmpeg_path.display(), e))?
        };
        let mut decoder_stdin = decoder
            .stdin
            .take()
            .ok_or_else(|| anyhow!("Failed to open the walk decoder's stdin"))?;
        let decoder_stdout = decoder
            .stdout
            .take()
            .ok_or_else(|| anyhow!("Failed to open the walk decoder's stdout"))?;
        let reader = Self::spawn_sink_reader(
            decoder_stdout,
            sink.clone(),
            geometry,
            is_streaming.clone(),
        );

        // Vary the walk per run so restarting a clip doesn't replay the same wander.
        let seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0x5DEE_CE66_D000_0001);
        // Starts on whatever the decoder has so far; grow() extends it as more lands.
        let mut walk = crate::frame_walk::FrameWalk::new(frames.lock().unwrap().len(), seed);

        // Nothing upstream paces us now that `-re` is gone, so this tick is the clock the
        // virtual camera runs on. `from_secs_f64` rather than integer millisecond division,
        // which would quantise 30 fps to 30.30.
        let mut ticker = tokio::time::interval(Duration::from_secs_f64(1.0 / fps));
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        let max_loops = settings.loop_count;
        let mut frames_emitted = 0u64;
        let mut frames_processed = 0u64;
        let mut last_fps_update = Instant::now();

        let mut frame_len = 0usize;
        let mut buffer_bytes = 0u64;

        loop {
            if !is_streaming.load(Ordering::Relaxed) {
                break;
            }

            // Pick up whatever the decoder has appended since the last tick. Cheap: a lock,
            // a length, and one Arc clone.
            let (idx, idx_frame, len_now) = {
                let store = frames.lock().unwrap();
                walk.grow(store.len());
                let idx = walk.next_index();
                (idx, Arc::clone(&store[idx]), store.len())
            };
            if len_now != frame_len {
                frame_len = len_now;
                buffer_bytes = frames.lock().unwrap().iter().map(|f| f.len() as u64).sum();
                let mut status = stream_status.write().await;
                status.walk_seconds = frame_len as f64 / fps;
            }

            // The walk has no seam, so there is no pass to count. Measure elapsed playback in
            // clip-lengths instead: N loops means N clip durations, which keeps the existing
            // loop slider (and `0 == forever`) meaningful.
            let current_loop = (frames_emitted / frame_len.max(1) as u64) as u32;
            if max_loops > 0 && current_loop >= max_loops {
                let mut status = stream_status.write().await;
                status.current_loop = current_loop;
                status.is_playing = false;
                break;
            }

            ticker.tick().await;

            let process_start = Instant::now();

            // No copy of the JPEG itself: the store hands out an Arc and the decoder only
            // reads these bytes. This used to clone the whole frame twice per tick — once
            // for the camera and once for the preview — off a buffer already resident.
            if decoder_stdin.write_all(&idx_frame).await.is_err() {
                // The decoder exited (stop, or ffmpeg died). Nothing left to feed.
                break;
            }

            frames_emitted += 1;
            frames_processed += 1;

            {
                let mut metrics = performance_metrics.write().await;
                metrics.frames_processed = frames_emitted;
                metrics.average_encode_time = process_start.elapsed().as_secs_f64() * 1000.0;
                metrics.buffer_size = frame_len;
                metrics.memory_usage = buffer_bytes;
            }

            {
                let mut status = stream_status.write().await;
                // Reports where in the clip the walk currently is, so the readout wanders
                // back and forth exactly as the footage does.
                status.current_time = idx as f64 / fps;
                status.current_loop = current_loop;
                status.buffer_health = 1.0;

                let now = Instant::now();
                if now.duration_since(last_fps_update) >= Duration::from_secs(1) {
                    status.actual_fps = frames_processed as f64
                        / now.duration_since(last_fps_update).as_secs_f64();
                    last_fps_update = now;
                    frames_processed = 0;
                }
            }
        }

        // Closing stdin gives ffmpeg EOF, which lets the reader below finish rather than
        // block forever on a decoder that will never produce another frame.
        drop(decoder_stdin);
        let _ = reader.await;
        let _ = decoder.kill().await;
        let _ = decoder.wait().await;

        {
            let mut status = stream_status.write().await;
            status.is_playing = false;
            status.current_time = 0.0;
            status.buffer_health = 0.0;
            status.actual_fps = 0.0;
        }

        println!("Natural motion streaming stopped after {frames_emitted} frames");
        Ok(())
    }

    /// Drain a pipeline's stdout for the lifetime of a stream.
    ///
    /// Which stream that is depends on the sink: a push sink (Windows softcam) sends raw
    /// frames back for Rust to forward, while a device sink has already delivered the frames
    /// itself and sends only the preview MJPEG.
    fn spawn_sink_reader(
        mut stdout: tokio::process::ChildStdout,
        sink: pipeline::CameraSink,
        geometry: Geometry,
        is_streaming: Arc<AtomicBool>,
    ) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            match sink.frame_bytes(&geometry) {
                Some(size) => {
                    let mut buf = vec![0u8; size];
                    let mut preview = PreviewEncoder::new(&geometry, PreviewSpec::default());
                    while is_streaming.load(Ordering::Relaxed) {
                        if stdout.read_exact(&mut buf).await.is_err() {
                            break;
                        }
                        if crate::virtual_camera::push_raw_frame(&buf).await.is_err() {
                            break;
                        }
                        preview.maybe_publish(&buf);
                    }
                }
                None => {
                    let mut reader = JpegStreamReader::new();
                    let mut chunk = vec![0u8; 32 * 1024];
                    while is_streaming.load(Ordering::Relaxed) {
                        match stdout.read(&mut chunk).await {
                            Ok(0) | Err(_) => break,
                            Ok(n) => reader.extend(&chunk[..n]),
                        }
                        while let Some(jpeg) = reader.next_frame() {
                            publish_preview_frame(jpeg);
                            crate::virtual_camera::note_frame_delivered().await;
                        }
                    }
                }
            }
        })
    }

    #[cfg(windows)]
    fn parse_mp4_file(file_path: &std::path::PathBuf, original_filename: &str) -> Result<VideoInfo> {
        use std::fs::File;
        use std::io::BufReader;
        
        let file = File::open(file_path)?;
        let file_size = file.metadata()?.len();
        let reader = BufReader::new(file);
        
        let mp4_reader = mp4::Mp4Reader::read_header(reader, file_size)
            .map_err(|e| anyhow!("Failed to read MP4 header: {}", e))?;
        
        // Find the first video track
        let video_track = mp4_reader.tracks().values()
            .find(|track| track.track_type().map_or(false, |t| t == mp4::TrackType::Video))
            .ok_or_else(|| anyhow!("No video track found in MP4 file"))?;
        
        // Extract video information
        let duration_secs = mp4_reader.duration().as_secs_f64();
        let track_duration = video_track.duration().as_secs_f64();
        let actual_duration = if track_duration > 0.0 { track_duration } else { duration_secs };
        
        // Get video dimensions from the reader (fallback to defaults)
        let (width, height) = (1920, 1080); // Default fallback since sample_description isn't available
        
        // Calculate frame rate
        let fps = if actual_duration > 0.0 {
            video_track.sample_count() as f64 / actual_duration
        } else {
            30.0 // Default fallback
        };
        
        let format_name = "MP4 Video"; // Simplified since we can't access codec details easily
        
        println!("Parsed MP4 video track: {}x{} at {:.2}fps, duration: {:.2}s, samples: {}", 
            width, height, fps, actual_duration, video_track.sample_count());
        
        Ok(VideoInfo {
            id: Uuid::new_v4().to_string(),
            filename: original_filename.to_string(),
            duration: actual_duration,
            width,
            height,
            fps,
            format: format!("MP4 ({}) - {}", format_name, original_filename),
        })
    }

    #[cfg(windows)]
    fn create_estimated_video_info(original_filename: &str, file_size: u64, file_extension: &str) -> VideoInfo {
        // Basic video duration estimation based on file size
        let estimated_duration = match file_size {
            0..=1_000_000 => 5.0,           // < 1MB: ~5 seconds
            1_000_001..=10_000_000 => 15.0, // 1-10MB: ~15 seconds
            10_000_001..=50_000_000 => 60.0, // 10-50MB: ~1 minute
            50_000_001..=200_000_000 => 300.0, // 50-200MB: ~5 minutes
            _ => 600.0,                      // > 200MB: ~10 minutes
        };
        
        // Basic resolution estimation based on file size and format
        let (estimated_width, estimated_height) = match file_size {
            0..=5_000_000 => (640, 480),     // Small files: 480p
            5_000_001..=20_000_000 => (1280, 720), // Medium files: 720p
            _ => (1920, 1080),               // Large files: 1080p
        };
        
        VideoInfo {
            id: Uuid::new_v4().to_string(),
            filename: original_filename.to_string(),
            duration: estimated_duration,
            width: estimated_width,
            height: estimated_height,
            fps: 30.0, // Default fps
            format: format!("Windows Compatible ({})", file_extension.to_uppercase()),
        }
    }
}

#[tauri::command]
pub async fn get_video_frame_batch() -> Result<Option<FrameBatch>, String> {
    if let Ok(mut q) = FRAME_QUEUE.try_lock() {
        Ok(q.pop_front())
    } else {
        Ok(None)
    }
}

/// Hand a preview JPEG to the UI.
///
/// `try_lock`, and drop the frame on contention: the preview must never be able to slow the
/// pipeline that is feeding the camera. Only the newest frame is worth anything to a viewer,
/// so the queue is capped and drops from the front.
pub fn publish_preview_frame(jpeg: Vec<u8>) {
    if let Ok(mut q) = FRAME_QUEUE.try_lock() {
        while q.len() >= 4 {
            q.pop_front();
        }
        let len = jpeg.len();
        q.push_back(FrameBatch {
            frames: vec![VideoFrame { data: jpeg, timestamp: 0.0, width: 0, height: 0 }],
            sequence_id: 0,
            total_frames: 1,
        });
        let _ = len;
    }
}

// Global video processor instance
static VIDEO_PROCESSOR: Lazy<Arc<Mutex<VideoProcessor>>> = 
    Lazy::new(|| Arc::new(Mutex::new(VideoProcessor::new())));

// Public API functions
pub async fn load_video_file(file_path: String) -> Result<VideoInfo> {
    let path = PathBuf::from(file_path);
    let mut processor = VIDEO_PROCESSOR.lock().await;
    processor.load_video(path).await
}

pub async fn load_video_file_with_original_name(file_path: String, original_filename: String) -> Result<VideoInfo> {
    let path = PathBuf::from(file_path);
    let mut processor = VIDEO_PROCESSOR.lock().await;
    processor.load_video_with_original_name(path, &original_filename).await
}

pub async fn start_video_stream() -> Result<()> {
    let mut processor = VIDEO_PROCESSOR.lock().await;
    processor.start_streaming().await
}

pub async fn pause_video_stream() -> Result<()> {
    let mut processor = VIDEO_PROCESSOR.lock().await;
    processor.pause_streaming().await
}

pub async fn stop_video_stream() -> Result<()> {
    let mut processor = VIDEO_PROCESSOR.lock().await;
    processor.stop_streaming().await
}

pub async fn get_video_stream_status() -> StreamStatus {
    let processor = VIDEO_PROCESSOR.lock().await;
    processor.get_stream_status().await
}

pub async fn set_video_loop_settings(settings: LoopSettings) -> Result<()> {
    let processor = VIDEO_PROCESSOR.lock().await;
    processor.set_loop_settings(settings).await
}

pub async fn get_performance_metrics() -> Result<PerformanceMetrics> {
    let processor = VIDEO_PROCESSOR.lock().await;
    Ok(processor.get_performance_metrics().await)
}

pub async fn init_video_processor(app_handle: AppHandle) {
    let mut processor = VIDEO_PROCESSOR.lock().await;
    processor.set_app_handle(app_handle);
} 
#[cfg(test)]
mod tests {
    use super::*;

    // --- create_estimated_video_info ----------------------------------------------
    //
    // The fallback used when a file cannot be parsed. Duration and resolution key off the
    // *same* file size but with different cut points (1/10/50/200 MB vs 5/20 MB), which is
    // exactly the kind of pairing that silently drifts when one range is edited.

    #[cfg(windows)]
    #[test]
    fn estimated_duration_matches_its_size_bands() {
        let d = |size| VideoProcessor::create_estimated_video_info("c.mp4", size, "mp4").duration;
        assert_eq!(d(1_000_000), 5.0);
        assert_eq!(d(1_000_001), 15.0, "1 MB is the upper bound of the 5s band");
        assert_eq!(d(10_000_000), 15.0);
        assert_eq!(d(10_000_001), 60.0);
        assert_eq!(d(50_000_000), 60.0);
        assert_eq!(d(50_000_001), 300.0);
        assert_eq!(d(200_000_000), 300.0);
        assert_eq!(d(200_000_001), 600.0);
    }

    #[cfg(windows)]
    #[test]
    fn estimated_resolution_uses_its_own_size_bands() {
        let wh = |size| {
            let i = VideoProcessor::create_estimated_video_info("c.mp4", size, "mp4");
            (i.width, i.height)
        };
        assert_eq!(wh(5_000_000), (640, 480));
        assert_eq!(wh(5_000_001), (1280, 720));
        assert_eq!(wh(20_000_000), (1280, 720));
        assert_eq!(wh(20_000_001), (1920, 1080));
    }

    #[cfg(windows)]
    #[test]
    fn estimated_info_keeps_the_original_filename_and_is_uniquely_identified() {
        let a = VideoProcessor::create_estimated_video_info("holiday clip.mp4", 3_000_000, "mp4");
        let b = VideoProcessor::create_estimated_video_info("holiday clip.mp4", 3_000_000, "mp4");
        assert_eq!(a.filename, "holiday clip.mp4");
        assert_ne!(a.id, b.id, "each load must get its own id");
    }

    // --- decode_walk_frames -------------------------------------------------------
    //
    // Natural motion buffers the whole clip up front, and this function's cap decides
    // both how much memory that costs and whether the UI tells the user the clip was
    // cut short. The boundary case is invisible in ordinary use -- a clip exactly the
    // window long once reported itself truncated -- so pin it against a real decode
    // rather than trusting the arithmetic by eye.
    //
    // Skipped when ffmpeg isn't on PATH: the Windows CI runner has no system ffmpeg
    // (the app ships its own as a bundled resource), and a decode test is not the place
    // to discover that.

    fn ffmpeg_available() -> bool {
        std::process::Command::new("ffmpeg")
            .arg("-version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    /// A `seconds`-long clip at 30 fps, so the exact frame count is known.
    fn make_fixture(dir: &std::path::Path, seconds: u32) -> PathBuf {
        let path = dir.join("fixture.mp4");
        let status = std::process::Command::new("ffmpeg")
            .args([
                "-hide_banner", "-loglevel", "error", "-y",
                "-f", "lavfi",
                "-i", &format!("testsrc=size=320x240:rate=30:duration={seconds}"),
                "-c:v", "libx264", "-pix_fmt", "yuv420p",
            ])
            .arg(&path)
            .status()
            .expect("failed to run ffmpeg");
        assert!(status.success(), "ffmpeg could not build the fixture");
        path
    }

    #[tokio::test]
    async fn decode_walk_frames_caps_frames_and_reports_truncation() {
        if !ffmpeg_available() {
            eprintln!("skipping: ffmpeg not on PATH");
            return;
        }

        let dir = tempfile::tempdir().unwrap();
        let fixture = make_fixture(dir.path(), 4); // 4s at 30 fps == 120 frames
        let ffmpeg = PathBuf::from("ffmpeg");
        let streaming = Arc::new(AtomicBool::new(true));

        // Cap well above the clip: everything is buffered, nothing is claimed truncated.
        let (all, truncated) =
            VideoProcessor::decode_walk_frames(&ffmpeg, &fixture, 320, 1_000, &streaming)
                .await
                .unwrap();
        assert_eq!(all.len(), 120, "expected every frame of a 4s 30fps clip");
        assert!(!truncated);

        // Each carved chunk must be a standalone JPEG, or the virtual camera gets garbage.
        for (i, frame) in all.iter().enumerate() {
            assert!(frame.starts_with(&[0xFF, 0xD8]), "frame {i} has no JPEG SOI");
            assert!(frame.ends_with(&[0xFF, 0xD9]), "frame {i} has no JPEG EOI");
        }

        // Cap below the clip: buffer stops exactly at the cap and says so.
        let (capped, truncated) =
            VideoProcessor::decode_walk_frames(&ffmpeg, &fixture, 320, 50, &streaming)
                .await
                .unwrap();
        assert_eq!(capped.len(), 50);
        assert!(truncated);

        // Cap exactly equal to the clip: the boundary that used to report a false
        // truncation, which would have shown the user a warning about a clip that fit.
        let (exact, truncated) =
            VideoProcessor::decode_walk_frames(&ffmpeg, &fixture, 320, 120, &streaming)
                .await
                .unwrap();
        assert_eq!(exact.len(), 120);
        assert!(!truncated, "a clip exactly the window long is not truncated");
    }

    #[tokio::test]
    async fn decode_walk_frames_errors_on_a_file_with_no_video() {
        if !ffmpeg_available() {
            eprintln!("skipping: ffmpeg not on PATH");
            return;
        }

        // An unreadable source must surface an error, not an empty buffer -- the walk
        // would otherwise index into nothing.
        let dir = tempfile::tempdir().unwrap();
        let junk = dir.path().join("not-a-video.mp4");
        std::fs::write(&junk, b"this is not a video file").unwrap();

        let result = VideoProcessor::decode_walk_frames(
            &PathBuf::from("ffmpeg"),
            &junk,
            320,
            100,
            &Arc::new(AtomicBool::new(true)),
        ).await;

        assert!(result.is_err(), "a non-video file must not decode to zero frames silently");
    }
}
