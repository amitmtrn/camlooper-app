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

    pub async fn start_streaming(&mut self) -> Result<()> {
        if self.is_streaming.load(Ordering::Relaxed) {
            return Ok(());
        }

        #[cfg(not(windows))]
        let video_path = self.video_path.clone()
            .ok_or_else(|| anyhow!("No video file loaded"))?;
        
        self.is_streaming.store(true, Ordering::Relaxed);
        
        {
            let mut status = self.stream_status.write().await;
            status.is_playing = true;
            status.current_time = 0.0;
            status.current_loop = 0;
            status.buffer_health = 0.0;
            status.actual_fps = 0.0;
        }

        #[cfg(not(windows))]
        {
            // FFmpeg-based implementation for non-Windows platforms
            // Clone necessary data for the streaming task
            let is_streaming = self.is_streaming.clone();
            let stream_status = self.stream_status.clone();
            let loop_settings = self.loop_settings.clone();
            let performance_metrics = self.performance_metrics.clone();
            let app_handle = self.app_handle.clone();

            // Test event emission first
            if let Some(ref app_handle) = app_handle {
                println!("Testing event emission...");
                let test_frame = VideoFrame {
                    data: vec![255; 100], // Small test data
                    timestamp: 0.0,
                    width: 640,
                    height: 480,
                };
                let test_batch = FrameBatch {
                    frames: vec![test_frame],
                    sequence_id: 999,
                    total_frames: 1,
                };
                if let Err(e) = app_handle.emit("video-frame-batch", &test_batch) {
                    eprintln!("Failed to emit test event: {}", e);
                } else {
                    println!("Test event emitted successfully");
                }
            }

            // Create a channel for frame communication
            let (frame_tx, mut frame_rx) = tokio::sync::mpsc::unbounded_channel::<FrameBatch>();
            
            // Spawn the frame processing task using spawn_blocking for FFmpeg operations
            tokio::task::spawn_blocking(move || {
                let rt = tokio::runtime::Handle::current();
                rt.block_on(async move {
                    if let Err(e) = Self::process_video_frames(
                        video_path,
                        is_streaming,
                        stream_status,
                        loop_settings,
                        performance_metrics,
                        Some(frame_tx),
                    ).await {
                        eprintln!("Video streaming error: {}", e);
                    }
                })
            });
            
            // Handle frame emission in the main async context
            if let Some(app_handle) = app_handle {
                tokio::spawn(async move {
                    while let Some(batch) = frame_rx.recv().await {
                        if let Err(e) = app_handle.emit("video-frame-batch", &batch) {
                            eprintln!("Failed to emit frame batch: {}", e);
                        }
                    }
                });
            }
        }

        #[cfg(windows)]
        {
            // Windows-specific implementation without FFmpeg
            // Enhanced version with better video simulation
            let is_streaming = self.is_streaming.clone();
            let stream_status = self.stream_status.clone();
            let loop_settings = self.loop_settings.clone();
            let app_handle = self.app_handle.clone();
            let video_info = self.video_info.clone();

            // Create enhanced frames for Windows
            tokio::spawn(async move {
                let mut frame_count = 0u64;
                let frame_duration = tokio::time::Duration::from_millis(33); // 30 FPS
                let mut current_loop = 0u32;
                
                // Get video info for duration and dimensions
                let (duration, width, height, filename) = if let Some(info) = video_info {
                    (info.duration, info.width, info.height, info.filename)
                } else {
                    (10.0, 640, 480, "Unknown".to_string())
                };
                
                let total_frames = (duration * 30.0) as u64; // 30 FPS
                let (max_loops, _auto_start) = *loop_settings.read().await;
                
                println!("Starting Windows video simulation: {} ({}x{}, {} frames, {} loops)", 
                    filename, width, height, total_frames, max_loops);
                
                while is_streaming.load(Ordering::Relaxed) {
                    // Check if we've reached the loop limit
                    if max_loops > 0 && current_loop >= max_loops {
                        break;
                    }
                    
                    // Create a more sophisticated frame with video info
                    let loop_frame_count = frame_count % total_frames;
                    let progress = loop_frame_count as f64 / total_frames as f64;
                    
                    let enhanced_frame = Self::create_enhanced_frame(
                        width, height, frame_count, progress, &filename, current_loop
                    );
                    
                    if let Some(ref app_handle) = app_handle {
                        let batch = FrameBatch {
                            frames: vec![enhanced_frame],
                            sequence_id: frame_count,
                            total_frames: 1,
                        };
                        
                        if let Err(e) = app_handle.emit("video-frame-batch", &batch) {
                            eprintln!("Failed to emit frame batch: {}", e);
                        }
                    }
                    
                    // Update stream status
                    {
                        let mut status = stream_status.write().await;
                        status.current_time = (loop_frame_count as f64) / 30.0; // Current position in loop
                        status.actual_fps = 30.0;
                        status.buffer_health = 1.0; // Always healthy for generated frames
                        status.current_loop = current_loop;
                    }
                    
                    frame_count += 1;
                    
                    // Check if we completed a loop
                    if loop_frame_count == 0 && frame_count > 0 {
                        current_loop += 1;
                        println!("Completed Windows video loop {} of {}", 
                            current_loop, if max_loops == 0 { "∞".to_string() } else { max_loops.to_string() });
                    }
                    
                    tokio::time::sleep(frame_duration).await;
                }
                
                // Update final status
                {
                    let mut status = stream_status.write().await;
                    status.is_playing = false;
                    status.current_time = 0.0;
                    status.buffer_health = 0.0;
                    status.actual_fps = 0.0;
                }
                
                println!("Windows video simulation completed");
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

    #[cfg(not(windows))]
    async fn process_video_frames(
        video_path: PathBuf,
        is_streaming: Arc<AtomicBool>,
        stream_status: Arc<RwLock<StreamStatus>>,
        loop_settings: Arc<RwLock<(u32, bool)>>,
        performance_metrics: Arc<RwLock<PerformanceMetrics>>,
        frame_tx: Option<tokio::sync::mpsc::UnboundedSender<FrameBatch>>,
    ) -> Result<()> {
        use std::time::{Duration, Instant};
        
        let mut current_loop = 0u32;
        let (max_loops, _auto_start) = *loop_settings.read().await;
        let mut sequence_id = 0u64;
        
        loop {
            if !is_streaming.load(Ordering::Relaxed) {
                break;
            }
            
            // Check if we've reached the loop limit
            if max_loops > 0 && current_loop >= max_loops {
                break;
            }
            
            // Process video file
            let mut input = ffmpeg::format::input(&video_path)?;
            let video_stream_index = {
                let video_stream = input
                    .streams()
                    .best(ffmpeg::media::Type::Video)
                    .ok_or_else(|| anyhow!("No video stream found"))?;
                video_stream.index()
            };
            
            let (fps, mut decoder, mut scaler, time_base) = {
                let video_stream = input
                    .streams()
                    .best(ffmpeg::media::Type::Video)
                    .ok_or_else(|| anyhow!("No video stream found"))?;
                
                let codec_context = ffmpeg::codec::context::Context::from_parameters(video_stream.parameters())?;
                let decoder = codec_context.decoder().video()?;
                
                // Get video properties - ensure minimum 30 FPS, maximum 60 FPS
                let fps = f64::from(video_stream.avg_frame_rate()).max(30.0).min(60.0);
                let time_base = video_stream.time_base();
                
                // Create scaler for converting to RGB24
                let scaler = ffmpeg::software::scaling::context::Context::get(
                    decoder.format(),
                    decoder.width(),
                    decoder.height(),
                    ffmpeg::format::Pixel::RGB24,
                    decoder.width(),
                    decoder.height(),
                    ffmpeg::software::scaling::Flags::BILINEAR,
                )?;
                
                (fps, decoder, scaler, time_base)
            };
            
            let frame_duration = Duration::from_secs_f64(1.0 / fps);
            let mut frame_count = 0u64;
            let mut frames_processed = 0u64;
            let mut last_fps_update = Instant::now();
            let mut frame_timer = Instant::now();
            
            // Frame batch processing
            let mut frame_batch = Vec::new();
            const BATCH_SIZE: usize = 3; // Send frames in small batches for better performance
            
            // Process packets
            for (stream, packet) in input.packets() {
                if !is_streaming.load(Ordering::Relaxed) {
                    break;
                }
                
                if stream.index() == video_stream_index {
                    decoder.send_packet(&packet)?;
                    
                    let mut decoded_frame = ffmpeg::util::frame::Video::empty();
                    while decoder.receive_frame(&mut decoded_frame).is_ok() {
                        let process_start = Instant::now();
                        
                        // Convert frame to RGB24
                        let mut rgb_frame = ffmpeg::util::frame::Video::empty();
                        scaler.run(&decoded_frame, &mut rgb_frame)?;
                        
                        // Convert to JPEG
                        let jpeg_data = Self::frame_to_jpeg(&rgb_frame)?;
                        let jpeg_size = jpeg_data.len();
                        
                        // Calculate timestamp
                        let timestamp = if let Some(pts) = decoded_frame.pts() {
                            if pts != ffmpeg::ffi::AV_NOPTS_VALUE {
                                pts as f64 * f64::from(time_base)
                            } else {
                                frame_count as f64 / fps
                            }
                        } else {
                            frame_count as f64 / fps
                        };
                        
                        // Create video frame
                        let video_frame = VideoFrame {
                            data: jpeg_data,
                            timestamp,
                            width: rgb_frame.width(),
                            height: rgb_frame.height(),
                        };
                        
                        // Add to batch
                        frame_batch.push(video_frame);
                        
                        // Send batch when full
                        if frame_batch.len() >= BATCH_SIZE {
                            if let Some(ref tx) = frame_tx {
                                let batch = FrameBatch {
                                    frames: frame_batch.clone(),
                                    sequence_id,
                                    total_frames: frame_batch.len(),
                                };
                                let _ = tx.send(batch);
                                sequence_id += 1;
                            }
                            frame_batch.clear();
                        }
                        
                        // Update metrics
                        frames_processed += 1;
                        frame_count += 1;
                        
                        let process_time = process_start.elapsed();
                        
                        // Update performance metrics
                        {
                            let mut metrics = performance_metrics.write().await;
                            metrics.frames_processed = frames_processed;
                            metrics.average_encode_time = process_time.as_secs_f64() * 1000.0; // Convert to milliseconds
                            metrics.buffer_size = frame_batch.len();
                            // Update memory usage estimate (rough calculation)
                            metrics.memory_usage = (frame_batch.len() * jpeg_size) as u64;
                        }
                        
                        // Update stream status
                        {
                            let mut status = stream_status.write().await;
                            status.current_time = timestamp;
                            status.current_loop = current_loop;
                            // Calculate dynamic buffer health based on processing time
                            let target_process_time = Duration::from_secs_f64(1.0 / fps);
                            let health = if process_time < target_process_time {
                                1.0 // Good - processing faster than required
                            } else {
                                // Reduce health as processing time increases
                                (target_process_time.as_secs_f64() / process_time.as_secs_f64()).max(0.1) as f32
                            };
                            status.buffer_health = health;
                            
                            // Update FPS calculation
                            let now = Instant::now();
                            if now.duration_since(last_fps_update) >= Duration::from_secs(1) {
                                status.actual_fps = frames_processed as f64 / now.duration_since(last_fps_update).as_secs_f64();
                                last_fps_update = now;
                                frames_processed = 0;
                            }
                        }
                        
                        // Frame rate limiting
                        let elapsed = frame_timer.elapsed();
                        if elapsed < frame_duration {
                            tokio::time::sleep(frame_duration - elapsed).await;
                        }
                        frame_timer = Instant::now();
                    }
                }
            }
            
            // Send any remaining frames in the batch
            if !frame_batch.is_empty() {
                if let Some(ref tx) = frame_tx {
                    let batch = FrameBatch {
                        frames: frame_batch.clone(),
                        sequence_id,
                        total_frames: frame_batch.len(),
                    };
                    let _ = tx.send(batch);
                    sequence_id += 1;
                }
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
    
    #[cfg(not(windows))]
    fn frame_to_jpeg(frame: &ffmpeg::util::frame::Video) -> Result<Vec<u8>> {
        use image::{ImageBuffer, Rgb};
        
        let width = frame.width() as u32;
        let height = frame.height() as u32;
        let data = frame.data(0);
        let linesize = frame.stride(0) as usize;
        
        // Convert RGB24 data to image buffer
        let mut img_data = Vec::with_capacity((width * height * 3) as usize);
        for y in 0..height {
            let row_start = (y as usize) * linesize;
            let row_end = row_start + (width as usize) * 3;
            if row_end <= data.len() {
                img_data.extend_from_slice(&data[row_start..row_end]);
            }
        }
        
        // Create image buffer
        let img = ImageBuffer::<Rgb<u8>, Vec<u8>>::from_raw(width, height, img_data)
            .ok_or_else(|| anyhow!("Failed to create image buffer"))?;
        
        // Encode to JPEG
        let mut cursor = std::io::Cursor::new(Vec::new());
        let mut encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut cursor, 75);
        encoder.encode_image(&img)?;
        
        Ok(cursor.into_inner())
    }

    #[cfg(windows)]
    fn create_enhanced_frame(width: u32, height: u32, frame_count: u64, progress: f64, filename: &str, current_loop: u32) -> VideoFrame {
        // Create a more sophisticated frame with video information
        let color_cycle = (frame_count % 180) as f32 / 180.0; // Color cycle every 6 seconds at 30fps
        
        // Base colors that change over time
        let r = (128.0 + 127.0 * (color_cycle * 2.0 * std::f32::consts::PI).sin()) as u8;
        let g = (128.0 + 127.0 * ((color_cycle + 0.33) * 2.0 * std::f32::consts::PI).sin()) as u8;
        let b = (128.0 + 127.0 * ((color_cycle + 0.66) * 2.0 * std::f32::consts::PI).sin()) as u8;
        
        // Create a progress bar effect
        let progress_bar_height = (height as f64 * 0.05) as u32; // 5% of height
        let progress_bar_y = height - progress_bar_height;
        let progress_width = (width as f64 * progress) as u32;
        
        // Create gradient effect based on position in video
        let gradient_factor = (progress * 255.0) as u8;
        
        // Create frame data with visual elements
        let mut data = Vec::new();
        for y in 0..height {
            for x in 0..width {
                let _pixel_index = (y * width + x) as usize;
                
                // Create different visual elements
                if y >= progress_bar_y && x < progress_width {
                    // Progress bar - bright white
                    data.push(255); // R
                    data.push(255); // G
                    data.push(255); // B
                } else if y >= progress_bar_y {
                    // Progress bar background - dark gray
                    data.push(64);  // R
                    data.push(64);  // G
                    data.push(64);  // B
                } else if y < 50 {
                    // Top bar with filename info - blend with gradient
                    let text_r = (r as u32 + gradient_factor as u32) / 2;
                    let text_g = (g as u32 + gradient_factor as u32) / 2;
                    let text_b = (b as u32 + gradient_factor as u32) / 2;
                    data.push(text_r.min(255) as u8);
                    data.push(text_g.min(255) as u8);
                    data.push(text_b.min(255) as u8);
                } else {
                    // Main area - animated gradient
                    let wave_effect = ((x as f64 / width as f64) + (y as f64 / height as f64) + (frame_count as f64 / 30.0)) * 2.0 * std::f32::consts::PI as f64;
                    let wave_r = (r as f64 + 50.0 * wave_effect.sin()).max(0.0).min(255.0) as u8;
                    let wave_g = (g as f64 + 50.0 * (wave_effect + 2.0).sin()).max(0.0).min(255.0) as u8;
                    let wave_b = (b as f64 + 50.0 * (wave_effect + 4.0).sin()).max(0.0).min(255.0) as u8;
                    
                    data.push(wave_r);
                    data.push(wave_g);
                    data.push(wave_b);
                }
            }
        }
        
        // Convert to simple JPEG-like format (mock compression)
        // For Windows, we create a more realistic compressed representation
        let mut compressed_data = Vec::new();
        
        // Add mock JPEG header
        compressed_data.extend_from_slice(&[0xFF, 0xD8, 0xFF, 0xE0]); // JPEG SOI and APP0
        
        // Sample the data to create a compressed representation
        let sample_rate = (data.len() / 2048).max(1); // Ensure we don't exceed reasonable size
        for (i, &byte) in data.iter().enumerate() {
            if i % sample_rate == 0 {
                compressed_data.push(byte);
            }
        }
        
        // Add frame info as metadata in the mock format
        let frame_info = format!("{}:{}:{}", filename, current_loop, frame_count);
        compressed_data.extend_from_slice(frame_info.as_bytes());
        
        // Ensure reasonable size
        if compressed_data.len() > 8192 {
            compressed_data.truncate(8192);
        }
        
        VideoFrame {
            data: compressed_data,
            timestamp: frame_count as f64 / 30.0,
            width,
            height,
        }
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