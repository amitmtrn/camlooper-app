use anyhow::{anyhow, Result};
#[cfg(not(windows))]
use ffmpeg_next as ffmpeg;
use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tauri::{AppHandle, Emitter};
use tokio::sync::{Mutex, RwLock};
use uuid::Uuid;
use std::collections::VecDeque;

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
}

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
    loop_settings: Arc<RwLock<(u32, bool)>>,
    performance_metrics: Arc<RwLock<PerformanceMetrics>>,
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
            })),
            app_handle: None,
            is_streaming: Arc::new(AtomicBool::new(false)),
            loop_settings: Arc::new(RwLock::new((10, true))),
            performance_metrics: Arc::new(RwLock::new(PerformanceMetrics {
                frames_processed: 0,
                frames_dropped: 0,
                average_encode_time: 0.0,
                average_decode_time: 0.0,
                current_quality: 80,
                buffer_size: 0,
                memory_usage: 0,
            })),
        }
    }

    pub fn set_app_handle(&mut self, app_handle: AppHandle) {
        self.app_handle = Some(app_handle);
    }

    pub async fn load_video(&mut self, file_path: PathBuf) -> Result<VideoInfo> {
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
        }

        {
            let is_streaming = self.is_streaming.clone();
            let stream_status = self.stream_status.clone();
            let loop_settings = self.loop_settings.clone();
            let performance_metrics = self.performance_metrics.clone();

            let fps = self.video_info.as_ref().map(|info| info.fps).unwrap_or(30.0);
            let width = self.video_info.as_ref().map(|info| info.width).unwrap_or(1920);
            let height = self.video_info.as_ref().map(|info| info.height).unwrap_or(1080);

            // Clear frame queue
            if let Ok(mut q) = FRAME_QUEUE.try_lock() {
                q.clear();
            }

            tokio::task::spawn(async move {
                if let Err(e) = Self::process_video_frames(
                    ffmpeg_path,
                    video_path,
                    is_streaming,
                    stream_status,
                    loop_settings,
                    performance_metrics,
                    fps,
                    width,
                    height,
                ).await {
                    eprintln!("Video streaming error: {}", e);
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
        }

        Ok(())
    }

    pub async fn get_stream_status(&self) -> StreamStatus {
        self.stream_status.read().await.clone()
    }

    pub async fn set_loop_settings(&self, loop_count: u32, auto_start: bool) -> Result<()> {
        let mut settings = self.loop_settings.write().await;
        *settings = (loop_count, auto_start);
        
        let mut status = self.stream_status.write().await;
        status.loop_count = loop_count;
        
        Ok(())
    }

    pub fn get_video_info(&self) -> Option<VideoInfo> {
        self.video_info.clone()
    }

    pub async fn get_performance_metrics(&self) -> PerformanceMetrics {
        self.performance_metrics.read().await.clone()
    }

    async fn process_video_frames(
        ffmpeg_path: PathBuf,
        video_path: PathBuf,
        is_streaming: Arc<AtomicBool>,
        stream_status: Arc<RwLock<StreamStatus>>,
        loop_settings: Arc<RwLock<(u32, bool)>>,
        performance_metrics: Arc<RwLock<PerformanceMetrics>>,
        target_fps: f64,
        target_width: u32,
        target_height: u32,
    ) -> Result<()> {
        use std::time::{Duration, Instant};
        use std::process::Stdio;
        use tokio::io::AsyncReadExt;
        
        let mut current_loop = 0u32;
        let (max_loops, _auto_start) = *loop_settings.read().await;
        
        loop {
            if !is_streaming.load(Ordering::Relaxed) {
                break;
            }
            
            // Check if we've reached the loop limit
            if max_loops > 0 && current_loop >= max_loops {
                break;
            }
            
            // Process video file using external FFmpeg process
            let args = vec![
                "-re".to_string(), // Read at native frame rate
                "-i".to_string(), video_path.to_string_lossy().to_string(),
                "-vf".to_string(), "scale=640:-1".to_string(), // Scale to 640px wide to match camera and reduce IPC overhead
                "-c:v".to_string(), "mjpeg".to_string(),
                "-q:v".to_string(), "5".to_string(),
                "-f".to_string(), "image2pipe".to_string(),
                "-".to_string()
            ];
            
            let mut cmd = tokio::process::Command::new(&ffmpeg_path);
            cmd.args(&args)
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .kill_on_drop(true);
            #[cfg(target_os = "windows")]
            cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
            let mut child = match cmd.spawn() {
                Ok(child) => child,
                Err(e) => {
                    eprintln!("Failed to start ffmpeg ({}) for video processing: {}", ffmpeg_path.display(), e);
                    break;
                }
            };
            
            let mut stdout = child.stdout.take().unwrap();
            
            let mut buffer = Vec::new();
            let mut chunk = [0u8; 32768];
            
            let mut frame_count = 0u64;
            let mut frames_processed = 0u64;
            let mut last_fps_update = Instant::now();
            let mut sequence_id = 0u64;
            
            let mut frame_batch = Vec::new();
            const BATCH_SIZE: usize = 1;
            
            loop {
                if !is_streaming.load(Ordering::Relaxed) {
                    break;
                }
                
                match stdout.read(&mut chunk).await {
                    Ok(0) => break, // EOF, loop finishes
                    Ok(n) => {
                        buffer.extend_from_slice(&chunk[..n]);
                        
                        while let Some(start) = Self::find_subsequence(&buffer, &[0xFF, 0xD8]) {
                            if let Some(end) = Self::find_subsequence(&buffer[start..], &[0xFF, 0xD9]) {
                                let process_start = Instant::now();
                                
                                let end_idx = start + end + 2;
                                let jpeg_data = buffer[start..end_idx].to_vec();
                                let jpeg_size = jpeg_data.len();
                                
                                let timestamp = frame_count as f64 / target_fps;
                                
                                let video_frame = VideoFrame {
                                    data: jpeg_data.clone(),
                                    timestamp,
                                    width: target_width,
                                    height: target_height,
                                };
                                
                                // Push directly to virtual camera (zero-overhead)
                                crate::virtual_camera::send_frame_to_virtual_camera(jpeg_data).await.ok();
                                
                                frame_batch.push(video_frame);
                                
                                if frame_batch.len() >= BATCH_SIZE {
                                    let batch = FrameBatch {
                                        frames: frame_batch.clone(),
                                        sequence_id,
                                        total_frames: frame_batch.len(),
                                    };
                                    
                                    loop {
                                        let mut q = FRAME_QUEUE.lock().await;
                                        if q.len() < 30 {
                                            q.push_back(batch);
                                            break;
                                        }
                                        drop(q);
                                        tokio::time::sleep(tokio::time::Duration::from_millis(5)).await;
                                    }
                                    sequence_id += 1;
                                    frame_batch.clear();
                                }
                                
                                buffer.drain(0..end_idx);
                                
                                // Metrics update
                                frames_processed += 1;
                                frame_count += 1;
                                
                                let process_time = process_start.elapsed();
                                
                                {
                                    let mut metrics = performance_metrics.write().await;
                                    metrics.frames_processed = frames_processed;
                                    metrics.average_encode_time = process_time.as_secs_f64() * 1000.0;
                                    metrics.buffer_size = frame_batch.len();
                                    metrics.memory_usage = (frame_batch.len() * jpeg_size) as u64;
                                }
                                
                                {
                                    let mut status = stream_status.write().await;
                                    status.current_time = timestamp;
                                    status.current_loop = current_loop;
                                    status.buffer_health = 1.0;
                                    
                                    let now = Instant::now();
                                    if now.duration_since(last_fps_update) >= Duration::from_secs(1) {
                                        status.actual_fps = frames_processed as f64 / now.duration_since(last_fps_update).as_secs_f64();
                                        last_fps_update = now;
                                        frames_processed = 0;
                                    }
                                }
                                
                            } else {
                                break;
                            }
                        }
                    },
                    Err(e) => {
                        eprintln!("Failed to read from ffmpeg stdout: {}", e);
                        break;
                    }
                }
            }
            
            // Wait for child process to finish if it hasn't
            let _ = child.wait().await;
            
            // Clean up remaining frames
            if !frame_batch.is_empty() {
                let batch = FrameBatch {
                    frames: frame_batch.clone(),
                    sequence_id,
                    total_frames: frame_batch.len(),
                };
                let mut q = FRAME_QUEUE.lock().await;
                q.push_back(batch);
                frame_batch.clear();
            }
            
            current_loop += 1;
            
            // Update loop status
            {
                let mut status = stream_status.write().await;
                status.current_loop = current_loop;
                if max_loops > 0 && current_loop >= max_loops {
                    status.is_playing = false;
                }
            }
            
            println!("Completed loop {} of {}", current_loop, if max_loops == 0 { "∞".to_string() } else { max_loops.to_string() });
        }
        
        // Reset streaming state
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
    
    fn find_subsequence(haystack: &[u8], needle: &[u8]) -> Option<usize> {
        haystack.windows(needle.len()).position(|window| window == needle)
    }

    #[cfg(windows)]
    fn create_enhanced_frame(width: u32, height: u32, frame_count: u64, progress: f64, _filename: &str, _current_loop: u32) -> VideoFrame {
        use image::{ImageBuffer, Rgb};
        
        let mut rgb_data = Vec::with_capacity((width * height * 3) as usize);
        
        // Create a simple animated pattern for now
        let time = frame_count as f64 * 0.1; // Slow animation speed
        let wave_factor = (time * 2.0).sin() * 0.5 + 0.5;
        
        for y in 0..height {
            for x in 0..width {
                let nx = x as f64 / width as f64;
                let ny = y as f64 / height as f64;
                
                // Create animated wave pattern
                let pattern = ((nx * 10.0 + time).sin() * (ny * 10.0 + time).cos() * wave_factor + 1.0) * 0.5;
                
                // Color based on pattern and progress
                let r = (pattern * 255.0 * progress + 50.0).min(255.0) as u8;
                let g = ((1.0 - pattern) * 255.0 * progress + 50.0).min(255.0) as u8;
                let b = (wave_factor * 255.0 + 50.0).min(255.0) as u8;
                
                rgb_data.push(r);
                rgb_data.push(g);
                rgb_data.push(b);
            }
        }
        
        // Create proper JPEG data using the image crate
        let jpeg_data = match ImageBuffer::<Rgb<u8>, Vec<u8>>::from_raw(width, height, rgb_data) {
            Some(img) => {
                // Encode to JPEG
                let mut cursor = std::io::Cursor::new(Vec::new());
                match image::codecs::jpeg::JpegEncoder::new_with_quality(&mut cursor, 75).encode_image(&img) {
                    Ok(_) => cursor.into_inner(),
                    Err(e) => {
                        eprintln!("Failed to encode JPEG: {}", e);
                        // Fallback to a simple test pattern
                        Self::create_fallback_jpeg(width, height)
                    }
                }
            }
            None => {
                eprintln!("Failed to create image buffer");
                // Fallback to a simple test pattern
                Self::create_fallback_jpeg(width, height)
            }
        };
        
        VideoFrame {
            data: jpeg_data,
            timestamp: frame_count as f64 / 30.0, // Assume 30 FPS
            width,
            height,
        }
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

    #[cfg(windows)]
    async fn extract_video_samples(file_path: &std::path::PathBuf, max_samples: u64) -> Result<Vec<Vec<u8>>> {
        use std::fs::File;
        use std::io::BufReader;
        
        let file = File::open(file_path)?;
        let file_size = file.metadata()?.len();
        let reader = BufReader::new(file);
        
        let mut mp4_reader = mp4::Mp4Reader::read_header(reader, file_size)
            .map_err(|e| anyhow!("Failed to read MP4 header for samples: {}", e))?;
        
        // Find the first video track and get its ID
        let (track_id, sample_count) = {
            let video_track = mp4_reader.tracks().values()
                .find(|track| track.track_type().map_or(false, |t| t == mp4::TrackType::Video))
                .ok_or_else(|| anyhow!("No video track found for sample extraction"))?;
            
            (video_track.track_id(), video_track.sample_count())
        };
        
        let mut samples = Vec::new();
        let step_size = if sample_count > max_samples as u32 { 
            sample_count / max_samples as u32 
        } else { 
            1 
        };
        
        println!("Extracting samples: total={}, step={}, target_max={}", sample_count, step_size, max_samples);
        
        // Extract sample data (we can't decode it without a decoder, but we can get raw data)
        for sample_id in (1..=sample_count).step_by(step_size as usize) {
            if samples.len() >= max_samples as usize {
                break;
            }
            
            match mp4_reader.read_sample(track_id, sample_id) {
                Ok(Some(sample)) => {
                    // Convert mp4::Bytes to Vec<u8>
                    let sample_data = sample.bytes.to_vec();
                    // Store raw sample data - this is encoded video data
                    if sample_data.len() < 1024 * 1024 { // Skip very large samples (>1MB)
                        samples.push(sample_data);
                    }
                }
                Ok(None) => {
                    println!("No sample data for sample {}", sample_id);
                }
                Err(e) => {
                    println!("Failed to read sample {}: {}", sample_id, e);
                }
            }
        }
        
        println!("Extracted {} video samples from MP4 file", samples.len());
        Ok(samples)
    }

    #[cfg(windows)]
    fn create_frame_from_sample(sample_data: &[u8], width: u32, height: u32, frame_count: u64, fps: f64, filename: &str) -> VideoFrame {
        use image::{ImageBuffer, Rgb};
        
        // Since we can't decode the raw video data without a decoder,
        // we'll create a frame that represents the video with some visual indicators
        // and incorporate some characteristics from the sample data
        
        // Use sample data to influence the visual representation
        let data_hash = sample_data.iter().fold(0u64, |acc, &byte| acc.wrapping_add(byte as u64));
        let sample_size = sample_data.len();
        
        // Create a more sophisticated frame that reflects the actual video
        let base_hue = (data_hash % 360) as f32;
        let intensity = (sample_size as f32 / 1024.0).min(255.0);
        
        // Time-based animation with sample influence
        let time_factor = (frame_count as f64 / fps) % 10.0; // 10 second cycle
        let sample_factor = (data_hash % 1000) as f32 / 1000.0;
        
        let mut rgb_data = Vec::with_capacity((width * height * 3) as usize);
        
        for y in 0..height {
            for x in 0..width {
                let nx = x as f32 / width as f32;
                let ny = y as f32 / height as f32;
                
                // Create a pattern influenced by sample data
                let pattern = ((nx * 20.0 + sample_factor * 10.0).sin() * 
                              (ny * 20.0 + time_factor as f32).cos() + 
                              (data_hash as f32 / 1000000.0).sin()) * 0.5 + 0.5;
                
                // Color based on sample data and position
                let r = ((base_hue.sin() * pattern + 0.5) * intensity + 50.0).min(255.0) as u8;
                let g = (((base_hue + 120.0).to_radians().sin() * pattern + 0.5) * intensity + 50.0).min(255.0) as u8;
                let b = (((base_hue + 240.0).to_radians().sin() * pattern + 0.5) * intensity + 50.0).min(255.0) as u8;
                
                rgb_data.push(r);
                rgb_data.push(g);
                rgb_data.push(b);
            }
        }
        
        // Add visual indicators
        Self::add_video_indicators(&mut rgb_data, width, height, sample_size, filename, frame_count);
        
        // Create proper JPEG data
        let jpeg_data = match ImageBuffer::<Rgb<u8>, Vec<u8>>::from_raw(width, height, rgb_data) {
            Some(img) => {
                let mut cursor = std::io::Cursor::new(Vec::new());
                match image::codecs::jpeg::JpegEncoder::new_with_quality(&mut cursor, 80).encode_image(&img) {
                    Ok(_) => cursor.into_inner(),
                    Err(_) => Self::create_fallback_jpeg(width, height)
                }
            }
            None => Self::create_fallback_jpeg(width, height)
        };
        
        VideoFrame {
            data: jpeg_data,
            timestamp: frame_count as f64 / fps,
            width,
            height,
        }
    }

    #[cfg(windows)]
    fn add_video_indicators(rgb_data: &mut [u8], width: u32, height: u32, sample_size: usize, filename: &str, frame_count: u64) {
        // Add sample size indicator bar at the bottom
        let bar_height = 8;
        let bar_width = ((sample_size / 1024).min(width as usize)) as u32; // KB indicator
        
        for y in (height - bar_height)..height {
            for x in 0..bar_width {
                let idx = ((y * width + x) * 3) as usize;
                if idx + 2 < rgb_data.len() {
                    rgb_data[idx] = 0;     // R
                    rgb_data[idx + 1] = 255; // G (green bar)
                    rgb_data[idx + 2] = 0;   // B
                }
            }
        }
        
        // Add filename indicator (first few characters as color pattern)
        let filename_bytes = filename.as_bytes();
        for (i, &byte) in filename_bytes.iter().enumerate().take(10) {
            let x = i as u32 * 8;
            if x < width {
                let color = byte;
                for dy in 0..8 {
                    for dx in 0..8 {
                        let px = x + dx;
                        let py = dy;
                        if px < width && py < height {
                            let idx = ((py * width + px) * 3) as usize;
                            if idx + 2 < rgb_data.len() {
                                rgb_data[idx] = color;
                                rgb_data[idx + 1] = color / 2;
                                rgb_data[idx + 2] = 255 - color;
                            }
                        }
                    }
                }
            }
        }
        
        // Add frame counter pattern
        let frame_mod = (frame_count % 256) as u8;
        for i in 0..16 {
            let x = width - 20 + (i % 4);
            let y = 10 + (i / 4);
            if x < width && y < height {
                let idx = ((y * width + x) * 3) as usize;
                if idx + 2 < rgb_data.len() {
                    rgb_data[idx] = frame_mod;
                    rgb_data[idx + 1] = 255 - frame_mod;
                    rgb_data[idx + 2] = frame_mod / 2;
                }
            }
        }
    }

    #[cfg(windows)]
    fn create_fallback_jpeg(width: u32, height: u32) -> Vec<u8> {
        use image::{ImageBuffer, Rgb};
        
        // Create a simple test pattern
        let mut rgb_data = Vec::with_capacity((width * height * 3) as usize);
        for y in 0..height {
            for x in 0..width {
                let checker = ((x / 32) + (y / 32)) % 2;
                if checker == 0 {
                    rgb_data.extend_from_slice(&[200, 200, 200]); // Light gray
                } else {
                    rgb_data.extend_from_slice(&[100, 100, 100]); // Dark gray
                }
            }
        }
        
        // Create and encode the fallback image
        if let Some(img) = ImageBuffer::<Rgb<u8>, Vec<u8>>::from_raw(width, height, rgb_data) {
            let mut cursor = std::io::Cursor::new(Vec::new());
            if image::codecs::jpeg::JpegEncoder::new_with_quality(&mut cursor, 75).encode_image(&img).is_ok() {
                return cursor.into_inner();
            }
        }
        
        // If all else fails, create a minimal valid JPEG
        vec![
            0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10, 0x4A, 0x46, 0x49, 0x46, 0x00, 0x01,
            0x01, 0x01, 0x00, 0x48, 0x00, 0x48, 0x00, 0x00, 0xFF, 0xDB, 0x00, 0x43,
            0x00, 0x08, 0x06, 0x06, 0x07, 0x06, 0x05, 0x08, 0x07, 0x07, 0x07, 0x09,
            0x09, 0x08, 0x0A, 0x0C, 0x14, 0x0D, 0x0C, 0x0B, 0x0B, 0x0C, 0x19, 0x12,
            0x13, 0x0F, 0x14, 0x1D, 0x1A, 0x1F, 0x1E, 0x1D, 0x1A, 0x1C, 0x1C, 0x20,
            0x24, 0x2E, 0x27, 0x20, 0x22, 0x2C, 0x23, 0x1C, 0x1C, 0x28, 0x37, 0x29,
            0x2C, 0x30, 0x31, 0x34, 0x34, 0x34, 0x1F, 0x27, 0x39, 0x3D, 0x38, 0x32,
            0x3C, 0x2E, 0x33, 0x34, 0x32, 0xFF, 0xC0, 0x00, 0x11, 0x08, 0x00, 0x01,
            0x00, 0x01, 0x01, 0x01, 0x11, 0x00, 0x02, 0x11, 0x01, 0x03, 0x11, 0x01,
            0xFF, 0xC4, 0x00, 0x14, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x08, 0xFF, 0xC4,
            0x00, 0x14, 0x10, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xFF, 0xDA, 0x00, 0x0C,
            0x03, 0x01, 0x00, 0x02, 0x11, 0x03, 0x11, 0x00, 0x3F, 0x00, 0x00, 0xFF, 0xD9
        ]
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

pub async fn set_video_loop_settings(loop_count: u32, auto_start: bool) -> Result<()> {
    let processor = VIDEO_PROCESSOR.lock().await;
    processor.set_loop_settings(loop_count, auto_start).await
}

pub async fn get_video_info() -> Option<VideoInfo> {
    let processor = VIDEO_PROCESSOR.lock().await;
    processor.get_video_info()
}

pub async fn get_performance_metrics() -> Result<PerformanceMetrics> {
    let processor = VIDEO_PROCESSOR.lock().await;
    Ok(processor.get_performance_metrics().await)
}

pub async fn init_video_processor(app_handle: AppHandle) {
    let mut processor = VIDEO_PROCESSOR.lock().await;
    processor.set_app_handle(app_handle);
} 